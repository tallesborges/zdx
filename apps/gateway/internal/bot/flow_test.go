package bot

import (
	"bufio"
	"context"
	"encoding/json"
	"fmt"
	"io"
	"net"
	"net/http"
	"net/http/httptest"
	"strings"
	"sync"
	"testing"
	"time"

	"github.com/tallesborges/zdx/apps/gateway/internal/engine"
	"github.com/tallesborges/zdx/apps/gateway/internal/telegram"
)

// --- fake Bot API ---

type tgCall struct {
	method string
	body   map[string]any
	// id is the message_id a send answered with.
	id int64
}

type fakeTG struct {
	srv     *httptest.Server
	mu      sync.Mutex
	calls   []tgCall
	nextID  int64
	replies map[string][]string
	seen    chan string
	// lastSent is the id of the newest sendMessage answer.
	lastSent int64
	// files are the downloadable files by getFile path ("docs/<file_id>");
	// a path in failing answers 500.
	files   map[string]string
	failing map[string]bool
}

func newFakeTG(t *testing.T) *fakeTG {
	f := &fakeTG{nextID: 100, replies: map[string][]string{}, seen: make(chan string, 256)}
	f.srv = httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if _, path, ok := strings.Cut(r.URL.Path, "/file/bot1:SECRET/"); ok {
			f.mu.Lock()
			f.calls = append(f.calls, tgCall{method: "file:" + path})
			data, found := f.files[path]
			fail := f.failing[path]
			f.mu.Unlock()
			switch {
			case fail:
				http.Error(w, "down", http.StatusInternalServerError)
			case !found:
				http.NotFound(w, r)
			default:
				io.WriteString(w, data)
			}
			return
		}
		method := r.URL.Path[strings.LastIndex(r.URL.Path, "/")+1:]
		body := map[string]any{}
		if strings.HasPrefix(r.Header.Get("Content-Type"), "multipart/") {
			if err := r.ParseMultipartForm(64 << 20); err == nil {
				for k, v := range r.MultipartForm.Value {
					body[k] = v[0]
				}
				for k, fh := range r.MultipartForm.File {
					body[k] = fh[0].Filename + "|" + fh[0].Header.Get("Content-Type")
				}
			}
		} else {
			raw, _ := io.ReadAll(r.Body)
			_ = json.Unmarshal(raw, &body)
		}
		f.mu.Lock()
		var reply string
		if q := f.replies[method]; len(q) > 0 {
			reply, f.replies[method] = q[0], q[1:]
		}
		f.nextID++
		id := f.nextID
		f.calls = append(f.calls, tgCall{method, body, id})
		if method == "sendMessage" && reply == "" {
			f.lastSent = id
		}
		f.mu.Unlock()
		f.seen <- method
		if reply != "" {
			io.WriteString(w, reply)
			return
		}
		switch method {
		case "getFile":
			fmt.Fprintf(w, `{"ok":true,"result":{"file_id":%q,"file_path":"docs/%s"}}`, body["file_id"], body["file_id"])
		case "sendMessage", "sendDocument", "sendPhoto", "sendVoice", "sendAudio", "sendVideo":
			fmt.Fprintf(w, `{"ok":true,"result":{"message_id":%d,"chat":{"id":1,"type":"private"},"date":1}}`, id)
		default:
			io.WriteString(w, `{"ok":true,"result":true}`)
		}
	}))
	t.Cleanup(f.srv.Close)
	return f
}

func (f *fakeTG) client() *telegram.Client {
	return telegram.NewClient("1:SECRET", telegram.Options{BaseURL: f.srv.URL, Logger: quiet})
}

func (f *fakeTG) queue(method, reply string) {
	f.mu.Lock()
	defer f.mu.Unlock()
	f.replies[method] = append(f.replies[method], reply)
}

func (f *fakeTG) of(method string) []tgCall {
	f.mu.Lock()
	defer f.mu.Unlock()
	var out []tgCall
	for _, c := range f.calls {
		if c.method == method {
			out = append(out, c)
		}
	}
	return out
}

// waitFor waits until pred holds over the recorded calls.
func (f *fakeTG) waitFor(t *testing.T, what string, pred func() bool) {
	t.Helper()
	f.waitWithin(t, 5*time.Second, what, pred)
}

func (f *fakeTG) waitWithin(t *testing.T, limit time.Duration, what string, pred func() bool) {
	t.Helper()
	deadline := time.After(limit)
	for !pred() {
		select {
		case <-f.seen:
		case <-time.After(20 * time.Millisecond):
		case <-deadline:
			t.Fatalf("timed out waiting for %s; calls: %+v", what, f.calls)
		}
	}
}

