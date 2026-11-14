package commands

import (
	"strconv"
	"strings"

	"github.com/tallesborges/zdx/apps/gateway/internal/render"
	"github.com/tallesborges/zdx/apps/gateway/internal/telegram"
)

var esc = render.EscapeHTML

// GeneralRefusal is the answer to a topic-only command sent in a forum's
// General topic. ok is false for commands that work there.
func GeneralRefusal(c Command) (string, bool) {
	switch c {
	case CmdWorktree:
		return "/worktree must be used inside a topic, not General.", true
	case CmdHandoff:
		return "/handoff must be used inside a topic, not General.", true
	case CmdBtw:
		return "/btw must be used inside a topic, not General.", true
	case CmdCommands:
		return "/commands must be used inside a topic, not General.", true
	case CmdPromptBuilder:
		return "/prompt_builder must be used inside a topic, not General.", true
	}
	return "", false
}

// LauncherOutsideGeneral answers /launcher anywhere but General.
const LauncherOutsideGeneral = "/launcher is only available in General."

// StaleBtwInput answers a /btw question that skipped the topic queue after
// its session ended (Rust notify_stale_btw_input).
const StaleBtwInput = "💬 No side question is pending anymore — send /btw again to ask one."

// ThreadIDText is the /threadid answer.
func ThreadIDText(threadID string) string {
	return "<b>Thread ID</b> <i>(tap to copy)</i>\n<code>" + esc(threadID) + "</code>"
}

// ThreadsNotConfigured answers /threads without a Mini App.
const ThreadsNotConfigured = "The Threads Mini App is not configured."

// ThreadsReply is the /threads answer: a deep link into the Mini App.
func ThreadsReply(miniAppURL, threadID string) (string, *telegram.InlineKeyboardMarkup) {
	return "📑 <b>Thread Viewer</b>\nTap below to open the interactive transcript:",
		&telegram.InlineKeyboardMarkup{InlineKeyboard: [][]telegram.InlineKeyboardButton{{
			telegram.URLButton("💬 Open Thread in Mini App", miniAppURL+"?startapp="+threadID),
		}}}
}

// WhereAmIText is the /whereami answer. In discovery mode (a group not on
// the allowlist) it shows ids only, never the bot's folders or profiles.
func WhereAmIText(chat int64, topic *int64, b Binding, discovery bool) string {
	id := strconv.FormatInt(chat, 10)
	lines := []string{"<b>Where am I</b>", "Chat ID: <code>" + id + "</code>"}
	if topic != nil {
		lines = append(lines, "Topic ID: <code>"+strconv.FormatInt(*topic, 10)+"</code>")
	}
	if discovery {
		lines = append(lines,
			"Status: <code>chat not on bot allowlist</code>",
			"Allow this chat by adding <code>"+id+"</code> to <code>telegram.allowlist_chat_ids</code> in <code>config.toml</code>, then restart the bot.",
			"Bind workspace: <code>zdx bot profile add &lt;name&gt; "+id+" &lt;cwd&gt;</code>",
		)
		return strings.Join(lines, "\n")
	}
	if b.Profile != "" {
		lines = append(lines,
			"Profile: <code>"+esc(b.Profile)+"</code>",
			"CWD: <code>"+esc(b.Root)+"</code>",
		)
	} else {
		lines = append(lines,
			"Profile: <code>none</code> (fallback root)",
			"CWD: <code>"+esc(b.Root)+"</code>",
			"Bind this chat with: <code>zdx bot profile add &lt;name&gt; "+id+" &lt;cwd&gt;</code>",
		)
	}
	return strings.Join(lines, "\n")
}

// TldrPending is the placeholder /tldr edits into the recap.
const TldrPending = "⏳ Generating TLDR…"

// TldrText is the finished /tldr message.
func TldrText(recap string, err error) string {
	if err != nil {
		return "⚠️ TLDR failed:\n<pre>" + esc(err.Error()) + "</pre>"
	}
	return "📝 <b>TLDR</b>\n" + render.TruncateHTML(render.ToTelegramHTML(recap), 3900)
}

// TopicLink is the t.me link of a supergroup topic (zdx_engine::telegram::
// topic_link); ok is false for other chats.
func TopicLink(chat, topic int64) (string, bool) {
	internal, ok := strings.CutPrefix(strconv.FormatInt(chat, 10), "-100")
	if !ok || internal == "" || topic <= 0 {
		return "", false
	}
	return "https://t.me/c/" + internal + "/" + strconv.FormatInt(topic, 10), true
}

// OrchestratorTopicName marks an orchestrator topic's name once.
func OrchestratorTopicName(name string) string {
	t := strings.TrimSpace(name)
	if strings.HasPrefix(t, "🎛") {
		return t
	}
	return "🎛️ " + t
}

func syntheticChatType(chatType string) string {
	if chatType == "private" {
		return "private"
	}
	return "supergroup"
}
