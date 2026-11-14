package gateway

import (
	"context"
	"fmt"
	"io"
	"os"
	"os/exec"
	"path/filepath"
	"strconv"
	"strings"
	"testing"

	"github.com/tallesborges/zdx/apps/gateway/internal/sandbox"
)

func TestPidFileGuardsAndReleases(t *testing.T) {
	home := t.TempDir()
	path := filepath.Join(home, "run", "bot.pid")
	if err := os.MkdirAll(filepath.Dir(path), 0o777); err != nil {
		t.Fatal(err)
	}
	// Another live process (the test's parent) holds the slot.
	os.WriteFile(path, []byte(strconv.Itoa(os.Getppid())), 0o666)
	if _, err := AcquirePidFile(home); err == nil || !strings.Contains(err.Error(), "already running") {
		t.Fatalf("a live holder must refuse the start: %v", err)
	}
	// A dead holder is replaced.
	os.WriteFile(path, []byte("999999"), 0o666)
	p, err := AcquirePidFile(home)
	if err != nil {
		t.Fatal(err)
	}
	if b, _ := os.ReadFile(path); string(b) != strconv.Itoa(os.Getpid()) {
		t.Fatalf("pidfile holds %q", b)
	}
	// A second gateway in this process is refused by the lock, even though
	// the file names this very PID.
	if _, err := AcquirePidFile(home); err == nil || !strings.Contains(err.Error(), "already running") {
		t.Fatalf("the lock must refuse a second holder: %v", err)
	}
	p.Release()
	if _, err := os.Stat(path); !os.IsNotExist(err) {
		t.Fatal("release removes the pidfile")
	}
	again, err := AcquirePidFile(home)
	if err != nil {
		t.Fatalf("the lock is free after release: %v", err)
	}
	again.Release()
	// Release never removes another process's file.
	os.WriteFile(path, []byte(strconv.Itoa(os.Getppid())), 0o666)
	p.Release()
	if _, err := os.Stat(path); err != nil {
		t.Fatal("release must keep another process's pidfile")
	}
	if _, err := AcquirePidFile("relative"); err == nil {
		t.Fatal("a relative home is refused")
	}
}

func TestSupervisionMarker(t *testing.T) {
	home := t.TempDir()
	env := map[string]string{}
	get := func(k string) string { return env[k] }
	if wrote, err := MarkSupervised(home, get); wrote || err != nil || Supervised(home) {
		t.Fatalf("no marker outside launchd: %v %v", wrote, err)
	}
	env[SupervisorEnv] = "launchd"
	if wrote, err := MarkSupervised(home, get); !wrote || err != nil || !Supervised(home) {
		t.Fatalf("launchd marks supervision: %v %v", wrote, err)
	}
	os.WriteFile(filepath.Join(home, "run", "bot.supervised"), []byte("999999"), 0o666)
	if Supervised(home) {
		t.Fatal("a dead supervisor is not supervision")
	}
}

// Settings come from the real `zdx telegram settings` in a temp home.
func TestLoadSettingsFromRealZdx(t *testing.T) {
	bin := sandbox.RustBinary(t)
	home := t.TempDir()
	root := filepath.Join(home, "root")
	os.MkdirAll(root, 0o777)
	os.WriteFile(filepath.Join(home, "config.toml"), []byte("[telegram]\nbot_token = \"123:SECRET\"\nallowlist_user_ids = [7]\n"), 0o600)
	env := []string{"PATH=/usr/bin:/bin", "HOME=" + home, "ZDX_HOME=" + home}
	s, err := LoadSettings(context.Background(), bin, root, env)
	if err != nil {
		t.Fatal(err)
	}
	if s.BotToken != "123:SECRET" || len(s.AllowlistUserIDs) != 1 || !strings.HasSuffix(s.Root, "/root") {
		t.Fatalf("unexpected settings %v", s)
	}
	os.WriteFile(filepath.Join(home, "config.toml"), []byte("[telegram]\nallowlist_user_ids = [7]\n"), 0o600)
	if _, err := LoadSettings(context.Background(), bin, root, env); err == nil {
		t.Fatal("settings without a token must fail")
	}
}

// A profile edit reaches the running gateway through the layers the real
// `zdx telegram settings` lists (config_layers), as the Rust bot reloads.
func TestRealSettingsFollowProfileEdits(t *testing.T) {
	bin := sandbox.RustBinary(t)
	home := t.TempDir()
	root, a, b := filepath.Join(home, "root"), filepath.Join(home, "a"), filepath.Join(home, "b")
	for _, d := range []string{root, a, b} {
		os.MkdirAll(d, 0o777)
	}
	config := func(cwd string) {
		body := fmt.Sprintf("[telegram]\nbot_token = \"123:SECRET\"\nallowlist_user_ids = [7]\n\n[telegram.profiles.p]\nchat_id = -100\ncwd = %q\n", cwd)
		if err := os.WriteFile(filepath.Join(home, "config.toml"), []byte(body), 0o600); err != nil {
			t.Fatal(err)
		}
	}
	config(a)
	env := []string{"PATH=/usr/bin:/bin", "HOME=" + home, "ZDX_HOME=" + home}
	load := func(ctx context.Context) (Settings, error) { return LoadSettings(ctx, bin, root, env) }
	s, err := load(context.Background())
	if err != nil {
		t.Fatal(err)
	}
	if len(s.ConfigLayers) == 0 {
		sandbox.RustFeatureMissing(t, "this zdx predates config_layers in `telegram settings`")
	}
	s = WatchSettings(s, load, quiet)
	if !sameDir(s.RootForChat(-100), a) {
		t.Fatalf("root %s", s.RootForChat(-100))
	}
	config(b + "/")
	s.Refresh(context.Background())
	if !sameDir(s.RootForChat(-100), b) || s.BotToken != "123:SECRET" {
		t.Fatalf("the edit must apply: %s", s.RootForChat(-100))
	}
}

