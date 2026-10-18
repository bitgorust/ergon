// SPDX-License-Identifier: Apache-2.0

package nriplugin

import (
	"context"
	"log/slog"
	"os"
	"strings"
	"testing"
	"time"

	"github.com/containerd/nri/pkg/api"

	"github.com/mimran1980/ergon/k8s/nri-shared-cpus/internal/config"
	"github.com/mimran1980/ergon/k8s/nri-shared-cpus/internal/cpuset"
	"github.com/mimran1980/ergon/k8s/nri-shared-cpus/internal/hft"
	"github.com/mimran1980/ergon/k8s/nri-shared-cpus/internal/metrics"
	"github.com/mimran1980/ergon/k8s/nri-shared-cpus/internal/names"
)

type testPluginResult struct {
	*Plugin
	m *metrics.Metrics
}

func testPlugin(t *testing.T, shared string, opts ...func(*config.File)) testPluginResult {
	t.Helper()
	text := "sharedCpus: \"" + shared + "\"\nnamespaces: [trading]\n"
	// Options are applied to the YAML text through the File shape the config
	// package accepts.
	f := &config.File{SharedCPUs: &shared, Namespaces: []string{"trading"}}
	for _, o := range opts {
		o(f)
	}
	if f.AnnotationOptIn {
		text += "annotationOptIn: true\n"
	}
	if f.DefaultAffinity != "" {
		text += "defaultAffinity: " + f.DefaultAffinity + "\n"
	}
	cfg, err := config.Parse([]byte(text))
	if err != nil {
		t.Fatal(err)
	}
	m := metrics.New()
	p := New(cfg, map[int]cpuset.Set{}, slog.New(slog.NewTextHandler(os.Stderr, &slog.HandlerOptions{Level: slog.LevelError})), m)
	p.ResendDelay = 0
	p.After = func(_ time.Duration, fn func()) { go fn() }
	return testPluginResult{Plugin: p, m: m}
}

func pod() *api.PodSandbox {
	return &api.PodSandbox{
		Id:          "sandbox-1",
		Name:        "trader",
		Namespace:   "trading",
		Uid:         "uid-1",
		Annotations: map[string]string{},
	}
}

func ctr(id string, cpus string, env map[string]string, ann map[string]string) *api.Container {
	envList := make([]string, 0, len(env))
	for k, v := range env {
		envList = append(envList, k+"="+v)
	}
	q := int64(-1)
	s := uint64(2048)
	return &api.Container{
		Id:           id,
		Name:         "app",
		PodSandboxId: "sandbox-1",
		Annotations:  ann,
		Env:          envList,
		State:        api.ContainerState_CONTAINER_CREATED,
		Linux: &api.LinuxContainer{
			Resources: &api.LinuxResources{
				Cpu: &api.LinuxCPU{
					Cpus:   cpus,
					Quota:  &api.OptionalInt64{Value: q},
					Shares: &api.OptionalUInt64{Value: s},
				},
			},
		},
	}
}

func TestCreateAdjustment(t *testing.T) {
	p := testPlugin(t, "0,4")
	c := ctr("ctr-1", "1,5", nil, map[string]string{names.MilliAnnotation: "250"})
	adj, _, err := p.CreateContainer(context.Background(), pod(), c)
	if err != nil {
		t.Fatal(err)
	}
	if adj == nil {
		t.Fatal("no adjustment")
	}
	cpu := adj.GetLinux().GetResources().GetCpu()
	// E is 1,5 and M=250 needs one shared CPU, so the union has 3 CPUs:
	// 2 exclusive + the chosen shared CPU.
	got, err := cpuset.Parse(cpu.GetCpus())
	if err != nil {
		t.Fatalf("cpuset %q: %v", cpu.GetCpus(), err)
	}
	if got.Len() != 3 {
		t.Errorf("cpuset is %q, want 3 CPUs (2 exclusive + 1 shared)", cpu.GetCpus())
	}
	if !cpuset.New(1, 5).IsSubsetOf(got) {
		t.Errorf("cpuset %q does not contain the exclusive CPUs 1,5", cpu.GetCpus())
	}
	shared := got.Difference(cpuset.New(1, 5))
	if shared.Len() != 1 || !shared.IsSubsetOf(cpuset.New(0, 4)) {
		t.Errorf("shared part is %q, want one of 0,4", shared)
	}
	env := map[string]string{}
	for _, e := range adj.GetEnv() {
		env[e.Key] = e.Value
	}
	if env[names.EnvIsolated] != "1,5" {
		t.Errorf("%s = %q, want 1,5", names.EnvIsolated, env[names.EnvIsolated])
	}
	if env[names.EnvShared] != "0" && env[names.EnvShared] != "4" {
		t.Errorf("%s = %q, want one shared CPU", names.EnvShared, env[names.EnvShared])
	}
	if env[names.EnvHelper] != env[names.EnvShared] {
		t.Errorf("%s = %q, %s = %q; they must match", names.EnvHelper, env[names.EnvHelper], names.EnvShared, env[names.EnvShared])
	}
	if env[names.EnvMilli] != "250" {
		t.Errorf("%s = %q, want 250", names.EnvMilli, env[names.EnvMilli])
	}
	hooks := adj.GetHooks()
	if hooks == nil || len(hooks.GetCreateRuntime()) != 1 {
		t.Fatalf("hooks are %v, want one createRuntime hook", hooks)
	}
	h := hooks.GetCreateRuntime()[0]
	if h.GetPath() != p.cfg.HookPath {
		t.Errorf("hook path is %q, want %q", h.GetPath(), p.cfg.HookPath)
	}
	if len(h.GetArgs()) != 4 || h.GetArgs()[2] != "--shared" || h.GetArgs()[3] != env[names.EnvShared] {
		t.Errorf("hook args are %v, want hook --shared <S>", h.GetArgs())
	}
	// Shared-only: there is no isolated child to take back down.
	if got := hooks.GetPoststop(); len(got) != 0 {
		t.Errorf("poststop hooks are %v, want none without a partition", got)
	}
}

