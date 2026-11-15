//go:build unix

package jobs

import (
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"syscall"
	"time"
)

// Lock is a held job lock: an exclusive flock on `state/jobs/<name>/lock`,
// released by Release or when every process holding the descriptor exits.
type Lock struct{ f *os.File }

// File is the locked descriptor, to pass to a child that must keep the lock
// held while it lives.
func (l *Lock) File() *os.File { return l.f }

// Release drops the lock (this process's descriptor).
func (l *Lock) Release() {
	if l != nil && l.f != nil {
		_ = l.f.Close()
		l.f = nil
	}
}

// TryLock takes the job's run lock, or returns nil when another process
// holds it. A status probe holds it for microseconds, so a busy lock is
// tried three times, 20 ms apart.
func (h Home) TryLock(job string) (*Lock, error) {
	path := filepath.Join(h.jobStateDir(job), "lock")
	if err := os.MkdirAll(filepath.Dir(path), 0o777); err != nil {
		return nil, fmt.Errorf("create %s: %w", filepath.Dir(path), err)
	}
	f, err := os.OpenFile(path, os.O_CREATE|os.O_WRONLY, 0o666)
	if err != nil {
		return nil, fmt.Errorf("open job lock %s: %w", path, err)
	}
	for attempt := 0; ; attempt++ {
		err := flock(f, syscall.LOCK_EX|syscall.LOCK_NB)
		if err == nil {
			return &Lock{f: f}, nil
		}
		if !errors.Is(err, syscall.EWOULDBLOCK) {
			f.Close()
			return nil, fmt.Errorf("lock %s: %w", path, err)
		}
		if attempt == 2 {
			f.Close()
			return nil, nil
		}
		time.Sleep(20 * time.Millisecond)
	}
}

// IsRunning reports whether some process holds the job's run lock now.
func (h Home) IsRunning(job string) bool {
	f, err := os.Open(filepath.Join(h.jobStateDir(job), "lock"))
	if err != nil {
		return false
	}
	defer f.Close()
	return errors.Is(flock(f, syscall.LOCK_SH|syscall.LOCK_NB), syscall.EWOULDBLOCK)
}

// lockFile waits for an exclusive flock on path and returns its release.
func lockFile(path string) (func(), error) {
	f, err := os.OpenFile(path, os.O_CREATE|os.O_WRONLY, 0o666)
	if err != nil {
		return nil, fmt.Errorf("open %s: %w", path, err)
	}
	if err := flock(f, syscall.LOCK_EX); err != nil {
		f.Close()
		return nil, fmt.Errorf("lock %s: %w", path, err)
	}
	return func() { _ = f.Close() }, nil
}

func flock(f *os.File, how int) error {
	for {
		err := syscall.Flock(int(f.Fd()), how)
		if !errors.Is(err, syscall.EINTR) {
			return err
		}
	}
}
