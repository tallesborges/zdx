// Package sandbox keeps tests off the developer's real ZDX home.
//
// Test binaries call Enter from TestMain: it moves HOME, ZDX_HOME and
// TMPDIR into a fresh directory and sets Env to it. While Env is set, the
// gateway refuses any ZDX home outside that directory (CheckHome, CheckEnv)
// before it opens a file there or runs zdx with it, and Enter's cleanup
// fails the run when a file of the inherited ZDX home's run directory
// changed. Production never sets Env.
package sandbox

import (
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"time"
)

// Env names the sandbox directory of a test binary.
const Env = "ZDX_GATEWAY_TEST_SANDBOX"

// ErrOutside: a test reached a ZDX home outside its sandbox.
var ErrOutside = errors.New("test sandbox: refusing a ZDX home outside " + Env)

// CheckHome refuses home when a sandbox is set and home is not inside it.
func CheckHome(home string) error {
	root := os.Getenv(Env)
	if root == "" {
		return nil
	}
	clean := filepath.Clean(home)
	if !filepath.IsAbs(clean) || !strings.HasPrefix(clean+string(filepath.Separator), filepath.Clean(root)+string(filepath.Separator)) {
		return fmt.Errorf("%w: %s", ErrOutside, home)
	}
	return nil
}

// CheckEnv refuses a child environment whose ZDX home (ZDX_HOME, else
// HOME/.zdx) is outside the sandbox. A nil env is the process's own.
func CheckEnv(env []string) error {
	if os.Getenv(Env) == "" {
		return nil
	}
	if env == nil {
		env = os.Environ()
	}
	var zdxHome, home string
	for _, kv := range env {
		k, v, _ := strings.Cut(kv, "=")
		switch k {
		case "ZDX_HOME":
			zdxHome = v
		case "HOME":
			home = v
		}
	}
	switch {
	case zdxHome != "":
		return CheckHome(zdxHome)
	case home != "":
		return CheckHome(filepath.Join(home, ".zdx"))
	}
	return fmt.Errorf("%w: the child environment names no HOME or ZDX_HOME", ErrOutside)
}

// watched are the live-bot files a test must never change.
var watched = []string{"bot.pid", "bot.supervised", "restart-queue.json", "engine.sock"}

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

// Enter moves the test binary into a fresh sandbox: HOME, ZDX_HOME and
// TMPDIR point inside it, Env names it, and every other inherited ZDX_
// variable (supervisor, agent run, engine path, token) is dropped. With fakeZdx, PATH is
// <sandbox>/bin:/usr/bin:/bin and <sandbox>/bin/zdx fails loudly, so a test
// that forgets --zdx never runs the real binary. The returned check runs
// after the tests: it removes the sandbox and reports a changed file in the
// inherited ZDX home's run directory (a live bot restarting meanwhile
// trips it too; rerun then).
func Enter(fakeZdx bool) (check func() error, err error) {
	realHome := os.Getenv("ZDX_HOME")
	if realHome == "" {
		if h, err := os.UserHomeDir(); err == nil {
			realHome = filepath.Join(h, ".zdx")
		}
	}
	// Short names, symlinks kept: macOS limits Unix socket paths to 104
	// bytes, and tests make sockets a few levels below TMPDIR.
	dir, err := os.MkdirTemp("", "s")
	if err != nil {
		return nil, err
	}
	home := filepath.Join(dir, "h")
	tmp := filepath.Join(dir, "t")
	for _, d := range []string{home, tmp} {
		if err := os.MkdirAll(d, 0o700); err != nil {
			return nil, err
		}
	}
	before := map[string]stamp{}
	if realHome != "" {
		for _, name := range watched {
			p := filepath.Join(realHome, "run", name)
			before[p] = stampOf(p)
		}
	}
	set := map[string]string{"HOME": home, "ZDX_HOME": filepath.Join(home, ".zdx"), "TMPDIR": tmp, Env: dir}
	if fakeZdx {
		bin := filepath.Join(dir, "bin")
		if err := os.MkdirAll(bin, 0o700); err != nil {
			return nil, err
		}
		script := "#!/bin/sh\necho 'test sandbox: a test ran zdx from PATH; pass a fake --zdx' >&2\nexit 97\n"
		if err := os.WriteFile(filepath.Join(bin, "zdx"), []byte(script), 0o755); err != nil {
			return nil, err
		}
		set["PATH"] = bin + ":/usr/bin:/bin"
	}
	for k, v := range set {
		if err := os.Setenv(k, v); err != nil {
			return nil, err
		}
	}
	// Every other ZDX_ variable of the parent goes too: a launchd bot's
	// engine path (ZDX_BIN), an agent run's thread, a real bot token.
	// The real-Rust test switches (ZDX_GATEWAY_RUST_*) stay.
	for _, kv := range os.Environ() {
		k, _, _ := strings.Cut(kv, "=")
		if _, ours := set[k]; ours || !strings.HasPrefix(k, "ZDX_") || strings.HasPrefix(k, "ZDX_GATEWAY_RUST_") {
			continue
		}
		os.Unsetenv(k)
	}
	// The quota read takes OPENCODE_API_KEY from the environment; no test
	// may reach the real provider with it.
	os.Unsetenv("OPENCODE_API_KEY")
	return func() error {
		os.RemoveAll(dir)
		var changed []string
		for p, was := range before {
			if now := stampOf(p); now != was {
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
func Main(run func() int, fakeZdx bool) int {
	check, err := Enter(fakeZdx)
	if err != nil {
		fmt.Fprintln(os.Stderr, "test sandbox:", err)
		return 1
	}
	code := run()
	if err := check(); err != nil {
		fmt.Fprintln(os.Stderr, err)
		return 1
	}
	return code
}
