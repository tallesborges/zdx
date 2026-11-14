//go:build unix

package gitview

import (
	"errors"
	"os"
	"os/exec"
	"syscall"
)

// ownGroup runs git in its own process group and kills the whole group on
// cancel, so a helper git started (a shell alias, a hook) dies with it.
func ownGroup(cmd *exec.Cmd) {
	cmd.SysProcAttr = &syscall.SysProcAttr{Setpgid: true}
	cmd.Cancel = func() error {
		err := syscall.Kill(-cmd.Process.Pid, syscall.SIGKILL)
		if errors.Is(err, syscall.ESRCH) {
			return os.ErrProcessDone
		}
		return err
	}
}
