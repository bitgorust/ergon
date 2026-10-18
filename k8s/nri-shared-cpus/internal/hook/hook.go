// SPDX-License-Identifier: Apache-2.0

// Package hook implements the OCI createRuntime hook that starts a container
// on its shared CPUs and, when asked, puts its exclusive CPUs in an isolated
// cpuset partition. The hook runs on the node, after the cgroup cpuset is
// written and before the user program starts. Every thread the program later
// creates inherits the mask.
//
// The isolated child takes whole processes, not single threads. cgroup.procs
// moves a thread group and cgroup.threads only exists in a threaded subtree,
// which cannot use cpuset partitions, so one hot thread of a multi-threaded
// program stays in the container cgroup and is placed with sched_setaffinity
// (see PinProcess and the ergon-cpus wrapper). Use the partition when every
// thread of the process belongs on the exclusive CPUs.
package hook

import (
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"io/fs"
	"os"
	"path/filepath"
	"strconv"
	"strings"

	"golang.org/x/sys/unix"

	"github.com/mimran1980/ergon/k8s/nri-shared-cpus/internal/cpuset"
)

// MaxStateBytes is the largest OCI state JSON we read from stdin.
const MaxStateBytes = 1 << 20

// MaxPasses bounds the TID scan: runc init can add threads while we work.
const MaxPasses = 5

// TimerSlackNS is the timer slack the hook gives every container it touches.
// The kernel's default is 50,000 ns: it is added to every sleep and to every
// epoll/io_uring timeout, so a 100 µs wait can take 150 µs. 1 ns is the
// kernel's floor, and a sleep can then fire as soon as the timeout expires.
const TimerSlackNS = 1

// State is the OCI runtime state JSON runc writes to a hook's stdin.
type State struct {
	// Pid is the container's init PID (host namespace).
	Pid int `json:"pid"`
	// ID is the container ID, for error messages.
	ID string `json:"id"`
}

// PartitionRequest is the isolated child cgroup the hook sets up. The child
// holds only Exclusive, and the kernel keeps everything else load-balanced.
type PartitionRequest struct {
	// Exclusive is the CPU list the child holds.
	Exclusive cpuset.Set
	// Child is the absolute path of the child cgroup to create.
	Child string
}

// ParseArgs reads the hook's command line: --shared S starts the container on
// the shared CPUs, and --isolated E --partition-child PATH builds the isolated
// child cgroup. At least one of the two must be present. The exclusive CPUs
// must not overlap the shared ones: an isolated shared CPU would take load
// balancing away from the helpers.
func ParseArgs(args []string) (shared cpuset.Set, part *PartitionRequest, err error) {
	var sharedText, isolatedText, child string
	for i := 0; i < len(args); i++ {
		flag := args[i]
		switch flag {
		case "--shared", "--isolated", "--partition-child":
		default:
			return shared, nil, fmt.Errorf("hook: unknown argument %q", flag)
		}
		if i+1 >= len(args) || args[i+1] == "" {
			return shared, nil, fmt.Errorf("hook: %s needs a value", flag)
		}
		value := args[i+1]
		i++
		switch flag {
		case "--shared":
			sharedText = value
		case "--isolated":
			isolatedText = value
		case "--partition-child":
			child = value
		}
	}
	if sharedText == "" && isolatedText == "" {
		return shared, nil, fmt.Errorf("hook: --shared or --isolated is required")
	}
	if sharedText != "" {
		shared, err = cpuset.Parse(sharedText)
		if err != nil {
			return shared, nil, fmt.Errorf("hook: --shared: %w", err)
		}
	}
	if isolatedText != "" || child != "" {
		if isolatedText == "" || child == "" {
			return shared, nil, fmt.Errorf("hook: --isolated and --partition-child go together")
		}
		if !filepath.IsAbs(child) || filepath.Clean(child) != child {
			return shared, nil, fmt.Errorf("hook: --partition-child %q must be a clean absolute path", child)
		}
		excl, err := cpuset.Parse(isolatedText)
		if err != nil {
			return shared, nil, fmt.Errorf("hook: --isolated: %w", err)
		}
		if excl.IsEmpty() {
			return shared, nil, fmt.Errorf("hook: --isolated is empty")
		}
		if both := excl.Intersection(shared); !both.IsEmpty() {
			return shared, nil, fmt.Errorf("hook: CPU %s is both exclusive and shared; an isolated partition must not hold shared CPUs", both)
		}
		part = &PartitionRequest{Exclusive: excl, Child: child}
	}
	return shared, part, nil
}

