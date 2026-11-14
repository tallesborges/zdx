package bot

import (
	"context"
	"errors"
	"net"
	"strings"
	"sync/atomic"
	"testing"
	"time"

	"github.com/tallesborges/zdx/apps/gateway/internal/engine"
)

// holdService is a fake service with hold tokens (Rust holds v7.2).
func holdService(t *testing.T, onSubscribe func(conn net.Conn, req map[string]any)) (string, chan map[string]any) {
	sock, ln := listen(t)
	releases := make(chan map[string]any, 8)
	go func() {
		for {
			conn, err := ln.Accept()
			if err != nil {
				return
			}
			go func() {
				req := readReq(conn)
				switch req["op"] {
				case "status":
					write(conn, map[string]any{"reply": "status", "report": map[string]any{"protocol": 2, "capabilities": []string{engine.CapHoldTokens, engine.CapRecordDelivery}, "settled": true, "threads": []any{}}})
					conn.Close()
				case "subscribe":
					onSubscribe(conn, req)
				case "release":
					releases <- req
					write(conn, map[string]any{"reply": "ok"})
					conn.Close()
				default:
					conn.Close()
				}
			}()
		}
	}()
	return sock, releases
}

// The renderer subscribes with replay, takes its token from the direct
// reply (events carry none), releases its turns and replays with it, and
// renders a replay as a reply without a status message.
func TestBridgeOwnsItsTurnsWithTheSubscriptionToken(t *testing.T) {
	route := map[string]any{"chat": -100, "topic": 5}
	subs := make(chan map[string]any, 1)
	sock, releases := holdService(t, func(conn net.Conn, req map[string]any) {
		subs <- req
		write(conn, map[string]any{"reply": "subscribed", "holder": "R1"})
		ev := func(e map[string]any) { write(conn, map[string]any{"reply": "event", "event": e}) }
		ev(map[string]any{"type": "turn_started", "thread_id": "o", "turn_id": 3, "kind": "worker_update", "inputs": []any{}, "route": route, "followed": false})
		ev(map[string]any{"type": "agent", "thread_id": "o", "turn_id": 3, "event": map[string]any{"type": "turn_finished", "status": map[string]any{"status": "completed"}, "final_text": "done", "messages": []any{}}})
		ev(map[string]any{"type": "turn_ended", "thread_id": "o", "turn_id": 3, "held": true})
		ev(map[string]any{"type": "turn_replay", "thread_id": "lost", "turn_id": 7, "route": route, "status": "completed", "final_text": "replayed answer", "error": nil})
		time.Sleep(50 * time.Millisecond)
		conn.Close()
	})
	r := &renderLog{done: make(chan struct{}, 4)}
	ctx, cancel := context.WithCancel(context.Background())
	b := &Bridge{Engine: engine.NewClient(sock), Renderer: r, Log: quiet, Delay: time.Hour, Replay: true}
	finished := make(chan error, 1)
	go func() { finished <- b.Run(ctx) }()
	<-r.done
	<-r.done
	got := map[string]map[string]any{}
	for range 2 {
		rel := <-releases
		got[rel["thread_id"].(string)] = rel
	}
	cancel()
	if err := <-finished; err != nil {
		t.Fatal(err)
	}
	if req := <-subs; req["renderer"] != true || req["replay"] != true {
		t.Fatalf("subscribe %v", req)
	}
	if got["o"]["holder"] != "R1" || got["o"]["turn_id"] != float64(3) || got["lost"]["holder"] != "R1" || got["lost"]["turn_id"] != float64(7) {
		t.Fatalf("releases carry the subscription token: %v", got)
	}
	if _, forced := got["o"]["force"]; forced {
		t.Fatal("the gateway never forces a release")
	}
	r.mu.Lock()
	defer r.mu.Unlock()
	for i, rt := range r.rendered {
		if rt.Holder != "R1" || rt.Replay != (rt.ThreadID == "lost") {
			t.Fatalf("turn %s: holder %q replay %v", rt.ThreadID, rt.Holder, rt.Replay)
		}
		if rt.ThreadID == "lost" && r.finals[i] != "replayed answer/completed" {
			t.Fatalf("replay final %q", r.finals[i])
		}
	}
}

