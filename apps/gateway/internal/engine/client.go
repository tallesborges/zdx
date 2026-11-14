package engine

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

// ReplyTimeout is the longest wait for a one-shot reply, as in the Rust client.
const ReplyTimeout = 30 * time.Second

// maxLine bounds one reply line. Agent events with message snapshots are
// stripped by the service, but tool output can still be large.
const maxLine = 64 << 20

// Client talks to the orchestrator service. One request per connection.
type Client struct {
	Socket       string
	ReplyTimeout time.Duration
}

// NewClient returns a client for the socket at path.
func NewClient(socket string) *Client {
	return &Client{Socket: socket, ReplyTimeout: ReplyTimeout}
}

// ErrNotRunning means no service listens on the socket.
var ErrNotRunning = errors.New("orchestrator service is not running")

// ErrUnsupported means the running service lacks a capability the caller
// needs. Nothing was sent.
type ErrUnsupported struct {
	Capability string
	Protocol   uint32
}

func (e *ErrUnsupported) Error() string {
	return fmt.Sprintf("the orchestrator service (protocol %d) does not support %s yet; update and restart the orchestrator service", e.Protocol, e.Capability)
}

func (c *Client) dial(ctx context.Context) (net.Conn, error) {
	var d net.Dialer
	conn, err := d.DialContext(ctx, "unix", c.Socket)
	if err != nil {
		if errors.Is(err, os.ErrNotExist) || errors.Is(err, syscall.ENOENT) || errors.Is(err, syscall.ECONNREFUSED) {
			return nil, fmt.Errorf("%w (no listener at %s); start it with `zdx service start orchestrator` or `zdx orchestrator serve`", ErrNotRunning, c.Socket)
		}
		if errors.Is(err, syscall.EINVAL) && len(c.Socket) > 100 {
			return nil, fmt.Errorf("connect to %s: %w (the path is %d bytes; Unix socket paths are limited to about 104)", c.Socket, err, len(c.Socket))
		}
		return nil, fmt.Errorf("connect to %s: %w", c.Socket, err)
	}
	return conn, nil
}

func (c *Client) timeout() time.Duration {
	if c.ReplyTimeout > 0 {
		return c.ReplyTimeout
	}
	return ReplyTimeout
}

// Per-operation reply budgets for operations that can wait for work in the
// service. Rust's image normalization waits up to 60 seconds for a slot
// (core/artifact_feedback/canonical.rs SLOT_WAIT) before it decodes and
// encodes, so these get twice that.
const (
	FeedbackTimeout  = 2 * time.Minute
	CanonicalTimeout = 2 * time.Minute
	// TranscribeTimeout bounds one transcription: a provider call on a file
	// of up to 25 MiB.
	TranscribeTimeout = 5 * time.Minute
)

// Request outcome classes, for callers that must not claim more than they
// know about a write. Every transport error of Open, Call and CallWithin
// wraps exactly one of them.
var (
	// ErrRequestNotSent: the request line was never written (encode or
	// connect failed). Nothing happened.
	ErrRequestNotSent = errors.New("request not sent")
	// ErrRequestOutcomeUnknown: the line may have reached the service (a
	// write error after connecting, or no readable reply). It may complete.
	ErrRequestOutcomeUnknown = errors.New("request outcome unknown")
)

// ErrNoAnswer means the request was sent and no reply came in time. The
// service may still complete it.
var ErrNoAnswer = fmt.Errorf("%w: the orchestrator service did not answer in time; the request may still complete", ErrRequestOutcomeUnknown)

// Call sends one request and waits for its reply. An error reply is returned
// as a value; use Reply.Err or CallOK to turn it into an error.
func (c *Client) Call(ctx context.Context, req Request) (*Reply, error) {
	return c.CallWithin(ctx, req, c.timeout())
}

