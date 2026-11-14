package commands

import (
	"context"
	"errors"
	"fmt"
	"strconv"
	"strings"
	"time"

	"github.com/tallesborges/zdx/apps/gateway/internal/conversation"
	"github.com/tallesborges/zdx/apps/gateway/internal/telegram"
)

// UnsetupTopicNotice is posted into a launcher orchestrator topic that
// could not be set up and could not be deleted.
const UnsetupTopicNotice = "⚠️ I couldn't set up this topic as an orchestrator on the picked model, so I won't run messages here. Open a new one from /launcher."

// defaultRepostDelay coalesces bursts of General messages into one
// launcher repost (Rust LAUNCHER_REPOST_DEBOUNCE).
const defaultRepostDelay = 700 * time.Millisecond

// launcherState is the chat's open launcher. Presence means one is open.
type launcherState struct {
	message    int64
	generation uint64
}

func (h *Handler) launcherReady() bool {
	return h.Launches != nil && h.Topics != nil && h.Threads != nil && h.Chats != nil
}

func roleLabel(role string) string {
	if role == RoleOrchestrator {
		return "🧭 Orchestrator"
	}
	return "🤖 Agent"
}

func otherRole(role string) string {
	if role == RoleOrchestrator {
		return RoleAgent
	}
	return RoleOrchestrator
}

func roleCode(role string) string {
	if role == RoleOrchestrator {
		return "o"
	}
	return "a"
}

func (h *Handler) defaultRole(chat int64) string {
	if h.Chats.OrchestratorChat(chat) {
		return RoleOrchestrator
	}
	return RoleAgent
}

func afterLastColon(s string) string {
	if i := strings.LastIndex(s, ":"); i >= 0 {
		return s[i+1:]
	}
	return s
}

func thinkingSuffix(level string) string {
	if level == "" || level == "off" {
		return ""
	}
	return " · " + level
}

// LauncherHeader is the launcher text: the role new topics start as, its
// default model, and each mode with what it maps to.
func LauncherHeader(v LauncherView, role string) string {
	defaultID := afterLastColon(v.DefaultModel)
	thinking := thinkingSuffix(v.DefaultThinking)
	roleLine := "New topics start as: <b>" + roleLabel(role) + "</b>"
	defaultLine := "Default: <code>" + esc(defaultID) + "</code>" + thinking
	if v.DefaultMode != "" {
		defaultLine = "Default: <code>" + esc(v.DefaultMode) + "</code> → <code>" + esc(defaultID) + "</code>" + thinking
	}
	var text string
	if len(v.Modes) == 0 {
		text = "🚀 <b>Thread launcher</b>\n" + roleLine + "\n" + defaultLine + "\nNo model modes configured yet — use 🎛 Custom, or add <code>[[model_modes]]</code> to your config."
	} else {
		lines := []string{"🚀 <b>Thread launcher</b>", roleLine, defaultLine, ""}
		for _, m := range v.Modes {
			description := ""
			if d := strings.TrimSpace(m.Description); d != "" {
				description = " — " + esc(d)
			}
			lines = append(lines, "<b>"+esc(m.Name)+"</b> → <code>"+esc(afterLastColon(m.Primary))+"</code>"+thinkingSuffix(m.Thinking)+description)
		}
		lines = append(lines, "", "<i>Tap a mode, or use 🎛 Custom / 🔄 Continue. The first button switches agent / orchestrator.</i>")
		text = strings.Join(lines, "\n")
	}
	if v.LaunchError != "" {
		text += "\n\n⚠️ " + esc(v.LaunchError) + "; new topics of this role are refused until it is fixed."
	}
	return text
}

// LauncherKeyboard is the role switch, the modes, Custom and Continue, and
// Dismiss.
func LauncherKeyboard(v LauncherView, role string) *telegram.InlineKeyboardMarkup {
	rows := [][]telegram.InlineKeyboardButton{{
		telegram.CallbackButton("New as: "+roleLabel(role)+" ⇄", "nt:as:"+roleCode(otherRole(role))),
	}}
	prefix, custom := "nt:p:", "nt:custom"
	if role == RoleOrchestrator {
		prefix, custom = "nt:op:", "nt:ocustom"
	}
	var modes []telegram.InlineKeyboardButton
	for _, m := range v.Modes {
		modes = append(modes, telegram.CallbackButton(m.Name, prefix+m.Name))
	}
	rows = append(rows, chunk(modes, 3)...)
	rows = append(rows,
		[]telegram.InlineKeyboardButton{telegram.CallbackButton("🎛 Custom", custom), telegram.CallbackButton("🔄 Continue", "nt:resume")},
		[]telegram.InlineKeyboardButton{telegram.CallbackButton("✖ Dismiss", "nt:x")},
	)
	return &telegram.InlineKeyboardMarkup{InlineKeyboard: rows}
}

