// Package protocol is the shared Go side of the orchestrator socket wire
// format: newline-delimited JSON, one request per connection, at
// $ZDX_HOME/run/orchestrator.sock. The Rust definitions in
// crates/zdx-engine/src/orchestration/protocol.rs and views.rs are the
// source of truth. This package holds the types and encodings every Go
// client shares (status, capabilities, views); it does no I/O. Package
// socket sends them.
package protocol

import (
	"bytes"
	"encoding/json"
	"fmt"
	"path/filepath"
)

// SocketPath is the service socket for a ZDX home directory.
func SocketPath(zdxHome string) string {
	return filepath.Join(zdxHome, "run", "orchestrator.sock")
}

// EncodeLine renders one request line (without the newline): body's JSON
// object fields tagged with "op" first, like the Rust enum, without HTML
// escaping. body must encode to a JSON object.
func EncodeLine(op string, body any) ([]byte, error) {
	var buf bytes.Buffer
	enc := json.NewEncoder(&buf)
	enc.SetEscapeHTML(false)
	if err := enc.Encode(body); err != nil {
		return nil, err
	}
	fields := bytes.TrimRight(buf.Bytes(), "\n")
	if len(fields) < 2 || fields[0] != '{' {
		return nil, fmt.Errorf("request %s must encode to a JSON object", op)
	}
	tag, _ := json.Marshal(op)
	line := append([]byte(`{"op":`), tag...)
	if len(fields) > 2 {
		line = append(line, ',')
	}
	return append(line, fields[1:]...), nil
}

// Status asks for one thread, or every thread plus global settlement.
type Status struct {
	ThreadID *string `json:"thread_id,omitempty"`
}

// Route is the Telegram chat and topic a thread answers in.
type Route struct {
	Chat  int64  `json:"chat"`
	Topic *int64 `json:"topic"`
}

// ProtocolCapabilities is the first protocol version that reports
// capabilities.
const ProtocolCapabilities uint32 = 2

// Capability names the service reports in status once each lands. Check the
// exact capability before any new operation or field: known operations
// silently ignore unknown fields.
const (
	CapOrdinarySubmit  = "ordinary_submit"
	CapRerun           = "rerun"
	CapUpdateThread    = "update_thread"
	CapRecordDelivery  = "record_delivery"
	CapResetThread     = "reset_thread"
	CapGenerate        = "generate"
	CapFeedbackSubmit  = "feedback_submit"
	CapThreadViews     = "thread_views"
	CapThreadRoot      = "thread_root"
	CapMonitorViews    = "monitor_views"
	CapArtifactViews   = "artifact_views"
	CapComposeUserText = "compose_user_text"
	CapResolveThread   = "resolve_thread"
	CapReplyParts      = "reply_parts"
	CapPrepareImage    = "prepare_image"
	CapTranscribeAudio = "transcribe_audio"
	// CapHoldTokens: one renderer, hold tokens on releases, turn ids on
	// delivery records, replay of undelivered turns (Rust holds v7).
	CapHoldTokens = "hold_tokens"
	// CapWorkerMirrorRoute: WorkerSnapshot.Mirror names a worker's legacy
	// mirror topic.
	CapWorkerMirrorRoute = "worker_mirror_route"
	// CapPreparePhoto: prepare_photo, a reply photo made ready for
	// sendPhoto.
	CapPreparePhoto = "prepare_photo"
	// CapTouchedWorkers: turn_ended.touched_workers.
	CapTouchedWorkers = "touched_workers"
)

// StatusReport is the body of a status reply.
type StatusReport struct {
	// Protocol is the service protocol version; absent means 1.
	Protocol     *uint32        `json:"protocol,omitempty"`
	Capabilities []string       `json:"capabilities,omitempty"`
	Settled      bool           `json:"settled"`
	Threads      []ThreadStatus `json:"threads"`
	// ViewsRoot is the root the service's views read config, usage and the
	// Git fallback from (its --root), when it serves views.
	ViewsRoot *string `json:"views_root,omitempty"`
}

// Version is the report's protocol version (1 when absent).
func (r *StatusReport) Version() uint32 {
	if r == nil || r.Protocol == nil {
		return 1
	}
	return *r.Protocol
}

// Has reports whether the service supports a capability. A report older
// than ProtocolCapabilities has none.
func (r *StatusReport) Has(capability string) bool {
	if r.Version() < ProtocolCapabilities {
		return false
	}
	for _, c := range r.Capabilities {
		if c == capability {
			return true
		}
	}
	return false
}

