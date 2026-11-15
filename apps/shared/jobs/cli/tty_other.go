//go:build !darwin && !linux

package cli

func terminalForeground() bool { return false }
