// Package gateway wires the Go gateway together: Telegram client, polling
// router, renderer bridge, and the Mini App HTTP handler over the Rust
// orchestrator socket.
package gateway

import (
	"context"
	"encoding/base64"
	"encoding/json"
	"errors"
	"fmt"
	"log/slog"
	"math"
	"path/filepath"

	"github.com/tallesborges/zdx/apps/gateway/internal/bot"
	"github.com/tallesborges/zdx/apps/gateway/internal/conversation"
	"github.com/tallesborges/zdx/apps/gateway/internal/engine"
	"github.com/tallesborges/zdx/apps/gateway/internal/miniapp"
	"github.com/tallesborges/zdx/apps/gateway/internal/telegram"
	"github.com/tallesborges/zdx/apps/shared/quota"
	"github.com/tallesborges/zdx/apps/shared/version"
)

// MiniAppEngine implements miniapp.Engine over the orchestrator socket.
// Every read except quotas is a Rust view (views.rs) whose body is the
// route's JSON, passed through unchanged. A view whose capability the
// service lacks answers miniapp.ErrUnavailable (HTTP 503) without sending
// it. Quotas are read in this process by the shared quota package.
type MiniAppEngine struct {
	Client *engine.Client
	// Quota are the inputs of the in-process quota read: the ZDX home whose
	// oauth.json is read (read-only) and the zdx version for the providers'
	// user agent. An empty Version fails every quota read with
	// version.ErrUnknown.
	Quota quota.Options
	// RootForChat is the chat root a feedback submission is sent with, the
	// same function the bot's Handler uses (Settings.RootForChat). Root is
	// the bot's root, for a thread id that names no Telegram chat (Rust
	// refuses those). The root never comes from the HTTP body.
	RootForChat func(chat int64) string
	Root        string
	// Telegram posts the notice of queued feedback; nil posts none.
	Telegram *telegram.Client
	Log      *slog.Logger
}

var _ miniapp.Engine = (*MiniAppEngine)(nil)

// mapError turns engine errors into the miniapp error contract.
func mapError(err error) error {
	if err == nil {
		return nil
	}
	var unsupported *engine.ErrUnsupported
	if errors.As(err, &unsupported) {
		return fmt.Errorf("%w: %s", miniapp.ErrUnavailable, unsupported.Error())
	}
	var se *engine.ServiceError
	if errors.As(err, &se) {
		return &miniapp.EngineError{Code: se.Code, Message: se.Message}
	}
	return err
}

func (e *MiniAppEngine) view(ctx context.Context, v engine.View) (engine.ViewOutput, error) {
	out, err := e.Client.View(ctx, v)
	return out, mapError(err)
}

func (e *MiniAppEngine) body(ctx context.Context, v engine.View) (json.RawMessage, error) {
	out, err := e.view(ctx, v)
	if err != nil {
		return nil, err
	}
	return json.RawMessage(out.Body), nil
}

// optional is nil for an empty string: the request named none.
func optional(s string) *string {
	if s == "" {
		return nil
	}
	return &s
}

func (e *MiniAppEngine) ThreadList(ctx context.Context, kind string) (json.RawMessage, error) {
	return e.body(ctx, engine.View{Op: engine.ViewThreadsList, Kind: optional(kind)})
}

func (e *MiniAppEngine) ThreadView(ctx context.Context, threadID string, after *uint64) (miniapp.ThreadView, error) {
	out, err := e.view(ctx, engine.View{Op: engine.ViewThreadView, ThreadID: &threadID, After: after})
	if err != nil {
		return miniapp.ThreadView{}, err
	}
	view := miniapp.ThreadView{Body: out.Body}
	if out.SourceID != nil {
		view.SourceID = *out.SourceID
	}
	return view, nil
}

func (e *MiniAppEngine) ThreadTrajectory(ctx context.Context, threadID string) (json.RawMessage, error) {
	return e.body(ctx, engine.View{Op: engine.ViewThreadTrajectory, ThreadID: &threadID})
}

func (e *MiniAppEngine) ThreadWorkers(ctx context.Context, threadID string) (json.RawMessage, error) {
	return e.body(ctx, engine.View{Op: engine.ViewThreadWorkers, ThreadID: &threadID})
}

func (e *MiniAppEngine) ArtifactsList(ctx context.Context, threadID string) (json.RawMessage, error) {
	return e.body(ctx, engine.View{Op: engine.ViewArtifactsList, ThreadID: &threadID})
}

