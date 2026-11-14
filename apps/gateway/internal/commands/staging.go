package commands

import (
	"context"
	"fmt"
	"strings"
	"time"

	"github.com/tallesborges/zdx/apps/gateway/internal/conversation"
	"github.com/tallesborges/zdx/apps/gateway/internal/render"
	"github.com/tallesborges/zdx/apps/gateway/internal/telegram"
)

// StagingTTL is how long a staged command waits for input. A stale session
// is dropped on the next message, which then runs as a normal turn.
const StagingTTL = 15 * time.Minute

// maxSuggestionChars bounds a suggestion shown in a preview or record.
const maxSuggestionChars = 3000

type staged int

const (
	stageHandoff staged = iota
	stageBtw
	stagePromptBuilder
)

// opensTopic: completing it opens a new forum topic.
func (s staged) opensTopic() bool { return s != stagePromptBuilder }

func stagedOf(text string) (staged, bool) {
	switch c, _ := Parse(text); c {
	case CmdHandoff:
		return stageHandoff, true
	case CmdBtw:
		return stageBtw, true
	case CmdPromptBuilder:
		return stagePromptBuilder, true
	}
	return 0, false
}

// role is what a /handoff topic continues as.
type role int

const (
	roleNone role = iota
	roleAgent
	roleOrchestrator
)

func roleOf(profile string) role {
	if profile == OrchestratorProfile {
		return roleOrchestrator
	}
	return roleAgent
}

func (r role) toggled() role {
	if r == roleOrchestrator {
		return roleAgent
	}
	return roleOrchestrator
}

func (r role) profile() string {
	if r == roleOrchestrator {
		return OrchestratorProfile
	}
	return ""
}

func (r role) label() string {
	if r == roleOrchestrator {
		return "Orchestrator"
	}
	return "Agent"
}

// session is one staged command, memory-only like the Rust bot's.
type session struct {
	command staged
	// commandMessageID: messages up to it predate the command and are not
	// its input.
	commandMessageID int64
	// model is the pick from the card's `🎛 Model`; nil inherits.
	model *string
	role  role
	// suggestion is the full prompt-builder draft (the preview may be cut).
	suggestion          *string
	suggestionMessageID *int64
	botMessages         []int64
	userMessages        []int64
	created             time.Time
}

func (h *Handler) expired(s *session) bool { return h.now().Sub(s.created) > StagingTTL }

// AwaitingBtwInput reports a live /btw session on threadID. Its question
// opens a side topic, so the bot skips the topic's intake queue for it.
func (h *Handler) AwaitingBtwInput(threadID string, synthetic bool) bool {
	h.mu.Lock()
	defer h.mu.Unlock()
	s := h.sessions[threadID]
	return !synthetic && s != nil && s.command == stageBtw && !h.expired(s)
}

// HasLiveSession reports a live staged command on threadID. Its input
// belongs to the session and is never steered into a running turn.
func (h *Handler) HasLiveSession(threadID string) bool {
	h.mu.Lock()
	defer h.mu.Unlock()
	s := h.sessions[threadID]
	return s != nil && !h.expired(s)
}

// StagingCard is what the staging model picker shows for a live session.
type StagingCard struct {
	// Profile the new topic starts with ("" for an ordinary agent).
	Profile string
	// Model is the pick so far, nil when none.
	Model *string
}

// Card returns the live session on threadID that opens a topic.
func (h *Handler) Card(threadID string) (StagingCard, bool) {
	h.mu.Lock()
	defer h.mu.Unlock()
	s := h.sessions[threadID]
	if s == nil || !s.command.opensTopic() {
		return StagingCard{}, false
	}
	return StagingCard{Profile: s.role.profile(), Model: s.model}, true
}

// FinishModelPick closes the staging model picker on messageID and restores
// the card. model is the resolved pick, nil when the picker was cancelled.
// false means no session is live on threadID any more.
func (h *Handler) FinishModelPick(ctx context.Context, threadID string, chat, messageID int64, model *string) bool {
	h.mu.Lock()
	s := h.sessions[threadID]
	if s == nil {
		h.mu.Unlock()
		return false
	}
	if model != nil {
		s.model = model
	}
	command, r, picked := s.command, s.role, s.model
	h.mu.Unlock()
	h.edit(ctx, chat, messageID, askText(command, picked), stagingKeyboard(command, r))
	return true
}