// --- fake engine service ---

type svcConn struct {
	req  map[string]any
	conn net.Conn
}

type fakeSvc struct {
	t      *testing.T
	socket string
	caps   []string
	// recordErr answers record_delivery with an error.
	recordErr bool
	mu        sync.Mutex
	log       []string // ops in arrival order, with a few details
	reqs      []map[string]any
	submit    func(sc svcConn) // handles submit; nil answers an error
	// view answers view ops; nil answers an error.
	view func(req map[string]any) map[string]any
	// worker answers the worker op; nil answers an error.
	worker func(req map[string]any) map[string]any
	cancel chan map[string]any
}

func newFakeSvc(t *testing.T, caps []string) *fakeSvc {
	sock, ln := listen(t)
	f := &fakeSvc{t: t, socket: sock, caps: caps, cancel: make(chan map[string]any, 4)}
	go func() {
		for {
			conn, err := ln.Accept()
			if err != nil {
				return
			}
			go f.serve(conn)
		}
	}()
	return f
}

func (f *fakeSvc) setCaps(caps ...string) {
	f.mu.Lock()
	defer f.mu.Unlock()
	f.caps = caps
}

func (f *fakeSvc) note(entry string, req map[string]any) {
	f.mu.Lock()
	defer f.mu.Unlock()
	f.log = append(f.log, entry)
	f.reqs = append(f.reqs, req)
}

func (f *fakeSvc) ops() []string {
	f.mu.Lock()
	defer f.mu.Unlock()
	return append([]string(nil), f.log...)
}

func (f *fakeSvc) requests(op string) []map[string]any {
	f.mu.Lock()
	defer f.mu.Unlock()
	var out []map[string]any
	for _, r := range f.reqs {
		if r["op"] == op {
			out = append(out, r)
		}
	}
	return out
}

func (f *fakeSvc) serve(conn net.Conn) {
	line, err := bufio.NewReader(conn).ReadBytes('\n')
	if err != nil {
		conn.Close()
		return
	}
	var req map[string]any
	json.Unmarshal(line, &req)
	op, _ := req["op"].(string)
	switch op {
	case "status":
		f.mu.Lock()
		caps := append([]string{}, f.caps...)
		f.mu.Unlock()
		write(conn, map[string]any{"reply": "status", "report": map[string]any{"protocol": 2, "capabilities": caps, "settled": true, "threads": []any{}}})
		conn.Close()
		return
	case "compose_user_text":
		f.note(op, req)
		text, _ := req["text"].(string)
		write(conn, map[string]any{"reply": "text", "text": "composed:" + text})
	case "record_delivery":
		rec, _ := req["record"].(map[string]any)
		items, _ := rec["items"].([]any)
		first, _ := items[0].(map[string]any)
		f.note(fmt.Sprintf("record_delivery:%v:%v", rec["id"], first["status"]), req)
		f.mu.Lock()
		fail := f.recordErr
		f.mu.Unlock()
		if fail {
			write(conn, map[string]any{"reply": "error", "message": "disk full", "code": "internal"})
		} else {
			write(conn, map[string]any{"reply": "delivery_recorded"})
		}
	case "release":
		f.note(fmt.Sprintf("release:%v:%v", req["thread_id"], req["turn_id"]), req)
		write(conn, map[string]any{"reply": "ok"})
	case "cancel":
		f.note(op, req)
		write(conn, map[string]any{"reply": "cancelled", "thread_id": req["thread_id"], "cancelled_turn": false, "dropped_inputs": []any{}})
		conn.Close()
		f.cancel <- req
		return
	case "reset_thread":
		f.note(op, req)
		if req["thread_id"] == "busy" {
			write(conn, map[string]any{"reply": "error", "code": "conflict", "message": "thread 'busy' has work queued or running; cancel it first"})
		} else {
			write(conn, map[string]any{"reply": "ok"})
		}
	case "update_thread":
		f.note(op, req)
		switch m := req["model"]; {
		case m == "bogus":
			write(conn, map[string]any{"reply": "error", "code": "invalid", "message": "unknown model <bogus>"})
		case m == nil:
			write(conn, map[string]any{"reply": "thread_updated", "thread_id": req["thread_id"], "model_label": "default-model"})
		default:
			write(conn, map[string]any{"reply": "thread_updated", "thread_id": req["thread_id"], "model_label": "label:" + m.(string)})
		}
	case "worker":
		f.note(fmt.Sprintf("worker:%v", req["thread_id"]), req)
		if f.worker != nil {
			write(conn, f.worker(req))
		} else {
			write(conn, map[string]any{"reply": "error", "message": "no worker"})
		}
	case "set_chat_model":
		f.note(op, req)
		write(conn, map[string]any{"reply": "chat_model", "selection": req["model"]})
	case "enqueue_from_topic":
		f.note(op, req)
		write(conn, map[string]any{"reply": "worker", "worker": map[string]any{"thread_id": "w", "owner_thread_id": "o", "root": "/r", "status": "running", "queue_depth": 2, "queue": []any{}}})
	case "submit":
		f.note(op, req)
		if f.submit != nil {
			f.submit(svcConn{req, conn})
			return
		}
		write(conn, map[string]any{"reply": "error", "message": "no submit"})
	default:
		f.note(op, req)
		if f.view != nil && engine.ViewCapability(op) != "" {
			write(conn, f.view(req))
			break
		}
		write(conn, map[string]any{"reply": "error", "message": "invalid request"})
	}
	conn.Close()
}

