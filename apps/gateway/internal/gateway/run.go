package gateway

import (
	"context"
	"errors"
	"fmt"
	"io/fs"
	"log/slog"
	"net"
	"net/http"
	"path/filepath"
	"slices"
	"strconv"
	"sync"
	"sync/atomic"
	"time"

	"github.com/tallesborges/zdx/apps/gateway/internal/bot"
	"github.com/tallesborges/zdx/apps/gateway/internal/commands"
	"github.com/tallesborges/zdx/apps/gateway/internal/engine"
	"github.com/tallesborges/zdx/apps/gateway/internal/miniapp"
	"github.com/tallesborges/zdx/apps/gateway/internal/render"
	"github.com/tallesborges/zdx/apps/gateway/internal/telegram"
	"github.com/tallesborges/zdx/apps/shared/quota"
	"github.com/tallesborges/zdx/apps/shared/version"
)

// Deps are the shared handles the bot components are built from.
type Deps struct {
	Telegram   *telegram.Client
	Engine     *engine.Client
	Allow      bot.Allowlist
	Queues     *bot.TopicQueues
	Deliveries *miniapp.DeliveryTracker
	// Home is the ZDX home; attachments are saved under Home/telegram.
	Home string
	// Zdx is the zdx binary for CLI operations (worktrees); "" leaves them
	// unavailable.
	Zdx string
	Log *slog.Logger
	// Work is the context of the gateway's work. It outlives polling: on
	// shutdown polling stops first and work drains (Config.DrainTimeout).
	Work context.Context
	// RequestExit stops the gateway like a shutdown and makes Run return
	// ErrExitRequested, for the supervisor to start it again.
	RequestExit func()
	// Idle reports that the gateway holds no work of its own: no intake job,
	// album, handler or command goroutine, or rendered turn. Read it only
	// with Admission closed, or new work can enter right after.
	Idle func() bool
	// Admission gates new outside work (polled updates, Mini App writes).
	Admission *bot.Admission
}

// Components are the presentation parts of the bot: message intake, button
// handling, and turn rendering.
type Components struct {
	Messages  bot.MessageSink
	Callbacks bot.CallbackSink
	Renderer  bot.TurnRenderer
	Observe   bot.EventObserver
	// Wait, when set, blocks on shutdown until started renders finish.
	Wait func()
	// RendererReady, when set, gates the renderer subscription.
	RendererReady func(context.Context) error
	// Background, when set, runs beside polling and stops with it (the
	// queued-restart poller).
	Background func(ctx context.Context)
	// Connected, when set, runs each time the renderer subscription opens
	// (the service may have restarted and forgotten what it knew).
	Connected func()
	// Before, when set, runs before a service-started turn renders
	// (bot.Bridge.Before).
	Before func(ctx context.Context, threadID string)
	// Busy, when set, reports goroutines the components started that are
	// still running (part of Deps.Idle).
	Busy func() bool
}

// Parts are the bot parts that come from Rust or are not built yet.
type Parts struct {
	Planner   bot.ReplyPlanner
	Resolver  bot.TopicResolver
	Artifacts bot.ArtifactFinder
	// Attachments loads message files; nil refuses messages with media.
	Attachments bot.Attachments
	// Lifecycle runs /restart; nil refuses it.
	Lifecycle commands.Lifecycle
}

// ServiceParts are the production parts: reply plans from Rust reply_parts,
// topics from resolve_thread, attachments saved under Home/telegram as the
// Rust bot saves them (images and audio prepared by Rust), Send file
// lookups from Rust artifact views.
func ServiceParts(d Deps, settings Settings) Parts {
	return Parts{
		Planner:     bot.ServicePlanner{Engine: d.Engine, MiniAppBase: settings.MiniAppURL()},
		Resolver:    bot.ServiceResolver{Engine: d.Engine},
		Artifacts:   bot.ServiceArtifacts{Engine: d.Engine},
		Attachments: ServiceAttachments(d.Telegram, d.Engine, d.Home, settings.RootForChat, d.Log),
	}
}

