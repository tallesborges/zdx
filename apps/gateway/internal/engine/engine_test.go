package engine

import (
	"bufio"
	"context"
	"encoding/json"
	"errors"
	"net"
	"os"
	"path/filepath"
	"strings"
	"sync"
	"testing"
	"time"
)

// fakeService is a synthetic engine service on a temp Unix socket.
// handle gets each connection's decoded request and the connection.
type fakeService struct {
	t      *testing.T
	socket string
	ln     net.Listener
	mu     sync.Mutex
	seen   []map[string]any
	handle func(req map[string]any, conn net.Conn)
}

func newFakeService(t *testing.T, handle func(req map[string]any, conn net.Conn)) *fakeService {
	t.Helper()
	// Short path: macOS limits Unix socket paths to 104 bytes.
	dir, err := os.MkdirTemp("", "gw")
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { os.RemoveAll(dir) })
	socket := filepath.Join(dir, "o.sock")
	ln, err := net.Listen("unix", socket)
	if err != nil {
		t.Fatal(err)
	}
	f := &fakeService{t: t, socket: socket, ln: ln, handle: handle}
	t.Cleanup(func() { ln.Close() })
	go func() {
		for {
			conn, err := ln.Accept()
			if err != nil {
				return
			}
			go func() {
				line, err := bufio.NewReader(conn).ReadBytes('\n')
				if err != nil {
					conn.Close()
					return
				}
				var req map[string]any
				if err := json.Unmarshal(line, &req); err != nil {
					t.Errorf("bad request line %q: %v", line, err)
					conn.Close()
					return
				}
				f.mu.Lock()
				f.seen = append(f.seen, req)
				f.mu.Unlock()
				f.handle(req, conn)
			}()
		}
	}()
	return f
}

func (f *fakeService) requests(op string) []map[string]any {
	f.mu.Lock()
	defer f.mu.Unlock()
	var out []map[string]any
	for _, r := range f.seen {
		if r["op"] == op {
			out = append(out, r)
		}
	}
	return out
}

func send(t *testing.T, conn net.Conn, v any) {
	t.Helper()
	b, _ := json.Marshal(v)
	if _, err := conn.Write(append(b, '\n')); err != nil {
		t.Errorf("write: %v", err)
	}
}

func statusReply(protocol any, caps []string) map[string]any {
	report := map[string]any{"settled": true, "threads": []any{}}
	if protocol != nil {
		report["protocol"] = protocol
		report["capabilities"] = caps
	}
	return map[string]any{"reply": "status", "report": report}
}

func agentEvent(threadID string, turnID uint64, inner map[string]any) map[string]any {
	return map[string]any{"reply": "event", "event": map[string]any{
		"type": "agent", "thread_id": threadID, "turn_id": turnID, "event": inner,
	}}
}

func TestEncodeRequestTagsOpAndDefaults(t *testing.T) {
	line, err := EncodeRequest(Submit{Root: "/r", Text: "hi", Follow: FollowTurn, Delivery: DeliveryQueue})
	if err != nil {
		t.Fatal(err)
	}
	var got map[string]any
	json.Unmarshal(line, &got)
	if got["op"] != "submit" || got["root"] != "/r" || got["follow"] != "turn" {
		t.Fatalf("unexpected submit %s", line)
	}
	if imgs, ok := got["images"].([]any); !ok || len(imgs) != 0 {
		t.Fatalf("images must be an empty array, got %s", line)
	}
	if _, ok := got["ordinary"]; ok {
		t.Fatalf("ordinary must be absent unless set: %s", line)
	}
	id := uint64(7)
	line, _ = EncodeRequest(Cancel{ThreadID: "t", TurnID: &id})
	json.Unmarshal(line, &got)
	if got["op"] != "cancel" || got["turn_id"] != float64(7) || got["clear_queue"] != false {
		t.Fatalf("unexpected cancel %s", line)
	}
	line, _ = EncodeRequest(Subscribe{Renderer: true})
	if string(line) != `{"op":"subscribe","renderer":true}` {
		t.Fatalf("unexpected subscribe %s", line)
	}
}

