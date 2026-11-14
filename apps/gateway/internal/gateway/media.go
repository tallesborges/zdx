package gateway

import (
	"context"
	"log/slog"
	"path/filepath"

	"github.com/tallesborges/zdx/apps/gateway/internal/bot"
	"github.com/tallesborges/zdx/apps/gateway/internal/engine"
	"github.com/tallesborges/zdx/apps/gateway/internal/intake"
	"github.com/tallesborges/zdx/apps/gateway/internal/sandbox"
	"github.com/tallesborges/zdx/apps/gateway/internal/telegram"
)

// ServiceAttachments loads message files through the Bot API, saves them
// under home/telegram/<chat>/<message>_<name> (the Rust bot's layout, a
// write Go owns), and asks Rust to prepare images (prepare_image) and
// transcribe audio with the chat root's config (transcribe_audio).
func ServiceAttachments(tg *telegram.Client, eng *engine.Client, home string, rootForChat func(int64) string, log *slog.Logger) bot.Attachments {
	return serviceAttachments{tg: tg, eng: eng, store: intake.DirStore{Root: filepath.Join(home, "telegram")}, rootForChat: rootForChat, log: log}
}

type serviceAttachments struct {
	tg          *telegram.Client
	eng         *engine.Client
	store       intake.DirStore
	rootForChat func(int64) string
	log         *slog.Logger
}

// Collect implements bot.Attachments. Transcription uses the root of the
// message's chat, so the collector is built per message.
func (a serviceAttachments) Collect(ctx context.Context, head *telegram.Message, grouped []*telegram.Message) (intake.Result, error) {
	if err := sandbox.CheckHome(a.store.Root); err != nil {
		return intake.Result{}, err
	}
	media := serviceMedia{eng: a.eng, root: a.rootForChat(head.Chat.ID)}
	c := &intake.Collector{Files: a.tg, Store: a.store, Transcriber: media, Images: media, Log: a.log}
	return c.Collect(ctx, head, grouped)
}

// serviceMedia is Rust's image preparation and transcription.
type serviceMedia struct {
	eng  *engine.Client
	root string
}

func (m serviceMedia) Transcribe(ctx context.Context, audio intake.AudioFile) (*string, error) {
	return m.eng.TranscribeAudio(ctx, engine.TranscribeAudio{Path: audio.Path, FileName: audio.FileName, MimeType: audio.MimeType, Root: m.root})
}

func (m serviceMedia) PrepareImage(ctx context.Context, image intake.ImageFile) (intake.PreparedImage, error) {
	p, err := m.eng.PrepareImage(ctx, image.Path)
	if err != nil {
		return intake.PreparedImage{}, err
	}
	return intake.PreparedImage{Delivery: p.Delivery, Inline: p.Image}, nil
}