// KernelAtLeast reports whether a kernel release is at least major.minor.
// The isolated partition needs 6.1 (cpuset.cpus.partition).
func KernelAtLeast(release string, major, minor int) (bool, error) {
	parts := strings.SplitN(release, ".", 3)
	if len(parts) < 2 {
		return false, fmt.Errorf("kernel release %q does not parse", release)
	}
	maj, err1 := strconv.Atoi(parts[0])
	minText := parts[1]
	if i := strings.IndexFunc(minText, func(r rune) bool { return r < '0' || r > '9' }); i >= 0 {
		minText = minText[:i]
	}
	mnr, err2 := strconv.Atoi(minText)
	if err1 != nil || err2 != nil {
		return false, fmt.Errorf("kernel release %q does not parse", release)
	}
	return maj > major || (maj == major && mnr >= minor), nil
}

// Run is the hook entry point: read the state from stdin, set up the isolated
// child cgroup when asked, set every thread of the container's init to shared
// when asked, and check both results. Any failure prints a message to stderr
// and returns a non-nil error so the caller exits 1 (runc then fails the
// create: fail closed).
func Run(stdin io.Reader, stderr io.Writer, shared cpuset.Set, part *PartitionRequest) error {
	if shared.IsEmpty() && part == nil {
		return fmt.Errorf("hook: nothing to do (no shared CPUs and no partition)")
	}
	data, err := io.ReadAll(io.LimitReader(stdin, MaxStateBytes+1))
	if err != nil {
		return fmt.Errorf("hook: read state: %w", err)
	}
	if len(data) > MaxStateBytes {
		return fmt.Errorf("hook: state is larger than %d bytes", MaxStateBytes)
	}
	var st State
	if err := json.Unmarshal(data, &st); err != nil {
		return fmt.Errorf("hook: state does not parse: %w", err)
	}
	if st.Pid <= 0 {
		return fmt.Errorf("hook: state has no pid")
	}
	for _, c := range shared.Slice() {
		if c >= maxCPUs {
			return fmt.Errorf("hook: CPU %d is outside cpu_set_t (max %d)", c, maxCPUs-1)
		}
	}
	// Partition first: a container that is not isolated must never start.
	if part != nil {
		if err := setupPartition(st.Pid, part); err != nil {
			return err
		}
		fmt.Fprintf(stderr, "ergon-cpus hook: container %s (pid %d) isolated %s in %s\n",
			st.ID, st.Pid, part.Exclusive.List(), part.Child)
	}
	// Timer slack next: it is inherited by every thread and child the program
	// later makes, so one write on the threads that exist now covers the tree.
	// A container that cannot have it must not start with the kernel default.
	if err := SetTimerSlack(st.Pid, TimerSlackNS); err != nil {
		return err
	}
	fmt.Fprintf(stderr, "ergon-cpus hook: container %s (pid %d) timer slack %d ns\n",
		st.ID, st.Pid, TimerSlackNS)
	if !shared.IsEmpty() {
		if err := PinProcess(st.Pid, shared); err != nil {
			return err
		}
		fmt.Fprintf(stderr, "ergon-cpus hook: container %s (pid %d) starts on %s\n", st.ID, st.Pid, shared.List())
	}
	return nil
}

