package bot

import (
	"context"
	"errors"
	"fmt"
	"log/slog"
	"strconv"
	"strings"
	"sync"
	"sync/atomic"
	"time"

	"github.com/tallesborges/zdx/apps/gateway/internal/engine"
	"github.com/tallesborges/zdx/apps/gateway/internal/intake"
	"github.com/tallesborges/zdx/apps/gateway/internal/render"
	"github.com/tallesborges/zdx/apps/gateway/internal/telegram"
)

// TopicKind is how a topic's thread runs, as Rust reports it.
type TopicKind string

const (
	// TopicOrdinary is an ordinary agent thread (submit with ordinary).
	TopicOrdinary TopicKind = "ordinary"
	// TopicProfile is a persistent-profile (orchestrator) thread.
	TopicProfile TopicKind = "profile"
	// TopicWorkerMirror is a legacy worker mirror topic: messages go to the
	// worker's FIFO, never to a local turn.
	TopicWorkerMirror TopicKind = "worker_mirror"
)

// TopicInfo is Rust's authoritative view of a chat topic.
type TopicInfo struct {
	// ThreadID is the thread turns run on: the topic's thread id after one
	// alias_to hop.
	ThreadID string
	Kind     TopicKind
	// WorkerThreadID is set for TopicWorkerMirror.
	WorkerThreadID string
	// PendingTopicTitle: the first turn should title the topic.
	PendingTopicTitle bool
	// Profile is the persistent profile of a TopicProfile thread.
	Profile string
	// Exists: the conversation's thread has been written.
	Exists bool
}

// TopicResolver asks Rust what a topic's thread is. Go never reads thread
// metadata itself.
type TopicResolver interface {
	ResolveTopic(ctx context.Context, chat int64, topic *int64) (TopicInfo, error)
}

// TextComposer asks Rust for a message's user text.
type TextComposer interface {
	ComposeUserText(ctx context.Context, req engine.ComposeUserText) (string, error)
}

// Turns submits messages and follows their turns.
type Turns interface {
	SubmitFollow(ctx context.Context, s engine.Submit) (*engine.Submission, error)
	SubmitOrdinary(ctx context.Context, s engine.Submit) (*engine.Submission, error)
	SubmitRerun(ctx context.Context, s engine.Submit) (*engine.Submission, error)
	EnqueueToWorker(ctx context.Context, workerThreadID, prompt string) (*engine.WorkerSnapshot, error)
}

// Handler is the bot's message intake and button handling. Intake runs one
// message at a time per topic, only up to the submit: the Rust service owns
// the execution queue, steering and cancellation, so a running turn never
// blocks the next message of its topic.
type Handler struct {
	Telegram  *telegram.Client
	Engine    *engine.Client
	Resolver  TopicResolver
	Composer  TextComposer
	Turns     Turns
	Queues    *TopicQueues
	Presenter *Presenter
	Acks      *SteerAcks
	Artifacts ArtifactFinder
	ThreadOps ThreadOps
	// Attachments loads message files; nil refuses messages with media.
	Attachments Attachments
	// Commands runs the slash commands this package does not handle; nil
	// refuses them. See CommandHook.
	Commands CommandHook
	// CommandCallbacks answers the buttons of those commands; nil answers
	// them as unavailable.
	CommandCallbacks CallbackHook
	// Blocks refuses turns in topics whose orchestrator setup failed.
	Blocks *TopicBlocks
	// Topics opens a topic for each General message that is not a command
	// and titles new topics; nil refuses General messages.
	Topics TopicHooks
	// Replay opts followed turns in to replay (Submit.Replay, capability
	// hold_tokens): when this process goes away before delivery started,
	// the service hands the turn to the next renderer.
	Replay bool
	// Bypass picks messages that skip their topic's intake queue; nil
	// queues every message.
	Bypass QueueBypass
	// RootForChat is the project root a chat's turns submit with.
	RootForChat func(chat int64) string
	// TurnReady must pass before a turn is submitted: the capabilities its
	// rendering needs (reply parts, delivery records). A turn whose reply
	// cannot be planned and recorded is not started.
	TurnReady func(ctx context.Context) error
	// NotSentRetry bounds how long intake sends a service request again
	// that was never sent (default NotSentRetryWindow).
	NotSentRetry time.Duration
	Log          *slog.Logger

	renders    sync.WaitGroup
	standalone sync.WaitGroup
	// tasks are goroutines started for messages and buttons (notices,
	// follow-up taps, Send file, Try again).
	tasks sync.WaitGroup
	// busy counts every goroutine above that is still running.
	busy      atomic.Int64
	topics    topicMemos
	sendFiles SendFiles
}

