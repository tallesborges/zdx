package bot

import (
	"context"
	"strings"
	"sync"
	"unicode"

	"github.com/tallesborges/zdx/apps/gateway/internal/engine"
	"github.com/tallesborges/zdx/apps/gateway/internal/intake"
	"github.com/tallesborges/zdx/apps/gateway/internal/telegram"
)

// CommandRequest is one slash command after routing, for a CommandHook.
type CommandRequest struct {
	// Message is the user message. For an album it is the head.
	Message *telegram.Message
	// Text is the trimmed command text: the message text, or for media the
	// joined text and captions (as the Rust bot reads commands).
	Text string
	// Topic is Rust's view of the topic. Never a worker mirror: those
	// messages go to the worker before any command interpretation.
	Topic TopicInfo
	// Root is the project root the chat's turns submit with.
	Root string
	// ReplyTo is the message an answer quotes (nil for a topic's creation
	// message).
	ReplyTo *int64
	// HasLoadedImagesOrAudio: intake loaded at least one image or audio file
	// from the message or its album (intake.Result, not the raw message).
	// The Rust bot reads no command from such a message; staged input it
	// still takes.
	HasLoadedImagesOrAudio bool
	// Synthetic: a bot-made message (Incoming.Synthetic). It never starts
	// or answers a staged command.
	Synthetic bool
}

// Synthetic messages. Rule: text the bot builds itself and dispatches as a
// message (command picker taps, launcher seeds, Mini App feedback) is
// Incoming.Synthetic; it never starts or answers a staged command and never
// matches the /btw queue bypass. Follow-up taps stay non-synthetic: they are
// the user's own pick from a reply, as in the Rust bot. The Rust bot marks
// only Mini App feedback synthetic, so a command picker tap during a staged
// command differs on purpose: Rust took it as the staged input, the gateway
// runs it as a turn.

// CommandHook runs the slash commands the bot package does not handle
// itself (internal/commands). The bot keeps /new and topic /model set|reset
// and asks the hook first for everything else.
//
// HandleCommand reports whether req was a command the hook owns; it answers
// the user itself, including refusals of commands it knows but cannot run.
// A false return sends the message on to the agent as ordinary text, so the
// hook must claim every command name it knows.
//
// It runs on the topic's intake queue: the next message of the topic waits
// until it returns. Work that waits on a turn or a slow Rust operation must
// move to its own goroutine. To run a turn as if the user typed a message,
// call Handler.Dispatch with a synthetic message.
type CommandHook interface {
	HandleCommand(ctx context.Context, req CommandRequest) bool
}

// StagingHook is a CommandHook that also owns the input of staged commands
// (/btw, /handoff, prompt builder): the next plain message of the thread.
type StagingHook interface {
	CommandHook
	// StagedInput runs a message that is not a command as the input of a
	// live staged command on its thread; true means it was consumed and
	// runs no turn. transcript is the first voice transcript; synthetic
	// marks a bot-made message.
	StagedInput(ctx context.Context, req CommandRequest, transcript string, synthetic bool) bool
	// HasLiveSession reports a live staged command on threadID. Its input
	// is never steered into a running turn.
	HasLiveSession(threadID string) bool
}

// CallbackHook answers the buttons of hook-owned commands. HandleCallback
// reports whether it owns q's data; when it does, it must answer q itself
// and move any slow work off the polling loop. Buttons the bot handles
// (follow-ups, Send file, Cancel, Try again) never reach it.
type CallbackHook interface {
	HandleCallback(ctx context.Context, q *telegram.CallbackQuery) bool
}

// command runs text when it is a slash command: the bot's own commands
// first (the hook first in General, where /new opens a topic), then the
// hook. It reports whether the message was consumed. With loaded images or
// audio the bot's own commands do not run, as in the Rust bot; the hook
// decides for its commands from CommandRequest.HasLoadedImagesOrAudio.
func (h *Handler) command(ctx context.Context, in Incoming, info TopicInfo, text string, loaded bool, reply *int64) bool {
	m := in.Message
	general := m.Chat.IsForumEnabled() && m.EffectiveThreadID() == nil
	// /new in General opens a topic: the hook's.
	if cmd, ok := parseCommand(text); ok && !loaded && !(general && cmd.kind == "new" && h.Commands != nil) {
		h.runCommand(ctx, m, info, cmd, reply)
		return true
	}
	if h.Commands == nil || len(text) == 0 || text[0] != '/' {
		return false
	}
	claimed := h.Commands.HandleCommand(ctx, h.commandRequest(in, info, text, loaded, reply))
	if claimed && !loaded && !in.Synthetic && telegram.CommandMatches(text, "/btw") {
		h.topics.btwCommand(KeyOf(m.Chat.ID, m.EffectiveThreadID()), info.ThreadID, m.ID)
	}
	return claimed
}

// staged offers a plain message to a live staged command.
func (h *Handler) staged(ctx context.Context, in Incoming, info TopicInfo, text, transcript string, loaded bool, reply *int64) bool {
	s, ok := h.Commands.(StagingHook)
	if !ok {
		return false
	}
	return s.StagedInput(ctx, h.commandRequest(in, info, text, loaded, reply), transcript, in.Synthetic)
}

