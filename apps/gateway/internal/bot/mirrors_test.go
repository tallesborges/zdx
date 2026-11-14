package bot

import (
	"context"
	"encoding/json"
	"slices"
	"strings"
	"testing"
	"time"

	"github.com/tallesborges/zdx/apps/gateway/internal/engine"
	"github.com/tallesborges/zdx/apps/gateway/internal/render"
)

// mirrorSvc is a fake service whose worker "legacy" has mirror topic 9 and
// whose worker "silent" has none.
func mirrorSvc(t *testing.T, caps []string) *fakeSvc {
	svc := newFakeSvc(t, caps)
	svc.worker = func(req map[string]any) map[string]any {
		w := map[string]any{"thread_id": req["thread_id"], "owner_thread_id": "owner", "root": "/r", "status": "running", "queue_depth": 0, "queue": []any{}}
		switch req["thread_id"] {
		case "legacy":
			w["mirror_url"] = "https://t.me/c/100/9"
			w["mirror"] = map[string]any{"chat": -100, "topic": 9, "thread_id": "telegram--100-topic-9"}
		case "mismatch":
			w["mirror"] = map[string]any{"chat": -100, "topic": 9, "thread_id": "telegram--100-topic-8"}
		}
		return map[string]any{"reply": "worker", "worker": w}
	}
	return svc
}

func workerEv(inner string, route *engine.Route) engine.Event {
	owner := "owner"
	return engine.Event{Type: engine.EventWorker, OwnerThreadID: &owner, Route: route, Inner: []byte(inner)}
}

// The legacy feed posts what the Rust bot posts in a worker's mirror topic:
// the orchestrator's prompt, one live status line (edited at most every
// interval, deleted at the end) and the result, then the owner's notice.
// A worker without a mirror topic gets none of it.
func TestMirrorFeedMatchesTheRustBot(t *testing.T) {
	tg := newFakeTG(t)
	svc := mirrorSvc(t, []string{engine.CapWorkerMirrorRoute})
	eng := engine.NewClient(svc.socket)
	var refreshed []string
	feed := &MirrorFeed{Telegram: tg.client(), Lookup: eng.WorkerMirror, MiniAppBase: "https://t.me/x/app", Log: quiet, Interval: 100 * time.Millisecond}
	w := &WorkerNotices{Telegram: tg.client(), MiniAppBase: "https://t.me/x/app", Mirrors: feed, Log: quiet,
		RefreshHeader: func(_ context.Context, owner string) { refreshed = append(refreshed, owner) }}
	route := &engine.Route{Chat: -100, Topic: ptr(int64(5))}
	ctx := context.Background()

	w.Observe(ctx, workerEv(`{"kind":"prompted","worker_thread_id":"legacy","prompt":"**do** it"}`, route))
	w.Observe(ctx, workerEv(`{"kind":"activity","worker_thread_id":"legacy","activity":{"kind":"tool_started","id":"1","name":"bash"}}`, route))
	w.Observe(ctx, workerEv(`{"kind":"activity","worker_thread_id":"legacy","activity":{"kind":"tool_input","id":"1","arg":"ls"}}`, route))
	w.Observe(ctx, workerEv(`{"kind":"activity","worker_thread_id":"legacy","activity":{"kind":"tool_finished","id":"1","ok":true}}`, route))
	if n := len(tg.of("editMessageText")); n != 0 {
		t.Fatalf("an edit waits for the interval: %d", n)
	}
	tg.waitFor(t, "the debounced edit", func() bool { return len(tg.of("editMessageText")) == 1 })
	w.Observe(ctx, workerEv(`{"kind":"completed","owner_thread_id":"owner","worker_thread_id":"legacy","status":"failed","final_text":null,"error":"\u001b[31mprovider: boom <x>\u001b[0m","mirror_url":"https://t.me/c/100/9"}`, route))
	// A silent worker: no mirror posts, no refresh on activity.
	w.Observe(ctx, workerEv(`{"kind":"prompted","worker_thread_id":"silent","prompt":"p"}`, route))
	w.Observe(ctx, workerEv(`{"kind":"activity","worker_thread_id":"silent","activity":{"kind":"tool_started","id":"1","name":"bash"}}`, route))

	tg.mu.Lock()
	calls := append([]tgCall(nil), tg.calls...)
	tg.mu.Unlock()
	var got []string
	for _, c := range calls {
		got = append(got, c.method)
	}
	if want := []string{"sendMessage", "sendMessage", "editMessageText", "deleteMessage", "sendMessage", "sendMessage"}; !slices.Equal(got, want) {
		t.Fatalf("calls %v, want %v", got, want)
	}
	prompt, status, edit, del, result, notice := calls[0], calls[1], calls[2], calls[3], calls[4], calls[5]
	if prompt.body["text"] != "<b>📤 Prompt from the orchestrator</b>\n\n"+render.ToTelegramHTML("**do** it") || prompt.body["message_thread_id"] != float64(9) || prompt.body["chat_id"] != float64(-100) {
		t.Fatalf("prompt %v", prompt.body)
	}
	kb, _ := status.body["reply_markup"].(map[string]any)
	if status.body["text"] != ToolRunningStatus("bash") || status.body["message_thread_id"] != float64(9) ||
		!strings.Contains(toString(kb), `"callback_data":"wk:c"`) || !strings.Contains(toString(kb), "https://t.me/x/app?startapp=legacy") {
		t.Fatalf("status %v", status.body)
	}
	if edit.body["text"] != StatusWaiting || edit.body["message_id"] != float64(status.id) {
		t.Fatalf("edit %v", edit.body)
	}
	if del.body["message_id"] != float64(status.id) {
		t.Fatalf("delete %v", del.body)
	}
	wantResult := `❌ Turn failed: boom &lt;x&gt;` + "\n\n" + `<a href="https://t.me/x/app?startapp=legacy">↗ Open thread</a>`
	if result.body["text"] != wantResult || result.body["message_thread_id"] != float64(9) {
		t.Fatalf("result %q", result.body["text"])
	}
	if notice.body["message_thread_id"] != float64(5) || !strings.Contains(notice.body["text"].(string), `href="https://t.me/c/100/9"`) {
		t.Fatalf("notice %v", notice.body)
	}
	if strings.Join(refreshed, ",") != "owner,owner,owner,owner" {
		t.Fatalf("refresh on prompted, the first mirrored activity, completed, prompted: %v", refreshed)
	}
	var lookups []string
	for _, op := range svc.ops() {
		if strings.HasPrefix(op, "worker:") {
			lookups = append(lookups, op)
		}
	}
	if strings.Join(lookups, ",") != "worker:legacy,worker:silent" {
		t.Fatalf("one lookup per worker: %v", lookups)
	}
}