// NotSentRetryWindow is how long intake keeps sending a service request
// that never reached the service (it is restarting) before it gives up and
// tells the user. The topic's later messages wait behind it, so order holds.
const NotSentRetryWindow = 30 * time.Second

// notSent: the request never reached the service, so sending it again
// cannot repeat anything.
func notSent(err error) bool {
	return errors.Is(err, engine.ErrRequestNotSent) && !errors.Is(err, engine.ErrRequestOutcomeUnknown)
}

// untilSent runs f again while it fails with a request that was never sent,
// for up to NotSentRetry. Any other error (a refusal, or an outcome that
// may have happened) returns at once: an ambiguous submit is never sent
// twice.
func (h *Handler) untilSent(ctx context.Context, stage string, f func() error) error {
	window := h.NotSentRetry
	if window == 0 {
		window = NotSentRetryWindow
	}
	deadline := time.Now().Add(window)
	delay := 250 * time.Millisecond
	for {
		err := f()
		if err == nil || !notSent(err) || ctx.Err() != nil {
			return err
		}
		left := time.Until(deadline)
		if left <= 0 {
			return err
		}
		h.log().Info("orchestrator service unavailable; sending again", "stage", stage, "err", err)
		select {
		case <-ctx.Done():
			return err
		case <-time.After(min(delay, left)):
		}
		delay = min(delay*2, 4*time.Second)
	}
}

// render renders a followed turn on its own goroutine and releases its hold.
func (h *Handler) render(ctx context.Context, t RoutedTurn) {
	h.renders.Add(1)
	h.busy.Add(1)
	go func() {
		defer h.renders.Done()
		defer h.busy.Add(-1)
		Render(ctx, h.Engine, h.Presenter, t, h.log())
	}()
}

// Wait blocks until every turn this handler started rendering is done.
func (h *Handler) Wait() {
	h.standalone.Wait()
	h.tasks.Wait()
	h.renders.Wait()
}

// Busy reports goroutines this handler started that are still running.
func (h *Handler) Busy() bool { return h.busy.Load() > 0 }

// task runs f on its own goroutine, counted by Wait and Busy.
func (h *Handler) task(f func()) {
	h.tasks.Add(1)
	h.busy.Add(1)
	go func() {
		defer h.tasks.Done()
		defer h.busy.Add(-1)
		f()
	}()
}

var (
	_ MessageSink  = (*Handler)(nil)
	_ CallbackSink = (*Handler)(nil)
	_ AckEditor    = (*Handler)(nil)
)

func (h *Handler) log() *slog.Logger {
	if h.Log != nil {
		return h.Log
	}
	return slog.Default()
}

// Attachments loads the attachments of a message or album for its turn
// (*intake.Collector).
type Attachments interface {
	Collect(ctx context.Context, head *telegram.Message, grouped []*telegram.Message) (intake.Result, error)
}

// members is the message and then its album members.
func (in Incoming) members() []*telegram.Message {
	out := []*telegram.Message{in.Message}
	for _, m := range in.Grouped {
		if m != nil {
			out = append(out, m)
		}
	}
	return out
}

// hasMedia: the message or an album member carries a file intake reads.
func (in Incoming) hasMedia() bool {
	for _, m := range in.members() {
		if len(m.Photo) > 0 || m.Voice != nil || m.Audio != nil || m.Document != nil || m.Video != nil || m.VideoNote != nil || m.Animation != nil {
			return true
		}
	}
	return false
}

