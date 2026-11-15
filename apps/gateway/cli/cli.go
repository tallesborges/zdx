// Package cli is `zdx-go gateway`, the Go Telegram bot and Mini App gateway
// for zdx. It talks to the Rust orchestrator service over its Unix socket
// and owns only Telegram transport, presentation and HTTP.
//
// `serve` refuses to start: the bot runs as the service argv
// `--root ROOT bot`. Never run two bots for the same token.
package cli

import (
	"context"
	"encoding/json"
	"errors"
	"flag"
	"fmt"
	"io"
	"log/slog"
	"os"
	"os/exec"
	"os/signal"
	"path/filepath"
	"slices"
	"strings"
	"syscall"
	"time"

	"github.com/tallesborges/zdx/apps/gateway/internal/engine"
	"github.com/tallesborges/zdx/apps/gateway/internal/gateway"
)

const usage = `usage: zdx-go gateway <check|serve> --settings FILE|- [--zdx-home DIR] [--socket PATH]
       zdx-go gateway --root ROOT bot [--zdx PATH] [--web-dist DIR]
       zdx-go gateway preflight --root ROOT --json [--zdx PATH] [--web-dist DIR]

  check   read the settings, ask the orchestrator service for its protocol
          and capabilities, and list what the gateway still lacks. Read-only;
          does not contact Telegram.
  serve   run the bot and Mini App. Refuses while parts are missing.
  preflight  read-only readiness check for ` + "`zdx service install bot --program`" + `:
          prints {"service":"bot","ready":bool,"problems":[],"warnings":[]}.
          Takes no pidfile, does not contact Telegram, runs no service command.
  bot     the launchd form ` + "`zdx service install bot`" + ` runs:
          settings come from ` + "`zdx --root ROOT telegram settings --include-token`" + `,
          ZDX_HOME from the environment, --web-dist overrides the embedded
          Mini App. Refuses while parts are missing.

Settings are the output of ` + "`zdx telegram settings --include-token`" + ` (always JSON),
piped in: zdx telegram settings --include-token | zdx-go gateway check --settings -
`

// Main runs `zdx-go gateway ARGS` and returns the exit code.
func Main(args []string) int {
	if err := run(args, os.Stdin, os.Stdout); err != nil {
		fmt.Fprintln(os.Stderr, "zdx-go gateway:", err)
		return 2
	}
	return 0
}

func run(args []string, stdin io.Reader, stdout io.Writer) error {
	if len(args) == 0 {
		return errors.New(usage)
	}
	if args[0] == "preflight" || (strings.HasPrefix(args[0], "--root") && slices.Contains(args, "preflight")) {
		return runPreflight(args, stdout)
	}
	if strings.HasPrefix(args[0], "--root") {
		return runBot(args)
	}
	cmd := args[0]
	fs := flag.NewFlagSet(cmd, flag.ContinueOnError)
	settingsPath := fs.String("settings", "", "settings JSON file, or - for stdin")
	zdxHome := fs.String("zdx-home", defaultHome(), "ZDX home directory")
	socket := fs.String("socket", "", "orchestrator socket (default <zdx-home>/run/orchestrator.sock)")
	if err := fs.Parse(args[1:]); err != nil {
		return err
	}
	if *socket == "" {
		*socket = engine.SocketPath(*zdxHome)
	}
	switch cmd {
	case "check":
		settings, err := loadSettings(*settingsPath, stdin)
		if err != nil {
			return err
		}
		return check(settings, *socket, stdout)
	case "serve":
		if _, err := loadSettings(*settingsPath, stdin); err != nil {
			return err
		}
		return notReady()
	default:
		return errors.New(usage)
	}
}

func notReady() error {
	if len(gateway.Missing) == 0 {
		return errors.New("serve is not available: run the gateway as the bot service, `zdx-go gateway --root ROOT bot`")
	}
	return fmt.Errorf("serve is not available in this build; missing:\n  - %s", strings.Join(gateway.Missing, "\n  - "))
}

