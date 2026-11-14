package miniapp

import (
	"encoding/json"
	"errors"
	"io"
	"net/http"
	"strings"
	"unicode/utf8"

	"github.com/tallesborges/zdx/apps/gateway/internal/conversation"
)

// maxSubmissionBytes is the engine's request cap for feedback
// (`artifact_feedback::MAX_SUBMISSION_BYTES`): every media item at its limit,
// base64-encoded, plus JSON overhead.
const maxSubmissionBytes = 48 * 1024 * 1024

// feedbackConcurrency is how many submissions are read and saved at once.
// A submission can be tens of MiB, so the cap holds before the body is read.
const feedbackConcurrency = 2

type feedbackResponse struct {
	// Status is `queued` (on the chat's queue) or `duplicate` (already
	// queued, nothing sent again). Neither means a model has read it.
	Status      string `json:"status"`
	Annotations int    `json:"annotations"`
	Stale       bool   `json:"stale"`
}

// postFeedback accepts annotations on a thread artifact and hands them to
// the engine, which parses, validates, saves and queues them in the
// thread's chat.
//
// Order, as in server.rs: the upload slot is taken first (429), then the
// path is read, then the content type (415) and the body (413). The body is
// parsed by the engine, so Go never accepts JSON the Rust parser refuses.
// Go refuses `active`, bad ids, senders the bot gate refuses, and a
// submission already in flight before the engine sees anything. The engine
// call is not cancelled by a client that leaves, and the slot is held until
// it answers.
func (s *server) postFeedback(w http.ResponseWriter, r *http.Request, c call) {
	select {
	case s.feedbackPermits <- struct{}{}:
	default:
		writeText(w, http.StatusTooManyRequests, "Another feedback upload is in progress. Try again in a moment.")
		return
	}
	held := true
	release := func() {
		if held {
			held = false
			<-s.feedbackPermits
		}
	}
	defer release()

	if c.badID {
		badIDResponse(w)
		return
	}
	if !jsonContentType(r.Header.Values("Content-Type")) {
		writeText(w, http.StatusUnsupportedMediaType, "Expected request with `Content-Type: application/json`")
		return
	}
	body, err := io.ReadAll(http.MaxBytesReader(w, r.Body, maxSubmissionBytes))
	if err != nil {
		var tooLarge *http.MaxBytesError
		if errors.As(err, &tooLarge) {
			writeText(w, http.StatusRequestEntityTooLarge, "Failed to buffer the request body: length limit exceeded")
			return
		}
		writeText(w, http.StatusBadRequest, "Failed to buffer the request body")
		return
	}
	// The engine takes the body as text. Rust refuses invalid UTF-8 as a
	// JSON syntax error (400) too, with a position Go does not compute.
	if !utf8.Valid(body) {
		writeText(w, http.StatusBadRequest, "Failed to parse the request body as JSON: invalid unicode code point")
		return
	}

	if s.feedback == nil {
		writeText(w, http.StatusServiceUnavailable, "Feedback delivery is not available on this server")
		return
	}
	// `active` moves to whichever thread was used last, so a write through
	// it could land in another conversation than the one previewed.
	if c.id == "active" {
		writeText(w, http.StatusBadRequest, "Reopen the thread from its chat to send feedback.")
		return
	}
	if !validThreadID(c.id) {
		writeText(w, http.StatusBadRequest, "Invalid thread ID")
		return
	}
	// A thread with no chat goes to the engine, which refuses it
	// (`not_routable`) with the text for a worker or a non-Telegram thread.
	if route, ok := conversation.Parse(c.id); ok && !s.feedback.Allows(c.user, route) {
		writeText(w, http.StatusForbidden, "You cannot send feedback into this chat")
		return
	}

	// A cheap local guard against sending the same upload twice at once.
	// The engine keys submissions by the alias-resolved thread and answers
	// `in_flight` itself; a body Go cannot read the id from is left to it.
	if submission, ok := submissionID(body); ok {
		key := c.id + "/" + submission
		s.feedbackMu.Lock()
		_, busy := s.feedbackKeys[key]
		if !busy {
			s.feedbackKeys[key] = struct{}{}
		}
		s.feedbackMu.Unlock()
		if busy {
			writeText(w, http.StatusConflict, "This feedback is still being sent. Try again in a moment.")
			return
		}
		defer func() {
			s.feedbackMu.Lock()
			delete(s.feedbackKeys, key)
			s.feedbackMu.Unlock()
		}()
	}

	result, err := s.engine.FeedbackSubmit(detached(r.Context()), FeedbackSubmit{
		ThreadID:   c.id,
		UserID:     c.user,
		Submission: string(body),
	})
	release()
	if err != nil {
		s.feedbackFailure(w, c.id, err)
		return
	}
	switch result.Status {
	case "queued":
		writeJSON(w, http.StatusAccepted, feedbackResponse{"queued", result.Annotations, result.Stale})
	case "duplicate":
		writeJSON(w, http.StatusOK, feedbackResponse{Status: "duplicate"})
	default:
		s.log.Error("Engine answered feedback with an unknown status", "thread_id", c.id, "status", result.Status)
		writeText(w, http.StatusInternalServerError, "Feedback could not be queued. Try again.")
	}
}

