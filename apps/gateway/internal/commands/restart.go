package commands

import (
	"context"
	"strconv"

	"github.com/tallesborges/zdx/apps/gateway/internal/conversation"
)

// QueuedRestart is the answer to a queued restart request.
type QueuedRestart struct {
	// AlreadyPending: a restart was already queued; this one merged in.
	AlreadyPending bool
	// ActiveRuns is how many agent runs are active now.
	ActiveRuns int
}

// Restart outcomes of Lifecycle.RestartServices.
const (
	// RestartDone: the services restarted; the gateway exits next.
	RestartDone = "restarted"
	// RestartBlocked: refused because active work would be lost.
	RestartBlocked = "blocked"
)

// RestartResult is a restart attempt that did not fail. Text is the
// services' status (RestartDone) or the guard's reason (RestartBlocked),
// plain text.
type RestartResult struct {
	Outcome string
	Text    string
}

// Lifecycle is the gateway's process and service control for /restart.
// The gateway owns it (supervision, the shared restart request, the
// guarded service restart, its own exit); this package only words the
// answers.
type Lifecycle interface {
	// Supervised: a supervisor restarts the gateway when it exits.
	Supervised(ctx context.Context) bool
	// QueueRestart records a restart that runs once no agent run, worker or
	// pending orchestrator turn is left (zdx service restart --queue),
	// announced in threadID.
	QueueRestart(ctx context.Context, threadID string) (QueuedRestart, error)
	// RestartServices restarts the daemon and, when installed, the
	// orchestrator service, refusing while work is active unless force. An
	// error means the restart failed and the gateway stays up.
	RestartServices(ctx context.Context, force bool) (RestartResult, error)
	// RequestExit asks the gateway to exit, for its supervisor to restart
	// it. Called after the answer is sent.
	RequestExit()
}

const notSupervised = "⚠️ No active supervisor — refusing to exit. Enable supervision in `zdx monitor` (Ctrl+R on `bot`) first."

// restart is /restart [f|q] (Rust handle_restart_command). The topic's
// conversation id, not its alias, names where a queued restart is
// announced.
func (h *Handler) restart(ctx context.Context, m Message) {
	mode, ok := ParseRestart(m.Text)
	if !ok {
		return
	}
	say := func(html string) { h.reply(ctx, m, html, nil) }
	if !h.Lifecycle.Supervised(ctx) {
		say(notSupervised)
		return
	}
	if mode == RestartQueued {
		q, err := h.Lifecycle.QueueRestart(ctx, conversation.ThreadID(m.Chat, m.Topic))
		active := strconv.Itoa(q.ActiveRuns)
		switch {
		case err != nil:
			say("⚠️ Couldn't queue the restart.\n<code>" + esc(err.Error()) + "</code>")
		case q.AlreadyPending:
			say("⏳ A restart is already queued; it fires once no agent run, worker, or pending orchestrator turn is left (" + active + " agent runs active now) and will be announced here.")
		default:
			say("⏳ Restart queued. I'll restart once no agent run, worker, or pending orchestrator turn is left (" + active + " agent runs active now).\n\n<code>/restart f</code> restarts immediately instead.")
		}
		return
	}
	r, err := h.Lifecycle.RestartServices(ctx, mode == RestartForce)
	switch {
	case err != nil:
		say("⚠️ Service restart failed, so I left the bot running.\n<code>" + esc(err.Error()) + "</code>")
	case r.Outcome == RestartDone:
		say("🔄 " + esc(r.Text) + ".\n👋 Exiting bot… launchd will restart it shortly.")
		h.Lifecycle.RequestExit()
	default:
		say("⚠️ " + esc(r.Text) + ".\n\n<code>/restart q</code> — restart automatically once it finishes\n<code>/restart f</code> — interrupt it and restart now")
	}
}