// CallWithin is Call with its own reply budget.
func (c *Client) CallWithin(ctx context.Context, req Request, budget time.Duration) (*Reply, error) {
	ctx, cancel := context.WithTimeout(ctx, budget)
	defer cancel()
	s, err := c.Open(ctx, req)
	if err != nil {
		return nil, err
	}
	defer s.Close()
	reply, err := s.Next()
	switch {
	case err == nil:
		return reply, nil
	case errors.Is(err, io.EOF):
		return nil, fmt.Errorf("%w: orchestrator service closed the connection without a reply", ErrRequestOutcomeUnknown)
	case ctx.Err() != nil:
		return nil, ErrNoAnswer
	default:
		return nil, fmt.Errorf("%w: %w", ErrRequestOutcomeUnknown, err)
	}
}

// CallOK is Call that also fails on an error reply.
func (c *Client) CallOK(ctx context.Context, req Request) (*Reply, error) {
	reply, err := c.Call(ctx, req)
	if err != nil {
		return nil, err
	}
	if err := reply.Err(); err != nil {
		return nil, err
	}
	return reply, nil
}

// Open sends a request whose reply is a stream (Submit with a follow mode,
// Subscribe). Cancelling ctx closes the connection.
func (c *Client) Open(ctx context.Context, req Request) (*Stream, error) {
	line, err := EncodeRequest(req)
	if err != nil {
		return nil, fmt.Errorf("%w: encode request: %w", ErrRequestNotSent, err)
	}
	conn, err := c.dial(ctx)
	if err != nil {
		return nil, fmt.Errorf("%w: %w", ErrRequestNotSent, err)
	}
	if _, err := conn.Write(append(line, '\n')); err != nil {
		conn.Close()
		// Part or all of the line may have been read by the service.
		return nil, fmt.Errorf("%w: send request: %w", ErrRequestOutcomeUnknown, err)
	}
	s := &Stream{conn: conn, scanner: bufio.NewScanner(conn), done: make(chan struct{})}
	s.scanner.Buffer(make([]byte, 0, 64<<10), maxLine)
	go func() {
		select {
		case <-ctx.Done():
			conn.Close()
		case <-s.done:
		}
	}()
	return s, nil
}

// Stream is the reply stream of one connection.
type Stream struct {
	conn    net.Conn
	scanner *bufio.Scanner
	done    chan struct{}
	closed  bool
}

// Next returns the next reply, or io.EOF once the service closes the
// connection.
func (s *Stream) Next() (*Reply, error) {
	if !s.scanner.Scan() {
		if err := s.scanner.Err(); err != nil {
			return nil, fmt.Errorf("read reply: %w", err)
		}
		return nil, io.EOF
	}
	var r Reply
	if err := json.Unmarshal(s.scanner.Bytes(), &r); err != nil {
		return nil, fmt.Errorf("decode orchestrator reply: %w", err)
	}
	return &r, nil
}

// Close closes the connection. For a followed routed turn, close only after
// the service closed it (after Release): closing early lets the next turn
// start before delivery records land.
func (s *Stream) Close() error {
	if s.closed {
		return nil
	}
	s.closed = true
	close(s.done)
	return s.conn.Close()
}

// StatusReport returns the service status for one thread or all.
func (c *Client) StatusReport(ctx context.Context, threadID *string) (*StatusReport, error) {
	reply, err := c.CallOK(ctx, Status{ThreadID: threadID})
	if err != nil {
		return nil, err
	}
	if reply.Kind != ReplyStatus || reply.Report == nil {
		return nil, fmt.Errorf("unexpected reply %q to status", reply.Kind)
	}
	return reply.Report, nil
}

// Require fails with *ErrUnsupported unless the service reports capability.
// It asks on every call: the service can restart onto another build at any
// time, so a cached answer could send a field an older build ignores.
func (c *Client) Require(ctx context.Context, capability string) error {
	return c.RequireAll(ctx, capability)
}

// RequireAll is Require for several capabilities with one status call. It
// names the first missing one.
func (c *Client) RequireAll(ctx context.Context, capabilities ...string) error {
	report, err := c.StatusReport(ctx, nil)
	if err != nil {
		return err
	}
	for _, capability := range capabilities {
		if !report.Has(capability) {
			return &ErrUnsupported{Capability: capability, Protocol: report.Version()}
		}
	}
	return nil
}

// Release tells the service the renderer finished a held turn.
func (c *Client) Release(ctx context.Context, threadID string, turnID uint64) error {
	return c.ReleaseHeld(ctx, threadID, turnID, "")
}