func TestCreateAdjustmentNoHookOnOptOut(t *testing.T) {
	p := testPlugin(t, "0,4")
	podObj := pod()
	podObj.Annotations = map[string]string{names.AffinityPrefix + "app": "all"}
	c := ctr("ctr-1", "1,5", nil, map[string]string{names.MilliAnnotation: "250"})
	adj, _, err := p.CreateContainer(context.Background(), podObj, c)
	if err != nil {
		t.Fatal(err)
	}
	if adj.GetHooks() != nil {
		t.Errorf("hooks are %v, want none", adj.GetHooks())
	}
}

func TestCreateRefusesAndCounts(t *testing.T) {
	p := testPlugin(t, "0,4")
	c := ctr("ctr-1", "1,5", nil, map[string]string{names.MilliAnnotation: "250"})
	podObj := pod()
	podObj.Namespace = "default"
	_, _, err := p.CreateContainer(context.Background(), podObj, c)
	if err == nil {
		t.Fatal("create in a non-allowlisted namespace was accepted")
	}
	if got := p.m.Counter("refused/namespace"); got != 1 {
		t.Errorf("refused/namespace = %d, want 1", got)
	}
}

func TestCreateNonOptedInIsUntouched(t *testing.T) {
	p := testPlugin(t, "0,4")
	c := ctr("ctr-1", "1,5", nil, nil)
	adj, _, err := p.CreateContainer(context.Background(), pod(), c)
	if err != nil || adj != nil {
		t.Fatalf("got (%v, %v), want (nil, nil)", adj, err)
	}
}

func TestUpdateRewidens(t *testing.T) {
	p := testPlugin(t, "0,4")
	c := ctr("ctr-1", "0,1,4,5", map[string]string{
		names.EnvIsolated: "1,5",
		names.EnvShared:   "0",
	}, map[string]string{names.MilliAnnotation: "250"})
	res := &api.LinuxResources{Cpu: &api.LinuxCPU{Cpus: "1,5"}} // kubelet narrowed it
	updates, err := p.UpdateContainer(context.Background(), pod(), c, res)
	if err != nil {
		t.Fatal(err)
	}
	if len(updates) != 1 {
		t.Fatalf("got %d updates, want 1", len(updates))
	}
	if !sameCPUs(t, updates[0].GetLinux().GetResources().GetCpu().GetCpus(), "0,1,5") {
		t.Errorf("union is %q, want 0,1,5", updates[0].GetLinux().GetResources().GetCpu().GetCpus())
	}
	if got := p.m.Counter("rewidened/update"); got != 1 {
		t.Errorf("rewidened/update = %d, want 1", got)
	}
}

func TestUpdateMemoryOnlyIsUntouched(t *testing.T) {
	p := testPlugin(t, "0,4")
	c := ctr("ctr-1", "0,1,4,5", map[string]string{
		names.EnvIsolated: "1,5",
		names.EnvShared:   "0",
	}, map[string]string{names.MilliAnnotation: "250"})
	res := &api.LinuxResources{Memory: &api.LinuxMemory{Limit: &api.OptionalInt64{Value: 1 << 30}}}
	updates, err := p.UpdateContainer(context.Background(), pod(), c, res)
	if err != nil {
		t.Fatal(err)
	}
	if len(updates) != 0 {
		t.Fatalf("got %d updates for a memory-only update, want 0", len(updates))
	}
}

func TestUpdateNonOursIsUntouched(t *testing.T) {
	p := testPlugin(t, "0,4")
	c := ctr("ctr-1", "1,5", map[string]string{"PATH": "/bin"}, nil)
	res := &api.LinuxResources{Cpu: &api.LinuxCPU{Cpus: "1"}}
	updates, err := p.UpdateContainer(context.Background(), pod(), c, res)
	if err != nil {
		t.Fatal(err)
	}
	if len(updates) != 0 {
		t.Fatalf("got %d updates for a container that is not ours, want 0", len(updates))
	}
}

