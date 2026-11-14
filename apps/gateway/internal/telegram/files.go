package telegram

import (
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"image"
	"image/jpeg"
	"mime/multipart"
	"net/http"
	"net/textproto"
	"os"
	"path/filepath"
	"strconv"
	"strings"
)

// Bot API upload limits.
const (
	MaxDocumentBytes = 50 << 20
	MaxPhotoBytes    = 10 << 20
	// MaxPhotoLongEdge and MaxPhotoAspectRatio are the bot's photo
	// policy (crates/zdx-bot/src/telegram/mod.rs): larger photos are
	// resized, more extreme ones cropped.
	MaxPhotoLongEdge    = 1920
	MaxPhotoAspectRatio = 20
)

// FileMethod is how a file is sent.
type FileMethod struct {
	Method string // sendDocument, sendPhoto, sendVoice, sendAudio
	Field  string // document, photo, voice, audio
	// DefaultName and DefaultMime apply when the path has no name or the
	// content type is not recognised.
	DefaultName string
	DefaultMime string
	// FixedMime always applies (voice notes are OGG/Opus).
	FixedMime string
}

var (
	MethodDocument = FileMethod{Method: "sendDocument", Field: "document", DefaultName: "document.bin", DefaultMime: "application/octet-stream"}
	MethodPhoto    = FileMethod{Method: "sendPhoto", Field: "photo", DefaultName: "photo.png", DefaultMime: "application/octet-stream"}
	MethodVoice    = FileMethod{Method: "sendVoice", Field: "voice", DefaultName: "voice.ogg", FixedMime: "audio/ogg"}
	MethodAudio    = FileMethod{Method: "sendAudio", Field: "audio", DefaultName: "audio.mp3", DefaultMime: "audio/mpeg"}
)

// Upload is one file's bytes.
type Upload struct {
	Data     []byte
	FileName string
	MimeType string
}

// MediaSend are the options of a file send.
type MediaSend struct {
	// Caption is HTML; on a parse refusal it is repaired, then sent plain.
	Caption          *string
	ReplyMarkup      *InlineKeyboardMarkup
	ReplyToMessageID *int64
	MessageThreadID  *int64
	ReplyParameters  *ReplyParameters
}

// ErrPhotoNeedsPreparation means a photo is outside Telegram's photo
// limits or not JPEG, so it must be re-encoded before sendPhoto. Nothing
// was sent.
var ErrPhotoNeedsPreparation = errors.New("photo needs resizing or re-encoding before Telegram accepts it")

// ReadUpload reads a file for upload. Files over the document limit are
// refused before they are read.
func ReadUpload(path string, m FileMethod) (Upload, error) {
	info, err := os.Stat(path)
	if err != nil {
		return Upload{}, fmt.Errorf("read %s file %s: %w", m.Field, path, err)
	}
	if info.Size() > MaxDocumentBytes {
		return Upload{}, fmt.Errorf("%s exceeds Telegram limit (%d bytes > %d bytes)", m.Field, info.Size(), MaxDocumentBytes)
	}
	data, err := os.ReadFile(path)
	if err != nil {
		return Upload{}, fmt.Errorf("read %s file %s: %w", m.Field, path, err)
	}
	name := filepath.Base(path)
	if name == "." || name == "/" || name == "" {
		name = m.DefaultName
	}
	mime := m.FixedMime
	if mime == "" {
		mime = sniffMime(data, m.DefaultMime)
	}
	return Upload{Data: data, FileName: name, MimeType: mime}, nil
}

// sniffMime recognises formats by their bytes, close to Rust's `infer`
// crate: binary signatures, and of text only HTML, XML and shell scripts.
// Plain text is not recognised and gets the fallback.
func sniffMime(data []byte, fallback string) string {
	if bytes.HasPrefix(data, []byte("#!")) {
		return "text/x-shellscript"
	}
	mime := http.DetectContentType(data)
	if i := strings.IndexByte(mime, ';'); i >= 0 {
		mime = mime[:i]
	}
	switch {
	case mime == "application/octet-stream", mime == "text/plain":
		return fallback
	}
	return mime
}