// setupPartition builds the isolated child cgroup for pid. The child path is
// the one the plugin exported (ERGON_ISOLATED_CGROUP); its parent must be the
// container's own cgroup, or the create is refused.
func setupPartition(pid int, part *PartitionRequest) error {
	ok, err := KernelAtLeast(kernelRelease(), 6, 1)
	if err != nil {
		return fmt.Errorf("hook: %w", err)
	}
	if !ok {
		return fmt.Errorf("hook: kernel %s is older than 6.1: cpuset.cpus.partition needs 6.1", kernelRelease())
	}
	cgroup, err := cgroupOf(pid)
	if err != nil {
		return err
	}
	parent := filepath.Dir(part.Child)
	if parent != cgroup {
		return fmt.Errorf("hook: the isolated child %s is not under the container cgroup %s", part.Child, cgroup)
	}
	root, err := cgroupRootOf()
	if err != nil {
		return err
	}
	plan, err := cpuset.PartitionPlan(root, cgroup, part.Exclusive, cpuset.Set{})
	if err != nil {
		return fmt.Errorf("hook: %w", err)
	}
	if plan.Child != part.Child {
		return fmt.Errorf("hook: the planned child %s is not the requested %s", plan.Child, part.Child)
	}
	if err := ApplyPartition(plan); err != nil {
		return err
	}
	return nil
}

// PartitionLockPath is the lock file serialising partition plans across
// container creates. Two OCI hooks can run at once on one node and both read
// and rewrite the same ancestors' cpuset.cpus.exclusive; without a common
// lock the second one's read-modify-write can lose the first one's CPUs.
// It lives outside the cgroup filesystem, which cannot hold a lock file.
var PartitionLockPath = "/run/ergon-shared-cpus.partition.lock"

// ApplyPartition performs the ordered writes of a partition plan and checks
// that every one of them stuck. The kernel owns the order (cgroup-v2.rst,
// "remote partition"): cpuset.cpus.exclusive has to be written down the
// hierarchy before cpuset.cpus.partition=isolated is accepted.
//
// Ancestors' cpuset.cpus.exclusive is a union with what is already reserved
// there, under one lock, so a second container's create cannot drop the first
// container's CPUs. A write the kernel refuses, or one that does not read
// back as written, is a failure: a container must not start claiming an
// isolation it does not have.
func ApplyPartition(plan *cpuset.Plan) error {
	unlock, err := lockPartition()
	if err != nil {
		return err
	}
	defer unlock()
	for _, w := range plan.Writes {
		switch w.Kind {
		case cpuset.WriteMkdir:
			if err := os.MkdirAll(w.Path, 0o755); err != nil {
				return fmt.Errorf("hook: create %s: %w", w.Path, err)
			}
		case cpuset.WriteExclusiveAdd:
			value, err := unionInto(w.Path, w.Value)
			if err != nil {
				return fmt.Errorf("hook: reserve %s in %s: %w", w.Value, w.Path, err)
			}
			if err := writeCPUList(w.Path, value); err != nil {
				return err
			}
		case cpuset.WriteCpus, cpuset.WriteExclusive:
			if err := writeCPUList(w.Path, w.Value); err != nil {
				return err
			}
		default:
			if err := writeAndVerify(w.Path, w.Value); err != nil {
				return err
			}
		}
	}
	return verifyPartition(plan.Child)
}

// lockPartition takes the exclusive partition lock for the duration of a
// plan. The unlock function is returned so the caller can defer it.
func lockPartition() (func(), error) {
	f, err := os.OpenFile(PartitionLockPath, os.O_CREATE|os.O_RDWR, 0o644)
	if err != nil {
		return nil, fmt.Errorf("hook: open the partition lock %s: %w", PartitionLockPath, err)
	}
	if err := unix.Flock(int(f.Fd()), unix.LOCK_EX); err != nil {
		_ = f.Close()
		return nil, fmt.Errorf("hook: lock %s: %w", PartitionLockPath, err)
	}
	return func() {
		_ = unix.Flock(int(f.Fd()), unix.LOCK_UN)
		_ = f.Close()
	}, nil
}

