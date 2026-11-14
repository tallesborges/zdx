// Package wire is the monitor's read-only client for the orchestrator
// socket ($ZDX_HOME/run/orchestrator.sock, NDJSON, one request per
// connection). The transport and wire types are the shared module's
// (apps/shared: protocol, socket); this package re-exports what the monitor
// uses and keeps its read-only surface: it sends only `status` and view ops,
// never a submit, subscribe, cancel, or release.
package wire

import (
	"context"
	"errors"
	"time"

	"github.com/tallesborges/zdx/apps/shared/protocol"
	"github.com/tallesborges/zdx/apps/shared/socket"
)

// Shared wire types (package protocol).
type (
	StatusReport   = protocol.StatusReport
	ThreadStatus   = protocol.ThreadStatus
	Route          = protocol.Route
	WorkerSnapshot = protocol.WorkerSnapshot
	View           = protocol.View
	Output         = protocol.Output
	ServiceError   = protocol.ServiceError
	ErrUnsupported = protocol.ErrUnsupported
)

// Capabilities the monitor's views need.
const (
	CapThreadViews  = protocol.CapThreadViews
	CapMonitorViews = protocol.CapMonitorViews
)

// View ops the monitor uses.
const (
	ViewThreadsList      = protocol.ViewThreadsList
	ViewThreadView       = protocol.ViewThreadView
	ViewThreadTrajectory = protocol.ViewThreadTrajectory
	ViewMonitorSnapshot  = protocol.ViewMonitorSnapshot
	ViewActiveAgents     = protocol.ViewActiveAgents
	ViewWorkspaces       = protocol.ViewWorkspaces
	ViewUsageReport      = protocol.ViewUsageReport
)

// ReplyTimeout bounds one reply.
const ReplyTimeout = socket.ReplyTimeout

// QuotasTimeout is the budget for one in-process quota.Fetch
// (source.Quotas), which asks every provider account over the network. It
// is not a socket reply budget.
const QuotasTimeout = 2 * time.Minute

// ErrNotRunning means nothing listens on the socket. Nothing was sent.
var ErrNotRunning = socket.ErrNotRunning

// ViewCapability is the capability a view op needs, or "" for an unknown op.
func ViewCapability(op string) string { return protocol.ViewCapability(op) }

// SocketPath is the orchestrator socket of a ZDX home.
func SocketPath(home string) string { return protocol.SocketPath(home) }

// DecodeViewReply reads a `{"reply":"view",...}` line (also the stdout of
// `zdx views`). An error reply's text carries its code.
func DecodeViewReply(line []byte, op string) (Output, error) {
	out, err := protocol.DecodeViewReply(line, op)
	return out, monitorError(err)
}

// Client talks to one socket. A zero ReplyTimeout means ReplyTimeout.
type Client struct {
	Socket       string
	ReplyTimeout time.Duration
}

// NewClient returns a client for the socket at path.
func NewClient(socketPath string) *Client {
	return &Client{Socket: socketPath, ReplyTimeout: ReplyTimeout}
}

func (c *Client) shared() *socket.Client {
	return &socket.Client{Socket: c.Socket, ReplyTimeout: c.ReplyTimeout}
}

// Status returns the service status of every thread.
func (c *Client) Status(ctx context.Context) (*StatusReport, error) {
	report, err := c.shared().Status(ctx, nil)
	return report, monitorError(err)
}

// View runs one view after checking its capability; without it nothing is
// sent (*ErrUnsupported). It asks for status on every call: the service can
// restart onto another build at any time.
func (c *Client) View(ctx context.Context, v View) (Output, error) {
	out, err := c.shared().View(ctx, v)
	return out, monitorError(err)
}

// monitorError rewords two shared errors for the screen and keeps the
// original in the chain, so errors.Is and errors.As still match it.
func monitorError(err error) error {
	var se *protocol.ServiceError
	switch {
	case err == nil:
		return nil
	case errors.As(err, &se) && se.Code != "":
		// The shared Error is the message alone; the monitor shows the code.
		return &codedError{se}
	case errors.Is(err, socket.ErrNoAnswer):
		// A view is read-only, so "the request may still complete" does
		// not apply.
		return &noAnswer{err}
	}
	return err
}

type codedError struct{ se *protocol.ServiceError }

func (e *codedError) Error() string { return e.se.Code + ": " + e.se.Message }
func (e *codedError) Unwrap() error { return e.se }

type noAnswer struct{ cause error }

func (e *noAnswer) Error() string { return "the orchestrator service did not answer in time" }
func (e *noAnswer) Unwrap() error { return e.cause }
