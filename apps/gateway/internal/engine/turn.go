package engine

import (
	"context"
	"errors"
	"fmt"
	"io"
	"log/slog"
	"sync"
	"time"
)

// TurnEnd is how a followed turn ended. Ran is false when no turn ran (the
// input was dropped, or the service went away first). Held means the service
// waits for Release before the thread's next turn.
type TurnEnd struct {
	TurnID uint64
	Held   bool
	Ran    bool
	// TouchedWorkers are turn_ended.touched_workers.
	TouchedWorkers []string
}

// Messages for turns that never report a result, as in the Rust bot.
const (
	MsgInputDropped   = "The engine service dropped this message before it ran."
	MsgConnectionLost = "Lost the connection to the engine service."
)

// KindConnectionLost is the status kind of a turn whose events stopped
// because the connection to the service went away: the turn itself may
// still finish in the service.
const KindConnectionLost = "connection_lost"

// CancelTarget names the work a turn's cancel stops: the submitted input
// (while queued or running) or a known turn.
type CancelTarget struct {
	InputID *uint64
	TurnID  *uint64
}

// Turn is one service turn seen through its events. Agent events arrive on
// Events, which closes when the turn ends; then Ended has the result. A turn
// that ends without turn_finished gets a synthetic one (interrupted after a
// cancel, otherwise failed), so the renderer always sees a terminal event.
type Turn struct {
	Events <-chan AgentEvent

	ended      chan TurnEnd
	end        TurnEnd
	endOnce    sync.Once
	cancelOnce sync.Once
	cancelReq  chan struct{}
}

// Cancel asks the service to stop the turn. Safe to call more than once.
func (t *Turn) Cancel() {
	t.cancelOnce.Do(func() { close(t.cancelReq) })
}

// Wait blocks until the turn ends and returns how.
func (t *Turn) Wait() TurnEnd {
	t.endOnce.Do(func() { t.end = <-t.ended })
	return t.end
}

// PumpTurn turns the service events of one turn into a Turn. in closes when
// the source is gone. Cancel requests go to client on their own connection.
func PumpTurn(client *Client, threadID string, target CancelTarget, in <-chan Event) *Turn {
	out := make(chan AgentEvent, 64)
	t := &Turn{Events: out, ended: make(chan TurnEnd, 1), cancelReq: make(chan struct{})}
	go func() {
		defer close(out)
		finished := false
		cancelSent := false
		var end TurnEnd
		var failure string
		cancelReq := t.cancelReq
	loop:
		for {
			select {
			case <-cancelReq:
				cancelReq = nil
				cancelSent = true
				go func() {
					ctx, cancel := context.WithTimeout(context.Background(), client.timeout())
					defer cancel()
					req := Cancel{ThreadID: threadID, InputID: target.InputID, TurnID: target.TurnID}
					if _, err := client.CancelWork(ctx, req); err != nil {
						slog.Warn("failed to cancel orchestrator turn", "thread", threadID, "err", err)
					}
				}()
			case ev, ok := <-in:
				if !ok {
					failure = MsgConnectionLost
					break loop
				}
				switch ev.Type {
				case EventAgent:
					a, err := ev.Agent()
					if err != nil {
						slog.Warn("undecodable agent event", "thread", threadID, "err", err)
						continue
					}
					if a.Type == AgentTurnFinished {
						finished = true
					}
					out <- a
				case EventTurnEnded:
					end = TurnEnd{TurnID: ev.TurnID, Held: ev.Held, Ran: true, TouchedWorkers: ev.TouchedWorkers}
					break loop
				case EventInputsDropped:
					failure = MsgInputDropped
					break loop
				}
			}
		}
		if !finished && failure != "" {
			status := &TurnStatus{Status: "failed", Kind: "internal", Message: failure}
			if failure == MsgConnectionLost {
				status.Kind = KindConnectionLost
			}
			if cancelSent {
				status = &TurnStatus{Status: "interrupted"}
			}
			out <- AgentEvent{Type: AgentTurnFinished, Status: status}
		}
		t.ended <- end
	}()
	return t
}

// Submission is what happened to a submitted message: followed (Turn set),
// or steered into the running turn (InTurn, whose renderer answers).
type Submission struct {
	ThreadID    string
	InputID     uint64
	QueuedAhead int
	InTurn      bool
	Turn        *Turn
	// Holder is this follower's hold token (capability hold_tokens, routed
	// submits): it owns the followed turn's hold.
	Holder string
}

