// Package engine is the Go side of the Rust orchestrator service protocol:
// newline-delimited JSON over the Unix socket at
// $ZDX_HOME/run/orchestrator.sock. The Rust definitions in
// crates/zdx-engine/src/orchestration/protocol.rs are the source of truth;
// this package mirrors them and adds no engine behavior.
//
// A connection carries one request. Most requests get one reply and the
// connection closes. Submit with a follow mode and Subscribe keep the
// connection open and stream event replies.
package engine

import (
	"bytes"
	"encoding/json"
	"path/filepath"
)

// SocketPath is the service socket for a ZDX home directory.
func SocketPath(zdxHome string) string {
	return filepath.Join(zdxHome, "run", "orchestrator.sock")
}

// Route is the Telegram chat and topic a thread answers in.
type Route struct {
	Chat  int64  `json:"chat"`
	Topic *int64 `json:"topic"`
}

// Follow is what a submit connection streams after the reply.
type Follow string

const (
	FollowNone    Follow = "none"
	FollowTurn    Follow = "turn"
	FollowSettled Follow = "settled"
)

// Delivery is how a submit reaches the thread.
type Delivery string

const (
	DeliveryQueue Delivery = "queue"
	// DeliverySteer joins the running turn at its next safe point. With no
	// running turn, a closing one, or images, the service queues it.
	DeliverySteer Delivery = "steer"
)

// InputImage is one image of a submit; Data is base64.
type InputImage struct {
	MimeType string `json:"mime_type"`
	Data     string `json:"data"`
}

// Request is one of the request types below.
type Request interface{ op() string }

