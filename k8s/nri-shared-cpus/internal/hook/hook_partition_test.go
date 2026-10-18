// SPDX-License-Identifier: Apache-2.0

package hook

import (
	"os"
	"path/filepath"
	"strings"
	"testing"

	"github.com/mimran1980/ergon/k8s/nri-shared-cpus/internal/cpuset"
)

func TestParseArgsSharedOnly(t *testing.T) {
	shared, part, err := ParseArgs([]string{"--shared", "0,4"})
	if err != nil {
		t.Fatal(err)
	}
	if shared.String() != "0,4" {
		t.Errorf("shared = %q", shared)
	}
	if part != nil {
		t.Errorf("partition = %+v, want none", part)
	}
}

func TestParseArgsPartitionOnly(t *testing.T) {
	shared, part, err := ParseArgs([]string{"--isolated", "1,5", "--partition-child", "/sys/fs/cgroup/pod/ctr/ergon-isolated"})
	if err != nil {
		t.Fatal(err)
	}
	if !shared.IsEmpty() {
		t.Errorf("shared = %q, want empty for a partition-only hook", shared)
	}
	if part == nil {
		t.Fatal("want a partition request")
	}
	if part.Exclusive.String() != "1,5" {
		t.Errorf("exclusive = %q", part.Exclusive)
	}
	if part.Child != "/sys/fs/cgroup/pod/ctr/ergon-isolated" {
		t.Errorf("child = %q", part.Child)
	}
}

func TestParseArgsBoth(t *testing.T) {
	shared, part, err := ParseArgs([]string{"--shared", "0", "--isolated", "1,5", "--partition-child", "/c/ergon-isolated"})
	if err != nil {
		t.Fatal(err)
	}
	if shared.String() != "0" || part == nil || part.Exclusive.String() != "1,5" {
		t.Fatalf("shared %q part %+v", shared, part)
	}
}

func TestParseArgsRefused(t *testing.T) {
	for _, tc := range []struct {
		name string
		args []string
	}{
		{"empty", nil},
		{"unknown", []string{"--nope"}},
		{"shared without value", []string{"--shared"}},
		{"shared empty", []string{"--shared", ""}},
		{"isolated without child", []string{"--isolated", "1,5"}},
		{"child without isolated", []string{"--partition-child", "/c/ergon-isolated"}},
		{"relative child", []string{"--isolated", "1,5", "--partition-child", "ergon-isolated"}},
		{"exclusive overlap shared", []string{"--shared", "1", "--isolated", "1,5", "--partition-child", "/c/ergon-isolated"}},
		{"bad exclusive", []string{"--isolated", "x", "--partition-child", "/c/ergon-isolated"}},
	} {
		t.Run(tc.name, func(t *testing.T) {
			if _, _, err := ParseArgs(tc.args); err == nil {
				t.Errorf("ParseArgs(%v) must refuse", tc.args)
			}
		})
	}
}

func TestKernelAtLeast(t *testing.T) {
	for _, tc := range []struct {
		release string
		want    bool
	}{
		{"6.1.0", true},
		{"6.1", true},
		{"6.2.0-100-aws", true},
		{"6.8.0", true},
		{"6.0.9", false},
		{"5.15.0-100-generic", false},
	} {
		got, err := KernelAtLeast(tc.release, 6, 1)
		if err != nil {
			t.Fatalf("%s: %v", tc.release, err)
		}
		if got != tc.want {
			t.Errorf("KernelAtLeast(%q, 6, 1) = %v, want %v", tc.release, got, tc.want)
		}
	}
	if _, err := KernelAtLeast("not-a-version", 6, 1); err == nil {
		t.Error("a release that does not parse must be an error")
	}
}

// writeTree is a tiny fake cgroup tree. Every level below root gets an empty
// cpuset.cpus.exclusive, which is what a real cgroup shows while nothing is
// reserved there, so a plan can union into it. The root has no such file.
func writeTree(t *testing.T, root, rel string) string {
	t.Helper()
	dir := filepath.Join(root, rel)
	if err := os.MkdirAll(dir, 0o755); err != nil {
		t.Fatal(err)
	}
	for cur := dir; cur != root && cur != "/"; cur = filepath.Dir(cur) {
		ex := filepath.Join(cur, "cpuset.cpus.exclusive")
		if _, err := os.Stat(ex); err == nil {
			continue
		}
		if err := os.WriteFile(ex, []byte("\n"), 0o644); err != nil {
			t.Fatal(err)
		}
	}
	return dir
}

