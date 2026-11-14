//go:build !unix

package source

import "os/exec"

func ownGroup(*exec.Cmd) {}