func TestUpdateResizeMarksStale(t *testing.T) {
	p := testPlugin(t, "0,4")
	c := ctr("ctr-1", "0,1,4,5", map[string]string{
		names.EnvIsolated: "1,5",
		names.EnvShared:   "0",
	}, map[string]string{names.MilliAnnotation: "250"})
	res := &api.LinuxResources{Cpu: &api.LinuxCPU{Cpus: "1,2,5"}} // resized
	updates, err := p.UpdateContainer(context.Background(), pod(), c, res)
	if err != nil {
		t.Fatal(err)
	}
	if len(updates) != 1 {
		t.Fatalf("got %d updates, want 1", len(updates))
	}
	if !sameCPUs(t, updates[0].GetLinux().GetResources().GetCpu().GetCpus(), "0,1,2,5") {
		t.Errorf("union is %q, want 0,1,2,5", updates[0].GetLinux().GetResources().GetCpu().GetCpus())
	}
	if got := p.m.Counter("stale_env"); got != 1 {
		t.Errorf("stale_env = %d, want 1", got)
	}
}

func TestSynchronizeRewidensAndRebuildsLoad(t *testing.T) {
	p := testPlugin(t, "0,4")
	narrow := ctr("ctr-1", "1,5", map[string]string{
		names.EnvIsolated: "1,5",
		names.EnvShared:   "0",
		names.EnvMilli:    "250",
	}, map[string]string{names.MilliAnnotation: "250"})
	narrow.State = api.ContainerState_CONTAINER_RUNNING
	other := ctr("ctr-2", "2", nil, nil)
	other.State = api.ContainerState_CONTAINER_RUNNING

	updates, err := p.Synchronize(context.Background(), []*api.PodSandbox{pod()}, []*api.Container{narrow, other})
	if err != nil {
		t.Fatal(err)
	}
	if len(updates) != 1 {
		t.Fatalf("got %d updates, want 1", len(updates))
	}
	if !sameCPUs(t, updates[0].GetLinux().GetResources().GetCpu().GetCpus(), "0,1,5") {
		t.Errorf("union is %q, want 0,1,5", updates[0].GetLinux().GetResources().GetCpu().GetCpus())
	}
	load, held := p.Table()
	if held != 1 {
		t.Errorf("held = %d, want 1", held)
	}
	if load[0] != 250 {
		t.Errorf("load on CPU 0 is %d, want 250", load[0])
	}
	if got := p.m.Counter("rewidened/sync"); got != 1 {
		t.Errorf("rewidened/sync = %d, want 1", got)
	}
}

func TestSynchronizeUnhandled(t *testing.T) {
	p := testPlugin(t, "0,4")
	c := ctr("ctr-1", "1,5", nil, map[string]string{names.MilliAnnotation: "250"})
	c.State = api.ContainerState_CONTAINER_RUNNING
	updates, err := p.Synchronize(context.Background(), []*api.PodSandbox{pod()}, []*api.Container{c})
	if err != nil {
		t.Fatal(err)
	}
	if len(updates) != 0 {
		t.Fatalf("got %d updates, want 0", len(updates))
	}
	if got := p.m.Counter("unhandled"); got != 1 {
		t.Errorf("unhandled = %d, want 1", got)
	}
}

func TestSynchronizeKeepsOwnShared(t *testing.T) {
	p := testPlugin(t, "0,4")
	c := ctr("ctr-1", "1,5,4", map[string]string{
		names.EnvIsolated: "1,5",
		names.EnvShared:   "4",
		names.EnvMilli:    "250",
	}, map[string]string{names.MilliAnnotation: "250"})
	c.State = api.ContainerState_CONTAINER_RUNNING
	updates, err := p.Synchronize(context.Background(), []*api.PodSandbox{pod()}, []*api.Container{c})
	if err != nil {
		t.Fatal(err)
	}
	if len(updates) != 0 {
		t.Fatalf("got %d updates for a container that already has its shared CPU, want 0", len(updates))
	}
	load, _ := p.Table()
	if load[4] != 250 {
		t.Errorf("load on CPU 4 is %d, want 250", load[4])
	}
}

func TestSynchronizeResend(t *testing.T) {
	p := testPlugin(t, "0,4")
	c := ctr("ctr-1", "1,5", map[string]string{
		names.EnvIsolated: "1,5",
		names.EnvShared:   "0",
		names.EnvMilli:    "250",
	}, map[string]string{names.MilliAnnotation: "250"})
	c.State = api.ContainerState_CONTAINER_RUNNING

	resent := make(chan []*api.ContainerUpdate, 1)
	p.SetResender(func(updates []*api.ContainerUpdate) ([]*api.ContainerUpdate, error) {
		resent <- updates
		return nil, nil
	})
	if _, err := p.Synchronize(context.Background(), []*api.PodSandbox{pod()}, []*api.Container{c}); err != nil {
		t.Fatal(err)
	}
	select {
	case got := <-resent:
		if len(got) != 1 {
			t.Fatalf("resent %d updates, want 1", len(got))
		}
		if cpus := got[0].GetLinux().GetResources().GetCpu().GetCpus(); !sameCPUs(t, cpus, "0,1,5") {
			t.Errorf("resent cpuset is %q, want 0,1,5", cpus)
		}
	case <-time.After(2 * time.Second):
		t.Fatal("the 2 s resend never ran")
	}
}