func event(e map[string]any) map[string]any { return map[string]any{"reply": "event", "event": e} }

func agent(thread string, turn uint64, inner map[string]any) map[string]any {
	return event(map[string]any{"type": "agent", "thread_id": thread, "turn_id": turn, "event": inner})
}

// --- helpers ---

type staticResolver TopicInfo

func (r staticResolver) ResolveTopic(context.Context, int64, *int64) (TopicInfo, error) {
	return TopicInfo(r), nil
}

type tracker struct {
	mu      sync.Mutex
	running map[string]int
	ended   int
}

func (t *tracker) Start(id string) func() {
	t.mu.Lock()
	if t.running == nil {
		t.running = map[string]int{}
	}
	t.running[id]++
	t.mu.Unlock()
	return func() {
		t.mu.Lock()
		t.running[id]--
		t.ended++
		t.mu.Unlock()
	}
}

func newHandler(tg *fakeTG, svc *fakeSvc, info TopicInfo) (*Handler, *tracker) {
	eng := engine.NewClient(svc.socket)
	client := tg.client()
	tr := &tracker{}
	pres := &Presenter{
		Telegram: client, Engine: eng, Retries: &Retries{}, Planner: TextPlanner{}, Cancels: &Cancels{}, Log: quiet, Interval: 10 * time.Millisecond,
		Deliverer: &Deliverer{Telegram: client, Store: eng, Tracker: tr, Followups: &Followups{}, Log: quiet},
	}
	return &Handler{
		Telegram: client, Engine: eng, Resolver: staticResolver(info), Composer: eng, Turns: eng,
		Queues: NewTopicQueues(context.Background()), Presenter: pres, Acks: &SteerAcks{}, Artifacts: UnavailableArtifacts{},
		RootForChat: func(int64) string { return "/project" }, Log: quiet,
		TurnReady: func(ctx context.Context) error { return eng.RequireAll(ctx, engine.CapRecordDelivery) },
		ThreadOps: eng,
	}, tr
}

func userMessage(t *testing.T, id int64, text string) Incoming {
	return Incoming{Message: msg(t, fmt.Sprintf(`{"message_id":%d,"chat":{"id":-100,"type":"supergroup","is_forum":true},"message_thread_id":5,"from":{"id":7,"is_bot":false},"text":%q}`, id, text))}
}

const thread = "telegram--100-topic-5"

