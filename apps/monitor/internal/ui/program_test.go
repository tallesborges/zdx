package ui

import (
	"bytes"
	"context"
	"runtime"
	"sync"
	"sync/atomic"
	"testing"
	"time"

	tea "charm.land/bubbletea/v2"

	"github.com/tallesborges/zdx/apps/monitor/internal/data"
	"github.com/tallesborges/zdx/apps/monitor/internal/source"
)

type syncBuffer struct {
	mu sync.Mutex
	b  bytes.Buffer
}

func (s *syncBuffer) Write(p []byte) (int, error) {
	s.mu.Lock()
	defer s.mu.Unlock()
	return s.b.Write(p)
}

// TestProgramQuitsCleanlyWithAFetchInFlight runs the real Bubble Tea loop
// against a backend whose agents call never answers: quitting must cancel
// it, Run must return, and no goroutine may stay behind.
func TestProgramQuitsCleanlyWithAFetchInFlight(t *testing.T) {
	baseline := runtime.NumGoroutine()
	var blocked, released atomic.Int32
	f := &fake{
		agents: func(ctx context.Context) ([]data.Agent, error) {
			blocked.Add(1)
			<-ctx.Done()
			released.Add(1)
			return nil, ctx.Err()
		},
		logs: func(int, int, source.LogStamp) (source.LogTail, error) { return source.LogTail{}, nil },
	}
	ctx, cancel := context.WithCancel(context.Background())
	m := New(ctx, f, Options{Root: "/r", Home: "/h"})
	var out syncBuffer
	p := tea.NewProgram(m, tea.WithContext(ctx), tea.WithInput(nil), tea.WithOutput(&out), tea.WithWindowSize(100, 30), tea.WithoutSignalHandler())
	done := make(chan error, 1)
	go func() {
		_, err := p.Run()
		done <- err
	}()
	deadline := time.After(5 * time.Second)
	for blocked.Load() == 0 {
		select {
		case <-deadline:
			t.Fatal("the agents fetch never started")
		case <-time.After(10 * time.Millisecond):
		}
	}
	// Ticks keep coming while the fetch hangs; it must not be started again.
	time.Sleep(2500 * time.Millisecond)
	if n := blocked.Load(); n != 1 {
		t.Fatalf("agents fetch started %d times while one was in flight", n)
	}
	p.Send(key("q"))
	select {
	case err := <-done:
		if err != nil {
			t.Fatalf("run: %v", err)
		}
	case <-time.After(5 * time.Second):
		t.Fatal("the program did not quit")
	}
	cancel()
	if released.Load() != 1 {
		t.Fatal("quit did not cancel the hanging fetch")
	}
	// Allow the runtime to retire finished goroutines.
	var now int
	for i := 0; i < 100; i++ {
		now = runtime.NumGoroutine()
		if now <= baseline+1 {
			break
		}
		time.Sleep(20 * time.Millisecond)
	}
	if now > baseline+1 {
		buf := make([]byte, 1<<16)
		t.Fatalf("goroutines: %d before, %d after\n%s", baseline, now, buf[:runtime.Stack(buf, true)])
	}
}
