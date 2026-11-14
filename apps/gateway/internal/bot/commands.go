package bot

import (
	"context"
	"strings"

	"github.com/tallesborges/zdx/apps/gateway/internal/engine"
	"github.com/tallesborges/zdx/apps/gateway/internal/telegram"
)

// ThreadOps are the Rust thread operations commands use.
type ThreadOps interface {
	UpdateThread(ctx context.Context, u engine.UpdateThread) (engine.ThreadUpdated, error)
	ResetThread(ctx context.Context, threadID string) error
}

// command is a slash command this gateway handles.
type command struct {
	kind  string // "new", "model_set", "model_reset"
	model string
}

// parseCommand recognises the commands this build handles, with the Rust
// bot's parsing (crates/zdx-bot/src/commands.rs). Everything else that
// starts with "/" stays unsupported.
func parseCommand(text string) (command, bool) {
	t := strings.TrimSpace(text)
	if telegram.CommandMatches(t, "/new") {
		return command{kind: "new"}, true
	}
	var rest string
	switch {
	case strings.HasPrefix(t, "/model@"):
		after := strings.TrimPrefix(t, "/model")
		if i := strings.IndexByte(after, ' '); i >= 0 {
			rest = after[i:]
		} else {
			rest = ""
		}
	case t == "/model" || strings.HasPrefix(t, "/model "):
		rest = strings.TrimPrefix(t, "/model")
	default:
		return command{}, false
	}
	parts := strings.Fields(rest)
	switch {
	case len(parts) >= 2 && parts[0] == "set":
		return command{kind: "model_set", model: parts[1]}, true
	case len(parts) == 1 && parts[0] == "reset":
		return command{kind: "model_reset"}, true
	}
	// /model and /model list show the picker, which this build lacks.
	return command{}, false
}

// chatDefaultModel is /model set|reset in General (Rust model_set_reply,
// ModelSubcommand::Reset with is_general).
func (h *Handler) chatDefaultModel(ctx context.Context, root string, cmd command, say func(string), failed func(error)) {
	if cmd.kind == "model_set" {
		if _, err := h.Engine.SetChatModel(ctx, root, cmd.model); err != nil {
			failed(err)
			return
		}
		say("✅ Default model set to <code>" + EscapeHTML(cmd.model) + "</code>.")
		return
	}
	var picker struct {
		ConfigError string `json:"config_error"`
		Label       string `json:"label"`
	}
	if err := h.Engine.Catalog(ctx, engine.Catalog{Kind: engine.CatalogPicker, Root: root, Scope: "general"}, &picker); err != nil {
		failed(err)
		return
	}
	if picker.ConfigError != "" {
		say("❌ " + EscapeHTML(picker.ConfigError))
		return
	}
	say("Default model: <code>" + EscapeHTML(picker.Label) + "</code>\n\nUse /model set &lt;id&gt; to change.")
}

// runCommand handles a parsed command for a resolved, non-mirror topic.
func (h *Handler) runCommand(ctx context.Context, m *telegram.Message, info TopicInfo, cmd command, reply *int64) {
	chat := m.Chat.ID
	topic := m.EffectiveThreadID()
	say := func(html string) { h.notifyHTML(ctx, chat, topic, reply, html) }
	failed := func(err error) { say("❌ " + EscapeHTML(err.Error())) }
	switch cmd.kind {
	case "new":
		// Rust: refused in forum topics, and in orchestrator threads, whose
		// service turns and worker reviews own them.
		if m.Chat.IsForumEnabled() && topic != nil {
			say("/new is not allowed in topics.")
			return
		}
		if info.Kind == TopicProfile {
			say("/new is not available in an orchestrator thread. Start a new thread instead.")
			return
		}
		if err := h.ThreadOps.ResetThread(ctx, info.ThreadID); err != nil {
			failed(err)
			return
		}
		say("History cleared. Start a new conversation anytime.")
	case "model_set", "model_reset":
		root := h.RootForChat(chat)
		// General sets the chat's default (Rust set_chat_model); every other
		// conversation, private chats and groups without topics included,
		// sets its own thread's model, as in the Rust bot.
		if m.Chat.IsForumEnabled() && topic == nil {
			h.chatDefaultModel(ctx, root, cmd, say, failed)
			return
		}
		u := engine.UpdateThread{ThreadID: info.ThreadID, ConfigRoot: &root}
		if cmd.kind == "model_set" {
			u.Model = &cmd.model
		} else {
			u.ClearModel = true
		}
		updated, err := h.ThreadOps.UpdateThread(ctx, u)
		if err != nil {
			failed(err)
			return
		}
		switch {
		case cmd.kind == "model_set" && updated.ModelLabel != nil:
			say("✅ Model set to <code>" + EscapeHTML(*updated.ModelLabel) + "</code> for this topic.")
		case cmd.kind == "model_set":
			say("✅ Model set to <code>" + EscapeHTML(cmd.model) + "</code> for this topic.")
		case updated.ModelLabel != nil:
			say("✅ Model reset to default: <code>" + EscapeHTML(*updated.ModelLabel) + "</code>")
		default:
			say("✅ Model reset to default.")
		}
	}
}