// ArtifactResolve takes Rust's {path, mime, filename}. Rust refuses any
// path the thread's artifact projection does not list. A body without an
// absolute path, a mime type and a file name is an error (HTTP 500).
func (e *MiniAppEngine) ArtifactResolve(ctx context.Context, threadID, path string) (miniapp.ResolvedArtifact, error) {
	body, err := e.body(ctx, engine.View{Op: engine.ViewArtifactResolve, ThreadID: &threadID, Path: &path})
	if err != nil {
		return miniapp.ResolvedArtifact{}, err
	}
	var r struct {
		Path     string `json:"path"`
		Mime     string `json:"mime"`
		Filename string `json:"filename"`
	}
	if err := json.Unmarshal(body, &r); err != nil {
		return miniapp.ResolvedArtifact{}, fmt.Errorf("artifact_resolve: malformed body: %w", err)
	}
	if !filepath.IsAbs(r.Path) || r.Mime == "" || r.Filename == "" {
		return miniapp.ResolvedArtifact{}, errors.New("artifact_resolve: body needs an absolute path, a mime type and a file name")
	}
	return miniapp.ResolvedArtifact{Path: r.Path, Mime: r.Mime, Filename: r.Filename}, nil
}

// ArtifactVersion is Rust's hash, size and modified time of the file; Rust
// checks the same hash when feedback names a version.
func (e *MiniAppEngine) ArtifactVersion(ctx context.Context, threadID, path string) (json.RawMessage, error) {
	return e.body(ctx, engine.View{Op: engine.ViewArtifactVersion, ThreadID: &threadID, Path: &path})
}

// ArtifactCanonical decodes Rust's canonical image: base64 data and its
// identity. The view gets engine.CanonicalTimeout (Rust waits for an image
// slot). A body without data, mime or hashes is an error (HTTP 500).
func (e *MiniAppEngine) ArtifactCanonical(ctx context.Context, threadID, path string) (miniapp.CanonicalImage, error) {
	body, err := e.body(ctx, engine.View{Op: engine.ViewArtifactCanonical, ThreadID: &threadID, Path: &path})
	if err != nil {
		return miniapp.CanonicalImage{}, err
	}
	var r struct {
		Mime         string `json:"mime"`
		Data         string `json:"data"`
		SourceSHA256 string `json:"source_sha256"`
		SHA256       string `json:"sha256"`
		Width        uint32 `json:"width"`
		Height       uint32 `json:"height"`
		SourceWidth  uint32 `json:"source_width"`
		SourceHeight uint32 `json:"source_height"`
	}
	if err := json.Unmarshal(body, &r); err != nil {
		return miniapp.CanonicalImage{}, fmt.Errorf("artifact_canonical: malformed body: %w", err)
	}
	if r.Data == "" || r.Mime == "" || r.SourceSHA256 == "" || r.SHA256 == "" {
		return miniapp.CanonicalImage{}, errors.New("artifact_canonical: body needs data, mime, source_sha256 and sha256")
	}
	data, err := base64.StdEncoding.DecodeString(r.Data)
	if err != nil || len(data) == 0 {
		return miniapp.CanonicalImage{}, fmt.Errorf("artifact_canonical: data is not base64 image bytes: %w", err)
	}
	return miniapp.CanonicalImage{
		Data: data, Mime: r.Mime, SourceSHA256: r.SourceSHA256, SHA256: r.SHA256,
		Width: r.Width, Height: r.Height, SourceWidth: r.SourceWidth, SourceHeight: r.SourceHeight,
	}, nil
}

func (e *MiniAppEngine) UsageReport(ctx context.Context, days int) (json.RawMessage, error) {
	if days < 0 || int64(days) > math.MaxUint32 {
		return nil, &miniapp.EngineError{Code: miniapp.CodeInvalid, Message: "invalid days"}
	}
	d := uint32(days)
	return e.body(ctx, engine.View{Op: engine.ViewUsageReport, Days: &d})
}

func (e *MiniAppEngine) MonitorSnapshot(ctx context.Context) (json.RawMessage, error) {
	return e.body(ctx, engine.View{Op: engine.ViewMonitorSnapshot})
}

func (e *MiniAppEngine) ActiveAgents(ctx context.Context) (json.RawMessage, error) {
	return e.body(ctx, engine.View{Op: engine.ViewActiveAgents})
}

// SubscriptionQuotas reads every stored account's quota with the shared
// quota package and returns the `subscriptions` body (quota.Subscriptions,
// the JSON the retired Rust view sent). It sends nothing to the socket. A
// missing version, an unreadable account store, or a cancelled ctx is an
// error, never an empty list, so the caller keeps its last known quotas.
func (e *MiniAppEngine) SubscriptionQuotas(ctx context.Context) (json.RawMessage, error) {
	if e.Quota.Version == "" {
		return nil, version.ErrUnknown
	}
	snapshot, err := quota.Fetch(ctx, e.Quota)
	if err != nil {
		return nil, err
	}
	return json.RawMessage(quota.SubscriptionsJSON(quota.Subscriptions(snapshot))), nil
}

func (e *MiniAppEngine) Workspaces(ctx context.Context) (json.RawMessage, error) {
	return e.body(ctx, engine.View{Op: engine.ViewWorkspaces})
}

func (e *MiniAppEngine) GitStatus(ctx context.Context, threadID string) (json.RawMessage, error) {
	return e.body(ctx, engine.View{Op: engine.ViewGitStatus, ThreadID: optional(threadID)})
}

