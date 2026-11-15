// Package engine is the chat TUI's side of the orchestrator socket: the
// requests an interactive terminal chat sends (submit, cancel,
// update_thread, catalog, generate, transcribe_audio, prepare_image) and the
// thread subscription it reads every event from. crates/zdx-engine/src/orchestration/protocol.rs is
// the source of truth; apps/shared/socket moves the lines and never drops,
// merges, or reorders them, and neither does this package: every event line
// of the subscription reaches the channel in order.
package engine

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"time"

	"github.com/tallesborges/zdx/apps/shared/protocol"
	"github.com/tallesborges/zdx/apps/shared/socket"
	"github.com/tallesborges/zdx/apps/shared/transcript"
)

// Required is what a chat needs before its first turn.
var Required = []string{
	protocol.CapOrdinarySubmit, protocol.CapChatSubmit, protocol.CapSubscribeThread,
	protocol.CapUpdateThread, protocol.CapCatalog, protocol.CapThreadViews,
}

// Client talks to the service at one socket for one project root.
type Client struct {
	sock *socket.Client
	// Root is the chat's root: config layers load from it, and a new
	// thread runs there.
	Root string
}

// New returns a client for the service of a ZDX home.
func New(home, root string) *Client {
	return &Client{sock: socket.NewClient(protocol.SocketPath(home)), Root: root}
}

// NewAt returns a client for an explicit socket path (tests).
func NewAt(sock, root string) *Client {
	return &Client{sock: socket.NewClient(sock), Root: root}
}

// Require fails unless the service reports every capability in caps.
func (c *Client) Require(ctx context.Context, caps ...string) error {
	return c.sock.Require(ctx, caps...)
}

// reply is a one-shot reply line.
type reply struct {
	protocol.Reply
	ThreadID    string          `json:"thread_id"`
	InputID     uint64          `json:"input_id"`
	QueuedAhead int             `json:"queued_ahead"`
	InTurn      bool            `json:"in_turn"`
	Event       json.RawMessage `json:"event"`
	Text        string          `json:"text"`
	Transcript  *string         `json:"transcript"`
	ModelLabel  *string         `json:"model_label"`
	Title       *string         `json:"title"`
	Image       *Image          `json:"image"`
	Delivery    *ImageDelivery  `json:"delivery"`
}

func (c *Client) call(ctx context.Context, op string, body any, want string, budget time.Duration) (*reply, error) {
	line, err := protocol.EncodeLine(op, body)
	if err != nil {
		return nil, fmt.Errorf("%w: encode request: %w", socket.ErrRequestNotSent, err)
	}
	raw, err := c.sock.Call(ctx, line, budget)
	if err != nil {
		return nil, err
	}
	var r reply
	if err := json.Unmarshal(raw, &r); err != nil {
		return nil, fmt.Errorf("decode %s reply: %w", op, err)
	}
	if err := r.Err(); err != nil {
		return nil, err
	}
	if r.Kind != want {
		return nil, fmt.Errorf("unexpected reply %q to %s", r.Kind, op)
	}
	return &r, nil
}

// Image is one inline image of a submit; Data is base64.
type Image struct {
	MimeType string `json:"mime_type"`
	Data     string `json:"data"`
}

// ImageDelivery is how prepare_image says the model gets an image: inline,
// or saved only with a reason.
type ImageDelivery struct {
	Type       string `json:"type"`
	Downscaled bool   `json:"downscaled,omitempty"`
	Reason     string `json:"reason,omitempty"`
}

// PrepareImage makes a local image ready for a submit: shrunk to the inline
// limits, or refused with the reason the model cannot see it.
func (c *Client) PrepareImage(ctx context.Context, path string) (*Image, error) {
	if err := c.Require(ctx, protocol.CapPrepareImage); err != nil {
		return nil, err
	}
	r, err := c.call(ctx, "prepare_image", map[string]string{"path": path}, "prepared_image", 0)
	if err != nil {
		return nil, err
	}
	if r.Image == nil {
		reason := "the model cannot see it inline"
		if r.Delivery != nil && r.Delivery.Reason != "" {
			reason = r.Delivery.Reason
		}
		return nil, errors.New(reason)
	}
	return r.Image, nil
}

// TranscribeAudio transcribes a local audio file with the root's
// transcription config. An empty text means no provider is configured.
func (c *Client) TranscribeAudio(ctx context.Context, path, mime string) (string, error) {
	if err := c.Require(ctx, protocol.CapTranscribeAudio); err != nil {
		return "", err
	}
	body := map[string]string{"path": path, "file_name": "voice_input.wav", "mime_type": mime, "root": c.Root}
	r, err := c.call(ctx, "transcribe_audio", body, "transcript", 2*time.Minute)
	if err != nil {
		return "", err
	}
	if r.Transcript == nil {
		return "", nil
	}
	return *r.Transcript, nil
}

