package bot

import (
	"fmt"
	"strings"
	"time"

	"github.com/tallesborges/zdx/apps/gateway/internal/engine"
)

// Status texts of a turn (crates/zdx-bot/src/agent/mod.rs).
const (
	StatusWaiting   = "⏳ Waiting for model..."
	StatusThinking  = "🧠 Thinking..."
	StatusWriting   = "✍️ Writing reply..."
	StatusCancelled = "Cancelled ✓"
)

// StatusConnectionLost replaces the status of a replayable turn whose
// connection went away: the reply comes through the renderer subscription
// if the turn finishes (the Rust bot has no replay, so no Rust text).
const StatusConnectionLost = "Lost the connection to the orchestrator service. If this turn finishes, its reply will be posted here."

// StatusDebounce is the minimum interval between status edits.
const StatusDebounce = 3 * time.Second

// EventStatus is the progress line an agent event moves the status to, or
// false when the event does not change it.
func EventStatus(ev engine.AgentEvent) (string, bool) {
	switch ev.Type {
	case engine.AgentTurnStarted, "reasoning_completed", engine.AgentToolCompleted:
		return StatusWaiting, true
	case engine.AgentReasoningDelta:
		return StatusThinking, true
	case engine.AgentAssistantDelta, engine.AgentAssistantCompleted:
		return StatusWriting, true
	case engine.AgentToolRequested:
		return fmt.Sprintf("⚙️ Preparing `%s`...", ev.Name), true
	case engine.AgentToolStarted:
		return ToolRunningStatus(ev.Name), true
	}
	return "", false
}

// ToolRunningStatus is the one-line "running tool" status.
func ToolRunningStatus(name string) string {
	emoji := "⚙️"
	switch name {
	case "bash":
		emoji = "🔧"
	case "read":
		emoji = "📖"
	case "write", "edit", "apply_patch":
		emoji = "✏️"
	case "web_search":
		emoji = "🔍"
	case "fetch_webpage":
		emoji = "🌐"
	case "read_thread":
		emoji = "💬"
	}
	return fmt.Sprintf("%s Running `%s`...", emoji, name)
}

// Debounce keeps the status message's progress line current with at most
// one edit per StatusDebounce. A change inside the interval is kept and sent
// when the interval ends, even with no further event; only the newest status
// is sent.
type Debounce struct {
	shown, desired string
	lastEdit       time.Time
	interval       time.Duration
}

// NewDebounce starts from the status the message shows now; the first change
// may be sent at once.
func NewDebounce(shown string, now time.Time) *Debounce {
	return &Debounce{shown: shown, desired: shown, lastEdit: now.Add(-StatusDebounce), interval: StatusDebounce}
}

// Observe records the newest status.
func (d *Debounce) Observe(status string) { d.desired = status }

// NextFlush is when the pending status may be sent; false when none is.
func (d *Debounce) NextFlush() (time.Time, bool) {
	if d.desired == d.shown {
		return time.Time{}, false
	}
	return d.lastEdit.Add(d.interval), true
}

// Due returns the status to send now, if one is pending and due.
func (d *Debounce) Due(now time.Time) (string, bool) {
	at, ok := d.NextFlush()
	if !ok || at.After(now) {
		return "", false
	}
	return d.desired, true
}

// Record notes an edit attempt. A flood-wait outcome keeps the status
// pending for one more interval; anything else (sent, refused, unknown) is
// not repeated.
func (d *Debounce) Record(text string, now time.Time, floodWait bool) {
	d.lastEdit = now
	if !floodWait {
		d.shown = text
	}
}

// FormatUserError is the visible text of a failed turn.
func FormatUserError(message string) string {
	trimmed := strings.TrimSpace(message)
	if len(trimmed) > 700 {
		r := []rune(trimmed)
		if len(r) > 700 {
			r = r[:700]
		}
		trimmed = string(r) + "…"
	}
	return "❌ Request failed.\n\n<pre>" + EscapeHTML(trimmed) + "</pre>"
}

// EscapeHTML escapes text for Telegram HTML.
func EscapeHTML(s string) string {
	return strings.NewReplacer("&", "&amp;", "<", "&lt;", ">", "&gt;").Replace(s)
}
