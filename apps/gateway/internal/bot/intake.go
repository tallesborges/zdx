package bot

import (
	"context"
	"log/slog"
	"sync"
	"sync/atomic"
	"time"

	"github.com/tallesborges/zdx/apps/gateway/internal/telegram"
)

// MediaGroupDebounce is how long an album is held after its first member.
const MediaGroupDebounce = 1500 * time.Millisecond

// How long and how many flushed album keys are remembered, only to log late
// members.
const (
	flushMemory    = 2 * time.Minute
	flushMemoryCap = 64
)

// Incoming is one message for intake: a single message, or an album head
// with the other members in Grouped (arrival order).
type Incoming struct {
	Message *telegram.Message
	Grouped []*telegram.Message
	Receipt Receipt
	// Synthetic marks a bot-made message (not typed by the user). It never
	// starts or answers a staged command, and never steers.
	Synthetic bool
	// ForceQueue: `/queue <text>` was typed; the text runs as a queued
	// turn, never steered into the running one.
	ForceQueue bool
	// FromGeneral: a General message moved into the topic opened for it.
	// Its answers quote the original across topics; its text titles the
	// topic.
	FromGeneral bool
}

// MediaGroupKey identifies an album: chat, topic, sender, media group id.
type MediaGroupKey struct {
	Chat   int64
	Topic  int64
	Sender int64
	Group  string
}

func mediaGroupKey(m *telegram.Message) (MediaGroupKey, bool) {
	if m.From == nil || m.MediaGroupID == nil {
		return MediaGroupKey{}, false
	}
	var topic int64
	if t := m.EffectiveThreadID(); t != nil {
		topic = *t
	}
	return MediaGroupKey{Chat: m.Chat.ID, Topic: topic, Sender: m.From.ID, Group: *m.MediaGroupID}, true
}

type pendingGroup struct {
	in       Incoming
	openedAt time.Time
	lastAt   time.Time
}

// MediaGroups holds album members until the debounce ends. A member that
// arrives after its group flushed opens a new group.
type MediaGroups struct {
	mu      sync.Mutex
	pending map[MediaGroupKey]*pendingGroup
	flushed map[MediaGroupKey]time.Time
}

// OfferResult is what happened to an offered message.
type OfferResult int

const (
	// Ungrouped: not in an album; dispatch now.
	Ungrouped OfferResult = iota
	// Opened: first member; schedule Flush for the key.
	Opened
	// Joined: added to a held group.
	Joined
)

// Offer adds a message to the buffer. LateAfterFlush is set when an Opened
// group's key flushed recently (a late member).
func (g *MediaGroups) Offer(in Incoming, now time.Time) (res OfferResult, key MediaGroupKey, lateAfterFlush *time.Duration) {
	key, ok := mediaGroupKey(in.Message)
	if !ok {
		return Ungrouped, key, nil
	}
	g.mu.Lock()
	defer g.mu.Unlock()
	if g.pending == nil {
		g.pending = map[MediaGroupKey]*pendingGroup{}
		g.flushed = map[MediaGroupKey]time.Time{}
	}
	if p, ok := g.pending[key]; ok {
		p.in.Grouped = append(p.in.Grouped, in.Message)
		p.lastAt = now
		return Joined, key, nil
	}
	if at, ok := g.flushed[key]; ok {
		if since := now.Sub(at); since < flushMemory {
			lateAfterFlush = &since
		}
	}
	g.pending[key] = &pendingGroup{in: in, openedAt: now, lastAt: now}
	return Opened, key, lateAfterFlush
}

