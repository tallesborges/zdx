package bot

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"path/filepath"
	"strings"
	"sync"
	"time"

	"github.com/tallesborges/zdx/apps/gateway/internal/engine"
	"github.com/tallesborges/zdx/apps/gateway/internal/telegram"
)

// ArtifactKeyLen is zdx_engine::core::artifacts::ARTIFACT_KEY_LEN.
const ArtifactKeyLen = 16

// StaleNoticeWindow bounds the "tap again" notices: one per card.
const StaleNoticeWindow = 10 * time.Minute

const maxReasonChars = 200

// ArtifactFinder resolves a file card's artifact key in a thread's artifact
// list, as the Mini App does. The error text is shown to the user.
type ArtifactFinder interface {
	FindArtifact(ctx context.Context, threadID, key string) (path, name string, err error)
}

// UnavailableArtifacts refuses every lookup until Rust exposes the artifact
// list (capability thread_views).
type UnavailableArtifacts struct{}

func (UnavailableArtifacts) FindArtifact(context.Context, string, string) (string, string, error) {
	return "", "", errors.New("this gateway build cannot look up the chat's files yet")
}

// ServiceArtifacts finds file cards' files through Rust's artifact views
// (capability artifact_views), with the Rust bot's checks and wording
// (file_card.rs resolve).
type ServiceArtifacts struct{ Engine *engine.Client }

func (a ServiceArtifacts) FindArtifact(ctx context.Context, threadID, key string) (string, string, error) {
	list, err := a.Engine.View(ctx, engine.View{Op: engine.ViewArtifactsList, ThreadID: &threadID})
	if err != nil {
		var unsupported *engine.ErrUnsupported
		if errors.As(err, &unsupported) {
			return "", "", errors.New("this gateway build cannot look up the chat's files yet")
		}
		return "", "", errors.New("the file list of this chat could not be read")
	}
	var body struct {
		Artifacts []struct {
			Key    string `json:"key"`
			Name   string `json:"name"`
			Path   string `json:"path"`
			Exists bool   `json:"exists"`
		} `json:"artifacts"`
	}
	if err := json.Unmarshal(list.Body, &body); err != nil {
		return "", "", errors.New("the file list of this chat could not be read")
	}
	for _, row := range body.Artifacts {
		if row.Key != key {
			continue
		}
		gone := fmt.Errorf("%s is no longer on the Mac", row.Name)
		if !row.Exists {
			return "", "", gone
		}
		path := row.Path
		out, err := a.Engine.View(ctx, engine.View{Op: engine.ViewArtifactResolve, ThreadID: &threadID, Path: &path})
		if err != nil {
			return "", "", gone
		}
		var resolved struct {
			Path string `json:"path"`
		}
		if json.Unmarshal(out.Body, &resolved) != nil || !filepath.IsAbs(resolved.Path) {
			return "", "", gone
		}
		return resolved.Path, row.Name, nil
	}
	return "", "", errors.New("this file is no longer part of this chat")
}

// SendFiles is the Send file state: cards being sent, and when each card
// last got a stale-tap notice.
type SendFiles struct {
	mu       sync.Mutex
	inFlight map[[2]int64]bool
	notices  map[[2]int64]time.Time
}

func (s *SendFiles) begin(card [2]int64) bool {
	s.mu.Lock()
	defer s.mu.Unlock()
	if s.inFlight == nil {
		s.inFlight = map[[2]int64]bool{}
	}
	if s.inFlight[card] {
		return false
	}
	s.inFlight[card] = true
	return true
}

func (s *SendFiles) finish(card [2]int64) {
	s.mu.Lock()
	defer s.mu.Unlock()
	delete(s.inFlight, card)
}

func (s *SendFiles) shouldNotifyStale(card [2]int64, now time.Time) bool {
	s.mu.Lock()
	defer s.mu.Unlock()
	if s.notices == nil {
		s.notices = map[[2]int64]time.Time{}
	}
	for k, at := range s.notices {
		if now.Sub(at) >= StaleNoticeWindow {
			delete(s.notices, k)
		}
	}
	if _, ok := s.notices[card]; ok {
		return false
	}
	s.notices[card] = now
	return true
}

func validArtifactKey(k string) bool {
	if len(k) != ArtifactKeyLen {
		return false
	}
	for _, c := range k {
		if !(c >= '0' && c <= '9' || c >= 'a' && c <= 'f') {
			return false
		}
	}
	return true
}

func truncateReason(s string) string {
	s = strings.TrimSpace(s)
	r := []rune(s)
	if len(r) <= maxReasonChars {
		return s
	}
	return string(r[:maxReasonChars-1]) + "…"
}