// TurnCapabilities are what rendering a routed turn needs: a turn is not
// submitted, and the renderer subscription is not opened, without them.
// Hold tokens make the holds safe: one renderer, releases bound to their
// holder, records bound to their turn, and replay of a reply whose holder
// went away (Rust holds v7). There is no fallback without them.
var TurnCapabilities = []string{engine.CapRecordDelivery, engine.CapReplyParts, engine.CapHoldTokens}

// ServiceCapabilities are every capability the gateway uses: preflight
// refuses an orchestrator service that lacks one. Each operation still
// checks its own at the call (the service can restart onto another build).
var ServiceCapabilities = append(slices.Clone(TurnCapabilities),
	engine.CapOrdinarySubmit, engine.CapRerun, engine.CapUpdateThread, engine.CapResetThread, engine.CapResolveThread,
	engine.CapComposeUserText, engine.CapPrepareImage, engine.CapTranscribeAudio, engine.CapPreparePhoto,
	engine.CapTouchedWorkers, engine.CapWorkerMirrorRoute, engine.CapFeedbackSubmit, engine.CapGenerate,
	engine.CapBtwSeed, engine.CapTopicStart, engine.CapCatalog, engine.CapSetChatModel,
	engine.CapThreadViews, engine.CapArtifactViews, engine.CapMonitorViews, engine.CapThreadRoot,
)

// BotComponents builds the bot from the shared handles.
func BotComponents(ctx context.Context, d Deps, settings Settings, parts Parts) Components {
	ready := func(ctx context.Context) error { return d.Engine.RequireAll(ctx, TurnCapabilities...) }
	presenter := &bot.Presenter{
		Telegram:   d.Telegram,
		Engine:     d.Engine,
		Retries:    &bot.Retries{},
		Planner:    parts.Planner,
		Deliverer:  &bot.Deliverer{Telegram: d.Telegram, Store: d.Engine, Tracker: d.Deliveries, Photos: bot.ServicePhotos{Engine: d.Engine}, Followups: &bot.Followups{}, Log: d.Log},
		Cancels:    &bot.Cancels{},
		MiniAppURL: settings.MiniAppURL(),
		Log:        d.Log,
	}
	acks := &bot.SteerAcks{}
	h := &bot.Handler{
		Telegram: d.Telegram, Engine: d.Engine, Resolver: parts.Resolver, Composer: d.Engine, Turns: d.Engine,
		Queues: d.Queues, Presenter: presenter, Acks: acks, Artifacts: parts.Artifacts, RootForChat: settings.RootForChat,
		TurnReady: ready, ThreadOps: d.Engine, Attachments: parts.Attachments, Log: d.Log,
		// TurnReady requires hold_tokens, so every followed turn opts in.
		Replay: true,
	}
	blocks := &bot.TopicBlocks{Dir: filepath.Join(d.Home, "telegram", "unsetup-topics")}
	hook := commandHook(d, settings, h, blocks, parts.Lifecycle)
	h.Commands, h.CommandCallbacks, h.Bypass, h.Blocks = hook, hook, queueBypass{hook}, blocks
	h.Topics = topicHooks{chats: settingsChats{settings: settings, allow: d.Allow}, hook: hook}
	steer := bot.SteerObserver(ctx, acks, h, d.Log)
	mirrors := &bot.MirrorFeed{Telegram: d.Telegram, Lookup: d.Engine.WorkerMirror, MiniAppBase: settings.MiniAppURL(), Log: d.Log}
	workers := &bot.WorkerNotices{Telegram: d.Telegram, Engine: d.Engine, MiniAppBase: settings.MiniAppURL(), RefreshHeader: hook.RefreshHeader, Mirrors: mirrors, Log: d.Log}
	observe := func(ev engine.Event) {
		steer(ev)
		workers.Dispatch(ctx, ev)
	}
	return Components{
		Messages: h, Callbacks: h, Renderer: presenter, Observe: observe,
		Wait:          func() { h.Wait(); hook.Wait(); workers.Wait() },
		RendererReady: ready,
		Connected:     mirrors.Forget,
		Before:        workers.Before,
		Busy:          func() bool { return h.Busy() || hookBusy(hook) || workers.Busy() },
	}
}