func TestStatusWithoutProtocolIsV1WithoutCapabilities(t *testing.T) {
	f := newFakeService(t, func(_ map[string]any, conn net.Conn) {
		send(t, conn, statusReply(nil, nil))
		conn.Close()
	})
	c := NewClient(f.socket)
	report, err := c.StatusReport(context.Background(), nil)
	if err != nil {
		t.Fatal(err)
	}
	if report.Version() != 1 || report.Has(CapOrdinarySubmit) {
		t.Fatalf("absent protocol must read as v1 without capabilities: %+v", report)
	}
}

func TestCapabilityNeedsProtocolTwo(t *testing.T) {
	one := uint32(1)
	r := &StatusReport{Protocol: &one, Capabilities: []string{CapOrdinarySubmit}}
	if r.Has(CapOrdinarySubmit) {
		t.Fatal("capabilities below protocol 2 must be ignored")
	}
	two := uint32(2)
	r.Protocol = &two
	if !r.Has(CapOrdinarySubmit) || r.Has(CapRerun) {
		t.Fatal("exact capability match expected")
	}
}

func TestSubmitOrdinarySendsNothingWithoutCapability(t *testing.T) {
	f := newFakeService(t, func(req map[string]any, conn net.Conn) {
		if req["op"] == "status" {
			send(t, conn, statusReply(2, []string{CapRerun}))
		}
		conn.Close()
	})
	c := NewClient(f.socket)
	_, err := c.SubmitOrdinary(context.Background(), Submit{Root: "/r", Text: "hi"})
	var unsupported *ErrUnsupported
	if !errors.As(err, &unsupported) || unsupported.Capability != CapOrdinarySubmit || unsupported.Protocol != 2 {
		t.Fatalf("want ErrUnsupported, got %v", err)
	}
	if n := len(f.requests("submit")); n != 0 {
		t.Fatalf("no submit may be sent, got %d", n)
	}
}

func TestSubmitOrdinaryRefusesProfile(t *testing.T) {
	c := NewClient("/nonexistent/o.sock")
	p := "orchestrator"
	if _, err := c.SubmitOrdinary(context.Background(), Submit{Profile: &p}); err == nil {
		t.Fatal("profile must be refused")
	}
}

func TestSteeredSubmitHasNoTurn(t *testing.T) {
	f := newFakeService(t, func(req map[string]any, conn net.Conn) {
		switch req["op"] {
		case "status":
			send(t, conn, statusReply(2, []string{CapOrdinarySubmit}))
		case "submit":
			if req["ordinary"] != true || req["delivery"] != "steer" || req["follow"] != "turn" {
				t.Errorf("unexpected submit %v", req)
			}
			send(t, conn, map[string]any{"reply": "submitted", "thread_id": "t1", "input_id": 4, "queued_ahead": 0, "in_turn": true})
		}
		conn.Close()
	})
	c := NewClient(f.socket)
	sub, err := c.SubmitOrdinary(context.Background(), Submit{Root: "/r", Text: "x", Delivery: DeliverySteer})
	if err != nil {
		t.Fatal(err)
	}
	if !sub.InTurn || sub.Turn != nil || sub.InputID != 4 {
		t.Fatalf("unexpected submission %+v", sub)
	}
}

