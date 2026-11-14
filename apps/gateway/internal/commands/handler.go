package commands

import (
	"context"
	"errors"
	"log/slog"
	"strings"
	"sync"
	"sync/atomic"
	"time"

	"github.com/tallesborges/zdx/apps/gateway/internal/conversation"
	"github.com/tallesborges/zdx/apps/gateway/internal/engine"
	"github.com/tallesborges/zdx/apps/gateway/internal/telegram"
)

// Handler runs the commands, staged commands, pickers, launcher and thread
// headers of this package. Every dependency but Telegram is optional: a
// command whose dependencies are nil is not handled (HandleMessage returns
// false) and its buttons are not claimed, so the bot answers them as
// unsupported.
//
// Calls block on Telegram and the engine. The bot calls HandleMessage from
// the topic's intake slot and HandleCallback off the polling loop.
type Handler struct {
	Telegram Telegram
	Chats    Chats
	// Resolver: button handlers find the topic's thread.
	Resolver  Resolver
	Dispatch  Dispatcher
	Topics    Topics
	Threads   Threads
	Blocker   TopicBlocker
	Seeds     Seeds
	Generator Generator
	Commands  CommandSource
	Workers   WorkerCanceller
	Worktrees Worktrees
	Models    Models
	Defaults  ChatDefaults
	Launches  Launches
	// Statuses: /status and the pinned thread header. Without it new topics
	// get no header (Rust only warns when posting one fails).
	Statuses Statuses
	// Lifecycle: /restart.
	Lifecycle Lifecycle
	// Now is the clock for staging expiry, topic names and resume ages.
	Now func() time.Time
	// RepostDelay coalesces launcher reposts (default 700ms).
	RepostDelay time.Duration
	Log         *slog.Logger

	mu        sync.Mutex
	sessions  map[string]*session
	pickers   map[pickerKey][]CustomCommand
	launchers map[int64]*launcherState
	headers   map[string]header
	bg        sync.WaitGroup
	busy      atomic.Int64
}

func (h *Handler) now() time.Time {
	if h.Now != nil {
		return h.Now()
	}
	return time.Now()
}

func (h *Handler) log() *slog.Logger {
	if h.Log != nil {
		return h.Log
	}
	return slog.Default()
}

// Wait blocks until the work this handler moved to its own goroutines
// (reposts, slow commands, buttons through Hook) is done.
func (h *Handler) Wait() { h.bg.Wait() }

// Busy reports work this handler runs on its own goroutines: a slow
// command (/tldr, /restart), any button through Hook (staging, pickers,
// launcher, header refresh, worker cancel), or a launcher repost, from the
// call that starts it until it returns. A task that dispatches a message
// returns only after the bot queued it. Work on the caller's goroutine (a
// command run from the topic queue) is the bot's to count. Busy and Wait
// cover the same goroutines.
func (h *Handler) Busy() bool { return h.busy.Load() > 0 }

// goOwned runs f on its own goroutine. Both counts go up before the
// goroutine starts, so Busy is never false between the call and f.
func (h *Handler) goOwned(f func()) {
	h.busy.Add(1)
	h.bg.Add(1)
	go func() {
		defer h.bg.Done()
		defer h.busy.Add(-1)
		f()
	}()
}

// HandleMessage handles m when it is a command this package owns, or the
// input of a live staged command on its thread. true means m is consumed
// and must not run a turn.
//
// Call it after worker-mirror routing (mirror topics never reach it) and
// after the bot's own /new and /model set|reset, before submitting a turn.
// t is the thread of m's conversation.
func (h *Handler) HandleMessage(ctx context.Context, m Message, t Thread) bool {
	if t.Kind == KindWorkerMirror {
		return false
	}
	if !m.HasMedia {
		if c, ok := Parse(m.Text); ok && h.command(ctx, m, t, c) {
			return true
		}
		if mc, ok := ParseModel(m.Text); ok && h.modelCommand(ctx, m, t, mc) {
			return true
		}
	}
	return h.staging(ctx, m, t)
}

func (h *Handler) reply(ctx context.Context, m Message, html string, markup *telegram.InlineKeyboardMarkup) *telegram.Message {
	return h.send(ctx, telegram.TextSend{ChatID: m.Chat, Text: html, MessageThreadID: m.Topic, ReplyToMessageID: m.ReplyTo, ReplyMarkup: markup})
}