// runBot is the service argv `--root ROOT bot`. While gateway.Missing is
// not empty it only checks the settings and refuses, before it writes
// anything. Otherwise it serves until SIGTERM/SIGINT, and exits with
// gateway.ExitRequested when a restart asks for it.
func runBot(args []string) error {
	fs := flag.NewFlagSet("bot", flag.ContinueOnError)
	root := fs.String("root", "", "project root (the service's --root)")
	if err := fs.Parse(args); err != nil {
		return err
	}
	rest := fs.Args()
	if *root == "" || len(rest) == 0 || rest[0] != "bot" {
		return errors.New(usage)
	}
	sub := flag.NewFlagSet("bot", flag.ContinueOnError)
	zdx := sub.String("zdx", "", "zdx binary for settings and service commands (default: zdx on PATH)")
	webDist := sub.String("web-dist", "", "built Mini App directory (default: the embedded build)")
	if err := sub.Parse(rest[1:]); err != nil {
		return err
	}
	path, _, err := resolveZdx(*zdx)
	if err != nil {
		return err
	}
	*zdx = path
	if len(gateway.Missing) > 0 {
		ctx, cancel := context.WithTimeout(context.Background(), 30*time.Second)
		defer cancel()
		if _, err := gateway.LoadSettings(ctx, *zdx, *root, os.Environ()); err != nil {
			return err
		}
		return notReady()
	}
	ctx, stop := signal.NotifyContext(context.Background(), syscall.SIGTERM, syscall.SIGINT)
	defer stop()
	err = gateway.Serve(ctx, gateway.ServeOptions{
		Home: defaultHome(), Root: *root, Zdx: *zdx, Env: os.Environ(), Getenv: os.Getenv, WebDist: *webDist,
		Log: slog.New(slog.NewTextHandler(os.Stderr, nil)),
	})
	if errors.Is(err, gateway.ErrExitRequested) {
		os.Exit(gateway.ExitRequested)
	}
	return err
}

// runPreflight is `preflight --root ROOT --json [--zdx PATH] [--web-dist
// DIR]` (also `--root ROOT preflight --json`): the read-only readiness
// check `zdx service install bot --program` runs before it replaces the
// running bot. It always prints the JSON result and returns nil once it
// could run; `ready` carries the answer.
func runPreflight(args []string, stdout io.Writer) error {
	rest := slices.DeleteFunc(slices.Clone(args), func(a string) bool { return a == "preflight" })
	fs := flag.NewFlagSet("preflight", flag.ContinueOnError)
	root := fs.String("root", "", "project root (the service's --root)")
	asJSON := fs.Bool("json", false, "print the result as JSON (required)")
	zdx := fs.String("zdx", "", "zdx binary for settings (default: zdx on PATH)")
	webDist := fs.String("web-dist", "", "built Mini App directory (default: the embedded build)")
	if err := fs.Parse(rest); err != nil {
		return err
	}
	if *root == "" || !*asJSON || fs.NArg() != 0 {
		return errors.New("usage: zdx-go gateway preflight --root ROOT --json [--zdx PATH] [--web-dist DIR]")
	}
	path, from, err := resolveZdx(*zdx)
	if err != nil {
		return err
	}
	r := gateway.Preflight(context.Background(), gateway.PreflightOptions{
		Home: defaultHome(), Root: *root, Zdx: path, ZdxFrom: from, Env: os.Environ(), WebDist: *webDist,
	})
	return json.NewEncoder(stdout).Encode(r)
}

// resolveZdx is the engine binary the gateway runs for settings and
// service commands: --zdx, else $ZDX_BIN (the bot service plist sets it),
// else zdx on PATH. from names which one it was.
func resolveZdx(flagValue string) (path, from string, err error) {
	switch {
	case flagValue != "":
		path, from = flagValue, "--zdx"
	case os.Getenv("ZDX_BIN") != "":
		path, from = os.Getenv("ZDX_BIN"), "ZDX_BIN"
	default:
		if path, err = exec.LookPath("zdx"); err != nil {
			return "", "", fmt.Errorf("find zdx for settings (pass --zdx or set ZDX_BIN): %w", err)
		}
		from = "PATH"
	}
	if path, err = filepath.Abs(path); err != nil {
		return "", "", err
	}
	return path, from, nil
}

func defaultHome() string {
	if h := os.Getenv("ZDX_HOME"); h != "" {
		return h
	}
	home, err := os.UserHomeDir()
	if err != nil {
		return ".zdx"
	}
	return filepath.Join(home, ".zdx")
}

func loadSettings(path string, stdin io.Reader) (gateway.Settings, error) {
	switch path {
	case "":
		return gateway.Settings{}, errors.New("--settings is required")
	case "-":
		return gateway.ReadSettings(stdin)
	}
	f, err := os.Open(path)
	if err != nil {
		return gateway.Settings{}, err
	}
	defer f.Close()
	return gateway.ReadSettings(f)
}

func check(settings gateway.Settings, socket string, out io.Writer) error {
	fmt.Fprintln(out, "settings:", settings)
	ctx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
	defer cancel()
	report, err := engine.NewClient(socket).StatusReport(ctx, nil)
	if err != nil {
		fmt.Fprintln(out, "orchestrator service:", err)
	} else {
		fmt.Fprintf(out, "orchestrator service: protocol %d, capabilities %v\n", report.Version(), report.Capabilities)
	}
	fmt.Fprintln(out, "missing:")
	for _, m := range gateway.Missing {
		fmt.Fprintln(out, "  -", m)
	}
	fmt.Fprintln(out, "acceptance checks before cutover (need a deployment):")
	for _, m := range gateway.AcceptanceChecks {
		fmt.Fprintln(out, "  -", m)
	}
	return nil
}
