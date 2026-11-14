package gateway

import (
	"context"
	"errors"
	"fmt"
	"io/fs"
	"log/slog"
	"net"
	"time"

	"github.com/tallesborges/zdx/apps/gateway/internal/engine"
	"github.com/tallesborges/zdx/apps/gateway/internal/telegram"
)

// ServeOptions is one gateway process as the bot service runs it.
type ServeOptions struct {
	// Home is the ZDX home; Root the service's --root.
	Home, Root string
	// Zdx is the zdx binary for settings and CLI operations; Env its
	// environment.
	Zdx string
	Env []string
	// Getenv reads the gateway's own environment (supervision marker).
	Getenv func(string) string
	// WebDist overrides the embedded Mini App build.
	WebDist string
	// Socket overrides engine.SocketPath(Home).
	Socket string
	// Telegram, HTTPAddr, Ready and DrainTimeout are passed to Run (tests).
	Telegram     telegram.Options
	HTTPAddr     string
	Ready        func(net.Addr)
	DrainTimeout time.Duration
	// QueuedRestartEvery overrides QueuedRestartPoll (tests).
	QueuedRestartEvery time.Duration
	Log                *slog.Logger
}

// Serve starts the gateway as the bot service: it takes run/bot.pid first
// (refusing a second bot), loads the Rust-resolved settings, checks the Mini
// App build when the server is enabled, marks launchd supervision, and runs
// until ctx ends or a restart asks it to exit (ErrExitRequested). The
// pidfile is released on return.
func Serve(ctx context.Context, o ServeOptions) error {
	log := o.Log
	if log == nil {
		log = slog.Default()
	}
	pid, err := AcquirePidFile(o.Home)
	if err != nil {
		return err
	}
	defer pid.Release()

	sctx, cancel := context.WithTimeout(ctx, 30*time.Second)
	settings, err := LoadSettings(sctx, o.Zdx, o.Root, o.Env)
	cancel()
	if err != nil {
		return err
	}
	settings = WatchSettings(settings, func(ctx context.Context) (Settings, error) {
		return LoadSettings(ctx, o.Zdx, o.Root, o.Env)
	}, log)
	var assets fs.FS
	if settings.Server.Enabled {
		if assets, err = MiniAppAssets(o.WebDist); err != nil {
			return fmt.Errorf("mini app: %w", err)
		}
	}
	getenv := o.Getenv
	if getenv == nil {
		getenv = func(string) string { return "" }
	}
	if _, err := MarkSupervised(o.Home, getenv); err != nil {
		log.Warn("could not mark launchd supervision; /restart will refuse", "err", err)
	}
	socket := o.Socket
	if socket == "" {
		socket = engine.SocketPath(o.Home)
	}
	err = Run(ctx, Config{
		Settings: settings, Socket: socket, Home: o.Home, Zdx: o.Zdx, Telegram: o.Telegram, Assets: assets,
		HTTPAddr: o.HTTPAddr, Ready: o.Ready, DrainTimeout: o.DrainTimeout, Log: log,
		Build: func(d Deps) (Components, error) { return serveComponents(d, settings, o, log), nil },
	})
	if errors.Is(err, ErrExitRequested) {
		log.Info("exiting for a restart", "code", ExitRequested)
	}
	return err
}

// serveComponents is the production bot: the service parts, /restart over
// the zdx CLI, and the queued-restart poller beside polling.
func serveComponents(d Deps, settings Settings, o ServeOptions, log *slog.Logger) Components {
	lifecycle := &Lifecycle{Home: o.Home, Run: ExecRunner(o.Zdx, o.Env), Exit: d.RequestExit}
	parts := ServiceParts(d, settings)
	parts.Lifecycle = lifecycle
	comps := BotComponents(d.Work, d, settings, parts)
	restarts := &QueuedRestarts{Lifecycle: lifecycle, Idle: d.Idle, Gate: d.Admission, Telegram: d.Telegram, Allow: d.Allow, Log: log, Every: o.QueuedRestartEvery}
	comps.Background = restarts.Run
	return comps
}