// useTempLock points the partition lock at a scratch file so a test never
// touches /run and never races another test's lock.
func useTempLock(t *testing.T) {
	t.Helper()
	old := PartitionLockPath
	PartitionLockPath = filepath.Join(t.TempDir(), "partition.lock")
	t.Cleanup(func() { PartitionLockPath = old })
}

func TestApplyPartitionWritesInOrder(t *testing.T) {
	useTempLock(t)
	root := t.TempDir()
	ctrDir := writeTree(t, root, "kubepods/pod/ctr")
	// Pre-seed the ancestor so the exclusive writes have somewhere to go.
	writeTree(t, root, "kubepods/pod")
	plan, err := cpuset.PartitionPlan(root, ctrDir, cpuset.New(1, 5), cpuset.New(0))
	if err != nil {
		t.Fatal(err)
	}
	if err := ApplyPartition(plan); err != nil {
		t.Fatal(err)
	}
	child := plan.Child
	got, err := os.ReadFile(filepath.Join(child, "cpuset.cpus"))
	if err != nil {
		t.Fatal(err)
	}
	if strings.TrimSpace(string(got)) != "1,5" {
		t.Errorf("child cpuset.cpus = %q, want 1,5", got)
	}
	got, err = os.ReadFile(filepath.Join(child, "cpuset.cpus.partition"))
	if err != nil {
		t.Fatal(err)
	}
	if strings.TrimSpace(string(got)) != "isolated" {
		t.Errorf("child cpuset.cpus.partition = %q, want isolated", got)
	}
	// The exclusive files were written down the hierarchy.
	for _, dir := range []string{ctrDir, child} {
		got, err := os.ReadFile(filepath.Join(dir, "cpuset.cpus.exclusive"))
		if err != nil {
			t.Fatalf("%s: %v", dir, err)
		}
		if strings.TrimSpace(string(got)) != "1,5" {
			t.Errorf("%s/cpuset.cpus.exclusive = %q, want 1,5", dir, got)
		}
	}
}

// TestApplyPartitionKeepsASiblingsExclusiveCPUs is the P1 regression, end to
// end: two containers under one shared ancestor. The second create must add
// its CPUs to the ancestor's cpuset.cpus.exclusive, not replace it. Replacing
// it took the exclusive CPUs away from the partition the first pod was
// already running in.
func TestApplyPartitionKeepsASiblingsExclusiveCPUs(t *testing.T) {
	useTempLock(t)
	root := t.TempDir()
	// Both containers live under one pod slice, so they share every ancestor
	// above their own cgroup.
	podA := writeTree(t, root, "kubepods/podA.slice/cri-a.scope")
	podB := writeTree(t, root, "kubepods/podB.slice/cri-b.scope")
	shared := writeTree(t, root, "kubepods")

	planA, err := cpuset.PartitionPlan(root, podA, cpuset.New(1, 5), cpuset.New(0))
	if err != nil {
		t.Fatal(err)
	}
	if err := ApplyPartition(planA); err != nil {
		t.Fatal(err)
	}

	planB, err := cpuset.PartitionPlan(root, podB, cpuset.New(2, 3), cpuset.New(0))
	if err != nil {
		t.Fatal(err)
	}
	if err := ApplyPartition(planB); err != nil {
		t.Fatal(err)
	}

	// The shared ancestor holds both partitions' CPUs.
	got, err := os.ReadFile(filepath.Join(shared, "cpuset.cpus.exclusive"))
	if err != nil {
		t.Fatal(err)
	}
	have, err := cpuset.Parse(strings.TrimSpace(string(got)))
	if err != nil {
		t.Fatal(err)
	}
	want, err := cpuset.Parse("1,2,3,5")
	if err != nil {
		t.Fatal(err)
	}
	if !have.Equals(want) {
		t.Errorf("%s/cpuset.cpus.exclusive = %s, want %s (pod A's CPUs must survive pod B's create)",
			shared, have.List(), want.List())
	}
	// Pod A's own cgroup is untouched by pod B's create.
	got, err = os.ReadFile(filepath.Join(podA, "cpuset.cpus.exclusive"))
	if err != nil {
		t.Fatal(err)
	}
	if strings.TrimSpace(string(got)) != "1,5" {
		t.Errorf("pod A's cpuset.cpus.exclusive = %q, want 1,5", got)
	}
	// And each child still holds only its own CPUs.
	for _, tc := range []struct{ dir, want string }{
		{filepath.Join(podA, "ergon-isolated"), "1,5"},
		{filepath.Join(podB, "ergon-isolated"), "2,3"},
	} {
		got, err = os.ReadFile(filepath.Join(tc.dir, "cpuset.cpus"))
		if err != nil {
			t.Fatal(err)
		}
		if strings.TrimSpace(string(got)) != tc.want {
			t.Errorf("%s/cpuset.cpus = %q, want %s", tc.dir, got, tc.want)
		}
	}
}

