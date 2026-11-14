// Package intake turns one Telegram message, or an album, into the inputs of
// a turn: the saved attachments and notices for compose_user_text, and the
// images the model sees inline. It mirrors the attachment half of
// crates/zdx-bot/src/ingest/mod.rs (collect_attachments, extract_text and
// the load_* helpers). Telegram transport, storage, transcription and image
// preparation are injected: the Bot API client belongs to package telegram,
// and transcription and image preparation are Rust operations.
package intake

import (
	"context"
	"errors"
	"fmt"
	"log/slog"
	"strings"
	"time"

	"github.com/tallesborges/zdx/apps/gateway/internal/engine"
	"github.com/tallesborges/zdx/apps/gateway/internal/telegram"
)

// Limits of crates/zdx-bot/src/ingest/mod.rs.
const (
	// TelegramDownloadLimitBytes is the largest file getFile serves.
	TelegramDownloadLimitBytes = 20 << 20
	MaxAudioBytes              = 25 << 20
	MaxDocumentBytes           = 25 << 20
	// MaxVideoBytes is ask_media's inline cap (zdx_engine::media::MAX_INLINE_BYTES),
	// so every saved video is one the agent can inspect.
	MaxVideoBytes = 15 << 20
	// DownloadAttempts is the tries per Telegram file (getFile plus fetch).
	DownloadAttempts = 3
	// DownloadRetryDelay times n is the wait before retry n.
	DownloadRetryDelay = 2 * time.Second
)

// UnreadableAttachmentReply is the Rust bot's answer to a message whose
// attachments all failed to give any input (Result.Empty with
// HadAttachments).
const UnreadableAttachmentReply = "Sorry, I couldn't read that attachment."

// Files is the Bot API download transport. *telegram.Client has GetFile.
// Errors must be token-free and typed like the rest of package telegram:
// *telegram.TransportError for a failed exchange, *telegram.APIError for a
// refusal, with the HTTP status as Code for a failed file fetch.
// DownloadFile must not read more than maxBytes+1 bytes and must fail,
// with neither of those types, when the file is larger than maxBytes.
type Files interface {
	GetFile(ctx context.Context, fileID string) (*telegram.RemoteFile, error)
	DownloadFile(ctx context.Context, filePath string, maxBytes int64) ([]byte, error)
}

// Store saves an attachment and returns its absolute path. See DirStore.
type Store interface {
	Save(chat, message int64, fileName string, data []byte) (string, error)
}

// AudioFile is a saved audio attachment to transcribe.
type AudioFile struct {
	Path     string
	FileName string
	// MimeType is Telegram's MIME type, when it sent one.
	MimeType *string
	Data     []byte
}

// Transcriber transcribes audio with the bot's transcription settings. A nil
// transcript means none is configured. Other errors are logged and leave the
// transcript empty, as in the Rust bot; a context error stops the message.
type Transcriber interface {
	Transcribe(ctx context.Context, audio AudioFile) (*string, error)
}

// ImageFile is a saved image to prepare for the model.
type ImageFile struct {
	Path string
	Data []byte
}

// PreparedImage is how a saved image reaches the model. Inline is set
// exactly when Delivery.Type is "inline".
type PreparedImage struct {
	Delivery engine.ImageDelivery
	Inline   *engine.InputImage
}

// ImagePreparer is Rust's prepare_inline_image: a fitted inline copy, or the
// reason the model cannot see the image. An error stops the message: the
// image would otherwise vanish without the agent being told.
type ImagePreparer interface {
	PrepareImage(ctx context.Context, image ImageFile) (PreparedImage, error)
}

// DownloadUnavailable is an attachment whose download failed on a network or
// server error after every try. No turn may run without it: the caller keeps
// the message and offers to load it again, because Telegram still has the
// file.
type DownloadUnavailable struct {
	// Label is what the user sent, e.g. "voice message".
	Label string
}

func (e *DownloadUnavailable) Error() string {
	return e.Label + " could not be downloaded from Telegram"
}

// Result is the input of one turn.
type Result struct {
	// Compose is the compose_user_text request without ReplyTo, which the
	// caller sets.
	Compose engine.ComposeUserText
	// Images are the inline images of Compose.Images, in order, for Submit.
	Images []engine.InputImage
	// HadAttachments: the message carried media intake looked at.
	HadAttachments bool
}

