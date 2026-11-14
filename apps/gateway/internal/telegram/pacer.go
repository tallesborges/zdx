package telegram

import (
	"context"
	"sort"
	"sync"
	"time"
)

// Clock is the time source of the client, replaceable in tests.
type Clock interface {
	Now() time.Time
	// Sleep waits d or until ctx ends.
	Sleep(ctx context.Context, d time.Duration) error
}

type realClock struct{}

func (realClock) Now() time.Time { return time.Now() }

func (realClock) Sleep(ctx context.Context, d time.Duration) error {
	if d <= 0 {
		return ctx.Err()
	}
	t := time.NewTimer(d)
	defer t.Stop()
	select {
	case <-ctx.Done():
		return ctx.Err()
	case <-t.C:
		return nil
	}
}

// RealClock is the wall clock.
var RealClock Clock = realClock{}

// ChatPacer is the pacing and cooldown state of one chat. Only new messages
// are paced and counted; edits, deletes and typing are not. A flood wait
// blocks every call to the chat until it ends. Admission is checked and
// recorded in one step, so waiters that wake together still go out one slot
// apart.
type ChatPacer struct {
	lastNewMessage time.Time
	window         []time.Time // ascending send times of recent new messages
	cooldownUntil  time.Time
}

// CooldownRemaining is the time left in an announced flood wait.
func (p *ChatPacer) CooldownRemaining(now time.Time) (time.Duration, bool) {
	if p.cooldownUntil.IsZero() {
		return 0, false
	}
	left := p.cooldownUntil.Sub(now)
	return left, left > 0
}

// NoteFloodWait records a refusal: nothing goes to the chat for retryAfter.
func (p *ChatPacer) NoteFloodWait(now time.Time, retryAfter time.Duration) {
	until := now.Add(retryAfter)
	if until.After(p.cooldownUntil) {
		p.cooldownUntil = until
	}
}

// TryAdmitNewMessage admits a new message now and counts it, or returns the
// earliest time it may be retried.
func (p *ChatPacer) TryAdmitNewMessage(now time.Time, isGroup bool) (time.Time, bool) {
	p.prune(now)
	at := now
	if p.cooldownUntil.After(at) {
		at = p.cooldownUntil
	}
	if !p.lastNewMessage.IsZero() {
		if next := p.lastNewMessage.Add(MinNewMessageSpacing); next.After(at) {
			at = next
		}
	}
	if isGroup && len(p.window) >= GroupWindowMaxNewMessages {
		oldest := p.window[len(p.window)-GroupWindowMaxNewMessages]
		if next := oldest.Add(GroupWindow); next.After(at) {
			at = next
		}
	}
	if at.After(now) {
		return at, false
	}
	p.RecordNewMessage(now)
	return now, true
}

// RecordNewMessage counts a new message sent at at without admission.
func (p *ChatPacer) RecordNewMessage(at time.Time) {
	if at.After(p.lastNewMessage) {
		p.lastNewMessage = at
	}
	i := sort.Search(len(p.window), func(i int) bool { return p.window[i].After(at) })
	p.window = append(p.window, time.Time{})
	copy(p.window[i+1:], p.window[i:])
	p.window[i] = at
}

func (p *ChatPacer) prune(now time.Time) {
	n := 0
	for n < len(p.window) && !p.window[n].Add(GroupWindow).After(now) {
		n++
	}
	p.window = p.window[n:]
}

// chatGate is the per-chat state shared by every copy of a client (one bot
// token, so the key is bot + chat). Process-local.
type chatGate struct {
	mu    sync.Mutex
	chats map[int64]*ChatPacer
}

func (g *chatGate) with(chat int64, f func(*ChatPacer)) {
	g.mu.Lock()
	defer g.mu.Unlock()
	if g.chats == nil {
		g.chats = map[int64]*ChatPacer{}
	}
	p, ok := g.chats[chat]
	if !ok {
		p = &ChatPacer{}
		g.chats[chat] = p
	}
	f(p)
}