// handleSendFile uploads a file card's file as a reply to the card
// (crates/zdx-bot/src/file_card.rs). The card is resolved from itself (its
// chat and topic, and the key), so it works after a restart. The tap is
// answered first; when Telegram refuses that answer as too old, nothing is
// uploaded and one notice per card asks to tap again.
func (h *Handler) handleSendFile(ctx context.Context, q *telegram.CallbackQuery, key string) {
	answer := func(text string) error { return h.Telegram.AnswerCallbackQuery(ctx, q.ID, &text) }
	m := q.Message
	if m == nil {
		_ = answer("No message context")
		return
	}
	if !validArtifactKey(key) {
		_ = answer("This button no longer works")
		return
	}
	card := [2]int64{m.Chat.ID, m.ID}
	topic := m.EffectiveThreadID()
	if !h.sendFiles.begin(card) {
		_ = answer("Already sending this file")
		return
	}
	defer h.sendFiles.finish(card)
	if err := answer("Sending the file…"); err != nil {
		if d, ok := telegram.APIDescription(err); ok && strings.Contains(d, "query is too old") {
			h.log().Info("Send file tap arrived too late; not sent", "chat_id", m.Chat.ID, "card_id", m.ID)
			if h.sendFiles.shouldNotifyStale(card, time.Now()) {
				h.cardNotice(ctx, m.Chat.ID, topic, m.ID, "⏱ That ⬇ Send file tap reached the bot late (the Mac was probably asleep), so nothing was sent. Tap ⬇ Send file again.")
			}
			return
		}
		h.log().Warn("failed to answer Send file callback", "err", err)
	}

	info, err := h.Resolver.ResolveTopic(ctx, m.Chat.ID, topic)
	if err != nil {
		h.cardNotice(ctx, m.Chat.ID, topic, m.ID, "⚠️ Not sent: "+EscapeHTML(truncateReason(err.Error())))
		return
	}
	store := h.Presenter.Deliverer.Store
	if err := store.DeliveryReady(ctx); err != nil {
		h.cardNotice(ctx, m.Chat.ID, topic, m.ID, "⚠️ Not sent: "+EscapeHTML(truncateReason(err.Error())))
		return
	}
	path, name, err := h.Artifacts.FindArtifact(ctx, info.ThreadID, key)
	if err != nil {
		h.cardNotice(ctx, m.Chat.ID, topic, m.ID, "⚠️ Not sent: "+EscapeHTML(truncateReason(err.Error())))
		return
	}
	if name == "" {
		name = filepath.Base(path)
	}

	msg, err := h.uploadToCard(ctx, m.Chat.ID, topic, m.ID, path)
	if err != nil {
		reason := err.Error()
		if d, ok := telegram.APIDescription(err); ok {
			reason = d
		}
		reason = truncateReason(reason)
		h.log().Warn("Send file failed", "chat_id", m.Chat.ID, "card_id", m.ID, "file", name, "err", err)
		text := fmt.Sprintf("⚠️ %s was not sent (%s). Tap ⬇ Send file to try again.", EscapeHTML(name), EscapeHTML(reason))
		if class, _ := telegram.Classify(err); class == telegram.ClassUnknown {
			text = fmt.Sprintf("⚠️ %s may not have arrived (%s). Check above before you tap ⬇ Send file again.", EscapeHTML(name), EscapeHTML(reason))
		}
		h.cardNotice(ctx, m.Chat.ID, topic, m.ID, text)
		return
	}
	h.log().Info("Send file delivered", "chat_id", m.Chat.ID, "card_id", m.ID, "message_id", msg.ID, "file", name)
	item := engine.FileItem(path)
	id := msg.ID
	item.MarkSent(&id)
	record := engine.NewDeliveryRecord(m.Chat.ID, topic, []engine.DeliveryItem{item})
	if _, err := store.RecordDelivery(context.WithoutCancel(ctx), info.ThreadID, nil, record); err != nil {
		h.log().Warn("failed to record Send file delivery", "thread", info.ThreadID, "err", err)
	}
}

// uploadToCard sends the file as a document replying to the card; a refused
// reply target is dropped and the upload repeated once.
func (h *Handler) uploadToCard(ctx context.Context, chat int64, topic *int64, cardID int64, path string) (*telegram.Message, error) {
	upload, err := telegram.ReadUpload(path, telegram.MethodDocument)
	if err != nil {
		return nil, err
	}
	c := h.Telegram.Reliable()
	opts := telegram.MediaSend{ReplyToMessageID: &cardID, MessageThreadID: topic}
	msg, err := c.SendFile(ctx, chat, telegram.MethodDocument, upload, opts)
	if err != nil && telegram.IsInvalidReplyError(err) {
		opts.ReplyToMessageID = nil
		msg, err = c.SendFile(ctx, chat, telegram.MethodDocument, upload, opts)
	}
	return msg, err
}

func (h *Handler) cardNotice(ctx context.Context, chat int64, topic *int64, cardID int64, html string) {
	req := telegram.TextSend{ChatID: chat, Text: html, MessageThreadID: topic, ReplyToMessageID: &cardID}
	if _, err := h.Telegram.Paced().SendText(ctx, req); err != nil {
		h.log().Warn("failed to post Send file notice", "chat_id", chat, "card_id", cardID, "err", err)
	}
}
