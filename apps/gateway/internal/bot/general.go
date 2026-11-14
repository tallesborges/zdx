package bot

import (
	"context"
	"strings"
	"time"
	"unicode"

	"github.com/tallesborges/zdx/apps/gateway/internal/conversation"
	"github.com/tallesborges/zdx/apps/gateway/internal/engine"
	"github.com/tallesborges/zdx/apps/gateway/internal/telegram"
)

// TopicHooks are what opening a topic from General needs from the commands
// package (Rust bot/queue.rs dispatch_message, topic_title.rs).
type TopicHooks interface {
	// OrchestratorChat: General messages of this chat open orchestrator
	// home bases (telegram.profiles.<name>.orchestrator).
	OrchestratorChat(chat int64) bool
	// OrchestratorTopicName marks a topic name as an orchestrator's.
	OrchestratorTopicName(name string) string
	// PostThreadHeader posts and pins a new topic's header card.
	PostThreadHeader(ctx context.Context, chat, topic int64, threadID string) error
	// ScheduleLauncherRepost keeps the launcher the last message in General.
	ScheduleLauncherRepost(ctx context.Context, chat int64)
	// IsCommand reports a built-in command (Rust parse_command).
	IsCommand(text string) bool
}

// initDMOrchestrator makes a new Threaded Mode thread in the bot's private
// chat an orchestrator home base, as a General message's topic is (Rust
// init_dm_orchestrator_thread): first sighting only, never for a built-in
// command. It reports whether the message may run; without the profile it
// would run the default coding agent instead.
func (h *Handler) initDMOrchestrator(ctx context.Context, in Incoming, info *TopicInfo) bool {
	m := in.Message
	topic := m.EffectiveThreadID()
	if !m.Chat.IsPrivate() || topic == nil || info.Exists || info.Kind != TopicOrdinary || (m.Text != nil && h.Topics.IsCommand(*m.Text)) {
		return true
	}
	chat := m.Chat.ID
	if err := h.markOrchestrator(ctx, chat, info.ThreadID, true); err != nil {
		h.log().Error("failed to mark the new DM thread as orchestrator; not running the turn", "thread", info.ThreadID, "err", err)
		h.notify(ctx, chat, topic, nil, OrchestratorInitFailedNotice)
		return false
	}
	if err := h.Topics.PostThreadHeader(ctx, chat, *topic, info.ThreadID); err != nil {
		h.log().Warn("failed to post the header of a new DM orchestrator thread", "thread", info.ThreadID, "err", err)
	}
	resolved, err := h.Resolver.ResolveTopic(ctx, chat, topic)
	if err != nil {
		h.log().Warn("message not submitted", "chat_id", chat, "stage", "resolve topic", "err", err)
		h.notifyHTML(ctx, chat, topic, nil, FormatUserError(err.Error()))
		return false
	}
	*info = resolved
	return true
}

// Notices of a General message that could not open its topic (Rust).
const (
	TopicCreateFailedNotice      = "⚠️ I couldn't create a new topic for this message, so I didn't answer in General. Please try again."
	OrchestratorInitFailedNotice = "⚠️ I couldn't initialize this thread as an orchestrator, so I didn't run your message. Please try again."
)

// maxTopicNameChars is Rust's MAX_TOPIC_NAME_LEN.
const maxTopicNameChars = 64

// TopicName is the name of a topic opened for a General message: its first
// line, cut at a word past half the limit, else a dated name (Rust
// generate_topic_name).
func TopicName(text string, now time.Time) string {
	trimmed := strings.TrimSpace(text)
	if trimmed == "" {
		return now.UTC().Format("Chat 2006-01-02 15:04")
	}
	first, _, _ := strings.Cut(trimmed, "\n")
	first = strings.TrimSuffix(first, "\r")
	runes := []rune(first)
	if len(runes) <= maxTopicNameChars {
		return first
	}
	cut := string(runes[:maxTopicNameChars])
	if i := strings.LastIndex(cut, " "); i > maxTopicNameChars/2 {
		return cut[:i] + "…"
	}
	return strings.TrimRightFunc(cut, unicode.IsSpace) + "…"
}