// A renderer that does not opt in to replay still owns the unfollowed
// turns of a service with hold tokens: it reads the token from the direct
// subscribed reply and releases with it. A token in an event is ignored.
func TestBridgeWithoutReplayStillTakesTheSubscriptionToken(t *testing.T) {
	route := map[string]any{"chat": -100, "topic": 5}
	subs := make(chan map[string]any, 1)
	sock, releases := holdService(t, func(conn net.Conn, req map[string]any) {
		subs <- req
		write(conn, map[string]any{"reply": "subscribed", "holder": "R3"})
		ev := func(e map[string]any) { write(conn, map[string]any{"reply": "event", "event": e}) }
		ev(map[string]any{"type": "turn_started", "thread_id": "u", "turn_id": 4, "kind": "user", "inputs": []any{}, "route": route, "followed": false, "holder": "FROM-EVENT"})
		ev(map[string]any{"type": "agent", "thread_id": "u", "turn_id": 4, "event": map[string]any{"type": "turn_finished", "status": map[string]any{"status": "completed"}, "final_text": "hi", "messages": []any{}}})
		ev(map[string]any{"type": "turn_ended", "thread_id": "u", "turn_id": 4, "held": true, "holder": "FROM-EVENT"})
		time.Sleep(50 * time.Millisecond)
		conn.Close()
	})
	r := &renderLog{done: make(chan struct{}, 1)}
	ctx, cancel := context.WithCancel(context.Background())
	b := &Bridge{Engine: engine.NewClient(sock), Renderer: r, Log: quiet, Delay: time.Hour}
	finished := make(chan error, 1)
	go func() { finished <- b.Run(ctx) }()
	<-r.done
	rel := <-releases
	cancel()
	if err := <-finished; err != nil {
		t.Fatal(err)
	}
	if req := <-subs; req["renderer"] != true {
		t.Fatalf("subscribe %v", req)
	} else if _, replay := req["replay"]; replay {
		t.Fatalf("no replay opt-in was asked for: %v", req)
	}
	if rel["holder"] != "R3" || rel["turn_id"] != float64(4) {
		t.Fatalf("release carries the subscription token: %v", rel)
	}
	r.mu.Lock()
	defer r.mu.Unlock()
	if len(r.rendered) != 1 || r.rendered[0].Holder != "R3" {
		t.Fatalf("rendered %+v", r.rendered)
	}
}

// Status said no hold tokens, but the service restarted onto a build with
// them before the subscription: the bridge resubscribes to read the token
// instead of rendering holds it cannot release.
func TestBridgeResubscribesWhenTheServiceGainsHoldTokens(t *testing.T) {
	sock, ln := listen(t)
	var statuses atomic.Int32
	subs := make(chan map[string]any, 4)
	releases := make(chan map[string]any, 4)
	route := map[string]any{"chat": -100, "topic": 5}
	go func() {
		for {
			conn, err := ln.Accept()
			if err != nil {
				return
			}
			go func() {
				req := readReq(conn)
				switch req["op"] {
				case "status":
					caps := []string{engine.CapRecordDelivery}
					if statuses.Add(1) > 1 {
						caps = append(caps, engine.CapHoldTokens)
					}
					write(conn, map[string]any{"reply": "status", "report": map[string]any{"protocol": 2, "capabilities": caps, "settled": true, "threads": []any{}}})
					conn.Close()
				case "subscribe":
					subs <- req
					write(conn, map[string]any{"reply": "subscribed", "holder": "R4"})
					ev := func(e map[string]any) { write(conn, map[string]any{"reply": "event", "event": e}) }
					ev(map[string]any{"type": "turn_started", "thread_id": "u", "turn_id": 1, "kind": "user", "inputs": []any{}, "route": route, "followed": false})
					ev(map[string]any{"type": "agent", "thread_id": "u", "turn_id": 1, "event": map[string]any{"type": "turn_finished", "status": map[string]any{"status": "completed"}, "final_text": "hi", "messages": []any{}}})
					ev(map[string]any{"type": "turn_ended", "thread_id": "u", "turn_id": 1, "held": true})
					time.Sleep(50 * time.Millisecond)
					conn.Close()
				case "release":
					releases <- req
					write(conn, map[string]any{"reply": "ok"})
					conn.Close()
				default:
					conn.Close()
				}
			}()
		}
	}()
	r := &renderLog{done: make(chan struct{}, 2)}
	ctx, cancel := context.WithCancel(context.Background())
	b := &Bridge{Engine: engine.NewClient(sock), Renderer: r, Log: quiet, Delay: 10 * time.Millisecond, Replay: true}
	finished := make(chan error, 1)
	go func() { finished <- b.Run(ctx) }()
	<-r.done
	rel := <-releases
	cancel()
	if err := <-finished; err != nil {
		t.Fatal(err)
	}
	if rel["holder"] != "R4" {
		t.Fatalf("release %v", rel)
	}
	first, second := <-subs, <-subs
	if _, replay := first["replay"]; replay || second["replay"] != true {
		t.Fatalf("subscriptions %v then %v", first, second)
	}
	r.mu.Lock()
	defer r.mu.Unlock()
	if len(r.rendered) != 1 {
		t.Fatalf("only the turn seen with its token renders, got %d", len(r.rendered))
	}
}

