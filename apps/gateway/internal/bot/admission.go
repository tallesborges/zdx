package bot

import (
	"context"
	"sync"
)

// Admission decides when new outside work may enter the gateway: polled
// updates and Mini App writes. Close stops new entries and waits until
// every admitted caller has handed its work off, so an idle check after
// Close cannot miss work that slipped in. A queued restart closes it before
// the idle check and keeps it closed until Rust answers; Shut closes it for
// good when the gateway exits. A nil Admission admits everything.
type Admission struct {
	mu      sync.Mutex
	closed  bool
	shut    bool
	active  int
	changed chan struct{}
}

func (a *Admission) notify() {
	if a.changed != nil {
		close(a.changed)
	}
	a.changed = make(chan struct{})
}

func (a *Admission) wait() <-chan struct{} {
	if a.changed == nil {
		a.changed = make(chan struct{})
	}
	return a.changed
}

// Admit waits while admission is closed, then counts the caller in; call
// release once the work is handed off. ok is false when admission is shut
// or ctx ends: the caller must not take the work.
func (a *Admission) Admit(ctx context.Context) (release func(), ok bool) {
	if a == nil {
		return func() {}, true
	}
	a.mu.Lock()
	for a.closed && !a.shut {
		ch := a.wait()
		a.mu.Unlock()
		select {
		case <-ch:
		case <-ctx.Done():
			return nil, false
		}
		a.mu.Lock()
	}
	if a.shut || ctx.Err() != nil {
		a.mu.Unlock()
		return nil, false
	}
	a.active++
	a.mu.Unlock()
	var once sync.Once
	return func() {
		once.Do(func() {
			a.mu.Lock()
			a.active--
			a.notify()
			a.mu.Unlock()
		})
	}, true
}

// Close stops new admissions and waits until admitted callers released.
// If ctx ends first it opens again and returns ctx's error.
func (a *Admission) Close(ctx context.Context) error {
	if a == nil {
		return nil
	}
	a.mu.Lock()
	a.closed = true
	a.notify()
	for a.active > 0 {
		ch := a.wait()
		a.mu.Unlock()
		select {
		case <-ch:
		case <-ctx.Done():
			a.Open()
			return ctx.Err()
		}
		a.mu.Lock()
	}
	a.mu.Unlock()
	return nil
}

// Open lets waiting and new callers in again, unless admission is shut.
func (a *Admission) Open() {
	if a == nil {
		return
	}
	a.mu.Lock()
	defer a.mu.Unlock()
	if !a.shut {
		a.closed = false
	}
	a.notify()
}

// Shut closes admission for good: waiting and new callers are refused.
func (a *Admission) Shut() {
	if a == nil {
		return
	}
	a.mu.Lock()
	defer a.mu.Unlock()
	a.closed, a.shut = true, true
	a.notify()
}
