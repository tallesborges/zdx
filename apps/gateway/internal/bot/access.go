// Package bot is the gateway's Telegram lifecycle: update polling with a
// memory-only offset, allowlist gates, per-topic intake ordering, and the
// renderer bridge that follows routed turns of the Rust orchestrator service
// and releases their holds. Engine work (queues, execution, persistence)
// stays in Rust; this package only moves updates in and renders turns out.
package bot

import (
	"log/slog"

	"github.com/tallesborges/zdx/apps/gateway/internal/telegram"
)

// Allowlist is the resolved Telegram access list: allowed user ids, and
// allowed group chat ids (profile chats included, as Rust resolves them).
type Allowlist struct {
	Users map[int64]bool
	Chats map[int64]bool
}

// NewAllowlist builds an allowlist from id lists.
func NewAllowlist(users, chats []int64) Allowlist {
	a := Allowlist{Users: map[int64]bool{}, Chats: map[int64]bool{}}
	for _, id := range users {
		a.Users[id] = true
	}
	for _, id := range chats {
		a.Chats[id] = true
	}
	return a
}

// IsWhereAmI reports a /whereami command (also /whereami@bot).
func IsWhereAmI(text *string) bool {
	return text != nil && telegram.CommandMatches(*text, "/whereami")
}

// MessageAllowed is the message gate: a human sender on the user allowlist,
// in a private chat or an allowlisted group. The one exception is /whereami
// from an allowlisted user in any group, so the operator can learn a new
// group's id before binding it. Other chat types are ignored.
func (a Allowlist) MessageAllowed(m *telegram.Message, log *slog.Logger) bool {
	if m.From == nil {
		log.Debug("ignoring message without sender", "chat_id", m.Chat.ID)
		return false
	}
	if m.From.IsBot {
		return false
	}
	if !a.Users[m.From.ID] {
		log.Warn("denied user", "user_id", m.From.ID, "chat_id", m.Chat.ID)
		return false
	}
	switch {
	case m.Chat.IsGroup():
		if a.Chats[m.Chat.ID] {
			return true
		}
		if IsWhereAmI(m.Text) {
			return true
		}
		log.Debug("ignoring non-allowlisted group chat", "chat_id", m.Chat.ID)
		return false
	case m.Chat.IsPrivate():
		return true
	default:
		log.Debug("ignoring unsupported chat type", "chat_id", m.Chat.ID)
		return false
	}
}

// CallbackAllowed is the button gate: an allowlisted user pressing a button
// on a bot message in a private chat or an allowlisted group.
func (a Allowlist) CallbackAllowed(q *telegram.CallbackQuery) bool {
	if !a.Users[q.From.ID] || q.Message == nil {
		return false
	}
	chat := q.Message.Chat
	return chat.IsPrivate() || (chat.IsGroup() && a.Chats[chat.ID])
}

// FeedbackAllowed is the Mini App feedback gate (route_allowed in
// artifact_feedback.rs): an allowlisted user, into an allowlisted group
// chat, or into their own private chat.
func (a Allowlist) FeedbackAllowed(userID, chat int64) bool {
	if !a.Users[userID] {
		return false
	}
	if chat < 0 {
		return a.Chats[chat]
	}
	return chat == userID
}
