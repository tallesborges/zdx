package bot

import (
	"context"
	"errors"
	"fmt"
	"log/slog"
	"path/filepath"
	"strconv"
	"strings"
	"sync"

	"github.com/tallesborges/zdx/apps/gateway/internal/conversation"
	"github.com/tallesborges/zdx/apps/gateway/internal/engine"
	"github.com/tallesborges/zdx/apps/gateway/internal/render"
	"github.com/tallesborges/zdx/apps/gateway/internal/telegram"
)

// ReplyPlanner turns a turn's final text into a delivery plan. workers are
// the turn's touched workers (turn_ended.touched_workers), linked under the
// reply text.
type ReplyPlanner interface {
	PlanReply(ctx context.Context, finalText string, target ReplyTarget, threadID string, workers []string) (render.Plan, error)
}

// TextPlanner plans the reply text only. Until Rust reports reply parts
// (follow-ups, media files, artifact keys, worker links), `<media>` and
// `<followups>` tags reach the chat as escaped text.
type TextPlanner struct {
	MiniAppBase string
}

// PlanReply implements ReplyPlanner.
func (p TextPlanner) PlanReply(_ context.Context, finalText string, target ReplyTarget, threadID string, _ []string) (render.Plan, error) {
	reply := render.Reply{HTML: render.ToTelegramHTML(finalText)}
	return render.PlanReply(reply, render.Options{
		MiniAppBase:   p.MiniAppBase,
		ThreadID:      threadID,
		TopicThreadID: conversation.ThreadID(target.Chat, target.Topic),
	}), nil
}

// DeliveryStore persists delivery records through Rust.
type DeliveryStore interface {
	// DeliveryReady fails when the service cannot record deliveries at all
	// (*engine.ErrUnsupported). A turn whose reply must be recorded is not
	// started then.
	DeliveryReady(ctx context.Context) error
	// RecordDelivery persists a record and returns its user summary (set
	// when an item is not confirmed delivered).
	RecordDelivery(ctx context.Context, threadID string, hold *engine.Hold, record engine.DeliveryRecord) (string, error)
}

// DeliveryTracker marks reply deliveries in flight for the Mini App's
// `delivering` flag. Start runs before the pending record is written; the
// returned function runs after the outcome is persisted.
type DeliveryTracker interface {
	Start(threadID string) func()
}

// PhotoPreparer makes a photo fit Telegram's photo limits.
type PhotoPreparer interface {
	PreparePhoto(ctx context.Context, path string, u telegram.Upload) (telegram.Upload, error)
}

// CheckOnlyPhotos sends photos Telegram takes as is and refuses the rest
// before any request. Resizing and re-encoding stay in Rust (requested op);
// until it exists, such photos are recorded as not delivered.
type CheckOnlyPhotos struct{}

// PreparePhoto implements PhotoPreparer.
func (CheckOnlyPhotos) PreparePhoto(_ context.Context, path string, u telegram.Upload) (telegram.Upload, error) {
	if err := telegram.CheckPhoto(u); err != nil {
		if errors.Is(err, telegram.ErrPhotoNeedsPreparation) {
			return telegram.Upload{}, fmt.Errorf("prepare photo for Telegram %s: %w (photo preparation is not available in the Go gateway yet)", path, err)
		}
		return telegram.Upload{}, err
	}
	return u, nil
}

// ServicePhotos prepares reply photos with Rust prepare_photo, the rule the
// Rust bot uses. A photo Rust cannot prepare fails the item, as in the Rust
// bot; it is not sent as a document.
type ServicePhotos struct{ Engine *engine.Client }

// PreparePhoto implements PhotoPreparer.
func (s ServicePhotos) PreparePhoto(ctx context.Context, path string, u telegram.Upload) (telegram.Upload, error) {
	p, err := s.Engine.PreparePhoto(ctx, path)
	if err != nil {
		return telegram.Upload{}, fmt.Errorf("prepare photo for Telegram %s: %w", path, err)
	}
	data := u.Data
	if p.Changed {
		data = p.Data
	}
	return telegram.Upload{Data: data, FileName: p.FileName, MimeType: p.MimeType}, nil
}

