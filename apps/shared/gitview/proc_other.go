//go:build !unix

package gitview

import "os/exec"

// ownGroup keeps exec's default: cancel kills git itself.
func ownGroup(*exec.Cmd) {}