// Empty reports a message with nothing to submit. With HadAttachments the
// caller answers UnreadableAttachmentReply; otherwise it ignores it.
func (r Result) Empty() bool {
	c := r.Compose
	return c.Text == nil && len(c.Images) == 0 && len(c.Audios) == 0 && len(c.Documents) == 0 && len(c.Videos) == 0 && len(c.Notices) == 0
}

// Collector loads the attachments of messages.
type Collector struct {
	Files       Files
	Store       Store
	Transcriber Transcriber
	Images      ImagePreparer
	Log         *slog.Logger
	// Sleep waits between download tries; nil waits on a timer.
	Sleep func(ctx context.Context, d time.Duration) error
}

func (c *Collector) log() *slog.Logger {
	if c.Log != nil {
		return c.Log
	}
	return slog.Default()
}

// noticeError is an attachment that could not be loaded, with the sentence
// the agent gets in its place.
type noticeError string

func (e noticeError) Error() string { return string(e) }

// Collect loads the attachments of head and then of each album member, in
// order, and joins their texts and captions. Errors are a context error,
// *DownloadUnavailable, or a failed image preparation; any other failure of
// one attachment becomes a notice.
func (c *Collector) Collect(ctx context.Context, head *telegram.Message, grouped []*telegram.Message) (Result, error) {
	var r Result
	r.Compose.Text = extractText(head, grouped)
	chat := head.Chat.ID
	for _, m := range members(head, grouped) {
		if m.Photo != nil {
			r.HadAttachments = true
			if p, ok := bestPhoto(m.Photo); ok {
				src := fileRef{id: p.FileID, size: p.FileSize}
				if err := c.push(ctx, &r, "photo", func() error { return c.loadImage(ctx, &r, chat, m.ID, src) }); err != nil {
					return Result{}, err
				}
			}
		}
		if d := m.Document; d != nil && d.MimeType != nil && m.Animation == nil && !strings.HasPrefix(*d.MimeType, "video/") {
			r.HadAttachments = true
			mime := *d.MimeType
			var err error
			switch {
			case strings.HasPrefix(mime, "image/"):
				err = c.push(ctx, &r, "image file", func() error { return c.loadImage(ctx, &r, chat, m.ID, refOf(d)) })
			case strings.HasPrefix(mime, "audio/"):
				err = c.push(ctx, &r, "audio file", func() error { return c.loadAudio(ctx, &r, chat, m.ID, d, d.FileName) })
			default:
				err = c.push(ctx, &r, "file", func() error { return c.loadDocument(ctx, &r, chat, m.ID, d) })
			}
			if err != nil {
				return Result{}, err
			}
		}
		if v := m.Voice; v != nil {
			r.HadAttachments = true
			if err := c.push(ctx, &r, "voice message", func() error { return c.loadAudio(ctx, &r, chat, m.ID, v, nil) }); err != nil {
				return Result{}, err
			}
		}
		if a := m.Audio; a != nil {
			r.HadAttachments = true
			if err := c.push(ctx, &r, "audio message", func() error { return c.loadAudio(ctx, &r, chat, m.ID, a, a.FileName) }); err != nil {
				return Result{}, err
			}
		}
		for _, v := range videoAttachments(m) {
			r.HadAttachments = true
			if err := c.push(ctx, &r, "video", func() error { return c.loadVideo(ctx, &r, chat, m.ID, v.kind, v.ref) }); err != nil {
				return Result{}, err
			}
		}
	}
	return r, nil
}

// push runs one load. A failure that is not a stop becomes a notice; the raw
// error never reaches the notice.
func (c *Collector) push(ctx context.Context, r *Result, label string, load func() error) error {
	err := load()
	if err == nil {
		return nil
	}
	if ctxErr := ctx.Err(); ctxErr != nil {
		return ctxErr
	}
	if errors.Is(err, context.Canceled) || errors.Is(err, context.DeadlineExceeded) {
		return err
	}
	var stop *stopError
	if errors.As(err, &stop) {
		return stop.err
	}
	c.log().Error("failed to load attachment", "label", label, "err", err)
	if isTransient(err) {
		return &DownloadUnavailable{Label: label}
	}
	var notice noticeError
	if errors.As(err, &notice) {
		r.Compose.Notices = append(r.Compose.Notices, string(notice))
	} else {
		r.Compose.Notices = append(r.Compose.Notices, fmt.Sprintf("One attachment (%s) could not be loaded from Telegram, so it is missing from this message. Tell the user and ask them to resend it.", label))
	}
	return nil
}

