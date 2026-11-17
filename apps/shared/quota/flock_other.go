//go:build !unix

package quota

import "os"

func tryFlock(f *os.File) (bool, error) {
	return true, nil
}

func unlockFlock(f *os.File) {}
