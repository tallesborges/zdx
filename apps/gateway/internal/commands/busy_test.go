package commands

import (
	"context"
	"sync"
	"testing"
	"time"

	"github.com/tallesborges/zdx/apps/gateway/internal/telegram"
)

// gate blocks a fake until released and tells when it was reached.
type gate struct {
	reached chan struct{}
	release chan struct{}
	once    sync.Once
}

func newGate() *gate { return &gate{reached: make(chan struct{}), release: make(chan struct{})} }

func (g *gate) wait() {
	g.once.Do(func() { close(g.reached) })
	<-g.release
}

type slowGen struct {
	fakeGen
	g *gate
}

func (s *slowGen) TLDR(ctx context.Context, chat int64, thread string) (string, error) {
	s.g.wait()
	return s.fakeGen.TLDR(ctx, chat, thread)
}

type slowLifecycle struct {
	fakeLifecycle
	g *gate
}

func (s *slowLifecycle) RestartServices(ctx context.Context, force bool) (RestartResult, error) {
	s.g.wait()
	return s.fakeLifecycle.RestartServices(ctx, force)
}

type slowModels struct {
	fakeModels
	g *gate
}

func (s *slowModels) Picker(ctx context.Context, q PickerQuery) (Picker, error) {
	s.g.wait()
	return s.fakeModels.Picker(ctx, q)
}

// queuedRestartMayRun is the gateway's admission check for a queued
// restart, as far as this package is concerned.
func queuedRestartMayRun(k *Hook) bool { return !k.Busy() }

func waitReached(t *testing.T, g *gate) {
	t.Helper()
	select {
	case <-g.reached:
	case <-time.After(5 * time.Second):
		t.Fatal("the task never started")
	}
}

func TestBusyCoversEveryOwnedTask(t *testing.T) {
	t.Run("tldr", func(t *testing.T) {
		g := newGate()
		k := NewHook(&Handler{Telegram: newTG(), Generator: &slowGen{g: g}})
		k.HandleCommand(context.Background(), request("/tldr"))
		if queuedRestartMayRun(k) {
			t.Fatal("busy from the call, before the goroutine runs")
		}
		waitReached(t, g)
		if queuedRestartMayRun(k) {
			t.Fatal("busy while the generation runs")
		}
		close(g.release)
		k.Wait()
		if k.Busy() {
			t.Fatal("idle once the recap is edited in")
		}
	})

	t.Run("restart", func(t *testing.T) {
		g := newGate()
		lc := &slowLifecycle{fakeLifecycle: fakeLifecycle{supervised: true, result: RestartResult{Outcome: RestartBlocked, Text: "busy"}}, g: g}
		k := NewHook(&Handler{Telegram: newTG(), Lifecycle: lc})
		k.HandleCommand(context.Background(), request("/restart"))
		waitReached(t, g)
		if queuedRestartMayRun(k) {
			t.Fatal("busy while services restart")
		}
		close(g.release)
		k.Wait()
		if k.Busy() {
			t.Fatal("idle after the answer")
		}
	})

	t.Run("staging callback", func(t *testing.T) {
		th := ordinary()
		r := newStaging(th)
		g := newGate()
		r.h.Models = &slowModels{fakeModels: fakeModels{pickers: map[Scope]Picker{ScopeStaging: samplePicker("x")}}, g: g}
		k := NewHook(r.h)
		r.msg(t, 10, "/btw", th)
		card := r.tg.lastSend(t)
		data := "stg:m"
		k.HandleCallback(context.Background(), &telegram.CallbackQuery{ID: "q", From: telegram.User{ID: privateID}, Data: &data,
			Message: callback(forumChat, ptr(topicID), card.id, data).Message})
		waitReached(t, g)
		if queuedRestartMayRun(k) {
			t.Fatal("busy while a button is handled")
		}
		close(g.release)
		k.Wait()
		if k.Busy() || len(r.tg.answers()) != 1 {
			t.Fatal("idle once the button is answered")
		}
	})

	t.Run("dispatching tap", func(t *testing.T) {
		tg := newTG()
		d := &fakeDispatch{}
		h := &Handler{Telegram: tg, Dispatch: d, Commands: fakeCommands{list: []CustomCommand{{Name: "c", Source: "user", Content: "go"}}}}
		k := NewHook(h)
		h.HandleMessage(context.Background(), topicMsg(10, "/commands"), ordinary())
		k.HandleCallback(context.Background(), callback(forumChat, ptr(topicID), tg.lastSend(t).id, "cmd:0"))
		k.Wait()
		if k.Busy() || len(d.sent) != 1 {
			t.Fatal("the task ends only after the bot took the message")
		}
	})

	t.Run("launcher repost", func(t *testing.T) {
		r := newLauncher(false)
		r.h.RepostDelay = 50 * time.Millisecond
		k := NewHook(r.h)
		r.h.HandleMessage(context.Background(), generalMsg(10, "/launcher"), generalThread)
		k.ScheduleLauncherRepost(context.Background(), forumChat)
		if queuedRestartMayRun(k) {
			t.Fatal("busy while a repost waits to run")
		}
		k.Wait()
		if k.Busy() || len(r.tg.of("send")) != 2 {
			t.Fatal("idle once the launcher is reposted")
		}
		// No open launcher: nothing is scheduled, nothing is busy.
		k.ScheduleLauncherRepost(context.Background(), -1)
		if k.Busy() {
			t.Fatal("no repost, no work")
		}
	})
}