func (h *Handler) available(c staged) bool {
	switch c {
	case stageBtw:
		return h.Topics != nil && h.Threads != nil && h.Seeds != nil && h.Dispatch != nil
	case stageHandoff:
		return h.Topics != nil && h.Threads != nil && h.Seeds != nil && h.Dispatch != nil && h.Generator != nil
	default:
		return h.Generator != nil && h.Dispatch != nil
	}
}

func (h *Handler) take(threadID string) *session {
	h.mu.Lock()
	defer h.mu.Unlock()
	s := h.sessions[threadID]
	delete(h.sessions, threadID)
	return s
}

func (h *Handler) put(threadID string, s *session) {
	h.mu.Lock()
	defer h.mu.Unlock()
	if h.sessions == nil {
		h.sessions = map[string]*session{}
	}
	h.sessions[threadID] = s
}

func (h *Handler) track(threadID string, messageID int64) {
	h.mu.Lock()
	defer h.mu.Unlock()
	if s := h.sessions[threadID]; s != nil {
		s.botMessages = append(s.botMessages, messageID)
	}
}

type inputKind int

const (
	notInput inputKind = iota
	expiredInput
	isInput
)

// classify decides whether the live session on threadID takes message id
// as input. It removes the session only when it expired.
func (h *Handler) classify(threadID string, id int64, synthetic bool) (inputKind, *session) {
	if synthetic {
		return notInput, nil
	}
	h.mu.Lock()
	defer h.mu.Unlock()
	s := h.sessions[threadID]
	switch {
	case s == nil:
		return notInput, nil
	case h.expired(s):
		delete(h.sessions, threadID)
		return expiredInput, s
	case id <= s.commandMessageID:
		return notInput, nil
	}
	return isInput, s
}

func (h *Handler) staging(ctx context.Context, m Message, t Thread) bool {
	if !m.Synthetic && !m.HasMedia {
		if c, ok := stagedOf(m.Text); ok {
			if !h.available(c) {
				return false
			}
			h.startStaging(ctx, m, t, c)
			return true
		}
	}
	kind, s := h.classify(t.ID, m.ID, m.Synthetic)
	switch kind {
	case notInput:
		return false
	case expiredInput:
		h.cleanup(ctx, m.Chat, s)
		return false
	}
	if strings.TrimSpace(m.Text) == "/cancel" {
		if s := h.take(t.ID); s != nil {
			s.userMessages = append(s.userMessages, m.ID)
			h.cleanup(ctx, m.Chat, s)
		}
		return true
	}
	h.stagedInput(ctx, m, t)
	return true
}

func (h *Handler) startStaging(ctx context.Context, m Message, t Thread, c staged) {
	if c.opensTopic() && (!m.IsForum || m.Topic == nil) {
		name := "/handoff"
		if c == stageBtw {
			name = "/btw"
		}
		h.send(ctx, telegram.TextSend{ChatID: m.Chat, Text: name + " needs a forum topic (a group with topics enabled).", MessageThreadID: m.Topic, ReplyToMessageID: m.ReplyTo})
		return
	}
	// A new staged command replaces the topic's previous one.
	if previous := h.take(t.ID); previous != nil {
		h.cleanup(ctx, m.Chat, previous)
	}
	r := roleNone
	if c == stageHandoff {
		r = roleOf(t.Profile)
	}
	ask, err := h.Telegram.SendText(ctx, telegram.TextSend{
		ChatID: m.Chat, Text: askText(c, nil), MessageThreadID: m.Topic, ReplyToMessageID: m.ReplyTo, ReplyMarkup: stagingKeyboard(c, r),
	})
	if err != nil {
		h.log().Warn("failed to start a staged command", "chat_id", m.Chat, "err", err)
		return
	}
	h.put(t.ID, &session{
		command: c, commandMessageID: m.ID, role: r,
		botMessages: []int64{ask.ID}, userMessages: []int64{m.ID}, created: h.now(),
	})
}

func inputText(m Message) string {
	if m.Text != "" {
		return strings.TrimSpace(m.Text)
	}
	return strings.TrimSpace(m.Transcript)
}

