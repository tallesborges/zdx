package bot

import (
	"context"
	"errors"
	"fmt"
	"strings"
	"sync"
	"sync/atomic"
	"testing"
	"time"

	"github.com/tallesborges/zdx/apps/gateway/internal/engine"
	"github.com/tallesborges/zdx/apps/gateway/internal/telegram"
)

// While the service does not answer, a polled batch is not handed off and
// no further poll confirms it; a stop then leaves it for the next process.
func TestPollerHoldsABatchWhileTheServiceIsDown(t *testing.T) {
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	src := &scriptedSource{cancel: cancel, batches: [][]telegram.Update{
		{{ID: 40, Message: &telegram.Message{ID: 1}}},
		{{ID: 41, Message: &telegram.Message{ID: 2}}},
	}}
	var down atomic.Bool
	down.Store(true)
	var checks atomic.Int64
	rec := &recorder{}
	p := &Poller{Source: src, Handler: rec, Log: quiet, ReadyDelay: time.Millisecond, Ready: func(context.Context) error {
		checks.Add(1)
		if down.Load() {
			return fmt.Errorf("%w: down", engine.ErrRequestNotSent)
		}
		return nil
	}}
	rec.poller = p
	done := make(chan error, 1)
	go func() { done <- p.Run(ctx) }()
	deadline := time.Now().Add(2 * time.Second)
	for checks.Load() < 5 && time.Now().Before(deadline) {
		time.Sleep(time.Millisecond)
	}
	rec.mu.Lock()
	handled := len(rec.messages)
	rec.mu.Unlock()
	src.mu.Lock()
	polls := len(src.offsets)
	src.mu.Unlock()
	if handled != 0 || polls != 1 || p.Offset() != nil {
		t.Fatalf("a held batch is neither handed off nor confirmed: handled=%d polls=%d", handled, polls)
	}
	down.Store(false)
	if err := <-done; err != nil {
		t.Fatal(err)
	}
	if len(rec.messages) != 2 || *src.offsets[1] != 41 {
		t.Fatalf("messages %v offsets %v", rec.messages, src.offsets)
	}

	// A stop while the service is down confirms nothing.
	ctx2, cancel2 := context.WithCancel(context.Background())
	src2 := &scriptedSource{cancel: cancel2, batches: [][]telegram.Update{{{ID: 50, Message: &telegram.Message{ID: 3}}}}}
	rec2 := &recorder{}
	p2 := &Poller{Source: src2, Handler: rec2, Log: quiet, ReadyDelay: time.Millisecond, Ready: func(context.Context) error {
		return engine.ErrNotRunning
	}}
	rec2.poller = p2
	go func() { time.Sleep(30 * time.Millisecond); cancel2() }()
	if err := p2.Run(ctx2); err != nil {
		t.Fatal(err)
	}
	if len(rec2.messages) != 0 || p2.Offset() != nil {
		t.Fatalf("nothing may be handed off or confirmed: %v", rec2.messages)
	}
}

// flakyTurns fails submits with errs in order, then submits for real.
type flakyTurns struct {
	Turns
	mu    sync.Mutex
	errs  []error
	calls int
}

func (f *flakyTurns) next() error {
	f.mu.Lock()
	defer f.mu.Unlock()
	f.calls++
	if len(f.errs) == 0 {
		return nil
	}
	err := f.errs[0]
	f.errs = f.errs[1:]
	return err
}

func (f *flakyTurns) SubmitOrdinary(ctx context.Context, s engine.Submit) (*engine.Submission, error) {
	if err := f.next(); err != nil {
		return nil, err
	}
	return f.Turns.SubmitOrdinary(ctx, s)
}

type flakyResolver struct {
	TopicResolver
	fails atomic.Int64
}

func (r *flakyResolver) ResolveTopic(ctx context.Context, chat int64, topic *int64) (TopicInfo, error) {
	if r.fails.Add(-1) >= 0 {
		return TopicInfo{}, fmt.Errorf("%w: connect: %w", engine.ErrRequestNotSent, engine.ErrNotRunning)
	}
	return r.TopicResolver.ResolveTopic(ctx, chat, topic)
}