// Cancel stops the thread's running turn and, with clearQueue, its queued
// inputs.
func (c *Client) Cancel(ctx context.Context, threadID string, clearQueue bool) error {
	_, err := c.call(ctx, "cancel", map[string]any{"thread_id": threadID, "clear_queue": clearQueue}, "cancelled", 0)
	return err
}

// Update is update_thread: each set field changes the thread. A pointer to
// nil clears the model or thinking level.
type Update struct {
	ThreadID   string   `json:"thread_id"`
	ConfigRoot string   `json:"config_root,omitempty"`
	Root       string   `json:"root,omitempty"`
	Model      **string `json:"model,omitempty"`
	Thinking   **string `json:"thinking,omitempty"`
	Title      **string `json:"title,omitempty"`
}

// Updated is what update_thread answered.
type Updated struct {
	ModelLabel string
	Title      string
}

// UpdateThread applies u; ConfigRoot defaults to the client's root.
func (c *Client) UpdateThread(ctx context.Context, u Update) (Updated, error) {
	if u.ConfigRoot == "" {
		u.ConfigRoot = c.Root
	}
	r, err := c.call(ctx, "update_thread", u, "thread_updated", 0)
	if err != nil {
		return Updated{}, err
	}
	var out Updated
	if r.ModelLabel != nil {
		out.ModelLabel = *r.ModelLabel
	}
	if r.Title != nil {
		out.Title = *r.Title
	}
	return out, nil
}

// GenerateTitle asks the service for a thread title from the first message.
func (c *Client) GenerateTitle(ctx context.Context, threadID, text string) (string, error) {
	if err := c.Require(ctx, protocol.CapGenerate); err != nil {
		return "", err
	}
	body := map[string]any{"kind": "title", "root": c.Root, "text": text, "thread_id": threadID}
	r, err := c.call(ctx, "generate", body, "generated", 2*time.Minute)
	if err != nil {
		return "", err
	}
	return r.Text, nil
}

// PickerModel is one model of the picker: its id and the spec to pick per
// thinking level display name.
type PickerModel struct {
	ID    string            `json:"id"`
	Name  string            `json:"name"`
	Specs map[string]string `json:"specs"`
}

// PickerMode is a configured mode and the selection a pick keeps.
type PickerMode struct {
	Name string `json:"name"`
	Ref  string `json:"ref"`
}

// Picker is the model picker of one thread (catalog picker, scope topic).
type Picker struct {
	ConfigError *string       `json:"config_error"`
	Models      []PickerModel `json:"models"`
	Modes       []PickerMode  `json:"modes"`
	Label       string        `json:"label"`
	// Override is the thread's stored selection (a spec or `mode:<name>`).
	Override string `json:"override"`
	Model    string `json:"model"`
	Thinking string `json:"thinking"`
}

// ThreadStatus is the catalog thread_status card: what a thread runs and
// how full its context is.
type ThreadStatus struct {
	ConfigError   *string `json:"config_error"`
	Refused       *string `json:"refused"`
	Model         string  `json:"model"`
	Thinking      string  `json:"thinking"`
	Root          string  `json:"root"`
	Branch        *string `json:"branch"`
	ContextLimit  uint64  `json:"context_limit"`
	ContextTokens uint64  `json:"context_tokens"`
}

func (c *Client) catalog(ctx context.Context, query map[string]any, out any) error {
	if err := c.Require(ctx, protocol.CapCatalog); err != nil {
		return err
	}
	line, err := protocol.EncodeLine("catalog", query)
	if err != nil {
		return fmt.Errorf("%w: encode request: %w", socket.ErrRequestNotSent, err)
	}
	raw, err := c.sock.Call(ctx, line, 0)
	if err != nil {
		return err
	}
	var r protocol.Reply
	if err := json.Unmarshal(raw, &r); err != nil {
		return fmt.Errorf("decode catalog reply: %w", err)
	}
	if err := r.Err(); err != nil {
		return err
	}
	if r.Kind != "catalog" {
		return fmt.Errorf("unexpected reply %q to catalog", r.Kind)
	}
	return json.Unmarshal(r.Body, out)
}