// A refused renderer subscription (another renderer is live) is retried at
// most 3 times, then Run fails with ErrRendererConflict.
func TestBridgeStopsAfterRepeatedRendererConflicts(t *testing.T) {
	var attempts atomic.Int32
	sock, _ := holdService(t, func(conn net.Conn, _ map[string]any) {
		attempts.Add(1)
		write(conn, map[string]any{"reply": "error", "code": "conflict", "message": "another renderer is subscribed"})
		conn.Close()
	})
	r := &renderLog{done: make(chan struct{}, 4)}
	b := &Bridge{Engine: engine.NewClient(sock), Renderer: r, Log: quiet, Delay: time.Hour, Replay: true, ConflictDelay: 10 * time.Millisecond}
	done := make(chan error, 1)
	go func() { done <- b.Run(context.Background()) }()
	select {
	case err := <-done:
		if !errors.Is(err, ErrRendererConflict) || attempts.Load() != 4 || len(r.rendered) != 0 {
			t.Fatalf("err=%v attempts=%d rendered=%d", err, attempts.Load(), len(r.rendered))
		}
	case <-time.After(5 * time.Second):
		t.Fatal("the bridge must stop after repeated conflicts")
	}
}

// A conflict that clears within the retries connects normally.
func TestBridgeRecoversFromABriefRendererConflict(t *testing.T) {
	var attempts atomic.Int32
	connected := make(chan struct{}, 1)
	sock, _ := holdService(t, func(conn net.Conn, _ map[string]any) {
		if attempts.Add(1) <= 2 {
			write(conn, map[string]any{"reply": "error", "code": "conflict", "message": "another renderer is subscribed"})
			conn.Close()
			return
		}
		write(conn, map[string]any{"reply": "subscribed", "holder": "R2"})
		connected <- struct{}{}
		time.Sleep(50 * time.Millisecond)
		conn.Close()
	})
	ctx, cancel := context.WithCancel(context.Background())
	b := &Bridge{Engine: engine.NewClient(sock), Renderer: &renderLog{done: make(chan struct{}, 1)}, Log: quiet, Delay: time.Hour, Replay: true, ConflictDelay: 10 * time.Millisecond}
	done := make(chan error, 1)
	go func() { done <- b.Run(ctx) }()
	select {
	case <-connected:
	case <-time.After(5 * time.Second):
		t.Fatal("no connection after the conflict cleared")
	}
	cancel()
	if err := <-done; err != nil {
		t.Fatal(err)
	}
}

// A followed held turn: replay opt-in on the submit, records and release
// carry the token from the submit reply.
func TestFollowedHeldTurnUsesTheSubmitToken(t *testing.T) {
	tg := newFakeTG(t)
	svc := newFakeSvc(t, []string{engine.CapOrdinarySubmit, engine.CapComposeUserText, engine.CapRecordDelivery, engine.CapHoldTokens})
	svc.submit = func(sc svcConn) {
		c := sc.conn
		write(c, map[string]any{"reply": "submitted", "thread_id": thread, "input_id": 1, "queued_ahead": 0, "holder": "F1"})
		write(c, agent(thread, 9, map[string]any{"type": "turn_finished", "status": map[string]any{"status": "completed"}, "final_text": "answer", "messages": []any{}}))
		write(c, event(map[string]any{"type": "turn_ended", "thread_id": thread, "turn_id": 9, "held": true}))
		for range 500 {
			if len(svc.requests("release")) > 0 {
				break
			}
			time.Sleep(10 * time.Millisecond)
		}
		c.Close()
	}
	h, _ := newHandler(tg, svc, TopicInfo{ThreadID: thread, Kind: TopicOrdinary})
	h.Replay = true
	h.Dispatch(context.Background(), userMessage(t, 42, "hi"))
	tg.waitFor(t, "the release", func() bool { return len(svc.requests("release")) == 1 })
	h.Wait()
	if sub := svc.requests("submit")[0]; sub["replay"] != true {
		t.Fatalf("the follower opts in to replay: %v", sub)
	}
	recs := svc.requests("record_delivery")
	if len(recs) != 2 {
		t.Fatalf("records %v", recs)
	}
	for _, r := range recs {
		if r["turn_id"] != float64(9) || r["holder"] != "F1" {
			t.Fatalf("records name the hold: %v", r)
		}
	}
	if rel := svc.requests("release")[0]; rel["holder"] != "F1" || rel["turn_id"] != float64(9) {
		t.Fatalf("release %v", rel)
	}
}