// A request that never reached the service is sent again; the turn runs once.
func TestIntakeSendsAgainOnlyWhatWasNeverSent(t *testing.T) {
	tg := newFakeTG(t)
	svc := newFakeSvc(t, []string{engine.CapOrdinarySubmit, engine.CapComposeUserText, engine.CapRecordDelivery})
	svc.submit = func(sc svcConn) {
		write(sc.conn, map[string]any{"reply": "submitted", "thread_id": thread, "input_id": 1, "queued_ahead": 0, "in_turn": true})
		sc.conn.Close()
	}
	h, _ := newHandler(tg, svc, TopicInfo{ThreadID: thread, Kind: TopicOrdinary})
	notSentErr := fmt.Errorf("%w: connect: %w", engine.ErrRequestNotSent, engine.ErrNotRunning)
	turns := &flakyTurns{Turns: h.Turns, errs: []error{notSentErr, notSentErr}}
	resolver := &flakyResolver{TopicResolver: h.Resolver}
	resolver.fails.Store(2)
	h.Turns, h.Resolver = turns, resolver
	h.Dispatch(context.Background(), userMessage(t, 42, "hi"))
	tg.waitFor(t, "the steer acknowledgement", func() bool { return len(tg.of("sendMessage")) == 1 })
	h.Queues.Wait()
	if n := len(svc.requests("submit")); n != 1 || turns.calls != 3 {
		t.Fatalf("one submit after two unsent attempts: submits=%d calls=%d", n, turns.calls)
	}
	if text := tg.of("sendMessage")[0].body["text"]; text != AckPending {
		t.Fatalf("no error notice for a retried request: %v", text)
	}
}

// An ambiguous submit is never sent again: the user is told, once.
func TestIntakeNeverResendsAnAmbiguousSubmit(t *testing.T) {
	for _, ambiguous := range []error{
		engine.ErrNoAnswer,
		fmt.Errorf("%w: send request: broken pipe", engine.ErrRequestOutcomeUnknown),
		errors.New("unclassified"),
	} {
		tg := newFakeTG(t)
		svc := newFakeSvc(t, []string{engine.CapOrdinarySubmit, engine.CapComposeUserText, engine.CapRecordDelivery})
		h, _ := newHandler(tg, svc, TopicInfo{ThreadID: thread, Kind: TopicOrdinary})
		turns := &flakyTurns{Turns: h.Turns, errs: []error{ambiguous}}
		h.Turns = turns
		h.Dispatch(context.Background(), userMessage(t, 42, "hi"))
		tg.waitFor(t, "the notice", func() bool { return len(tg.of("sendMessage")) == 1 })
		h.Queues.Wait()
		if turns.calls != 1 || len(svc.requests("submit")) != 0 {
			t.Fatalf("%v: sent again (calls=%d)", ambiguous, turns.calls)
		}
	}
}

// Retrying stops at the window and tells the user.
func TestIntakeGivesUpAfterTheRetryWindow(t *testing.T) {
	tg := newFakeTG(t)
	svc := newFakeSvc(t, []string{engine.CapOrdinarySubmit, engine.CapComposeUserText, engine.CapRecordDelivery})
	h, _ := newHandler(tg, svc, TopicInfo{ThreadID: thread, Kind: TopicOrdinary})
	resolver := &flakyResolver{TopicResolver: h.Resolver}
	resolver.fails.Store(1 << 30)
	h.Resolver, h.NotSentRetry = resolver, 600*time.Millisecond
	start := time.Now()
	h.Dispatch(context.Background(), userMessage(t, 42, "hi"))
	tg.waitFor(t, "the notice", func() bool { return len(tg.of("sendMessage")) == 1 })
	if el := time.Since(start); el < 500*time.Millisecond {
		t.Fatalf("gave up after %v", el)
	}
	if text := tg.of("sendMessage")[0].body["text"].(string); !strings.Contains(text, "not running") {
		t.Fatalf("notice %q", text)
	}
	if len(svc.requests("submit")) != 0 {
		t.Fatal("nothing may be submitted")
	}
}

// A failed capability check before a submit sent no submit, whatever the
// status call's own outcome: the submit may go out again.
func TestSubmitCapabilityCheckFailureIsNotSent(t *testing.T) {
	c := engine.NewClient(t.TempDir() + "/none.sock")
	_, err := c.SubmitOrdinary(context.Background(), engine.Submit{})
	if !errors.Is(err, engine.ErrRequestNotSent) || errors.Is(err, engine.ErrRequestOutcomeUnknown) {
		t.Fatalf("got %v", err)
	}
}