func (h *Handler) stagedInput(ctx context.Context, m Message, t Thread) {
	h.mu.Lock()
	s := h.sessions[t.ID]
	if s == nil {
		h.mu.Unlock()
		return
	}
	s.userMessages = append(s.userMessages, m.ID)
	s.suggestion = nil
	previous := s.suggestionMessageID
	s.suggestionMessageID = nil
	command, r := s.command, s.role
	h.mu.Unlock()
	if previous != nil {
		if err := h.delete(ctx, m.Chat, *previous); err != nil {
			h.log().Warn("failed to delete the previous staging suggestion", "err", err)
		}
	}
	reply := m.ID
	input := inputText(m)
	if input == "" {
		hint := map[staged]string{
			stageHandoff:       "Send text or a voice note to use as the handoff message, or /cancel to abort.",
			stageBtw:           "Send text or a voice note with your side question, or /cancel to abort.",
			stagePromptBuilder: "Send text or a voice note describing the prompt you want, or /cancel to abort.",
		}[command]
		if msg := h.send(ctx, telegram.TextSend{ChatID: m.Chat, Text: hint, MessageThreadID: m.Topic, ReplyToMessageID: &reply, ReplyMarkup: stagingKeyboard(command, r)}); msg != nil {
			h.track(t.ID, msg.ID)
		}
		return
	}
	switch command {
	case stageBtw:
		h.btwTopic(ctx, m, t, input)
	case stageHandoff:
		h.handoffTopic(ctx, m, t, input)
	default:
		h.buildPrompt(ctx, m, t, input)
	}
}

func (h *Handler) buildPrompt(ctx context.Context, m Message, t Thread, input string) {
	reply := m.ID
	pending := h.send(ctx, telegram.TextSend{ChatID: m.Chat, Text: "⏳ Building prompt…", MessageThreadID: m.Topic, ReplyToMessageID: &reply, ReplyMarkup: telegram.EmptyKeyboard()})
	if pending == nil {
		return
	}
	suggestion, err := h.Generator.PromptBuilder(ctx, m.Chat, t.ID, input)
	if err != nil {
		h.edit(ctx, m.Chat, pending.ID, "⚠️ Generation failed:\n<pre>"+esc(err.Error())+"</pre>\nSend another message to retry, or /cancel.", nil)
		h.track(t.ID, pending.ID)
		return
	}
	h.edit(ctx, m.Chat, pending.ID, suggestionPreview(suggestion), acceptDiscardKeyboard())
	h.mu.Lock()
	s := h.sessions[t.ID]
	if s != nil {
		s.suggestion = &suggestion
		id := pending.ID
		s.suggestionMessageID = &id
	}
	h.mu.Unlock()
	if s == nil {
		// Discarded while generating.
		_ = h.delete(ctx, m.Chat, pending.ID)
	}
}

// btwTopic opens the side topic at once: no LLM call, no Accept. On
// failure the session stays so the next message retries.
func (h *Handler) btwTopic(ctx context.Context, m Message, t Thread, question string) {
	s := h.take(t.ID)
	var model *string
	if s != nil {
		model = s.model
	}
	seed, err := h.Seeds.SideQuestionSeed(ctx, t.ID, question)
	var topic seededTopic
	if err == nil {
		topic, err = h.seedTopic(ctx, m, t.ID, "Btw", seed, btwRecord(question, sourceURL(m)), "", model)
	}
	if err == nil {
		h.send(ctx, telegram.TextSend{ChatID: m.Chat, Text: topicCreatedText("💬 <b>Side question created</b>", m.Chat, topic), MessageThreadID: m.Topic, DisablePreview: true})
		if s != nil {
			h.cleanup(ctx, m.Chat, s)
		}
		return
	}
	if s != nil {
		h.put(t.ID, s)
	}
	reply := m.ID
	text := "⚠️ Couldn't open the side topic:\n<pre>" + esc(err.Error()) + "</pre>\nSend another message to retry, or /cancel."
	if msg := h.send(ctx, telegram.TextSend{ChatID: m.Chat, Text: text, MessageThreadID: m.Topic, ReplyToMessageID: &reply, ReplyMarkup: stagingKeyboard(stageBtw, roleNone)}); msg != nil {
		h.track(t.ID, msg.ID)
	}
}