// ReplyTarget is where a reply goes.
type ReplyTarget struct {
	Chat    int64
	Topic   *int64
	ReplyTo *int64
	// Quote, when set, is what the reply quotes instead of ReplyTo: a
	// message in another topic (the status message still uses ReplyTo).
	Quote *telegram.ReplyParameters
}

// Deliverer sends a final reply and records what reached the chat.
type Deliverer struct {
	Telegram  *telegram.Client
	Store     DeliveryStore
	Tracker   DeliveryTracker
	Photos    PhotoPreparer
	Followups *Followups
	Log       *slog.Logger
}

func (d *Deliverer) log() *slog.Logger {
	if d.Log != nil {
		return d.Log
	}
	return slog.Default()
}

// Deliver writes the pending record, deletes the status message, runs the
// plan (render.Deliver: ambiguous sends unknown and never resent, nothing
// attempted after an unabsorbed flood refusal, captioned-file fallback),
// and writes the outcome under the same record id. A reply without text
// then posts its follow-ups on their own.
//
// When the service cannot record deliveries at all (the capability went
// away after the turn started), nothing is sent and ErrDeliveryUnrecorded
// is returned: a reply the thread cannot know about would mislead the next
// turn.
//
// hold names the held turn and its token (hold tokens): every record
// carries them, and the first send happens only after the service
// acknowledged the pending record (Rust holds v7.2). A pending record that
// is not acknowledged after one retry with the same id stops the delivery:
// nothing is sent, the reply is lost and logged, and the caller releases.
// Without a hold any other recording failure is logged and the delivery
// goes on, as the Rust bot does.
func (d *Deliverer) Deliver(ctx context.Context, threadID string, hold *engine.Hold, target ReplyTarget, statusMessageID *int64, plan render.Plan) error {
	if len(plan.Items) == 0 {
		if hold != nil && render.FollowupKeyboard(plan.Followups) != nil {
			// A held turn sends nothing before an acknowledged pending
			// record, a follow-ups-only reply included. The record has no
			// items: it only marks delivery started, so a crash releases
			// the turn instead of replaying it.
			record := engine.NewDeliveryRecord(target.Chat, target.Topic, []engine.DeliveryItem{})
			if err := d.pending(ctx, threadID, hold, record); err != nil {
				return err
			}
		}
		d.deleteStatus(ctx, target.Chat, statusMessageID)
		d.separateFollowups(ctx, target, plan)
		return nil
	}
	end := func() {}
	if d.Tracker != nil {
		end = sync.OnceFunc(d.Tracker.Start(threadID))
	}
	defer end()
	record := engine.NewDeliveryRecord(target.Chat, target.Topic, recordItems(plan.Items))
	if err := d.pending(ctx, threadID, hold, record); err != nil {
		return err
	}

	d.deleteStatus(ctx, target.Chat, statusMessageID)

	result := render.Deliver(ctx, &sender{d: d, client: d.Telegram.Reliable(), target: target}, plan)
	applyOutcomes(&record, result)
	summary, err := d.persist(ctx, threadID, hold, record)
	var se *engine.ServiceError
	if err != nil && !errors.As(err, &se) {
		// The same id again: a record that landed is superseded by itself.
		summary, err = d.persist(ctx, threadID, hold, record)
	}
	if err != nil {
		// The sends are done and never repeated; the thread keeps the
		// pending record, which reads as not confirmed.
		d.log().Error("reply sent but its outcome is not recorded; the thread keeps the pending record", "thread", threadID, "record", record.ID, "err", err)
	}
	d.logDelivery(threadID, record)
	// The outcome is persisted; Mini App clients may stop polling.
	end()

	if result.FollowupsMessageID != nil {
		d.Followups.Register(target.Chat, *result.FollowupsMessageID, plan.Followups)
	}
	if plan.SeparateFollowups {
		d.separateFollowups(ctx, target, plan)
	}
	d.postSummary(ctx, target, summary)
	return nil
}

