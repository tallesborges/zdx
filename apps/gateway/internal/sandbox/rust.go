package sandbox

import (
	"os"
	"testing"
)

// Real-Rust tests run against a prebuilt zdx named by RustBinaryEnv. By
// default they skip without one, or on a binary that lacks a feature they
// check. With RustStrictEnv=1 every such skip fails instead: a gate run
// against the binary that should have everything cannot pass by skipping.
const (
	RustBinaryEnv = "ZDX_GATEWAY_RUST_ZDX"
	RustStrictEnv = "ZDX_GATEWAY_RUST_STRICT"
)

// RustBinary is the zdx binary of real-Rust tests; it skips (strict: fails)
// when none is set.
func RustBinary(t testing.TB) string {
	t.Helper()
	bin := os.Getenv(RustBinaryEnv)
	if bin == "" {
		RustFeatureMissing(t, RustBinaryEnv+" not set")
	}
	return bin
}

// RustFeatureMissing skips the test (strict: fails it) because the binary
// lacks what it checks.
func RustFeatureMissing(t testing.TB, why string) {
	t.Helper()
	if os.Getenv(RustStrictEnv) != "" {
		t.Fatalf("%s=%s: %s", RustStrictEnv, os.Getenv(RustStrictEnv), why)
	}
	t.Skip(why)
}
