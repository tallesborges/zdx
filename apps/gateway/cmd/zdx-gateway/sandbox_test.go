package main

import (
	"bytes"
	"errors"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"testing"

	"github.com/tallesborges/zdx/apps/gateway/internal/sandbox"
)

// Every test here runs in a sandbox: HOME, ZDX_HOME and TMPDIR are fresh
// directories, and zdx on PATH is a stub that fails. `bot` and `preflight`
// read ZDX_HOME, so an inherited one must never reach them.
func TestMain(m *testing.M) { os.Exit(sandbox.Main(m.Run, true)) }

// The incident: `bot` with an inherited ZDX_HOME reached the live bot's
// pidfile. An inherited home outside the sandbox is refused before the
// pidfile is opened, and preflight reports it without running zdx.
func TestInheritedZdxHomeIsRefusedBeforeThePidfile(t *testing.T) {
	root := os.Getenv(sandbox.Env)
	outside, err := os.MkdirTemp(filepath.Dir(root), "outside")
	if err != nil {
		t.Fatal(err)
	}
	defer os.RemoveAll(outside)
	marker := filepath.Join(outside, "zdx-ran")
	zdx := filepath.Join(t.TempDir(), "zdx")
	if err := os.WriteFile(zdx, []byte("#!/bin/sh\ntouch '"+marker+"'\n"), 0o755); err != nil {
		t.Fatal(err)
	}
	t.Setenv("ZDX_HOME", outside)
	if err := run([]string{"--root", t.TempDir(), "bot", "--zdx", zdx}, nil, &bytes.Buffer{}); !errors.Is(err, sandbox.ErrOutside) {
		t.Fatalf("bot: want a sandbox refusal, got %v", err)
	}
	var out bytes.Buffer
	if err := run([]string{"preflight", "--root", t.TempDir(), "--json", "--zdx", zdx}, nil, &out); err != nil {
		t.Fatal(err)
	}
	if !strings.Contains(out.String(), `"ready":false`) || !strings.Contains(out.String(), "test sandbox") {
		t.Fatalf("preflight: %s", out.String())
	}
	if entries, _ := os.ReadDir(outside); len(entries) != 0 {
		t.Fatalf("nothing is written outside the sandbox: %v", entries)
	}
	// zdx on PATH is the failing stub.
	if p, err := exec.LookPath("zdx"); err != nil || !strings.HasPrefix(p, root+"/") {
		t.Fatalf("zdx on PATH is %q (%v)", p, err)
	}
}