// stopError ends the whole message instead of becoming a notice.
type stopError struct{ err error }

func (e *stopError) Error() string { return e.err.Error() }
func (e *stopError) Unwrap() error { return e.err }

// extractText joins, for the head and then each member, the trimmed plain
// text (Text, else the flattened rich message) and the trimmed caption.
func extractText(head *telegram.Message, grouped []*telegram.Message) *string {
	var parts []string
	for _, m := range members(head, grouped) {
		for _, s := range []*string{m.PlainText(), m.Caption} {
			if s != nil {
				if t := strings.TrimSpace(*s); t != "" {
					parts = append(parts, t)
				}
			}
		}
	}
	if len(parts) == 0 {
		return nil
	}
	text := strings.Join(parts, "\n\n")
	return &text
}

// members is the head and then the album members, in arrival order.
func members(head *telegram.Message, grouped []*telegram.Message) []*telegram.Message {
	out := []*telegram.Message{head}
	for _, m := range grouped {
		if m != nil {
			out = append(out, m)
		}
	}
	return out
}

// bestPhoto is the size with the largest (file size, area), the last one on
// a tie, like Rust's max_by_key.
func bestPhoto(photos []telegram.PhotoSize) (telegram.PhotoSize, bool) {
	var best telegram.PhotoSize
	var bestSize uint64
	var bestArea uint64
	found := false
	for _, p := range photos {
		var size uint64
		if p.FileSize != nil {
			size = *p.FileSize
		}
		area := uint64(max(p.Width, 0)) * uint64(max(p.Height, 0))
		if !found || size > bestSize || (size == bestSize && area >= bestArea) {
			best, bestSize, bestArea, found = p, size, area, true
		}
	}
	return best, found
}

type fileRef struct {
	id   string
	size *uint64
	name *string
}

func refOf(f *telegram.File) fileRef {
	return fileRef{id: f.FileID, size: f.FileSize, name: f.FileName}
}

type videoRef struct {
	kind string
	ref  fileRef
}

// videoAttachments are the video media of one message, a video/* document
// included. An animation also sets document; that twin is not counted.
func videoAttachments(m *telegram.Message) []videoRef {
	var out []videoRef
	add := func(kind string, f *telegram.File) {
		if f != nil {
			out = append(out, videoRef{kind, refOf(f)})
		}
	}
	add("Video", m.Video)
	add("Video note", m.VideoNote)
	add("Animation", m.Animation)
	if d := m.Document; d != nil && m.Animation == nil && d.MimeType != nil && strings.HasPrefix(*d.MimeType, "video/") {
		add("Video file", d)
	}
	return out
}

// savedName is the sender's file name, else the Telegram path's, else the
// fallback.
func savedName(hint *string, filePath, fallback string) string {
	if hint != nil {
		if n, ok := fileNameFromPath(*hint); ok {
			return n
		}
	}
	if n, ok := fileNameFromPath(filePath); ok {
		return n
	}
	return fallback
}

func formatMiB(bytes uint64) string {
	return fmt.Sprintf("%.1f MiB", float64(bytes)/(1024*1024))
}

