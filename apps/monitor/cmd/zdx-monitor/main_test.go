package main

import (
	"errors"
	"flag"
	"io"
	"os"
	"path/filepath"
	"testing"

	"github.com/tallesborges/zdx/apps/monitor/internal/sandbox"
)

func TestMain(m *testing.M) { os.Exit(sandbox.Main(m.Run)) }

func env(vars map[string]string) func(string) string {
	return func(k string) string { return vars[k] }
}

func noZdx(string) (string, error) { return "", errors.New("not found") }

func TestDefaultsComeFromTheEnvironment(t *testing.T) {
	root := t.TempDir()
	o, err := parseArgs([]string{"--root", root}, env(map[string]string{"ZDX_HOME": "/h/.zdx", "ZDX_BIN": "/b/zdx"}), noZdx, io.Discard)
	if err != nil {
		t.Fatal(err)
	}
	if o.home != "/h/.zdx" || o.zdx != "/b/zdx" || o.cliFallback {
		t.Fatalf("got %+v", o)
	}
	if abs, _ := filepath.Abs(root); o.root != abs {
		t.Fatalf("root %s", o.root)
	}
}

func TestFlagsWinOverTheEnvironment(t *testing.T) {
	root := t.TempDir()
	o, err := parseArgs([]string{"--root", root, "--zdx-home", "/x", "--zdx", "/y/zdx", "--cli-fallback"}, env(map[string]string{"ZDX_HOME": "/h", "ZDX_BIN": "/b"}), noZdx, io.Discard)
	if err != nil {
		t.Fatal(err)
	}
	if o.home != "/x" || o.zdx != "/y/zdx" || !o.cliFallback {
		t.Fatalf("got %+v", o)
	}
}

func TestMissingZdxIsNotFatal(t *testing.T) {
	o, err := parseArgs([]string{"--root", t.TempDir()}, env(nil), noZdx, io.Discard)
	if err != nil || o.zdx != "" {
		t.Fatalf("got %+v %v", o, err)
	}
	if o.home != filepath.Join(os.Getenv("HOME"), ".zdx") {
		t.Fatalf("home %s", o.home)
	}
}

func TestBadArguments(t *testing.T) {
	if _, err := parseArgs([]string{"--root", "/does/not/exist"}, env(nil), noZdx, io.Discard); err == nil {
		t.Fatal("a missing root must fail")
	}
	if _, err := parseArgs([]string{"extra"}, env(nil), noZdx, io.Discard); err == nil {
		t.Fatal("a positional argument must fail")
	}
	if _, err := parseArgs([]string{"-h"}, env(nil), noZdx, io.Discard); !errors.Is(err, flag.ErrHelp) {
		t.Fatalf("got %v", err)
	}
}