// Picker lists the models a thread can pick and its current selection.
func (c *Client) Picker(ctx context.Context, threadID string) (Picker, error) {
	var p Picker
	err := c.catalog(ctx, map[string]any{"kind": "picker", "root": c.Root, "scope": "topic", "thread_id": threadID}, &p)
	return p, err
}

// Status is the thread's status card.
func (c *Client) Status(ctx context.Context, threadID string) (ThreadStatus, error) {
	var s ThreadStatus
	err := c.catalog(ctx, map[string]any{"kind": "thread_status", "root": c.Root, "thread_id": threadID}, &s)
	return s, err
}

// Threads is the recent thread list (threads_list, at most 50).
func (c *Client) Threads(ctx context.Context) ([]transcript.ThreadListItem, error) {
	kind := "all"
	out, err := c.sock.View(ctx, protocol.View{Op: protocol.ViewThreadsList, Kind: &kind})
	if err != nil {
		return nil, err
	}
	var list transcript.ThreadList
	if err := json.Unmarshal(out.Body, &list); err != nil {
		return nil, fmt.Errorf("decode threads_list: %w", err)
	}
	return list.Threads, nil
}

// Thread is thread_view: the whole thread, or with after the items past
// that cursor (Partial). A thread that does not exist yet is not_found.
func (c *Client) Thread(ctx context.Context, id string, after *uint64) (transcript.Thread, error) {
	out, err := c.sock.View(ctx, protocol.View{Op: protocol.ViewThreadView, ThreadID: &id, After: after})
	if err != nil {
		return transcript.Thread{}, err
	}
	var th transcript.Thread
	if err := json.Unmarshal(out.Body, &th); err != nil {
		return th, fmt.Errorf("decode thread_view: %w", err)
	}
	if out.SourceID != nil {
		th.SourceID = *out.SourceID
	}
	return th, nil
}

// IsNotFound reports a not_found refusal.
func IsNotFound(err error) bool {
	var se *protocol.ServiceError
	return errors.As(err, &se) && se.Code == "not_found"
}

// Event is one streamed service event (protocol.rs Event, tag "type").
type Event struct {
	Type     string `json:"type"`
	ThreadID string `json:"thread_id"`
	TurnID   uint64 `json:"turn_id"`
	Inputs   []struct {
		InputID uint64 `json:"input_id"`
	} `json:"inputs"`
	InputIDs []uint64        `json:"input_ids"`
	Inner    json.RawMessage `json:"event"`
}

// Service event types.
const (
	EventTurnStarted   = "turn_started"
	EventAgent         = "agent"
	EventTurnEnded     = "turn_ended"
	EventInputsDropped = "inputs_dropped"
	EventSettled       = "settled"
)

// Agent decodes an agent event's payload.
func (e Event) Agent() (AgentEvent, error) {
	var a AgentEvent
	err := json.Unmarshal(e.Inner, &a)
	return a, err
}

// AgentEvent is the part of zdx_types::AgentEvent the chat renders.
type AgentEvent struct {
	Type  string          `json:"type"`
	Text  string          `json:"text"`
	ID    string          `json:"id"`
	Name  string          `json:"name"`
	Input json.RawMessage `json:"input"`
	Delta string          `json:"delta"`
	Chunk string          `json:"chunk"`
	// tool_completed: the ToolOutput envelope ({ok, data} or {ok, error}).
	Result *struct {
		OK    bool `json:"ok"`
		Error *struct {
			Code    string `json:"code"`
			Message string `json:"message"`
		} `json:"error"`
	} `json:"result"`
	DurationMS *uint64 `json:"duration_ms"`

	Kind    string  `json:"kind"`
	Message string  `json:"message"`
	Details *string `json:"details"`
	Attempt int     `json:"attempt"`
	Max     int     `json:"max_retries"`
	DelayMS uint64  `json:"delay_ms"`
	Source  string  `json:"source"`

	Status *struct {
		Status  string `json:"status"`
		Kind    string `json:"kind"`
		Message string `json:"message"`
	} `json:"status"`

	InputTokens      uint64 `json:"input_tokens"`
	OutputTokens     uint64 `json:"output_tokens"`
	CacheReadTokens  uint64 `json:"cache_read_input_tokens"`
	CacheWriteTokens uint64 `json:"cache_creation_input_tokens"`
	Model            string `json:"model"`
}

// Submitted is a submitted message: the thread (created when new), its
// input id, how many inputs are ahead of it, and whether it joined the
// running turn (InTurn: a steer).
type Submitted struct {
	ThreadID    string
	InputID     uint64
	QueuedAhead int
	InTurn      bool
}