func (h *Handler) launcherView(ctx context.Context, chat int64, role string) (string, *telegram.InlineKeyboardMarkup, error) {
	v, err := h.Launches.LauncherView(ctx, chat, role)
	if err != nil {
		return "", nil, err
	}
	return LauncherHeader(v, role), LauncherKeyboard(v, role), nil
}

func (h *Handler) sendLauncher(ctx context.Context, chat int64, reply *int64) (int64, error) {
	text, kb, err := h.launcherView(ctx, chat, h.defaultRole(chat))
	if err != nil {
		return 0, err
	}
	msg, err := h.Telegram.SendText(ctx, telegram.TextSend{ChatID: chat, Text: text, ReplyToMessageID: reply, ReplyMarkup: kb})
	if err != nil {
		return 0, fmt.Errorf("post launcher keyboard: %w", err)
	}
	return msg.ID, nil
}

// PostLauncher posts the launcher into General and keeps it the last
// message there (ScheduleLauncherRepost).
func (h *Handler) PostLauncher(ctx context.Context, chat int64, reply *int64) error {
	id, err := h.sendLauncher(ctx, chat, reply)
	if err != nil {
		return err
	}
	h.mu.Lock()
	defer h.mu.Unlock()
	if h.launchers == nil {
		h.launchers = map[int64]*launcherState{}
	}
	st := h.launchers[chat]
	if st == nil {
		st = &launcherState{}
		h.launchers[chat] = st
	}
	st.generation++
	st.message = id
	return nil
}

// ScheduleLauncherRepost moves an open launcher back to the bottom of
// General (delete and repost). Bursts coalesce: only the newest request
// reposts. The bot calls it after posting into General.
func (h *Handler) ScheduleLauncherRepost(ctx context.Context, chat int64) {
	if !h.launcherReady() {
		return
	}
	h.mu.Lock()
	st := h.launchers[chat]
	if st == nil {
		h.mu.Unlock()
		return
	}
	st.generation++
	mine := st.generation
	h.mu.Unlock()
	delay := h.RepostDelay
	if delay <= 0 {
		delay = defaultRepostDelay
	}
	h.goOwned(func() {
		ctx := context.WithoutCancel(ctx)
		time.Sleep(delay)
		h.mu.Lock()
		st := h.launchers[chat]
		if st == nil || st.generation != mine {
			h.mu.Unlock()
			return
		}
		old := st.message
		h.mu.Unlock()
		if err := h.delete(ctx, chat, old); err != nil {
			h.log().Warn("failed to delete the old launcher", "chat_id", chat, "err", err)
		}
		id, err := h.sendLauncher(ctx, chat, nil)
		if err != nil {
			h.log().Warn("failed to repost the launcher", "chat_id", chat, "err", err)
			return
		}
		h.mu.Lock()
		st = h.launchers[chat]
		superseded := st == nil || st.generation != mine
		if !superseded {
			st.message = id
		}
		h.mu.Unlock()
		if superseded {
			_ = h.delete(ctx, chat, id)
		}
	})
}

// renderLauncher edits message back into the launcher for role.
func (h *Handler) renderLauncher(ctx context.Context, chat, message int64, role string) {
	text, kb, err := h.launcherView(ctx, chat, role)
	if err != nil {
		h.log().Warn("failed to render the launcher", "chat_id", chat, "err", err)
		return
	}
	h.edit(ctx, chat, message, text, kb)
}

// dismissLauncher deletes a launcher, or drops its buttons when Telegram
// refuses the delete, and stops reposting it.
func (h *Handler) dismissLauncher(ctx context.Context, chat, message int64) {
	h.mu.Lock()
	if st := h.launchers[chat]; st != nil && st.message == message {
		delete(h.launchers, chat)
	}
	h.mu.Unlock()
	if err := h.delete(ctx, chat, message); err != nil {
		h.log().Warn("failed to delete a dismissed launcher; removing its buttons", "chat_id", chat, "err", err)
		if err := h.Telegram.EditMessageReplyMarkup(ctx, chat, message, telegram.EmptyKeyboard()); err != nil {
			h.log().Warn("failed to remove the dismissed launcher's buttons", "chat_id", chat, "err", err)
		}
	}
}

