// Package sandbox keeps the shared module's tests off the developer's real
// ZDX home.
//
// TestMain calls Main: it moves HOME, ZDX_HOME and TMPDIR into a fresh
// directory, drops every other inherited ZDX_ variable (a real home, token,
// engine path, or agent run) and OPENCODE_API_KEY, and puts a failing `zdx` first on PATH, so a
// test never reaches real credentials or the real binary. Variables that
// start with KeepPrefix stay. After the tests it removes the sandbox and
// fails the run when a live-service file or the OAuth cache of the
// inherited home changed (a live service writing one meanwhile trips it
// too; rerun then).
package sandbox

import (
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"time"
)

// KeepPrefix marks test switches that survive the sandbox.
const KeepPrefix = "ZDX_SHARED_TEST_"

// credentialEnv are provider credentials read from the environment.
var credentialEnv = []string{"OPENCODE_API_KEY"}

// watched are files of the inherited ZDX home a test must never touch. Only
// their size and modification time are read, never their contents.
var watched = []string{
	"oauth.json", "oauth.lock",
	"run/bot.pid", "run/bot.supervised", "run/orchestrator.sock", "run/orchestrator.pid", "run/restart-queue.json",
}

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

// Enter moves the test binary into a fresh sandbox. The returned check
// removes it and reports a changed watched file.
func Enter() (check func() error, err error) {
	realHome := os.Getenv("ZDX_HOME")
	if realHome == "" {
		if h, err := os.UserHomeDir(); err == nil {
			realHome = filepath.Join(h, ".zdx")
		}
	}
	// Short names: macOS limits Unix socket paths to 104 bytes.
	dir, err := os.MkdirTemp("", "z")
	if err != nil {
		return nil, err
	}
	home, tmp, bin := filepath.Join(dir, "h"), filepath.Join(dir, "t"), filepath.Join(dir, "bin")
	for _, d := range []string{home, tmp, bin} {
		if err := os.MkdirAll(d, 0o700); err != nil {
			return nil, err
		}
	}
	script := "#!/bin/sh\necho 'test sandbox: a test ran zdx from PATH' >&2\nexit 97\n"
	if err := os.WriteFile(filepath.Join(bin, "zdx"), []byte(script), 0o755); err != nil {
		return nil, err
	}
	before := map[string]stamp{}
	if realHome != "" {
		for _, name := range watched {
			p := filepath.Join(realHome, name)
			before[p] = stampOf(p)
		}
	}
	for _, kv := range os.Environ() {
		k, _, _ := strings.Cut(kv, "=")
		if strings.HasPrefix(k, "ZDX_") && !strings.HasPrefix(k, KeepPrefix) {
			os.Unsetenv(k)
		}
	}
	// Credentials quota reads from the environment: a test that forgets its
	// synthetic LookupEnv must not reach a real provider.
	for _, k := range credentialEnv {
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
		var changed []string
		for p, was := range before {
			if stampOf(p) != was {
				changed = append(changed, p)
			}
		}
		if len(changed) > 0 {
			return fmt.Errorf("test sandbox: files of the real ZDX home changed during the tests: %s", strings.Join(changed, ", "))
		}
		return nil
	}, nil
}

// Main is a TestMain body: Enter, run, check. It returns the exit code.
func Main(run func() int) int {
	check, err := Enter()
	if err != nil {
		fmt.Fprintln(os.Stderr, "test sandbox:", err)
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