// pending writes a delivery's pending record before any send. A held turn
// needs the acknowledgement: no answer or a lost connection gets one retry
// with the same id, a refusal none, and without it nothing is sent (the
// reply is lost and logged). Without a hold a failure is logged and the
// delivery goes on, as in the Rust bot, unless the service cannot record
// deliveries at all.
func (d *Deliverer) pending(ctx context.Context, threadID string, hold *engine.Hold, record engine.DeliveryRecord) error {
	_, err := d.persist(ctx, threadID, hold, record)
	var se *engine.ServiceError
	if err != nil && hold != nil && !errors.As(err, &se) {
		_, err = d.persist(ctx, threadID, hold, record)
	}
	if err == nil {
		return nil
	}
	var unsupported *engine.ErrUnsupported
	if errors.As(err, &unsupported) || hold != nil {
		d.log().Error("reply not sent: its pending delivery record was not acknowledged; the reply is lost", "thread", threadID, "record", record.ID, "err", err)
		return fmt.Errorf("%w: %v", ErrDeliveryUnrecorded, err)
	}
	return nil
}

// postSummary posts the final record's problem summary once, through the
// reliable client. Its own failure is only logged, so a flood cannot
// cascade into more notices.
func (d *Deliverer) postSummary(ctx context.Context, target ReplyTarget, summary string) {
	if summary == "" {
		return
	}
	req := telegram.TextSend{ChatID: target.Chat, Text: render.EscapeHTML(summary), MessageThreadID: target.Topic}
	if _, err := d.Telegram.Reliable().SendText(ctx, req); err != nil {
		d.log().Warn("failed to send delivery summary", "chat_id", target.Chat, "err", err)
	}
}

// ErrDeliveryUnrecorded means a reply was not sent because its delivery
// could not be recorded.
var ErrDeliveryUnrecorded = errors.New("the reply was not sent: the engine service cannot record deliveries")

// recordItems maps plan items to pending delivery items, in plan order.
func recordItems(items []render.Item) []engine.DeliveryItem {
	out := make([]engine.DeliveryItem, len(items))
	for i, it := range items {
		switch it.Kind {
		case render.ItemFile:
			out[i] = engine.FileItem(it.Path)
		default:
			out[i] = engine.DeliveryItem{Kind: engine.DeliveryItemText, Status: engine.DeliveryPending}
			if it.Total > 0 {
				out[i].Part = &engine.TextPart{Index: it.Part, Total: it.Total}
			}
		}
	}
	return out
}

func applyOutcomes(record *engine.DeliveryRecord, result render.Result) {
	for i, o := range result.Items {
		item := &record.Items[i]
		item.LinkOnly = o.LinkOnly
		switch o.Status {
		case render.StatusSent:
			item.MarkSent(o.MessageID)
		case render.StatusFailed:
			item.MarkFailed(o.Error)
		case render.StatusUnknown:
			item.MarkUnknown(o.Error)
		}
	}
}

func (d *Deliverer) separateFollowups(ctx context.Context, target ReplyTarget, plan render.Plan) {
	kb := render.FollowupKeyboard(plan.Followups)
	if kb == nil {
		return
	}
	req := telegram.TextSend{ChatID: target.Chat, Text: render.SuggestedRepliesText, MessageThreadID: target.Topic, ReplyMarkup: kb}
	msg, err := d.Telegram.Paced().SendText(ctx, req)
	if err != nil {
		d.log().Warn("failed to send follow-up buttons", "chat_id", target.Chat, "err", err)
		return
	}
	d.Followups.Register(target.Chat, msg.ID, plan.Followups)
}

// persist writes a record; a failure is logged, as in the Rust bot, and the
// delivery goes on. A shutdown must not drop the outcome, so it runs
// without the caller's cancellation.
func (d *Deliverer) persist(ctx context.Context, threadID string, hold *engine.Hold, record engine.DeliveryRecord) (string, error) {
	summary, err := d.Store.RecordDelivery(context.WithoutCancel(ctx), threadID, hold, record)
	if err != nil {
		d.log().Warn("failed to record delivery outcome", "thread", threadID, "record", record.ID, "err", err)
	}
	return summary, err
}

