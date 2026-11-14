package bot

import (
	"context"
	"errors"
	"fmt"
	"log/slog"
	"sync"
	"sync/atomic"
	"time"

	"github.com/tallesborges/zdx/apps/gateway/internal/engine"
)

// ReconnectDelay is the pause between renderer subscription attempts.
const ReconnectDelay = 3 * time.Second

// RoutedTurn is a turn the gateway renders into Telegram.
type RoutedTurn struct {
	ThreadID string
	TurnID   uint64
	Kind     string // engine.TurnKindUser or engine.TurnKindWorkerUpdate
	Route    engine.Route
	Inputs   []engine.InputRef
	Turn     *engine.Turn
	// Origin is the user message a followed turn answers; nil for turns the
	// service started on its own.
	Origin *Origin
	// Retryable: a failed turn gets "Try again" (ordinary threads only; the
	// Rust bot offers none on orchestrator topics).
	Retryable bool
	// Holder is the token that owns the turn's hold (capability
	// hold_tokens): the follower's submit reply, or the renderer
	// subscription's. "" from a service without hold tokens.
	Holder string
	// Replay: the service replayed this turn after its holder went away.
	// Only the reply is posted, no status message.
	Replay bool
	// Replayable: the turn opted in to replay with a hold token, so when
	// this connection goes away before delivery the service hands its
	// outcome to the next renderer subscription.
	Replayable bool
}

// TurnRenderer delivers one routed turn to Telegram. It must drain
// Turn.Events and, once delivery records are persisted through Rust, return.
// The caller releases the hold after it returns, whatever happened: the
// thread must not wait for a delivery that will not come.
type TurnRenderer interface {
	RenderTurn(ctx context.Context, t RoutedTurn)
}

// EventObserver sees every service event before it is routed (steer
// acknowledgements, worker notices and mirrors). It runs on the bridge loop
// and must not block.
type EventObserver func(ev engine.Event)

// Bridge holds the renderer subscription to the orchestrator service. Turns
// the service starts without a follower (worker updates, feedback, other
// clients' unfollowed routed submits) are rendered here; followed turns are
// rendered by their submitter.
//
// subscribe{renderer:true} has authority over routed-turn holds, so run
// exactly one bridge per bot token.
type Bridge struct {
	Engine   *engine.Client
	Renderer TurnRenderer
	Observe  EventObserver
	Log      *slog.Logger
	// Delay is the reconnect pause (default ReconnectDelay).
	Delay time.Duration
	// Ready, when set, must pass before the bridge subscribes as renderer
	// (delivery recording). Without the subscription the service holds no
	// unfollowed routed turn, so none waits for a delivery that cannot be
	// recorded; such turns go unrendered until it passes, as when the bridge
	// is disconnected.
	Ready func(context.Context) error
	// Replay opts the renderer in to replays (replay:true, sent only to a
	// service with hold_tokens): the service hands this renderer held turns
	// whose holder went away before delivery started.
	Replay bool
	// ConflictRetries and ConflictDelay bound the retries of a refused
	// subscription (another renderer is live): defaults 3 and 600ms, about
	// 2s in all (Rust holds v7.2).
	ConflictRetries int
	ConflictDelay   time.Duration
	// Connected, when set, runs after each successful subscription, before
	// any event of it.
	Connected func()
	// Before, when set, runs on a turn's render goroutine before the turn
	// renders: what Observe handed off for the thread goes out first (a
	// worker's completion notice before its owner's review turn).
	Before func(ctx context.Context, threadID string)

	renders sync.WaitGroup
	active  atomic.Int64
}

// ErrRendererConflict: the service kept refusing the renderer subscription
// because another renderer (the Rust bot or another gateway) is live. The
// gateway must stop, not restart itself.
var ErrRendererConflict = errors.New("another renderer is subscribed to the orchestrator service (the Rust bot or another gateway); stop it first")

// Active is how many turns the bridge is rendering now.
func (b *Bridge) Active() int64 { return b.active.Load() }