// ThreadStatus is one thread the service knows.
type ThreadStatus struct {
	ThreadID     string           `json:"thread_id"`
	Profile      *string          `json:"profile"`
	State        string           `json:"state"` // idle, queued, running, delivering
	RunningTurn  *uint64          `json:"running_turn"`
	QueuedInputs int              `json:"queued_inputs"`
	Route        *Route           `json:"route"`
	Settled      bool             `json:"settled"`
	Workers      []WorkerSnapshot `json:"workers"`
}

// WorkerSnapshot is a point-in-time view of a managed worker. Raw keeps the
// full object so callers can pass it through unchanged.
type WorkerSnapshot struct {
	ThreadID        string  `json:"thread_id"`
	OwnerThreadID   string  `json:"owner_thread_id"`
	Root            string  `json:"root"`
	Status          string  `json:"status"`
	QueueDepth      int     `json:"queue_depth"`
	LatestFinalText *string `json:"latest_final_text"`
	LastError       *string `json:"last_error"`
	MirrorURL       *string `json:"mirror_url"`
	CurrentTool     *string `json:"current_tool"`
	// Mirror is the worker's legacy mirror topic (capability
	// worker_mirror_route); nil for a worker without one.
	Mirror *WorkerMirror `json:"mirror,omitempty"`

	Raw json.RawMessage `json:"-"`
}

// WorkerMirror is a worker's legacy mirror topic: the chat and topic the
// feed posts to, and the mirror thread `telegram-<chat>-topic-<topic>`.
type WorkerMirror struct {
	Chat     int64  `json:"chat"`
	Topic    int64  `json:"topic"`
	ThreadID string `json:"thread_id"`
}

func (w *WorkerSnapshot) UnmarshalJSON(data []byte) error {
	type plain WorkerSnapshot
	var p plain
	if err := json.Unmarshal(data, &p); err != nil {
		return err
	}
	*w = WorkerSnapshot(p)
	w.Raw = append(json.RawMessage(nil), data...)
	return nil
}

func (w WorkerSnapshot) MarshalJSON() ([]byte, error) {
	if len(w.Raw) > 0 {
		return w.Raw, nil
	}
	type plain WorkerSnapshot
	return json.Marshal(plain(w))
}

// ServiceError is a reply of kind "error": the service refused the request.
// Code is empty for older operations. Error is the message alone; a surface
// that shows the code formats it itself.
type ServiceError struct{ Message, Code string }

func (e *ServiceError) Error() string { return e.Message }

// ErrUnsupported means the running service lacks a capability the caller
// needs. Nothing was sent.
type ErrUnsupported struct {
	Capability string
	Protocol   uint32
}

func (e *ErrUnsupported) Error() string {
	return fmt.Sprintf("the orchestrator service (protocol %d) does not support %s yet; update and restart the orchestrator service", e.Protocol, e.Capability)
}

// Reply is the part of a reply line every client reads: its kind, an error
// reply's fields, a status report, and a view's body. Clients with more
// operations decode the line into their own wider type.
type Reply struct {
	Kind     string          `json:"reply"`
	Message  string          `json:"message,omitempty"`
	Code     string          `json:"code,omitempty"`
	Report   *StatusReport   `json:"report,omitempty"`
	Body     json.RawMessage `json:"body,omitempty"`
	SourceID *string         `json:"source_id,omitempty"`
}

// Reply kinds this package decodes.
const (
	ReplyError  = "error"
	ReplyStatus = "status"
	ReplyView   = "view"
)

// Err turns an error reply into a *ServiceError.
func (r *Reply) Err() error {
	if r.Kind == ReplyError {
		return &ServiceError{Message: r.Message, Code: r.Code}
	}
	return nil
}

// DecodeStatusReply reads the reply line to a status request.
func DecodeStatusReply(line []byte) (*StatusReport, error) {
	var r Reply
	if err := json.Unmarshal(line, &r); err != nil {
		return nil, fmt.Errorf("decode status reply: %w", err)
	}
	if err := r.Err(); err != nil {
		return nil, err
	}
	if r.Kind != ReplyStatus || r.Report == nil {
		return nil, fmt.Errorf("unexpected reply %q to status", r.Kind)
	}
	return r.Report, nil
}
