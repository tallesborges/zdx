// Package wire is a thin read-only client for the orchestrator socket
// ($ZDX_HOME/run/orchestrator.sock, NDJSON, one request per connection).
// crates/zdx-engine/src/orchestration/protocol.rs and views.rs are the source
// of truth. The monitor sends only `status` and view ops: it never submits,
// subscribes, cancels, or releases.
package wire

import (
	"bufio"
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"net"
	"os"
	"syscall"
	"time"
)

// Capability names (protocol.rs). Each view op needs one of the view
// capabilities; the client checks it before sending.
const (
	CapThreadViews  = "thread_views"
	CapMonitorViews = "monitor_views"
)

// View ops the monitor uses (views.rs ViewRequest).
const (
	ViewThreadsList        = "threads_list"
	ViewThreadView         = "thread_view"
	ViewThreadTrajectory   = "thread_trajectory"
	ViewMonitorSnapshot    = "monitor_snapshot"
	ViewActiveAgents       = "active_agents"
	ViewSubscriptionQuotas = "subscription_quotas"
	ViewWorkspaces         = "workspaces"
	ViewUsageReport        = "usage_report"
)

var viewCapability = map[string]string{
	ViewThreadsList:        CapThreadViews,
	ViewThreadView:         CapThreadViews,
	ViewThreadTrajectory:   CapThreadViews,
	ViewMonitorSnapshot:    CapMonitorViews,
	ViewActiveAgents:       CapMonitorViews,
	ViewSubscriptionQuotas: CapMonitorViews,
	ViewWorkspaces:         CapMonitorViews,
	ViewUsageReport:        CapMonitorViews,
}

// ViewCapability is the capability a view op needs, or "" for an op the
// monitor does not know.
func ViewCapability(op string) string { return viewCapability[op] }

// ReplyTimeout bounds one reply, as in the Rust client.
const ReplyTimeout = 30 * time.Second

// QuotasTimeout is subscription_quotas' budget: Rust asks every provider
// account over the network.
const QuotasTimeout = 2 * time.Minute

const maxLine = 64 << 20

// ErrNotRunning means nothing listens on the socket.
var ErrNotRunning = errors.New("orchestrator service is not running")

// ErrUnsupported means the running service lacks a capability. Nothing was
// sent.
type ErrUnsupported struct {
	Capability string
	Protocol   uint32
}

func (e *ErrUnsupported) Error() string {
	return fmt.Sprintf("the orchestrator service (protocol %d) does not report %s", e.Protocol, e.Capability)
}

// ServiceError is an error reply.
type ServiceError struct{ Message, Code string }

func (e *ServiceError) Error() string {
	if e.Code != "" {
		return e.Code + ": " + e.Message
	}
	return e.Message
}

// StatusReport is protocol.rs StatusReport (the fields the monitor shows).
type StatusReport struct {
	Protocol     *uint32        `json:"protocol,omitempty"`
	Capabilities []string       `json:"capabilities,omitempty"`
	Settled      bool           `json:"settled"`
	Threads      []ThreadStatus `json:"threads"`
	ViewsRoot    *string        `json:"views_root,omitempty"`
}

// Version is the protocol version; an absent field means 1.
func (r *StatusReport) Version() uint32 {
	if r.Protocol == nil {
		return 1
	}
	return *r.Protocol
}

// Has reports whether the service lists capability.
func (r *StatusReport) Has(capability string) bool {
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
	State        string           `json:"state"`
	RunningTurn  *uint64          `json:"running_turn"`
	QueuedInputs int              `json:"queued_inputs"`
	Route        *Route           `json:"route"`
	Settled      bool             `json:"settled"`
	Workers      []WorkerSnapshot `json:"workers"`
}

// Route is a Telegram chat and topic.
type Route struct {
	Chat  int64  `json:"chat"`
	Topic *int64 `json:"topic"`
}

// WorkerSnapshot is the part of a managed worker the monitor shows.
type WorkerSnapshot struct {
	ThreadID   string `json:"thread_id"`
	Status     string `json:"status"`
	QueueDepth int    `json:"queue_depth"`
}

// View is one view request. Op names it; set only the fields it takes.
type View struct {
	Op       string  `json:"-"`
	ThreadID *string `json:"thread_id,omitempty"`
	Kind     *string `json:"kind,omitempty"`
	After    *uint64 `json:"after,omitempty"`
	Days     *uint32 `json:"days,omitempty"`
}

// Encode is the request line: the fields plus `op`.
func (v View) Encode() ([]byte, error) {
	type plain View
	raw, err := json.Marshal(plain(v))
	if err != nil {
		return nil, err
	}
	var fields map[string]json.RawMessage
	if err := json.Unmarshal(raw, &fields); err != nil {
		return nil, err
	}
	op, _ := json.Marshal(v.Op)
	fields["op"] = op
	return json.Marshal(fields)
}

