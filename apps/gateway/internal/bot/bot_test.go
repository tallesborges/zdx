package bot

import (
	"bufio"
	"context"
	"encoding/json"
	"errors"
	"io"
	"log/slog"
	"net"
	"os"
	"path/filepath"
	"sync"
	"sync/atomic"
	"testing"
	"time"

	"github.com/tallesborges/zdx/apps/gateway/internal/engine"
	"github.com/tallesborges/zdx/apps/gateway/internal/telegram"
)

var quiet = slog.New(slog.DiscardHandler)

func msg(t *testing.T, raw string) *telegram.Message {
	t.Helper()
	var m telegram.Message
	if err := json.Unmarshal([]byte(raw), &m); err != nil {
		t.Fatal(err)
	}
	return &m
}

func TestMessageGate(t *testing.T) {
	a := NewAllowlist([]int64{7}, []int64{-100})
	cases := []struct {
		name string
		raw  string
		want bool
	}{
		{"private allowed user", `{"message_id":1,"chat":{"id":7,"type":"private"},"from":{"id":7,"is_bot":false},"text":"hi"}`, true},
		{"private other user", `{"message_id":1,"chat":{"id":8,"type":"private"},"from":{"id":8,"is_bot":false},"text":"hi"}`, false},
		{"no sender", `{"message_id":1,"chat":{"id":7,"type":"private"},"text":"hi"}`, false},
		{"bot sender", `{"message_id":1,"chat":{"id":7,"type":"private"},"from":{"id":7,"is_bot":true},"text":"hi"}`, false},
		{"allowed group", `{"message_id":1,"chat":{"id":-100,"type":"supergroup"},"from":{"id":7,"is_bot":false},"text":"hi"}`, true},
		{"unbound group", `{"message_id":1,"chat":{"id":-200,"type":"supergroup"},"from":{"id":7,"is_bot":false},"text":"hi"}`, false},
		{"whereami in unbound group", `{"message_id":1,"chat":{"id":-200,"type":"group"},"from":{"id":7,"is_bot":false},"text":" /whereami@zdx_bot "}`, true},
		{"whereami from stranger", `{"message_id":1,"chat":{"id":-200,"type":"group"},"from":{"id":9,"is_bot":false},"text":"/whereami"}`, false},
		{"whereami with args", `{"message_id":1,"chat":{"id":-200,"type":"group"},"from":{"id":7,"is_bot":false},"text":"/whereami now"}`, false},
		{"channel", `{"message_id":1,"chat":{"id":-100,"type":"channel"},"from":{"id":7,"is_bot":false},"text":"hi"}`, false},
	}
	for _, c := range cases {
		if got := a.MessageAllowed(msg(t, c.raw), quiet); got != c.want {
			t.Errorf("%s: got %v", c.name, got)
		}
	}
}

func TestCallbackGate(t *testing.T) {
	a := NewAllowlist([]int64{7}, []int64{-100})
	cb := func(user, chat int64, kind string, withMessage bool) *telegram.CallbackQuery {
		q := &telegram.CallbackQuery{ID: "q", From: telegram.User{ID: user}}
		if withMessage {
			q.Message = &telegram.Message{Chat: telegram.Chat{ID: chat, Type: kind}}
		}
		return q
	}
	if !a.CallbackAllowed(cb(7, 7, "private", true)) || !a.CallbackAllowed(cb(7, -100, "supergroup", true)) {
		t.Fatal("allowed callbacks refused")
	}
	if a.CallbackAllowed(cb(8, 8, "private", true)) || a.CallbackAllowed(cb(7, -200, "supergroup", true)) || a.CallbackAllowed(cb(7, 7, "private", false)) {
		t.Fatal("denied callbacks allowed")
	}
}

type scriptedSource struct {
	mu      sync.Mutex
	offsets []*int64
	batches [][]telegram.Update
	errs    []error
	cancel  context.CancelFunc
}

func (s *scriptedSource) GetUpdates(ctx context.Context, offset *int64, _ time.Duration) ([]telegram.Update, error) {
	s.mu.Lock()
	defer s.mu.Unlock()
	if offset != nil {
		v := *offset
		offset = &v
	}
	s.offsets = append(s.offsets, offset)
	if len(s.errs) > 0 {
		err := s.errs[0]
		s.errs = s.errs[1:]
		if err != nil {
			return nil, err
		}
	}
	if len(s.batches) == 0 {
		s.cancel()
		return nil, ctx.Err()
	}
	b := s.batches[0]
	s.batches = s.batches[1:]
	return b, nil
}

type recorder struct {
	mu        sync.Mutex
	messages  []int64
	callbacks []string
	offsets   []int64
	poller    *Poller
}