// The follower must keep its connection open until the service closes it
// after release; the fake fails if the client closes first.
func TestFollowedTurnKeepsConnectionUntilRelease(t *testing.T) {
	released := make(chan struct{})
	closedEarly := make(chan bool, 1)
	f := newFakeService(t, func(req map[string]any, conn net.Conn) {
		switch req["op"] {
		case "submit":
			send(t, conn, map[string]any{"reply": "submitted", "thread_id": "t1", "input_id": 1, "queued_ahead": 0})
			send(t, conn, map[string]any{"reply": "event", "event": map[string]any{
				"type": "turn_started", "thread_id": "t1", "turn_id": 9, "kind": "user",
				"inputs": []any{map[string]any{"input_id": 1}}, "route": map[string]any{"chat": -100, "topic": 5}, "followed": true,
			}})
			send(t, conn, agentEvent("t1", 9, map[string]any{"type": "assistant_delta", "text": "he"}))
			send(t, conn, agentEvent("t1", 9, map[string]any{"type": "future_event_type", "x": 1}))
			send(t, conn, agentEvent("t1", 9, map[string]any{
				"type": "turn_finished", "status": map[string]any{"status": "completed"},
				"final_text": "hello", "messages": []any{}, "prior_message_count": 0,
			}))
			send(t, conn, map[string]any{"reply": "event", "event": map[string]any{"type": "turn_ended", "thread_id": "t1", "turn_id": 9, "held": true}})
			// Detect an early client close while the turn is held.
			conn.SetReadDeadline(time.Now().Add(5 * time.Second))
			go func() {
				buf := make([]byte, 1)
				_, err := conn.Read(buf)
				select {
				case <-released:
					closedEarly <- false
				default:
					closedEarly <- err != nil
				}
			}()
			<-released
			conn.Close()
		case "release":
			if req["thread_id"] != "t1" || req["turn_id"] != float64(9) {
				t.Errorf("unexpected release %v", req)
			}
			close(released)
			send(t, conn, map[string]any{"reply": "ok"})
			conn.Close()
		}
	})
	c := NewClient(f.socket)
	sub, err := c.SubmitFollow(context.Background(), Submit{Root: "/r", Text: "hi"})
	if err != nil {
		t.Fatal(err)
	}
	var types []string
	var final string
	for ev := range sub.Turn.Events {
		types = append(types, ev.Type)
		if ev.Type == AgentTurnFinished {
			final = ev.FinalText
			if ev.Status == nil || ev.Status.Status != "completed" {
				t.Fatalf("bad status %+v", ev.Status)
			}
		}
	}
	end := sub.Turn.Wait()
	if !end.Ran || !end.Held || end.TurnID != 9 || final != "hello" {
		t.Fatalf("end=%+v final=%q", end, final)
	}
	if len(types) != 3 || types[1] != "future_event_type" {
		t.Fatalf("unknown agent events must pass through: %v", types)
	}
	time.Sleep(50 * time.Millisecond)
	select {
	case early := <-closedEarly:
		if early {
			t.Fatal("client closed the followed connection before release")
		}
	default:
	}
	if err := c.Release(context.Background(), "t1", end.TurnID); err != nil {
		t.Fatal(err)
	}
	if early := <-closedEarly; early {
		t.Fatal("client closed the followed connection before release")
	}
}

func TestDroppedInputEndsWithFailure(t *testing.T) {
	f := newFakeService(t, func(_ map[string]any, conn net.Conn) {
		send(t, conn, map[string]any{"reply": "submitted", "thread_id": "t1", "input_id": 3, "queued_ahead": 1})
		send(t, conn, map[string]any{"reply": "event", "event": map[string]any{"type": "inputs_dropped", "thread_id": "t1", "input_ids": []any{3}}})
		conn.Close()
	})
	sub, err := NewClient(f.socket).SubmitFollow(context.Background(), Submit{Root: "/r"})
	if err != nil {
		t.Fatal(err)
	}
	var last AgentEvent
	for ev := range sub.Turn.Events {
		last = ev
	}
	if last.Type != AgentTurnFinished || last.Status.Status != "failed" || last.Status.Message != MsgInputDropped {
		t.Fatalf("unexpected last event %+v", last)
	}
	if end := sub.Turn.Wait(); end.Ran {
		t.Fatalf("no turn ran: %+v", end)
	}
}