// unionInto reads the CPU list already reserved at path and returns it with
// add's CPUs merged in. An empty file is "nothing reserved yet", which is what
// a real cpuset.cpus.exclusive shows on a cgroup holding no exclusive CPUs. A
// missing or unreadable file is an error, not an empty reservation: guessing
// there would either drop another partition's CPUs or hide a path that does
// not exist.
func unionInto(path, add string) (string, error) {
	current, err := os.ReadFile(path)
	if err != nil {
		return "", err
	}
	base := cpuset.Set{}
	if text := strings.TrimSpace(string(current)); text != "" {
		base, err = cpuset.Parse(text)
		if err != nil {
			return "", fmt.Errorf("parse the existing reservation %q: %w", current, err)
		}
	}
	more, err := cpuset.Parse(add)
	if err != nil {
		return "", err
	}
	return base.Union(more).List(), nil
}

// writeCPUList writes a CPU list and reads it back as a set. A write the kernel
// accepts silently and does not keep is a failure: cpuset.cpus.exclusive is a
// replace, so a value that did not stick would leave the plan claiming CPUs it
// does not hold. The comparison is on the CPU set, not the text, because the
// kernel may print "2-3" where we wrote "2,3".
func writeCPUList(path, value string) error {
	if err := os.WriteFile(path, []byte(value), 0o644); err != nil {
		return fmt.Errorf("hook: write %s=%q: %w", path, value, err)
	}
	got, err := os.ReadFile(path)
	if err != nil {
		return fmt.Errorf("hook: read back %s: %w", path, err)
	}
	want, err := cpuset.Parse(value)
	if err != nil {
		return err
	}
	have, err := cpuset.Parse(strings.TrimSpace(string(got)))
	if err != nil {
		return fmt.Errorf("hook: %s does not read back as a CPU list (%q): %w", path, got, err)
	}
	if !have.Equals(want) {
		return fmt.Errorf("hook: %s is %s after writing %s", path, have.List(), want.List())
	}
	return nil
}

// writeAndVerify writes value to path and reads the same text back. It is for
// the files that are not CPU lists (cpuset.cpus.partition).
func writeAndVerify(path, value string) error {
	if err := os.WriteFile(path, []byte(value), 0o644); err != nil {
		return fmt.Errorf("hook: write %s=%q: %w", path, value, err)
	}
	got, err := os.ReadFile(path)
	if err != nil {
		return fmt.Errorf("hook: read back %s: %w", path, err)
	}
	if strings.TrimSpace(string(got)) != value {
		return fmt.Errorf("hook: %s is %q after writing %q", path, strings.TrimSpace(string(got)), value)
	}
	return nil
}

// verifyPartition reads cpuset.cpus.partition back. Anything but "isolated"
// is a failure: the create must not continue without the isolation.
func verifyPartition(child string) error {
	data, err := os.ReadFile(filepath.Join(child, "cpuset.cpus.partition"))
	if err != nil {
		return fmt.Errorf("hook: read %s/cpuset.cpus.partition: %w", child, err)
	}
	if got := strings.TrimSpace(string(data)); got != "isolated" {
		return fmt.Errorf("hook: %s/cpuset.cpus.partition is %q, wanted isolated", child, got)
	}
	return nil
}

// kernelRelease reads /proc/sys/kernel/osrelease.
func kernelRelease() string {
	data, err := os.ReadFile("/proc/sys/kernel/osrelease")
	if err != nil {
		return ""
	}
	return strings.TrimSpace(string(data))
}

