package bot

import (
	"context"
	"fmt"
	"log/slog"
	"strconv"
	"strings"
	"sync"
	"sync/atomic"
	"time"

	"github.com/tallesborges/zdx/apps/gateway/internal/engine"
	"github.com/tallesborges/zdx/apps/gateway/internal/render"
	"github.com/tallesborges/zdx/apps/gateway/internal/telegram"
)

// CancelKey names a turn's status message Cancel button: the chat and the
// user message the turn answers (a synthetic negative id for turns the
// service started on its own).
type CancelKey struct {
	Chat int64
	ID   int64
}

// CallbackData is the button payload, `cancel:{chat}:{id}` as in Rust.
func (k CancelKey) CallbackData() string {
	return "cancel:" + strconv.FormatInt(k.Chat, 10) + ":" + strconv.FormatInt(k.ID, 10)
}

// ParseCancelCallback parses `cancel:{chat}:{id}`.
func ParseCancelCallback(data string) (CancelKey, bool) {
	rest, ok := strings.CutPrefix(data, "cancel:")
	if !ok || strings.HasPrefix(rest, "q") {
		return CancelKey{}, false
	}
	chat, id, ok := strings.Cut(rest, ":")
	if !ok {
		return CancelKey{}, false
	}
	c, err1 := strconv.ParseInt(chat, 10, 64)
	i, err2 := strconv.ParseInt(id, 10, 64)
	if err1 != nil || err2 != nil {
		return CancelKey{}, false
	}
	return CancelKey{Chat: c, ID: i}, true
}

type cancelEntry struct {
	turn      *engine.Turn
	requested atomic.Bool
}

// Cancels maps live Cancel buttons to their turns. Cancelling asks the
// Rust service to stop the work (the running turn, or the input while it
// still waits in the service queue).
type Cancels struct {
	mu sync.Mutex
	m  map[CancelKey]*cancelEntry
}

func (c *Cancels) register(key CancelKey, t *engine.Turn) *cancelEntry {
	c.mu.Lock()
	defer c.mu.Unlock()
	if c.m == nil {
		c.m = map[CancelKey]*cancelEntry{}
	}
	e := &cancelEntry{turn: t}
	c.m[key] = e
	return e
}

func (c *Cancels) remove(key CancelKey, e *cancelEntry) {
	c.mu.Lock()
	defer c.mu.Unlock()
	if c.m[key] == e {
		delete(c.m, key)
	}
}

// Cancel stops the turn behind key; false when no turn is live there.
func (c *Cancels) Cancel(key CancelKey) bool {
	c.mu.Lock()
	e := c.m[key]
	c.mu.Unlock()
	if e == nil {
		return false
	}
	e.requested.Store(true)
	e.turn.Cancel()
	return true
}

var syntheticIDs atomic.Int64

// nextSyntheticID is a negative id for turns no user message started.
func nextSyntheticID() int64 { return -syntheticIDs.Add(1) }

// Origin is the user message a followed turn answers.
type Origin struct {
	MessageID int64
	// ReplyTo is the message the status and reply quote; nil quotes nothing.
	ReplyTo *int64
	// Quote replaces ReplyTo for the reply of a General message moved into
	// its own topic: it quotes the original in General.
	Quote *telegram.ReplyParameters
}

// origin is the Origin of a turn intake submits for in.
func origin(in Incoming, reply *int64) *Origin {
	o := &Origin{MessageID: in.Message.ID, ReplyTo: reply}
	if in.FromGeneral {
		chat, yes := in.Message.Chat.ID, true
		o.Quote = &telegram.ReplyParameters{MessageID: in.Message.ID, ChatID: &chat, AllowSendingWithoutReply: &yes}
	}
	return o
}

// Presenter renders one routed turn in Telegram like the Rust bot: typing,
// a status message with Cancel, a debounced progress line, then a fresh
// reply message (or the error, or "Cancelled ✓").
type Presenter struct {
	Telegram *telegram.Client
	// Engine and Retries enable "Try again" after a failed ordinary turn.
	Engine     *engine.Client
	Retries    *Retries
	Planner    ReplyPlanner
	Deliverer  *Deliverer
	Cancels    *Cancels
	MiniAppURL string
	Log        *slog.Logger
	// Interval is the status edit interval (default StatusDebounce).
	Interval time.Duration
}

var _ TurnRenderer = (*Presenter)(nil)

func (p *Presenter) log() *slog.Logger {
	if p.Log != nil {
		return p.Log
	}
	return slog.Default()
}

