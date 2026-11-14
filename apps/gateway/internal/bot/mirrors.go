package bot

import (
	"context"
	"encoding/json"
	"errors"
	"log/slog"
	"strings"
	"sync"
	"time"

	"github.com/tallesborges/zdx/apps/gateway/internal/conversation"
	"github.com/tallesborges/zdx/apps/gateway/internal/engine"
	"github.com/tallesborges/zdx/apps/gateway/internal/render"
	"github.com/tallesborges/zdx/apps/gateway/internal/telegram"
)

// Mirror feed constants (crates/zdx-bot/src/orchestrator.rs).
const (
	// MaxMirrorTextChars bounds a prompt or result posted in a mirror topic.
	MaxMirrorTextChars = 3500
	// MirrorEditInterval is the minimum spacing of live status edits.
	MirrorEditInterval = 3 * time.Second
	// mirrorRetry is how long a worker without a known mirror topic (none,
	// or the service lacked worker_mirror_route) is not looked up again.
	mirrorRetry = time.Minute
)

// MirrorFeed is the legacy mirror-topic feed of the Rust bot's orchestrator
// bridge, for workers created before silent workers: orchestrator prompts,
// one live status line per running worker turn (with ⏹ Cancel worker and
// 💬 Open Thread), and the result. The mirror topic of a worker comes from
// the service (WorkerSnapshot.Mirror, capability worker_mirror_route),
// never from a t.me link. Without the capability nothing is posted.
type MirrorFeed struct {
	Telegram *telegram.Client
	// Lookup returns a worker's mirror topic (engine.Client.WorkerMirror).
	Lookup func(ctx context.Context, worker string) (*engine.WorkerMirror, error)
	// MiniAppBase adds 💬 Open Thread and the ↗ Open thread link; "" for none.
	MiniAppBase string
	Log         *slog.Logger
	// Interval is the live edit spacing (default MirrorEditInterval).
	Interval time.Duration

	mu    sync.Mutex
	known map[string]mirrorEntry
	live  map[string]*liveMirrorTurn
}

// mirrorEntry is a looked-up mirror topic. A found one holds until the
// service restarts (Forget); none is asked again after mirrorRetry.
type mirrorEntry struct {
	mirror *engine.WorkerMirror
	until  time.Time
}

// Forget drops what the feed learned from the service. Call it when the
// renderer subscription opens: the service may have restarted onto other
// state.
func (f *MirrorFeed) Forget() {
	f.mu.Lock()
	defer f.mu.Unlock()
	f.known = nil
}

type liveMirrorTurn struct {
	mirror    engine.WorkerMirror
	messageID int64
	status    string
	dirty     bool
	lastEdit  time.Time
	timer     *time.Timer
}

func (f *MirrorFeed) log() *slog.Logger {
	if f.Log != nil {
		return f.Log
	}
	return slog.Default()
}

func (f *MirrorFeed) interval() time.Duration {
	if f.Interval > 0 {
		return f.Interval
	}
	return MirrorEditInterval
}

// mirrorOf returns the worker's mirror topic, nil for none. Call with mu.
func (f *MirrorFeed) mirrorOf(ctx context.Context, worker string) *engine.WorkerMirror {
	if e, ok := f.known[worker]; ok && (e.until.IsZero() || time.Now().Before(e.until)) {
		return e.mirror
	}
	if f.known == nil {
		f.known = map[string]mirrorEntry{}
	}
	m, err := f.Lookup(ctx, worker)
	var unsupported *engine.ErrUnsupported
	switch {
	case errors.As(err, &unsupported):
		f.known[worker] = mirrorEntry{until: time.Now().Add(mirrorRetry)}
		return nil
	case err != nil:
		f.log().Warn("failed to look up a worker's mirror topic", "worker", worker, "err", err)
		return nil
	}
	if m != nil {
		topic := m.Topic
		if conversation.ThreadID(m.Chat, &topic) != m.ThreadID {
			f.log().Error("worker mirror route does not match its thread; ignoring it", "worker", worker, "chat", m.Chat, "topic", m.Topic, "thread", m.ThreadID)
			m = nil
		}
	}
	entry := mirrorEntry{mirror: m}
	if m == nil {
		entry.until = time.Now().Add(mirrorRetry)
	}
	f.known[worker] = entry
	return m
}