func TestStopAndRemoveReleaseLoad(t *testing.T) {
	p := testPlugin(t, "0,4")
	c := ctr("ctr-1", "1,5", nil, map[string]string{names.MilliAnnotation: "250"})
	if _, _, err := p.CreateContainer(context.Background(), pod(), c); err != nil {
		t.Fatal(err)
	}
	if _, held := p.Table(); held != 1 {
		t.Fatalf("held = %d after create, want 1", held)
	}
	if _, err := p.StopContainer(context.Background(), pod(), c); err != nil {
		t.Fatal(err)
	}
	if _, held := p.Table(); held != 0 {
		t.Fatalf("held = %d after stop, want 0", held)
	}
}

func TestRemovePodSandboxForgetsSticky(t *testing.T) {
	p := testPlugin(t, "0,4")
	c := ctr("ctr-1", "1,5", nil, map[string]string{names.MilliAnnotation: "250"})
	if _, _, err := p.CreateContainer(context.Background(), pod(), c); err != nil {
		t.Fatal(err)
	}
	// Release the load but keep the sticky mapping.
	if _, err := p.StopContainer(context.Background(), pod(), c); err != nil {
		t.Fatal(err)
	}
	// Same pod + container name: sticky would give the same CPUs back.
	c2 := ctr("ctr-2", "1,5", nil, map[string]string{names.MilliAnnotation: "250"})
	if _, _, err := p.CreateContainer(context.Background(), pod(), c2); err != nil {
		t.Fatal(err)
	}
	load, _ := p.Table()
	if load[0] != 250 {
		t.Errorf("load on CPU 0 is %d after a sticky restart, want 250", load[0])
	}
	if _, err := p.StopContainer(context.Background(), pod(), c2); err != nil {
		t.Fatal(err)
	}
	if err := p.RemovePodSandbox(context.Background(), pod()); err != nil {
		t.Fatal(err)
	}
	// After the pod is gone the sticky mapping is too: the next create may
	// land on the other shared CPU (the load is equal, so the lower CPU wins).
	c3 := ctr("ctr-3", "1,5", nil, map[string]string{names.MilliAnnotation: "250"})
	if _, _, err := p.CreateContainer(context.Background(), pod(), c3); err != nil {
		t.Fatal(err)
	}
}

func TestBalanceOverSharedCPUs(t *testing.T) {
	// sharedCpus "0,1", the README example: A (250) -> 0, B (250) -> 1,
	// C (250) -> 0 (tie 250/250, lower CPU number wins).
	p := testPlugin(t, "0,1")
	mk := func(name, uid, cpus string, shares uint64) (*api.PodSandbox, *api.Container) {
		pd := pod()
		pd.Uid, pd.Name = uid, name
		c := ctr(name, cpus, nil, map[string]string{names.MilliAnnotation: "250"})
		c.Name = name
		c.Linux.Resources.Cpu.Shares = &api.OptionalUInt64{Value: shares}
		return pd, c
	}
	pdA, a := mk("a", "ua", "2", 1024)
	if _, _, err := p.CreateContainer(context.Background(), pdA, a); err != nil {
		t.Fatal(err)
	}
	pdB, b := mk("b", "ub", "3", 1024)
	if _, _, err := p.CreateContainer(context.Background(), pdB, b); err != nil {
		t.Fatal(err)
	}
	pdC, c := mk("c", "uc", "4", 1024)
	if _, _, err := p.CreateContainer(context.Background(), pdC, c); err != nil {
		t.Fatal(err)
	}
	load, held := p.Table()
	if held != 3 {
		t.Fatalf("held = %d, want 3", held)
	}
	if load[0] != 500 || load[1] != 250 {
		t.Fatalf("load is %v, want CPU 0 = 500 (A+C) and CPU 1 = 250 (B)", load)
	}
}

func TestConfigureMarksNRIConnected(t *testing.T) {
	// Configure is the first call on a new NRI connection. It must flip the
	// connected gauge (so /readyz can open) and run the on-connect callback
	// (so the device plugin marks its devices healthy again).
	p := testPlugin(t, "0,4")
	connected := false
	p.OnConnected(func() { connected = true })
	if p.m.Ready() {
		t.Fatal("ready before anything happened")
	}
	mask, err := p.Configure(context.Background(), "", "containerd", "v2.3.4")
	if err != nil {
		t.Fatal(err)
	}
	_ = mask
	if !connected {
		t.Fatal("OnConnected did not run")
	}
	p.m.SetRegistered(true)
	if !p.m.Ready() {
		t.Fatal("not ready after Configure and device-plugin registration")
	}
}

// A pod author can set container env by hand. Without our marker that must
// never be treated as ours: otherwise the pod could grow its cpuset onto
// other pods' exclusive CPUs at the next kubelet cpuset write (kubelet
// restart, resize, reconcile).
func TestUpdateHandSetEnvWithoutMarkerIsUntouched(t *testing.T) {
	p := testPlugin(t, "0,4")
	c := ctr("ctr-1", "1,5", map[string]string{
		names.EnvIsolated: "1,5",
		names.EnvShared:   "0,4",
		names.EnvMilli:    "250",
	}, nil) // no marker: not ours
	res := &api.LinuxResources{Cpu: &api.LinuxCPU{Cpus: "1,5"}}
	updates, err := p.UpdateContainer(context.Background(), pod(), c, res)
	if err != nil {
		t.Fatal(err)
	}
	if len(updates) != 0 {
		t.Fatalf("got %d updates for a hand-set env, want 0", len(updates))
	}
}