// CheckPhoto passes a photo Telegram accepts as is: JPEG, at most
// MaxPhotoBytes, long edge at most MaxPhotoLongEdge, aspect ratio at most
// MaxPhotoAspectRatio. Anything else is ErrPhotoNeedsPreparation. The
// resize and re-encode itself is not done in Go.
func CheckPhoto(u Upload) error {
	if u.MimeType != "image/jpeg" || len(u.Data) > MaxPhotoBytes {
		return ErrPhotoNeedsPreparation
	}
	cfg, err := jpeg.DecodeConfig(bytes.NewReader(u.Data))
	if err != nil {
		return ErrPhotoNeedsPreparation
	}
	return checkDimensions(cfg)
}

func checkDimensions(cfg image.Config) error {
	w, h := cfg.Width, cfg.Height
	short, long := min(w, h), max(w, h)
	if short == 0 || long > MaxPhotoLongEdge || long > short*MaxPhotoAspectRatio {
		return ErrPhotoNeedsPreparation
	}
	return nil
}

// SendFile uploads a file. With a caption it sends HTML and, on a parse
// refusal, the repaired caption, then the caption as plain text; each is a
// new request because the earlier one was refused. Oversized files are
// refused before any request.
func (c *Client) SendFile(ctx context.Context, chatID int64, m FileMethod, file Upload, opts MediaSend) (*Message, error) {
	if len(file.Data) > MaxDocumentBytes {
		return nil, fmt.Errorf("%s exceeds Telegram limit (%d bytes > %d bytes)", m.Field, len(file.Data), MaxDocumentBytes)
	}
	var caption string
	if opts.Caption != nil {
		caption = strings.TrimSpace(*opts.Caption)
	}
	if caption == "" {
		return c.sendFileRaw(ctx, chatID, m, file, opts, nil, false)
	}
	msg, err := c.sendFileRaw(ctx, chatID, m, file, opts, &caption, true)
	if err == nil || !IsHTMLParseError(err) {
		return msg, err
	}
	if c.Repair != nil {
		if repaired := c.Repair(caption); repaired != caption {
			msg, err := c.sendFileRaw(ctx, chatID, m, file, opts, &repaired, true)
			if err == nil || !IsHTMLParseError(err) {
				return msg, err
			}
		}
	}
	return c.sendFileRaw(ctx, chatID, m, file, opts, &caption, false)
}

func (c *Client) sendFileRaw(ctx context.Context, chatID int64, m FileMethod, file Upload, opts MediaSend, caption *string, html bool) (*Message, error) {
	spec := newMessageSpec(m.Method, chatID, opts.MessageThreadID)
	url := c.url(m.Method)
	return call[*Message](ctx, c, spec, func() (*http.Request, error) {
		var buf bytes.Buffer
		w := multipart.NewWriter(&buf)
		_ = w.WriteField("chat_id", strconv.FormatInt(chatID, 10))
		header := textproto.MIMEHeader{}
		header.Set("Content-Disposition", fmt.Sprintf(`form-data; name=%q; filename=%q`, m.Field, file.FileName))
		header.Set("Content-Type", file.MimeType)
		part, err := w.CreatePart(header)
		if err != nil {
			return nil, err
		}
		if _, err := part.Write(file.Data); err != nil {
			return nil, err
		}
		if caption != nil {
			_ = w.WriteField("caption", *caption)
			if html {
				_ = w.WriteField("parse_mode", ParseModeHTML)
			}
		}
		if opts.ReplyMarkup != nil {
			b, err := json.Marshal(opts.ReplyMarkup)
			if err != nil {
				return nil, err
			}
			_ = w.WriteField("reply_markup", string(b))
		}
		if opts.ReplyToMessageID != nil {
			_ = w.WriteField("reply_to_message_id", strconv.FormatInt(*opts.ReplyToMessageID, 10))
			_ = w.WriteField("allow_sending_without_reply", "true")
		}
		if opts.MessageThreadID != nil {
			_ = w.WriteField("message_thread_id", strconv.FormatInt(*opts.MessageThreadID, 10))
		}
		if opts.ReplyParameters != nil {
			b, err := json.Marshal(opts.ReplyParameters)
			if err != nil {
				return nil, err
			}
			_ = w.WriteField("reply_parameters", string(b))
		}
		if err := w.Close(); err != nil {
			return nil, err
		}
		req, err := http.NewRequest(http.MethodPost, url, &buf)
		if err != nil {
			return nil, err
		}
		req.Header.Set("Content-Type", w.FormDataContentType())
		return req, nil
	})
}

// SendDocument uploads a document.
func (c *Client) SendDocument(ctx context.Context, chatID int64, file Upload, opts MediaSend) (*Message, error) {
	return c.SendFile(ctx, chatID, MethodDocument, file, opts)
}