// feedbackStatus maps engine codes to the statuses server.rs answers.
var feedbackStatus = map[string]int{
	CodeJSONSyntax:          http.StatusBadRequest,
	CodeJSONData:            http.StatusUnprocessableEntity,
	CodeInvalid:             http.StatusBadRequest,
	CodeArtifactNotFound:    http.StatusNotFound,
	CodeUnsupportedArtifact: http.StatusUnprocessableEntity,
	// Distinct from the 409s so the client can offer "send anyway".
	CodeVersionMismatch: http.StatusPreconditionFailed,
	CodeIDConflict:      http.StatusConflict,
	CodeInFlight:        http.StatusConflict,
	CodeBusy:            http.StatusServiceUnavailable,
	CodeNotRoutable:     http.StatusConflict,
	// Another run (a TUI, `zdx exec`) holds the thread; nothing was queued.
	CodeConflict:  http.StatusConflict,
	CodeForbidden: http.StatusForbidden,
}

// feedbackOutcomeUnknown is the answer when the engine got the submission
// and did not say what happened. It stays a 500 like every other delivery
// failure (the client shows the text of any status but 401, 412 and 413,
// and keeps the notes and their submission id until they change), so only
// the text tells the user that sending again is safe.
const feedbackOutcomeUnknown = "The bot did not confirm this feedback in time. It may still be processing. " +
	"Retry without changing the notes so the same submission can be recognized."

func (s *server) feedbackFailure(w http.ResponseWriter, id string, err error) {
	if errors.Is(err, ErrUnavailable) {
		writeText(w, http.StatusServiceUnavailable, "Feedback delivery is not available on this server")
		return
	}
	code, message, replied := engineCode(err)
	switch {
	case replied:
		status, known := feedbackStatus[code]
		if !known {
			s.log.Error("Failed to save artifact feedback", "thread_id", id, "error", err)
			writeText(w, http.StatusInternalServerError, "Saving feedback failed")
			return
		}
		writeText(w, status, message)
	case errors.Is(err, ErrNotSent):
		s.log.Warn("Feedback did not reach the engine", "thread_id", id, "error", err)
		writeText(w, http.StatusInternalServerError, "Feedback could not be queued. Try again.")
	default:
		if !errors.Is(err, ErrOutcomeUnknown) {
			s.log.Error("Unclassified feedback failure, reported as unknown outcome", "thread_id", id, "error", err)
		}
		s.log.Error("Feedback outcome unknown", "thread_id", id, "error", err)
		writeText(w, http.StatusInternalServerError, feedbackOutcomeUnknown)
	}
}

