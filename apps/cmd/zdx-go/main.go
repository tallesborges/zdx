// Command zdx-go is the Go half of zdx: one binary with a command per Go
// program. Rust `zdx` stays the command people type and execs this binary
// with the command first (crates/zdx-cli/src/cli/commands/go_program.rs);
// the bot service runs `zdx-go gateway --root ROOT bot` directly.
//
// Usage: zdx-go <command> ARGS
//
//	gateway  the Telegram bot and Mini App server (`zdx bot`, the bot service)
//	monitor  the read-only dashboard (`zdx monitor`)
//	tui      the interactive chat (bare `zdx` on a terminal)
//	quota    live subscription quotas (`zdx quota`)
//	jobs     the jobs program (`zdx jobs`, and the scheduler service)
//	git      the Git views as JSON (`zdx git`)
//	version  print the zdx version this binary was built with
package main

import (
	"fmt"
	"io"
	"os"

	gateway "github.com/tallesborges/zdx/apps/gateway/cli"
	monitor "github.com/tallesborges/zdx/apps/monitor/cli"
	git "github.com/tallesborges/zdx/apps/shared/gitview/cli"
	jobs "github.com/tallesborges/zdx/apps/shared/jobs/cli"
	quota "github.com/tallesborges/zdx/apps/shared/quota/cli"
	"github.com/tallesborges/zdx/apps/shared/version"
	tui "github.com/tallesborges/zdx/apps/tui/cli"
)

const usage = `usage: zdx-go <command> ARGS

  gateway  the Telegram bot and Mini App server (zdx bot, the bot service)
  monitor  the read-only dashboard (zdx monitor)
  tui      the interactive chat (bare zdx on a terminal)
  quota    live subscription quotas (zdx quota)
  jobs     the jobs program (zdx jobs, and the scheduler service)
  git      the Git views as JSON (zdx git)
  version  print the zdx version this binary was built with

Run these through zdx; it passes --root, ZDX_HOME, and ZDX_BIN.`

var commands = map[string]func([]string) int{
	"gateway": gateway.Main,
	"monitor": monitor.Main,
	"tui":     tui.Main,
	"quota":   quota.Main,
	"jobs":    jobs.Main,
	"git":     git.Main,
}

func main() {
	os.Exit(run(os.Args[1:], os.Stdout, os.Stderr))
}

func run(args []string, stdout, stderr io.Writer) int {
	if len(args) == 0 {
		fmt.Fprintln(stderr, usage)
		return 2
	}
	switch args[0] {
	case "-h", "--help", "help":
		fmt.Fprintln(stdout, usage)
		return 0
	case "version", "--version":
		if version.Zdx == "" {
			fmt.Fprintln(stderr, "zdx-go: built without a zdx version (build with apps/shared/ldflags.sh)")
			return 1
		}
		fmt.Fprintln(stdout, "zdx-go", version.Zdx)
		return 0
	}
	command, ok := commands[args[0]]
	if !ok {
		fmt.Fprintf(stderr, "zdx-go: unknown command %q\n%s\n", args[0], usage)
		return 2
	}
	return command(args[1:])
}
