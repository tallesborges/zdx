package bot

import (
	"context"
	"testing"
	"time"

	"github.com/tallesborges/zdx/apps/gateway/internal/telegram"
)

func TestAdmissionCloseWaitsForAdmittedWorkAndHoldsNewWork(t *testing.T) {
	a := &Admission{}
	release, ok := a.Admit(context.Background())
	if !ok {
		t.Fatal("open admission admits")
	}
	closed := make(chan error, 1)
	go func() { closed <- a.Close(context.Background()) }()
	select {
	case <-closed:
		t.Fatal("Close must wait for admitted work")
	case <-time.After(30 * time.Millisecond):
	}
	release()
	if err := <-closed; err != nil {
		t.Fatal(err)
	}
	admitted := make(chan bool, 1)
	go func() {
		r, ok := a.Admit(context.Background())
		if ok {
			r()
		}
		admitted <- ok
	}()
	select {
	case <-admitted:
		t.Fatal("closed admission must hold new work")
	case <-time.After(30 * time.Millisecond):
	}
	a.Open()
	if !<-admitted {
		t.Fatal("reopened admission admits the waiter")
	}

	// A Close that times out opens again.
	busy, _ := a.Admit(context.Background())
	ctx, cancel := context.WithTimeout(context.Background(), 20*time.Millisecond)
	defer cancel()
	if err := a.Close(ctx); err == nil {
		t.Fatal("Close must give up while work stays admitted")
	}
	if r, ok := a.Admit(context.Background()); !ok {
		t.Fatal("a failed Close reopens")
	} else {
		r()
	}
	busy()

	// Shut refuses waiting and new callers.
	a.Close(context.Background())
	waiting := make(chan bool, 1)
	go func() { _, ok := a.Admit(context.Background()); waiting <- ok }()
	time.Sleep(20 * time.Millisecond)
	a.Shut()
	if <-waiting {
		t.Fatal("a waiter is refused on shut")
	}
	a.Open()
	if _, ok := a.Admit(context.Background()); ok {
		t.Fatal("shut is final")
	}
}

// A closed gate holds the update before hand-off; the offset does not move,
// so a stop then leaves it unconfirmed for the next process.
func TestPollerHoldsUpdatesAtAClosedGate(t *testing.T) {
	a := &Admission{}
	a.Close(context.Background())
	ctx, cancel := context.WithCancel(context.Background())
	src := &scriptedSource{cancel: cancel, batches: [][]telegram.Update{{{ID: 30, Message: &telegram.Message{ID: 1}}}}}
	rec := &recorder{}
	p := &Poller{Source: src, Handler: rec, Log: quiet, Admission: a}
	rec.poller = p
	done := make(chan error, 1)
	go func() { done <- p.Run(ctx) }()
	time.Sleep(50 * time.Millisecond)
	rec.mu.Lock()
	n := len(rec.messages)
	rec.mu.Unlock()
	if n != 0 || p.Offset() != nil {
		t.Fatalf("nothing is handed off while closed: %d %v", n, p.Offset())
	}
	a.Shut()
	if err := <-done; err != nil {
		t.Fatal(err)
	}
	if p.Offset() != nil {
		t.Fatal("a held update never moves the offset")
	}
	cancel()
}
