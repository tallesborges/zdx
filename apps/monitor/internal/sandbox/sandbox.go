// Package sandbox keeps tests off the developer's real ZDX home.
//
// TestMain calls Enter: it moves HOME, ZDX_HOME and TMPDIR into a fresh
// directory, drops every other inherited ZDX_ variable and the provider
// credentials the quota fetch reads (OPENCODE_API_KEY), and puts a failing
// `zdx` first on PATH, so a test that forgets its fake binary never runs the
// real one. Variables starting with KeepPrefix stay. The returned check
// removes the sandbox and fails the run when a live-service file or the
// OAuth cache of the inherited home changed (only size and modification
// time are read, never contents).
package sandbox

import (
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"time"
)

// KeepPrefix marks test switches that survive the sandbox (for example
// ZDX_MONITOR_TEST_ZDX, the prebuilt binary of the opt-in real check).
const KeepPrefix = "ZDX_MONITOR_TEST_"

// watched are files of the inherited home a test must never touch.
var watched = []string{
	"oauth.json", "oauth.lock",
	"run/bot.pid", "run/bot.supervised", "run/orchestrator.sock", "run/orchestrator.pid", "run/restart-queue.json",
}

// dropped are credential variables outside the ZDX_ prefix.
var dropped = []string{"OPENCODE_API_KEY"}

type stamp struct {
	exists bool
	size   int64
	mod    time.Time
}

func stampOf(path string) stamp {
	info, err := os.Lstat(path)
	if err != nil {
		return stamp{}
	}
	return stamp{exists: true, size: info.Size(), mod: info.ModTime()}
}

// Enter moves the test binary into a fresh sandbox.
func Enter() (check func() error, err error) {
	realHome := os.Getenv("ZDX_HOME")
	if realHome == "" {
		if h, err := os.UserHomeDir(); err == nil {
			realHome = filepath.Join(h, ".zdx")
		}
	}
	// Short names: macOS limits Unix socket paths to 104 bytes.
	dir, err := os.MkdirTemp("", "m")
	if err != nil {
		return nil, err
	}
	home, tmp, bin := filepath.Join(dir, "h"), filepath.Join(dir, "t"), filepath.Join(dir, "bin")
	for _, d := range []string{home, tmp, bin} {
		if err := os.MkdirAll(d, 0o700); err != nil {
			return nil, err
		}
	}
	script := "#!/bin/sh\necho 'test sandbox: a test ran zdx from PATH; pass a fake binary' >&2\nexit 97\n"
	if err := os.WriteFile(filepath.Join(bin, "zdx"), []byte(script), 0o755); err != nil {
		return nil, err
	}
	before := map[string]stamp{}
	for _, name := range watched {
		p := filepath.Join(realHome, name)
		before[p] = stampOf(p)
	}
	for _, kv := range os.Environ() {
		k, _, _ := strings.Cut(kv, "=")
		if strings.HasPrefix(k, "ZDX_") && !strings.HasPrefix(k, KeepPrefix) {
			os.Unsetenv(k)
		}
	}
	for _, k := range dropped {
		os.Unsetenv(k)
	}
	set := map[string]string{
		"HOME":     home,
		"ZDX_HOME": filepath.Join(home, ".zdx"),
		"TMPDIR":   tmp,
		"PATH":     bin + ":/usr/bin:/bin",
	}
	for k, v := range set {
		if err := os.Setenv(k, v); err != nil {
			return nil, err
		}
	}
	return func() error {
		os.RemoveAll(dir)
		for p, s := range before {
			if stampOf(p) != s {
				return fmt.Errorf("test sandbox: %s changed during the tests", p)
			}
		}
		return nil
	}, nil
}

// Main is the body of a TestMain: enter, run, check.
func Main(run func() int) int {
	check, err := Enter()
	if err != nil {
		fmt.Fprintln(os.Stderr, "sandbox:", err)
		return 1
	}
	code := run()
	if err := check(); err != nil {
		fmt.Fprintln(os.Stderr, err)
		if code == 0 {
			code = 1
		}
	}
	return code
}