// Submit sends one message to a thread. Without Ordinary it targets a
// persistent-profile thread. With Ordinary (capability ordinary_submit) it
// runs a normal agent thread; Profile must be absent. Never set Ordinary
// directly: use Client.SubmitOrdinary, which checks the capability first,
// because an older service ignores unknown fields and would run a profile
// turn.
type Submit struct {
	ThreadID *string `json:"thread_id,omitempty"`
	Ordinary bool    `json:"ordinary,omitempty"`
	// Rerun runs an ordinary thread again on its persisted history without
	// a new user message (capability rerun). Text and Images must be empty;
	// never steered. Use Client.SubmitRerun.
	Rerun    bool         `json:"rerun,omitempty"`
	Root     string       `json:"root"`
	Profile  *string      `json:"profile,omitempty"`
	Model    *string      `json:"model,omitempty"`
	Route    *Route       `json:"route,omitempty"`
	Text     string       `json:"text"`
	Images   []InputImage `json:"images"`
	Follow   Follow       `json:"follow"`
	Delivery Delivery     `json:"delivery"`
	// Replay opts this follower's held turn in to replay when the follower
	// goes away before delivery started (capability hold_tokens). Set it
	// only with the capability: SubmitFollow checks.
	Replay bool `json:"replay,omitempty"`
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

// Status asks for one thread, or every thread plus global settlement.
type Status struct {
	ThreadID *string `json:"thread_id,omitempty"`
}

// Cancel stops work on a thread. With InputID or TurnID, only that work.
type Cancel struct {
	ThreadID   string  `json:"thread_id"`
	InputID    *uint64 `json:"input_id,omitempty"`
	TurnID     *uint64 `json:"turn_id,omitempty"`
	ClearQueue bool    `json:"clear_queue"`
}

// Release tells the service the renderer finished delivering a turn.
type Release struct {
	ThreadID string `json:"thread_id"`
	TurnID   uint64 `json:"turn_id"`
	// Holder is the hold's owner token (capability hold_tokens); a release
	// with another token is refused. The gateway never sends `force`.
	Holder string `json:"holder,omitempty"`
}

// Subscribe streams every event. A renderer holds Telegram-routed threads
// after each turn until it releases them; it is not a plain observer.
type Subscribe struct {
	Renderer bool `json:"renderer"`
	// Replay opts the renderer in to turn_replay (capability hold_tokens).
	Replay bool `json:"replay,omitempty"`
}

// Workers lists managed workers, all or of one owner.
type Workers struct {
	OwnerThreadID *string `json:"owner_thread_id,omitempty"`
}

// Worker asks for one managed worker.
type Worker struct {
	ThreadID string `json:"thread_id"`
}

// CancelWorker cancels one managed worker.
type CancelWorker struct {
	ThreadID string `json:"thread_id"`
}

// EnqueueFromTopic is a message typed in a worker's legacy mirror topic.
type EnqueueFromTopic struct {
	WorkerThreadID string `json:"worker_thread_id"`
	Prompt         string `json:"prompt"`
}

// FeedbackSubmit hands a Mini App artifact feedback submission to the
// service (capability feedback_submit, PLANNED). The caller has already
// authenticated the user and checked the allowlist and route. The service
// parses and validates the submission, saves it, writes the queued marker,
// and queues the routed input. Submission is the HTTP body text, sent as a
// JSON string so Rust parses it with its own rules. This string form is the
// Mini App owner's proposal and is not yet agreed with the Rust lead.
type FeedbackSubmit struct {
	ThreadID string `json:"thread_id"`
	UserID   int64  `json:"user_id"`
	// Root is the chat's root (required): its config gives the
	// allowlists and transcription, and the turn's config layers.
	Root       string `json:"root"`
	Submission string `json:"submission"`
}

func (FeedbackSubmit) op() string { return "feedback_submit" }

func (Submit) op() string           { return "submit" }
func (Status) op() string           { return "status" }
func (Cancel) op() string           { return "cancel" }
func (Release) op() string          { return "release" }
func (Subscribe) op() string        { return "subscribe" }
func (Workers) op() string          { return "workers" }
func (Worker) op() string           { return "worker" }
func (CancelWorker) op() string     { return "cancel_worker" }
func (EnqueueFromTopic) op() string { return "enqueue_from_topic" }

// EncodeRequest renders one request line (without the newline), tagged with
// "op" like the Rust enum, without HTML escaping.
func EncodeRequest(r Request) ([]byte, error) {
	if s, ok := r.(Submit); ok {
		// Rust's enums have no empty value: unset means the default.
		if s.Images == nil {
			s.Images = []InputImage{}
		}
		if s.Delivery == "" {
			s.Delivery = DeliveryQueue
		}
		if s.Follow == "" {
			s.Follow = FollowNone
		}
		r = s
	}
	var buf bytes.Buffer
	enc := json.NewEncoder(&buf)
	enc.SetEscapeHTML(false)
	if err := enc.Encode(r); err != nil {
		return nil, err
	}
	body := bytes.TrimRight(buf.Bytes(), "\n")
	op, _ := json.Marshal(r.op())
	line := append([]byte(`{"op":`), op...)
	if len(body) > 2 {
		line = append(line, ',')
	}
	return append(line, body[1:]...), nil
}

// Reply kinds.
const (
	ReplyError            = "error"
	ReplySubmitted        = "submitted"
	ReplyStatus           = "status"
	ReplyCancelled        = "cancelled"
	ReplyWorkers          = "workers"
	ReplyWorker           = "worker"
	ReplyOK               = "ok"
	ReplyEvent            = "event"
	ReplyFeedback         = "feedback"
	ReplyText             = "text"
	ReplyThreadRoute      = "thread_route"
	ReplyDeliveryRecorded = "delivery_recorded"
	ReplyReplyParts       = "reply_parts"
	ReplyView             = "view"
	// ReplySubscribed answers a renderer subscription with its holder token
	// (capability hold_tokens).
	ReplySubscribed = "subscribed"
)

// Reply is the flat union of every reply; Kind says which fields are set.
type Reply struct {
	Kind string `json:"reply"`

	// error; Code is set by newer operations (for example feedback_submit).
	Message string `json:"message,omitempty"`
	Code    string `json:"code,omitempty"`

	// submitted, cancelled
	ThreadID    string  `json:"thread_id,omitempty"`
	InputID     *uint64 `json:"input_id,omitempty"`
	QueuedAhead int     `json:"queued_ahead,omitempty"`
	InTurn      bool    `json:"in_turn,omitempty"`

	// status
	Report *StatusReport `json:"report,omitempty"`

	// cancelled
	CancelledTurn bool     `json:"cancelled_turn,omitempty"`
	DroppedInputs []uint64 `json:"dropped_inputs,omitempty"`

	// workers, worker
	Workers []WorkerSnapshot `json:"workers,omitempty"`
	Worker  *WorkerSnapshot  `json:"worker,omitempty"`

	// event
	Event *Event `json:"event,omitempty"`

	// text (compose_user_text), reply_parts (the reply Markdown)
	Text *string `json:"text,omitempty"`

	// thread_route (resolve_thread); ThreadID is the conversation's own id.
	EffectiveThreadID string  `json:"effective_thread_id,omitempty"`
	RouteKind         string  `json:"kind,omitempty"`
	Profile           *string `json:"profile,omitempty"`
	Exists            bool    `json:"exists,omitempty"`
	Root              *string `json:"root,omitempty"`
	PendingTopicTitle bool    `json:"pending_topic_title,omitempty"`

	// delivery_recorded
	UserSummary *string `json:"user_summary,omitempty"`

	// thread_updated
	ModelLabel *string `json:"model_label,omitempty"`
	Title      *string `json:"title,omitempty"`

	// reply_parts
	Followups []string     `json:"followups,omitempty"`
	Media     []ReplyMedia `json:"media,omitempty"`

	// prepared_image: Delivery is the ImageDelivery object; Image is set for
	// an inline delivery.
	Delivery json.RawMessage `json:"delivery,omitempty"`
	Image    *InputImage     `json:"image,omitempty"`

	// transcript: null when no transcription provider is configured.
	Transcript *string `json:"transcript,omitempty"`

	// submitted (a routed follow), subscribed (renderer): the connection's
	// hold token (capability hold_tokens). Tokens are only ever in these
	// direct replies.
	Holder string `json:"holder,omitempty"`

	// view: Body is the route's JSON body; SourceID is set by thread_view.
	Body     json.RawMessage `json:"body,omitempty"`
	SourceID *string         `json:"source_id,omitempty"`

	// feedback: FeedbackStatus is queued or duplicate; InputID is set when
	// queued. Annotations and Stale pass through unchanged.
	FeedbackStatus string          `json:"status,omitempty"`
	Annotations    json.RawMessage `json:"annotations,omitempty"`
	Stale          json.RawMessage `json:"stale,omitempty"`
	// feedback (queued only): the annotated artifact's name and where the
	// turn answers, for the chat notice.
	ArtifactName *string `json:"artifact_name,omitempty"`
	Route        *Route  `json:"route,omitempty"`

	// topic_start: Selection null means none; chat_model: the stored
	// selection.
	Selection *string `json:"selection,omitempty"`
	Label     *string `json:"label,omitempty"`

	// prepared_photo: Data (base64) is set only when Changed.
	Changed  bool    `json:"changed,omitempty"`
	FileName string  `json:"file_name,omitempty"`
	MimeType string  `json:"mime_type,omitempty"`
	Data     *string `json:"data,omitempty"`
}

// ServiceError is a reply of kind "error": the service refused the request.
// Code is empty for older operations.
type ServiceError struct{ Message, Code string }

func (e *ServiceError) Error() string { return e.Message }

// Err turns an error reply into a *ServiceError.
func (r *Reply) Err() error {
	if r.Kind == ReplyError {
		return &ServiceError{Message: r.Message, Code: r.Code}
	}
	return nil
}

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
	CapGitViews        = "git_views"
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

// ProtocolCapabilities is the first protocol version that reports
// capabilities.
const ProtocolCapabilities uint32 = 2

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

// Event types.
const (
	EventTurnStarted   = "turn_started"
	EventAgent         = "agent"
	EventTurnEnded     = "turn_ended"
	EventInputsDropped = "inputs_dropped"
	EventWorker        = "worker"
	EventSettled       = "settled"
	// EventTurnReplay hands this renderer (opted in with replay) a held turn
	// whose holder went away before delivery started (capability
	// hold_tokens). The hold now belongs to this renderer's subscription
	// token: post the reply to Route with the delivery rules, then release.
	EventTurnReplay = "turn_replay"
)

// Turn kinds.
const (
	TurnKindUser         = "user"
	TurnKindWorkerUpdate = "worker_update"
)

type InputRef struct {
	InputID uint64 `json:"input_id"`
}

// Event is the flat union of service events; Type says which fields are set.
// Inner holds the agent event (type "agent") or worker event (type "worker").
type Event struct {
	Type     string `json:"type"`
	ThreadID string `json:"thread_id,omitempty"`
	TurnID   uint64 `json:"turn_id,omitempty"`

	// turn_started
	Kind     string     `json:"kind,omitempty"`
	Inputs   []InputRef `json:"inputs,omitempty"`
	Route    *Route     `json:"route,omitempty"`
	Followed bool       `json:"followed,omitempty"`

	// turn_ended; TouchedWorkers (capability touched_workers) are the
	// workers the turn created or prompted, first touch first.
	Held           bool     `json:"held,omitempty"`
	TouchedWorkers []string `json:"touched_workers,omitempty"`

	// turn_replay: Route is set; Status is completed, failed or
	// interrupted (ReplayStatus); Error is the failure message when failed. Events never
	// carry a holder token.
	Status    string  `json:"status,omitempty"`
	FinalText *string `json:"final_text,omitempty"`
	Error     *string `json:"error,omitempty"`

	// inputs_dropped
	InputIDs []uint64 `json:"input_ids,omitempty"`

	// worker
	OwnerThreadID *string `json:"owner_thread_id,omitempty"`

	Inner json.RawMessage `json:"event,omitempty"`
}

// Owner is the thread the event belongs to (the owner, for worker events).
func (e *Event) Owner() string {
	if e.Type == EventWorker {
		if e.OwnerThreadID != nil {
			return *e.OwnerThreadID
		}
		return ""
	}
	return e.ThreadID
}

// Agent decodes the inner agent event of an "agent" event.
func (e *Event) Agent() (AgentEvent, error) {
	var a AgentEvent
	err := json.Unmarshal(e.Inner, &a)
	return a, err
}

// WorkerEvent decodes the inner worker event of a "worker" event.
func (e *Event) WorkerEvent() (WorkerEvent, error) {
	var w WorkerEvent
	err := json.Unmarshal(e.Inner, &w)
	return w, err
}

// Agent event types the gateway reads. Other types keep Type and Raw.
const (
	AgentTurnStarted         = "turn_started"
	AgentReasoningDelta      = "reasoning_delta"
	AgentAssistantDelta      = "assistant_delta"
	AgentAssistantCompleted  = "assistant_completed"
	AgentToolRequested       = "tool_requested"
	AgentToolInputCompleted  = "tool_input_completed"
	AgentToolStarted         = "tool_started"
	AgentToolCompleted       = "tool_completed"
	AgentError               = "error"
	AgentNotice              = "notice"
	AgentProviderRetry       = "provider_retry"
	AgentTurnFinished        = "turn_finished"
	AgentUserMessageInjected = "user_message_injected"
	AgentUsageUpdate         = "usage_update"
)

// AgentEvent is the subset of zdx_types::AgentEvent the gateway renders.
type AgentEvent struct {
	Type string `json:"type"`

	Text  string          `json:"text,omitempty"`  // deltas, completed, injected
	ID    string          `json:"id,omitempty"`    // tool events, injected
	Name  string          `json:"name,omitempty"`  // tool events
	Input json.RawMessage `json:"input,omitempty"` // tool_requested, tool_input_completed

	// error, notice, provider_retry: Kind is the category.
	Kind    string  `json:"kind,omitempty"`
	Message string  `json:"message,omitempty"`
	Details *string `json:"details,omitempty"`

	// turn_finished
	Status    *TurnStatus `json:"status,omitempty"`
	FinalText string      `json:"final_text,omitempty"`

	// user_message_injected
	Source string `json:"source,omitempty"`

	Raw json.RawMessage `json:"-"`
}

func (a *AgentEvent) UnmarshalJSON(data []byte) error {
	type plain AgentEvent
	var p plain
	if err := json.Unmarshal(data, &p); err != nil {
		return err
	}
	*a = AgentEvent(p)
	a.Raw = append(json.RawMessage(nil), data...)
	return nil
}

// TurnStatus is the terminal status of a turn: completed, interrupted, or
// failed with Kind/Message.
type TurnStatus struct {
	Status  string  `json:"status"`
	Kind    string  `json:"kind,omitempty"`
	Message string  `json:"message,omitempty"`
	Details *string `json:"details,omitempty"`
}

// Worker event kinds.
const (
	WorkerCreated      = "created"
	WorkerPrompted     = "prompted"
	WorkerActivity     = "activity"
	WorkerCompleted    = "completed"
	WorkerQueueChanged = "queue_changed"
)

// WorkerEvent is zdx_engine::core::workers::WorkerEvent (tag "kind"; the
// completed variant carries the CompletionEvent fields inline).
type WorkerEvent struct {
	Kind           string          `json:"kind"`
	OwnerThreadID  string          `json:"owner_thread_id,omitempty"`
	WorkerThreadID string          `json:"worker_thread_id"`
	Prompt         string          `json:"prompt,omitempty"`
	Activity       json.RawMessage `json:"activity,omitempty"`
	Status         string          `json:"status,omitempty"`
	FinalText      *string         `json:"final_text,omitempty"`
	Error          *string         `json:"error,omitempty"`
	MirrorURL      *string         `json:"mirror_url,omitempty"`
}
