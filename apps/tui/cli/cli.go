// Package cli is `zdx-go tui`, the interactive ZDX chat in the terminal. Every turn
// runs in the orchestrator service: this program composes messages, sends
// them over the service socket, and streams each turn's events. It reads
// and writes no thread, index, config, or credential file itself.
package cli

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

	"github.com/tallesborges/zdx/apps/tui/internal/engine"
	"github.com/tallesborges/zdx/apps/tui/internal/ui"
)

const usage = `Usage: zdx-go tui [--root DIR] [--zdx-home DIR] [--zdx PATH] [--thread ID] [--model SPEC]

Interactive ZDX chat (Bubble Tea). Turns run in the orchestrator service
(zdx service start orchestrator).

  --root DIR       project root: config layers load from it, and a new
                   thread runs there (default: current directory)
  --zdx-home DIR   ZDX home (default: $ZDX_HOME, else ~/.zdx)
  --zdx PATH       zdx binary for /login and /logout (default: $ZDX_BIN,
                   else zdx on PATH)
  --thread ID      resume this thread (default: a new thread)
  --model SPEC     set the thread's model (a spec or mode:NAME) at start
`

type options struct {
	root, home, zdx, thread, model string
}

func parseArgs(args []string, getenv func(string) string, lookPath func(string) (string, error), stderr io.Writer) (options, error) {
	fs := flag.NewFlagSet("zdx-go tui", flag.ContinueOnError)
	fs.SetOutput(stderr)
	fs.Usage = func() { fmt.Fprint(stderr, usage) }
	var o options
	fs.StringVar(&o.root, "root", ".", "")
	fs.StringVar(&o.home, "zdx-home", "", "")
	fs.StringVar(&o.zdx, "zdx", "", "")
	fs.StringVar(&o.thread, "thread", "", "")
	fs.StringVar(&o.model, "model", "", "")
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
		o.zdx, _ = lookPath("zdx")
	}
	return o, nil
}

// Main runs `zdx-go tui ARGS` and returns the exit code.
func Main(args []string) int {
	o, err := parseArgs(args, os.Getenv, exec.LookPath, os.Stderr)
	if errors.Is(err, flag.ErrHelp) {
		return 0
	}
	if err != nil {
		fmt.Fprintln(os.Stderr, "zdx-go tui:", err)
		return 2
	}
	if err := run(o); err != nil {
		fmt.Fprintln(os.Stderr, "zdx-go tui:", err)
		return 1
	}
	return 0
}

func run(o options) error {
	ctx, stop := signal.NotifyContext(context.Background(), syscall.SIGTERM, syscall.SIGHUP)
	defer stop()
	ctx, cancel := context.WithCancel(ctx)
	// Cancelling closes every turn stream; turns keep running in the
	// service.
	defer cancel()
	client := engine.New(o.home, o.root)
	if err := client.Require(ctx, engine.Required...); err != nil {
		return err
	}
	model := ui.New(ctx, client, ui.Options{Root: o.root, Home: o.home, Zdx: o.zdx, ThreadID: o.thread, Model: o.model})
	defer model.Close()
	_, err := tea.NewProgram(model, tea.WithContext(ctx)).Run()
	if errors.Is(err, tea.ErrProgramKilled) && ctx.Err() != nil {
		return nil
	}
	return err
}