func (p *Presenter) statusMarkup(key CancelKey, threadID string) *telegram.InlineKeyboardMarkup {
	row := []telegram.InlineKeyboardButton{telegram.CallbackButton("⏹ Cancel", key.CallbackData())}
	if p.MiniAppURL != "" && threadID != "" {
		row = append(row, telegram.URLButton("💬 Open Thread", p.MiniAppURL+"?startapp="+threadID))
	}
	return &telegram.InlineKeyboardMarkup{InlineKeyboard: [][]telegram.InlineKeyboardButton{row}}
}

// sendStatus posts the status message, paced so that during a flood wait
// the turn shows its Cancel button late rather than never. Only a refused
// reply target is dropped and the send repeated.
func (p *Presenter) sendStatus(ctx context.Context, target ReplyTarget, markup *telegram.InlineKeyboardMarkup) *int64 {
	c := p.Telegram.Paced()
	req := telegram.TextSend{ChatID: target.Chat, Text: StatusWaiting, MessageThreadID: target.Topic, ReplyToMessageID: target.ReplyTo, ReplyMarkup: markup}
	msg, err := c.SendText(ctx, req)
	if err != nil && req.ReplyToMessageID != nil && telegram.IsInvalidReplyError(err) {
		req.ReplyToMessageID = nil
		msg, err = c.SendText(ctx, req)
	}
	if err != nil {
		p.log().Warn("failed to post turn status", "chat_id", target.Chat, "err", err)
		return nil
	}
	return &msg.ID
}

// RenderTurn implements TurnRenderer.
func (p *Presenter) RenderTurn(ctx context.Context, t RoutedTurn) {
	target := ReplyTarget{Chat: t.Route.Chat, Topic: t.Route.Topic}
	key := CancelKey{Chat: t.Route.Chat, ID: nextSyntheticID()}
	if t.Origin != nil {
		target.ReplyTo = t.Origin.ReplyTo
		target.Quote = t.Origin.Quote
		key.ID = t.Origin.MessageID
	}
	if t.Replay {
		p.renderReplay(ctx, t, target)
		return
	}
	stopTyping := p.Telegram.Typing(target.Chat, target.Topic)
	defer stopTyping()
	markup := p.statusMarkup(key, t.ThreadID)
	entry := p.Cancels.register(key, t.Turn)
	statusID := p.sendStatus(ctx, target, markup)

	interval := p.Interval
	if interval == 0 {
		interval = StatusDebounce
	}
	debounce := NewDebounce(StatusWaiting, time.Now())
	debounce.interval = interval
	flush := func(now time.Time) {
		text, ok := debounce.Due(now)
		if !ok {
			return
		}
		if statusID == nil {
			debounce.Record(text, now, false)
			return
		}
		err := p.Telegram.EditMessageText(ctx, target.Chat, *statusID, text, markup)
		class, _ := telegram.Classify(err)
		debounce.Record(text, now, err != nil && class == telegram.ClassFloodWait)
	}

	var final *engine.AgentEvent
	var timer <-chan time.Time
	events := t.Turn.Events
loop:
	for {
		if at, ok := debounce.NextFlush(); ok && final == nil && !entry.requested.Load() {
			timer = time.After(max(time.Until(at), 0))
		} else {
			timer = nil
		}
		select {
		case ev, ok := <-events:
			if !ok {
				break loop
			}
			if final != nil {
				continue
			}
			if ev.Type == engine.AgentTurnFinished {
				// Read even after a cancel request: the turn may have
				// finished before the cancel reached the service.
				f := ev
				final = &f
				continue
			}
			if entry.requested.Load() {
				continue
			}
			switch ev.Type {
			case engine.AgentError:
				p.log().Error("agent error event", "thread", t.ThreadID, "message", ev.Message)
			case engine.AgentNotice:
				p.log().Info("agent notice event", "thread", t.ThreadID, "kind", ev.Kind, "message", ev.Message)
			}
			if s, ok := EventStatus(ev); ok {
				debounce.Observe(s)
			}
			flush(time.Now())
		case now := <-timer:
			flush(now)
		}
	}
	p.Cancels.remove(key, entry)

	// A cancel request is not a cancellation: only a turn that ended
	// interrupted (or never ran) shows Cancelled. One that completed or
	// failed first shows that outcome.
	if entry.requested.Load() && (final == nil || final.Status == nil || final.Status.Status == "interrupted") {
		if statusID != nil {
			_ = p.Telegram.EditMessageText(ctx, target.Chat, *statusID, StatusCancelled, telegram.EmptyKeyboard())
		}
		return
	}
	if final == nil || final.Status == nil {
		// PumpTurn always ends a turn with turn_finished; keep the status
		// truthful if that ever breaks.
		final = &engine.AgentEvent{Type: engine.AgentTurnFinished, Status: &engine.TurnStatus{Status: "failed", Message: "The turn ended without a result."}}
	}
	if final.Status.Status == "failed" && final.Status.Kind == engine.KindConnectionLost && t.Replayable {
		// Not a failure yet: the service replays the outcome to the next
		// renderer subscription. No error and no Try again, which could run
		// the turn twice.
		if statusID != nil {
			_ = p.Telegram.EditMessageText(ctx, target.Chat, *statusID, StatusConnectionLost, telegram.EmptyKeyboard())
		}
		return
	}
	if final.Status.Status == "failed" {
		if statusID != nil {
			text := "Sorry, something went wrong."
			if final.Status.Message != "" {
				text = FormatUserError(final.Status.Message)
			}
			_ = p.Telegram.EditMessageText(ctx, target.Chat, *statusID, text, nil)
		}
		if t.Retryable && t.Origin != nil {
			p.offerRetry(ctx, RetryRequest{ThreadID: t.ThreadID, Chat: target.Chat, Topic: target.Topic, Origin: *t.Origin})
		}
		return
	}
	// The events are closed, so the turn has ended.
	end := t.Turn.Wait()
	plan, err := p.Planner.PlanReply(ctx, final.FinalText, target, t.ThreadID, end.TouchedWorkers)
	if err != nil {
		p.log().Error("failed to plan the reply", "thread", t.ThreadID, "err", err)
		if statusID != nil {
			_ = p.Telegram.EditMessageText(ctx, target.Chat, *statusID, FormatUserError(fmt.Sprintf("The reply could not be prepared: %v", err)), telegram.EmptyKeyboard())
		}
		return
	}
	// With hold tokens every record names the held turn and its token.
	var hold *engine.Hold
	if end.Held && t.Holder != "" {
		hold = &engine.Hold{TurnID: end.TurnID, Holder: t.Holder}
	}
	if err := p.Deliverer.Deliver(ctx, t.ThreadID, hold, target, statusID, plan); err != nil {
		p.log().Error("reply not delivered", "thread", t.ThreadID, "err", err)
		if statusID != nil {
			_ = p.Telegram.EditMessageText(ctx, target.Chat, *statusID, FormatUserError(err.Error()), telegram.EmptyKeyboard())
		}
	}
}