// hookBusy reports command goroutines still running, when the commands
// package can tell (a Busy method); otherwise it reports none.
func hookBusy(hook any) bool {
	b, ok := hook.(interface{ Busy() bool })
	return ok && b.Busy()
}

// Missing lists what the implementation still lacks. serve refuses to
// start while it is not empty. Checks that need a deployment are not here:
// see AcceptanceChecks.
var Missing = []string{}

// AcceptanceChecks must pass on the real machine before cutover. They need
// a deployment, so they do not block serve; `check` lists them.
var AcceptanceChecks = []string{
	"with the real Bot API: a message, a reply with files, a button tap and a Mini App open through the gateway",
	"under launchd (`zdx service install bot --program <zdx-gateway>`): start, /restart, a queued restart (exit 42 and back), and stop-old/start-new from the Rust bot with no update lost",
}

// Config is one gateway process.
type Config struct {
	Settings Settings
	// Socket is the orchestrator service socket.
	Socket string
	// Home is the ZDX home (Deps.Home).
	Home string
	// Zdx is the zdx binary (Deps.Zdx).
	Zdx string
	// Telegram overrides client options (tests use a fake Bot API).
	Telegram telegram.Options
	// Assets is the built Mini App (apps/web/dist); nil makes the shell 500.
	Assets fs.FS
	// HTTPAddr overrides the Mini App listen address (default
	// ":<server.port>", all interfaces, as the Rust server binds).
	HTTPAddr string
	// Build makes the presentation components from the shared handles.
	Build func(Deps) (Components, error)
	Log   *slog.Logger
	// Ready is called with the Mini App address once it listens.
	Ready func(httpAddr net.Addr)
	// DrainTimeout bounds the drain on shutdown (default DefaultDrainTimeout).
	DrainTimeout time.Duration
}

// DefaultDrainTimeout is how long a shutdown waits for started work.
const DefaultDrainTimeout = 2 * time.Minute

// ErrExitRequested: Deps.RequestExit stopped the gateway (a restart). The
// process exits with ExitRequested.
var ErrExitRequested = errors.New("exit requested")