// mediaCapabilities are the Rust operations the message's media need:
// images need Rust's image preparation, audio its transcription. Intake
// must not run without them: it would drop the image or the transcript
// without telling anyone. The checks mirror the ones in intake.Collect.
func (in Incoming) mediaCapabilities() []string {
	var images, audio bool
	for _, m := range in.members() {
		mime := ""
		if d := m.Document; d != nil && d.MimeType != nil && m.Animation == nil {
			mime = *d.MimeType
		}
		images = images || len(m.Photo) > 0 || strings.HasPrefix(mime, "image/")
		audio = audio || m.Voice != nil || m.Audio != nil || strings.HasPrefix(mime, "audio/")
	}
	var caps []string
	if images {
		caps = append(caps, engine.CapPrepareImage)
	}
	if audio {
		caps = append(caps, engine.CapTranscribeAudio)
	}
	return caps
}

// ownsCommand: text is a command this handler or its hook answers in
// General (/model set|reset here, everything else in the hook).
func (h *Handler) ownsCommand(text string) bool {
	t := strings.TrimSpace(text)
	if cmd, ok := parseCommand(t); ok && cmd.kind != "new" {
		return true
	}
	return h.Commands != nil && strings.HasPrefix(t, "/")
}

// unsupported names what this build cannot handle in a message, or "".
func (h *Handler) unsupported(in Incoming) string {
	m := in.Message
	for _, member := range in.members() {
		if len(member.PaidMedia) > 0 {
			return "paid media"
		}
	}
	media := in.hasMedia()
	text := m.PlainText()
	switch {
	case media && h.Attachments == nil:
		return "attachments"
	case h.Topics == nil && m.Chat.IsForumEnabled() && m.EffectiveThreadID() == nil && !(text != nil && !media && h.ownsCommand(*text)):
		// Commands in General go to the hook; other General messages open
		// a topic through Topics.
		return "messages in a forum's General topic"
	case !media && text != nil && strings.HasPrefix(strings.TrimSpace(*text), "/") && h.Commands == nil:
		if _, ok := parseCommand(*text); ok {
			return ""
		}
		return "commands"
	}
	return ""
}

// replyTo is the message a turn's status and reply quote: the user message,
// except for the topic's creation message (forum topics reply to it
// implicitly).
func replyTo(m *telegram.Message) *int64 {
	if t := m.EffectiveThreadID(); t != nil && *t == m.ID {
		return nil
	}
	id := m.ID
	return &id
}

// MaxRepliedToChars is the longest replied-to text kept, in characters.
const MaxRepliedToChars = 3000

// repliedTo is the message this one explicitly replies to, as the Rust
// ingest sees it: in a forum topic every message replies to the topic's
// creation message, which is not a reply. The quote wins over the replied
// message's text, then its caption; long text is cut. The wording around it
// is Rust's (compose_user_text).
func repliedTo(m *telegram.Message) *engine.ReplyTo {
	r := m.ReplyTo
	if r == nil {
		return nil
	}
	if t := m.EffectiveThreadID(); t != nil && *t == r.ID {
		return nil
	}
	var text string
	quoted := false
	switch {
	case m.Quote != nil && strings.TrimSpace(m.Quote.Text) != "":
		text, quoted = strings.TrimSpace(m.Quote.Text), true
	case r.PlainText() != nil && strings.TrimSpace(*r.PlainText()) != "":
		text = strings.TrimSpace(*r.PlainText())
	case r.Caption != nil && strings.TrimSpace(*r.Caption) != "":
		text = strings.TrimSpace(*r.Caption)
	default:
		return nil
	}
	if runes := []rune(text); len(runes) > MaxRepliedToChars {
		text = string(runes[:MaxRepliedToChars]) + "\n[…cut]"
	}
	return &engine.ReplyTo{MessageID: r.ID, Text: text, FromBot: r.From != nil && r.From.IsBot, Quoted: quoted}
}

// Dispatch implements MessageSink.
func (h *Handler) Dispatch(ctx context.Context, in Incoming) {
	m := in.Message
	topic := m.EffectiveThreadID()
	// The bypass reads the message as typed; everything after it reads
	// `/queue <text>` as <text> (Rust dispatch_message, route_steer).
	queued := queueCommand(in)
	if what := h.unsupported(queued); what != "" {
		h.task(func() {
			h.notify(ctx, m.Chat.ID, topic, replyTo(m), fmt.Sprintf("This gateway build cannot handle %s yet.", what))
		})
		return
	}
	if h.bypass(ctx, in) {
		return
	}
	in = queued
	h.Queues.Enqueue(KeyOf(m.Chat.ID, topic), func(ctx context.Context) { h.intake(ctx, in, false) })
}