func (e *MiniAppEngine) GitScope(ctx context.Context, threadID, scope string) (json.RawMessage, error) {
	return e.body(ctx, engine.View{Op: engine.ViewGitScope, ThreadID: optional(threadID), Scope: &scope})
}

func (e *MiniAppEngine) GitDiff(ctx context.Context, req miniapp.GitDiff) (json.RawMessage, error) {
	return e.body(ctx, engine.View{Op: engine.ViewGitDiff, ThreadID: optional(req.ThreadID), Path: &req.Path, Kind: &req.Kind, Commit: optional(req.Commit)})
}

// FeedbackSubmit sends the submission text unchanged and maps the reply.
// Errors keep the write outcome: ErrNotSent only when nothing was written,
// ErrOutcomeUnknown for anything after the write (no or unreadable reply, a
// feedback reply with malformed fields), EngineError for a typed refusal.
func (e *MiniAppEngine) FeedbackSubmit(ctx context.Context, req miniapp.FeedbackSubmit) (miniapp.FeedbackResult, error) {
	reply, err := e.Client.SubmitFeedback(ctx, engine.FeedbackSubmit{
		ThreadID: req.ThreadID, UserID: req.UserID, Root: e.feedbackRoot(req.ThreadID), Submission: req.Submission,
	})
	if err != nil {
		return miniapp.FeedbackResult{}, mapFeedbackError(err)
	}
	if reply.FeedbackStatus == "queued" {
		defer e.feedbackNotice(ctx, reply)
	}
	unknown := func(what string, err error) error {
		return fmt.Errorf("%w: decode feedback %s: %w", miniapp.ErrOutcomeUnknown, what, err)
	}
	res := miniapp.FeedbackResult{Status: reply.FeedbackStatus, ThreadID: reply.ThreadID, InputID: reply.InputID}
	if len(reply.Annotations) > 0 {
		if err := json.Unmarshal(reply.Annotations, &res.Annotations); err != nil {
			return miniapp.FeedbackResult{}, unknown("annotations", err)
		}
	}
	if len(reply.Stale) > 0 {
		if err := json.Unmarshal(reply.Stale, &res.Stale); err != nil {
			return miniapp.FeedbackResult{}, unknown("stale flag", err)
		}
	}
	return res, nil
}

// feedbackRoot is the root of the requested thread's chat; the bot's root
// when the id names no Telegram chat.
func (e *MiniAppEngine) feedbackRoot(threadID string) string {
	if r, ok := conversation.Parse(threadID); ok && e.RootForChat != nil {
		return e.RootForChat(r.Chat)
	}
	return e.Root
}

// feedbackNotice posts what the Rust bot posts for queued feedback, best
// effort: the turn runs either way.
func (e *MiniAppEngine) feedbackNotice(ctx context.Context, reply *engine.Reply) {
	if e.Telegram == nil || reply.Route == nil || reply.ArtifactName == nil {
		return
	}
	var n int
	_ = json.Unmarshal(reply.Annotations, &n)
	if _, err := e.Telegram.SendText(context.WithoutCancel(ctx), telegram.TextSend{
		ChatID: reply.Route.Chat, Text: FeedbackNotice(*reply.ArtifactName, n), MessageThreadID: reply.Route.Topic,
	}); err != nil {
		log := e.Log
		if log == nil {
			log = slog.Default()
		}
		log.Warn("failed to post artifact feedback notice", "chat_id", reply.Route.Chat, "err", err)
	}
}

// FeedbackNotice is the chat notice of queued feedback (Rust
// artifact_feedback.rs).
func FeedbackNotice(artifactName string, notes int) string {
	noun := "notes"
	if notes == 1 {
		noun = "note"
	}
	return fmt.Sprintf("📝 Feedback on <b>%s</b> from the Mini App (%d %s).", bot.EscapeHTML(artifactName), notes, noun)
}

// mapFeedbackError classifies a feedback failure by the stage it happened
// in. Anything unclassified counts as unknown, the answer that claims less.
func mapFeedbackError(err error) error {
	var unsupported *engine.ErrUnsupported
	var se *engine.ServiceError
	switch {
	case errors.As(err, &unsupported):
		return fmt.Errorf("%w: %s", miniapp.ErrUnavailable, unsupported.Error())
	case errors.As(err, &se):
		return &miniapp.EngineError{Code: se.Code, Message: se.Message}
	case errors.Is(err, engine.ErrRequestNotSent):
		return fmt.Errorf("%w: %w", miniapp.ErrNotSent, err)
	default:
		return fmt.Errorf("%w: %w", miniapp.ErrOutcomeUnknown, err)
	}
}

// FeedbackGate is the bot's sender check for Mini App feedback.
type FeedbackGate struct{ Allow bot.Allowlist }

var _ miniapp.FeedbackGate = FeedbackGate{}

func (g FeedbackGate) Allows(userID int64, route miniapp.FeedbackRoute) bool {
	return g.Allow.FeedbackAllowed(userID, route.Chat)
}