// errNoBlocker refuses orchestrator launches while nothing can block a
// topic whose setup fails.
var errNoBlocker = errors.New("orchestrator topics need a topic blocker")

// launchTopic opens a topic of role pinned to model. An agent topic whose
// metadata write fails still opens on defaults; an orchestrator fails
// closed: the topic is blocked, then deleted or warned.
func (h *Handler) launchTopic(ctx context.Context, chat int64, model, role string) (int64, error) {
	if role == RoleOrchestrator && h.Blocker == nil {
		return 0, errNoBlocker
	}
	name := "Chat " + h.now().UTC().Format("2006-01-02 15:04")
	setup := ThreadSetup{Model: &model, PendingTitle: true}
	if role == RoleOrchestrator {
		name = OrchestratorTopicName(name)
		setup.Profile = OrchestratorProfile
	}
	topic, err := h.Topics.CreateForumTopic(ctx, chat, name)
	if err != nil {
		return 0, fmt.Errorf("create forum topic for launcher: %w", err)
	}
	setup.ThreadID = conversation.ThreadID(chat, &topic)
	if err := h.Threads.SetupThread(ctx, setup); err != nil {
		if role == RoleOrchestrator {
			h.abandonOrchestratorTopic(ctx, chat, topic, setup.ThreadID)
			return 0, fmt.Errorf("set up launcher orchestrator topic: %w", err)
		}
		h.log().Warn("launcher created a topic but failed to apply its model", "thread_id", setup.ThreadID, "err", err)
	}
	h.postHeader(ctx, chat, topic, setup.ThreadID)
	return topic, nil
}

func (h *Handler) abandonOrchestratorTopic(ctx context.Context, chat, topic int64, threadID string) {
	if err := h.Blocker.BlockTopic(ctx, threadID); err != nil {
		h.log().Error("failed to block an orchestrator topic whose setup failed", "thread_id", threadID, "err", err)
	}
	if err := h.Topics.DeleteForumTopic(ctx, chat, topic); err != nil {
		h.log().Warn("failed to delete an orchestrator topic whose setup failed", "chat_id", chat, "topic_id", topic, "err", err)
		h.send(ctx, telegram.TextSend{ChatID: chat, Text: UnsetupTopicNotice, MessageThreadID: &topic})
	}
}

func (h *Handler) answerLaunched(ctx context.Context, q *telegram.CallbackQuery, role string, err error) {
	var text string
	switch {
	case err == nil && role == RoleOrchestrator:
		text = "New orchestrator ready ✓"
	case err == nil:
		text = "New thread ready ✓"
	case role == RoleOrchestrator:
		text = "Couldn't create the orchestrator"
	default:
		text = "Couldn't create the topic"
	}
	if err != nil {
		h.log().Error("launcher failed to create a topic", "role", role, "err", err)
	}
	h.answer(ctx, q, text)
}

// relativeAge is a compact age label for a resume button.
func relativeAge(now time.Time, modified *time.Time) string {
	if modified == nil || modified.After(now) {
		return "?"
	}
	secs := int64(now.Sub(*modified) / time.Second)
	switch {
	case secs < 60:
		return "now"
	case secs < 3600:
		return strconv.FormatInt(secs/60, 10) + "m"
	case secs < 86_400:
		return strconv.FormatInt(secs/3600, 10) + "h"
	}
	return strconv.FormatInt(secs/86_400, 10) + "d"
}

// ResumeKeyboard is one button per thread (nt:r:<id>) and Back.
func ResumeKeyboard(threads []ResumeThread, now time.Time) *telegram.InlineKeyboardMarkup {
	var rows [][]telegram.InlineKeyboardButton
	for _, t := range threads {
		rows = append(rows, []telegram.InlineKeyboardButton{
			telegram.CallbackButton(truncateRunes(t.Title, 40)+" · "+relativeAge(now, t.Modified), "nt:r:"+t.ID),
		})
	}
	rows = append(rows, []telegram.InlineKeyboardButton{telegram.CallbackButton("← Back", "nt:back")})
	return &telegram.InlineKeyboardMarkup{InlineKeyboard: rows}
}