// Run runs polling, the renderer bridge and the Mini App until ctx ends or
// Deps.RequestExit is called.
//
// Shutdown is a drain, not a cut: admission shuts and polling stops first,
// the Mini App stops taking requests, and started work finishes (intake
// jobs, their turns and replies, rendered service turns, command
// goroutines). Then Telegram is told the handed-off updates are done
// (Poller.Confirm). Only after DrainTimeout is the remaining work
// cancelled; then nothing is confirmed, a reply cut off may be partial or
// lost, and the cancel is logged with what was still running. Unknown
// sends are never resent.
//
// Never run it next to the Rust bot for the same token: two pollers split
// updates, and two renderer subscriptions fight over routed-turn holds.
func Run(ctx context.Context, cfg Config) error {
	log := cfg.Log
	if log == nil {
		log = slog.Default()
	}
	if cfg.Build == nil {
		return errors.New("gateway: no components")
	}
	drainTimeout := cfg.DrainTimeout
	if drainTimeout == 0 {
		drainTimeout = DefaultDrainTimeout
	}
	opts := cfg.Telegram
	if opts.Logger == nil {
		opts.Logger = log
	}
	// stop ends polling (shutdown or exit request); work outlives it until
	// the drain ends or times out.
	stopCtx, stop := context.WithCancel(ctx)
	defer stop()
	work, force := context.WithCancel(context.WithoutCancel(ctx))
	defer force()
	var exitRequested atomic.Bool
	requestExit := func() {
		exitRequested.Store(true)
		stop()
	}

	tg := telegram.NewClient(cfg.Settings.BotToken, opts)
	tg.Repair = render.Sanitize
	eng := engine.NewClient(cfg.Socket)
	allow := bot.NewAllowlist(cfg.Settings.AllowlistUserIDs, cfg.Settings.AllowlistChatIDs)
	queues := bot.NewTopicQueues(work)
	deliveries := &miniapp.DeliveryTracker{}
	admission := &bot.Admission{}
	// Set before any goroutine that calls Idle starts.
	var router *bot.Router
	var bridge *bot.Bridge
	var compsBusy func() bool
	idle := func() bool {
		return !queues.Busy() && !router.Busy() && bridge.Active() == 0 && (compsBusy == nil || !compsBusy())
	}
	comps, err := cfg.Build(Deps{
		Telegram: tg, Engine: eng, Allow: allow, Queues: queues, Deliveries: deliveries, Home: cfg.Home, Zdx: cfg.Zdx, Log: log,
		Work: work, RequestExit: requestExit, Idle: idle, Admission: admission,
	})
	if err != nil {
		return err
	}
	if comps.Messages == nil || comps.Callbacks == nil || comps.Renderer == nil {
		return errors.New("gateway: message, callback and renderer components are required")
	}

	var srv *http.Server
	var ln net.Listener
	if cfg.Settings.Server.Enabled {
		zdxVersion, err := version.Resolve("")
		if err != nil {
			log.Warn("Mini App subscription quotas are unavailable", "error", err)
		}
		handler, err := miniapp.New(miniapp.Config{
			BotToken:       cfg.Settings.BotToken,
			AllowedUserIDs: cfg.Settings.AllowlistUserIDs,
			Assets:         cfg.Assets,
			Engine:         &MiniAppEngine{Client: eng, RootForChat: cfg.Settings.RootForChat, Root: cfg.Settings.Root, Telegram: tg, Log: log, Quota: quota.Options{Home: cfg.Home, Version: zdxVersion}, Home: cfg.Home},
			Deliveries:     deliveries,
			Feedback:       FeedbackGate{Allow: allow},
			Logger:         log,
		})
		if err != nil {
			return err
		}
		addr := cfg.HTTPAddr
		if addr == "" {
			addr = ":" + strconv.Itoa(int(cfg.Settings.Server.Port))
		}
		ln, err = net.Listen("tcp", addr)
		if err != nil {
			return fmt.Errorf("mini app listen on %s: %w", addr, err)
		}
		// No read or write deadline: feedback uploads (up to 48 MiB) can be
		// slow, and the miniapp package bounds bodies itself.
		srv = &http.Server{Handler: admitWrites{next: handler, gate: admission, settings: cfg.Settings}, ReadHeaderTimeout: 10 * time.Second, IdleTimeout: 120 * time.Second, MaxHeaderBytes: 64 << 10,
			BaseContext: func(net.Listener) context.Context { return work }}
		if cfg.Ready != nil {
			cfg.Ready(ln.Addr())
		}
	}

	var errMu sync.Mutex
	var firstErr error
	fail := func(err error) {
		errMu.Lock()
		if firstErr == nil {
			firstErr = err
		}
		errMu.Unlock()
		stop()
	}
	router = &bot.Router{Allow: allow, Answer: tg, Messages: comps.Messages, Callbacks: comps.Callbacks, Log: log}
	// Every routed message needs the service (resolve, compose, submit):
	// while it does not answer, polled batches stay unconfirmed.
	serviceUp := func(ctx context.Context) error { _, err := eng.StatusReport(ctx, nil); return err }
	poller := &bot.Poller{Source: tg, Handler: refreshing{next: router, settings: cfg.Settings}, Log: log, Work: work, Admission: admission, Ready: serviceUp}
	// RendererReady requires hold_tokens, so the renderer opts in to replay.
	bridge = &bot.Bridge{Engine: eng, Renderer: comps.Renderer, Observe: comps.Observe, Ready: comps.RendererReady, Connected: comps.Connected, Before: comps.Before, Log: log, Replay: comps.RendererReady != nil}
	compsBusy = comps.Busy

	polled := make(chan struct{})
	go func() {
		defer close(polled)
		if err := poller.Run(stopCtx); err != nil {
			log.Error("polling stopped", "err", err)
			fail(err)
		}
	}()
	background := make(chan struct{})
	go func() {
		defer close(background)
		if comps.Background != nil {
			comps.Background(stopCtx)
		}
	}()
	bridged := make(chan struct{})
	go func() {
		defer close(bridged)
		if err := bridge.Run(work); err != nil {
			fail(err)
		}
	}()
	served := make(chan struct{})
	if srv != nil {
		go func() {
			defer close(served)
			if err := srv.Serve(ln); err != nil && !errors.Is(err, http.ErrServerClosed) {
				fail(err)
			}
		}()
	} else {
		close(served)
	}
	log.Info("gateway started", "allowlist_users", len(allow.Users), "allowlist_chats", len(allow.Chats), "miniapp", srv != nil)

	<-stopCtx.Done()
	admission.Shut()
	<-polled
	<-background
	log.Info("gateway stopping: polling stopped, draining", "timeout", drainTimeout)

	deadline, dcancel := context.WithTimeout(context.Background(), drainTimeout)
	defer dcancel()
	if srv != nil {
		if err := srv.Shutdown(deadline); err != nil {
			log.Warn("mini app requests still running at the drain deadline", "err", err)
		}
	}
	drained := make(chan struct{})
	go func() {
		defer close(drained)
		router.WaitFlushes()
		for {
			queues.Wait()
			if comps.Wait != nil {
				comps.Wait()
			}
			if bridge.Active() == 0 && !queues.Busy() && (comps.Busy == nil || !comps.Busy()) {
				return
			}
			time.Sleep(20 * time.Millisecond)
		}
	}()
	select {
	case <-drained:
		log.Info("gateway drained")
		// Every handed-off update was submitted or answered: confirm them.
		cctx, ccancel := context.WithTimeout(context.Background(), 10*time.Second)
		if err := poller.Confirm(cctx); err != nil {
			log.Warn("could not confirm the last updates; Telegram will send them again", "err", err)
		}
		ccancel()
	case <-deadline.Done():
		// Not confirmed: the last batch comes again to the next process, as
		// after a crash, rather than being lost with the cancelled work.
		log.Error("drain timed out; cancelling the remaining work, whose replies may be partial or lost; the last updates are not confirmed",
			"intake_busy", queues.Busy(), "service_turns_rendering", bridge.Active())
	}
	force()
	// Cancelled work ends quickly; holds are still released on a fresh
	// context (bot.Render), so allow one reply timeout for that.
	select {
	case <-drained:
	case <-time.After(engine.ReplyTimeout + 5*time.Second):
		log.Error("work still running after cancel; exiting anyway")
	}
	<-bridged
	<-served

	errMu.Lock()
	defer errMu.Unlock()
	if firstErr != nil {
		return firstErr
	}
	if exitRequested.Load() {
		return ErrExitRequested
	}
	return nil
}