// cgroupOf returns the absolute cgroup v2 path of pid.
func cgroupOf(pid int) (string, error) {
	data, err := os.ReadFile(filepath.Join("/proc", strconv.Itoa(pid), "cgroup"))
	if err != nil {
		return "", fmt.Errorf("hook: read /proc/%d/cgroup: %w", pid, err)
	}
	for _, line := range strings.Split(string(data), "\n") {
		// "0::<path>" is the cgroup v2 entry.
		rest, ok := strings.CutPrefix(line, "0::")
		if !ok {
			continue
		}
		rel := strings.TrimSpace(rest)
		if rel == "" {
			return "", fmt.Errorf("hook: /proc/%d/cgroup has an empty cgroup v2 path", pid)
		}
		root, err := cgroupRootOf()
		if err != nil {
			return "", err
		}
		if strings.HasPrefix(rel, "/") {
			rel = rel[1:]
		}
		return filepath.Join(root, rel), nil
	}
	return "", fmt.Errorf("hook: /proc/%d/cgroup has no cgroup v2 entry", pid)
}

// cgroupRootOf finds the cgroup v2 mount point.
func cgroupRootOf() (string, error) {
	data, err := os.ReadFile("/proc/self/mountinfo")
	if err != nil {
		return "", fmt.Errorf("hook: read /proc/self/mountinfo: %w", err)
	}
	for _, line := range strings.Split(string(data), "\n") {
		fields := strings.Fields(line)
		// mountinfo: ... - fstype source superopts
		i := indexOf(fields, "-")
		if i < 0 || i+1 >= len(fields) || fields[i+1] != "cgroup2" {
			continue
		}
		if i < 4 {
			continue
		}
		return fields[4], nil
	}
	return "", fmt.Errorf("hook: cgroup v2 is not mounted")
}

func indexOf(fields []string, s string) int {
	for i, f := range fields {
		if f == s {
			return i
		}
	}
	return -1
}

// PinProcess sets every thread of pid to shared and checks the result.
// SetTimerSlack writes ns as the timer slack of every thread of pid, and the
// kernel then copies the value to every thread and child the process later
// makes (copy_process). Both halves are needed: a thread that already exists
// keeps its own slack, so writing only pid would leave those at the default.
// ns of 1 is the kernel's floor and removes the 50,000 ns default that would
// otherwise be added to every sleep and every epoll/io_uring timeout.
//
// Access to /proc/<pid>/timerslack_ns is the same for reading and writing
// (timerslack_ns_show and timerslack_ns_write, fs/proc/base.c): free only
// when the file's task is the caller (`p == current`), otherwise
// CAP_SYS_NICE over that task's user namespace, and it is refused with
// EPERM. There is no thread-group exception, so a sibling thread reading
// /proc/self/timerslack_ns is refused (that path resolves to the leader,
// fs/proc/self.c). The hook both writes the threads and reads the result
// back, so it needs the capability. It has it: containerd runs hooks as root
// with full capabilities, and the DaemonSet's `capabilities: drop: ["ALL"]`
// applies to the plugin process only. A program can always set its own slack
// with prctl(PR_SET_TIMERSLACK), which has no permission check, and can read
// its own thread's slack at /proc/thread-self/timerslack_ns.
//
// Writes to threads that vanish mid-scan are not errors; everything else is,
// and a write that does not stick is an error.
//
// Errors: ns is below the kernel's floor, the process is gone, or the kernel
// refused the write.
func SetTimerSlack(pid int, ns int64) error {
	if ns < 1 {
		return fmt.Errorf("hook: pid %d: timer slack %d is below the kernel's floor of 1", pid, ns)
	}
	tids, err := tidsOf(pid)
	if err != nil {
		return fmt.Errorf("hook: pid %d: %w", pid, err)
	}
	text := strconv.FormatInt(ns, 10)
	written := 0
	for _, tid := range tids {
		path := filepath.Join("/proc", strconv.Itoa(tid), "timerslack_ns")
		if err := os.WriteFile(path, []byte(text), 0o644); err != nil {
			if errors.Is(err, unix.ESRCH) || errors.Is(err, fs.ErrNotExist) {
				continue
			}
			if errors.Is(err, unix.EPERM) {
				return fmt.Errorf("hook: pid %d tid %d: write %s: %w (need CAP_SYS_NICE over the task; the OCI hook has it)", pid, tid, path, err)
			}
			return fmt.Errorf("hook: pid %d tid %d: write %s: %w", pid, tid, path, err)
		}
		written++
	}
	if written == 0 {
		return fmt.Errorf("hook: pid %d: every thread vanished before its timer slack was written", pid)
	}
	// Read back on the process's own task: a write that did not stick must
	// not be reported as done.
	got, err := readTimerSlack(pid)
	if err != nil {
		return fmt.Errorf("hook: pid %d: %w", pid, err)
	}
	if got != ns {
		return fmt.Errorf("hook: pid %d: timer slack is %d ns, wanted %d ns", pid, got, ns)
	}
	return nil
}

