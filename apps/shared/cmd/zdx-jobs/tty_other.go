//go:build !darwin && !linux

package main

func terminalForeground() bool { return false }
