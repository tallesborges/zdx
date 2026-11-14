//go:build unix

package source

import (
	"os/exec"
	"syscall"
)

// ownGroup runs cmd in its own process group and kills the whole group on
// cancel, so a wrapper's children die with it.
func ownGroup(cmd *exec.Cmd) {
	cmd.SysProcAttr = &syscall.SysProcAttr{Setpgid: true}
	cmd.Cancel = func() error { return syscall.Kill(-cmd.Process.Pid, syscall.SIGKILL) }
}
