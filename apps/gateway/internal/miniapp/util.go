package miniapp

import (
	"fmt"
	"sync"
	"time"
)

// DeliveryTracker is the bot's set of reply deliveries in flight, the Go
// form of `delivery_tracker.rs`. The reply path calls Start before it writes
// the pending delivery record and calls the returned function after the
// outcome is persisted. It implements [Deliveries].
type DeliveryTracker struct {
	mu       sync.Mutex
	running  map[string]int
	finished uint64
}

// Start marks a delivery for threadID (the alias-resolved thread) as running.
// The returned function ends it; calling it again does nothing.
func (t *DeliveryTracker) Start(threadID string) func() {
	t.mu.Lock()
	if t.running == nil {
		t.running = map[string]int{}
	}
	t.running[threadID]++
	t.mu.Unlock()
	var once sync.Once
	return func() {
		once.Do(func() {
			t.mu.Lock()
			defer t.mu.Unlock()
			if t.running[threadID] <= 1 {
				delete(t.running, threadID)
			} else {
				t.running[threadID]--
			}
			t.finished++
		})
	}
}

// Finished implements [Deliveries].
func (t *DeliveryTracker) Finished() uint64 {
	t.mu.Lock()
	defer t.mu.Unlock()
	return t.finished
}

// Running implements [Deliveries].
func (t *DeliveryTracker) Running(threadID string) bool {
	t.mu.Lock()
	defer t.mu.Unlock()
	return t.running[threadID] > 0
}

// formatUptime is `zdx_engine::service::format_uptime`: `42s`, `12m`,
// `3h 5m`, `2d 4h`. Used only for the age of Go's own quota cache.
func formatUptime(d time.Duration) string {
	secs := int64(d / time.Second)
	if secs < 0 {
		secs = 0
	}
	switch {
	case secs < 60:
		return fmt.Sprintf("%ds", secs)
	case secs < 3600:
		return fmt.Sprintf("%dm", secs/60)
	case secs < 86400:
		return fmt.Sprintf("%dh %dm", secs/3600, (secs%3600)/60)
	default:
		return fmt.Sprintf("%dd %dh", secs/86400, (secs%86400)/3600)
	}
}
