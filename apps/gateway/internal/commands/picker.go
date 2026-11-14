package commands

import (
	"context"
	"sort"
	"strconv"
	"strings"
	"unicode/utf8"

	"github.com/tallesborges/zdx/apps/gateway/internal/telegram"
)

// pickerKey is a /commands picker message.
type pickerKey struct{ chat, message int64 }

// maxPickerChars bounds the picker body (characters).
const maxPickerChars = 3500

func sourceRank(source string) int {
	switch source {
	case "project":
		return 0
	case "user":
		return 1
	}
	return 2
}

// PickerBody is the /commands message: project commands first, then user,
// then bundled ones.
func PickerBody(commands []CustomCommand) string {
	lines := []string{"🧰 <b>Project commands</b>"}
	for _, c := range commands {
		description := ""
		if c.Description != "" {
			description = " — " + esc(c.Description)
		}
		lines = append(lines, "• /"+esc(c.Name)+description+" <i>("+c.Source+")</i>")
	}
	return truncateChars(strings.Join(lines, "\n"), maxPickerChars)
}

// PickerKeyboard is two commands per row (cmd:<index>) and Dismiss (cmd:x).
func PickerKeyboard(commands []CustomCommand) *telegram.InlineKeyboardMarkup {
	var rows [][]telegram.InlineKeyboardButton
	for i := 0; i < len(commands); i += 2 {
		var row []telegram.InlineKeyboardButton
		for j := i; j < i+2 && j < len(commands); j++ {
			row = append(row, telegram.CallbackButton("/"+commands[j].Name, "cmd:"+strconv.Itoa(j)))
		}
		rows = append(rows, row)
	}
	rows = append(rows, []telegram.InlineKeyboardButton{telegram.CallbackButton("✕ Dismiss", "cmd:x")})
	return &telegram.InlineKeyboardMarkup{InlineKeyboard: rows}
}

func truncateChars(s string, max int) string {
	if utf8.RuneCountInString(s) <= max {
		return s
	}
	return string([]rune(s)[:max-1]) + "…"
}

func (h *Handler) commandPicker(ctx context.Context, m Message, t Thread) {
	listed, err := h.Commands.CustomCommands(ctx, m.Chat, t.ID, NativeNames())
	if err != nil {
		h.send(ctx, telegram.TextSend{ChatID: m.Chat, Text: "❌ " + esc(err.Error()), MessageThreadID: m.Topic, ReplyToMessageID: m.ReplyTo})
		return
	}
	sorted := append([]CustomCommand(nil), listed...)
	sort.SliceStable(sorted, func(i, j int) bool { return sourceRank(sorted[i].Source) < sourceRank(sorted[j].Source) })
	msg := h.send(ctx, telegram.TextSend{
		ChatID: m.Chat, Text: PickerBody(sorted), MessageThreadID: m.Topic, ReplyToMessageID: m.ReplyTo, ReplyMarkup: PickerKeyboard(sorted),
	})
	if msg == nil {
		return
	}
	h.mu.Lock()
	if h.pickers == nil {
		h.pickers = map[pickerKey][]CustomCommand{}
	}
	h.pickers[pickerKey{m.Chat, msg.ID}] = sorted
	h.mu.Unlock()
}

// pickerCallback handles cmd:<index> and cmd:x. A picker is one-shot: the
// first tap or Dismiss consumes it.
func (h *Handler) pickerCallback(ctx context.Context, q *telegram.CallbackQuery, rest string) {
	msg := q.Message
	if msg == nil {
		h.answer(ctx, q, "No message context")
		return
	}
	key := pickerKey{msg.Chat.ID, msg.ID}
	if rest == "x" {
		h.mu.Lock()
		delete(h.pickers, key)
		h.mu.Unlock()
		if err := h.delete(ctx, msg.Chat.ID, msg.ID); err != nil {
			h.log().Warn("failed to delete a command picker", "err", err)
		}
		h.answer(ctx, q, "")
		return
	}
	var entry *CustomCommand
	if idx, err := strconv.ParseUint(rest, 10, 64); err == nil {
		h.mu.Lock()
		entries, ok := h.pickers[key]
		delete(h.pickers, key)
		h.mu.Unlock()
		if ok && idx < uint64(len(entries)) {
			entry = &entries[idx]
		}
	}
	if entry == nil {
		h.answer(ctx, q, "This picker is no longer active")
		return
	}
	h.edit(ctx, msg.Chat.ID, msg.ID, "▶️ /"+esc(entry.Name), nil)
	h.answer(ctx, q, "")
	h.Dispatch.Dispatch(ctx, Synthetic{
		Chat: msg.Chat.ID, ChatType: syntheticChatType(msg.Chat.Type), IsForum: msg.Chat.IsForumEnabled(),
		Topic: msg.EffectiveThreadID(), MessageID: msg.ID, UserID: q.From.ID, Text: entry.Content,
	})
}