// openTopic moves a General message into a topic of its own, as the Rust
// bot does: create the topic (orchestrator-marked in orchestrator chats,
// fail closed when the profile cannot be stored), post its header, forward
// the original (and its album) into it, then run it there, quoted across
// topics, and repost the launcher.
func (h *Handler) openTopic(ctx context.Context, in Incoming) {
	m := in.Message
	chat := m.Chat.ID
	orchestrator := h.Topics.OrchestratorChat(chat)
	text := ""
	if p := m.PlainText(); p != nil {
		text = *p
	}
	name := TopicName(text, time.Now())
	if orchestrator {
		name = h.Topics.OrchestratorTopicName(name)
	}
	topicID, err := h.Telegram.CreateForumTopic(ctx, chat, name)
	if err != nil {
		h.log().Error("failed to create topic", "chat_id", chat, "err", err)
		h.notify(ctx, chat, nil, &m.ID, TopicCreateFailedNotice)
		return
	}
	h.log().Info("created topic", "chat_id", chat, "topic_id", topicID)
	topic := topicID
	threadID := conversation.ThreadID(chat, &topic)
	if orchestrator {
		if err := h.markOrchestrator(ctx, chat, threadID, false); err != nil {
			h.log().Error("failed to mark the new topic as orchestrator; not running the turn", "thread", threadID, "err", err)
			// Fail closed for later messages too: without the profile the
			// topic would run the default coding agent.
			if h.Blocks != nil {
				if err := h.Blocks.BlockTopic(ctx, threadID); err != nil {
					h.log().Error("failed to block the orchestrator topic whose setup failed", "thread", threadID, "err", err)
				}
			}
			h.notify(ctx, chat, &topic, nil, OrchestratorInitFailedNotice)
			return
		}
	}
	if err := h.Topics.PostThreadHeader(ctx, chat, topicID, threadID); err != nil {
		h.log().Warn("created topic but failed to post its header", "thread", threadID, "err", err)
	}
	for _, member := range in.members() {
		if err := h.Telegram.ForwardMessage(ctx, chat, chat, member.ID, &topic); err != nil {
			h.log().Warn("failed to forward the General message into its topic", "chat_id", chat, "message_id", member.ID, "err", err)
		}
	}
	routed := in
	routed.Message = inTopic(m, topicID)
	routed.Grouped = nil
	for _, g := range in.Grouped {
		routed.Grouped = append(routed.Grouped, inTopic(g, topicID))
	}
	routed.FromGeneral = true
	h.Queues.Enqueue(KeyOf(chat, &topic), func(ctx context.Context) { h.intake(ctx, routed, false) })
	h.Topics.ScheduleLauncherRepost(ctx, chat)
}

// inTopic is a copy of m placed in topic.
func inTopic(m *telegram.Message, topic int64) *telegram.Message {
	c := *m
	c.ThreadID = &topic
	c.IsTopicMessage = true
	return &c
}

// markOrchestrator stores the orchestrator profile on a new thread. Rust
// checks the profile against the chat's root (config_root) and stores no
// root: the first turn records the root it is submitted with, as in the
// Rust bot. pendingTitle marks a thread whose first message titles it.
func (h *Handler) markOrchestrator(ctx context.Context, chat int64, threadID string, pendingTitle bool) error {
	profile, root := OrchestratorProfile, h.RootForChat(chat)
	u := engine.UpdateThread{ThreadID: threadID, PersistentProfile: &profile, ConfigRoot: &root}
	if pendingTitle {
		u.PendingTopicTitle = &pendingTitle
	}
	_, err := h.ThreadOps.UpdateThread(ctx, u)
	return err
}

// OrchestratorProfile is the reserved profile of orchestrator threads.
const OrchestratorProfile = "orchestrator"

// titleTopic names a topic after its first message, off the intake queue
// (Rust spawn_topic_title_update): Rust generates the title in the chat's
// root; on a successful rename the thread stores it. An orchestrator
// topic's name keeps its mark; the stored title does not.
func (h *Handler) titleTopic(ctx context.Context, chat, topic int64, info TopicInfo, text string) {
	if info.PendingTopicTitle {
		no := false
		if _, err := h.ThreadOps.UpdateThread(ctx, engine.UpdateThread{ThreadID: info.ThreadID, PendingTopicTitle: &no}); err != nil {
			h.log().Warn("failed to clear the pending topic title", "thread", info.ThreadID, "err", err)
			return
		}
	}
	threadID := info.ThreadID
	root := h.RootForChat(chat)
	h.task(func() {
		ctx := context.WithoutCancel(ctx)
		title, err := h.Engine.Generate(ctx, engine.Generate{Kind: engine.GenerateTitle, Root: root, Text: text, ThreadID: &threadID})
		if err != nil {
			h.log().Error("topic title generation failed", "topic_id", topic, "err", err)
			return
		}
		name := title
		if info.Kind == TopicProfile && info.Profile == OrchestratorProfile && h.Topics != nil {
			name = h.Topics.OrchestratorTopicName(title)
		}
		if err := h.Telegram.EditForumTopic(ctx, chat, topic, name); err != nil {
			h.log().Error("failed to rename topic", "topic_id", topic, "err", err)
			return
		}
		if _, err := h.ThreadOps.UpdateThread(ctx, engine.UpdateThread{ThreadID: threadID, Title: &title}); err != nil {
			h.log().Warn("renamed topic but failed to store the thread title", "thread", threadID, "err", err)
		}
	})
}