func (f *MirrorFeed) keyboard(worker string) *telegram.InlineKeyboardMarkup {
	row := []telegram.InlineKeyboardButton{telegram.CallbackButton("⏹ Cancel worker", "wk:c")}
	if f.MiniAppBase != "" {
		row = append(row, telegram.URLButton("💬 Open Thread", f.MiniAppBase+"?startapp="+worker))
	}
	return &telegram.InlineKeyboardMarkup{InlineKeyboard: [][]telegram.InlineKeyboardButton{row}}
}

// Prompted posts an orchestrator's prompt into the worker's mirror topic.
func (f *MirrorFeed) Prompted(ctx context.Context, worker, prompt string) {
	f.mu.Lock()
	defer f.mu.Unlock()
	m := f.mirrorOf(ctx, worker)
	if m == nil {
		return
	}
	body := render.TruncateHTML(render.ToTelegramHTML(prompt), MaxMirrorTextChars)
	topic := m.Topic
	text := "<b>📤 Prompt from the orchestrator</b>\n\n" + body
	if _, err := f.Telegram.SendText(ctx, telegram.TextSend{ChatID: m.Chat, Text: text, MessageThreadID: &topic}); err != nil {
		f.log().Warn("failed to post mirror prompt", "worker", worker, "err", err)
	}
}

// activityStatus is the live line a worker activity moves to, false when
// it does not change it.
func activityStatus(raw json.RawMessage) (string, bool) {
	var a struct {
		Kind string `json:"kind"`
		Name string `json:"name"`
	}
	if json.Unmarshal(raw, &a) != nil {
		return "", false
	}
	switch a.Kind {
	case "tool_started":
		return ToolRunningStatus(a.Name), true
	case "tool_finished":
		return StatusWaiting, true
	}
	return "", false
}

// Activity feeds the worker's live status line. It reports whether this is
// the first activity of a mirrored worker's turn (the worker went from
// queued to running), which refreshes its owner's header.
func (f *MirrorFeed) Activity(ctx context.Context, worker string, activity json.RawMessage) bool {
	f.mu.Lock()
	defer f.mu.Unlock()
	if turn, ok := f.live[worker]; ok {
		if next, changed := activityStatus(activity); changed && next != turn.status {
			turn.status, turn.dirty = next, true
			if wait := f.interval() - time.Since(turn.lastEdit); wait <= 0 {
				f.flush(ctx, worker, turn)
			} else if turn.timer == nil {
				turn.timer = time.AfterFunc(wait, func() { f.flushLater(worker, turn) })
			}
		}
		return false
	}
	m := f.mirrorOf(ctx, worker)
	if m == nil {
		return false
	}
	turn := &liveMirrorTurn{mirror: *m, status: StatusWaiting, lastEdit: time.Now()}
	if next, changed := activityStatus(activity); changed {
		turn.status = next
	}
	topic := m.Topic
	msg, err := f.Telegram.SendText(ctx, telegram.TextSend{ChatID: m.Chat, Text: turn.status, MessageThreadID: &topic, ReplyMarkup: f.keyboard(worker)})
	if err != nil {
		f.log().Warn("failed to post live status message", "worker", worker, "err", err)
		return true
	}
	turn.messageID = msg.ID
	if f.live == nil {
		f.live = map[string]*liveMirrorTurn{}
	}
	f.live[worker] = turn
	return true
}

func (f *MirrorFeed) flushLater(worker string, turn *liveMirrorTurn) {
	f.mu.Lock()
	defer f.mu.Unlock()
	turn.timer = nil
	if f.live[worker] != turn || !turn.dirty {
		return
	}
	ctx, cancel := context.WithTimeout(context.Background(), WorkerNoticeTimeout)
	defer cancel()
	f.flush(ctx, worker, turn)
}