func (h *Handler) notify(ctx context.Context, chat int64, topic, reply *int64, text string) {
	req := telegram.TextSend{ChatID: chat, Text: EscapeHTML(text), MessageThreadID: topic, ReplyToMessageID: reply, DisablePreview: true}
	if _, err := h.Telegram.SendText(ctx, req); err != nil {
		h.log().Warn("failed to send notice", "chat_id", chat, "err", err)
	}
}

// intake resolves the topic, loads attachments, composes the user text, and
// submits. It returns as soon as the service answered; the turn renders on
// its own goroutine. The order follows the Rust bot: worker mirror topics
// get everything verbatim before any command interpretation.
//
// btw marks a message that skipped the queue as a /btw question (bypass):
// it never runs a turn.
func (h *Handler) intake(ctx context.Context, in Incoming, btw bool) {
	m := in.Message
	chat := m.Chat.ID
	topic := m.EffectiveThreadID()
	reply := replyTo(m)
	if in.FromGeneral {
		// The original stays in General; answers quote it across topics.
		reply = nil
	}
	fail := func(stage string, err error) {
		h.log().Warn("message not submitted", "chat_id", chat, "stage", stage, "err", err)
		h.notifyHTML(ctx, chat, topic, reply, FormatUserError(err.Error()))
	}

	var info TopicInfo
	err := h.untilSent(ctx, "resolve topic", func() (err error) {
		info, err = h.Resolver.ResolveTopic(ctx, chat, topic)
		return err
	})
	if err != nil {
		fail("resolve topic", err)
		return
	}
	if h.Topics != nil && !h.initDMOrchestrator(ctx, in, &info) {
		return
	}
	h.topics.remember(KeyOf(chat, topic), info.ThreadID)
	if btw && !h.topics.sameThread(KeyOf(chat, topic), info.ThreadID) {
		// The topic now runs another thread than the one the bypass
		// matched: handle the message in order after all.
		h.Queues.Enqueue(KeyOf(chat, topic), func(ctx context.Context) { h.intake(ctx, in, false) })
		return
	}
	mirror := info.Kind == TopicWorkerMirror
	general := m.Chat.IsForumEnabled() && topic == nil
	media := in.hasMedia()
	// Text or a flattened rich message (Rust plain_text).
	plain := m.PlainText()
	if !media && !mirror {
		if plain == nil || strings.TrimSpace(*plain) == "" {
			h.log().Debug("ignoring empty message", "chat_id", chat)
			return
		}
		if text := strings.TrimSpace(*plain); h.command(ctx, in, info, text, false, reply) || h.staged(ctx, in, info, text, "", false, reply) {
			return
		}
	}
	if btw {
		// The session ended before this question ran (cancelled, or taken
		// by an earlier input). It must not become a turn.
		h.notifyHTML(ctx, chat, topic, reply, h.Bypass.StaleBtwInput())
		return
	}
	if general {
		if h.Topics == nil {
			h.notify(ctx, chat, topic, reply, "This gateway build cannot handle messages in a forum's General topic yet.")
			return
		}
		h.openTopic(ctx, in)
		return
	}
	if !mirror && h.Blocks != nil && h.Blocks.Blocked(info.ThreadID) {
		h.notify(ctx, chat, topic, reply, UnsetupTopicNotice)
		return
	}
	if !mirror {
		if err := h.untilSent(ctx, "turn capabilities", func() error { return h.TurnReady(ctx) }); err != nil {
			fail("turn capabilities", err)
			return
		}
	}
	var res intake.Result
	if media {
		if caps := in.mediaCapabilities(); len(caps) > 0 {
			if err := h.untilSent(ctx, "media capabilities", func() error { return h.Engine.RequireAll(ctx, caps...) }); err != nil {
				fail("media capabilities", err)
				return
			}
		}
		res, err = h.Attachments.Collect(ctx, m, in.Grouped)
		var unavailable *intake.DownloadUnavailable
		switch {
		case ctx.Err() != nil || errors.Is(err, context.Canceled) || errors.Is(err, context.DeadlineExceeded):
			return
		case errors.As(err, &unavailable):
			h.offerDownloadRetry(ctx, in, unavailable.Label)
			return
		case err != nil:
			fail("attachments", err)
			return
		}
		if res.Empty() {
			if res.HadAttachments {
				h.notify(ctx, chat, topic, reply, intake.UnreadableAttachmentReply)
			} else {
				h.log().Debug("ignoring empty message", "chat_id", chat)
			}
			return
		}
		// A command in a caption still runs, as in the Rust bot; a voice
		// transcript can answer a staged command.
		if !mirror {
			caption := ""
			if res.Compose.Text != nil {
				caption = *res.Compose.Text
			}
			loaded := len(res.Compose.Images) > 0 || len(res.Compose.Audios) > 0
			if caption != "" && h.command(ctx, in, info, caption, loaded, reply) {
				return
			}
			if h.staged(ctx, in, info, caption, firstTranscript(res), loaded, reply) {
				return
			}
		}
	} else {
		if plain == nil || strings.TrimSpace(*plain) == "" {
			h.log().Debug("ignoring empty message", "chat_id", chat)
			return
		}
		trimmed := strings.TrimSpace(*plain)
		res.Compose.Text = &trimmed
	}
	res.Compose.ReplyTo = repliedTo(m)
	var text string
	err = h.untilSent(ctx, "compose user text", func() (err error) {
		text, err = h.Composer.ComposeUserText(ctx, res.Compose)
		return err
	})
	if err != nil {
		fail("compose user text", err)
		return
	}
	h.log().Info("accepted message", "user_id", m.From.ID, "chat_id", chat, "topic_id", deref(topic), "message_id", m.ID, "album_members", len(in.Grouped))

	if mirror {
		h.enqueueToWorker(ctx, chat, topic, reply, info.WorkerThreadID, text)
		return
	}

	threadID := info.ThreadID
	composed := ""
	if res.Compose.Text != nil {
		composed = *res.Compose.Text
	}
	// A topic opened from General, or one waiting for its title, is named
	// after this message (its text, else its first transcript).
	if topic != nil && (in.FromGeneral || info.PendingTopicTitle) {
		if title := strings.TrimSpace(composed); title != "" {
			h.titleTopic(ctx, chat, *topic, info, title)
		} else if t := firstTranscript(res); strings.TrimSpace(t) != "" {
			h.titleTopic(ctx, chat, *topic, info, t)
		}
	}
	submit := engine.Submit{
		ThreadID: &threadID,
		Root:     h.RootForChat(chat),
		Route:    &engine.Route{Chat: chat, Topic: topic},
		Text:     text,
		Images:   res.Images,
		Delivery: h.delivery(in, threadID, composed),
		Replay:   h.Replay,
	}
	var sub *engine.Submission
	err = h.untilSent(ctx, "submit", func() (err error) {
		switch info.Kind {
		case TopicOrdinary:
			sub, err = h.Turns.SubmitOrdinary(ctx, submit)
		case TopicProfile:
			sub, err = h.Turns.SubmitFollow(ctx, submit)
		default:
			err = fmt.Errorf("unknown topic kind %q", info.Kind)
		}
		return err
	})
	if err != nil {
		fail("submit", err)
		return
	}
	if sub.InTurn {
		h.postSteerAck(ctx, chat, topic, reply, strconv.FormatUint(sub.InputID, 10))
		return
	}
	h.render(ctx, RoutedTurn{
		ThreadID:   sub.ThreadID,
		Kind:       engine.TurnKindUser,
		Route:      engine.Route{Chat: chat, Topic: topic},
		Turn:       sub.Turn,
		Origin:     origin(in, reply),
		Retryable:  info.Kind == TopicOrdinary,
		Holder:     sub.Holder,
		Replayable: submit.Replay && sub.Holder != "",
	})
}