// handoffTopic generates the handoff context and opens the new topic at
// once. On failure the session stays so the next message retries.
func (h *Handler) handoffTopic(ctx context.Context, m Message, t Thread, goal string) {
	reply := m.ID
	pending := h.send(ctx, telegram.TextSend{ChatID: m.Chat, Text: "⏳ Generating handoff…", MessageThreadID: m.Topic, ReplyToMessageID: &reply, ReplyMarkup: telegram.EmptyKeyboard()})
	if pending == nil {
		return
	}
	h.track(t.ID, pending.ID)
	h.mu.Lock()
	var model *string
	r := roleNone
	if s := h.sessions[t.ID]; s != nil {
		model, r = s.model, s.role
	}
	h.mu.Unlock()
	if r == roleNone {
		r = roleOf(t.Profile)
	}
	suggestion, err := h.Generator.Handoff(ctx, m.Chat, t.ID, goal)
	var topic seededTopic
	if err == nil {
		topic, err = h.seedTopic(ctx, m, t.ID, "Handoff", suggestion, handoffRecord(suggestion, sourceURL(m)), r.profile(), model)
	}
	if err == nil {
		s := h.take(t.ID)
		h.send(ctx, telegram.TextSend{ChatID: m.Chat, Text: topicCreatedText("🔀 <b>Handoff created</b>", m.Chat, topic), MessageThreadID: m.Topic, DisablePreview: true})
		if s != nil {
			h.cleanup(ctx, m.Chat, s)
		}
		return
	}
	h.mu.Lock()
	if s := h.sessions[t.ID]; s != nil && s.role != roleNone {
		r = s.role
	}
	h.mu.Unlock()
	text := "⚠️ Handoff failed:\n<pre>" + esc(err.Error()) + "</pre>\nSend another message to retry, or /cancel."
	h.edit(ctx, m.Chat, pending.ID, text, stagingKeyboard(stageHandoff, r))
}

type seededTopic struct {
	id   int64
	name string
}

// seedTopic opens a forum topic and dispatches seed as its first message
// (Rust seed_new_topic). record, posted into the new topic first, is the
// visible trace of the seed, which runs synthetically and never shows. The
// source thread is never written.
func (h *Handler) seedTopic(ctx context.Context, m Message, source, prefix, seed, record, profile string, model *string) (seededTopic, error) {
	start, err := h.Seeds.StartModel(ctx, m.Chat, source, profile)
	if err != nil {
		return seededTopic{}, err
	}
	if model == nil {
		model = start.Pin
	}
	name := prefix + " " + h.now().UTC().Format("2006-01-02 15:04")
	if profile == OrchestratorProfile {
		name = OrchestratorTopicName(name)
	}
	topicID, err := h.Topics.CreateForumTopic(ctx, m.Chat, name)
	if err != nil {
		return seededTopic{}, fmt.Errorf("create forum topic: %w", err)
	}
	threadID := conversation.ThreadID(m.Chat, &topicID)
	if err := h.Threads.SetupThread(ctx, ThreadSetup{ThreadID: threadID, HandoffFrom: source, Profile: profile, Model: model, PendingTitle: true}); err != nil {
		// A requested role fails closed: running the default agent where an
		// orchestrator was asked for is what a handoff must not do.
		if profile != "" {
			return seededTopic{}, fmt.Errorf("record the profile on the handoff thread: %w", err)
		}
		h.log().Warn("failed to record lineage on the new thread", "chat_id", m.Chat, "topic_id", topicID, "err", err)
	}
	h.postHeader(ctx, m.Chat, topicID, threadID)
	if _, err := h.Telegram.SendText(ctx, telegram.TextSend{ChatID: m.Chat, Text: record, MessageThreadID: &topicID, DisablePreview: true}); err != nil {
		h.log().Warn("created a seeded topic but failed to post its record", "chat_id", m.Chat, "topic_id", topicID, "err", err)
	}
	h.Dispatch.Dispatch(ctx, Synthetic{
		Chat: m.Chat, ChatType: "supergroup", IsForum: true, Topic: &topicID, MessageID: topicID, UserID: m.UserID, Text: seed,
	})
	return seededTopic{id: topicID, name: name}, nil
}

