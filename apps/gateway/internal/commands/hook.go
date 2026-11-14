package commands

import (
	"context"
	"strings"

	"github.com/tallesborges/zdx/apps/gateway/internal/bot"
	"github.com/tallesborges/zdx/apps/gateway/internal/engine"
	"github.com/tallesborges/zdx/apps/gateway/internal/telegram"
)

// Hook adapts a Handler to the bot's hooks (bot.CommandHook,
// bot.CallbackHook). The bot never imports this package.
type Hook struct {
	*Handler
}

var (
	_ bot.CommandHook  = (*Hook)(nil)
	_ bot.CallbackHook = (*Hook)(nil)
)

// NewHook wraps h.
func NewHook(h *Handler) *Hook { return &Hook{Handler: h} }

// ownedCallbackPrefixes are the button data of commands this package owns
// (or will own). Buttons of the parts not built yet are answered as
// unavailable instead of falling through.
var ownedCallbackPrefixes = []string{"stg:", "cmd:", "wk:", "nt:", "thread_header:", "model_", "cancel_q:"}

// HandleCommand implements bot.CommandHook. It claims every command the
// Rust bot knows: a command this build cannot run is refused, never sent
// to the agent as text. Unknown slash text (and /cancel without a staged
// command) is not claimed and runs as a turn, as in the Rust bot.
func (k *Hook) HandleCommand(ctx context.Context, req bot.CommandRequest) bool {
	m := messageOf(req)
	t := Thread{ID: req.Topic.ThreadID, Kind: string(req.Topic.Kind)}
	c, isCommand := Parse(req.Text)
	if isCommand && c == CmdHandoff && t.Kind == KindProfile && k.Resolver != nil {
		// The handoff role defaults to the source's profile, which the
		// bot's TopicInfo does not carry.
		if full, err := k.Resolver.ResolveThread(ctx, m.Chat, m.Topic); err == nil {
			t.Profile = full.Profile
		} else {
			k.log().Warn("failed to read the source thread's profile", "chat_id", m.Chat, "err", err)
		}
	}
	slow := (c == CmdTldr && k.Generator != nil) || (c == CmdRestart && k.Lifecycle != nil)
	if isCommand && slow && !m.HasMedia {
		// /tldr waits on a generation and /restart on the services; the
		// topic's next message must not.
		k.goOwned(func() { k.HandleMessage(context.WithoutCancel(ctx), m, t) })
		return true
	}
	if k.HandleMessage(ctx, m, t) {
		return true
	}
	if _, staged := stagedOf(req.Text); staged && m.Synthetic {
		// A bot-made /btw, /handoff or /prompt_builder text starts nothing:
		// it runs as the ordinary prompt it is.
		return false
	}
	if name, known := knownCommand(req.Text); known {
		k.send(ctx, telegram.TextSend{
			ChatID: m.Chat, Text: esc("This gateway build cannot handle " + name + " yet."), MessageThreadID: m.Topic, ReplyToMessageID: m.ReplyTo, DisablePreview: true,
		})
		return true
	}
	return false
}

// StagedInput runs a message that is not a command (typed text, or voice
// with its transcript) as the input of a live staged command on its
// thread. true means it was consumed and must not run a turn. The bot calls
// it for every non-mirror message before submitting; slash text reaches it
// through HandleCommand instead. A synthetic message is never taken.
func (k *Hook) StagedInput(ctx context.Context, req bot.CommandRequest, transcript string, synthetic bool) bool {
	m := messageOf(req)
	m.Transcript = transcript
	m.Synthetic = m.Synthetic || synthetic
	return k.staging(ctx, m, Thread{ID: req.Topic.ThreadID, Kind: string(req.Topic.Kind)})
}

// knownCommand names a command the Rust bot handles that this build did
// not run.
func knownCommand(text string) (string, bool) {
	if c, ok := Parse(text); ok {
		return "/" + string(c), true
	}
	if _, ok := ParseModel(text); ok {
		return "/model", true
	}
	if _, ok := ParseQueue(text); ok {
		return "/queue", true
	}
	return "", false
}

// HandleCallback implements bot.CallbackHook. Owned data is answered on
// its own goroutine; the polling loop never waits.
func (k *Hook) HandleCallback(ctx context.Context, q *telegram.CallbackQuery) bool {
	if q.Data == nil {
		return false
	}
	owned := false
	for _, p := range ownedCallbackPrefixes {
		if strings.HasPrefix(*q.Data, p) {
			owned = true
			break
		}
	}
	if !owned {
		return false
	}
	k.goOwned(func() {
		ctx := context.WithoutCancel(ctx)
		if !k.Handler.HandleCallback(ctx, q) {
			k.answer(ctx, q, "Not available in this gateway build yet")
		}
	})
	return true
}

// messageOf reads a bot request. Whether media blocks a command is the
// bot's intake result (HasLoadedImagesOrAudio: an image or audio loaded
// from any album member, as Rust checks), never the raw message: a photo
// that failed to load leaves a caption command working, an image in a later
// album member blocks it.
func messageOf(req bot.CommandRequest) Message {
	tm := req.Message
	m := Message{
		Chat: tm.Chat.ID, ChatType: tm.Chat.Type, IsForum: tm.Chat.IsForumEnabled(),
		Topic: tm.EffectiveThreadID(), ID: tm.ID, Text: req.Text, ReplyTo: req.ReplyTo,
		HasMedia: req.HasLoadedImagesOrAudio, Synthetic: req.Synthetic,
	}
	if tm.From != nil {
		m.UserID = tm.From.ID
	}
	return m
}

// BotDispatch is the Dispatcher over the bot's intake (*bot.Handler).
type BotDispatch struct {
	Bot interface {
		Dispatch(ctx context.Context, in bot.Incoming)
	}
}

// Dispatch implements Dispatcher. Rule: every message this package builds
// (a /commands tap, a prompt-builder Accept, a /handoff or /btw seed) is
// Incoming.Synthetic, so it never starts or answers a staged command and
// never skips the topic queue as a /btw question. Its id stays the real
// message its turn replies to. The Rust bot marks only Mini App feedback
// synthetic; a /commands tap during a staged command is therefore a turn
// here, not the staged input as in Rust. That difference is on purpose.
func (d BotDispatch) Dispatch(ctx context.Context, s Synthetic) {
	text := s.Text
	forum := s.IsForum
	d.Bot.Dispatch(ctx, bot.Incoming{Synthetic: true, Message: &telegram.Message{
		ID:       s.MessageID,
		Chat:     telegram.Chat{ID: s.Chat, Type: s.ChatType, IsForum: &forum},
		From:     &telegram.User{ID: s.UserID},
		Text:     &text,
		ThreadID: s.Topic,
	}})
}

// EngineThreads is the Resolver over the service's resolve_thread
// (*engine.Client).
type EngineThreads struct {
	Engine interface {
		ResolveThread(ctx context.Context, chat int64, topic *int64) (engine.ThreadRoute, error)
	}
}

// ResolveThread implements Resolver.
func (e EngineThreads) ResolveThread(ctx context.Context, chat int64, topic *int64) (Thread, error) {
	r, err := e.Engine.ResolveThread(ctx, chat, topic)
	if err != nil {
		return Thread{}, err
	}
	t := Thread{ID: r.EffectiveThreadID, Kind: r.Kind}
	if r.Profile != nil {
		t.Profile = *r.Profile
	}
	return t, nil
}