// SubmitRequest is one chat message.
type SubmitRequest struct {
	ThreadID string
	Text     string
	Images   []Image
	// Steer joins the running turn at its next safe point; the service
	// queues it when no turn runs, the turn is closing, or images come.
	Steer bool
	// Rerun runs the saved history again without a new message.
	Rerun bool
}

type submitBody struct {
	ThreadID string  `json:"thread_id"`
	Root     string  `json:"root"`
	Ordinary bool    `json:"ordinary"`
	Chat     bool    `json:"chat"`
	Rerun    bool    `json:"rerun,omitempty"`
	Text     string  `json:"text"`
	Images   []Image `json:"images"`
	Follow   string  `json:"follow"`
	Delivery string  `json:"delivery"`
}

// Submit sends one message as an ordinary chat turn. Its events arrive on
// the thread's subscription (Subscribe), which must be open first. It
// checks the chat capability first: an older service ignores `chat` and
// would run the non-interactive layer.
func (c *Client) Submit(ctx context.Context, req SubmitRequest) (Submitted, error) {
	caps := []string{protocol.CapOrdinarySubmit, protocol.CapChatSubmit}
	if req.Rerun {
		caps = append(caps, protocol.CapRerun)
	}
	if err := c.Require(ctx, caps...); err != nil {
		return Submitted{}, err
	}
	body := submitBody{
		ThreadID: req.ThreadID, Root: c.Root, Ordinary: true, Chat: true, Rerun: req.Rerun,
		Text: req.Text, Images: req.Images, Follow: "none", Delivery: "queue",
	}
	if body.Images == nil {
		body.Images = []Image{}
	}
	if req.Steer {
		body.Delivery = "steer"
	}
	r, err := c.call(ctx, "submit", body, "submitted", 0)
	if err != nil {
		return Submitted{}, err
	}
	return Submitted{ThreadID: r.ThreadID, InputID: r.InputID, QueuedAhead: r.QueuedAhead, InTurn: r.InTurn}, nil
}

// Subscription is one thread's event stream: every event in the order the
// service emitted it. Events closes when the connection ends; Err then says
// why (nil for a cancelled context or a clean close).
type Subscription struct {
	Events <-chan Event
	Err    func() error
}

// Subscribe streams one thread's events (capability subscribe_thread) as a
// plain observer, never a renderer: it holds nothing. Cancelling ctx closes
// the connection.
func (c *Client) Subscribe(ctx context.Context, threadID string) (*Subscription, error) {
	if err := c.Require(ctx, protocol.CapSubscribeThread); err != nil {
		return nil, err
	}
	line, err := protocol.EncodeLine("subscribe", map[string]any{"renderer": false, "thread_id": threadID})
	if err != nil {
		return nil, fmt.Errorf("%w: encode request: %w", socket.ErrRequestNotSent, err)
	}
	stream, err := c.sock.Open(ctx, line)
	if err != nil {
		return nil, err
	}
	first, err := stream.Next()
	if err != nil {
		stream.Close()
		if errors.Is(err, io.EOF) {
			return nil, fmt.Errorf("%w: the orchestrator service closed the connection without a reply", socket.ErrRequestOutcomeUnknown)
		}
		return nil, fmt.Errorf("%w: %w", socket.ErrRequestOutcomeUnknown, err)
	}
	var r reply
	if err := json.Unmarshal(first, &r); err != nil {
		stream.Close()
		return nil, fmt.Errorf("decode subscribe reply: %w", err)
	}
	if err := r.Err(); err != nil {
		stream.Close()
		return nil, err
	}
	if r.Kind != "ok" {
		stream.Close()
		return nil, fmt.Errorf("unexpected reply %q to subscribe", r.Kind)
	}
	events := make(chan Event)
	var readErr error
	done := make(chan struct{})
	go func() {
		defer close(done)
		defer close(events)
		defer stream.Close()
		for {
			line, err := stream.Next()
			if err != nil {
				if !errors.Is(err, io.EOF) && ctx.Err() == nil {
					readErr = err
				}
				return
			}
			var r reply
			if err := json.Unmarshal(line, &r); err != nil {
				readErr = fmt.Errorf("decode event: %w", err)
				return
			}
			if r.Kind != "event" {
				continue
			}
			var ev Event
			if err := json.Unmarshal(r.Event, &ev); err != nil {
				readErr = fmt.Errorf("decode event: %w", err)
				return
			}
			select {
			case events <- ev:
			case <-ctx.Done():
				return
			}
		}
	}()
	return &Subscription{Events: events, Err: func() error { <-done; return readErr }}, nil
}