// The core flow: plain text -> ordinary Rust turn -> persisted delivery ->
// reply, with the hold released only after the outcome is recorded.
func TestPlainTextRunsAnOrdinaryTurnAndRecordsDelivery(t *testing.T) {
	tg := newFakeTG(t)
	svc := newFakeSvc(t, []string{engine.CapOrdinarySubmit, engine.CapComposeUserText, engine.CapRecordDelivery})
	svc.submit = func(sc svcConn) {
		c := sc.conn
		write(c, map[string]any{"reply": "submitted", "thread_id": thread, "input_id": 1, "queued_ahead": 0, "in_turn": false})
		write(c, event(map[string]any{"type": "turn_started", "thread_id": thread, "turn_id": 9, "kind": "user", "inputs": []any{map[string]any{"input_id": 1}}, "route": map[string]any{"chat": -100, "topic": 5}, "followed": true}))
		write(c, agent(thread, 9, map[string]any{"type": "tool_started", "id": "a", "name": "bash"}))
		time.Sleep(30 * time.Millisecond)
		write(c, agent(thread, 9, map[string]any{"type": "turn_finished", "status": map[string]any{"status": "completed"}, "final_text": "hello <world>", "messages": []any{}}))
		write(c, event(map[string]any{"type": "turn_ended", "thread_id": thread, "turn_id": 9, "held": true}))
		// The service closes the follower only after the release.
		deadline := time.Now().Add(5 * time.Second)
		for time.Now().Before(deadline) {
			if len(svc.requests("release")) > 0 {
				break
			}
			time.Sleep(5 * time.Millisecond)
		}
		c.Close()
	}
	h, tr := newHandler(tg, svc, TopicInfo{ThreadID: thread, Kind: TopicOrdinary})
	h.Dispatch(context.Background(), userMessage(t, 42, "  hi there  "))

	tg.waitFor(t, "the reply", func() bool { return len(svc.requests("release")) > 0 })

	sub := svc.requests("submit")[0]
	if sub["ordinary"] != true || sub["delivery"] != "steer" || sub["follow"] != "turn" || sub["thread_id"] != thread || sub["root"] != "/project" || sub["text"] != "composed:hi there" {
		t.Fatalf("unexpected submit %v", sub)
	}
	if r, _ := sub["route"].(map[string]any); r["chat"] != float64(-100) || r["topic"] != float64(5) {
		t.Fatalf("unexpected route %v", sub["route"])
	}
	if _, ok := sub["profile"]; ok {
		t.Fatal("an ordinary submit names no profile")
	}

	ops := svc.ops()
	var deliveries []string
	releaseAt, finalAt := -1, -1
	for i, op := range ops {
		if strings.HasPrefix(op, "record_delivery:") {
			deliveries = append(deliveries, op)
			if strings.HasSuffix(op, ":sent") {
				finalAt = i
			}
		}
		if strings.HasPrefix(op, "release:") {
			releaseAt = i
			if op != "release:"+thread+":9" {
				t.Fatalf("unexpected release %s", op)
			}
		}
	}
	if len(deliveries) != 2 || !strings.HasSuffix(deliveries[0], ":pending") || finalAt < 0 {
		t.Fatalf("want pending then sent records, got %v", deliveries)
	}
	if strings.Split(deliveries[0], ":")[1] != strings.Split(deliveries[1], ":")[1] {
		t.Fatalf("pending and final records must share an id: %v", deliveries)
	}
	if releaseAt < finalAt {
		t.Fatalf("release must follow the final record: %v", ops)
	}
	final := svc.requests("record_delivery")[1]["record"].(map[string]any)
	item := final["items"].([]any)[0].(map[string]any)
	if final["surface"] != "telegram" || final["chat_id"] != float64(-100) || final["topic_id"] != float64(5) || item["kind"] != "text" || item["message_id"] == nil {
		t.Fatalf("unexpected final record %v", final)
	}

	sends := tg.of("sendMessage")
	if len(sends) != 2 {
		t.Fatalf("want status and reply, got %+v", sends)
	}
	status, reply := sends[0].body, sends[1].body
	if status["text"] != StatusWaiting || status["reply_to_message_id"] != float64(42) || status["message_thread_id"] != float64(5) {
		t.Fatalf("unexpected status %v", status)
	}
	kb := status["reply_markup"].(map[string]any)["inline_keyboard"].([]any)[0].([]any)[0].(map[string]any)
	if kb["callback_data"] != "cancel:-100:42" {
		t.Fatalf("unexpected cancel button %v", kb)
	}
	if reply["text"] != "hello &lt;world&gt;" || reply["reply_to_message_id"] != float64(42) || reply["link_preview_options"] == nil {
		t.Fatalf("unexpected reply %v", reply)
	}
	edits := tg.of("editMessageText")
	if len(edits) == 0 || edits[0].body["text"] != ToolRunningStatus("bash") {
		t.Fatalf("the status must show the running tool: %+v", edits)
	}
	if len(tg.of("deleteMessage")) != 1 {
		t.Fatal("the status message must be deleted before the reply")
	}
	tr.mu.Lock()
	defer tr.mu.Unlock()
	if tr.ended != 1 || tr.running[thread] != 0 {
		t.Fatalf("delivery tracking must start and end once: %+v", tr)
	}
}

