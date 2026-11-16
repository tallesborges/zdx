package gateway

import (
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"log/slog"
	"os/exec"
	"strings"
	"time"

	"github.com/tallesborges/zdx/apps/gateway/internal/bot"
	"github.com/tallesborges/zdx/apps/gateway/internal/commands"
	"github.com/tallesborges/zdx/apps/gateway/internal/conversation"
	"github.com/tallesborges/zdx/apps/gateway/internal/sandbox"
	"github.com/tallesborges/zdx/apps/gateway/internal/telegram"
)

// CommandRunner runs one zdx CLI command and returns its stdout. A
// non-zero exit is an error whose text is the command's stderr.
type CommandRunner func(ctx context.Context, args ...string) ([]byte, error)

// ExecRunner runs the zdx binary with env.
func ExecRunner(zdx string, env []string) CommandRunner {
	return func(ctx context.Context, args ...string) ([]byte, error) {
		if err := sandbox.CheckEnv(env); err != nil {
			return nil, err
		}
		cmd := exec.CommandContext(ctx, zdx, args...)
		cmd.Env = env
		var stdout, stderr bytes.Buffer
		cmd.Stdout, cmd.Stderr = &stdout, &stderr
		if err := cmd.Run(); err != nil {
			msg := strings.TrimSpace(stderr.String())
			if len(msg) > 1000 {
				msg = msg[:1000]
			}
			if msg == "" {
				msg = err.Error()
			}
			return nil, errors.New(msg)
		}
		return stdout.Bytes(), nil
	}
}

// Lifecycle is /restart and the queued restart over the zdx service CLI
// (Rust lifecycle contract v6). Rust gates and performs every restart of
// the other services; the gateway restarts itself only by exiting with
// ExitRequested for launchd to start it again.
type Lifecycle struct {
	Home string
	Run  CommandRunner
	// Exit asks the gateway to drain and exit (Deps.RequestExit).
	Exit func()
}

var _ commands.Lifecycle = (*Lifecycle)(nil)

// Supervised: run/bot.supervised names a live process.
func (l *Lifecycle) Supervised(context.Context) bool { return Supervised(l.Home) }

// QueueRestart is `zdx service restart all --queue --thread T --json`.
func (l *Lifecycle) QueueRestart(ctx context.Context, threadID string) (commands.QueuedRestart, error) {
	out, err := l.Run(ctx, "service", "restart", "all", "--queue", "--thread", threadID, "--json")
	if err != nil {
		return commands.QueuedRestart{}, err
	}
	var r struct {
		AlreadyPending *bool `json:"already_pending"`
		ActiveRuns     *int  `json:"active_runs"`
	}
	if err := json.Unmarshal(out, &r); err != nil || r.AlreadyPending == nil || r.ActiveRuns == nil {
		return commands.QueuedRestart{}, fmt.Errorf("unexpected answer from zdx service restart --queue: %.200s", out)
	}
	return commands.QueuedRestart{AlreadyPending: *r.AlreadyPending, ActiveRuns: *r.ActiveRuns}, nil
}

// RestartServices is `zdx service restart-peers [--force] --json`: the
// scheduler and an installed engine service, never the bot.
func (l *Lifecycle) RestartServices(ctx context.Context, force bool) (commands.RestartResult, error) {
	args := []string{"service", "restart-peers", "--json"}
	if force {
		args = []string{"service", "restart-peers", "--force", "--json"}
	}
	out, err := l.Run(ctx, args...)
	if err != nil {
		return commands.RestartResult{}, err
	}
	var r struct {
		Outcome string `json:"outcome"`
		Text    string `json:"text"`
	}
	if err := json.Unmarshal(out, &r); err != nil {
		return commands.RestartResult{}, fmt.Errorf("unexpected answer from zdx service restart-peers: %.200s", out)
	}
	switch r.Outcome {
	case "restarted":
		return commands.RestartResult{Outcome: commands.RestartDone, Text: r.Text}, nil
	case "blocked":
		return commands.RestartResult{Outcome: commands.RestartBlocked, Text: r.Text}, nil
	}
	return commands.RestartResult{}, fmt.Errorf("unknown restart outcome %q", r.Outcome)
}

// RequestExit asks the gateway to drain and exit.
func (l *Lifecycle) RequestExit() { l.Exit() }

// QueuedRestartPoll is how often the queued restart is checked, as in the
// Rust bot.
const QueuedRestartPoll = 3 * time.Second

// QueuedRestarts performs a restart queued with `/restart q` or `zdx
// service restart --queue` (Rust restart_queue.rs over the CLI).
type QueuedRestarts struct {
	Lifecycle *Lifecycle
	// Idle reports that the gateway holds no work of its own (Deps.Idle).
	Idle func() bool
	// Gate is closed around each check (Deps.Admission): no update or Mini
	// App write enters between the idle check and Rust's answer.
	Gate *bot.Admission
	// Telegram posts the announcement; Allow decides where it may go.
	Telegram *telegram.Client
	Allow    bot.Allowlist
	Log      *slog.Logger
	// Every is the poll interval (default QueuedRestartPoll).
	Every time.Duration
	// Timeout bounds one Rust check (default QueuedRestartTimeout).
	Timeout time.Duration

	// unknown is the request file as it was when a check timed out: that
	// request may be done in part, so it is not checked again until the
	// file changes (a new request).
	unknown *layerStamp
}