// deleteStatus is paced, so a running flood wait delays the delete instead
// of leaving a status message with a live Cancel button.
func (d *Deliverer) deleteStatus(ctx context.Context, chat int64, id *int64) {
	if id == nil {
		return
	}
	if err := d.Telegram.Paced().DeleteMessage(ctx, chat, *id); err != nil {
		d.log().Warn("failed to delete status message", "message_id", *id, "err", err)
	}
}

// sender is render.Sender for one reply: the target, the reliable client,
// no link previews on text, and one resend without reply_to after an
// invalid reply target (text only, as in the Rust bot; files are sent with
// allow_sending_without_reply).
type sender struct {
	d      *Deliverer
	client *telegram.Client
	target ReplyTarget
}

func (s *sender) SendText(ctx context.Context, html string, kb *telegram.InlineKeyboardMarkup) (int64, error) {
	req := telegram.TextSend{ChatID: s.target.Chat, Text: html, MessageThreadID: s.target.Topic, ReplyToMessageID: s.target.ReplyTo, ReplyMarkup: kb, DisablePreview: true}
	if s.target.Quote != nil {
		req.ReplyToMessageID, req.ReplyParameters = nil, s.target.Quote
	}
	msg, err := s.client.SendText(ctx, req)
	if err != nil && req.ReplyToMessageID != nil && telegram.IsInvalidReplyError(err) {
		req.ReplyToMessageID = nil
		msg, err = s.client.SendText(ctx, req)
	}
	if err != nil {
		return 0, err
	}
	return msg.ID, nil
}

var fileMethods = map[render.Method]telegram.FileMethod{
	render.MethodDocument: telegram.MethodDocument,
	render.MethodPhoto:    telegram.MethodPhoto,
	render.MethodVoice:    telegram.MethodVoice,
	render.MethodAudio:    telegram.MethodAudio,
}

func (s *sender) SendFile(ctx context.Context, f render.File, method render.Method, caption *string, kb *telegram.InlineKeyboardMarkup) (int64, error) {
	m, ok := fileMethods[method]
	if !ok {
		return 0, fmt.Errorf("unknown send method %v", method)
	}
	upload, err := telegram.ReadUpload(f.Path, m)
	if err != nil {
		return 0, err
	}
	if method == render.MethodPhoto {
		photos := s.d.Photos
		if photos == nil {
			photos = CheckOnlyPhotos{}
		}
		if upload, err = photos.PreparePhoto(ctx, f.Path, upload); err != nil {
			return 0, err
		}
	}
	opts := telegram.MediaSend{Caption: caption, ReplyMarkup: kb, ReplyToMessageID: s.target.ReplyTo, MessageThreadID: s.target.Topic}
	if s.target.Quote != nil {
		opts.ReplyToMessageID, opts.ReplyParameters = nil, s.target.Quote
	}
	msg, err := s.client.SendFile(ctx, s.target.Chat, m, upload, opts)
	if err != nil {
		return 0, err
	}
	return msg.ID, nil
}

// logDelivery logs one line per finished delivery: record id, thread, and
// each item's outcome and message id. File names only; no paths or bodies.
func (d *Deliverer) logDelivery(threadID string, record engine.DeliveryRecord) {
	parts := make([]string, len(record.Items))
	for i, item := range record.Items {
		name := "text"
		if item.Path != nil {
			name = filepath.Base(*item.Path)
		}
		s := name + "=" + item.Status
		if item.MessageID != nil {
			s += "#" + strconv.FormatInt(*item.MessageID, 10)
		}
		parts[i] = s
	}
	attrs := []any{"record", record.ID, "thread", threadID, "chat", deref(record.ChatID), "topic", deref(record.TopicID), "items", strings.Join(parts, ",")}
	if record.HasProblems() {
		d.log().Warn("Telegram delivery incomplete", attrs...)
	} else {
		d.log().Info("Telegram delivery", attrs...)
	}
}

// deref renders an optional id for logs: the value, or nil.
func deref(v *int64) any {
	if v == nil {
		return nil
	}
	return *v
}