// ReleaseHeld releases a hold as its owner (capability hold_tokens when
// holder is set). An empty holder is a release without a token, for
// services before hold_tokens.
func (c *Client) ReleaseHeld(ctx context.Context, threadID string, turnID uint64, holder string) error {
	if holder != "" {
		if err := c.Require(ctx, CapHoldTokens); err != nil {
			return err
		}
	}
	_, err := c.CallOK(ctx, Release{ThreadID: threadID, TurnID: turnID, Holder: holder})
	return err
}

// CancelWork sends a cancel and returns the service's answer.
func (c *Client) CancelWork(ctx context.Context, req Cancel) (*Reply, error) {
	return c.CallOK(ctx, req)
}

// ListWorkers lists managed workers, all or of one owner.
func (c *Client) ListWorkers(ctx context.Context, owner *string) ([]WorkerSnapshot, error) {
	reply, err := c.CallOK(ctx, Workers{OwnerThreadID: owner})
	if err != nil {
		return nil, err
	}
	if reply.Kind != ReplyWorkers {
		return nil, fmt.Errorf("unexpected reply %q to workers", reply.Kind)
	}
	return reply.Workers, nil
}

// GetWorker returns one managed worker, or nil when unmanaged.
func (c *Client) GetWorker(ctx context.Context, threadID string) (*WorkerSnapshot, error) {
	reply, err := c.CallOK(ctx, Worker{ThreadID: threadID})
	if err != nil {
		return nil, err
	}
	if reply.Kind != ReplyWorker {
		return nil, fmt.Errorf("unexpected reply %q to worker", reply.Kind)
	}
	return reply.Worker, nil
}

// WorkerMirror returns a worker's legacy mirror topic, nil when it has
// none or is not managed. It needs capability worker_mirror_route.
func (c *Client) WorkerMirror(ctx context.Context, threadID string) (*WorkerMirror, error) {
	if err := c.Require(ctx, CapWorkerMirrorRoute); err != nil {
		return nil, err
	}
	w, err := c.GetWorker(ctx, threadID)
	if err != nil || w == nil {
		return nil, err
	}
	return w.Mirror, nil
}

// CancelWorker stops one managed worker and returns its snapshot.
func (c *Client) CancelWorker(ctx context.Context, threadID string) (*WorkerSnapshot, error) {
	reply, err := c.CallOK(ctx, CancelWorker{ThreadID: threadID})
	if err != nil {
		return nil, err
	}
	if reply.Kind != ReplyWorker || reply.Worker == nil {
		return nil, fmt.Errorf("unexpected reply %q to cancel_worker", reply.Kind)
	}
	return reply.Worker, nil
}

// SubmitFeedback sends an artifact feedback submission. It needs capability
// feedback_submit; a refusal comes back as *ServiceError with its Code. The
// reply budget is FeedbackTimeout; callers pass a context without client
// cancellation, because the service keeps processing a submission whose
// client went away. ErrNoAnswer then means the outcome is unknown.
//
// Error classes: *ErrUnsupported, or ErrRequestNotSent when the capability
// check fails (nothing was written); *ServiceError for a refusal (nothing
// queued); ErrRequestOutcomeUnknown for anything after the write, including
// an unexpected reply.
func (c *Client) SubmitFeedback(ctx context.Context, req FeedbackSubmit) (*Reply, error) {
	if err := c.Require(ctx, CapFeedbackSubmit); err != nil {
		var unsupported *ErrUnsupported
		if errors.As(err, &unsupported) || errors.Is(err, ErrRequestNotSent) {
			return nil, err
		}
		return nil, fmt.Errorf("%w: capability check: %w", ErrRequestNotSent, err)
	}
	reply, err := c.CallWithin(ctx, req, FeedbackTimeout)
	if err != nil {
		return nil, err
	}
	if err := reply.Err(); err != nil {
		return nil, err
	}
	if reply.Kind != ReplyFeedback {
		return nil, fmt.Errorf("%w: unexpected reply %q to feedback_submit", ErrRequestOutcomeUnknown, reply.Kind)
	}
	return reply, nil
}