// submissionID reads a top-level `submission_id` string for the local
// in-flight guard. It stops at the first such member; it is not validation.
func submissionID(body []byte) (string, bool) {
	dec := json.NewDecoder(strings.NewReader(string(body)))
	if open, err := dec.Token(); err != nil || open != json.Delim('{') {
		return "", false
	}
	for dec.More() {
		key, err := dec.Token()
		if err != nil {
			return "", false
		}
		if key == "submission_id" {
			var id string
			if err := dec.Decode(&id); err != nil {
				return "", false
			}
			return id, true
		}
		var skip json.RawMessage
		if err := dec.Decode(&skip); err != nil {
			return "", false
		}
	}
	return "", false
}

// jsonContentType is axum's `Json` check on the first Content-Type header:
// readable as visible ASCII, parsed as by the `mime` crate (0.3.17), type
// `application` and subtype or `+suffix` `json`.
func jsonContentType(values []string) bool {
	value, ok := visibleHeader(values)
	if !ok {
		return false
	}
	kind, subtype, suffix, ok := parseMime(value)
	return ok && kind == "application" && (subtype == "json" || suffix == "json")
}

// parseMime follows `mime::Mime::from_str`, returning the lowercased type,
// subtype (up to the last `+`) and suffix (after it).
func parseMime(s string) (kind, subtype, suffix string, ok bool) {
	if s == "*/*" {
		return "*", "*", "", true
	}
	slash := -1
	for i := 0; i < len(s); i++ {
		if s[i] == '/' && i > 0 {
			slash = i
			break
		}
		if !isToken(s[i]) {
			return "", "", "", false
		}
	}
	if slash < 0 {
		return "", "", "", false
	}
	start := slash + 1
	plus, semicolon := -1, len(s)
	for i := start; i < len(s); i++ {
		c := s[i]
		if c == '+' && i > start {
			plus = i
		} else if c == ';' && i > start {
			semicolon = i
			break
		} else if !isToken(c) {
			return "", "", "", false
		}
	}
	if semicolon < len(s) && !validMimeParams(s, semicolon) {
		return "", "", "", false
	}
	lower := strings.ToLower(s)
	kind = lower[:slash]
	if plus >= 0 {
		return kind, lower[slash+1 : plus], lower[plus+1 : semicolon], true
	}
	return kind, lower[slash+1 : semicolon], "", true
}

// validMimeParams follows `params_from_str` of the mime crate.
func validMimeParams(s string, semicolon int) bool {
	i := semicolon + 1
	start := i
	for start < len(s) {
		// name: a space is skipped only where a name starts
		name := false
		for !name {
			if i >= len(s) {
				return false
			}
			c := s[i]
			switch {
			case c == ' ' && i == start:
				i++
				start = i
				if start >= len(s) {
					return true
				}
			case isToken(c):
				i++
			case c == '=' && i > start:
				i++
				start = i
				name = true
			default:
				return false
			}
		}
		quoted := false
		for done := false; !done; {
			if quoted {
				if i >= len(s) {
					return false
				}
				c := s[i]
				switch {
				case c == '"' && i > start:
					i++
					done = true
				case c > 31 && c != 127:
					i++
				default:
					return false
				}
				continue
			}
			if i >= len(s) {
				start = len(s)
				done = true
				continue
			}
			c := s[i]
			switch {
			case c == '"' && i == start:
				quoted = true
				i++
				start = i
			case isToken(c):
				i++
			case c == ';' && i > start:
				i++
				start = i
				done = true
			default:
				return false
			}
		}
		if quoted {
			for {
				if i >= len(s) {
					start = len(s)
					break
				}
				c := s[i]
				i++
				if c == ';' {
					start = i
					break
				}
				if c != ' ' {
					return false
				}
			}
		}
	}
	return true
}

func isToken(c byte) bool {
	switch {
	case 'a' <= c && c <= 'z', 'A' <= c && c <= 'Z', '0' <= c && c <= '9':
		return true
	}
	return strings.IndexByte("!#$%&'*+-.^_`|~", c) >= 0
}
