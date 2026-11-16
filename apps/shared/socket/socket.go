// Package socket is the shared transport to the engine service: one
// request line per Unix socket connection, newline-delimited JSON replies.
// It moves raw lines and never merges, drops or reorders them, so a
// streaming caller sees every reply the service wrote. It adds the checks
// every client shares: status, capabilities, and read-only views.
package socket

import (
	"bufio"
	"context"
	"errors"
	"fmt"
	"io"
	"net"
	"os"
	"syscall"
	"time"

	"github.com/tallesborges/zdx/apps/shared/protocol"
)

// ReplyTimeout is the longest wait for a one-shot reply, as in the Rust client.
const ReplyTimeout = 30 * time.Second

// MaxLine bounds one reply line. Agent events with message snapshots are
// stripped by the service, but tool output can still be large.
const MaxLine = 64 << 20

// ErrNotRunning means no service listens on the socket.
var ErrNotRunning = errors.New("engine service is not running")

// Request outcome classes, for callers that must not claim more than they
// know about a write. Every transport error of Open and Call wraps exactly
// one of them.
var (
	// ErrRequestNotSent: the request line was never written (encode or
	// connect failed). Nothing happened.
	ErrRequestNotSent = errors.New("request not sent")
	// ErrRequestOutcomeUnknown: the line may have reached the service (a
	// write error after connecting, or no readable reply). It may complete.
	ErrRequestOutcomeUnknown = errors.New("request outcome unknown")
)

// ErrNoAnswer means the request was sent and no reply came before the
// budget ended or the caller cancelled. The service may still complete it.
// The returned error also wraps the context's error, so a caller can tell
// its own cancellation (context.Canceled) from a timeout.
var ErrNoAnswer = fmt.Errorf("%w: the engine service did not answer in time; the request may still complete", ErrRequestOutcomeUnknown)

// noAnswer is ErrNoAnswer with the context error that ended the wait. Its
// text is ErrNoAnswer's, so messages built from it do not change.
type noAnswer struct{ cause error }

func (e noAnswer) Error() string   { return ErrNoAnswer.Error() }
func (e noAnswer) Unwrap() []error { return []error{ErrNoAnswer, e.cause} }

// Client talks to the engine service at Socket. One request per
// connection. A zero ReplyTimeout means ReplyTimeout.
type Client struct {
	Socket       string
	ReplyTimeout time.Duration
}

// NewClient returns a client for the socket at path.
func NewClient(socket string) *Client {
	return &Client{Socket: socket, ReplyTimeout: ReplyTimeout}
}

// Budget is the client's default reply budget.
func (c *Client) Budget() time.Duration {
	if c.ReplyTimeout > 0 {
		return c.ReplyTimeout
	}
	return ReplyTimeout
}

func (c *Client) dial(ctx context.Context) (net.Conn, error) {
	var d net.Dialer
	conn, err := d.DialContext(ctx, "unix", c.Socket)
	if err != nil {
		if errors.Is(err, os.ErrNotExist) || errors.Is(err, syscall.ENOENT) || errors.Is(err, syscall.ECONNREFUSED) {
			return nil, fmt.Errorf("%w (no listener at %s); start it with `zdx service start engine` or `zdx engine serve`", ErrNotRunning, c.Socket)
		}
		if errors.Is(err, syscall.EINVAL) && len(c.Socket) > 100 {
			return nil, fmt.Errorf("connect to %s: %w (the path is %d bytes; Unix socket paths are limited to about 104)", c.Socket, err, len(c.Socket))
		}
		return nil, fmt.Errorf("connect to %s: %w", c.Socket, err)
	}
	return conn, nil
}

// Open sends one request line (without its newline) and returns the reply
// stream. Cancelling ctx closes the connection.
func (c *Client) Open(ctx context.Context, line []byte) (*Stream, error) {
	conn, err := c.dial(ctx)
	if err != nil {
		return nil, fmt.Errorf("%w: %w", ErrRequestNotSent, err)
	}
	msg := make([]byte, 0, len(line)+1)
	msg = append(append(msg, line...), '\n')
	// Watch ctx before writing: a service that stops reading would block a
	// large request line, and only closing the connection ends that write.
	s := &Stream{conn: conn, done: make(chan struct{})}
	go func() {
		select {
		case <-ctx.Done():
			conn.Close()
		case <-s.done:
		}
	}()
	if _, err := conn.Write(msg); err != nil {
		s.Close()
		if ctxErr := ctx.Err(); ctxErr != nil {
			err = ctxErr
		}
		// Part or all of the line may have been read by the service.
		return nil, fmt.Errorf("%w: send request: %w", ErrRequestOutcomeUnknown, err)
	}
	s.scanner = bufio.NewScanner(conn)
	s.scanner.Buffer(make([]byte, 0, 64<<10), MaxLine)
	return s, nil
}