func TestCancelTargetsInputAndEndsInterrupted(t *testing.T) {
	cancelled := make(chan map[string]any, 1)
	f := newFakeService(t, func(req map[string]any, conn net.Conn) {
		switch req["op"] {
		case "submit":
			send(t, conn, map[string]any{"reply": "submitted", "thread_id": "t1", "input_id": 11, "queued_ahead": 0})
			<-cancelled
			conn.Close() // service went away without turn_finished
		case "cancel":
			send(t, conn, map[string]any{"reply": "cancelled", "thread_id": "t1", "cancelled_turn": false, "dropped_inputs": []any{}})
			conn.Close()
			cancelled <- req
		}
	})
	c := NewClient(f.socket)
	sub, err := c.SubmitFollow(context.Background(), Submit{Root: "/r"})
	if err != nil {
		t.Fatal(err)
	}
	sub.Turn.Cancel()
	sub.Turn.Cancel()
	var last AgentEvent
	for ev := range sub.Turn.Events {
		last = ev
	}
	if last.Status == nil || last.Status.Status != "interrupted" {
		t.Fatalf("want interrupted, got %+v", last)
	}
	reqs := f.requests("cancel")
	if len(reqs) != 1 || reqs[0]["input_id"] != float64(11) || reqs[0]["turn_id"] != nil {
		t.Fatalf("unexpected cancels %v", reqs)
	}
}

func TestErrorReplyCarriesCode(t *testing.T) {
	f := newFakeService(t, func(req map[string]any, conn net.Conn) {
		send(t, conn, map[string]any{"reply": "error", "message": "nope", "code": "busy"})
		conn.Close()
	})
	_, err := NewClient(f.socket).CallOK(context.Background(), Worker{ThreadID: "w"})
	var se *ServiceError
	if !errors.As(err, &se) || se.Message != "nope" || se.Code != "busy" {
		t.Fatalf("want ServiceError, got %v", err)
	}
}

func TestNotRunning(t *testing.T) {
	dir, _ := os.MkdirTemp("", "gw")
	defer os.RemoveAll(dir)
	_, err := NewClient(filepath.Join(dir, "none.sock")).Call(context.Background(), Status{})
	if !errors.Is(err, ErrNotRunning) {
		t.Fatalf("want ErrNotRunning, got %v", err)
	}
}

func TestCallTimesOut(t *testing.T) {
	f := newFakeService(t, func(_ map[string]any, conn net.Conn) {
		time.Sleep(500 * time.Millisecond)
		conn.Close()
	})
	c := NewClient(f.socket)
	c.ReplyTimeout = 50 * time.Millisecond
	if _, err := c.Call(context.Background(), Status{}); err == nil {
		t.Fatal("want timeout")
	}
}

func TestWorkerSnapshotPassesThroughUnchanged(t *testing.T) {
	raw := `{"thread_id":"w","owner_thread_id":"o","root":"/r","status":"running","queue_depth":0,"queue":[],"latest_final_text":null,"last_error":null,"mirror_url":null,"current_tool":"bash","current_tool_input":"ls","seconds_since_last_activity":2,"turn_elapsed_seconds":5}`
	var w WorkerSnapshot
	if err := json.Unmarshal([]byte(raw), &w); err != nil {
		t.Fatal(err)
	}
	if w.Status != "running" || *w.CurrentTool != "bash" {
		t.Fatalf("unexpected %+v", w)
	}
	out, _ := json.Marshal(w)
	if string(out) != raw {
		t.Fatalf("snapshot must round-trip unchanged:\n%s", out)
	}
}

func TestFeedbackSubmissionTravelsAsTheExactText(t *testing.T) {
	submission := "{\n  \"note\": \"a <b> & \\ud800\",\n  \"n\": 1.50\n}"
	line, err := EncodeRequest(FeedbackSubmit{ThreadID: "t", UserID: 1, Submission: submission})
	if err != nil {
		t.Fatal(err)
	}
	var got map[string]any
	if err := json.Unmarshal(line, &got); err != nil {
		t.Fatal(err)
	}
	if got["op"] != "feedback_submit" || got["submission"] != submission {
		t.Fatalf("the submission must arrive as the same text:\n%s", line)
	}
	if strings.Contains(string(line), `\u003c`) {
		t.Fatalf("no HTML escaping: %s", line)
	}
}

