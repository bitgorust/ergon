// SPDX-License-Identifier: Apache-2.0

package config

import (
	"os"
	"path/filepath"
	"strconv"
	"testing"
)

// fakeNuma builds a sysfs node directory with the given node<N> directories
// and the files the real one carries beside them.
func fakeNuma(t *testing.T, nodes ...int) string {
	t.Helper()
	root := t.TempDir()
	dir := filepath.Join(root, "devices/system/node")
	if err := os.MkdirAll(dir, 0o755); err != nil {
		t.Fatal(err)
	}
	for _, name := range []string{"online", "possible", "has_cpu", "has_memory"} {
		if err := os.WriteFile(filepath.Join(dir, name), []byte("0-1\n"), 0o644); err != nil {
			t.Fatal(err)
		}
	}
	for _, n := range nodes {
		if err := os.Mkdir(filepath.Join(dir, "node"+strconv.Itoa(n)), 0o755); err != nil {
			t.Fatal(err)
		}
	}
	return root
}

func TestDiscoverHostNuma(t *testing.T) {
	c := &Config{}
	if err := DiscoverHostNuma(c, fakeNuma(t, 0, 1)); err != nil {
		t.Fatal(err)
	}
	if len(c.NodeNumaNodes) != 2 || c.NodeNumaNodes[0] != 0 || c.NodeNumaNodes[1] != 1 {
		t.Errorf("NodeNumaNodes = %v, want [0 1]", c.NodeNumaNodes)
	}
}

func TestDiscoverHostNumaSingle(t *testing.T) {
	c := &Config{}
	if err := DiscoverHostNuma(c, fakeNuma(t, 0)); err != nil {
		t.Fatal(err)
	}
	if len(c.NodeNumaNodes) != 1 || c.NodeNumaNodes[0] != 0 {
		t.Errorf("NodeNumaNodes = %v, want [0]", c.NodeNumaNodes)
	}
}

// A kernel built without CONFIG_NUMA has no node directory at all. The
// machine still has one node, and cpuset.mems names it 0, so that is what
// the list says. This is the only case where the list is not read from
// node<N> directories, and it is still a real answer to bind to.
func TestDiscoverHostNumaWithoutANodeDirectory(t *testing.T) {
	c := &Config{}
	if err := DiscoverHostNuma(c, t.TempDir()); err != nil {
		t.Fatal(err)
	}
	if len(c.NodeNumaNodes) != 1 || c.NodeNumaNodes[0] != 0 {
		t.Errorf("NodeNumaNodes = %v, want [0]", c.NodeNumaNodes)
	}
}

// A node directory with no node<N> entry cannot be answered from: the
// plugin must not fall back to "no NUMA nodes" and drop a binding request.
func TestDiscoverHostNumaRefusesAnEmptyDirectory(t *testing.T) {
	c := &Config{}
	err := DiscoverHostNuma(c, fakeNuma(t))
	if err == nil {
		t.Fatal("want a failure when the node directory names no node")
	}
	if len(c.NodeNumaNodes) != 0 {
		t.Errorf("NodeNumaNodes = %v, want nothing recorded", c.NodeNumaNodes)
	}
}

func TestDiscoverHostNumaIgnoresOtherNames(t *testing.T) {
	root := fakeNuma(t, 0, 1)
	dir := filepath.Join(root, "devices/system/node")
	for _, name := range []string{"node", "nodes", "node-1", "cpu0"} {
		if err := os.Mkdir(filepath.Join(dir, name), 0o755); err != nil {
			t.Fatal(err)
		}
	}
	c := &Config{}
	if err := DiscoverHostNuma(c, root); err != nil {
		t.Fatal(err)
	}
	if len(c.NodeNumaNodes) != 2 || c.NodeNumaNodes[0] != 0 || c.NodeNumaNodes[1] != 1 {
		t.Errorf("NodeNumaNodes = %v, want [0 1]", c.NodeNumaNodes)
	}
}
