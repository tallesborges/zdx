package bot

import (
	"context"
	"log/slog"
	"strconv"
	"sync"

	"github.com/tallesborges/zdx/apps/gateway/internal/engine"
)

// Steer acknowledgement texts (crates/zdx-bot/src/steer.rs).
const (
	AckPending = "↪️ Will be added to the running turn"
	AckAdded   = "↪️ Added to the running turn"
	AckNewTurn = "↪️ The turn ended first, so this runs as the next turn"
	AckNotAdd  = "↩️ Not added: the turn was cancelled"
)

// Ack is where a steer acknowledgement was posted.
type Ack struct {
	Chat      int64
	MessageID int64
}

type ackEntry struct {
	ack     *Ack
	outcome string
}

// SteerAcks tracks the acknowledgement of each message steered into a
// service turn, keyed by input id. An outcome that arrives before the
// acknowledgement is posted is kept and applied when it is.
type SteerAcks struct {
	mu   sync.Mutex
	acks map[string]*ackEntry
}

func (s *SteerAcks) entry(id string) *ackEntry {
	if s.acks == nil {
		s.acks = map[string]*ackEntry{}
	}
	e, ok := s.acks[id]
	if !ok {
		e = &ackEntry{}
		s.acks[id] = e
	}
	return e
}

// Expect marks id as a steer whose acknowledgement is about to be posted.
func (s *SteerAcks) Expect(id string) {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.entry(id)
}

// Record stores the posted acknowledgement. It returns the outcome to edit
// it to when one arrived first.
func (s *SteerAcks) Record(id string, ack Ack) (string, bool) {
	s.mu.Lock()
	defer s.mu.Unlock()
	e := s.entry(id)
	if e.outcome != "" {
		delete(s.acks, id)
		return e.outcome, true
	}
	e.ack = &ack
	return "", false
}

// Forget drops id after its acknowledgement could not be posted.
func (s *SteerAcks) Forget(id string) {
	s.mu.Lock()
	defer s.mu.Unlock()
	delete(s.acks, id)
}

// Settle reports that steer id reached outcome. It returns the
// acknowledgement to edit, or keeps the outcome until it is posted. Unknown
// ids are ignored.
func (s *SteerAcks) Settle(id, outcome string) (Ack, bool) {
	s.mu.Lock()
	defer s.mu.Unlock()
	e, ok := s.acks[id]
	if !ok {
		return Ack{}, false
	}
	if e.ack != nil {
		delete(s.acks, id)
		return *e.ack, true
	}
	e.outcome = outcome
	return Ack{}, false
}

// SettleInjection reports that a service turn read steer id. Only steers
// are injected, so an id whose acknowledgement is not posted yet is
// remembered.
func (s *SteerAcks) SettleInjection(id string) (Ack, bool) {
	s.mu.Lock()
	s.entry(id)
	s.mu.Unlock()
	return s.Settle(id, AckAdded)
}

// AckEditor edits an acknowledgement message.
type AckEditor interface {
	EditAck(ctx context.Context, ack Ack, text string)
}

// SteerObserver keeps steer acknowledgements current from service events:
// added when the turn reads it, a new turn when it missed the turn, not added
// when a cancel gave it back. Use it as (part of) the bridge's observer.
func SteerObserver(ctx context.Context, acks *SteerAcks, editor AckEditor, log *slog.Logger) EventObserver {
	return func(ev engine.Event) {
		type update struct {
			ack  Ack
			text string
		}
		var updates []update
		switch ev.Type {
		case engine.EventAgent:
			a, err := ev.Agent()
			if err != nil || a.Type != engine.AgentUserMessageInjected {
				return
			}
			if ack, ok := acks.SettleInjection(a.ID); ok {
				updates = append(updates, update{ack, AckAdded})
			}
		case engine.EventTurnStarted:
			for _, in := range ev.Inputs {
				if ack, ok := acks.Settle(strconv.FormatUint(in.InputID, 10), AckNewTurn); ok {
					updates = append(updates, update{ack, AckNewTurn})
				}
			}
		case engine.EventInputsDropped:
			for _, id := range ev.InputIDs {
				if ack, ok := acks.Settle(strconv.FormatUint(id, 10), AckNotAdd); ok {
					updates = append(updates, update{ack, AckNotAdd})
				}
			}
		}
		if len(updates) == 0 {
			return
		}
		// Edits go off the bridge loop: they can wait on Telegram.
		go func() {
			for _, u := range updates {
				editor.EditAck(ctx, u.ack, u.text)
			}
		}()
	}
}
