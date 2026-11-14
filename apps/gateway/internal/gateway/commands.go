package gateway

import (
	"context"
	"os"

	"github.com/tallesborges/zdx/apps/gateway/internal/bot"
	"github.com/tallesborges/zdx/apps/gateway/internal/commands"
	"github.com/tallesborges/zdx/apps/gateway/internal/conversation"
	"github.com/tallesborges/zdx/apps/gateway/internal/engine"
)

// commandHook builds the commands package over the shared handles. The
// helper and catalog parts are Rust operations (serviceHelpers, each
// checking its capability). Worktrees run the zdx CLI when Deps.Zdx is
// set; /restart needs Parts.Lifecycle.
func commandHook(d Deps, settings Settings, intake *bot.Handler, blocks *bot.TopicBlocks, lifecycle commands.Lifecycle) *commands.Hook {
	var worktrees commands.Worktrees
	if d.Zdx != "" {
		worktrees = CLIWorktrees{Zdx: d.Zdx, Env: os.Environ()}
	}
	helpers := serviceHelpers{engine: d.Engine, root: settings.RootForChat}
	return commands.NewHook(&commands.Handler{
		Worktrees: worktrees,
		Lifecycle: lifecycle,
		Telegram:  d.Telegram,
		Chats:     settingsChats{settings: settings, allow: d.Allow},
		Resolver:  commands.EngineThreads{Engine: d.Engine},
		Dispatch:  commands.BotDispatch{Bot: intake},
		Topics:    d.Telegram,
		Threads:   threadSetup{engine: d.Engine, root: settings.RootForChat},
		Blocker:   blocks,
		Workers:   workerCanceller{d.Engine},
		Seeds:     helpers,
		Generator: helpers,
		Commands:  helpers,
		Models:    helpers,
		Defaults:  helpers,
		Launches:  helpers,
		Statuses:  helpers,
		Log:       d.Log,
	})
}

// settingsChats answers per-chat settings from the Rust-resolved settings.
type settingsChats struct {
	settings Settings
	allow    bot.Allowlist
}

var _ commands.Chats = settingsChats{}

func (c settingsChats) profile(chat int64) (Profile, bool) { return c.settings.Profile(chat) }

func (c settingsChats) Binding(chat int64) commands.Binding {
	if p, ok := c.profile(chat); ok {
		return commands.Binding{Profile: p.Name, Root: p.Cwd}
	}
	return commands.Binding{Root: c.settings.Root}
}

func (c settingsChats) ChatAllowed(chat int64) bool { return c.allow.Chats[chat] }

func (c settingsChats) MiniAppURL(int64) string { return c.settings.MiniAppURL() }

func (c settingsChats) OrchestratorChat(chat int64) bool {
	p, ok := c.profile(chat)
	return ok && p.Orchestrator
}

// threadSetup writes a new topic's thread metadata through update_thread.
// A model and a profile are checked against the topic's chat root
// (config_root; a new thread has no stored root yet).
type threadSetup struct {
	engine *engine.Client
	root   func(chat int64) string
}

func (t threadSetup) SetupThread(ctx context.Context, s commands.ThreadSetup) error {
	opt := func(v string) *string {
		if v == "" {
			return nil
		}
		return &v
	}
	u := engine.UpdateThread{ThreadID: s.ThreadID, Model: s.Model,
		HandoffFrom: opt(s.HandoffFrom), PersistentProfile: opt(s.Profile), AliasTo: opt(s.AliasTo), Root: opt(s.Root)}
	if r, ok := conversation.Parse(s.ThreadID); ok && t.root != nil {
		root := t.root(r.Chat)
		// Rust checks a model and a profile against the chat's root and
		// stores no root for them: the first turn records it, as in the
		// Rust bot.
		if s.Model != nil || s.Profile != "" {
			u.ConfigRoot = &root
		}
	}
	if s.PendingTitle {
		yes := true
		u.PendingTopicTitle = &yes
	}
	_, err := t.engine.UpdateThread(ctx, u)
	return err
}

// workerCanceller is cancel_worker.
type workerCanceller struct{ engine *engine.Client }

func (w workerCanceller) CancelWorker(ctx context.Context, threadID string) error {
	_, err := w.engine.CancelWorker(ctx, threadID)
	return err
}

// topicHooks open topics from General with the commands package's header,
// launcher and naming.
type topicHooks struct {
	chats settingsChats
	hook  *commands.Hook
}

var _ bot.TopicHooks = topicHooks{}

func (t topicHooks) OrchestratorChat(chat int64) bool { return t.chats.OrchestratorChat(chat) }

func (topicHooks) OrchestratorTopicName(name string) string {
	return commands.OrchestratorTopicName(name)
}

func (t topicHooks) PostThreadHeader(ctx context.Context, chat, topic int64, threadID string) error {
	return t.hook.PostThreadHeader(ctx, chat, topic, threadID)
}

func (topicHooks) IsCommand(text string) bool {
	_, ok := commands.Parse(text)
	return ok
}

func (t topicHooks) ScheduleLauncherRepost(ctx context.Context, chat int64) {
	t.hook.ScheduleLauncherRepost(ctx, chat)
}

// queueBypass is the commands package's queue rules for the bot.
type queueBypass struct{ hook *commands.Hook }

var _ bot.QueueBypass = queueBypass{}

func (q queueBypass) BypassesQueue(text string) bool { return commands.BypassesQueue(text) }

func (q queueBypass) AwaitingBtwInput(threadID string, synthetic bool) bool {
	return q.hook.AwaitingBtwInput(threadID, synthetic)
}

func (queueBypass) StaleBtwInput() string { return commands.StaleBtwInput }