func TestCancelButtonCancelsTheSubmittedInput(t *testing.T) {
	tg := newFakeTG(t)
	svc := newFakeSvc(t, []string{engine.CapOrdinarySubmit, engine.CapComposeUserText, engine.CapRecordDelivery})
	svc.submit = func(sc svcConn) {
		write(sc.conn, map[string]any{"reply": "submitted", "thread_id": thread, "input_id": 3, "queued_ahead": 1})
		<-svc.cancel
		write(sc.conn, event(map[string]any{"type": "inputs_dropped", "thread_id": thread, "input_ids": []any{3}}))
		sc.conn.Close()
	}
	h, _ := newHandler(tg, svc, TopicInfo{ThreadID: thread, Kind: TopicOrdinary})
	h.Dispatch(context.Background(), userMessage(t, 42, "hi"))
	tg.waitFor(t, "the status", func() bool { return len(tg.of("sendMessage")) == 1 })

	data := "cancel:-100:42"
	h.HandleCallback(context.Background(), &telegram.CallbackQuery{ID: "q", From: telegram.User{ID: 7}, Data: &data})
	tg.waitFor(t, "Cancelled", func() bool {
		for _, e := range tg.of("editMessageText") {
			if e.body["text"] == StatusCancelled {
				return true
			}
		}
		return false
	})
	c := svc.requests("cancel")[0]
	if c["input_id"] != float64(3) || c["thread_id"] != thread {
		t.Fatalf("unexpected cancel %v", c)
	}
	tg.waitFor(t, "answer", func() bool { return len(tg.of("answerCallbackQuery")) == 1 })
	if answers := tg.of("answerCallbackQuery"); len(answers) != 1 || answers[0].body["text"] != "Cancelling..." {
		t.Fatalf("answers %+v", answers)
	}
	if len(svc.requests("record_delivery")) != 0 || len(svc.requests("release")) != 0 {
		t.Fatal("a dropped input delivers and releases nothing")
	}
	h.HandleCallback(context.Background(), &telegram.CallbackQuery{ID: "q2", From: telegram.User{ID: 7}, Data: &data})
	tg.waitFor(t, "second answer", func() bool { return len(tg.of("answerCallbackQuery")) == 2 })
	if tg.of("answerCallbackQuery")[1].body["text"] != "Nothing to cancel" {
		t.Fatal("a finished turn has nothing to cancel")
	}
}

func TestSteeredMessageIsAcknowledgedAndUpdated(t *testing.T) {
	tg := newFakeTG(t)
	svc := newFakeSvc(t, []string{engine.CapOrdinarySubmit, engine.CapComposeUserText, engine.CapRecordDelivery})
	svc.submit = func(sc svcConn) {
		write(sc.conn, map[string]any{"reply": "submitted", "thread_id": thread, "input_id": 8, "queued_ahead": 0, "in_turn": true})
		sc.conn.Close()
	}
	h, _ := newHandler(tg, svc, TopicInfo{ThreadID: thread, Kind: TopicOrdinary})
	observe := SteerObserver(context.Background(), h.Acks, h, quiet)
	// The injection can arrive before the acknowledgement is posted.
	observe(engine.Event{Type: engine.EventAgent, ThreadID: thread, Inner: json.RawMessage(`{"type":"user_message_injected","id":"8","source":"user","text":"x"}`)})
	h.Dispatch(context.Background(), userMessage(t, 43, "also this"))
	tg.waitFor(t, "ack edit", func() bool { return len(tg.of("editMessageText")) == 1 })
	if s := tg.of("sendMessage"); len(s) != 1 || s[0].body["text"] != AckPending {
		t.Fatalf("want one pending ack, got %+v", s)
	}
	if e := tg.of("editMessageText")[0].body; e["text"] != AckAdded {
		t.Fatalf("unexpected edit %v", e)
	}
}

func TestSteerAckOutcomes(t *testing.T) {
	var acks SteerAcks
	acks.Expect("1")
	if _, ok := acks.Settle("1", AckNotAdd); ok {
		t.Fatal("no ack posted yet")
	}
	if out, ok := acks.Record("1", Ack{Chat: 1, MessageID: 2}); !ok || out != AckNotAdd {
		t.Fatal("an early outcome applies when the ack is posted")
	}
	acks.Expect("2")
	acks.Record("2", Ack{Chat: 1, MessageID: 3})
	if ack, ok := acks.Settle("2", AckNewTurn); !ok || ack.MessageID != 3 {
		t.Fatal("a posted ack is edited")
	}
	if _, ok := acks.Settle("2", AckAdded); ok {
		t.Fatal("settled once")
	}
	if _, ok := acks.Settle("unknown", AckAdded); ok {
		t.Fatal("unknown ids are ignored")
	}
}

