//go:build unix

package quota

import (
	"errors"
	"os"
	"syscall"
)

func tryFlock(f *os.File) (bool, error) {
	for {
		err := syscall.Flock(int(f.Fd()), syscall.LOCK_EX|syscall.LOCK_NB)
		if err == nil {
			return true, nil
		}
		if errors.Is(err, syscall.EWOULDBLOCK) {
			return false, nil
		}
		if errors.Is(err, syscall.EINTR) {
			continue
		}
		return false, err
	}
}

func unlockFlock(f *os.File) {
	_ = syscall.Flock(int(f.Fd()), syscall.LOCK_UN)
}