func (r *recorder) HandleMessage(_ context.Context, m *telegram.Message, rec Receipt) {
	r.mu.Lock()
	defer r.mu.Unlock()
	r.messages = append(r.messages, m.ID)
	r.offsets = append(r.offsets, *r.poller.Offset())
	if rec.UpdateID+1 != *r.poller.Offset() {
		panic("offset must move before the update is handled")
	}
}

func (r *recorder) HandleCallback(_ context.Context, q *telegram.CallbackQuery) {
	r.mu.Lock()
	defer r.mu.Unlock()
	r.callbacks = append(r.callbacks, q.ID)
}

func TestPollerAdvancesOffsetInMemoryAndRetriesErrors(t *testing.T) {
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	src := &scriptedSource{
		cancel: cancel,
		errs:   []error{io.ErrUnexpectedEOF, nil, nil},
		batches: [][]telegram.Update{
			{{ID: 10, Message: &telegram.Message{ID: 1}}, {ID: 11, CallbackQuery: &telegram.CallbackQuery{ID: "c"}}},
			{{ID: 15, Message: &telegram.Message{ID: 2}}},
		},
	}
	rec := &recorder{}
	p := &Poller{Source: src, Handler: rec, Log: quiet, ErrorDelay: time.Millisecond}
	rec.poller = p
	if err := p.Run(ctx); err != nil {
		t.Fatal(err)
	}
	if len(rec.messages) != 2 || len(rec.callbacks) != 1 {
		t.Fatalf("messages=%v callbacks=%v", rec.messages, rec.callbacks)
	}
	got := []any{}
	for _, o := range src.offsets {
		if o == nil {
			got = append(got, nil)
		} else {
			got = append(got, *o)
		}
	}
	// error poll (nil), retry (nil), after first batch 12, after second 16.
	want := []any{nil, nil, int64(12), int64(16)}
	if len(got) != len(want) {
		t.Fatalf("offsets %v", got)
	}
	for i := range want {
		if got[i] != want[i] {
			t.Fatalf("offsets %v, want %v", got, want)
		}
	}
}

func TestTopicQueuesSerializePerTopicAndRunTopicsConcurrently(t *testing.T) {
	q := NewTopicQueues(context.Background())
	a, b := KeyOf(-1, nil), KeyOf(-1, ptr(int64(5)))
	release := make(chan struct{})
	var order []string
	var mu sync.Mutex
	add := func(s string) { mu.Lock(); order = append(order, s); mu.Unlock() }
	q.Enqueue(a, func(context.Context) { <-release; add("a1") })
	q.Enqueue(a, func(context.Context) { add("a2") })
	done := make(chan struct{})
	q.Enqueue(b, func(context.Context) { add("b1"); close(done) })
	select {
	case <-done:
	case <-time.After(time.Second):
		t.Fatal("another topic must not wait for a busy one")
	}
	close(release)
	q.Wait()
	if len(order) != 3 || order[0] != "b1" || order[1] != "a1" || order[2] != "a2" {
		t.Fatalf("order %v", order)
	}
	if q.Busy() {
		t.Fatal("queues must be idle")
	}
}

func TestTicketCancelOnlyBeforeStart(t *testing.T) {
	q := NewTopicQueues(context.Background())
	k := KeyOf(1, nil)
	started := make(chan struct{})
	release := make(chan struct{})
	first := q.Enqueue(k, func(context.Context) { close(started); <-release })
	var ran atomic.Bool
	second := q.Enqueue(k, func(context.Context) { ran.Store(true) })
	<-started
	if first.Cancel() {
		t.Fatal("a started job cannot be cancelled")
	}
	if q.Pending(k) != 1 || !second.Cancel() || second.Cancel() || q.Pending(k) != 0 {
		t.Fatal("waiting job must cancel once")
	}
	close(release)
	q.Wait()
	if ran.Load() {
		t.Fatal("cancelled job ran")
	}
}

type sink struct {
	mu  sync.Mutex
	got []Incoming
}

func (s *sink) Dispatch(_ context.Context, in Incoming) {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.got = append(s.got, in)
}

func (s *sink) HandleCallback(context.Context, *telegram.CallbackQuery) {}

type answers struct {
	mu   sync.Mutex
	text []string
	done chan struct{}
}

func (a *answers) AnswerCallbackQuery(_ context.Context, _ string, text *string) error {
	a.mu.Lock()
	defer a.mu.Unlock()
	a.text = append(a.text, *text)
	close(a.done)
	return nil
}

