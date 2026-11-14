package gateway

import (
	"bytes"
	"context"
	"errors"
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"strconv"
	"strings"
	"syscall"

	"github.com/tallesborges/zdx/apps/gateway/internal/sandbox"
)

// ServiceName is the gateway's service under `zdx service`: it replaces the
// Rust bot, so it keeps the bot's pidfile and supervision marker.
const ServiceName = "bot"

// SupervisorEnv is set by the launchd plist (zdx_engine::service).
const SupervisorEnv = "ZDX_SERVICE_SUPERVISOR"

// ExitRequested is the exit code of a restart request: launchd starts the
// process again (zdx_bot::EXIT_REQUESTED).
const ExitRequested = 42

// PidFile is $ZDX_HOME/run/bot.pid, the decimal PID of the running bot, as
// zdx_engine::pidfile writes it. `zdx service status` and the restart guards
// read it. Go owns this file for the gateway.
//
// Two guards keep a second bot off the token: an exclusive flock on the
// file, held for the life of the process (atomic between gateways; the
// kernel drops it on a crash), and the PID it names (the Rust bot takes no
// lock, so a live PID there refuses too).
type PidFile struct {
	path string
	f    *os.File
}

func runPath(home, name string) string { return filepath.Join(home, "run", name) }

// AcquirePidFile takes the bot's pidfile or refuses: another gateway holds
// its lock, or it names another live process.
func AcquirePidFile(home string) (*PidFile, error) {
	if err := sandbox.CheckHome(home); err != nil {
		return nil, err
	}
	if !filepath.IsAbs(home) {
		return nil, errors.New("ZDX home must be an absolute path")
	}
	path := runPath(home, ServiceName+".pid")
	if err := os.MkdirAll(filepath.Dir(path), 0o777); err != nil {
		return nil, fmt.Errorf("create run dir: %w", err)
	}
	f, err := os.OpenFile(path, os.O_RDWR|os.O_CREATE, 0o666)
	if err != nil {
		return nil, fmt.Errorf("open PID file: %w", err)
	}
	if err := syscall.Flock(int(f.Fd()), syscall.LOCK_EX|syscall.LOCK_NB); err != nil {
		f.Close()
		if errors.Is(err, syscall.EWOULDBLOCK) {
			pid, _ := readPID(path)
			return nil, fmt.Errorf("%s is already running (PID %d holds %s). Stop the existing process first", ServiceName, pid, path)
		}
		return nil, fmt.Errorf("lock PID file: %w", err)
	}
	if pid, ok := readPID(path); ok && pid != os.Getpid() && alive(pid) {
		f.Close()
		return nil, fmt.Errorf("%s is already running (PID %d). Stop the existing process first", ServiceName, pid)
	}
	if err := f.Truncate(0); err != nil {
		f.Close()
		return nil, fmt.Errorf("write PID file: %w", err)
	}
	if _, err := f.WriteAt([]byte(strconv.Itoa(os.Getpid())), 0); err != nil {
		f.Close()
		return nil, fmt.Errorf("write PID file: %w", err)
	}
	return &PidFile{path: path, f: f}, nil
}

// Release removes the pidfile when it still names this process, then drops
// the lock.
func (p *PidFile) Release() {
	if pid, ok := readPID(p.path); ok && pid == os.Getpid() {
		_ = os.Remove(p.path)
	}
	_ = p.f.Close()
}

// MarkSupervised writes $ZDX_HOME/run/bot.supervised with this PID when
// launchd runs the gateway (SupervisorEnv=launchd), as the Rust bot does:
// there is no monitor to vouch for it. It reports whether it wrote one.
func MarkSupervised(home string, env func(string) string) (bool, error) {
	if env(SupervisorEnv) != "launchd" {
		return false, nil
	}
	if err := sandbox.CheckHome(home); err != nil {
		return false, err
	}
	path := runPath(home, ServiceName+".supervised")
	if err := os.MkdirAll(filepath.Dir(path), 0o777); err != nil {
		return false, err
	}
	return true, os.WriteFile(path, []byte(strconv.Itoa(os.Getpid())), 0o666)
}

// Supervised reports a live supervisor for the bot: the supervision marker
// names a live process (zdx_engine::pidfile::is_supervised, without its
// cleanup of stale markers).
func Supervised(home string) bool {
	pid, ok := readPID(runPath(home, ServiceName+".supervised"))
	return ok && alive(pid)
}

func readPID(path string) (int, bool) {
	b, err := os.ReadFile(path)
	if err != nil {
		return 0, false
	}
	pid, err := strconv.Atoi(strings.TrimSpace(string(b)))
	return pid, err == nil && pid > 0
}

// alive is kill(pid, 0) succeeding. Unlike Rust it does not tell a zombie
// from a live process, and treats EPERM as not alive, as Rust does.
func alive(pid int) bool {
	return syscall.Kill(pid, 0) == nil
}

// LoadSettings runs `zdx --root ROOT telegram settings --include-token` and
// reads its JSON. Rust resolves the config; the token never reaches logs or
// errors (only a cut of stderr is kept).
func LoadSettings(ctx context.Context, zdx, root string, env []string) (Settings, error) {
	if err := sandbox.CheckEnv(env); err != nil {
		return Settings{}, err
	}
	cmd := exec.CommandContext(ctx, zdx, "--root", root, "telegram", "settings", "--include-token")
	cmd.Dir = root
	cmd.Env = env
	var stdout, stderr bytes.Buffer
	cmd.Stdout, cmd.Stderr = &stdout, &stderr
	if err := cmd.Run(); err != nil {
		msg := strings.TrimSpace(stderr.String())
		if len(msg) > 500 {
			msg = msg[:500]
		}
		return Settings{}, fmt.Errorf("zdx telegram settings failed (%v): %s", err, msg)
	}
	return ReadSettings(&stdout)
}

// CLIWorktrees ensures thread worktrees with `zdx --root ROOT worktree
// ensure THREAD`, which prints the worktree path. Rust owns git and the
// worktree layout; Go only runs the command.
type CLIWorktrees struct {
	Zdx string
	Env []string
}

func (w CLIWorktrees) EnsureWorktree(ctx context.Context, root, threadID string) (string, error) {
	if !filepath.IsAbs(root) {
		return "", errors.New("project root must be an absolute path")
	}
	if err := sandbox.CheckEnv(w.Env); err != nil {
		return "", err
	}
	cmd := exec.CommandContext(ctx, w.Zdx, "--root", root, "worktree", "ensure", threadID)
	cmd.Dir = root
	cmd.Env = w.Env
	var stdout, stderr bytes.Buffer
	cmd.Stdout, cmd.Stderr = &stdout, &stderr
	if err := cmd.Run(); err != nil {
		msg := strings.TrimSpace(stderr.String())
		// The CLI adds this context; the Rust bot shows the error under it.
		msg = strings.TrimPrefix(msg, "ensure worktree for '"+threadID+"': ")
		if msg == "" {
			msg = err.Error()
		}
		return "", errors.New(msg)
	}
	path := strings.TrimSpace(stdout.String())
	if !filepath.IsAbs(path) || strings.Contains(path, "\n") {
		return "", fmt.Errorf("zdx worktree ensure printed %q, not one absolute path", path)
	}
	return path, nil
}
