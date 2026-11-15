// Package miniapp serves the Telegram Mini App: the built Svelte shell and
// its `/api/*` routes, with the same paths, auth, limits and JSON bodies as
// the Rust server it replaces (`crates/zdx-bot/src/server.rs`).
//
// Go owns HTTP only: Telegram `initData` auth, routing, static assets, gzip,
// body and concurrency limits, short in-memory caches, and the `delivering`
// flag of a thread view. Every domain projection (threads, trajectory,
// workers, artifacts, canonical images, usage, monitor, workspaces, git) and
// every write comes from the Rust engine through [Engine]. Bodies are passed
// through as raw JSON so Go never mirrors the engine DTOs.
package miniapp

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io/fs"
	"log/slog"
	"net/http"
	"time"

	"github.com/tallesborges/zdx/apps/gateway/internal/conversation"
)

// ErrUnavailable is returned (or wrapped) by an [Engine] when the running
// Rust service does not offer the capability an operation needs. Routes
// answer it with 503 and a message that names the missing capability,
// never with an empty success.
var ErrUnavailable = errors.New("engine capability unavailable")

// Write outcomes. A write operation ([Engine.FeedbackSubmit]) that fails
// without an engine error reply has one of two outcomes, and the route must
// not claim more than it knows:
//
//   - [ErrNotSent]: the request never reached the engine (no listener, the
//     capability check failed). Nothing happened; the client may retry.
//   - [ErrOutcomeUnknown]: the request was sent and no reply came (timeout,
//     connection closed, unreadable reply). The engine may have queued it.
//     A retry should reuse the submission id so the engine can recognize
//     it (`duplicate`, `in_flight`); exactly-once delivery is not promised.
//
// Adapters wrap their transport errors with one of these. Any other error
// that is not an *[EngineError] or [ErrUnavailable] is treated as
// [ErrOutcomeUnknown], the answer that claims less.
var (
	ErrNotSent        = errors.New("request did not reach the engine")
	ErrOutcomeUnknown = errors.New("engine did not answer; the request may still complete")
)

// EngineError is a `{"reply":"error"}` answer of the Rust service.
type EngineError struct {
	// Code is the machine code of the Rust reply (the Code constants).
	// Empty when the service sent none; routes answer 500 for an empty or
	// unknown code.
	Code    string
	Message string
}

func (e *EngineError) Error() string {
	if e.Code == "" {
		return e.Message
	}
	return fmt.Sprintf("%s: %s", e.Code, e.Message)
}

// Engine codes the routes map to HTTP statuses.
const (
	CodeJSONSyntax          = "json_syntax"
	CodeJSONData            = "json_data"
	CodeInFlight            = "in_flight"
	CodeNotFound            = "not_found"
	CodeConflict            = "conflict"
	CodeInternal            = "internal"
	CodeInvalid             = "invalid"
	CodeArtifactNotFound    = "artifact_not_found"
	CodeUnsupportedArtifact = "unsupported_artifact"
	CodeVersionMismatch     = "version_mismatch"
	CodeIDConflict          = "id_conflict"
	CodeBusy                = "busy"
	CodeNotRoutable         = "not_routable"
	CodeForbidden           = "forbidden"
)

// Engine is the Rust side of the Mini App: one method per socket operation.
//
// A method that returns json.RawMessage returns the exact HTTP body the
// route serves (the JSON the Rust server returned for that route), unless
// its doc says otherwise. Thread ids reach the engine as requested: thread
// read operations resolve `active` (latest `telegram-` thread, else the
// literal id) and one `alias_to` hop themselves, as server.rs does.
//
// Implementations return [ErrUnavailable] when the capability is missing,
// *[EngineError] for an error reply, and any other error for transport
// failures. Context cancellation may abort the socket call; routes that must
// outlive a disconnected client pass a context without cancellation.
type Engine interface {
	// ThreadList is `GET /api/threads`; kind is `all` or `workers`.
	ThreadList(ctx context.Context, kind string) (json.RawMessage, error)
	// ThreadView is `GET /api/threads/{id}` without the `delivering` flag.
	ThreadView(ctx context.Context, threadID string, after *uint64) (ThreadView, error)
	ThreadTrajectory(ctx context.Context, threadID string) (json.RawMessage, error)
	ThreadWorkers(ctx context.Context, threadID string) (json.RawMessage, error)
	ArtifactsList(ctx context.Context, threadID string) (json.RawMessage, error)
	// ArtifactResolve maps a listed artifact path to the file to serve. It
	// must refuse any path the thread's artifact projection does not list.
	ArtifactResolve(ctx context.Context, threadID, path string) (ResolvedArtifact, error)
	// ArtifactVersion is `GET /api/threads/{id}/artifacts/version`: the
	// file's SHA-256, size and modified time as the engine computes them.
	// The engine owns the hash because it checks the same hash when feedback
	// names a version.
	ArtifactVersion(ctx context.Context, threadID, path string) (json.RawMessage, error)
	// ArtifactCanonical returns the canonical copy of an image artifact,
	// making and caching it when missing.
	ArtifactCanonical(ctx context.Context, threadID, path string) (CanonicalImage, error)
	// FeedbackSubmit validates, saves and queues Mini App feedback. Errors
	// follow the write-outcome rules of [ErrNotSent] and [ErrOutcomeUnknown].
	FeedbackSubmit(ctx context.Context, req FeedbackSubmit) (FeedbackResult, error)
	// UsageReport is `GET /api/usage?days=N` for a validated window.
	UsageReport(ctx context.Context, days int) (json.RawMessage, error)
	// MonitorSnapshot is the cacheable part of `GET /api/monitor`: a JSON
	// object without `jobs`, `jobs_error`, `active_agents`,
	// `subscriptions`, `subscriptions_status` and `subscriptions_age`,
	// which Go overlays.
	MonitorSnapshot(ctx context.Context) (json.RawMessage, error)
	// JobSummaries is the monitor's `jobs` array (the shared job summaries:
	// schedule, status, next run, last outcome) and its `jobs_error`, set
	// when the jobs could not be listed. It is cached with the snapshot.
	JobSummaries(ctx context.Context) (jobs json.RawMessage, jobsError *string)
	// ActiveAgents is the `active_agents` array, read on every request.
	ActiveAgents(ctx context.Context) (json.RawMessage, error)
	// SubscriptionQuotas is the `subscriptions` array. Slow: it calls
	// providers. Go calls it only from a background refresh.
	SubscriptionQuotas(ctx context.Context) (json.RawMessage, error)
	// Workspaces is `GET /api/workspaces`.
	Workspaces(ctx context.Context) (json.RawMessage, error)
	// GitStatus, GitScope and GitDiff are `GET /api/git`, `/api/git/scope`
	// and `/api/git/diff`. threadID is empty when the request named none.
	GitStatus(ctx context.Context, threadID string) (json.RawMessage, error)
	GitScope(ctx context.Context, threadID, scope string) (json.RawMessage, error)
	GitDiff(ctx context.Context, req GitDiff) (json.RawMessage, error)
}