func TestSynchronizeHandSetEnvWithoutMarkerIsUntouched(t *testing.T) {
	p := testPlugin(t, "0,4")
	c := ctr("ctr-1", "1,5", map[string]string{
		names.EnvIsolated: "1,5",
		names.EnvShared:   "0,4",
		names.EnvMilli:    "250",
	}, nil) // no marker: not ours
	c.State = api.ContainerState_CONTAINER_RUNNING
	updates, err := p.Synchronize(context.Background(), []*api.PodSandbox{pod()}, []*api.Container{c})
	if err != nil {
		t.Fatal(err)
	}
	if len(updates) != 0 {
		t.Fatalf("got %d updates for a hand-set env, want 0", len(updates))
	}
	if _, held := p.Table(); held != 0 {
		t.Fatalf("held = %d for a hand-set env, want 0", held)
	}
}

// The only CPUs this plugin may add to a cpuset are the node's shared
// housekeeping CPUs. A set outside config sharedCpus (forged env, or a
// config change) must never be widened back in.
func TestUpdateForeignSharedIsNotWidened(t *testing.T) {
	p := testPlugin(t, "0,4")
	c := ctr("ctr-1", "1,5", map[string]string{
		names.EnvIsolated: "1,5",
		names.EnvShared:   "2", // CPU 2 is not in sharedCpus 0,4
	}, map[string]string{names.MilliAnnotation: "250"})
	res := &api.LinuxResources{Cpu: &api.LinuxCPU{Cpus: "1,5"}}
	updates, err := p.UpdateContainer(context.Background(), pod(), c, res)
	if err != nil {
		t.Fatal(err)
	}
	if len(updates) != 0 {
		t.Fatalf("got %d updates that add a foreign CPU, want 0", len(updates))
	}
}

func TestSynchronizeForeignSharedIsNotWidened(t *testing.T) {
	p := testPlugin(t, "0,4")
	c := ctr("ctr-1", "1,5", map[string]string{
		names.EnvIsolated: "1,5",
		names.EnvShared:   "2", // CPU 2 is not in sharedCpus 0,4
		names.EnvMilli:    "250",
	}, map[string]string{names.MilliAnnotation: "250"})
	c.State = api.ContainerState_CONTAINER_RUNNING
	updates, err := p.Synchronize(context.Background(), []*api.PodSandbox{pod()}, []*api.Container{c})
	if err != nil {
		t.Fatal(err)
	}
	if len(updates) != 0 {
		t.Fatalf("got %d updates that add a foreign CPU, want 0", len(updates))
	}
}

// A marker plus env that does not add up (more shared CPUs than it asked for)
// was not set up by this plugin. It must not be widened.
func TestSynchronizeMarkerEnvMismatchIsUnhandled(t *testing.T) {
	p := testPlugin(t, "0,4")
	c := ctr("ctr-1", "1,5", map[string]string{
		names.EnvIsolated: "1,5",
		names.EnvShared:   "0,4", // 2 CPUs, but the marker asked for 250 (1)
		names.EnvMilli:    "250",
	}, map[string]string{names.MilliAnnotation: "250"})
	c.State = api.ContainerState_CONTAINER_RUNNING
	updates, err := p.Synchronize(context.Background(), []*api.PodSandbox{pod()}, []*api.Container{c})
	if err != nil {
		t.Fatal(err)
	}
	if len(updates) != 0 {
		t.Fatalf("got %d updates, want 0", len(updates))
	}
	if got := p.m.Counter("unhandled"); got != 1 {
		t.Errorf("unhandled = %d, want 1", got)
	}
}

// A pod that opted in by annotation while the plugin was down was created
// without it: no env, or env the pod wrote. It must be counted as unhandled
// (recreate), not silently ignored.
func TestSynchronizeAnnotationMarkerWithoutEnvIsUnhandled(t *testing.T) {
	p := testPlugin(t, "0,4", func(f *config.File) { f.AnnotationOptIn = true })
	podObj := pod()
	podObj.Annotations = map[string]string{names.OptInPrefix + "app": "250"}
	c := ctr("ctr-1", "1,5", nil, nil) // created while the plugin was down
	c.State = api.ContainerState_CONTAINER_RUNNING
	updates, err := p.Synchronize(context.Background(), []*api.PodSandbox{podObj}, []*api.Container{c})
	if err != nil {
		t.Fatal(err)
	}
	if len(updates) != 0 {
		t.Fatalf("got %d updates, want 0", len(updates))
	}
	if got := p.m.Counter("unhandled"); got != 1 {
		t.Errorf("unhandled = %d, want 1", got)
	}
}

// An update that shows the container is no longer an exclusive integer-CPU
// container (a CPU quota appeared) must not keep the shared set.
func TestUpdateIneligibleQuotaIsNotWidened(t *testing.T) {
	p := testPlugin(t, "0,4")
	c := ctr("ctr-1", "0,1,4,5", map[string]string{
		names.EnvIsolated: "1,5",
		names.EnvShared:   "0",
	}, map[string]string{names.MilliAnnotation: "250"})
	q := int64(200000)
	res := &api.LinuxResources{Cpu: &api.LinuxCPU{Cpus: "1,5", Quota: &api.OptionalInt64{Value: q}}}
	updates, err := p.UpdateContainer(context.Background(), pod(), c, res)
	if err != nil {
		t.Fatal(err)
	}
	if len(updates) != 0 {
		t.Fatalf("got %d updates for a container with a CPU quota, want 0", len(updates))
	}
	if got := p.m.Counter("ineligible"); got != 1 {
		t.Errorf("ineligible = %d, want 1", got)
	}
}