func TestMissingCapabilitySubmitsNothingAndTellsTheUser(t *testing.T) {
	tg := newFakeTG(t)
	svc := newFakeSvc(t, []string{engine.CapComposeUserText, engine.CapRecordDelivery})
	h, _ := newHandler(tg, svc, TopicInfo{ThreadID: thread, Kind: TopicOrdinary})
	h.Dispatch(context.Background(), userMessage(t, 42, "hi"))
	tg.waitFor(t, "notice", func() bool { return len(tg.of("sendMessage")) == 1 })
	if text := tg.of("sendMessage")[0].body["text"].(string); !strings.Contains(text, "ordinary_submit") {
		t.Fatalf("the notice must name the missing capability: %s", text)
	}
	if len(svc.requests("submit")) != 0 {
		t.Fatal("nothing may be submitted")
	}
}

func TestProfileTopicSubmitsWithoutOrdinary(t *testing.T) {
	tg := newFakeTG(t)
	svc := newFakeSvc(t, []string{engine.CapComposeUserText, engine.CapRecordDelivery})
	svc.submit = func(sc svcConn) {
		write(sc.conn, map[string]any{"reply": "submitted", "thread_id": thread, "input_id": 1, "queued_ahead": 0, "in_turn": true})
		sc.conn.Close()
	}
	h, _ := newHandler(tg, svc, TopicInfo{ThreadID: thread, Kind: TopicProfile})
	h.Dispatch(context.Background(), userMessage(t, 42, "hi"))
	tg.waitFor(t, "submit", func() bool { return len(svc.requests("submit")) == 1 })
	if _, ok := svc.requests("submit")[0]["ordinary"]; ok {
		t.Fatal("a profile submit must not set ordinary")
	}
}

func TestWorkerMirrorTopicQueuesToTheWorker(t *testing.T) {
	tg := newFakeTG(t)
	svc := newFakeSvc(t, []string{engine.CapComposeUserText})
	h, _ := newHandler(tg, svc, TopicInfo{Kind: TopicWorkerMirror, WorkerThreadID: "w"})
	h.Dispatch(context.Background(), userMessage(t, 42, "do it"))
	tg.waitFor(t, "notice", func() bool { return len(tg.of("sendMessage")) == 1 })
	q := svc.requests("enqueue_from_topic")
	if len(q) != 1 || q[0]["worker_thread_id"] != "w" || q[0]["prompt"] != "composed:do it" {
		t.Fatalf("unexpected enqueue %v", q)
	}
	if text := tg.of("sendMessage")[0].body["text"]; text != "📨 Queued to the worker (2 waiting)." {
		t.Fatalf("unexpected notice %v", text)
	}
	if len(svc.requests("submit")) != 0 {
		t.Fatal("mirror topics never submit turns")
	}
}

func TestUnsupportedMessagesAreAnsweredWithoutEngineCalls(t *testing.T) {
	tg := newFakeTG(t)
	svc := newFakeSvc(t, nil)
	h, _ := newHandler(tg, svc, TopicInfo{ThreadID: thread, Kind: TopicOrdinary})
	h.Dispatch(context.Background(), userMessage(t, 42, "/status"))
	tg.waitFor(t, "notice", func() bool { return len(tg.of("sendMessage")) == 1 })
	if text := tg.of("sendMessage")[0].body["text"].(string); !strings.Contains(text, "commands") {
		t.Fatalf("unexpected notice %s", text)
	}
	if len(svc.ops()) != 0 {
		t.Fatalf("no engine calls expected, got %v", svc.ops())
	}
}

