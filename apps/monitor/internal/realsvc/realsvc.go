// Package realsvc runs a prebuilt zdx orchestrator service for opt-in
// integration tests. It never builds Rust: the binary comes from
// ZDX_MONITOR_TEST_ZDX. The child gets only PATH, TMPDIR, LANG and LC_*
// from the parent, then HOME and ZDX_HOME set to the test's temp home, so
// no real config, credentials, threads, or provider can be reached.
package realsvc

import (
	"context"
	"encoding/json"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"syscall"
	"testing"
	"time"

	"github.com/tallesborges/zdx/apps/monitor/internal/wire"
)

// BinaryEnv names the prebuilt binary.
const BinaryEnv = "ZDX_MONITOR_TEST_ZDX"

// Binary returns the prebuilt zdx, or skips the test without one.
func Binary(t *testing.T) string {
	t.Helper()
	bin := os.Getenv(BinaryEnv)
	if bin == "" {
		t.Skip("set " + BinaryEnv + " to a prebuilt zdx binary")
	}
	info, err := os.Stat(bin)
	if err != nil || info.IsDir() || info.Mode()&0o111 == 0 {
		t.Fatalf("%s=%s is not an executable file", BinaryEnv, bin)
	}
	return bin
}

// Env is the child environment for home.
func Env(parent []string, home string) []string {
	var out []string
	for _, kv := range parent {
		k, _, _ := strings.Cut(kv, "=")
		if k == "PATH" || k == "TMPDIR" || k == "LANG" || strings.HasPrefix(k, "LC_") {
			out = append(out, kv)
		}
	}
	return append(out, "HOME="+home, "ZDX_HOME="+home)
}

// Home makes a short temp home (socket paths are limited to 104 bytes).
func Home(t *testing.T) string {
	t.Helper()
	dir, err := os.MkdirTemp("", "h")
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { os.RemoveAll(dir) })
	return dir
}

// Start runs `zdx --root root orchestrator serve` with home and waits until
// a status call succeeds (the socket file appears before the listener
// accepts). The service stops on cleanup.
func Start(t *testing.T, bin, home, root string) {
	t.Helper()
	logPath := filepath.Join(home, "service.log")
	logFile, err := os.Create(logPath)
	if err != nil {
		t.Fatal(err)
	}
	cmd := exec.Command(bin, "--root", root, "orchestrator", "serve")
	cmd.Dir = root
	cmd.Env = Env(os.Environ(), home)
	cmd.Stdout, cmd.Stderr = logFile, logFile
	if err := cmd.Start(); err != nil {
		t.Fatal(err)
	}
	exited := make(chan error, 1)
	// Closed after the result, so cleanup still returns when the startup
	// loop already took it.
	go func() { exited <- cmd.Wait(); close(exited) }()
	t.Cleanup(func() {
		_ = cmd.Process.Signal(syscall.SIGTERM)
		select {
		case <-exited:
		case <-time.After(10 * time.Second):
			_ = cmd.Process.Kill()
			<-exited
		}
		logFile.Close()
	})
	log := func() string { b, _ := os.ReadFile(logPath); return string(b) }
	client := wire.NewClient(filepath.Join(home, "run", "orchestrator.sock"))
	deadline := time.Now().Add(30 * time.Second)
	for {
		select {
		case err := <-exited:
			t.Fatalf("service exited: %v\n%s", err, log())
		default:
		}
		ctx, cancel := context.WithTimeout(context.Background(), time.Second)
		_, err := client.Status(ctx)
		cancel()
		if err == nil {
			return
		}
		if time.Now().After(deadline) {
			t.Fatalf("service never answered: %v\n%s", err, log())
		}
		time.Sleep(100 * time.Millisecond)
	}
}

// Now is a thread event timestamp.
func Now() string { return time.Now().UTC().Format("2006-01-02T15:04:05.000Z") }

// AppendEvents appends thread events (crates/zdx-engine thread_persistence
// ThreadEvent JSON, one per line) to <home>/threads/<id>.jsonl.
func AppendEvents(t *testing.T, home, id string, events ...map[string]any) {
	t.Helper()
	dir := filepath.Join(home, "threads")
	if err := os.MkdirAll(dir, 0o700); err != nil {
		t.Fatal(err)
	}
	f, err := os.OpenFile(filepath.Join(dir, id+".jsonl"), os.O_CREATE|os.O_APPEND|os.O_WRONLY, 0o600)
	if err != nil {
		t.Fatal(err)
	}
	defer f.Close()
	for _, e := range events {
		line, err := json.Marshal(e)
		if err != nil {
			t.Fatal(err)
		}
		if _, err := f.Write(append(line, '\n')); err != nil {
			t.Fatal(err)
		}
	}
}

// WriteRunMarker writes an agent run marker (agent_activity RunRecord) at
// <home>/run/agents/<name>.json, written atomically as the engine does.
func WriteRunMarker(t *testing.T, home, name string, record map[string]any) {
	t.Helper()
	dir := filepath.Join(home, "run", "agents")
	if err := os.MkdirAll(dir, 0o700); err != nil {
		t.Fatal(err)
	}
	body, err := json.Marshal(record)
	if err != nil {
		t.Fatal(err)
	}
	tmp := filepath.Join(dir, "."+name+".tmp")
	if err := os.WriteFile(tmp, body, 0o600); err != nil {
		t.Fatal(err)
	}
	if err := os.Rename(tmp, filepath.Join(dir, name+".json")); err != nil {
		t.Fatal(err)
	}
}

// RemoveRunMarker deletes a marker written by WriteRunMarker.
func RemoveRunMarker(t *testing.T, home, name string) {
	t.Helper()
	if err := os.Remove(filepath.Join(home, "run", "agents", name+".json")); err != nil && !os.IsNotExist(err) {
		t.Fatal(err)
	}
}