// command answers the immediate commands. false leaves m to staging or to
// the bot.
func (h *Handler) command(ctx context.Context, m Message, t Thread, c Command) bool {
	if m.InGeneral() {
		if text, ok := GeneralRefusal(c); ok {
			h.reply(ctx, m, text, nil)
			return true
		}
	}
	switch c {
	case CmdThreadID:
		h.reply(ctx, m, ThreadIDText(t.ID), nil)
	case CmdThreads:
		if h.Chats == nil {
			return false
		}
		url := h.Chats.MiniAppURL(m.Chat)
		if url == "" {
			h.reply(ctx, m, ThreadsNotConfigured, nil)
			return true
		}
		text, kb := ThreadsReply(url, t.ID)
		h.reply(ctx, m, text, kb)
	case CmdWhereAmI:
		if h.Chats == nil {
			return false
		}
		// Private chats have chat id == user id and are trusted; a group
		// off the allowlist sees ids only.
		discovery := m.Chat != m.UserID && !h.Chats.ChatAllowed(m.Chat)
		h.reply(ctx, m, WhereAmIText(m.Chat, m.Topic, h.Chats.Binding(m.Chat), discovery), nil)
	case CmdStatus:
		if h.Statuses == nil {
			return false
		}
		st, err := h.status(ctx, m.Chat, t.ID)
		if err != nil {
			h.reply(ctx, m, "❌ "+esc(err.Error()), nil)
			return true
		}
		h.reply(ctx, m, StatusText(st, "<b>Status</b>", h.miniApp(m.Chat)), nil)
	case CmdTldr:
		if h.Generator == nil {
			return false
		}
		h.tldr(ctx, m, t)
	case CmdCommands:
		if h.Commands == nil || h.Dispatch == nil {
			return false
		}
		h.commandPicker(ctx, m, t)
	case CmdWorktree:
		if h.Worktrees == nil || h.Threads == nil || h.Chats == nil {
			return false
		}
		h.worktree(ctx, m, t)
	case CmdLauncher:
		if !m.InGeneral() {
			h.reply(ctx, m, LauncherOutsideGeneral, nil)
			return true
		}
		if !h.launcherReady() {
			return false
		}
		if err := h.PostLauncher(ctx, m.Chat, m.ReplyTo); err != nil {
			h.log().Warn("failed to post the launcher", "chat_id", m.Chat, "err", err)
			h.reply(ctx, m, "⚠️ I couldn't post the launcher. Please try again.", nil)
		}
	case CmdRestart:
		if h.Lifecycle == nil {
			return false
		}
		h.restart(ctx, m)
	case CmdNew:
		// Only /new in General is this package's: it opens an empty topic.
		// The bot answers /new elsewhere.
		if !m.InGeneral() || h.Topics == nil || h.Threads == nil {
			return false
		}
		h.newEmptyTopic(ctx, m)
	default:
		return false
	}
	return true
}

func (h *Handler) miniApp(chat int64) string {
	if h.Chats == nil {
		return ""
	}
	return h.Chats.MiniAppURL(chat)
}

func (h *Handler) tldr(ctx context.Context, m Message, t Thread) {
	placeholder, err := h.Telegram.SendText(ctx, telegram.TextSend{
		ChatID: m.Chat, Text: TldrPending, MessageThreadID: m.Topic, ReplyToMessageID: m.ReplyTo, ReplyMarkup: telegram.EmptyKeyboard(),
	})
	if err != nil {
		h.log().Warn("failed to post the TLDR placeholder", "chat_id", m.Chat, "err", err)
		return
	}
	recap, err := h.Generator.TLDR(ctx, m.Chat, t.ID)
	h.edit(ctx, m.Chat, placeholder.ID, TldrText(recap, err), nil)
}

// worktree enables a git worktree for the thread and makes it the thread's
// root (/worktree, /wt).
func (h *Handler) worktree(ctx context.Context, m Message, t Thread) {
	path, err := h.Worktrees.EnsureWorktree(ctx, h.Chats.Binding(m.Chat).Root, t.ID)
	if err != nil {
		h.reply(ctx, m, esc("Failed to enable worktree: "+err.Error()+"\n\nTip: start the bot from inside a git repo (or a subdirectory of one)."), nil)
		return
	}
	if err := h.Threads.SetupThread(ctx, ThreadSetup{ThreadID: t.ID, Root: path}); err != nil {
		h.reply(ctx, m, esc("Failed to persist worktree root: "+err.Error()), nil)
		return
	}
	h.reply(ctx, m, esc("Worktree enabled: "+path), nil)
}

// newEmptyTopic is /new in General: an empty topic titled by its first
// message.
func (h *Handler) newEmptyTopic(ctx context.Context, m Message) {
	name := "Chat " + h.now().UTC().Format("2006-01-02 15:04")
	topic, err := h.Topics.CreateForumTopic(ctx, m.Chat, name)
	if err != nil {
		h.log().Warn("failed to create a topic from /new", "chat_id", m.Chat, "err", err)
		h.send(ctx, telegram.TextSend{ChatID: m.Chat, Text: "⚠️ I couldn't create a new topic. Please try again.", ReplyToMessageID: m.ReplyTo})
		return
	}
	threadID := conversation.ThreadID(m.Chat, &topic)
	if err := h.Threads.SetupThread(ctx, ThreadSetup{ThreadID: threadID, PendingTitle: true}); err != nil {
		h.log().Warn("created an empty topic but failed to mark its title pending", "thread_id", threadID, "err", err)
	}
	h.postHeader(ctx, m.Chat, topic, threadID)
}