func TestRepliedToSkipsTopicCreationAndCutsLongText(t *testing.T) {
	topicStarter := msg(t, `{"message_id":2,"chat":{"id":-100,"type":"supergroup"},"message_thread_id":5,"reply_to_message":{"message_id":5,"chat":{"id":-100,"type":"supergroup"},"text":"topic"}}`)
	topicStarter.ThreadID = ptr(int64(5))
	if repliedTo(topicStarter) != nil {
		t.Fatal("the topic creation message is not a reply")
	}
	long := strings.Repeat("é", MaxRepliedToChars+5)
	m := msg(t, `{"message_id":2,"chat":{"id":7,"type":"private"},"reply_to_message":{"message_id":1,"chat":{"id":7,"type":"private"},"from":{"id":9,"is_bot":true},"text":"`+long+`"}}`)
	r := repliedTo(m)
	if r == nil || !r.FromBot || r.Quoted || !strings.HasSuffix(r.Text, "\n[…cut]") || len([]rune(r.Text)) != MaxRepliedToChars+len([]rune("\n[…cut]")) {
		t.Fatalf("unexpected reply %+v", r)
	}
	q := msg(t, `{"message_id":2,"chat":{"id":7,"type":"private"},"quote":{"text":" part "},"reply_to_message":{"message_id":1,"chat":{"id":7,"type":"private"},"text":"whole"}}`)
	if r := repliedTo(q); r == nil || r.Text != "part" || !r.Quoted {
		t.Fatalf("the quote wins: %+v", r)
	}
}

func heldTurn(svc *fakeSvc, turnID int, final string, onSubmit func()) func(sc svcConn) {
	return func(sc svcConn) {
		c := sc.conn
		write(c, map[string]any{"reply": "submitted", "thread_id": thread, "input_id": 1, "queued_ahead": 0})
		if onSubmit != nil {
			onSubmit()
		}
		write(c, agent(thread, uint64(turnID), map[string]any{"type": "turn_finished", "status": map[string]any{"status": "completed"}, "final_text": final, "messages": []any{}}))
		write(c, event(map[string]any{"type": "turn_ended", "thread_id": thread, "turn_id": turnID, "held": true}))
		for range 500 {
			if len(svc.requests("release")) > 0 {
				break
			}
			time.Sleep(10 * time.Millisecond)
		}
		c.Close()
	}
}

// Without record_delivery no turn starts: nothing is submitted, and the
// user is told why.
func TestNoTurnStartsWithoutDeliveryRecording(t *testing.T) {
	tg := newFakeTG(t)
	svc := newFakeSvc(t, []string{engine.CapOrdinarySubmit, engine.CapComposeUserText})
	h, _ := newHandler(tg, svc, TopicInfo{ThreadID: thread, Kind: TopicOrdinary})
	h.Dispatch(context.Background(), userMessage(t, 42, "hi"))
	tg.waitFor(t, "notice", func() bool { return len(tg.of("sendMessage")) == 1 })
	if text := tg.of("sendMessage")[0].body["text"].(string); !strings.Contains(text, engine.CapRecordDelivery) {
		t.Fatalf("the notice must name the missing capability: %s", text)
	}
	if len(svc.requests("submit")) != 0 || len(svc.requests("compose_user_text")) != 0 {
		t.Fatalf("nothing may be submitted: %v", svc.ops())
	}
}

// A runtime recording failure keeps Rust's behavior: logged, the reply
// still goes out, and the hold is released after it.
func TestRecordingFailureStillRepliesAndReleases(t *testing.T) {
	tg := newFakeTG(t)
	svc := newFakeSvc(t, []string{engine.CapOrdinarySubmit, engine.CapComposeUserText, engine.CapRecordDelivery})
	svc.recordErr = true
	svc.submit = heldTurn(svc, 4, "ok", nil)
	h, _ := newHandler(tg, svc, TopicInfo{ThreadID: thread, Kind: TopicOrdinary})
	h.Dispatch(context.Background(), userMessage(t, 42, "hi"))
	tg.waitFor(t, "release", func() bool { return len(svc.requests("release")) == 1 })
	h.Wait()
	if n := len(svc.requests("record_delivery")); n != 2 {
		t.Fatalf("pending and final were attempted: %d", n)
	}
	if sends := tg.of("sendMessage"); len(sends) != 2 || sends[1].body["text"] != "ok" {
		t.Fatalf("the reply must go out: %+v", sends)
	}
}

// The capability goes away after the turn started (the service restarted
// onto an older build): the reply is not sent unrecorded, the status shows
// why, and the hold is still released.
func TestCapabilityLostMidTurnSendsNoReplyAndReleases(t *testing.T) {
	tg := newFakeTG(t)
	svc := newFakeSvc(t, []string{engine.CapOrdinarySubmit, engine.CapComposeUserText, engine.CapRecordDelivery})
	svc.submit = heldTurn(svc, 6, "secret answer", func() { svc.setCaps(engine.CapOrdinarySubmit) })
	h, _ := newHandler(tg, svc, TopicInfo{ThreadID: thread, Kind: TopicOrdinary})
	h.Dispatch(context.Background(), userMessage(t, 42, "hi"))
	tg.waitFor(t, "release", func() bool { return len(svc.requests("release")) == 1 })
	h.Wait()
	for _, c := range tg.of("sendMessage") {
		if c.body["text"] == "secret answer" {
			t.Fatal("an unrecorded reply was sent")
		}
	}
	edits := tg.of("editMessageText")
	if len(edits) == 0 || !strings.Contains(edits[len(edits)-1].body["text"].(string), "cannot record deliveries") {
		t.Fatalf("the status must say why: %+v", edits)
	}
	if len(svc.requests("record_delivery")) != 0 {
		t.Fatal("no record may be sent without the capability")
	}
}