// The worktree adapter runs the real `zdx worktree ensure` in a temp git
// repository.
func TestCLIWorktreesWithRealZdx(t *testing.T) {
	bin := sandbox.RustBinary(t)
	home := t.TempDir()
	repo := filepath.Join(home, "repo")
	os.MkdirAll(repo, 0o777)
	env := []string{"PATH=/usr/bin:/bin", "HOME=" + home, "ZDX_HOME=" + home,
		"GIT_AUTHOR_NAME=t", "GIT_AUTHOR_EMAIL=t@example.com", "GIT_COMMITTER_NAME=t", "GIT_COMMITTER_EMAIL=t@example.com"}
	for _, args := range [][]string{{"init", "-q"}, {"commit", "-q", "--allow-empty", "-m", "init"}} {
		cmd := exec.Command("git", args...)
		cmd.Dir, cmd.Env = repo, env
		if out, err := cmd.CombinedOutput(); err != nil {
			t.Fatalf("git %v: %v %s", args, err, out)
		}
	}
	w := CLIWorktrees{Zdx: bin, Env: env}
	path, err := w.EnsureWorktree(context.Background(), repo, "telegram--100-topic-5")
	if err != nil {
		t.Fatal(err)
	}
	if st, err := os.Stat(path); err != nil || !st.IsDir() || !filepath.IsAbs(path) {
		t.Fatalf("worktree %q: %v", path, err)
	}
	realHome, _ := filepath.EvalSymlinks(home)
	if realPath, _ := filepath.EvalSymlinks(path); !strings.HasPrefix(realPath, realHome) {
		t.Fatalf("the worktree must stay in the temp home: %s", path)
	}
	again, err := w.EnsureWorktree(context.Background(), repo, "telegram--100-topic-5")
	if err != nil || again != path {
		t.Fatalf("ensure is idempotent: %q %v", again, err)
	}
	notRepo := filepath.Join(home, "plain")
	os.MkdirAll(notRepo, 0o777)
	if _, err := w.EnsureWorktree(context.Background(), notRepo, "x"); err == nil || strings.Contains(err.Error(), "ensure worktree for") || !strings.Contains(err.Error(), "git") {
		t.Fatalf("outside a repository: %v", err)
	}
}

// TestMain lets a test run this binary as a pidfile holder.
func TestMain(m *testing.M) {
	if home := os.Getenv("GW_TEST_HOLD_PIDFILE"); home != "" {
		// The helper child of TestPidFileLockAcrossProcesses: it must
		// run inside its parent's sandbox (AcquirePidFile then refuses a
		// home outside it). An inherited helper variable alone never
		// reaches a pidfile.
		if os.Getenv(sandbox.Env) == "" {
			os.Stdout.WriteString("ERR the pidfile helper runs only inside a test sandbox\n")
			os.Exit(1)
		}
		if _, err := AcquirePidFile(home); err != nil {
			os.Stdout.WriteString("ERR " + err.Error() + "\n")
			os.Exit(1)
		}
		os.Stdout.WriteString("HELD\n")
		buf := make([]byte, 1)
		os.Stdin.Read(buf) // hold until the parent closes stdin
		os.Exit(0)
	}
	os.Exit(sandbox.Main(m.Run, false))
}

// The lock refuses a second gateway process even when the file content
// was replaced by a dead PID; a crash (kill) frees it.
func TestPidFileLockAcrossProcesses(t *testing.T) {
	home := t.TempDir()
	cmd := exec.Command(os.Args[0], "-test.run=^$")
	cmd.Env = append(os.Environ(), "GW_TEST_HOLD_PIDFILE="+home)
	stdin, _ := cmd.StdinPipe()
	out, _ := cmd.StdoutPipe()
	if err := cmd.Start(); err != nil {
		t.Fatal(err)
	}
	line := make([]byte, 5)
	if _, err := io.ReadFull(out, line); err != nil || string(line) != "HELD\n" {
		t.Fatalf("child: %q %v", line, err)
	}
	os.WriteFile(filepath.Join(home, "run", "bot.pid"), []byte("999999"), 0o666)
	if _, err := AcquirePidFile(home); err == nil {
		t.Fatal("the child's lock must refuse this process")
	}
	cmd.Process.Kill()
	cmd.Wait()
	stdin.Close()
	p, err := AcquirePidFile(home)
	if err != nil {
		t.Fatalf("a killed holder frees the lock: %v", err)
	}
	p.Release()
}