// admitWrites gates Mini App writes (anything but GET, HEAD and OPTIONS)
// on admission: while a queued restart checks for idle they wait, and once
// the gateway is exiting they get 503 instead of starting work.
type admitWrites struct {
	next     http.Handler
	gate     *bot.Admission
	settings Settings
}

// refreshing picks up profile edits before each update is handed off, as
// the Rust bot reloads before each turn; every part of the bot reads the
// same current profiles (Settings.Watch).
type refreshing struct {
	next     bot.UpdateHandler
	settings Settings
}

func (r refreshing) HandleMessage(ctx context.Context, m *telegram.Message, rec bot.Receipt) {
	r.settings.Refresh(ctx)
	r.next.HandleMessage(ctx, m, rec)
}

func (r refreshing) HandleCallback(ctx context.Context, q *telegram.CallbackQuery) {
	r.settings.Refresh(ctx)
	r.next.HandleCallback(ctx, q)
}

// RestartingText answers a Mini App write refused while the gateway exits.
const RestartingText = "The bot is restarting. Try again in a moment."

func (a admitWrites) ServeHTTP(w http.ResponseWriter, r *http.Request) {
	switch r.Method {
	case http.MethodGet, http.MethodHead, http.MethodOptions:
		a.next.ServeHTTP(w, r)
		return
	}
	release, ok := a.gate.Admit(r.Context())
	if !ok {
		http.Error(w, RestartingText, http.StatusServiceUnavailable)
		return
	}
	defer release()
	a.settings.Refresh(r.Context())
	a.next.ServeHTTP(w, r)
}
