package bot

import (
	"context"
	"sync"

	"github.com/tallesborges/zdx/apps/gateway/internal/engine"
	"github.com/tallesborges/zdx/apps/gateway/internal/telegram"
)

// Retry button texts and data (crates/zdx-bot/src/retry.rs).
const (
	RetryPrompt   = "🔁 <i>Retry the failed request?</i>"
	RetryGo       = "retry:go"
	RetryDismiss  = "retry:x"
	retryingText  = "🔄 Retrying…"
	retryingToast = "Retrying…"

	loadingAgainText  = "🔄 Loading it again…"
	loadingAgainToast = "Loading it again…"
)

// downloadRetryText tells the user an attachment could not be downloaded.
func downloadRetryText(label string) string {
	return "⚠️ I couldn't download your " + EscapeHTML(label) + " from Telegram. Telegram still has it, so you don't need to send it again."
}

// offerDownloadRetry answers a message whose attachment download failed
// after every try: no turn ran, and the button loads the same message again.
// Without a retry registry the notice goes out without the button.
func (h *Handler) offerDownloadRetry(ctx context.Context, in Incoming, label string) {
	m := in.Message
	req := telegram.TextSend{ChatID: m.Chat.ID, Text: downloadRetryText(label), MessageThreadID: m.EffectiveThreadID(), ReplyToMessageID: replyTo(m), DisablePreview: true}
	retries := h.Presenter.Retries
	if retries != nil {
		req.ReplyMarkup = retryKeyboard()
	}
	sent, err := h.Telegram.SendText(ctx, req)
	if err != nil {
		h.log().Warn("failed to offer a download retry", "chat_id", m.Chat.ID, "err", err)
		return
	}
	if retries != nil {
		retries.putDownload(m.Chat.ID, sent.ID, in)
	}
}

func retryKeyboard() *telegram.InlineKeyboardMarkup {
	return &telegram.InlineKeyboardMarkup{InlineKeyboard: [][]telegram.InlineKeyboardButton{
		{telegram.CallbackButton("🔄 Try again", RetryGo)},
		{telegram.CallbackButton("✕ Dismiss", RetryDismiss)},
	}}
}

// RetryRequest is a failed ordinary turn that can run again on its
// persisted history.
type RetryRequest struct {
	ThreadID string
	Chat     int64
	Topic    *int64
	Origin   Origin
}

// Retries holds what live "Try again" buttons do, keyed by chat and buttons
// message: rerun a failed turn, or load a message again whose attachment
// download failed. Entries for ignored buttons stay until restart, as in the
// Rust bot.
type Retries struct {
	mu sync.Mutex
	m  map[[2]int64]retryAction
}

// retryAction is exactly one of a turn rerun or a message to load again.
type retryAction struct {
	turn     *RetryRequest
	download *Incoming
}

func (r *Retries) put(chat, messageID int64, req RetryRequest) {
	r.store(chat, messageID, retryAction{turn: &req})
}

func (r *Retries) putDownload(chat, messageID int64, in Incoming) {
	r.store(chat, messageID, retryAction{download: &in})
}

func (r *Retries) store(chat, messageID int64, a retryAction) {
	r.mu.Lock()
	defer r.mu.Unlock()
	if r.m == nil {
		r.m = map[[2]int64]retryAction{}
	}
	r.m[[2]int64{chat, messageID}] = a
}

func (r *Retries) take(chat, messageID int64) (retryAction, bool) {
	r.mu.Lock()
	defer r.mu.Unlock()
	key := [2]int64{chat, messageID}
	a, ok := r.m[key]
	delete(r.m, key)
	return a, ok
}

// offerRetry posts the "Try again" buttons after a failed ordinary turn.
// Only when the service can rerun: a button that cannot work is not shown.
func (p *Presenter) offerRetry(ctx context.Context, req RetryRequest) {
	if p.Retries == nil || p.Engine == nil {
		return
	}
	if err := p.Engine.RequireAll(ctx, engine.CapOrdinarySubmit, engine.CapRerun); err != nil {
		p.log().Info("no retry button: the service cannot rerun", "err", err)
		return
	}
	msg, err := p.Telegram.SendText(ctx, telegram.TextSend{ChatID: req.Chat, Text: RetryPrompt, MessageThreadID: req.Topic, ReplyMarkup: retryKeyboard()})
	if err != nil {
		p.log().Warn("failed to send retry button", "chat_id", req.Chat, "err", err)
		return
	}
	p.Retries.put(req.Chat, msg.ID, req)
}

// handleRetry answers a "Try again" or "Dismiss" tap.
func (h *Handler) handleRetry(ctx context.Context, q *telegram.CallbackQuery, data string) {
	answer := func(text string) {
		var t *string
		if text != "" {
			t = &text
		}
		if err := h.Telegram.AnswerCallbackQuery(ctx, q.ID, t); err != nil {
			h.log().Warn("failed to answer retry callback", "err", err)
		}
	}
	m := q.Message
	if m == nil {
		answer("No message context")
		return
	}
	retries := h.Presenter.Retries
	switch data {
	case RetryDismiss:
		retries.take(m.Chat.ID, m.ID)
		_ = h.Telegram.DeleteMessage(ctx, m.Chat.ID, m.ID)
		answer("")
		return
	case RetryGo:
	default:
		answer("")
		return
	}
	action, ok := retries.take(m.Chat.ID, m.ID)
	if !ok {
		answer("This retry is no longer active")
		return
	}
	if in := action.download; in != nil {
		_ = h.Telegram.EditMessageText(ctx, m.Chat.ID, m.ID, loadingAgainText, nil)
		answer(loadingAgainToast)
		h.Dispatch(ctx, *in)
		return
	}
	req := *action.turn
	// The topic may have become a worker mirror since (Rust's defense in
	// depth): the worker owns that thread.
	info, err := h.Resolver.ResolveTopic(ctx, req.Chat, req.Topic)
	if err == nil && info.Kind == TopicWorkerMirror {
		answer("Not available in a worker topic")
		return
	}
	_ = h.Telegram.EditMessageText(ctx, m.Chat.ID, m.ID, retryingText, nil)
	answer(retryingToast)
	h.Queues.Enqueue(KeyOf(req.Chat, req.Topic), func(ctx context.Context) { h.rerun(ctx, req) })
}

// rerun submits the retry and renders its turn like the original one.
func (h *Handler) rerun(ctx context.Context, req RetryRequest) {
	fail := func(err error) {
		h.log().Warn("retry not submitted", "thread", req.ThreadID, "err", err)
		h.notifyHTML(ctx, req.Chat, req.Topic, req.Origin.ReplyTo, FormatUserError(err.Error()))
	}
	if err := h.TurnReady(ctx); err != nil {
		fail(err)
		return
	}
	threadID := req.ThreadID
	sub, err := h.Turns.SubmitRerun(ctx, engine.Submit{
		ThreadID: &threadID,
		Root:     h.RootForChat(req.Chat),
		Route:    &engine.Route{Chat: req.Chat, Topic: req.Topic},
		Replay:   h.Replay,
	})
	if err != nil {
		fail(err)
		return
	}
	origin := req.Origin
	h.render(ctx, RoutedTurn{
		ThreadID: sub.ThreadID, Kind: engine.TurnKindUser, Route: engine.Route{Chat: req.Chat, Topic: req.Topic},
		Turn: sub.Turn, Origin: &origin, Retryable: true, Holder: sub.Holder,
	})
}