func TestSubmitFeedbackNeedsCapability(t *testing.T) {
	f := newFakeService(t, func(req map[string]any, conn net.Conn) {
		if req["op"] == "status" {
			send(t, conn, statusReply(2, []string{CapOrdinarySubmit}))
		}
		conn.Close()
	})
	_, err := NewClient(f.socket).SubmitFeedback(context.Background(), FeedbackSubmit{ThreadID: "t", Submission: `{}`})
	var unsupported *ErrUnsupported
	if !errors.As(err, &unsupported) || unsupported.Capability != CapFeedbackSubmit {
		t.Fatalf("want ErrUnsupported, got %v", err)
	}
	if len(f.requests("feedback_submit")) != 0 {
		t.Fatal("nothing may be sent without the capability")
	}
}

func agentEv(inner string) Event {
	return Event{Type: EventAgent, ThreadID: "t", TurnID: 1, Inner: json.RawMessage(inner)}
}

// A stalled reader must not make the buffer grow with deltas, and no other
// event (tool calls, unknown types) may be lost or reordered.
func TestBufferCoalescesProgressAndKeepsEveryLosslessEvent(t *testing.T) {
	in, out := Buffer()
	for i := range 20_000 {
		in <- agentEv(`{"type":"tool_output_delta","id":"a","chunk":"` + strings.Repeat("x", i%50) + `"}`)
	}
	in <- agentEv(`{"type":"user_message_injected","id":"7","source":"user","text":"s"}`)
	in <- agentEv(`{"type":"tool_started","id":"t1","name":"create_thread"}`)
	in <- agentEv(`{"type":"future_kind","x":1}`)
	in <- agentEv(`{"type":"future_kind","x":2}`)
	in <- agentEv(`{"type":"tool_completed","id":"t1","result":{"status":"success"}}`)
	in <- agentEv(`{"type":"assistant_delta","text":"a"}`)
	in <- agentEv(`{"type":"assistant_delta","text":"ab"}`)
	in <- agentEv(`{"type":"turn_finished","status":{"status":"completed"},"final_text":"ab"}`)
	in <- Event{Type: EventTurnEnded, ThreadID: "t", TurnID: 1, Held: true}
	close(in)
	var got []string
	for ev := range out {
		if ev.Type == EventAgent {
			a, _ := ev.Agent()
			got = append(got, a.Type+":"+a.Text)
		} else {
			got = append(got, ev.Type)
		}
	}
	want := []string{"tool_output_delta:", "user_message_injected:s", "tool_started:", "future_kind:", "future_kind:", "tool_completed:", "assistant_delta:ab", "turn_finished:", "turn_ended"}
	if strings.Join(got, ",") != strings.Join(want, ",") {
		t.Fatalf("got %v\nwant %v", got, want)
	}
}

// A slow feedback answer within FeedbackTimeout succeeds although it
// outlasts the default budget; past its budget the outcome is unknown.
func TestFeedbackHasItsOwnBudget(t *testing.T) {
	f := newFakeService(t, func(req map[string]any, conn net.Conn) {
		if req["op"] == "status" {
			send(t, conn, statusReply(2, []string{CapFeedbackSubmit}))
			conn.Close()
			return
		}
		time.Sleep(150 * time.Millisecond)
		// The client may have given up; that write error is expected.
		b, _ := json.Marshal(map[string]any{"reply": "feedback", "status": "queued", "thread_id": "t", "input_id": 1, "annotations": 1, "stale": false})
		conn.Write(append(b, '\n'))
		conn.Close()
	})
	c := NewClient(f.socket)
	c.ReplyTimeout = 50 * time.Millisecond
	if _, err := c.Call(context.Background(), Status{}); err != nil {
		t.Fatal(err)
	}
	if _, err := c.SubmitFeedback(context.Background(), FeedbackSubmit{ThreadID: "t", Submission: `{}`}); err != nil {
		t.Fatalf("feedback must not use the default budget: %v", err)
	}
	if _, err := c.CallWithin(context.Background(), FeedbackSubmit{ThreadID: "t", Submission: `{}`}, 50*time.Millisecond); !errors.Is(err, ErrNoAnswer) {
		t.Fatalf("want ErrNoAnswer, got %v", err)
	}
}