// A replay posts the reply only (no status message, no Cancel), with the
// hold's records; a failed replay posts its error.
func TestReplayPostsOnlyTheReply(t *testing.T) {
	tg := newFakeTG(t)
	store := &memStore{}
	p := &Presenter{Telegram: tg.client(), Planner: TextPlanner{}, Cancels: &Cancels{}, Log: quiet,
		Deliverer: &Deliverer{Telegram: tg.client(), Store: store, Followups: &Followups{}, Log: quiet}}
	route := engine.Route{Chat: -100, Topic: ptr(int64(5))}
	p.RenderTurn(context.Background(), RoutedTurn{ThreadID: thread, TurnID: 7, Route: route, Holder: "R1", Replay: true, Turn: engine.ReplayTurn(7, "completed", "late answer", nil)})
	sends := tg.of("sendMessage")
	if len(sends) != 1 || sends[0].body["text"] != "late answer" || sends[0].body["reply_markup"] != nil {
		t.Fatalf("only the reply: %+v", sends)
	}
	if len(store.holds) != 2 || *store.holds[0] != (engine.Hold{TurnID: 7, Holder: "R1"}) {
		t.Fatalf("records name the hold: %v", store.holds)
	}
	msg := "provider overloaded"
	p.RenderTurn(context.Background(), RoutedTurn{ThreadID: thread, TurnID: 8, Route: route, Holder: "R1", Replay: true, Turn: engine.ReplayTurn(8, "failed", "", &msg)})
	if last := tg.of("sendMessage")[1].body["text"].(string); !strings.Contains(last, "provider overloaded") {
		t.Fatalf("failed replay posts its error: %q", last)
	}
	if len(store.holds) != 4 || *store.holds[2] != (engine.Hold{TurnID: 8, Holder: "R1"}) {
		t.Fatalf("the error is recorded like a reply: %v", store.holds)
	}
	// A refused pending record sends nothing, error or reply.
	refused := &memStore{err: &engine.ServiceError{Code: "conflict", Message: "not the holder"}}
	p.Deliverer.Store = refused
	p.RenderTurn(context.Background(), RoutedTurn{ThreadID: thread, TurnID: 9, Route: route, Holder: "R1", Replay: true, Turn: engine.ReplayTurn(9, "failed", "", &msg)})
	if n := len(tg.of("sendMessage")); n != 2 {
		t.Fatalf("nothing is sent without an acknowledged pending record: %d", n)
	}
}

// A release with no answer is retried with the same token; a refused one
// is not.
func TestReleaseRetriesTransportFailuresOnly(t *testing.T) {
	for _, refuse := range []bool{false, true} {
		var releases atomic.Int32
		sock, ln := listen(t)
		go func() {
			for {
				conn, err := ln.Accept()
				if err != nil {
					return
				}
				go func() {
					defer conn.Close()
					req := readReq(conn)
					switch req["op"] {
					case "status":
						write(conn, map[string]any{"reply": "status", "report": map[string]any{"protocol": 2, "capabilities": []string{engine.CapHoldTokens}, "settled": true, "threads": []any{}}})
					case "release":
						if req["holder"] != "H" {
							t.Errorf("release %v", req)
						}
						switch n := releases.Add(1); {
						case refuse:
							write(conn, map[string]any{"reply": "error", "code": "conflict", "message": "not the holder"})
						case n == 1:
							// No answer: the connection just closes.
						default:
							write(conn, map[string]any{"reply": "ok"})
						}
					}
				}()
			}
		}()
		turn := engine.ReplayTurn(3, "completed", "x", nil)
		Render(context.Background(), engine.NewClient(sock), &renderLog{done: make(chan struct{}, 1)}, RoutedTurn{ThreadID: "t", TurnID: 3, Holder: "H", Turn: turn}, quiet)
		if want := map[bool]int32{false: 2, true: 1}[refuse]; releases.Load() != want {
			t.Fatalf("refuse=%v: %d releases, want %d", refuse, releases.Load(), want)
		}
	}
}