// ThreadView is one thread projection.
type ThreadView struct {
	// SourceID is the thread the events were read from: the requested id
	// after `active` and one alias hop. Reply deliveries are tracked under
	// it. Empty means the requested id.
	SourceID string
	// Body is the ThreadResponse JSON object; Go sets `delivering`.
	Body json.RawMessage
}

// ResolvedArtifact is a file the engine allows the route to serve.
type ResolvedArtifact struct {
	// Path is absolute.
	Path string
	Mime string
	// Filename is the engine's download name (`"` already replaced), used
	// as is in `Content-Disposition`. Go does not derive it.
	Filename string
}

// CanonicalImage is the canonical copy of an image artifact and its identity.
type CanonicalImage struct {
	// Data is the image, decoded from the view's base64 `data`. Go serves it
	// unchanged.
	Data         []byte
	Mime         string
	SourceSHA256 string
	SHA256       string
	Width        uint32
	Height       uint32
	// SourceWidth and SourceHeight are the original's upright size.
	SourceWidth  uint32
	SourceHeight uint32
}

// FeedbackSubmit is one feedback submission for the engine.
type FeedbackSubmit struct {
	// ThreadID is the requested id (the chat topic), never `active`.
	ThreadID string
	UserID   int64
	// Submission is the request body text, unparsed and valid UTF-8. Send it
	// as a JSON string field, not as embedded JSON: the engine parses it
	// with the rules of axum's `Json` extractor and reports syntax errors
	// (code `json_syntax`, 400) and type errors (`json_data`, 422) with axum's
	// text. Embedded raw JSON would let Go accept what that parser refuses
	// (lone surrogates, nesting past 128) or fail the whole socket request.
	Submission string
}

// FeedbackResult is the engine reply to a submission.
type FeedbackResult struct {
	// Status is `queued` or `duplicate`.
	Status      string
	ThreadID    string
	InputID     *uint64
	Annotations int
	Stale       bool
}

// GitDiff is one `GET /api/git/diff` request.
type GitDiff struct {
	ThreadID string
	Path     string
	Kind     string
	Commit   string
}

// Deliveries reports reply deliveries running in the bot process, with the
// observation rule of `delivery_tracker.rs`: a thread is delivering when a
// delivery for it runs after the read, or when any delivery finished during
// the read. [DeliveryTracker] implements it.
type Deliveries interface {
	// Finished counts deliveries that finished since start.
	Finished() uint64
	// Running reports a delivery for threadID in progress.
	Running(threadID string) bool
}

// FeedbackRoute is the Telegram chat (and topic) a thread id belongs to.
type FeedbackRoute = conversation.Route

// FeedbackGate is the bot's sender check for feedback, the same gate the
// message queue applies (`route_allowed` in artifact_feedback.rs).
type FeedbackGate interface {
	Allows(userID int64, route FeedbackRoute) bool
}

// Config builds a Mini App handler.
type Config struct {
	BotToken       string
	AllowedUserIDs []int64
	// Assets holds the built Mini App (`apps/web/dist`), `index.html` at
	// its root. Nil answers the shell with a 500 that names the build step.
	Assets fs.FS
	Engine Engine
	// Deliveries is optional; nil reports `delivering: false`.
	Deliveries Deliveries
	// Feedback is optional; nil refuses feedback with 503.
	Feedback FeedbackGate
	// Logger is optional; nil uses slog.Default().
	Logger *slog.Logger
	// Now is optional; nil uses time.Now. Used for initData freshness.
	Now func() time.Time
}

// New returns the handler for `/app`, `/app/{path...}`, `/threads`,
// `/monitor` and `/api/*`. The caller owns the listener.
func New(cfg Config) (http.Handler, error) {
	if cfg.BotToken == "" {
		return nil, errors.New("miniapp: bot token is required")
	}
	if cfg.Engine == nil {
		return nil, errors.New("miniapp: engine is required")
	}
	s := newServer(cfg)
	return s.routes(), nil
}