// Replies shaped like the Rust serde output (newtype variants flatten into
// the tagged object), from crates/zdx-engine/src/orchestration/protocol.rs.
func TestChatOpsDecodeRustReplies(t *testing.T) {
	caps := []string{CapResolveThread, CapReplyParts, CapRecordDelivery}
	f := newFakeService(t, func(req map[string]any, conn net.Conn) {
		switch req["op"] {
		case "status":
			send(t, conn, statusReply(2, caps))
		case "resolve_thread":
			if req["chat_id"] != float64(-100) || req["topic_id"] != float64(5) {
				t.Errorf("unexpected request %v", req)
			}
			conn.Write([]byte(`{"reply":"thread_route","thread_id":"telegram--100-topic-5","effective_thread_id":"src","kind":"worker_mirror","exists":true,"root":"/r","pending_topic_title":true}` + "\n"))
		case "reply_parts":
			if req["final_text"] != "raw" {
				t.Errorf("unexpected request %v", req)
			}
			conn.Write([]byte(`{"reply":"reply_parts","text":"","followups":[],"media":[{"path":"/a.png","artifact_key":"0123456789abcdef","kind":"image","send_as":"photo","lexical_is_physical":true,"sendable":false}]}` + "\n"))
		case "record_delivery":
			conn.Write([]byte(`{"reply":"delivery_recorded","user_summary":"⚠️ Part of this reply was not delivered:"}` + "\n"))
		}
		conn.Close()
	})
	c := NewClient(f.socket)
	ctx := context.Background()
	topic := int64(5)
	route, err := c.ResolveThread(ctx, -100, &topic)
	if err != nil || route.EffectiveThreadID != "src" || route.Kind != KindWorkerMirror || !route.Exists || *route.Root != "/r" || !route.PendingTopicTitle {
		t.Fatalf("route=%+v err=%v", route, err)
	}
	parts, err := c.ReplyParts(ctx, "raw")
	if err != nil || parts.Text != "" || len(parts.Media) != 1 || parts.Media[0].SendAs != SendAsPhoto || parts.Media[0].Sendable {
		t.Fatalf("parts=%+v err=%v", parts, err)
	}
	summary, err := c.RecordDelivery(ctx, "src", nil, NewDeliveryRecord(-100, &topic, TextItems(1)))
	if err != nil || summary != "⚠️ Part of this reply was not delivered:" {
		t.Fatalf("summary=%q err=%v", summary, err)
	}
	line, _ := EncodeRequest(ComposeUserText{Text: ptrTo("hi"), Images: []ComposeImage{{Path: "/i.png", Delivery: ImageDelivery{Type: "inline"}}}})
	if string(line) != `{"op":"compose_user_text","text":"hi","images":[{"path":"/i.png","delivery":{"type":"inline"}}]}` {
		t.Fatalf("unexpected compose request %s", line)
	}
}

func ptrTo[T any](v T) *T { return &v }

// Found against the real service: an unset delivery or follow must encode
// as Rust's default, not as "".
func TestSubmitDefaultsEncodeAsRustDefaults(t *testing.T) {
	line, _ := EncodeRequest(Submit{Root: "/r", Text: "x"})
	var got map[string]any
	json.Unmarshal(line, &got)
	if got["delivery"] != "queue" || got["follow"] != "none" {
		t.Fatalf("unexpected %s", line)
	}
}

func TestSavedOnlyImageAlwaysCarriesItsReason(t *testing.T) {
	b, _ := json.Marshal(ImageDelivery{Type: "saved_only"})
	if string(b) != `{"type":"saved_only","reason":"","ask_media":false}` {
		t.Fatalf("got %s", b)
	}
	b, _ = json.Marshal(ImageDelivery{Type: "inline", Downscaled: true})
	if string(b) != `{"type":"inline","downscaled":true}` {
		t.Fatalf("got %s", b)
	}
}

