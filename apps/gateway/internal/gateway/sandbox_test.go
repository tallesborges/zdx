package gateway

import (
	"context"
	"errors"
	"os"
	"path/filepath"
	"strings"
	"testing"

	"github.com/tallesborges/zdx/apps/gateway/internal/sandbox"
)

// outsideSandbox is a writable directory outside this binary's sandbox:
// without the guard, a write there would succeed.
func outsideSandbox(t *testing.T) string {
	t.Helper()
	root := os.Getenv(sandbox.Env)
	if root == "" {
		t.Fatal("TestMain did not enter the sandbox")
	}
	dir, err := os.MkdirTemp(filepath.Dir(root), "outside")
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { os.RemoveAll(dir) })
	return dir
}

// The test process runs in its sandbox: an inherited ZDX_HOME or HOME (an
// agent run sets ZDX_HOME to the live one) never reaches a test.
func TestTestsRunInTheSandbox(t *testing.T) {
	root := os.Getenv(sandbox.Env)
	for _, k := range []string{"HOME", "ZDX_HOME", "TMPDIR"} {
		if v := os.Getenv(k); root == "" || !strings.HasPrefix(v, root+"/") {
			t.Fatalf("%s=%q is outside the sandbox %q", k, v, root)
		}
	}
	if os.Getenv(SupervisorEnv) != "" {
		t.Fatal("the supervisor marker variable is dropped")
	}
}

// Every way the gateway touches a ZDX home refuses one outside the
// sandbox before it opens a file there or runs zdx: the pidfile (the
// first thing serve does), the supervision marker, settings, lifecycle
// commands and worktrees.
func TestSandboxRefusesAnOutsideHomeBeforeTouchingIt(t *testing.T) {
	outside := outsideSandbox(t)
	marker := filepath.Join(outside, "zdx-ran")
	zdx := filepath.Join(t.TempDir(), "zdx")
	if err := os.WriteFile(zdx, []byte("#!/bin/sh\ntouch '"+marker+"'\n"), 0o755); err != nil {
		t.Fatal(err)
	}
	env := []string{"PATH=/usr/bin:/bin", "HOME=" + outside, "ZDX_HOME=" + outside}
	refused := func(what string, err error) {
		t.Helper()
		if !errors.Is(err, sandbox.ErrOutside) {
			t.Errorf("%s: want a sandbox refusal, got %v", what, err)
		}
	}
	_, err := AcquirePidFile(outside)
	refused("pidfile", err)
	_, err = MarkSupervised(outside, func(string) string { return "launchd" })
	refused("supervision marker", err)
	_, err = LoadSettings(context.Background(), zdx, outside, env)
	refused("settings", err)
	_, err = ExecRunner(zdx, env)(context.Background(), "service", "status")
	refused("lifecycle command", err)
	_, err = CLIWorktrees{Zdx: zdx, Env: env}.EnsureWorktree(context.Background(), outside, "t")
	refused("worktree", err)
	_, err = LoadSettings(context.Background(), zdx, outside, []string{"PATH=/usr/bin:/bin"})
	refused("an environment without a home", err)
	err = Serve(context.Background(), ServeOptions{Home: outside, Root: outside, Zdx: zdx, Env: env, Log: quiet})
	refused("serve", err)
	entries, _ := os.ReadDir(outside)
	if len(entries) != 0 {
		t.Fatalf("nothing is written outside the sandbox: %v", entries)
	}
}