// HandleCallback handles button data this package owns. false leaves the
// callback to the bot. The bot checks the allowlist first.
func (h *Handler) HandleCallback(ctx context.Context, q *telegram.CallbackQuery) bool {
	if q.Data == nil {
		return false
	}
	data := *q.Data
	switch {
	case strings.HasPrefix(data, "stg:") && h.Resolver != nil:
		h.stagingCallback(ctx, q, strings.TrimPrefix(data, "stg:"))
	case strings.HasPrefix(data, "cmd:") && h.Commands != nil && h.Dispatch != nil:
		h.pickerCallback(ctx, q, strings.TrimPrefix(data, "cmd:"))
	case strings.HasPrefix(data, "wk:") && h.Resolver != nil && h.Workers != nil:
		h.workerCallback(ctx, q, strings.TrimPrefix(data, "wk:"))
	case strings.HasPrefix(data, "model_") && h.Models != nil && h.Resolver != nil:
		return h.modelCallback(ctx, q, data)
	case strings.HasPrefix(data, "nt:") && h.launcherReady():
		h.launcherCallback(ctx, q, strings.TrimPrefix(data, "nt:"))
	case data == headerRefresh && h.Statuses != nil && h.Resolver != nil:
		h.headerCallback(ctx, q)
	default:
		return false
	}
	return true
}

// workerCallback is a legacy mirror topic's `⏹ Cancel worker` (wk:c). The
// worker comes from the topic, never from the button data.
func (h *Handler) workerCallback(ctx context.Context, q *telegram.CallbackQuery, rest string) {
	if rest != "c" {
		h.answer(ctx, q, "Unknown action")
		return
	}
	if q.Message == nil || q.Message.EffectiveThreadID() == nil {
		h.answer(ctx, q, "Not inside a worker topic")
		return
	}
	t, err := h.Resolver.ResolveThread(ctx, q.Message.Chat.ID, q.Message.EffectiveThreadID())
	if err != nil {
		// Unknown, not "not a worker topic": the lookup itself failed.
		h.log().Warn("failed to resolve a worker topic", "chat_id", q.Message.Chat.ID, "err", err)
		h.answer(ctx, q, "Couldn't check this topic; try again")
		return
	}
	if t.Kind != KindWorkerMirror {
		h.answer(ctx, q, "Not a worker topic")
		return
	}
	if err := h.Workers.CancelWorker(ctx, t.ID); err != nil {
		var refused *engine.ServiceError
		if errors.As(err, &refused) {
			// The service answered with a refusal. "Not managed" does not
			// mean idle: an unmanaged worker may still run (after a service
			// restart), so say what the service said.
			h.log().Info("worker cancel refused", "worker_thread_id", t.ID, "err", err)
			h.answer(ctx, q, cancelRefusal(refused.Message))
			return
		}
		h.log().Warn("worker cancel failed", "worker_thread_id", t.ID, "err", err)
		h.answer(ctx, q, "Couldn't cancel the worker; try again")
		return
	}
	h.answer(ctx, q, "Cancelling worker…")
}

// cancelRefusal words a cancel_worker refusal. The one known case, Rust
// workers.rs "Worker '<id>' is not managed", gets a plain sentence; any
// other refusal is shown as the service gave it, cut to fit an answer.
func cancelRefusal(message string) string {
	if strings.HasPrefix(message, "Worker '") && strings.HasSuffix(message, "' is not managed") {
		return "This worker is not managed by the service"
	}
	return truncateRunes("Couldn't cancel the worker: "+message, 200)
}

func (h *Handler) send(ctx context.Context, s telegram.TextSend) *telegram.Message {
	msg, err := h.Telegram.SendText(ctx, s)
	if err != nil {
		h.log().Warn("failed to send a command reply", "chat_id", s.ChatID, "err", err)
		return nil
	}
	return msg
}

func (h *Handler) edit(ctx context.Context, chat, id int64, html string, markup *telegram.InlineKeyboardMarkup) {
	if err := h.Telegram.EditMessageText(ctx, chat, id, html, markup); err != nil {
		h.log().Warn("failed to edit a command message", "chat_id", chat, "message_id", id, "err", err)
	}
}

func (h *Handler) delete(ctx context.Context, chat, id int64) error {
	return h.Telegram.DeleteMessage(ctx, chat, id)
}

func (h *Handler) answer(ctx context.Context, q *telegram.CallbackQuery, text string) {
	var t *string
	if text != "" {
		t = &text
	}
	if err := h.Telegram.AnswerCallbackQuery(ctx, q.ID, t); err != nil {
		h.log().Warn("failed to answer a callback", "err", err)
	}
}
