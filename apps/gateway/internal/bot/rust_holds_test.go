package bot

import (
	"context"
	"errors"
	"testing"
	"time"

	"github.com/tallesborges/zdx/apps/gateway/internal/engine"
	"github.com/tallesborges/zdx/apps/gateway/internal/sandbox"
)

// Real-Rust checks of holds v7.2 (capability hold_tokens). They need a
// binary with the capability and skip on an older one; gateway.Missing
// keeps the holds item until they pass.

func (r *realRust) requireHolds() {
	r.t.Helper()
	if !r.holds {
		sandbox.RustFeatureMissing(r.t, "the binary lacks hold_tokens (Rust holds v7.2); the holds checks need a build with it")
	}
}

// followRaw submits a followed ordinary turn on realThread outside the bot
// (no Telegram sends) and returns once it ended held. cancel drops the
// follower's connection, as a crash would.
func (r *realRust) followRaw(text string, replay bool) (sub *engine.Submission, end engine.TurnEnd, cancel context.CancelFunc) {
	r.t.Helper()
	ctx, cancel := context.WithCancel(context.Background())
	r.t.Cleanup(cancel)
	thread, topic := realThread, int64(5)
	sub, err := pinnedModel{r.eng}.SubmitOrdinary(ctx, engine.Submit{
		ThreadID: &thread, Root: r.root, Route: &engine.Route{Chat: -100, Topic: &topic},
		Text: text, Delivery: engine.DeliveryQueue, Replay: replay,
	})
	if err != nil {
		r.t.Fatalf("submit: %v\n%s", err, r.log())
	}
	for range sub.Turn.Events {
	}
	end = sub.Turn.Wait()
	if !end.Ran || !end.Held || sub.Holder == "" {
		r.t.Fatalf("want a held turn with a token: end %+v holder %q\n%s", end, sub.Holder, r.log())
	}
	return sub, end, cancel
}

// rawRenderer subscribes as renderer outside the bot and returns its token.
// rawRenderer subscribes as renderer outside the bot. Like the bridge, it
// retries a conflict for a few seconds: the service notices a closed
// renderer connection (stopBridge, Close) only a moment later.
func (r *realRust) rawRenderer(replay bool) (*engine.Stream, string) {
	r.t.Helper()
	deadline := time.Now().Add(5 * time.Second)
	for {
		stream, err := r.eng.Open(context.Background(), engine.Subscribe{Renderer: true, Replay: replay})
		if err != nil {
			r.t.Fatal(err)
		}
		first, err := stream.Next()
		if err == nil {
			err = first.Err()
		}
		var se *engine.ServiceError
		if errors.As(err, &se) && se.Code == "conflict" && time.Now().Before(deadline) {
			stream.Close()
			time.Sleep(100 * time.Millisecond)
			continue
		}
		r.t.Cleanup(func() { stream.Close() })
		if err != nil || first.Kind != engine.ReplySubscribed || first.Holder == "" {
			r.t.Fatalf("renderer subscription: %+v %v\n%s", first, err, r.log())
		}
		return stream, first.Holder
	}
}

// nextEvent reads events until one matches or limit passes; nil on timeout
// or when the stream ends.
func nextEvent(stream *engine.Stream, limit time.Duration, match func(engine.Event) bool) *engine.Event {
	found := make(chan *engine.Event, 1)
	go func() {
		for {
			reply, err := stream.Next()
			if err != nil {
				found <- nil
				return
			}
			if reply.Kind == engine.ReplyEvent && reply.Event != nil && match(*reply.Event) {
				ev := *reply.Event
				found <- &ev
				return
			}
		}
	}()
	select {
	case ev := <-found:
		return ev
	case <-time.After(limit):
		return nil
	}
}

func (r *realRust) pendingRecord() engine.DeliveryRecord {
	topic := int64(5)
	return engine.NewDeliveryRecord(-100, &topic, engine.TextItems(1))
}

// lastDelivery is the item statuses of the thread's last delivery record.
func (r *realRust) lastDelivery() (id string, statuses []string) {
	d := r.events(realThread, "delivery")
	if len(d) == 0 {
		return "", nil
	}
	// The record's fields are flattened into the event line.
	rec := d[len(d)-1]
	id, _ = rec["id"].(string)
	items, _ := rec["items"].([]any)
	for _, it := range items {
		m, _ := it.(map[string]any)
		s, _ := m["status"].(string)
		statuses = append(statuses, s)
	}
	return id, statuses
}

