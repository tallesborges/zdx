package version

import (
	"errors"
	"testing"
)

func TestResolvePrefersExplicitThenBuildValue(t *testing.T) {
	was := Zdx
	t.Cleanup(func() { Zdx = was })

	Zdx = ""
	if _, err := Resolve(""); !errors.Is(err, ErrUnknown) {
		t.Fatalf("no version must fail, got %v", err)
	}
	if v, err := Resolve("9.9.9-test"); err != nil || v != "9.9.9-test" {
		t.Fatalf("%q %v", v, err)
	}
	Zdx = "1.2.3-test"
	if v, _ := Resolve(""); v != "1.2.3-test" {
		t.Fatalf("build value: %q", v)
	}
	if v, _ := Resolve("9.9.9-test"); v != "9.9.9-test" {
		t.Fatalf("explicit wins: %q", v)
	}
}
