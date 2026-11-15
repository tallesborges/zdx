//go:build darwin || linux

package main

import (
	"os"
	"syscall"
	"unsafe"
)

// terminalForeground reports whether this process's group is the
// foreground group of its controlling terminal, where a Ctrl-C goes to the
// whole group.
func terminalForeground() bool {
	tty, err := os.Open("/dev/tty")
	if err != nil {
		return false
	}
	defer tty.Close()
	var pgrp int32
	_, _, errno := syscall.Syscall(syscall.SYS_IOCTL, tty.Fd(), uintptr(syscall.TIOCGPGRP), uintptr(unsafe.Pointer(&pgrp)))
	return errno == 0 && int(pgrp) == syscall.Getpgrp()
}