func (h *Handler) notifyHTML(ctx context.Context, chat int64, topic, reply *int64, html string) {
	req := telegram.TextSend{ChatID: chat, Text: html, MessageThreadID: topic, ReplyToMessageID: reply, DisablePreview: true}
	if _, err := h.Telegram.SendText(ctx, req); err != nil {
		h.log().Warn("failed to send notice", "chat_id", chat, "err", err)
	}
}

func (h *Handler) enqueueToWorker(ctx context.Context, chat int64, topic, reply *int64, worker, text string) {
	var snap *engine.WorkerSnapshot
	err := h.untilSent(ctx, "queue to worker", func() (err error) {
		snap, err = h.Turns.EnqueueToWorker(ctx, worker, text)
		return err
	})
	var msg string
	switch {
	case err != nil:
		msg = "❌ Couldn't queue to the worker: " + err.Error()
	case snap == nil:
		msg = "❌ Couldn't queue to the worker: unexpected reply"
	case snap.Status == "running" || snap.QueueDepth > 1:
		msg = fmt.Sprintf("📨 Queued to the worker (%d waiting).", snap.QueueDepth)
	default:
		msg = "📨 Queued to the worker."
	}
	h.notify(ctx, chat, topic, reply, msg)
}

// postSteerAck answers a message that joined the running turn.
func (h *Handler) postSteerAck(ctx context.Context, chat int64, topic, reply *int64, id string) {
	h.Acks.Expect(id)
	msg, err := h.Telegram.SendText(ctx, telegram.TextSend{ChatID: chat, Text: AckPending, MessageThreadID: topic, ReplyToMessageID: reply, DisablePreview: true})
	if err != nil {
		h.Acks.Forget(id)
		h.log().Warn("failed to acknowledge a steer", "err", err)
		return
	}
	ack := Ack{Chat: chat, MessageID: msg.ID}
	if outcome, ok := h.Acks.Record(id, ack); ok {
		h.EditAck(ctx, ack, outcome)
	}
}