// The kubelet's CPU manager writes a cpuset with only cpuset_cpus set. The
// other CPU fields arrive as Go zero values (shares 0, quota 0), not as nil.
// That is "unchanged", not "shares were set to zero": the container is still
// an exclusive integer-CPU container and must keep (and get back) its shared
// CPUs. Regression: the shares test fired on 0 and dropped the shared CPUs on
// every kubelet reconcile.
func TestUpdateKubeletCpusetOnlyIsNotIneligible(t *testing.T) {
	p := testPlugin(t, "0,4")
	c := ctr("ctr-1", "0,1,5", map[string]string{
		names.EnvIsolated: "1,5",
		names.EnvShared:   "0",
	}, map[string]string{names.MilliAnnotation: "250"})
	// Exactly what a kubelet cpuset-only update looks like in NRI: the cpuset
	// is the exclusive CPUs, shares and quota are zero values.
	res := &api.LinuxResources{Cpu: &api.LinuxCPU{
		Cpus:   "1,5",
		Quota:  &api.OptionalInt64{Value: 0},
		Shares: &api.OptionalUInt64{Value: 0},
	}}
	updates, err := p.UpdateContainer(context.Background(), pod(), c, res)
	if err != nil {
		t.Fatal(err)
	}
	if len(updates) != 1 {
		t.Fatalf("got %d updates, want 1 (re-widen to 0,1,5)", len(updates))
	}
	if cpus := updates[0].GetLinux().GetResources().GetCpu().GetCpus(); !sameCPUs(t, cpus, "0,1,5") {
		t.Errorf("cpuset is %q, want 0,1,5", cpus)
	}
	if got := p.m.Counter("ineligible"); got != 0 {
		t.Errorf("ineligible = %d for a kubelet cpuset-only update, want 0", got)
	}
	if got := p.m.Counter("rewidened/update"); got != 1 {
		t.Errorf("rewidened/update = %d, want 1", got)
	}
}

// A live container that is no longer an exclusive integer-CPU container (a
// CPU quota appeared) must lose its shared CPUs at sync, not keep them.
func TestSynchronizeIneligibleQuotaDropsShared(t *testing.T) {
	p := testPlugin(t, "0,4")
	c := ctr("ctr-1", "0,1,5", map[string]string{
		names.EnvIsolated: "1,5",
		names.EnvShared:   "0",
		names.EnvMilli:    "250",
	}, map[string]string{names.MilliAnnotation: "250"})
	c.State = api.ContainerState_CONTAINER_RUNNING
	c.Linux.Resources.Cpu.Quota = &api.OptionalInt64{Value: 200000}
	updates, err := p.Synchronize(context.Background(), []*api.PodSandbox{pod()}, []*api.Container{c})
	if err != nil {
		t.Fatal(err)
	}
	if len(updates) != 1 {
		t.Fatalf("got %d updates, want 1 (shrink to the exclusive CPUs)", len(updates))
	}
	if cpus := updates[0].GetLinux().GetResources().GetCpu().GetCpus(); !sameCPUs(t, cpus, "1,5") {
		t.Errorf("cpuset is %q, want 1,5 (the shared CPU dropped)", cpus)
	}
	if got := p.m.Counter("ineligible"); got != 1 {
		t.Errorf("ineligible = %d, want 1", got)
	}
	if _, held := p.Table(); held != 0 {
		t.Errorf("held = %d for an ineligible container, want 0", held)
	}
}

// The same for cpu shares that no longer match the exclusive cpuset.
func TestSynchronizeIneligibleSharesDropsShared(t *testing.T) {
	p := testPlugin(t, "0,4")
	c := ctr("ctr-1", "0,1,5", map[string]string{
		names.EnvIsolated: "1,5",
		names.EnvShared:   "0",
		names.EnvMilli:    "250",
	}, map[string]string{names.MilliAnnotation: "250"})
	c.State = api.ContainerState_CONTAINER_RUNNING
	c.Linux.Resources.Cpu.Shares = &api.OptionalUInt64{Value: 100} // not 2 x 1024
	updates, err := p.Synchronize(context.Background(), []*api.PodSandbox{pod()}, []*api.Container{c})
	if err != nil {
		t.Fatal(err)
	}
	if len(updates) != 1 {
		t.Fatalf("got %d updates, want 1 (shrink to the exclusive CPUs)", len(updates))
	}
	if cpus := updates[0].GetLinux().GetResources().GetCpu().GetCpus(); !sameCPUs(t, cpus, "1,5") {
		t.Errorf("cpuset is %q, want 1,5 (the shared CPU dropped)", cpus)
	}
	if got := p.m.Counter("ineligible"); got != 1 {
		t.Errorf("ineligible = %d, want 1", got)
	}
}