// A failed turn shows the error and is still released.
func TestFailedTurnShowsTheErrorAndReleases(t *testing.T) {
	tg := newFakeTG(t)
	svc := newFakeSvc(t, []string{engine.CapOrdinarySubmit, engine.CapComposeUserText, engine.CapRecordDelivery})
	svc.submit = func(sc svcConn) {
		c := sc.conn
		write(c, map[string]any{"reply": "submitted", "thread_id": thread, "input_id": 1, "queued_ahead": 0})
		write(c, agent(thread, 5, map[string]any{"type": "turn_finished", "status": map[string]any{"status": "failed", "kind": "api_error", "message": "provider <down>"}, "final_text": "", "messages": []any{}}))
		write(c, event(map[string]any{"type": "turn_ended", "thread_id": thread, "turn_id": 5, "held": true}))
		for range 500 {
			if len(svc.requests("release")) > 0 {
				break
			}
			time.Sleep(10 * time.Millisecond)
		}
		c.Close()
	}
	h, _ := newHandler(tg, svc, TopicInfo{ThreadID: thread, Kind: TopicOrdinary})
	h.Dispatch(context.Background(), userMessage(t, 42, "hi"))
	tg.waitFor(t, "release", func() bool { return len(svc.requests("release")) == 1 })
	h.Wait()
	edits := tg.of("editMessageText")
	if len(edits) == 0 || !strings.Contains(edits[len(edits)-1].body["text"].(string), "provider &lt;down&gt;") {
		t.Fatalf("the status must show the error: %+v", edits)
	}
	if len(svc.requests("record_delivery")) != 0 {
		t.Fatal("a failed turn has no reply to record")
	}
}

func fmtAny(v any) string { return fmt.Sprint(v) }

// lastMessageID is the id the fake gave its newest sendMessage.
func lastMessageID(f *fakeTG) int64 {
	f.mu.Lock()
	defer f.mu.Unlock()
	return f.lastSent
}

// /queue text, synthetic messages and slash text are queued, never steered;
// a stale cancel_q button gets the Rust answer.
func TestQueueAndSyntheticMessagesAreNotSteered(t *testing.T) {
	tg := newFakeTG(t)
	svc := newFakeSvc(t, []string{engine.CapOrdinarySubmit, engine.CapComposeUserText, engine.CapRecordDelivery})
	h, _ := newHandler(tg, svc, TopicInfo{ThreadID: thread, Kind: TopicOrdinary})
	synthetic := userMessage(t, 43, "feedback text")
	synthetic.Synthetic = true
	h.Dispatch(context.Background(), userMessage(t, 42, "/queue run later"))
	h.Dispatch(context.Background(), synthetic)
	h.Dispatch(context.Background(), userMessage(t, 44, "plain"))
	h.Queues.Wait()
	subs := svc.requests("submit")
	if len(subs) != 3 {
		t.Fatalf("three submits: %v", svc.ops())
	}
	want := []string{"composed:run later/queue", "composed:feedback text/queue", "composed:plain/steer"}
	for i, s := range subs {
		if got := fmt.Sprint(s["text"], "/", s["delivery"]); got != want[i] {
			t.Errorf("submit %d: %s, want %s", i, got, want[i])
		}
	}
	data := "cancel_q:-100:42"
	h.HandleCallback(context.Background(), &telegram.CallbackQuery{ID: "q", From: telegram.User{ID: 7}, Data: &data, Message: msg(t, `{"message_id":9,"chat":{"id":-100,"type":"supergroup"}}`)})
	tg.waitFor(t, "the answer", func() bool { return len(tg.of("answerCallbackQuery")) == 1 })
	if a := tg.of("answerCallbackQuery")[0].body["text"]; a != "Already processing" {
		t.Fatalf("stale cancel_q: %v", a)
	}
}