// cleanup deletes a session's messages: the bot's own, then the user's
// (best effort: needs can_delete_messages, impossible in private chats).
func (h *Handler) cleanup(ctx context.Context, chat int64, s *session) {
	ids := append([]int64(nil), s.botMessages...)
	if s.suggestionMessageID != nil {
		ids = append(ids, *s.suggestionMessageID)
	}
	for _, id := range ids {
		if err := h.delete(ctx, chat, id); err != nil {
			h.log().Warn("failed to delete a staging message", "message_id", id, "err", err)
		}
	}
	for _, id := range s.userMessages {
		if err := h.delete(ctx, chat, id); err != nil {
			h.log().Debug("could not delete a user staging message", "message_id", id, "err", err)
		}
	}
}

// stagingCallback handles stg:a (accept), stg:d (discard), stg:m (model
// picker) and stg:r (handoff role). Sessions are memory-only, so a card
// from before a restart clears itself.
func (h *Handler) stagingCallback(ctx context.Context, q *telegram.CallbackQuery, data string) {
	msg := q.Message
	if msg == nil {
		h.answer(ctx, q, "No message context")
		return
	}
	chat, topic := msg.Chat.ID, msg.EffectiveThreadID()
	t, err := h.Resolver.ResolveThread(ctx, chat, topic)
	if err != nil {
		h.log().Warn("failed to resolve a staging card's thread", "chat_id", chat, "err", err)
		h.answer(ctx, q, "Couldn't read this topic; try again")
		return
	}
	switch data {
	case "m":
		h.openPicker(ctx, q, t.ID)
		return
	case "r":
		h.toggleRole(ctx, q, t.ID)
		return
	}
	s := h.take(t.ID)
	if s == nil {
		if data == "d" {
			_ = h.delete(ctx, chat, msg.ID)
			h.answer(ctx, q, "Discarded ✓")
		} else {
			h.edit(ctx, chat, msg.ID, "⚠️ Session expired — re-run the command.", nil)
			h.answer(ctx, q, "Session expired after restart — re-run the command")
		}
		return
	}
	switch data {
	case "d":
		h.cleanup(ctx, chat, s)
		h.answer(ctx, q, "Discarded ✓")
	case "a":
		if s.command != stagePromptBuilder {
			h.answer(ctx, q, "Nothing to accept")
			return
		}
		h.acceptPrompt(ctx, q, t.ID, s)
	default:
		h.answer(ctx, q, "")
		h.log().Warn("unknown staging callback", "data", data)
	}
}

// acceptPrompt runs the drafted prompt as the user's message in this topic.
// The preview stays as the turn's reply anchor.
func (h *Handler) acceptPrompt(ctx context.Context, q *telegram.CallbackQuery, threadID string, s *session) {
	msg := q.Message
	if s.suggestion == nil {
		h.answer(ctx, q, "Nothing staged to accept yet")
		h.put(threadID, s)
		return
	}
	h.edit(ctx, msg.Chat.ID, msg.ID, "▶️ Prompt accepted — running…", nil)
	s.suggestionMessageID = nil
	h.cleanup(ctx, msg.Chat.ID, s)
	h.answer(ctx, q, "Running prompt ✓")
	h.Dispatch.Dispatch(ctx, Synthetic{
		Chat: msg.Chat.ID, ChatType: syntheticChatType(msg.Chat.Type), IsForum: msg.Chat.IsForumEnabled(),
		Topic: msg.EffectiveThreadID(), MessageID: msg.ID, UserID: q.From.ID, Text: *s.suggestion,
	})
}

func (h *Handler) openPicker(ctx context.Context, q *telegram.CallbackQuery, threadID string) {
	card, live := h.Card(threadID)
	if !live {
		h.answer(ctx, q, "Session expired — re-run the command")
		return
	}
	if h.Models == nil {
		h.answer(ctx, q, "Not available in this gateway build yet")
		return
	}
	h.openStagingPicker(ctx, q, threadID, card)
}

func (h *Handler) toggleRole(ctx context.Context, q *telegram.CallbackQuery, threadID string) {
	h.mu.Lock()
	r := roleNone
	if s := h.sessions[threadID]; s != nil && s.role != roleNone {
		s.role = s.role.toggled()
		r = s.role
	}
	h.mu.Unlock()
	if r == roleNone {
		h.answer(ctx, q, "Session expired — re-run the command")
		return
	}
	if err := h.Telegram.EditMessageReplyMarkup(ctx, q.Message.Chat.ID, q.Message.ID, stagingKeyboard(stageHandoff, r)); err != nil {
		h.log().Warn("failed to update the handoff role button", "err", err)
	}
	h.answer(ctx, q, "The new topic continues as "+r.label())
}

