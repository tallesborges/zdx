// Package telegram is the gateway's Telegram Bot API client: transport,
// per-chat pacing, flood-wait handling, and delivery-outcome classification.
// It mirrors crates/zdx-bot/src/telegram/mod.rs and
// crates/zdx-engine/src/telegram/flood.rs. Tokens and message bodies are
// never logged or put in errors.
package telegram

import (
	"errors"
	"fmt"
	"strconv"
	"strings"
	"time"
)

// Pacing policy derived from Telegram's published limits (Bots FAQ): about
// one new message per second per chat, at most 20 per minute in a group.
// Whether a 429 is per chat, per method, or per bot is not documented, so
// every caller still handles refusals.
const (
	MinNewMessageSpacing      = time.Second
	GroupWindow               = time.Minute
	GroupWindowMaxNewMessages = 20
	// FallbackRetryAfter is the wait used when a 429 has no usable retry_after.
	FallbackRetryAfter = 5 * time.Second
)

// BotIdentity is the non-secret part of a token: the numeric bot id.
func BotIdentity(token string) string {
	id, _, ok := strings.Cut(token, ":")
	if !ok || id == "" {
		return "unknown"
	}
	for _, r := range id {
		if r < '0' || r > '9' {
			return "unknown"
		}
	}
	return id
}

// APIError is an `ok: false` answer: Telegram refused the request, so nothing
// was delivered.
type APIError struct {
	Method      string
	Code        int // 0 when absent
	Description string
	// RetryAfter is parameters.retry_after in seconds; nil when absent.
	RetryAfter *int64
}

func (e *APIError) Error() string {
	code := "unknown"
	if e.Code != 0 {
		code = strconv.Itoa(e.Code)
	}
	return fmt.Sprintf("Telegram API error for %s (%s): %s", e.Method, code, e.Description)
}

// FloodWait is the wait of a 429 refusal; ok is false for other refusals.
func (e *APIError) FloodWait() (time.Duration, bool) {
	if e.Code != 429 {
		return 0, false
	}
	secs, ok := int64(0), false
	if e.RetryAfter != nil {
		secs, ok = *e.RetryAfter, true
	} else {
		secs, ok = retryAfterFromDescription(e.Description)
	}
	if !ok {
		return FallbackRetryAfter, true
	}
	return time.Duration(max(secs, 1)) * time.Second, true
}

func retryAfterFromDescription(description string) (int64, bool) {
	lower := strings.ToLower(description)
	i := strings.Index(lower, "retry after ")
	if i < 0 {
		return 0, false
	}
	rest := lower[i+len("retry after "):]
	end := 0
	for end < len(rest) && rest[end] >= '0' && rest[end] <= '9' {
		end++
	}
	n, err := strconv.ParseInt(rest[:end], 10, 64)
	return n, err == nil
}

// TransportStage says whether a transport failure could have reached
// Telegram.
type TransportStage int

const (
	// NotSent: the request was never sent (connection or build failed).
	NotSent TransportStage = iota
	// Ambiguous: the request may have been processed (timeout, dropped
	// connection, unreadable answer).
	Ambiguous
)

// TransportError is a failed HTTP exchange. It never carries the URL,
// because Bot API URLs embed the token, nor the response body.
type TransportError struct {
	Method string
	Stage  TransportStage
	Detail string
}

func (e *TransportError) Error() string {
	if e.Stage == NotSent {
		return fmt.Sprintf("Telegram request for %s was not sent: %s", e.Method, e.Detail)
	}
	return fmt.Sprintf("Telegram request for %s got no readable answer and may have been delivered: %s", e.Method, e.Detail)
}

// FloodWaitActive is a call skipped locally because the chat is inside a
// flood wait Telegram announced earlier. Nothing was sent.
type FloodWaitActive struct {
	Method    string
	ChatID    int64
	Remaining time.Duration
}

func (e *FloodWaitActive) Error() string {
	secs := int64(e.Remaining / time.Second)
	if secs < 1 {
		secs = 1
	}
	return fmt.Sprintf("%s not sent: Telegram flood wait for chat %d has %ds left", e.Method, e.ChatID, secs)
}

// FailureClass is what a failed call means for delivery.
type FailureClass int

const (
	// ClassNotSent: never sent (local error, connection failure).
	ClassNotSent FailureClass = iota
	// ClassFloodWait: refused by flood control, or a known wait is running.
	ClassFloodWait
	// ClassRejected: refused for another reason.
	ClassRejected
	// ClassUnknown: may or may not have been delivered. Never resend.
	ClassUnknown
)

// Label is the delivery-record label of the class.
func (c FailureClass) Label() string {
	switch c {
	case ClassFloodWait:
		return "flood_wait"
	case ClassRejected:
		return "rejected"
	case ClassUnknown:
		return "unknown"
	default:
		return "not_sent"
	}
}

// DefinitelyUndelivered reports whether the item is known not delivered.
func (c FailureClass) DefinitelyUndelivered() bool { return c != ClassUnknown }

// Classify returns the class of a client error and, for flood waits, the
// wait. Errors without this package's types come from local work before any
// request, so they count as not sent; every network path must therefore
// return one of these types.
func Classify(err error) (FailureClass, time.Duration) {
	var api *APIError
	if errors.As(err, &api) {
		if wait, ok := api.FloodWait(); ok {
			return ClassFloodWait, wait
		}
		return ClassRejected, 0
	}
	var transport *TransportError
	if errors.As(err, &transport) {
		if transport.Stage == Ambiguous {
			return ClassUnknown, 0
		}
		return ClassNotSent, 0
	}
	var active *FloodWaitActive
	if errors.As(err, &active) {
		return ClassFloodWait, active.Remaining
	}
	return ClassNotSent, 0
}

// RetryPolicy bounds retries of important sends after a flood refusal.
type RetryPolicy struct {
	MaxAttempts int
	// Deadline bounds the time from the first attempt to the start of the
	// last one.
	Deadline time.Duration
}

// RetryImportant is the policy for reply text and files.
var RetryImportant = RetryPolicy{MaxAttempts: 3, Deadline: 2 * time.Minute}

// DelayBeforeRetry returns the wait before the next attempt, or false to
// give up. The wait is never shortened to fit the deadline.
func (p RetryPolicy) DelayBeforeRetry(attemptsMade int, elapsed, retryAfter time.Duration) (time.Duration, bool) {
	if attemptsMade >= p.MaxAttempts {
		return 0, false
	}
	if elapsed+retryAfter > p.Deadline {
		return 0, false
	}
	return retryAfter, true
}

// APIDescription is Telegram's description for a refusal anywhere in err.
func APIDescription(err error) (string, bool) {
	var api *APIError
	if errors.As(err, &api) {
		return api.Description, true
	}
	return "", false
}

// IsInvalidReplyError reports a refused send whose reply target vanished;
// resending without it is safe because the first request was refused.
func IsInvalidReplyError(err error) bool {
	d, ok := APIDescription(err)
	return ok && (strings.Contains(d, "REPLY_MESSAGE_ID_INVALID") || strings.Contains(d, "message to be replied not found"))
}

// IsHTMLParseError reports a refusal because Telegram could not parse the
// HTML markup.
func IsHTMLParseError(err error) bool {
	d, ok := APIDescription(err)
	return ok && (strings.Contains(d, "can't parse entities") || strings.Contains(d, "Can't find end of"))
}