// A marker in a namespace outside the allowlist grants nothing: the plugin
// must not touch that container at update time.
func TestUpdateNamespaceNotAllowedIsUntouched(t *testing.T) {
	p := testPlugin(t, "0,4")
	podObj := pod()
	podObj.Namespace = "kube-system"
	c := ctr("ctr-1", "1,5", map[string]string{
		names.EnvIsolated: "1,5",
		names.EnvShared:   "0",
		names.EnvMilli:    "250",
	}, map[string]string{names.MilliAnnotation: "250"})
	res := &api.LinuxResources{Cpu: &api.LinuxCPU{Cpus: "1,5"}}
	updates, err := p.UpdateContainer(context.Background(), podObj, c, res)
	if err != nil {
		t.Fatal(err)
	}
	if len(updates) != 0 {
		t.Fatalf("got %d updates for a namespace outside the allowlist, want 0", len(updates))
	}
}

func TestSynchronizeNamespaceNotAllowedIsUntouched(t *testing.T) {
	p := testPlugin(t, "0,4", func(f *config.File) { f.AnnotationOptIn = true })
	podObj := pod()
	podObj.Namespace = "kube-system"
	podObj.Annotations = map[string]string{names.OptInPrefix + "app": "250"}
	c := ctr("ctr-1", "1,5", map[string]string{
		names.EnvIsolated: "1,5",
		names.EnvShared:   "0",
		names.EnvMilli:    "250",
	}, nil)
	c.State = api.ContainerState_CONTAINER_RUNNING
	updates, err := p.Synchronize(context.Background(), []*api.PodSandbox{podObj}, []*api.Container{c})
	if err != nil {
		t.Fatal(err)
	}
	if len(updates) != 0 {
		t.Fatalf("got %d updates for a namespace outside the allowlist, want 0", len(updates))
	}
	if _, held := p.Table(); held != 0 {
		t.Fatalf("held = %d for a namespace outside the allowlist, want 0", held)
	}
}

// The pod annotation fallback is an opt-in, but only when annotationOptIn is
// on. With it off the annotation must not make a container ours.
func TestSynchronizeAnnotationIgnoredWhenOptInOff(t *testing.T) {
	p := testPlugin(t, "0,4") // annotationOptIn off
	podObj := pod()
	podObj.Annotations = map[string]string{names.OptInPrefix + "app": "250"}
	c := ctr("ctr-1", "1,5", map[string]string{
		names.EnvIsolated: "1,5",
		names.EnvShared:   "0",
		names.EnvMilli:    "250",
	}, nil)
	c.State = api.ContainerState_CONTAINER_RUNNING
	updates, err := p.Synchronize(context.Background(), []*api.PodSandbox{podObj}, []*api.Container{c})
	if err != nil {
		t.Fatal(err)
	}
	if len(updates) != 0 {
		t.Fatalf("got %d updates with annotationOptIn off, want 0", len(updates))
	}
	if _, held := p.Table(); held != 0 {
		t.Fatalf("held = %d with annotationOptIn off, want 0", held)
	}
}

// sameCPUs reports whether a kernel cpulist holds exactly the CPUs of want.
func sameCPUs(t *testing.T, got, want string) bool {
	t.Helper()
	g, err := cpuset.Parse(got)
	if err != nil {
		t.Fatalf("parse %q: %v", got, err)
	}
	w, err := cpuset.Parse(want)
	if err != nil {
		t.Fatalf("parse %q: %v", want, err)
	}
	return g.Equals(w)
}