// renderReplay posts a replayed turn's outcome only: no status message
// (the service cannot name the original one), no Cancel, no retry. A
// finished reply is delivered with the usual records; a failed turn posts
// its error; an interrupted turn posts what text it has.
func (p *Presenter) renderReplay(ctx context.Context, t RoutedTurn, target ReplyTarget) {
	var final *engine.AgentEvent
	for ev := range t.Turn.Events {
		if ev.Type == engine.AgentTurnFinished {
			f := ev
			final = &f
		}
	}
	if final == nil || final.Status == nil {
		p.log().Error("replayed turn without a result", "thread", t.ThreadID, "turn_id", t.TurnID)
		return
	}
	var hold *engine.Hold
	if end := t.Turn.Wait(); end.Held && t.Holder != "" {
		hold = &engine.Hold{TurnID: end.TurnID, Holder: t.Holder}
	}
	var plan render.Plan
	if final.Status.Status == "failed" {
		// The error goes out as a recorded text, like a reply: a held
		// turn sends nothing before an acknowledged pending record.
		text := "Sorry, something went wrong."
		if final.Status.Message != "" {
			text = FormatUserError(final.Status.Message)
		}
		plan = render.PlanReply(render.Reply{HTML: text}, render.Options{})
	} else {
		var err error
		if plan, err = p.Planner.PlanReply(ctx, final.FinalText, target, t.ThreadID, nil); err != nil {
			p.log().Error("failed to plan a replayed reply; it is lost", "thread", t.ThreadID, "err", err)
			return
		}
	}
	if err := p.Deliverer.Deliver(ctx, t.ThreadID, hold, target, nil, plan); err != nil {
		p.log().Error("replayed reply not delivered", "thread", t.ThreadID, "err", err)
	}
}