func TestRouterGroupsAlbumsAndGatesSenders(t *testing.T) {
	s := &sink{}
	r := &Router{Allow: NewAllowlist([]int64{7}, nil), Messages: s, Callbacks: s, Log: quiet, Debounce: 20 * time.Millisecond}
	member := func(id int64, from int64) *telegram.Message {
		return msg(t, `{"message_id":`+itoa(id)+`,"chat":{"id":7,"type":"private"},"from":{"id":`+itoa(from)+`,"is_bot":false},"media_group_id":"g"}`)
	}
	r.HandleMessage(context.Background(), member(1, 7), Receipt{})
	r.HandleMessage(context.Background(), member(2, 7), Receipt{})
	r.HandleMessage(context.Background(), member(3, 9), Receipt{}) // stranger: dropped
	r.HandleMessage(context.Background(), msg(t, `{"message_id":4,"chat":{"id":7,"type":"private"},"from":{"id":7,"is_bot":false},"text":"x"}`), Receipt{})
	r.WaitFlushes()
	s.mu.Lock()
	defer s.mu.Unlock()
	if len(s.got) != 2 || s.got[0].Message.ID != 4 || s.got[1].Message.ID != 1 || len(s.got[1].Grouped) != 1 || s.got[1].Grouped[0].ID != 2 {
		t.Fatalf("got %+v", s.got)
	}
}

func TestRouterAnswersDeniedCallbacks(t *testing.T) {
	a := &answers{done: make(chan struct{})}
	r := &Router{Allow: NewAllowlist([]int64{7}, nil), Answer: a, Messages: &sink{}, Callbacks: &sink{}, Log: quiet}
	r.HandleCallback(context.Background(), &telegram.CallbackQuery{ID: "q", From: telegram.User{ID: 9}})
	<-a.done
	if a.text[0] != "Access denied" {
		t.Fatalf("got %v", a.text)
	}
}

func TestMediaGroupLateMemberOpensNewGroup(t *testing.T) {
	var g MediaGroups
	now := time.Unix(0, 0)
	m := &telegram.Message{ID: 1, From: &telegram.User{ID: 7}, MediaGroupID: ptr("g")}
	if res, _, _ := g.Offer(Incoming{Message: m}, now); res != Opened {
		t.Fatal("first member opens")
	}
	key, _ := mediaGroupKey(m)
	if _, ok := g.Flush(key, now.Add(MediaGroupDebounce)); !ok {
		t.Fatal("flush")
	}
	res, _, late := g.Offer(Incoming{Message: m}, now.Add(MediaGroupDebounce+time.Second))
	if res != Opened || late == nil || *late != time.Second {
		t.Fatalf("late member: res=%v late=%v", res, late)
	}
}

// --- bridge against a fake engine service ---

type renderLog struct {
	mu       sync.Mutex
	rendered []RoutedTurn
	finals   []string
	done     chan struct{}
}

func (r *renderLog) RenderTurn(_ context.Context, t RoutedTurn) {
	var final string
	for ev := range t.Turn.Events {
		if ev.Type == engine.AgentTurnFinished {
			final = ev.FinalText + "/" + ev.Status.Status
		}
	}
	r.mu.Lock()
	r.rendered = append(r.rendered, t)
	r.finals = append(r.finals, final)
	r.mu.Unlock()
	r.done <- struct{}{}
}

func listen(t *testing.T) (string, net.Listener) {
	dir, err := os.MkdirTemp("", "gwb")
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { os.RemoveAll(dir) })
	sock := filepath.Join(dir, "o.sock")
	ln, err := net.Listen("unix", sock)
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { ln.Close() })
	return sock, ln
}

func readReq(conn net.Conn) map[string]any {
	line, err := bufio.NewReader(conn).ReadBytes('\n')
	if err != nil {
		return nil
	}
	var m map[string]any
	json.Unmarshal(line, &m)
	return m
}

func write(conn net.Conn, v any) {
	b, _ := json.Marshal(v)
	conn.Write(append(b, '\n'))
}