// Flush takes a held group out of the buffer.
func (g *MediaGroups) Flush(key MediaGroupKey, now time.Time) (Incoming, bool) {
	g.mu.Lock()
	defer g.mu.Unlock()
	p, ok := g.pending[key]
	if !ok {
		return Incoming{}, false
	}
	delete(g.pending, key)
	for k, at := range g.flushed {
		if now.Sub(at) >= flushMemory {
			delete(g.flushed, k)
		}
	}
	if _, ok := g.flushed[key]; !ok && len(g.flushed) >= flushMemoryCap {
		var oldest MediaGroupKey
		var oldestAt time.Time
		first := true
		for k, at := range g.flushed {
			if first || at.Before(oldestAt) {
				oldest, oldestAt, first = k, at, false
			}
		}
		delete(g.flushed, oldest)
	}
	g.flushed[key] = now
	return p.in, true
}

// MessageSink receives allowed messages (albums already grouped). It runs
// on the polling loop or a flush timer and must hand work off.
type MessageSink interface {
	Dispatch(ctx context.Context, in Incoming)
}

// CallbackSink receives allowed button presses. It must answer the query
// and hand work off.
type CallbackSink interface {
	HandleCallback(ctx context.Context, q *telegram.CallbackQuery)
}

// CallbackAnswerer answers callback queries.
type CallbackAnswerer interface {
	AnswerCallbackQuery(ctx context.Context, id string, text *string) error
}

// Router is the UpdateHandler of the bot: it applies the allowlist gates,
// groups albums, and passes the rest on.
type Router struct {
	Allow     Allowlist
	Answer    CallbackAnswerer
	Messages  MessageSink
	Callbacks CallbackSink
	Log       *slog.Logger
	// Debounce is the album hold (default MediaGroupDebounce).
	Debounce time.Duration
	Now      func() time.Time

	groups  MediaGroups
	timers  sync.WaitGroup
	pending atomic.Int64
}

// Busy reports albums still held for their debounce.
func (r *Router) Busy() bool { return r.pending.Load() > 0 }

func (r *Router) log() *slog.Logger {
	if r.Log != nil {
		return r.Log
	}
	return slog.Default()
}

func (r *Router) now() time.Time {
	if r.Now != nil {
		return r.Now()
	}
	return time.Now()
}

// HandleMessage implements UpdateHandler.
func (r *Router) HandleMessage(ctx context.Context, m *telegram.Message, rec Receipt) {
	if !r.Allow.MessageAllowed(m, r.log()) {
		return
	}
	in := Incoming{Message: m, Receipt: rec}
	res, key, late := r.groups.Offer(in, r.now())
	switch res {
	case Ungrouped:
		r.Messages.Dispatch(ctx, in)
	case Opened:
		if late != nil {
			r.log().Info("album member arrived after its group flushed", "chat_id", key.Chat, "late_ms", late.Milliseconds())
		}
		debounce := r.Debounce
		if debounce == 0 {
			debounce = MediaGroupDebounce
		}
		r.timers.Add(1)
		r.pending.Add(1)
		time.AfterFunc(debounce, func() {
			defer r.timers.Done()
			defer r.pending.Add(-1)
			if flushed, ok := r.groups.Flush(key, r.now()); ok {
				r.Messages.Dispatch(ctx, flushed)
			}
		})
	}
}

// HandleCallback implements UpdateHandler. Denied presses get an "Access
// denied" toast and go no further.
func (r *Router) HandleCallback(ctx context.Context, q *telegram.CallbackQuery) {
	if !r.Allow.CallbackAllowed(q) {
		var chat any
		if q.Message != nil {
			chat = q.Message.Chat.ID
		}
		r.log().Warn("denied callback outside allowlist", "user_id", q.From.ID, "chat_id", chat)
		text := "Access denied"
		go func() {
			if err := r.Answer.AnswerCallbackQuery(context.WithoutCancel(ctx), q.ID, &text); err != nil {
				r.log().Debug("failed to answer denied callback", "err", err)
			}
		}()
		return
	}
	r.Callbacks.HandleCallback(ctx, q)
}

// WaitFlushes waits for scheduled album flushes (for shutdown and tests).
func (r *Router) WaitFlushes() { r.timers.Wait() }