// flush edits the live line. Call with mu.
func (f *MirrorFeed) flush(ctx context.Context, worker string, turn *liveMirrorTurn) {
	turn.dirty = false
	turn.lastEdit = time.Now()
	if err := f.Telegram.EditMessageText(ctx, turn.mirror.Chat, turn.messageID, turn.status, f.keyboard(worker)); err != nil && !strings.Contains(err.Error(), "message is not modified") {
		f.log().Warn("failed to edit live status message", "message_id", turn.messageID, "err", err)
	}
}

// Completed deletes the live line and posts the worker turn's result in
// its mirror topic.
func (f *MirrorFeed) Completed(ctx context.Context, we engine.WorkerEvent) {
	f.mu.Lock()
	defer f.mu.Unlock()
	worker := we.WorkerThreadID
	if turn, ok := f.live[worker]; ok {
		delete(f.live, worker)
		if turn.timer != nil {
			turn.timer.Stop()
		}
		if err := f.Telegram.DeleteMessage(ctx, turn.mirror.Chat, turn.messageID); err != nil {
			f.log().Warn("failed to delete live status message", "message_id", turn.messageID, "err", err)
		}
	}
	m := f.mirrorOf(ctx, worker)
	if m == nil {
		return
	}
	text := render.AppendThreadLink(f.MiniAppBase, worker, MirrorResultText(we))
	topic := m.Topic
	if _, err := f.Telegram.SendText(ctx, telegram.TextSend{ChatID: m.Chat, Text: text, MessageThreadID: &topic, DisablePreview: true}); err != nil {
		f.log().Warn("failed to post mirror update", "worker", worker, "err", err)
	}
}

// MirrorResultText is a finished worker turn as posted in its mirror topic,
// before the thread link (Rust post_mirror_update).
func MirrorResultText(we engine.WorkerEvent) string {
	switch we.Status {
	case "completed":
		body := ""
		if we.FinalText != nil {
			body = *we.FinalText
		}
		return render.TruncateHTML(render.ToTelegramHTML(body), MaxMirrorTextChars)
	case "cancelled":
		return "🚫 Turn cancelled."
	}
	raw := "unknown error"
	if we.Error != nil {
		raw = *we.Error
	}
	return "❌ Turn " + we.Status + ": " + summarizeWorkerError(raw)
}

// summarizeWorkerError is Rust summarize_worker_error.
func summarizeWorkerError(e string) string {
	stripped := stripANSI(e)
	lower := strings.ToLower(stripped)
	if strings.Contains(lower, "overloaded_error") || strings.Contains(lower, ": overloaded") {
		return "provider overloaded"
	}
	if strings.Contains(lower, "rate_limit") || strings.Contains(lower, "rate limit") {
		return "provider rate limited"
	}
	lines := strings.Split(stripped, "\n")
	for i := len(lines) - 1; i >= 0; i-- {
		trimmed := strings.TrimSpace(lines[i])
		if trimmed == "" {
			continue
		}
		if msg, ok := strings.CutPrefix(trimmed, "execute prompt: "); ok {
			return EscapeHTML(truncateChars(msg, 200))
		}
		if msg, ok := strings.CutPrefix(trimmed, "provider: "); ok {
			return EscapeHTML(truncateChars(msg, 200))
		}
		if msg, ok := strings.CutPrefix(trimmed, "Subagent failed: "); ok &&
			!strings.Contains(msg, "WARN") && !strings.Contains(msg, "INFO") && !strings.Contains(msg, "ERROR") {
			return EscapeHTML(truncateChars(msg, 200))
		}
	}
	return "<pre>" + EscapeHTML(truncateChars(stripped, 500)) + "</pre>"
}

// truncateChars is zdx_engine::orchestration::updates::truncate_chars.
func truncateChars(s string, max int) string {
	r := []rune(s)
	if len(r) <= max {
		return s
	}
	return string(r[:max]) + "… [truncated — use Read_Thread for the rest]"
}

func stripANSI(s string) string {
	var out strings.Builder
	inEscape := false
	for _, c := range s {
		switch {
		case c == '\x1b':
			inEscape = true
		case inEscape:
			if c < 128 && (c >= 'a' && c <= 'z' || c >= 'A' && c <= 'Z') {
				inEscape = false
			}
		default:
			out.WriteRune(c)
		}
	}
	return out.String()
}