// A follower opted in to replay drops its connection before its pending
// record: the service hands the turn to the renderer, which posts the reply
// only (no status message), records it with its own token and releases.
func TestRealRustHoldsReplayAfterFollowerLoss(t *testing.T) {
	r := startRealRust(t, answer("REPLAYED-ANSWER"))
	r.requireHolds()
	_, _, crash := r.followRaw("hi", true)
	crash()
	r.waitFor("the replayed reply", 20*time.Second, func() bool { return r.sent("REPLAYED-ANSWER") })
	r.waitFor("settled", 20*time.Second, func() bool { return r.settled(realThread) })
	if n := len(r.tg.of("sendMessage")); n != 1 {
		t.Fatalf("a replay posts the reply only: %d messages %+v", n, r.tg.calls)
	}
	if _, st := r.lastDelivery(); len(st) != 1 || st[0] != engine.DeliverySent {
		t.Fatalf("the replay records its delivery: %v", st)
	}
}

// A follower whose pending record was acknowledged drops its connection:
// delivery started, so the service marks the pending items unknown and
// releases. Nothing is replayed or sent.
func TestRealRustHoldsCrashAfterPendingEndsUnknown(t *testing.T) {
	r := startRealRust(t, answer("NEVER-SENT"))
	r.requireHolds()
	sub, end, crash := r.followRaw("hi", true)
	rec := r.pendingRecord()
	if _, err := r.eng.RecordDelivery(context.Background(), realThread, &engine.Hold{TurnID: end.TurnID, Holder: sub.Holder}, rec); err != nil {
		t.Fatalf("pending record: %v\n%s", err, r.log())
	}
	crash()
	r.waitFor("settled", 20*time.Second, func() bool { return r.settled(realThread) })
	time.Sleep(time.Second)
	if n := len(r.tg.of("sendMessage")); n != 0 {
		t.Fatalf("a started delivery is never resent: %+v", r.tg.calls)
	}
	if id, st := r.lastDelivery(); id != rec.ID || len(st) != 1 || st[0] != engine.DeliveryUnknown {
		t.Fatalf("the pending record becomes unknown: %s %v (want %s)", id, st, rec.ID)
	}
}

// A replayed turn's renderer acknowledges its pending record and then
// crashes: the item becomes unknown and the next renderer gets no second
// replay.
func TestRealRustHoldsReplayedRendererCrashAfterPending(t *testing.T) {
	r := startRealRust(t, answer("REPLAY-ONCE"))
	r.requireHolds()
	r.stopBridge()
	first, holder := r.rawRenderer(true)
	_, end, crash := r.followRaw("hi", true)
	crash()
	replay := nextEvent(first, 20*time.Second, func(ev engine.Event) bool {
		return ev.Type == engine.EventTurnReplay && ev.ThreadID == realThread
	})
	if replay == nil || replay.TurnID != end.TurnID || replay.Route == nil || replay.FinalText == nil || *replay.FinalText != "REPLAY-ONCE" {
		t.Fatalf("turn_replay: %+v\n%s", replay, r.log())
	}
	rec := r.pendingRecord()
	if _, err := r.eng.RecordDelivery(context.Background(), realThread, &engine.Hold{TurnID: end.TurnID, Holder: holder}, rec); err != nil {
		t.Fatalf("the replay renderer records with its token: %v", err)
	}
	first.Close()
	r.waitFor("settled", 20*time.Second, func() bool { return r.settled(realThread) })
	second, _ := r.rawRenderer(true)
	if again := nextEvent(second, 2*time.Second, func(ev engine.Event) bool { return ev.Type == engine.EventTurnReplay }); again != nil {
		t.Fatalf("a started delivery is never replayed again: %+v", again)
	}
	if id, st := r.lastDelivery(); id != rec.ID || len(st) != 1 || st[0] != engine.DeliveryUnknown {
		t.Fatalf("the pending record becomes unknown: %s %v", id, st)
	}
}

// The renderer that owns an unfollowed turn goes away before its pending
// record: the next renderer (the Go bridge) gets the replay and posts the
// reply only.
func TestRealRustHoldsRendererLossBeforePending(t *testing.T) {
	r := startRealRust(t, answer("UNFOLLOWED-ANSWER"))
	r.requireHolds()
	r.stopBridge()
	first, _ := r.rawRenderer(true)
	thread, topic, model := realThread, int64(5), mockModel
	if _, err := r.eng.CallOK(context.Background(), engine.Submit{
		ThreadID: &thread, Ordinary: true, Root: r.root, Model: &model, Route: &engine.Route{Chat: -100, Topic: &topic},
		Text: "hi", Follow: engine.FollowNone, Delivery: engine.DeliveryQueue,
	}); err != nil {
		t.Fatalf("unfollowed submit: %v\n%s", err, r.log())
	}
	ended := nextEvent(first, 20*time.Second, func(ev engine.Event) bool {
		return ev.Type == engine.EventTurnEnded && ev.ThreadID == realThread
	})
	if ended == nil || !ended.Held {
		t.Fatalf("the renderer holds the unfollowed turn: %+v\n%s", ended, r.log())
	}
	first.Close()
	r.startBridge()
	r.waitFor("the replayed reply", 20*time.Second, func() bool { return r.sent("UNFOLLOWED-ANSWER") })
	r.waitFor("settled", 20*time.Second, func() bool { return r.settled(realThread) })
	if n := len(r.tg.of("sendMessage")); n != 1 {
		t.Fatalf("a replay posts the reply only: %d messages %+v", n, r.tg.calls)
	}
}