// A seccomp policy that goes in through requestSeccomp and comes back out
// through toAPISeccomp must lose nothing: an argument filter or a per-rule
// errno that is dropped turns a restricted syscall into an unconditional one.
func TestSeccompRoundTripKeepsEveryField(t *testing.T) {
	e := func(v uint32) *api.OptionalUInt32 { return &api.OptionalUInt32{Value: v} }
	in := &api.LinuxSeccomp{
		DefaultAction:    "SCMP_ACT_ERRNO",
		DefaultErrno:     e(1),
		Architectures:    []string{"SCMP_ARCH_X86_64", "SCMP_ARCH_AARCH64"},
		Flags:            []string{"SECCOMP_FILTER_FLAG_TSYNC"},
		ListenerPath:     "/run/nri/seccomp.bpf",
		ListenerMetadata: "meta",
		Syscalls: []*api.LinuxSyscall{
			{
				Names:    []string{"clone"},
				Action:   "SCMP_ACT_ERRNO",
				ErrnoRet: e(38),
				Args: []*api.LinuxSeccompArg{
					{Index: 0, Value: 2147483648, Op: "SCMP_CMP_MASKED_EQ", ValueTwo: 2147483648},
					{Index: 1, Value: 3, Op: "SCMP_CMP_EQ"},
				},
			},
			{Names: []string{"read", "write"}, Action: "SCMP_ACT_ALLOW"},
		},
	}
	container := &api.Container{Linux: &api.LinuxContainer{SeccompPolicy: in}}
	got := toAPISeccomp(requestSeccomp(container))
	if got.GetDefaultAction() != in.DefaultAction {
		t.Errorf("default action %q, want %q", got.GetDefaultAction(), in.DefaultAction)
	}
	if got.GetDefaultErrno().GetValue() != in.DefaultErrno.GetValue() {
		t.Errorf("default errno %v, want %v", got.GetDefaultErrno(), in.DefaultErrno)
	}
	if strings.Join(got.GetArchitectures(), ",") != strings.Join(in.Architectures, ",") {
		t.Errorf("architectures %v, want %v", got.GetArchitectures(), in.Architectures)
	}
	if strings.Join(got.GetFlags(), ",") != strings.Join(in.Flags, ",") {
		t.Errorf("flags %v, want %v", got.GetFlags(), in.Flags)
	}
	if got.GetListenerPath() != in.ListenerPath {
		t.Errorf("listener path %q, want %q", got.GetListenerPath(), in.ListenerPath)
	}
	if got.GetListenerMetadata() != in.ListenerMetadata {
		t.Errorf("listener metadata %q, want %q", got.GetListenerMetadata(), in.ListenerMetadata)
	}
	if len(got.GetSyscalls()) != len(in.Syscalls) {
		t.Fatalf("syscalls %d, want %d", len(got.GetSyscalls()), len(in.Syscalls))
	}
	for i, want := range in.Syscalls {
		have := got.GetSyscalls()[i]
		if strings.Join(have.GetNames(), ",") != strings.Join(want.Names, ",") {
			t.Errorf("syscall %d names %v, want %v", i, have.GetNames(), want.Names)
		}
		if have.GetAction() != want.Action {
			t.Errorf("syscall %d action %q, want %q", i, have.GetAction(), want.Action)
		}
		if (have.GetErrnoRet() == nil) != (want.ErrnoRet == nil) {
			t.Fatalf("syscall %d errno presence %v, want %v", i, have.GetErrnoRet(), want.ErrnoRet)
		}
		if want.ErrnoRet != nil && have.GetErrnoRet().GetValue() != want.ErrnoRet.GetValue() {
			t.Errorf("syscall %d errno %v, want %v", i, have.GetErrnoRet(), want.ErrnoRet)
		}
		if len(have.GetArgs()) != len(want.Args) {
			t.Fatalf("syscall %d args %d, want %d", i, len(have.GetArgs()), len(want.Args))
		}
		for j, a := range want.Args {
			b := have.GetArgs()[j]
			if b.GetIndex() != a.Index || b.GetValue() != a.Value ||
				b.GetValueTwo() != a.ValueTwo || b.GetOp() != a.Op {
				t.Errorf("syscall %d arg %d = %+v, want %+v", i, j, b, a)
			}
		}
	}
}

// The io-uring widening appends one rule and keeps every restriction the
// runtime put on the rest of the policy.
func TestSeccompIOUringKeepsEveryRestriction(t *testing.T) {
	e := func(v uint32) *uint32 { return &v }
	in := &hft.SeccompPolicy{
		DefaultAction:    "SCMP_ACT_ERRNO",
		DefaultErrno:     e(1),
		Architectures:    []string{"SCMP_ARCH_X86_64"},
		Flags:            []string{"SECCOMP_FILTER_FLAG_TSYNC"},
		ListenerPath:     "/run/nri/seccomp.bpf",
		ListenerMetadata: "meta",
		Syscalls: []hft.SeccompSyscall{{
			Names:    []string{"clone"},
			Action:   "SCMP_ACT_ERRNO",
			ErrnoRet: e(38),
			Args:     []hft.SeccompArg{{Index: 0, Value: 1, Op: "SCMP_CMP_EQ"}},
		}},
	}
	out, err := hft.WithIOUring(in, hft.ModeIOUring)
	if err != nil {
		t.Fatal(err)
	}
	if out.DefaultErrno == nil || *out.DefaultErrno != 1 {
		t.Errorf("default errno %v, want 1", out.DefaultErrno)
	}
	if len(out.Flags) != 1 || out.Flags[0] != "SECCOMP_FILTER_FLAG_TSYNC" {
		t.Errorf("flags %v, want the request's", out.Flags)
	}
	if out.ListenerPath != in.ListenerPath || out.ListenerMetadata != in.ListenerMetadata {
		t.Errorf("listener %q/%q, want %q/%q",
			out.ListenerPath, out.ListenerMetadata, in.ListenerPath, in.ListenerMetadata)
	}
	var clone *hft.SeccompSyscall
	for i := range out.Syscalls {
		if len(out.Syscalls[i].Names) > 0 && out.Syscalls[i].Names[0] == "clone" {
			clone = &out.Syscalls[i]
		}
	}
	if clone == nil {
		t.Fatal("the clone rule must still be there")
	}
	if clone.ErrnoRet == nil || *clone.ErrnoRet != 38 {
		t.Errorf("clone errno %v, want 38", clone.ErrnoRet)
	}
	if len(clone.Args) != 1 || clone.Args[0] != (hft.SeccompArg{Index: 0, Value: 1, Op: "SCMP_CMP_EQ"}) {
		t.Errorf("clone args %v, want the request's filter", clone.Args)
	}
	// The input is untouched, including its slices.
	if in.Syscalls[0].ErrnoRet == nil || *in.Syscalls[0].ErrnoRet != 38 {
		t.Error("the request's rule was mutated")
	}
	if len(in.Syscalls) != 1 {
		t.Errorf("the request's rules were mutated: %v", in.Syscalls)
	}
}