func TestUpdateThreadWritesOnlySetFields(t *testing.T) {
	root, alias, from, profile, model := "/w", "src", "parent", "orchestrator", "mode:fast"
	yes := true
	b, err := json.Marshal(UpdateThread{ThreadID: "t", Root: &root, AliasTo: &alias, HandoffFrom: &from, PersistentProfile: &profile, Model: &model, PendingTopicTitle: &yes})
	if err != nil {
		t.Fatal(err)
	}
	want := `{"alias_to":"src","handoff_from":"parent","model":"mode:fast","pending_topic_title":true,"persistent_profile":"orchestrator","root":"/w","thread_id":"t"}`
	if string(b) != want {
		t.Fatalf("got %s", b)
	}
	if b, _ := json.Marshal(UpdateThread{ThreadID: "t"}); string(b) != `{"thread_id":"t"}` {
		t.Fatalf("got %s", b)
	}
}

// Hold tokens (Rust holds v7.2): the wire shapes, tokens only in direct
// replies, replay without a token.
func TestHoldTokenWireShapes(t *testing.T) {
	line, _ := EncodeRequest(Release{ThreadID: "t", TurnID: 3, Holder: "h"})
	if string(line) != `{"op":"release","thread_id":"t","turn_id":3,"holder":"h"}` {
		t.Fatalf("release %s", line)
	}
	line, _ = EncodeRequest(Subscribe{Renderer: true, Replay: true})
	if string(line) != `{"op":"subscribe","renderer":true,"replay":true}` {
		t.Fatalf("subscribe %s", line)
	}
	turn := uint64(3)
	line, _ = EncodeRequest(RecordDelivery{ThreadID: "t", TurnID: &turn, Holder: "h", Record: DeliveryRecord{ID: "r", Surface: "telegram", Items: []DeliveryItem{}}})
	if !strings.HasPrefix(string(line), `{"op":"record_delivery","thread_id":"t","turn_id":3,"holder":"h","record":`) {
		t.Fatalf("record %s", line)
	}
	line, _ = EncodeRequest(RecordDelivery{ThreadID: "t", Record: DeliveryRecord{ID: "r", Surface: "telegram", Items: []DeliveryItem{}}})
	if strings.Contains(string(line), "turn_id") || strings.Contains(string(line), "holder") {
		t.Fatalf("an unheld record names no turn or holder: %s", line)
	}
	line, _ = EncodeRequest(Submit{Root: "/r", Replay: true})
	if !strings.Contains(string(line), `"replay":true`) {
		t.Fatalf("submit %s", line)
	}
	var ev Event
	if err := json.Unmarshal([]byte(`{"type":"turn_replay","thread_id":"t","turn_id":3,"route":{"chat":-100,"topic":5},"status":"failed","final_text":"","error":"boom"}`), &ev); err != nil ||
		ev.Type != EventTurnReplay || ev.Route == nil || ev.Status != "failed" || *ev.Error != "boom" {
		t.Fatalf("replay %+v %v", ev, err)
	}
	// ReplayStatus is completed, failed or interrupted.
	if err := json.Unmarshal([]byte(`{"type":"turn_replay","thread_id":"t","turn_id":4,"route":{"chat":-100,"topic":5},"status":"interrupted","final_text":"partial","error":null}`), &ev); err != nil ||
		ev.Status != "interrupted" || ev.Error != nil || *ev.FinalText != "partial" {
		t.Fatalf("interrupted replay %+v %v", ev, err)
	}
	msg := "boom"
	rt := ReplayTurn(3, "failed", "", &msg)
	a := <-rt.Events
	if _, open := <-rt.Events; open || a.Type != AgentTurnFinished || a.Status.Status != "failed" || a.Status.Message != "boom" {
		t.Fatalf("one terminal event: %+v", a)
	}
	if end := rt.Wait(); end.TurnID != 3 || !end.Held || !end.Ran || end.TouchedWorkers != nil {
		t.Fatalf("end %+v", end)
	}
}