// A Cancel that loses the race to the turn's end shows the real outcome:
// the completed reply goes out and the status never says Cancelled.
func TestCancelAfterCompletionKeepsTheReply(t *testing.T) {
	tg := newFakeTG(t)
	svc := newFakeSvc(t, []string{engine.CapOrdinarySubmit, engine.CapComposeUserText, engine.CapRecordDelivery})
	svc.submit = func(sc svcConn) {
		c := sc.conn
		write(c, map[string]any{"reply": "submitted", "thread_id": thread, "input_id": 1, "queued_ahead": 0, "in_turn": false})
		write(c, event(map[string]any{"type": "turn_started", "thread_id": thread, "turn_id": 9, "kind": "user", "inputs": []any{map[string]any{"input_id": 1}}, "route": map[string]any{"chat": -100, "topic": 5}, "followed": true}))
		<-svc.cancel
		// The turn had already finished when the cancel arrived.
		write(c, agent(thread, 9, map[string]any{"type": "turn_finished", "status": map[string]any{"status": "completed"}, "final_text": "done anyway", "messages": []any{}}))
		write(c, event(map[string]any{"type": "turn_ended", "thread_id": thread, "turn_id": 9, "held": true}))
		deadline := time.Now().Add(5 * time.Second)
		for time.Now().Before(deadline) && len(svc.requests("release")) == 0 {
			time.Sleep(5 * time.Millisecond)
		}
		c.Close()
	}
	h, _ := newHandler(tg, svc, TopicInfo{ThreadID: thread, Kind: TopicOrdinary})
	h.Dispatch(context.Background(), userMessage(t, 42, "hi"))
	tg.waitFor(t, "the status", func() bool { return len(tg.of("sendMessage")) == 1 })
	data := "cancel:-100:42"
	h.HandleCallback(context.Background(), &telegram.CallbackQuery{ID: "q", From: telegram.User{ID: 7}, Data: &data})
	tg.waitFor(t, "the release", func() bool { return len(svc.requests("release")) > 0 })
	h.Wait()
	replied := false
	for _, m := range append(tg.of("sendMessage"), tg.of("editMessageText")...) {
		if m.body["text"] == StatusCancelled {
			t.Fatal("a completed turn must not show Cancelled")
		}
		if text, _ := m.body["text"].(string); strings.Contains(text, "done anyway") {
			replied = true
		}
	}
	if !replied {
		t.Fatalf("the completed reply must go out: %+v", tg.of("sendMessage"))
	}
}

// A lost connection is no failure for a turn the service replays: the
// status says the reply may still come, with no error and no Try again.
func TestLostConnectionOfAReplayableTurnPostsNoError(t *testing.T) {
	for _, replayable := range []bool{true, false} {
		tg := newFakeTG(t)
		svc := newFakeSvc(t, nil)
		h, _ := newHandler(tg, svc, TopicInfo{})
		in := make(chan engine.Event)
		turn := engine.PumpTurn(h.Engine, thread, engine.CancelTarget{}, in)
		close(in)
		chat, topic, msgID := int64(-100), int64(5), int64(42)
		h.Presenter.RenderTurn(context.Background(), RoutedTurn{ThreadID: thread, Route: engine.Route{Chat: chat, Topic: &topic},
			Turn: turn, Origin: &Origin{MessageID: msgID, ReplyTo: &msgID}, Retryable: true, Holder: "h", Replayable: replayable})
		edits := tg.of("editMessageText")
		if len(edits) == 0 {
			t.Fatal("the status must change")
		}
		last, _ := edits[len(edits)-1].body["text"].(string)
		if replayable && last != StatusConnectionLost {
			t.Fatalf("replayable: %q", last)
		}
		if !replayable && !strings.Contains(last, engine.MsgConnectionLost) {
			t.Fatalf("not replayable: %q", last)
		}
		retry := false
		for _, c := range append(tg.of("sendMessage"), edits...) {
			if strings.Contains(fmt.Sprint(c.body["reply_markup"]), "retry:") {
				retry = true
			}
		}
		if replayable && retry {
			t.Fatal("no Try again while a replay may come")
		}
	}
}

// Worker events run per owner, in order, off the bridge loop: a slow owner
// delays nobody else, and Before waits only for its own owner's backlog.
func TestOwnerQueuesOrderPerOwnerOnly(t *testing.T) {
	var q ownerQueues
	var mu sync.Mutex
	var order []string
	add := func(s string) { mu.Lock(); order = append(order, s); mu.Unlock() }
	slow := make(chan struct{})
	q.enqueue("a", func() { <-slow; add("a1") })
	q.enqueue("a", func() { add("a2") })
	q.enqueue("b", func() { add("b1") })

	done := make(chan struct{})
	go func() { q.barrier(context.Background(), "b"); q.barrier(context.Background(), "c"); close(done) }()
	select {
	case <-done:
	case <-time.After(time.Second):
		t.Fatal("another owner must not wait for a slow one")
	}
	if !q.busy() {
		t.Fatal("a pending owner is busy")
	}
	waited := make(chan struct{})
	go func() { q.barrier(context.Background(), "a"); add("turn"); close(waited) }()
	time.Sleep(20 * time.Millisecond)
	select {
	case <-waited:
		t.Fatal("the owner's turn must wait for its notice")
	default:
	}
	close(slow)
	<-waited
	q.wg.Wait()
	mu.Lock()
	defer mu.Unlock()
	if strings.Join(order, ",") != "b1,a1,a2,turn" || q.busy() {
		t.Fatalf("order %v", order)
	}
	ctx, cancel := context.WithCancel(context.Background())
	block := make(chan struct{})
	q.enqueue("d", func() { <-block })
	cancel()
	q.barrier(ctx, "d")
	close(block)
	q.wg.Wait()
}