// Stream is the reply stream of one connection.
type Stream struct {
	conn    net.Conn
	scanner *bufio.Scanner
	done    chan struct{}
	closed  bool
}

// Next returns the next reply line (a copy), or io.EOF once the service
// closes the connection.
func (s *Stream) Next() ([]byte, error) {
	if !s.scanner.Scan() {
		if err := s.scanner.Err(); err != nil {
			return nil, fmt.Errorf("read reply: %w", err)
		}
		return nil, io.EOF
	}
	return append([]byte(nil), s.scanner.Bytes()...), nil
}

// Close closes the connection. It is safe to call more than once. A caller
// that must keep the service's state (a followed routed turn held for
// delivery) closes only after the service closed the connection.
func (s *Stream) Close() error {
	if s.closed {
		return nil
	}
	s.closed = true
	close(s.done)
	return s.conn.Close()
}

// Call sends one request line and returns its single reply line within
// budget (the client's default when budget is 0). Errors follow the outcome
// classes: ErrRequestNotSent before the write, ErrRequestOutcomeUnknown
// after it, and ErrNoAnswer when ctx or the budget ended first.
func (c *Client) Call(ctx context.Context, line []byte, budget time.Duration) ([]byte, error) {
	if budget <= 0 {
		budget = c.Budget()
	}
	ctx, cancel := context.WithTimeout(ctx, budget)
	defer cancel()
	s, err := c.Open(ctx, line)
	if err != nil {
		return nil, err
	}
	defer s.Close()
	reply, err := s.Next()
	switch {
	case err == nil:
		return reply, nil
	case errors.Is(err, io.EOF):
		return nil, fmt.Errorf("%w: engine service closed the connection without a reply", ErrRequestOutcomeUnknown)
	case ctx.Err() != nil:
		return nil, noAnswer{cause: ctx.Err()}
	default:
		return nil, fmt.Errorf("%w: %w", ErrRequestOutcomeUnknown, err)
	}
}

// Status returns the service status for one thread, or all when threadID
// is nil. An error reply is a *protocol.ServiceError.
func (c *Client) Status(ctx context.Context, threadID *string) (*protocol.StatusReport, error) {
	line, err := protocol.EncodeLine("status", protocol.Status{ThreadID: threadID})
	if err != nil {
		return nil, fmt.Errorf("%w: encode request: %w", ErrRequestNotSent, err)
	}
	reply, err := c.Call(ctx, line, 0)
	if err != nil {
		return nil, err
	}
	return protocol.DecodeStatusReply(reply)
}

// Require fails with *protocol.ErrUnsupported unless the service reports
// every capability; it names the first missing one. It asks on every call:
// the service can restart onto another build at any time, so a cached
// answer could send a field an older build ignores.
func (c *Client) Require(ctx context.Context, capabilities ...string) error {
	report, err := c.Status(ctx, nil)
	if err != nil {
		return err
	}
	for _, capability := range capabilities {
		if !report.Has(capability) {
			return &protocol.ErrUnsupported{Capability: capability, Protocol: report.Version()}
		}
	}
	return nil
}

// View runs one read-only view. It checks the view's capability first and
// sends nothing without it (*protocol.ErrUnsupported). A refusal is a
// *protocol.ServiceError whose Code maps to the route's HTTP status. The
// reply budget is protocol.ViewBudget.
func (c *Client) View(ctx context.Context, v protocol.View) (protocol.Output, error) {
	capability := protocol.ViewCapability(v.Op)
	if capability == "" {
		return protocol.Output{}, fmt.Errorf("%w: unknown view %q", ErrRequestNotSent, v.Op)
	}
	if err := c.Require(ctx, capability); err != nil {
		return protocol.Output{}, err
	}
	line, err := v.Encode()
	if err != nil {
		return protocol.Output{}, fmt.Errorf("%w: encode request: %w", ErrRequestNotSent, err)
	}
	reply, err := c.Call(ctx, line, protocol.ViewBudget(v.Op, c.Budget()))
	if err != nil {
		return protocol.Output{}, err
	}
	return protocol.DecodeViewReply(reply, v.Op)
}
