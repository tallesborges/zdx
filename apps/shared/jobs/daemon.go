//go:build unix

package jobs

import (
	"encoding/json"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"strconv"
	"strings"
	"syscall"
	"time"
)

// DaemonRecord is `run/daemon.context.json`: what the running daemon
// reports about itself (zdx_engine::job_daemon reads it).
type DaemonRecord struct {
	PID       int       `json:"pid"`
	Root      string    `json:"root"`
	ConfigDir string    `json:"config_dir"`
	StartedAt time.Time `json:"started_at"`
}

// DaemonFiles are the daemon's PID file and context record, held for the
// daemon's life.
type DaemonFiles struct {
	pid, record string
	f           *os.File
}

// AcquireDaemon takes `run/daemon.pid` (an exclusive flock held for the
// process's life, plus the PID; a PID file naming another live process also
// refuses) and writes `run/daemon.context.json`.
func AcquireDaemon(home Home, root, configDir string) (*DaemonFiles, error) {
	run := filepath.Join(home.Dir, "run")
	if err := os.MkdirAll(run, 0o777); err != nil {
		return nil, fmt.Errorf("create run dir %s: %w", run, err)
	}
	path := filepath.Join(run, "daemon.pid")
	f, err := os.OpenFile(path, os.O_RDWR|os.O_CREATE, 0o666)
	if err != nil {
		return nil, fmt.Errorf("open PID file: %w", err)
	}
	if err := flock(f, syscall.LOCK_EX|syscall.LOCK_NB); err != nil {
		f.Close()
		if errors.Is(err, syscall.EWOULDBLOCK) {
			pid, _ := readPID(path)
			return nil, fmt.Errorf("daemon is already running (PID %d). Stop the existing process first.", pid)
		}
		return nil, fmt.Errorf("lock PID file: %w", err)
	}
	if pid, ok := readPID(path); ok && pid != os.Getpid() && syscall.Kill(pid, 0) == nil {
		f.Close()
		return nil, fmt.Errorf("daemon is already running (PID %d). Stop the existing process first.", pid)
	}
	if err := f.Truncate(0); err != nil {
		f.Close()
		return nil, fmt.Errorf("write daemon PID file: %w", err)
	}
	if _, err := f.WriteAt([]byte(strconv.Itoa(os.Getpid())), 0); err != nil {
		f.Close()
		return nil, fmt.Errorf("write daemon PID file: %w", err)
	}
	if resolved, err := filepath.EvalSymlinks(root); err == nil {
		root = resolved
	}
	rec := DaemonRecord{PID: os.Getpid(), Root: root, ConfigDir: configDir, StartedAt: time.Now().UTC()}
	data, err := prettyJSON(rec)
	if err != nil {
		f.Close()
		return nil, err
	}
	d := &DaemonFiles{pid: path, record: filepath.Join(run, "daemon.context.json"), f: f}
	if err := writeAtomic(d.record, data); err != nil {
		d.Release()
		return nil, fmt.Errorf("write daemon context record: %w", err)
	}
	return d, nil
}

// Release removes both files when they still name this process.
func (d *DaemonFiles) Release() {
	var rec DaemonRecord
	if b, err := os.ReadFile(d.record); err == nil && json.Unmarshal(b, &rec) == nil && rec.PID == os.Getpid() {
		_ = os.Remove(d.record)
	}
	if pid, ok := readPID(d.pid); ok && pid == os.Getpid() {
		_ = os.Remove(d.pid)
	}
	_ = d.f.Close()
}

func readPID(path string) (int, bool) {
	b, err := os.ReadFile(path)
	if err != nil {
		return 0, false
	}
	pid, err := strconv.Atoi(strings.TrimSpace(string(b)))
	return pid, err == nil && pid > 0
}