func acceptDiscardKeyboard() *telegram.InlineKeyboardMarkup {
	return &telegram.InlineKeyboardMarkup{InlineKeyboard: [][]telegram.InlineKeyboardButton{{
		telegram.CallbackButton("✅ Accept", "stg:a"),
		telegram.CallbackButton("🗑 Discard", "stg:d"),
	}}}
}

// stagingKeyboard is a staged command's card. Topic-opening commands offer
// the new topic's model; /handoff also offers its role.
func stagingKeyboard(c staged, r role) *telegram.InlineKeyboardMarkup {
	discard := telegram.CallbackButton("🗑 Discard", "stg:d")
	if !c.opensTopic() {
		return &telegram.InlineKeyboardMarkup{InlineKeyboard: [][]telegram.InlineKeyboardButton{{discard}}}
	}
	model := telegram.CallbackButton("🎛 Model", "stg:m")
	if r != roleNone {
		return &telegram.InlineKeyboardMarkup{InlineKeyboard: [][]telegram.InlineKeyboardButton{
			{model, telegram.CallbackButton("Continue as: "+r.label(), "stg:r")},
			{discard},
		}}
	}
	return &telegram.InlineKeyboardMarkup{InlineKeyboard: [][]telegram.InlineKeyboardButton{{model, discard}}}
}

func askText(c staged, model *string) string {
	base := map[staged]string{
		stageHandoff:       "🔀 <b>Handoff</b>\nSend the message (text or voice) to start the new topic with — I'll generate the context and open it straight away. Send /cancel to abort.",
		stageBtw:           "💬 <b>Side question</b>\nSend your question (text or voice) — I'll answer it in a new topic that reads this thread, leaving this one untouched. Send /cancel to abort.",
		stagePromptBuilder: "🛠 <b>Prompt builder</b>\nSend your intent (text or voice) — I'll draft a prompt you can accept to run here, or discard. Send /cancel to abort.",
	}[c]
	if model != nil && c.opensTopic() {
		return base + "\n\nNew topic model: <code>" + esc(*model) + "</code>"
	}
	return base
}

func suggestionPreview(suggestion string) string {
	html := render.TruncateHTML(render.ToTelegramHTML(suggestion), maxSuggestionChars)
	return "🛠 <b>Prompt preview</b>\n\n" + html + "\n\n<i>Accept runs this prompt here. Send another message to regenerate.</i>"
}

// lines splits like Rust str::lines: no empty last line after a final
// newline, and a trailing \r is dropped.
func lines(s string) []string {
	if s == "" {
		return nil
	}
	out := strings.Split(strings.TrimSuffix(s, "\n"), "\n")
	for i, l := range out {
		out[i] = strings.TrimSuffix(l, "\r")
	}
	return out
}

func handoffRecord(suggestion, source string) string {
	ls := lines(suggestion)
	for i, l := range ls {
		ls[i] = "> " + l
	}
	html := render.TruncateHTML(render.ToTelegramHTML(strings.Join(ls, "\n")), maxSuggestionChars)
	return "🔀 <b>Handoff context</b>\n" + html + backLink(source)
}

func btwRecord(question, source string) string {
	quoted := render.TruncateHTML("<blockquote>"+esc(question)+"</blockquote>", maxSuggestionChars)
	return "💬 <b>Side question</b>\n" + quoted + backLink(source)
}

func backLink(source string) string {
	if source == "" {
		return ""
	}
	return "\n↩ <a href=\"" + source + "\">Back to the source topic</a>"
}

func sourceURL(m Message) string {
	if m.Topic == nil {
		return ""
	}
	url, _ := TopicLink(m.Chat, *m.Topic)
	return url
}

func topicCreatedText(heading string, chat int64, t seededTopic) string {
	name := esc(t.name)
	if url, ok := TopicLink(chat, t.id); ok {
		return heading + " → <a href=\"" + url + "\">" + name + "</a>"
	}
	return heading + " → " + name
}