func TestBridgeRendersUnfollowedRoutedTurnsAndReleasesHolds(t *testing.T) {
	sock, ln := listen(t)
	releases := make(chan map[string]any, 4)
	subscribed := make(chan map[string]any, 4)
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
					// A service before hold tokens.
					write(conn, map[string]any{"reply": "status", "report": map[string]any{"protocol": 2, "capabilities": []string{engine.CapRecordDelivery}, "settled": true, "threads": []any{}}})
					conn.Close()
				case "subscribe":
					subscribed <- req
					ev := func(e map[string]any) { write(conn, map[string]any{"reply": "event", "event": e}) }
					route := map[string]any{"chat": -100, "topic": 5}
					// Followed turn: rendered by its submitter, not here.
					ev(map[string]any{"type": "turn_started", "thread_id": "f", "turn_id": 1, "kind": "user", "inputs": []any{}, "route": route, "followed": true})
					// Unrouted turn: not rendered.
					ev(map[string]any{"type": "turn_started", "thread_id": "x", "turn_id": 2, "kind": "user", "inputs": []any{}, "route": nil, "followed": false})
					// Worker update turn: rendered and released.
					ev(map[string]any{"type": "turn_started", "thread_id": "o", "turn_id": 3, "kind": "worker_update", "inputs": []any{}, "route": route, "followed": false})
					ev(map[string]any{"type": "agent", "thread_id": "o", "turn_id": 3, "event": map[string]any{"type": "turn_finished", "status": map[string]any{"status": "completed"}, "final_text": "done", "messages": []any{}}})
					ev(map[string]any{"type": "turn_ended", "thread_id": "o", "turn_id": 3, "held": true})
					// A turn cut off by a service restart.
					ev(map[string]any{"type": "turn_started", "thread_id": "o2", "turn_id": 4, "kind": "user", "inputs": []any{}, "route": route, "followed": false})
					time.Sleep(20 * time.Millisecond)
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
	r := &renderLog{done: make(chan struct{}, 4)}
	var observed atomic.Int32
	ctx, cancel := context.WithCancel(context.Background())
	// Replay is wanted, but this service has no hold tokens: the field is
	// not sent and no token is read or released with.
	b := &Bridge{Engine: engine.NewClient(sock), Renderer: r, Log: quiet, Delay: time.Hour, Replay: true, Observe: func(engine.Event) { observed.Add(1) }}
	finished := make(chan struct{})
	go func() { b.Run(ctx); close(finished) }()
	<-r.done
	<-r.done
	rel := <-releases
	if _, token := rel["holder"]; rel["thread_id"] != "o" || rel["turn_id"] != float64(3) || token {
		t.Fatalf("release %v", rel)
	}
	if req := <-subscribed; req["renderer"] != true {
		t.Fatalf("subscribe must claim the renderer role: %v", req)
	} else if _, replay := req["replay"]; replay {
		t.Fatalf("replay is sent only to a service with hold tokens: %v", req)
	}
	cancel()
	<-finished
	r.mu.Lock()
	defer r.mu.Unlock()
	if len(r.rendered) != 2 {
		t.Fatalf("rendered %d turns", len(r.rendered))
	}
	byThread := map[string]string{}
	for i, rt := range r.rendered {
		byThread[rt.ThreadID] = r.finals[i]
	}
	if byThread["o"] != "done/completed" || byThread["o2"] != "/failed" {
		t.Fatalf("finals %v", byThread)
	}
	select {
	case extra := <-releases:
		t.Fatalf("only held turns are released, got %v", extra)
	default:
	}
	if observed.Load() != 6 {
		t.Fatalf("observer saw %d events", observed.Load())
	}
}

func ptr[T any](v T) *T { return &v }

func itoa(v int64) string {
	b, _ := json.Marshal(v)
	return string(b)
}

func TestFeedbackGate(t *testing.T) {
	a := NewAllowlist([]int64{7}, []int64{-100})
	if !a.FeedbackAllowed(7, -100) || !a.FeedbackAllowed(7, 7) {
		t.Fatal("allowed feedback refused")
	}
	if a.FeedbackAllowed(7, -200) || a.FeedbackAllowed(7, 8) || a.FeedbackAllowed(9, -100) {
		t.Fatal("denied feedback allowed")
	}
}

func TestPollerStopsOnConflictAndConfirmsOnStop(t *testing.T) {
	src := &scriptedSource{errs: []error{&telegram.APIError{Method: "getUpdates", Code: 409, Description: "Conflict: terminated by other getUpdates request"}}}
	p := &Poller{Source: src, Handler: &recorder{}, Log: quiet, ErrorDelay: time.Millisecond}
	if err := p.Run(context.Background()); !errors.Is(err, ErrPollConflict) {
		t.Fatalf("a 409 stops the poller: %v", err)
	}
	if err := p.Confirm(context.Background()); err != nil || len(src.offsets) != 1 {
		t.Fatalf("nothing to confirm before any update: %v %d", err, len(src.offsets))
	}

	ctx, cancel := context.WithCancel(context.Background())
	src = &scriptedSource{cancel: cancel, batches: [][]telegram.Update{{{ID: 20, Message: &telegram.Message{ID: 1}}}}}
	rec := &recorder{}
	p = &Poller{Source: src, Handler: rec, Log: quiet}
	rec.poller = p
	if err := p.Run(ctx); err != nil {
		t.Fatal(err)
	}
	src.batches = [][]telegram.Update{{{ID: 21, Message: &telegram.Message{ID: 2}}}}
	if err := p.Confirm(context.Background()); err != nil {
		t.Fatal(err)
	}
	last := src.offsets[len(src.offsets)-1]
	if last == nil || *last != 21 || len(rec.messages) != 1 {
		t.Fatalf("confirm sends offset 21 and hands nothing on: %v %v", last, rec.messages)
	}
}