// EditAck implements AckEditor.
func (h *Handler) EditAck(ctx context.Context, ack Ack, text string) {
	if err := h.Telegram.EditMessageText(ctx, ack.Chat, ack.MessageID, text, nil); err != nil {
		h.log().Warn("failed to update a steer acknowledgement", "err", err)
	}
}

// HandleCallback implements CallbackSink. Answers go out on their own
// goroutine so the polling loop never waits on Telegram.
func (h *Handler) HandleCallback(ctx context.Context, q *telegram.CallbackQuery) {
	data := ""
	if q.Data != nil {
		data = *q.Data
	}
	answer := func(text string) {
		go func() {
			var t *string
			if text != "" {
				t = &text
			}
			if err := h.Telegram.AnswerCallbackQuery(context.WithoutCancel(ctx), q.ID, t); err != nil {
				h.log().Warn("failed to answer callback", "err", err)
			}
		}()
	}
	if tap, ok := render.ParseFollowupData(data); ok {
		h.task(func() { h.handleFollowup(context.WithoutCancel(ctx), q, tap) })
		return
	}
	if key, ok := strings.CutPrefix(data, render.SendFilePrefix); ok {
		h.task(func() { h.handleSendFile(context.WithoutCancel(ctx), q, key) })
		return
	}
	if strings.HasPrefix(data, "cancel_q:") {
		// A queued-message button from the Rust bot: this gateway queues in
		// the service and cancels through Cancel; nothing waits here.
		answer("Already processing")
		return
	}
	if key, ok := ParseCancelCallback(data); ok {
		if h.Presenter.Cancels.Cancel(key) {
			h.log().Info("cancelling turn", "chat_id", key.Chat, "id", key.ID)
			answer("Cancelling...")
		} else {
			answer("Nothing to cancel")
		}
		return
	}
	if strings.HasPrefix(data, "retry:") {
		h.task(func() { h.handleRetry(context.WithoutCancel(ctx), q, data) })
		return
	}
	if h.CommandCallbacks != nil && h.CommandCallbacks.HandleCallback(ctx, q) {
		return
	}
	for _, prefix := range []string{"cancel_q:", "wk:", "stg:", "cmd:", "nt:", "thread_header:", "model_"} {
		if strings.HasPrefix(data, prefix) {
			answer("Not available in this gateway build yet")
			return
		}
	}
	h.log().Warn("unknown callback", "user_id", q.From.ID)
	answer("")
}