// Run keeps the subscription open until ctx ends, reconnecting whenever the
// service restarts or is not running yet. It returns after in-flight renders
// finish: nil when ctx ended, ErrRendererConflict when another renderer
// kept the role.
func (b *Bridge) Run(ctx context.Context) error {
	defer b.renders.Wait()
	log := b.log()
	delay := b.Delay
	if delay == 0 {
		delay = ReconnectDelay
	}
	retries, conflictDelay := b.ConflictRetries, b.ConflictDelay
	if retries == 0 {
		retries = 3
	}
	if conflictDelay == 0 {
		conflictDelay = 600 * time.Millisecond
	}
	conflicts := 0
	reportedDown := false
	reportedNotReady := false
	wait := func(d time.Duration) {
		select {
		case <-ctx.Done():
		case <-time.After(d):
		}
	}
	for ctx.Err() == nil {
		if b.Ready != nil {
			if err := b.Ready(ctx); err != nil {
				if !reportedNotReady {
					reportedNotReady = true
					log.Error("not rendering routed turns: the orchestrator service cannot record deliveries", "err", err)
				}
				wait(delay)
				continue
			}
			reportedNotReady = false
		}
		// The service may restart onto another build at any time, so each
		// connection asks again whether it hands out hold tokens.
		holds, err := b.holdTokens(ctx)
		var stream *engine.Stream
		if err == nil {
			stream, err = b.Engine.Open(ctx, engine.Subscribe{Renderer: true, Replay: holds && b.Replay})
		}
		if err != nil {
			if !reportedDown {
				reportedDown = true
				log.Warn("orchestrator service unavailable; routed turns will not render until it runs", "err", err)
			}
			wait(delay)
			continue
		}
		reportedDown = false
		holder, err := b.subscribed(stream, holds)
		if err != nil {
			stream.Close()
			var se *engine.ServiceError
			if errors.As(err, &se) && se.Code == "conflict" {
				conflicts++
				if conflicts > retries {
					log.Error("renderer subscription refused: another renderer is live", "attempts", conflicts, "err", se.Message)
					return ErrRendererConflict
				}
				log.Warn("renderer subscription refused; retrying", "attempt", conflicts, "err", se.Message)
				wait(conflictDelay)
				continue
			}
			log.Warn("renderer subscription failed", "err", err)
			wait(delay)
			continue
		}
		conflicts = 0
		log.Info("connected to the orchestrator service as renderer")
		if b.Connected != nil {
			b.Connected()
		}
		b.run(ctx, stream, holder)
		stream.Close()
		if ctx.Err() == nil {
			log.Warn("lost the connection to the orchestrator service")
		}
		wait(delay)
	}
	return nil
}

// holdTokens reports whether the service hands out hold tokens.
func (b *Bridge) holdTokens(ctx context.Context) (bool, error) {
	report, err := b.Engine.StatusReport(ctx, nil)
	if err != nil {
		return false, err
	}
	return report.Has(engine.CapHoldTokens), nil
}

// subscribed reads the subscription's first reply when the service has
// hold tokens: the token of every hold this renderer owns (unfollowed turns
// and replays, whether or not it opted in to replay). An older service
// sends no such reply (its first line is already an event, or nothing), and
// its holds take no token. A refusal is a *ServiceError.
func (b *Bridge) subscribed(stream *engine.Stream, holds bool) (string, error) {
	if !holds {
		return "", nil
	}
	first, err := stream.Next()
	if err != nil {
		return "", err
	}
	if err := first.Err(); err != nil {
		return "", err
	}
	if first.Kind != engine.ReplySubscribed || first.Holder == "" {
		return "", fmt.Errorf("unexpected first reply %q to a renderer subscription", first.Kind)
	}
	return first.Holder, nil
}

func (b *Bridge) log() *slog.Logger {
	if b.Log != nil {
		return b.Log
	}
	return slog.Default()
}