// resumeTopic opens a topic that continues source (alias_to).
func (h *Handler) resumeTopic(ctx context.Context, chat int64, source string) error {
	title, err := h.Launches.ResumeSource(ctx, source)
	if err != nil {
		return err
	}
	if title == "" {
		title = "Resumed " + truncateRunes(source, 8)
	}
	topic, err := h.Topics.CreateForumTopic(ctx, chat, title)
	if err != nil {
		return fmt.Errorf("create forum topic for resume: %w", err)
	}
	if err := h.Threads.SetupThread(ctx, ThreadSetup{ThreadID: conversation.ThreadID(chat, &topic), AliasTo: source}); err != nil {
		return fmt.Errorf("set alias on resumed topic: %w", err)
	}
	h.postHeader(ctx, chat, topic, source)
	return nil
}

func (h *Handler) launcherCallback(ctx context.Context, q *telegram.CallbackQuery, rest string) {
	msg := q.Message
	if msg == nil {
		h.answer(ctx, q, "No message context")
		return
	}
	chat := msg.Chat.ID
	switch {
	case strings.HasPrefix(rest, "p:"):
		h.openModeTopic(ctx, q, strings.TrimPrefix(rest, "p:"), RoleAgent)
	case strings.HasPrefix(rest, "op:"):
		h.openModeTopic(ctx, q, strings.TrimPrefix(rest, "op:"), RoleOrchestrator)
	case rest == "as:a" || rest == "as:o":
		role := RoleAgent
		if rest == "as:o" {
			role = RoleOrchestrator
		}
		h.renderLauncher(ctx, chat, msg.ID, role)
		h.answer(ctx, q, "")
	case rest == "x":
		h.answer(ctx, q, "")
		h.dismissLauncher(ctx, chat, msg.ID)
	case rest == "custom" || rest == "ocustom":
		if h.Models == nil {
			h.answer(ctx, q, "Not available in this gateway build yet")
			return
		}
		scope := ScopeNewThread
		if rest == "ocustom" {
			scope = ScopeNewOrchestrator
		}
		p, err := h.Models.Picker(ctx, PickerQuery{Chat: chat, Scope: scope})
		if err != nil {
			h.log().Warn("failed to open the launcher's model picker", "chat_id", chat, "err", err)
			h.answer(ctx, q, truncateRunes(err.Error(), 200))
			return
		}
		h.edit(ctx, chat, msg.ID, PickerHeader(p, scope, nil), ProviderKeyboard(p, scope))
		h.answer(ctx, q, "")
	case rest == "resume":
		threads, err := h.Launches.Resumable(ctx, chat)
		if err != nil {
			h.log().Warn("failed to list resumable threads", "chat_id", chat, "err", err)
		}
		if len(threads) == 0 {
			h.answer(ctx, q, "No recent threads for this project yet")
			return
		}
		h.edit(ctx, chat, msg.ID, "Resume a thread:", ResumeKeyboard(threads, h.now()))
		h.answer(ctx, q, "")
	case strings.HasPrefix(rest, "r:"):
		if err := h.resumeTopic(ctx, chat, strings.TrimPrefix(rest, "r:")); err != nil {
			h.log().Warn("failed to resume a thread", "chat_id", chat, "err", err)
			h.answer(ctx, q, "That thread no longer exists")
		} else {
			h.answer(ctx, q, "Resumed ✓")
		}
		h.renderLauncher(ctx, chat, msg.ID, h.defaultRole(chat))
	case rest == "back":
		h.renderLauncher(ctx, chat, msg.ID, h.defaultRole(chat))
		h.answer(ctx, q, "")
	default:
		h.answer(ctx, q, "")
		h.log().Warn("unknown launcher callback", "data", rest)
	}
}

// openModeTopic is nt:p:<name> / nt:op:<name>: a new topic on that mode.
// The mode is looked up now, so a stale button fails cleanly.
func (h *Handler) openModeTopic(ctx context.Context, q *telegram.CallbackQuery, name, role string) {
	chat := q.Message.Chat.ID
	v, err := h.Launches.LauncherView(ctx, chat, role)
	if err != nil {
		h.answer(ctx, q, truncateRunes(err.Error(), 200))
		return
	}
	if v.ConfigError != "" {
		h.answer(ctx, q, v.ConfigError)
		return
	}
	for _, m := range v.Modes {
		if m.Name == name {
			_, err := h.launchTopic(ctx, chat, m.Ref, role)
			h.answerLaunched(ctx, q, role, err)
			return
		}
	}
	h.answer(ctx, q, "Mode no longer configured")
}