// SubmitFollow submits with follow=turn and follows the turn that runs it.
// The connection is read until the service closes it, which for a routed
// turn is after Release; ctx cancellation closes it early and must only be
// used on shutdown.
func (c *Client) SubmitFollow(ctx context.Context, s Submit) (*Submission, error) {
	s.Follow = FollowTurn
	if s.Replay {
		if err := c.requireBeforeSubmit(ctx, CapHoldTokens); err != nil {
			return nil, err
		}
	}
	stream, err := c.Open(ctx, s)
	if err != nil {
		return nil, err
	}
	first, err := firstReply(c, stream)
	if err != nil {
		stream.Close()
		return nil, fmt.Errorf("%w: %w", ErrRequestOutcomeUnknown, err)
	}
	if err := first.Err(); err != nil {
		stream.Close()
		return nil, err
	}
	if first.Kind != ReplySubmitted || first.InputID == nil {
		stream.Close()
		return nil, fmt.Errorf("unexpected reply %q from the engine service", first.Kind)
	}
	sub := &Submission{ThreadID: first.ThreadID, InputID: *first.InputID, QueuedAhead: first.QueuedAhead, InTurn: first.InTurn, Holder: first.Holder}
	if first.InTurn {
		stream.Close()
		return sub, nil
	}
	events, out := Buffer()
	go func() {
		defer stream.Close()
		defer close(events)
		for {
			r, err := stream.Next()
			if err != nil {
				if !errors.Is(err, io.EOF) {
					slog.Warn("followed turn stream failed", "thread", sub.ThreadID, "err", err)
				}
				return
			}
			if r.Kind == ReplyEvent && r.Event != nil {
				events <- *r.Event
			}
		}
	}()
	inputID := sub.InputID
	sub.Turn = PumpTurn(c, sub.ThreadID, CancelTarget{InputID: &inputID}, out)
	return sub, nil
}

// requireBeforeSubmit is RequireAll ahead of a submit. A capability check
// that fails for any reason but a missing capability sent no submit, so its
// error is ErrRequestNotSent (the status call's own outcome does not
// matter): the caller may send the submit again.
func (c *Client) requireBeforeSubmit(ctx context.Context, capabilities ...string) error {
	err := c.RequireAll(ctx, capabilities...)
	var unsupported *ErrUnsupported
	if err == nil || errors.As(err, &unsupported) {
		return err
	}
	return fmt.Errorf("%w: capability check: %v", ErrRequestNotSent, err)
}

func firstReply(c *Client, s *Stream) (*Reply, error) {
	type result struct {
		r   *Reply
		err error
	}
	ch := make(chan result, 1)
	go func() {
		r, err := s.Next()
		ch <- result{r, err}
	}()
	select {
	case res := <-ch:
		if errors.Is(res.err, io.EOF) {
			return nil, errors.New("the engine service closed the connection")
		}
		return res.r, res.err
	case <-time.After(c.timeout()):
		return nil, errors.New("engine service did not reply in time")
	}
}

// SubmitOrdinary submits to an ordinary (non-profile) thread and follows the
// turn. It checks the ordinary_submit capability first and sends nothing to
// an older service, which would ignore the ordinary flag.
func (c *Client) SubmitOrdinary(ctx context.Context, s Submit) (*Submission, error) {
	if s.Profile != nil {
		return nil, errors.New("an ordinary submit must not name a profile")
	}
	if err := c.requireBeforeSubmit(ctx, CapOrdinarySubmit); err != nil {
		return nil, err
	}
	s.Ordinary = true
	return c.SubmitFollow(ctx, s)
}

// SubmitRerun runs an ordinary thread again on its persisted history (a
// retry after a failed turn) and follows the turn. It needs capabilities
// ordinary_submit and rerun; text and images are cleared and the delivery
// is queue, as Rust requires.
func (c *Client) SubmitRerun(ctx context.Context, s Submit) (*Submission, error) {
	if s.Profile != nil {
		return nil, errors.New("a rerun must not name a profile")
	}
	if err := c.requireBeforeSubmit(ctx, CapOrdinarySubmit, CapRerun); err != nil {
		return nil, err
	}
	s.Ordinary, s.Rerun = true, true
	s.Text, s.Images, s.Delivery = "", nil, DeliveryQueue
	return c.SubmitFollow(ctx, s)
}

// ReplayTurn is a held turn the service replays to this renderer: it ended
// with status and finalText (errMsg when failed) before its holder started
// delivery. Its one event is the terminal turn_finished; it is held.
func ReplayTurn(turnID uint64, status, finalText string, errMsg *string) *Turn {
	out := make(chan AgentEvent, 1)
	t := &Turn{Events: out, ended: make(chan TurnEnd, 1), cancelReq: make(chan struct{})}
	st := &TurnStatus{Status: status}
	if errMsg != nil {
		st.Message = *errMsg
	}
	out <- AgentEvent{Type: AgentTurnFinished, Status: st, FinalText: finalText}
	close(out)
	t.ended <- TurnEnd{TurnID: turnID, Held: true, Ran: true}
	return t
}
