package bot

import (
	"context"
	"sync"

	"github.com/tallesborges/zdx/apps/gateway/internal/render"
	"github.com/tallesborges/zdx/apps/gateway/internal/telegram"
)

// Followups holds the suggestions shown on each buttons message, keyed by
// chat and message id, until one is tapped or dismissed. Ignored entries
// stay until restart, as in the Rust bot; a restart makes old buttons
// answer "no longer active".
type Followups struct {
	mu sync.Mutex
	m  map[[2]int64][]string
}

// Register records the suggestions shown on a message. A nil registry
// ignores it.
func (f *Followups) Register(chat, messageID int64, items []string) {
	if f == nil || len(items) == 0 {
		return
	}
	f.mu.Lock()
	defer f.mu.Unlock()
	if f.m == nil {
		f.m = map[[2]int64][]string{}
	}
	f.m[[2]int64{chat, messageID}] = append([]string(nil), items...)
}

// take removes a message's suggestions and returns the one at index.
func (f *Followups) take(chat, messageID int64, index int) (string, bool) {
	if f == nil {
		return "", false
	}
	f.mu.Lock()
	defer f.mu.Unlock()
	key := [2]int64{chat, messageID}
	items, ok := f.m[key]
	delete(f.m, key)
	if !ok || index < 0 || index >= len(items) {
		return "", false
	}
	return items[index], true
}

func (f *Followups) remove(chat, messageID int64) {
	if f == nil {
		return
	}
	f.mu.Lock()
	defer f.mu.Unlock()
	delete(f.m, [2]int64{chat, messageID})
}

// handleFollowup answers a follow-up button (crates/zdx-bot/src/followups.rs).
// Taps change only the keyboard, never the text, and keep the message's
// link rows. A tap sends the suggestion as the user's next message.
func (h *Handler) handleFollowup(ctx context.Context, q *telegram.CallbackQuery, tap render.FollowupTap) {
	answer := func(text string) {
		var t *string
		if text != "" {
			t = &text
		}
		if err := h.Telegram.AnswerCallbackQuery(ctx, q.ID, t); err != nil {
			h.log().Warn("failed to answer follow-up callback", "err", err)
		}
	}
	m := q.Message
	if m == nil {
		answer("No message context")
		return
	}
	kept := render.LinkRows(m.ReplyMarkup)
	registry := h.Presenter.Deliverer.Followups
	if tap.Dismiss {
		registry.remove(m.Chat.ID, m.ID)
		rows := kept
		if rows == nil {
			rows = [][]telegram.InlineKeyboardButton{}
		}
		if err := h.Telegram.EditMessageReplyMarkup(ctx, m.Chat.ID, m.ID, &telegram.InlineKeyboardMarkup{InlineKeyboard: rows}); err != nil {
			h.log().Warn("failed to dismiss follow-ups", "err", err)
		}
		answer("")
		return
	}
	if tap.Index < 0 {
		answer("")
		return
	}
	item, ok := registry.take(m.Chat.ID, m.ID, tap.Index)
	if !ok {
		answer("These suggestions are no longer active")
		return
	}
	if err := h.Telegram.EditMessageReplyMarkup(ctx, m.Chat.ID, m.ID, render.ChosenFollowupKeyboard(kept, item)); err != nil {
		h.log().Warn("failed to mark the chosen follow-up", "err", err)
	}
	answer("")
	text := item
	synthetic := &telegram.Message{
		ID:       m.ID,
		Chat:     m.Chat,
		From:     &telegram.User{ID: q.From.ID},
		Text:     &text,
		ThreadID: m.EffectiveThreadID(),
	}
	h.Dispatch(ctx, Incoming{Message: synthetic})
}