// Output is a view's result.
type Output struct {
	Body     json.RawMessage
	SourceID *string
}

type reply struct {
	Kind     string          `json:"reply"`
	Message  string          `json:"message,omitempty"`
	Code     string          `json:"code,omitempty"`
	Report   *StatusReport   `json:"report,omitempty"`
	Body     json.RawMessage `json:"body,omitempty"`
	SourceID *string         `json:"source_id,omitempty"`
}

// DecodeViewReply reads a `{"reply":"view",...}` line (also the stdout of
// `zdx views`).
func DecodeViewReply(line []byte, op string) (Output, error) {
	var r reply
	if err := json.Unmarshal(line, &r); err != nil {
		return Output{}, fmt.Errorf("decode reply to %s: %w", op, err)
	}
	if r.Kind == "error" {
		return Output{}, &ServiceError{Message: r.Message, Code: r.Code}
	}
	if r.Kind != "view" || len(r.Body) == 0 {
		return Output{}, fmt.Errorf("unexpected reply %q to %s", r.Kind, op)
	}
	return Output{Body: r.Body, SourceID: r.SourceID}, nil
}

// Client talks to one socket.
type Client struct {
	Socket       string
	ReplyTimeout time.Duration
}

// NewClient returns a client for the socket at path.
func NewClient(socket string) *Client {
	return &Client{Socket: socket, ReplyTimeout: ReplyTimeout}
}

func (c *Client) budget() time.Duration {
	if c.ReplyTimeout > 0 {
		return c.ReplyTimeout
	}
	return ReplyTimeout
}

func (c *Client) call(ctx context.Context, line []byte, budget time.Duration) ([]byte, error) {
	ctx, cancel := context.WithTimeout(ctx, budget)
	defer cancel()
	var d net.Dialer
	conn, err := d.DialContext(ctx, "unix", c.Socket)
	if err != nil {
		if errors.Is(err, os.ErrNotExist) || errors.Is(err, syscall.ENOENT) || errors.Is(err, syscall.ECONNREFUSED) {
			return nil, fmt.Errorf("%w (no listener at %s)", ErrNotRunning, c.Socket)
		}
		return nil, fmt.Errorf("connect to %s: %w", c.Socket, err)
	}
	defer conn.Close()
	stop := context.AfterFunc(ctx, func() { conn.Close() })
	defer stop()
	if _, err := conn.Write(append(line, '\n')); err != nil {
		return nil, fmt.Errorf("send request: %w", err)
	}
	scanner := bufio.NewScanner(conn)
	scanner.Buffer(make([]byte, 0, 64<<10), maxLine)
	if !scanner.Scan() {
		if ctx.Err() != nil {
			return nil, fmt.Errorf("the orchestrator service did not answer within %s", budget)
		}
		if err := scanner.Err(); err != nil {
			return nil, fmt.Errorf("read reply: %w", err)
		}
		return nil, fmt.Errorf("read reply: %w", io.ErrUnexpectedEOF)
	}
	return append([]byte(nil), scanner.Bytes()...), nil
}

// Status returns the service status of every thread.
func (c *Client) Status(ctx context.Context) (*StatusReport, error) {
	line, err := c.call(ctx, []byte(`{"op":"status"}`), c.budget())
	if err != nil {
		return nil, err
	}
	var r reply
	if err := json.Unmarshal(line, &r); err != nil {
		return nil, fmt.Errorf("decode status reply: %w", err)
	}
	if r.Kind == "error" {
		return nil, &ServiceError{Message: r.Message, Code: r.Code}
	}
	if r.Kind != "status" || r.Report == nil {
		return nil, fmt.Errorf("unexpected reply %q to status", r.Kind)
	}
	return r.Report, nil
}

// View runs one view after checking its capability. It asks for status on
// every call: the service can restart onto another build at any time.
func (c *Client) View(ctx context.Context, v View) (Output, error) {
	capability := viewCapability[v.Op]
	if capability == "" {
		return Output{}, fmt.Errorf("unknown view %q", v.Op)
	}
	report, err := c.Status(ctx)
	if err != nil {
		return Output{}, err
	}
	if !report.Has(capability) {
		return Output{}, &ErrUnsupported{Capability: capability, Protocol: report.Version()}
	}
	line, err := v.Encode()
	if err != nil {
		return Output{}, err
	}
	budget := c.budget()
	if v.Op == ViewSubscriptionQuotas {
		budget = QuotasTimeout
	}
	out, err := c.call(ctx, line, budget)
	if err != nil {
		return Output{}, err
	}
	return DecodeViewReply(out, v.Op)
}