// Without worker_mirror_route nothing is asked or posted; a route whose
// thread does not match is ignored.
func TestMirrorFeedNeedsTheCapabilityAndAConsistentRoute(t *testing.T) {
	tg := newFakeTG(t)
	svc := mirrorSvc(t, nil)
	eng := engine.NewClient(svc.socket)
	feed := &MirrorFeed{Telegram: tg.client(), Lookup: eng.WorkerMirror, Log: quiet}
	ctx := context.Background()
	feed.Prompted(ctx, "legacy", "p")
	if feed.Activity(ctx, "legacy", []byte(`{"kind":"tool_started","id":"1","name":"bash"}`)) {
		t.Fatal("no feed without the capability")
	}
	svc.setCaps(engine.CapWorkerMirrorRoute)
	feed.Prompted(ctx, "legacy", "p")
	feed.Prompted(ctx, "mismatch", "p")
	tg.mu.Lock()
	n := len(tg.calls)
	tg.mu.Unlock()
	if n != 0 {
		t.Fatal("nothing posted")
	}
	for _, op := range svc.ops() {
		if strings.HasPrefix(op, "worker:legacy") {
			t.Fatalf("a missing capability is not asked again within %v: %v", mirrorRetry, svc.ops())
		}
	}
	if !slices.Contains(svc.ops(), "worker:mismatch") {
		t.Fatalf("ops %v", svc.ops())
	}
	// A new renderer subscription forgets: the service may have restarted.
	feed.Forget()
	feed.Prompted(ctx, "legacy", "p")
	tg.waitFor(t, "the prompt after Forget", func() bool { return len(tg.of("sendMessage")) == 1 })
}

func TestMirrorResultTextMatchesTheRustBot(t *testing.T) {
	str := func(s string) *string { return &s }
	long := strings.Repeat("x", 600)
	cases := []struct {
		ev   engine.WorkerEvent
		want string
	}{
		{engine.WorkerEvent{Status: "completed", FinalText: str("**ok**")}, render.ToTelegramHTML("**ok**")},
		{engine.WorkerEvent{Status: "completed"}, render.ToTelegramHTML("")},
		{engine.WorkerEvent{Status: "cancelled"}, "🚫 Turn cancelled."},
		{engine.WorkerEvent{Status: "failed"}, "❌ Turn failed: <pre>unknown error</pre>"},
		{engine.WorkerEvent{Status: "failed", Error: str("x: Overloaded")}, "❌ Turn failed: provider overloaded"},
		{engine.WorkerEvent{Status: "failed", Error: str("429 rate limit")}, "❌ Turn failed: provider rate limited"},
		{engine.WorkerEvent{Status: "failed", Error: str("a\nexecute prompt: bad & worse\n\n")}, "❌ Turn failed: bad &amp; worse"},
		{engine.WorkerEvent{Status: "failed", Error: str("Subagent failed: 12:00 WARN x")}, "❌ Turn failed: <pre>Subagent failed: 12:00 WARN x</pre>"},
		{engine.WorkerEvent{Status: "failed", Error: str(long)}, "❌ Turn failed: <pre>" + strings.Repeat("x", 500) + "… [truncated — use Read_Thread for the rest]</pre>"},
	}
	for _, c := range cases {
		if got := MirrorResultText(c.ev); got != c.want {
			t.Errorf("%+v:\n got %q\nwant %q", c.ev, got, c.want)
		}
	}
}

func toString(v any) string {
	b, _ := json.Marshal(v)
	return string(b)
}
