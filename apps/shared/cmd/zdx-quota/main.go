// Command zdx-quota prints the live subscription quota of every stored
// provider account: the reader `zdx quota [--json]` runs in its own place.
// It reads credentials read-only through package quota (it never refreshes
// or writes oauth.json) and never runs zdx.
//
// Usage: zdx-quota [--root ROOT] [--zdx-version VERSION] [--json]
//
// The ZDX home is $ZDX_HOME, else ~/.zdx. --root is accepted because every
// Go program `zdx` runs gets it; quotas do not depend on it. The version is
// the zdx version for the providers' `zdx/<version>` user agent: `zdx`
// passes its own; without the flag it is the build-time version.
package main

import (
	"context"
	"flag"
	"fmt"
	"io"
	"os"
	"os/signal"
	"syscall"
	"time"

	"github.com/tallesborges/zdx/apps/shared/quota"
	"github.com/tallesborges/zdx/apps/shared/version"
)

func main() {
	ctx, stop := signal.NotifyContext(context.Background(), os.Interrupt, syscall.SIGTERM)
	code := run(ctx, os.Args[1:], os.Stdout, os.Stderr, quota.Options{})
	stop()
	os.Exit(code)
}

// run is the command with its inputs explicit. base carries test overrides
// (home, environment, HTTP, endpoints); the version comes from the flags.
func run(ctx context.Context, args []string, stdout, stderr io.Writer, base quota.Options) int {
	fs := flag.NewFlagSet("zdx-quota", flag.ContinueOnError)
	fs.SetOutput(stderr)
	fs.String("root", "", "project root (accepted from zdx; unused)")
	zdxVersion := fs.String("zdx-version", "", "zdx version for the provider user agent (default: the build-time version)")
	asJSON := fs.Bool("json", false, "emit machine-readable JSON instead of a text summary")
	if err := fs.Parse(args); err != nil {
		return 2
	}
	if fs.NArg() > 0 {
		fmt.Fprintf(stderr, "zdx-quota: unexpected argument %q\n", fs.Arg(0))
		return 2
	}
	v, err := version.Resolve(*zdxVersion)
	if err != nil {
		fmt.Fprintln(stderr, "zdx-quota:", err)
		return 1
	}
	opts := base
	opts.Version = v
	snapshot, err := quota.Fetch(ctx, opts)
	if err != nil {
		fmt.Fprintln(stderr, "zdx-quota:", err)
		return 1
	}
	if *asJSON {
		if _, err := stdout.Write(quota.CLIJSON(snapshot)); err != nil {
			return 1
		}
		return 0
	}
	now := time.Now
	if opts.Now != nil {
		now = opts.Now
	}
	if _, err := io.WriteString(stdout, quota.CLIText(snapshot, now())); err != nil {
		return 1
	}
	return 0
}