func (h *Handler) commandRequest(in Incoming, info TopicInfo, text string, loaded bool, reply *int64) CommandRequest {
	m := in.Message
	return CommandRequest{
		Message: m, Text: text, Topic: info, Root: h.RootForChat(m.Chat.ID), ReplyTo: reply,
		HasLoadedImagesOrAudio: loaded, Synthetic: in.Synthetic,
	}
}

// firstTranscript is the first voice transcript of a message, "" for none.
func firstTranscript(r intake.Result) string {
	for _, a := range r.Compose.Audios {
		if a.Transcript != nil {
			return *a.Transcript
		}
	}
	return ""
}

// QueueBypass decides which messages skip their topic's intake queue
// (Rust bot/queue.rs dispatch_message): commands answered while the topic
// is busy, and the question of a live /btw session, which opens a side
// topic and must not wait behind the turn it asks about.
type QueueBypass interface {
	// BypassesQueue reports a command that skips the queue.
	BypassesQueue(text string) bool
	// AwaitingBtwInput reports a live /btw session on threadID that takes
	// a message (never a synthetic one).
	AwaitingBtwInput(threadID string, synthetic bool) bool
	// StaleBtwInput answers a bypassed question whose session ended.
	StaleBtwInput() string
}

// bypass runs in off its topic's queue when QueueBypass says so. A /btw
// question is matched by the thread the topic last resolved to and only
// when it is newer than the /btw command: an older message (a late album,
// a follow-up tap on an earlier reply) keeps its place in the queue.
func (h *Handler) bypass(ctx context.Context, in Incoming) bool {
	if h.Bypass == nil {
		return false
	}
	m := in.Message
	key := KeyOf(m.Chat.ID, m.EffectiveThreadID())
	btw := false
	switch {
	case !in.hasMedia() && m.Text != nil && h.Bypass.BypassesQueue(strings.TrimSpace(*m.Text)):
	case h.topics.btwCandidate(key, m.ID, func(thread string) bool { return h.Bypass.AwaitingBtwInput(thread, in.Synthetic) }):
		btw = true
	default:
		return false
	}
	h.standalone.Add(1)
	h.busy.Add(1)
	go func() {
		defer h.standalone.Done()
		defer h.busy.Add(-1)
		h.intake(ctx, in, btw)
	}()
	return true
}

// topicMemo is what intake last learned about a topic: the thread it
// resolved to, and the newest /btw command a hook claimed there.
type topicMemo struct {
	thread string
	btw    int64
}

// topicMemos remembers topics for the queue bypass, which runs on the
// polling loop and can not ask Rust.
type topicMemos struct {
	mu sync.Mutex
	m  map[TopicKey]topicMemo
}

func (t *topicMemos) remember(key TopicKey, thread string) {
	t.mu.Lock()
	defer t.mu.Unlock()
	if t.m == nil {
		t.m = map[TopicKey]topicMemo{}
	}
	memo := t.m[key]
	if memo.thread != thread {
		memo = topicMemo{thread: thread}
	}
	t.m[key] = memo
}

func (t *topicMemos) btwCommand(key TopicKey, thread string, id int64) {
	t.mu.Lock()
	defer t.mu.Unlock()
	if memo, ok := t.m[key]; ok && memo.thread == thread && id > memo.btw {
		memo.btw = id
		t.m[key] = memo
	}
}

func (t *topicMemos) sameThread(key TopicKey, thread string) bool {
	t.mu.Lock()
	defer t.mu.Unlock()
	return t.m[key].thread == thread
}

func (t *topicMemos) btwCandidate(key TopicKey, id int64, awaiting func(thread string) bool) bool {
	t.mu.Lock()
	memo, ok := t.m[key]
	t.mu.Unlock()
	return ok && memo.btw != 0 && id > memo.btw && awaiting(memo.thread)
}

// delivery is how a message joins its thread: as a steer into the running
// turn, except for `/queue`, bot-made messages, slash text and the input of
// a staged command, which queue (the Rust bot's route_steer and
// steer_eligible).
func (h *Handler) delivery(in Incoming, threadID, text string) engine.Delivery {
	if in.ForceQueue || in.Synthetic || strings.HasPrefix(strings.TrimSpace(text), "/") {
		return engine.DeliveryQueue
	}
	if s, ok := h.Commands.(StagingHook); ok && s.HasLiveSession(threadID) {
		return engine.DeliveryQueue
	}
	return engine.DeliverySteer
}

// queueCommand turns `/queue <text>` into <text> with ForceQueue (Rust
// parse_queue_command, which reads only the typed text). Everything else
// is returned unchanged.
func queueCommand(in Incoming) Incoming {
	m := in.Message
	if m.Text == nil {
		return in
	}
	t := strings.TrimLeftFunc(*m.Text, unicode.IsSpace)
	command, rest := t, ""
	if i := strings.IndexFunc(t, unicode.IsSpace); i >= 0 {
		command, rest = t[:i], t[i:]
	}
	rest = strings.TrimSpace(rest)
	name, bot, hasBot := strings.Cut(command, "@")
	if name != "/queue" || (hasBot && bot == "") || rest == "" {
		return in
	}
	copied := *m
	copied.Text = &rest
	in.Message = &copied
	in.ForceQueue = true
	return in
}