func (b *Bridge) run(ctx context.Context, stream *engine.Stream, holder string) {
	turns := map[uint64]chan<- engine.Event{}
	// Turns in flight cannot finish without the service: closing their
	// channels ends them with a lost-connection failure.
	defer func() {
		for _, ch := range turns {
			close(ch)
		}
	}()
	for {
		reply, err := stream.Next()
		if err != nil {
			if ctx.Err() == nil {
				b.log().Debug("renderer stream ended", "err", err)
			}
			return
		}
		if reply.Kind == engine.ReplyError {
			b.log().Error("renderer subscription ended by the service", "code", reply.Code, "err", reply.Message)
			return
		}
		if reply.Kind == engine.ReplySubscribed {
			// Status said no hold tokens, but the service restarted onto a
			// build with them before the subscription: its holds need this
			// token, so start over and read it.
			b.log().Warn("the orchestrator service now hands out hold tokens; resubscribing")
			return
		}
		if reply.Kind != engine.ReplyEvent || reply.Event == nil {
			continue
		}
		ev := *reply.Event
		if b.Observe != nil {
			b.Observe(ev)
		}
		switch ev.Type {
		case engine.EventTurnStarted:
			if ev.Route == nil || ev.Followed {
				continue
			}
			ch, out := engine.Buffer()
			turns[ev.TurnID] = ch
			turnID := ev.TurnID
			rt := RoutedTurn{
				ThreadID: ev.ThreadID, TurnID: ev.TurnID, Kind: ev.Kind, Route: *ev.Route, Inputs: ev.Inputs, Holder: holder,
				Replayable: b.Replay && holder != "",
				Turn:       engine.PumpTurn(b.Engine, ev.ThreadID, engine.CancelTarget{TurnID: &turnID}, out),
			}
			b.renders.Add(1)
			b.active.Add(1)
			go func() {
				defer b.renders.Done()
				defer b.active.Add(-1)
				if b.Before != nil {
					b.Before(ctx, rt.ThreadID)
				}
				Render(ctx, b.Engine, b.Renderer, rt, b.log())
			}()
		case engine.EventTurnReplay:
			if ev.Route == nil {
				b.log().Error("turn replay without a route", "thread", ev.ThreadID, "turn_id", ev.TurnID)
				continue
			}
			text := ""
			if ev.FinalText != nil {
				text = *ev.FinalText
			}
			b.log().Info("replaying an undelivered turn", "thread", ev.ThreadID, "turn_id", ev.TurnID, "status", ev.Status)
			rt := RoutedTurn{
				ThreadID: ev.ThreadID, TurnID: ev.TurnID, Route: *ev.Route, Holder: holder, Replay: true,
				Turn: engine.ReplayTurn(ev.TurnID, ev.Status, text, ev.Error),
			}
			b.renders.Add(1)
			b.active.Add(1)
			go func() {
				defer b.renders.Done()
				defer b.active.Add(-1)
				if b.Before != nil {
					b.Before(ctx, rt.ThreadID)
				}
				Render(ctx, b.Engine, b.Renderer, rt, b.log())
			}()
		case engine.EventAgent:
			if ch, ok := turns[ev.TurnID]; ok {
				ch <- ev
			}
		case engine.EventTurnEnded:
			if ch, ok := turns[ev.TurnID]; ok {
				ch <- ev
				close(ch)
				delete(turns, ev.TurnID)
			}
		}
	}
}

// Render runs the renderer for one turn, then releases the hold if the
// service holds the turn. Followed submits use it too: their connection
// stays open until the service closes it after the release.
func Render(ctx context.Context, eng *engine.Client, r TurnRenderer, t RoutedTurn, log *slog.Logger) {
	r.RenderTurn(ctx, t)
	// Drain anything the renderer left so the pump can finish.
	for range t.Turn.Events {
	}
	end := t.Turn.Wait()
	if !end.Ran || !end.Held {
		return
	}
	// Release even on shutdown: a fresh context, bounded by the reply
	// timeout. A transport failure is retried with the same token (a
	// release that landed answers ok again); a refusal is final. Without a
	// release the thread waits for RELEASE_TIMEOUT (10 minutes).
	rctx, cancel := context.WithTimeout(context.WithoutCancel(ctx), engine.ReplyTimeout)
	defer cancel()
	var err error
	for attempt := range ReleaseAttempts {
		if attempt > 0 {
			select {
			case <-rctx.Done():
			case <-time.After(ReleaseRetryDelay):
			}
		}
		if err = eng.ReleaseHeld(rctx, t.ThreadID, end.TurnID, t.Holder); err == nil {
			return
		}
		var se *engine.ServiceError
		if errors.As(err, &se) || rctx.Err() != nil {
			break
		}
	}
	log.Warn("failed to release orchestrator turn", "thread", t.ThreadID, "turn_id", end.TurnID, "err", err)
}

// ReleaseAttempts and ReleaseRetryDelay bound the release of a held turn.
const (
	ReleaseAttempts   = 3
	ReleaseRetryDelay = 500 * time.Millisecond
)