// readTimerSlack returns the timer slack of pid's own task, in nanoseconds.
// Reading a task that is not the caller needs CAP_SYS_NICE (see SetTimerSlack);
// the hook has it, and a refusal here is a failure, not a missing answer.
func readTimerSlack(pid int) (int64, error) {
	data, err := os.ReadFile(filepath.Join("/proc", strconv.Itoa(pid), "timerslack_ns"))
	if err != nil {
		return 0, fmt.Errorf("read /proc/%d/timerslack_ns: %w", pid, err)
	}
	ns, err := strconv.ParseInt(strings.TrimSpace(string(data)), 10, 64)
	if err != nil {
		return 0, fmt.Errorf("parse /proc/%d/timerslack_ns %q: %w", pid, data, err)
	}
	return ns, nil
}

func PinProcess(pid int, shared cpuset.Set) error {
	mask := maskOf(shared)
	for pass := 0; pass < MaxPasses; pass++ {
		tids, err := tidsOf(pid)
		if err != nil {
			return fmt.Errorf("hook: pid %d: %w", pid, err)
		}
		newTIDs := 0
		for _, tid := range tids {
			if err := unix.SchedSetaffinity(tid, &mask); err != nil {
				// A thread can exit between the scan and the call; that is
				// not an error. Anything else is.
				if err != unix.ESRCH {
					return fmt.Errorf("hook: pid %d tid %d: sched_setaffinity: %w", pid, tid, err)
				}
				continue
			}
			newTIDs++
		}
		if newTIDs == 0 && pass > 0 {
			break
		}
	}
	var got unix.CPUSet
	if err := unix.SchedGetaffinity(pid, &got); err != nil {
		return fmt.Errorf("hook: pid %d: sched_getaffinity: %w", pid, err)
	}
	if !sameMask(got, mask) {
		return fmt.Errorf("hook: pid %d: affinity is %s, wanted %s", pid, formatMask(got), shared.List())
	}
	return nil
}

// tidsOf lists the thread IDs of a process.
func tidsOf(pid int) ([]int, error) {
	entries, err := os.ReadDir(filepath.Join("/proc", strconv.Itoa(pid), "task"))
	if err != nil {
		return nil, fmt.Errorf("read /proc/%d/task: %w", pid, err)
	}
	var tids []int
	for _, e := range entries {
		tid, err := strconv.Atoi(e.Name())
		if err != nil {
			continue
		}
		tids = append(tids, tid)
	}
	if len(tids) == 0 {
		return nil, fmt.Errorf("/proc/%d/task is empty", pid)
	}
	return tids, nil
}

// maskOf turns a CPU set into a kernel CPU mask.
func maskOf(s cpuset.Set) unix.CPUSet {
	var mask unix.CPUSet
	for _, c := range s.Slice() {
		mask.Set(c)
	}
	return mask
}

// maxCPUs is the kernel's cpu_set_t size we build (1024 bits).
const maxCPUs = 1024

func sameMask(a, b unix.CPUSet) bool {
	for c := 0; c < maxCPUs; c++ {
		if a.IsSet(c) != b.IsSet(c) {
			return false
		}
	}
	return true
}

func formatMask(m unix.CPUSet) string {
	parts := make([]string, 0, 8)
	for c := 0; c < maxCPUs; c++ {
		if m.IsSet(c) {
			parts = append(parts, strconv.Itoa(c))
		}
	}
	return strings.Join(parts, ",")
}
