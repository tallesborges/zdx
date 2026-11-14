// Command zdx-monitor is a read-only terminal dashboard for ZDX: services,
// active agents, threads and transcripts, jobs, usage, logs, background
// processes, and config. It reads the orchestrator socket's views and a few
// read-only CLI JSON commands; it never starts a bot, an engine, or a
// service.
package main

import (
	"context"
	"errors"
	"flag"
	"fmt"
	"io"
	"os"
	"os/exec"
	"os/signal"
	"path/filepath"
	"syscall"

	tea "charm.land/bubbletea/v2"

	"github.com/tallesborges/zdx/apps/monitor/internal/source"
	"github.com/tallesborges/zdx/apps/monitor/internal/ui"
)

const usage = `Usage: zdx-monitor [--root DIR] [--zdx-home DIR] [--zdx PATH] [--zdx-version V] [--cli-fallback]

Read-only ZDX dashboard (Bubble Tea).

  --root DIR       project root for CLI calls (default: current directory)
  --zdx-home DIR   ZDX home to read (default: $ZDX_HOME, else ~/.zdx)
  --zdx PATH       zdx binary for read-only CLI JSON (default: $ZDX_BIN, else zdx on PATH)
  --zdx-version V  zdx version for provider requests (default: the version this
                   binary was built with; a plain go build or go run has none)
  --cli-fallback   while the orchestrator service is down, build views with
                   'zdx views' in its own process (refreshes derived caches)
`

type options struct {
	root, home, zdx string
	zdxVersion      string
	cliFallback     bool
}

func parseArgs(args []string, getenv func(string) string, lookPath func(string) (string, error), stderr io.Writer) (options, error) {
	fs := flag.NewFlagSet("zdx-monitor", flag.ContinueOnError)
	fs.SetOutput(stderr)
	fs.Usage = func() { fmt.Fprint(stderr, usage) }
	var o options
	fs.StringVar(&o.root, "root", ".", "")
	fs.StringVar(&o.home, "zdx-home", "", "")
	fs.StringVar(&o.zdx, "zdx", "", "")
	fs.StringVar(&o.zdxVersion, "zdx-version", "", "")
	fs.BoolVar(&o.cliFallback, "cli-fallback", false, "")
	if err := fs.Parse(args); err != nil {
		return o, err
	}
	if fs.NArg() > 0 {
		return o, fmt.Errorf("unexpected argument %q", fs.Arg(0))
	}
	root, err := filepath.Abs(o.root)
	if err != nil {
		return o, err
	}
	if info, err := os.Stat(root); err != nil || !info.IsDir() {
		return o, fmt.Errorf("--root %s is not a directory", root)
	}
	o.root = root
	if o.home == "" {
		o.home = getenv("ZDX_HOME")
	}
	if o.home == "" {
		h, err := os.UserHomeDir()
		if err != nil {
			return o, fmt.Errorf("no home directory for the default ZDX home: %w", err)
		}
		o.home = filepath.Join(h, ".zdx")
	}
	if o.home, err = filepath.Abs(o.home); err != nil {
		return o, err
	}
	if o.zdx == "" {
		o.zdx = getenv("ZDX_BIN")
	}
	if o.zdx == "" {
		// A missing binary is not fatal: views still come from the socket,
		// and the tabs that need the CLI show the error.
		o.zdx, _ = lookPath("zdx")
	}
	if o.zdx != "" {
		if o.zdx, err = filepath.Abs(o.zdx); err != nil {
			return o, err
		}
	}
	return o, nil
}

func main() {
	o, err := parseArgs(os.Args[1:], os.Getenv, exec.LookPath, os.Stderr)
	if errors.Is(err, flag.ErrHelp) {
		return
	}
	if err != nil {
		fmt.Fprintln(os.Stderr, "zdx-monitor:", err)
		os.Exit(2)
	}
	if err := run(o); err != nil {
		fmt.Fprintln(os.Stderr, "zdx-monitor:", err)
		os.Exit(1)
	}
}

func run(o options) error {
	ctx, stop := signal.NotifyContext(context.Background(), syscall.SIGTERM, syscall.SIGHUP)
	defer stop()
	ctx, cancel := context.WithCancel(ctx)
	// Cancelling stops every running fetch: sockets close, CLI children die.
	defer cancel()
	src := source.New(source.Config{Root: o.root, Home: o.home, Zdx: o.zdx, ZdxVersion: o.zdxVersion, CLIFallback: o.cliFallback})
	model := ui.New(ctx, src, ui.Options{Root: o.root, Home: o.home, CLIFallback: o.cliFallback})
	_, err := tea.NewProgram(model, tea.WithContext(ctx)).Run()
	if errors.Is(err, tea.ErrProgramKilled) && ctx.Err() != nil {
		return nil
	}
	return err
}