// A second renderer is refused while the bridge holds the role; a bridge
// that keeps being refused stops with ErrRendererConflict, and the first
// one keeps rendering.
func TestRealRustHoldsRendererConflict(t *testing.T) {
	r := startRealRust(t, answer("STILL-RENDERING"))
	r.requireHolds()
	stream, err := r.eng.Open(context.Background(), engine.Subscribe{Renderer: true, Replay: true})
	if err != nil {
		t.Fatal(err)
	}
	first, err := stream.Next()
	stream.Close()
	var se *engine.ServiceError
	if err == nil {
		err = first.Err()
	}
	if !errors.As(err, &se) || se.Code != "conflict" {
		t.Fatalf("a second renderer is refused with conflict: %+v %v", first, err)
	}
	b := r.newBridge()
	b.ConflictDelay = 10 * time.Millisecond
	done := make(chan error, 1)
	go func() { done <- b.Run(context.Background()) }()
	select {
	case err := <-done:
		if !errors.Is(err, ErrRendererConflict) {
			t.Fatalf("want ErrRendererConflict, got %v", err)
		}
	case <-time.After(10 * time.Second):
		t.Fatal("the refused bridge must stop")
	}
	r.h.Dispatch(context.Background(), userMessage(t, 42, "hi"))
	r.waitFor("the reply", 20*time.Second, func() bool { return r.sent("STILL-RENDERING") })
	r.h.Wait()
}

// Records and releases must name the hold and its token: a wrong token or
// none is refused and changes nothing, and the Deliverer sends nothing
// after a refused pending record.
func TestRealRustHoldsRefusedRecordsAndReleases(t *testing.T) {
	r := startRealRust(t, answer("REFUSED"))
	r.requireHolds()
	sub, end, _ := r.followRaw("hi", false)
	ctx := context.Background()
	code := func(err error) string {
		var se *engine.ServiceError
		if errors.As(err, &se) {
			return se.Code
		}
		return ""
	}
	if _, err := r.eng.RecordDelivery(ctx, realThread, &engine.Hold{TurnID: end.TurnID, Holder: "not-the-holder"}, r.pendingRecord()); code(err) != "conflict" {
		t.Fatalf("a record with another token: %v", err)
	}
	if _, err := r.eng.RecordDelivery(ctx, realThread, nil, r.pendingRecord()); code(err) != "conflict" {
		t.Fatalf("a record without the hold while held: %v", err)
	}
	topic := int64(5)
	target := ReplyTarget{Chat: -100, Topic: &topic}
	plan, err := ServicePlanner{Engine: r.eng}.PlanReply(ctx, "REFUSED", target, realThread, nil)
	if err != nil {
		t.Fatal(err)
	}
	client := r.tg.client()
	d := &Deliverer{Telegram: client, Store: r.eng, Followups: &Followups{}, Log: quiet}
	if err := d.Deliver(ctx, realThread, &engine.Hold{TurnID: end.TurnID, Holder: "not-the-holder"}, target, nil, plan); !errors.Is(err, ErrDeliveryUnrecorded) {
		t.Fatalf("a refused pending record stops the delivery: %v", err)
	}
	if n := len(r.tg.of("sendMessage")); n != 0 {
		t.Fatalf("nothing is sent without an acknowledged pending record: %+v", r.tg.calls)
	}
	if err := r.eng.ReleaseHeld(ctx, realThread, end.TurnID, "not-the-holder"); code(err) != "conflict" {
		t.Fatalf("a release with another token: %v", err)
	}
	if err := r.eng.Release(ctx, realThread, end.TurnID); code(err) != "invalid" {
		t.Fatalf("a release without a token: %v", err)
	}
	if r.settled(realThread) {
		t.Fatal("refused releases leave the hold")
	}
	if err := r.eng.ReleaseHeld(ctx, realThread, end.TurnID, sub.Holder); err != nil {
		t.Fatal(err)
	}
	r.waitFor("settled", 10*time.Second, func() bool { return r.settled(realThread) })
	if d := r.events(realThread, "delivery"); len(d) != 0 {
		t.Fatalf("refused records write nothing: %v", d)
	}
}
