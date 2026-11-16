package bot

import (
	"context"
	"errors"
	"fmt"
	"log/slog"
	"time"

	"github.com/tallesborges/zdx/apps/gateway/internal/telegram"
)

// UpdateSource is the getUpdates side of the Telegram client.
type UpdateSource interface {
	GetUpdates(ctx context.Context, offset *int64, timeout time.Duration) ([]telegram.Update, error)
}

// Receipt is when and where an update arrived, for the intake log.
type Receipt struct {
	UpdateID   int64
	ReceivedAt time.Time
	PollIndex  int
	PollSize   int
}

// UpdateHandler gets every update in order. It runs on the polling loop, so
// it must hand work off instead of waiting on Telegram sends or turns.
type UpdateHandler interface {
	HandleMessage(ctx context.Context, m *telegram.Message, r Receipt)
	HandleCallback(ctx context.Context, q *telegram.CallbackQuery)
}

// PollErrorDelay is the pause after a failed poll.
const PollErrorDelay = time.Second

// Poller runs the getUpdates loop.
//
// The offset lives in memory only and starts unset, as in the Rust bot: it
// moves to update_id+1 before each update is handled, and updates Telegram
// has not seen confirmed by a later poll are redelivered after a restart.
// Confirm closes that window on a clean stop. There is no update-id
// deduplication and no delivery guarantee: a crash before the next poll
// redelivers the last batch (work already submitted runs again), and a
// crash after it loses handed-off work not yet submitted.
//
// A 409 Conflict (another getUpdates or a webhook on the token) stops Run
// with ErrPollConflict: two pollers would split the updates.
type Poller struct {
	Source  UpdateSource
	Handler UpdateHandler
	Log     *slog.Logger
	// Timeout is the long-poll timeout (default telegram.PollTimeout).
	Timeout time.Duration
	// ErrorDelay is the pause after a failed poll (default PollErrorDelay).
	ErrorDelay time.Duration
	Now        func() time.Time
	// Admission gates each update before it is handed off. An update the
	// poller cannot hand off (admission shut, or Run stopping) does not
	// move the offset, so it is never confirmed and Telegram sends it
	// again to the next process.
	Admission *Admission
	// Work is the context handlers get (default Run's). A gateway stops
	// polling first and lets handed-off work finish, so it passes a context
	// that outlives polling.
	Work context.Context
	// Ready, when set, must pass before a non-empty batch is handed off
	// (the engine service answers). While it fails the poller waits
	// and does not poll again, so Telegram keeps the batch unconfirmed: a
	// service that is already down delays the batch instead of failing it,
	// and a stop or crash meanwhile gets the batch again. It is one check,
	// not a durability guarantee: once the batch is handed off the next poll
	// confirms it, and the windows in the Poller doc apply unchanged (the
	// service can still go down right after the check).
	Ready func(ctx context.Context) error
	// ReadyDelay is the first pause between Ready checks (default
	// PollErrorDelay); it doubles up to ReadyMaxDelay.
	ReadyDelay time.Duration

	offset *int64
}

// ReadyMaxDelay is the longest pause between Ready checks.
const ReadyMaxDelay = 5 * time.Second

// waitReady blocks until Ready passes. It returns false when ctx ends first.
func (p *Poller) waitReady(ctx context.Context, log *slog.Logger) bool {
	if p.Ready == nil {
		return true
	}
	delay := p.ReadyDelay
	if delay == 0 {
		delay = PollErrorDelay
	}
	waiting := false
	for {
		err := p.Ready(ctx)
		if err == nil {
			if waiting {
				log.Info("engine service answers again; handing off held updates")
			}
			return true
		}
		if ctx.Err() != nil {
			return false
		}
		if !waiting {
			log.Warn("engine service unavailable; holding updates unconfirmed until it answers", "err", err)
			waiting = true
		}
		select {
		case <-ctx.Done():
			return false
		case <-time.After(delay):
		}
		delay = min(delay*2, ReadyMaxDelay)
	}
}

// ErrPollConflict: Telegram refused the poll because another poller or a
// webhook uses the token.
var ErrPollConflict = errors.New("another poller or a webhook uses this bot token")

// Confirm tells Telegram every update this poller handed off is done: one
// poll with the current offset and no wait. Updates it returns are not
// handed off and not confirmed: they stay for the next process. Call it
// after Run returned, and only once the handed-off work was submitted
// (a complete drain); otherwise a stop would lose what a crash redelivers.
func (p *Poller) Confirm(ctx context.Context) error {
	if p.offset == nil {
		return nil
	}
	_, err := p.Source.GetUpdates(ctx, p.offset, 0)
	return err
}

// Offset is the next offset the poller will send; nil before any update.
func (p *Poller) Offset() *int64 { return p.offset }

// Run polls until ctx ends.
func (p *Poller) Run(ctx context.Context) error {
	log := p.Log
	if log == nil {
		log = slog.Default()
	}
	timeout := p.Timeout
	if timeout == 0 {
		timeout = telegram.PollTimeout
	}
	delay := p.ErrorDelay
	if delay == 0 {
		delay = PollErrorDelay
	}
	now := p.Now
	if now == nil {
		now = time.Now
	}
	work := p.Work
	if work == nil {
		work = ctx
	}
	for {
		if err := ctx.Err(); err != nil {
			return nil
		}
		updates, err := p.Source.GetUpdates(ctx, p.offset, timeout)
		if err != nil {
			if ctx.Err() != nil {
				return nil
			}
			var api *telegram.APIError
			if errors.As(err, &api) && api.Code == 409 {
				return fmt.Errorf("%w: %w", ErrPollConflict, err)
			}
			log.Error("Telegram polling error", "err", err)
			select {
			case <-ctx.Done():
				return nil
			case <-time.After(delay):
			}
			continue
		}
		if len(updates) > 0 {
			log.Debug("received updates", "count", len(updates))
		}
		received := now()
		if len(updates) > 0 && !p.waitReady(ctx, log) {
			return nil
		}
		for i := range updates {
			u := &updates[i]
			release, ok := p.Admission.Admit(ctx)
			if !ok {
				return nil
			}
			next := u.ID + 1
			p.offset = &next
			if u.Message != nil {
				p.Handler.HandleMessage(work, u.Message, Receipt{UpdateID: u.ID, ReceivedAt: received, PollIndex: i, PollSize: len(updates)})
			}
			if u.CallbackQuery != nil {
				p.Handler.HandleCallback(work, u.CallbackQuery)
			}
			release()
		}
	}
}