// TestApplyPartitionFailsOnAWriteThatDoesNotStick: a kernel refusal, or a value
// that does not read back as written, must fail the create. Claiming an
// isolation that is not there is worse than refusing to start.
func TestApplyPartitionFailsOnAWriteThatDoesNotStick(t *testing.T) {
	useTempLock(t)
	root := t.TempDir()
	ctrDir := writeTree(t, root, "kubepods/pod/ctr")
	plan, err := cpuset.PartitionPlan(root, ctrDir, cpuset.New(1), cpuset.Set{})
	if err != nil {
		t.Fatal(err)
	}
	// Make the partition file a directory: the write cannot stick.
	if err := os.MkdirAll(filepath.Join(plan.Child, "cpuset.cpus.partition"), 0o755); err != nil {
		t.Fatal(err)
	}
	// The mkdir in the plan is fine with an existing dir; the verify must fail.
	err = ApplyPartition(plan)
	if err == nil {
		t.Fatal("a partition that does not read back as isolated must fail closed")
	}
	if !strings.Contains(err.Error(), "isolated") {
		t.Errorf("error %q must say what was wanted", err)
	}
}

// TestApplyPartitionFailsWhenTheAncestorFileIsMissing: the plan must not
// invent an empty reservation for a path that does not exist. A missing
// cpuset.cpus.exclusive means the path is wrong or the cpuset controller is
// not on there, and guessing would hide that.
func TestApplyPartitionFailsWhenTheAncestorFileIsMissing(t *testing.T) {
	useTempLock(t)
	root := t.TempDir()
	ctrDir := writeTree(t, root, "kubepods/pod/ctr")
	if err := os.Remove(filepath.Join(ctrDir, "cpuset.cpus.exclusive")); err != nil {
		t.Fatal(err)
	}
	plan, err := cpuset.PartitionPlan(root, ctrDir, cpuset.New(1), cpuset.Set{})
	if err != nil {
		t.Fatal(err)
	}
	if err := ApplyPartition(plan); err == nil {
		t.Fatal("a missing ancestor cpuset.cpus.exclusive must fail closed")
	}
}

func TestVerifyPartition(t *testing.T) {
	dir := t.TempDir()
	if err := verifyPartition(dir); err == nil {
		t.Error("a missing cpuset.cpus.partition must fail")
	}
	if err := os.WriteFile(filepath.Join(dir, "cpuset.cpus.partition"), []byte("root\n"), 0o644); err != nil {
		t.Fatal(err)
	}
	if err := verifyPartition(dir); err == nil {
		t.Error("reading back \"root\" must fail closed")
	}
	if err := os.WriteFile(filepath.Join(dir, "cpuset.cpus.partition"), []byte("isolated\n"), 0o644); err != nil {
		t.Fatal(err)
	}
	if err := verifyPartition(dir); err != nil {
		t.Errorf("reading back \"isolated\\n\" must pass, got %v", err)
	}
}
