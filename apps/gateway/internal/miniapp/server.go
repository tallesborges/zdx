package miniapp

import (
	"context"
	"errors"
	"log/slog"
	"net/http"
	"strconv"
	"strings"
	"sync"
	"time"
)

// slowRequest is where an API request becomes a visible wait; slower
// requests are logged at info level (server.rs SLOW_REQUEST).
const slowRequest = 750 * time.Millisecond

type apiError struct {
	status  int
	message string
}

type server struct {
	botToken   string
	allowed    map[int64]struct{}
	assets     assetSource
	engine     Engine
	deliveries Deliveries
	feedback   FeedbackGate
	log        *slog.Logger
	now        func() time.Time

	monitor monitorCache
	usage   usageCache

	imagePermits    chan struct{}
	feedbackPermits chan struct{}
	feedbackMu      sync.Mutex
	feedbackKeys    map[string]struct{}
}

func newServer(cfg Config) *server {
	allowed := make(map[int64]struct{}, len(cfg.AllowedUserIDs))
	for _, id := range cfg.AllowedUserIDs {
		allowed[id] = struct{}{}
	}
	logger := cfg.Logger
	if logger == nil {
		logger = slog.Default()
	}
	now := cfg.Now
	if now == nil {
		now = time.Now
	}
	return &server{
		botToken:        cfg.BotToken,
		allowed:         allowed,
		assets:          assetSource{fs: cfg.Assets},
		engine:          cfg.Engine,
		deliveries:      cfg.Deliveries,
		feedback:        cfg.Feedback,
		log:             logger,
		now:             now,
		usage:           usageCache{entries: map[int]cachedUsage{}},
		imagePermits:    make(chan struct{}, imageReads),
		feedbackPermits: make(chan struct{}, feedbackConcurrency),
		feedbackKeys:    map[string]struct{}{},
	}
}

// logAPI logs every `/api/*` request with its status and duration: the
// Mini App renders its shell from static assets, so a visible wait is one
// of these requests. Slow ones are logged at info level.
func (s *server) logAPI(r *http.Request, status int, elapsed time.Duration) {
	attrs := []any{"method", r.Method, "path", r.URL.Path, "status", status, "elapsed_ms", elapsed.Milliseconds()}
	if elapsed >= slowRequest {
		s.log.Info("Slow Mini App API request", attrs...)
	} else {
		s.log.Debug("Mini App API request", attrs...)
	}
}

type statusRecorder struct {
	http.ResponseWriter
	status int
}

func (r *statusRecorder) WriteHeader(status int) {
	r.status = status
	r.ResponseWriter.WriteHeader(status)
}

func (r *statusRecorder) Unwrap() http.ResponseWriter { return r.ResponseWriter }

func writeText(w http.ResponseWriter, status int, message string) {
	w.Header().Set("Content-Type", "text/plain; charset=utf-8")
	w.Header().Set("Content-Length", strconv.Itoa(len(message)))
	w.WriteHeader(status)
	_, _ = w.Write([]byte(message))
}

func writeRawJSON(w http.ResponseWriter, status int, body []byte) {
	w.Header().Set("Content-Type", "application/json")
	w.Header().Set("Content-Length", strconv.Itoa(len(body)))
	w.WriteHeader(status)
	_, _ = w.Write(body)
}

func writeJSON(w http.ResponseWriter, status int, value any) {
	body, err := marshalJSON(value)
	if err != nil {
		writeText(w, http.StatusInternalServerError, "Response encoding failed")
		return
	}
	writeRawJSON(w, status, body)
}

// validThreadID is server.rs `valid_thread_id`: no empty id, no `.`/`..`,
// no path separators or NUL, so an id can never leave the threads directory.
func validThreadID(id string) bool {
	return id != "" && id != "." && id != ".." && !strings.ContainsAny(id, "/\\\x00")
}

// unavailable answers a missing engine capability.
func unavailable(w http.ResponseWriter, what string) {
	writeText(w, http.StatusServiceUnavailable, what+" is not available on this server yet")
}

// engineCode returns the code of an engine error reply, or "".
func engineCode(err error) (string, string, bool) {
	var engineErr *EngineError
	if errors.As(err, &engineErr) {
		return engineErr.Code, engineErr.Message, true
	}
	return "", "", false
}

// viewStatus maps the codes a read view refuses with to the status the
// Rust route answers; views.rs derives each code from that status.
var viewStatus = map[string]int{
	CodeInvalid:             http.StatusBadRequest,
	CodeNotFound:            http.StatusNotFound,
	CodeArtifactNotFound:    http.StatusNotFound,
	CodeUnsupportedArtifact: http.StatusUnprocessableEntity,
	CodeConflict:            http.StatusConflict,
	CodeForbidden:           http.StatusForbidden,
	CodeBusy:                http.StatusServiceUnavailable,
	CodeInternal:            http.StatusInternalServerError,
}

// viewFailure answers a failed engine read. A missing capability is 503
// naming it. A refusal with a known code is that code's status with the
// engine's text, which is the Rust route's own text. A refusal without a
// code or with an unknown one, and a transport failure, are a logged 500
// with the route's fallback text, so nothing unexpected reaches a client.
func (s *server) viewFailure(w http.ResponseWriter, err error, what, fallback string) {
	if errors.Is(err, ErrUnavailable) {
		unavailable(w, what)
		return
	}
	if code, message, ok := engineCode(err); ok {
		if status, known := viewStatus[code]; known && message != "" {
			if status == http.StatusInternalServerError {
				s.log.Warn("Mini App engine view failed", "route", what, "error", err)
			}
			writeText(w, status, message)
			return
		}
	}
	s.log.Warn("Mini App engine view failed", "route", what, "error", err)
	writeText(w, http.StatusInternalServerError, fallback)
}

// detached keeps request values but not cancellation, for work that must
// finish (and keep its permit) after the client leaves.
func detached(ctx context.Context) context.Context { return context.WithoutCancel(ctx) }