func (c *Collector) loadImage(ctx context.Context, r *Result, chat, message int64, src fileRef) error {
	if src.size != nil && *src.size > TelegramDownloadLimitBytes {
		return noticeError(fmt.Sprintf("An image attachment was not downloaded: it is %s and Telegram bots can only download files up to %s. Ask the user for a smaller file.", formatMiB(*src.size), formatMiB(TelegramDownloadLimitBytes)))
	}
	filePath, data, err := c.download(ctx, src.id)
	if err != nil {
		return err
	}
	name := savedName(src.name, filePath, fmt.Sprintf("image_%d.bin", message))
	path, err := c.Store.Save(chat, message, name, data)
	if err != nil {
		return err
	}
	prepared, err := c.Images.PrepareImage(ctx, ImageFile{Path: path, Data: data})
	if err != nil {
		return &stopError{fmt.Errorf("prepare image: %w", err)}
	}
	if (prepared.Delivery.Type == "inline") != (prepared.Inline != nil) || (prepared.Delivery.Type != "inline" && prepared.Delivery.Type != "saved_only") {
		return &stopError{fmt.Errorf("prepare image: inconsistent delivery %q", prepared.Delivery.Type)}
	}
	r.Compose.Images = append(r.Compose.Images, engine.ComposeImage{Path: path, Delivery: prepared.Delivery})
	if prepared.Inline != nil {
		r.Images = append(r.Images, *prepared.Inline)
	}
	return nil
}

func (c *Collector) loadDocument(ctx context.Context, r *Result, chat, message int64, d *telegram.File) error {
	if d.FileSize != nil && *d.FileSize > MaxDocumentBytes {
		c.log().Debug("skipping document exceeding max size", "chat_id", chat)
		return nil
	}
	filePath, data, err := c.download(ctx, d.FileID)
	if err != nil {
		return err
	}
	if len(data) > MaxDocumentBytes {
		c.log().Debug("downloaded document exceeds max size", "chat_id", chat)
		return nil
	}
	name := savedName(d.FileName, filePath, fmt.Sprintf("document_%d.bin", message))
	path, err := c.Store.Save(chat, message, name, data)
	if err != nil {
		return err
	}
	r.Compose.Documents = append(r.Compose.Documents, engine.ComposeDocument{FileName: name, Path: path})
	return nil
}

func (c *Collector) loadVideo(ctx context.Context, r *Result, chat, message int64, kind string, v fileRef) error {
	tooLarge := func(size uint64) error {
		c.log().Debug("skipping video exceeding ask_media limit", "chat_id", chat, "size_bytes", size)
		r.Compose.Videos = append(r.Compose.Videos, engine.ComposeVideo{Kind: kind, Status: "too_large", SizeBytes: &size})
		return nil
	}
	if v.size != nil && *v.size > MaxVideoBytes {
		return tooLarge(*v.size)
	}
	filePath, data, err := c.download(ctx, v.id)
	if err != nil {
		return err
	}
	if len(data) > MaxVideoBytes {
		return tooLarge(uint64(len(data)))
	}
	name := savedName(v.name, filePath, fmt.Sprintf("video_%d.mp4", message))
	path, err := c.Store.Save(chat, message, name, data)
	if err != nil {
		return err
	}
	r.Compose.Videos = append(r.Compose.Videos, engine.ComposeVideo{Kind: kind, Status: "saved", Path: &path})
	return nil
}

// loadAudio loads a voice message, audio message or audio document. hint is
// the sender's file name (none for voice messages).
func (c *Collector) loadAudio(ctx context.Context, r *Result, chat, message int64, f *telegram.File, hint *string) error {
	if err := ctx.Err(); err != nil {
		return err
	}
	if f.FileSize != nil && *f.FileSize > MaxAudioBytes {
		c.log().Debug("skipping audio exceeding max size", "chat_id", chat)
		return nil
	}
	filePath, data, err := c.download(ctx, f.FileID)
	if err != nil {
		return err
	}
	if len(data) > MaxAudioBytes {
		c.log().Debug("downloaded audio exceeds max size", "chat_id", chat)
		return nil
	}
	name := savedName(hint, filePath, fmt.Sprintf("audio_%d.bin", message))
	path, err := c.Store.Save(chat, message, name, data)
	if err != nil {
		return err
	}
	transcript, err := c.Transcriber.Transcribe(ctx, AudioFile{Path: path, FileName: name, MimeType: f.MimeType, Data: data})
	if err != nil {
		if ctx.Err() != nil || errors.Is(err, context.Canceled) || errors.Is(err, context.DeadlineExceeded) {
			return &stopError{err}
		}
		c.log().Error("audio transcription failed", "err", err)
		transcript = nil
	}
	r.Compose.Audios = append(r.Compose.Audios, engine.ComposeAudio{Path: path, Transcript: transcript})
	return nil
}