// QueuedRestartTimeout bounds one `zdx service run-queued-restart`, which
// runs while admission is closed (polling and Mini App writes wait).
const QueuedRestartTimeout = time.Minute

func (q *QueuedRestarts) requestStamp() (layerStamp, bool) {
	st := stampLayers([]string{runPath(q.Lifecycle.Home, "restart-queue.json")})[0]
	return st, st.exists
}

type queuedRestartResult struct {
	Status     string  `json:"status"`
	RestartBot bool    `json:"restart_bot"`
	ThreadID   *string `json:"thread_id"`
	Message    *string `json:"message"`
}

// Run polls until ctx ends or a done restart asks the bot to exit. It runs
// the Rust poller only while the gateway is idle and a request file exists;
// Rust checks agent runs and service settlement itself.
func (q *QueuedRestarts) Run(ctx context.Context) {
	every := q.Every
	if every == 0 {
		every = QueuedRestartPoll
	}
	t := time.NewTicker(every)
	defer t.Stop()
	for {
		select {
		case <-ctx.Done():
			return
		case <-t.C:
		}
		st, ok := q.requestStamp()
		if !ok || (q.unknown != nil && *q.unknown == st) {
			continue
		}
		q.unknown = nil
		if q.check(ctx) {
			return
		}
	}
}

// GateCloseTimeout bounds the wait for admitted work (a slow Mini App
// write) before a check; the check is skipped and tried again later.
const GateCloseTimeout = 10 * time.Second

// check closes admission, checks that the gateway is idle, and asks Rust;
// admission stays closed until Rust answers and opens again unless the
// gateway exits. true means it is exiting.
func (q *QueuedRestarts) check(ctx context.Context) bool {
	cctx, cancel := context.WithTimeout(ctx, GateCloseTimeout)
	err := q.Gate.Close(cctx)
	cancel()
	if err != nil {
		q.Log.Info("queued restart check skipped: admitted work did not finish in time", "err", err)
		return false
	}
	if !q.Idle() {
		q.Gate.Open()
		return false
	}
	if q.tick(ctx) {
		return true
	}
	q.Gate.Open()
	return false
}

// tick runs Rust's check; true means the gateway is exiting.
func (q *QueuedRestarts) tick(ctx context.Context) bool {
	timeout := q.Timeout
	if timeout == 0 {
		timeout = QueuedRestartTimeout
	}
	st, _ := q.requestStamp()
	tctx, cancel := context.WithTimeout(ctx, timeout)
	out, err := q.Lifecycle.Run(tctx, "service", "run-queued-restart", "--json")
	timedOut := ctx.Err() == nil && tctx.Err() != nil
	cancel()
	if timedOut {
		// Rust may have restarted services and not cleared the request:
		// checking it again could restart them a second time.
		q.unknown = &st
		q.Log.Error("queued restart check did not finish in time; its outcome is unknown and this request is not checked again (queue a new restart, or restart by hand)", "timeout", timeout, "err", err)
		return false
	}
	if err != nil {
		q.Log.Warn("queued restart check failed", "err", err)
		return false
	}
	var r queuedRestartResult
	if err := json.Unmarshal(out, &r); err != nil {
		q.Log.Warn("unexpected queued restart answer", "err", err)
		return false
	}
	if r.Message != nil && *r.Message != "" {
		q.announce(ctx, r.ThreadID, *r.Message)
	}
	if r.Status == "done" && r.RestartBot {
		q.Log.Info("queued restart done; exiting for launchd to restart the bot")
		q.Gate.Shut()
		q.Lifecycle.RequestExit()
		return true
	}
	return false
}

// announce posts the plain-text message to the thread's chat when it is a
// Telegram conversation of an allowlisted chat or user (Rust
// announce_target); otherwise it only logs it.
func (q *QueuedRestarts) announce(ctx context.Context, threadID *string, text string) {
	var route conversation.Route
	ok := threadID != nil
	if ok {
		route, ok = conversation.Parse(*threadID)
	}
	if !ok || !(q.Allow.Chats[route.Chat] || q.Allow.Users[route.Chat]) {
		q.Log.Info("queued restart (no thread to announce in)", "message", text)
		return
	}
	if _, err := q.Telegram.SendText(ctx, telegram.TextSend{ChatID: route.Chat, Text: bot.EscapeHTML(text), MessageThreadID: route.Topic, DisablePreview: true}); err != nil {
		q.Log.Warn("failed to announce the queued restart", "err", err)
	}
}
