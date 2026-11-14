package bot

import (
	"context"
	"strings"
	"testing"

	"github.com/tallesborges/zdx/apps/gateway/internal/engine"
)

func TestWorkerUpdateNoticeMatchesTheRustBot(t *testing.T) {
	cases := map[string]string{
		"completed": `✅ Worker 🛠 <a href="https://x/?startapp=w">a &lt;b&gt;</a> finished · reviewing…`,
		"cancelled": `🚫 Worker 🛠 <a href="https://x/?startapp=w">a &lt;b&gt;</a> was cancelled · reviewing…`,
		"failed":    `❌ Worker 🛠 <a href="https://x/?startapp=w">a &lt;b&gt;</a> failed · reviewing…`,
	}
	for status, want := range cases {
		if got := WorkerUpdateNotice("a <b>", workerLink(nil, "https://x/", "w"), status); got != want {
			t.Errorf("%s: %q", status, got)
		}
	}
	mirror := "https://t.me/c/1/2"
	if got := workerLink(&mirror, "https://x/", "w"); got != mirror {
		t.Fatalf("a mirror topic wins: %s", got)
	}
	if got := WorkerUpdateNotice("t", "", "completed"); got != "✅ Worker 🛠 t finished · reviewing…" {
		t.Fatalf("no link: %s", got)
	}
}

func TestWorkerNoticesPostOnlyForOwnedCompletions(t *testing.T) {
	tg := newFakeTG(t)
	svc := newFakeSvc(t, []string{engine.CapThreadViews})
	svc.view = func(req map[string]any) map[string]any {
		return map[string]any{"reply": "view", "body": map[string]any{"workers": []any{map[string]any{"thread_id": "worker-123456789", "title": "fix the build"}}}}
	}
	var refreshed []string
	w := &WorkerNotices{Telegram: tg.client(), Engine: engine.NewClient(svc.socket), RefreshHeader: func(_ context.Context, owner string) { refreshed = append(refreshed, owner) }, Log: quiet}
	route := &engine.Route{Chat: -100, Topic: ptr(int64(5))}
	ev := func(inner string, r *engine.Route) engine.Event {
		owner := "owner"
		return engine.Event{Type: engine.EventWorker, OwnerThreadID: &owner, Route: r, Inner: []byte(inner)}
	}
	w.Observe(context.Background(), ev(`{"kind":"created","owner_thread_id":"owner","worker_thread_id":"worker-123456789"}`, route))
	w.Observe(context.Background(), ev(`{"kind":"activity","worker_thread_id":"worker-123456789","activity":{"kind":"tool_started","id":"1","name":"bash"}}`, route))
	w.Observe(context.Background(), ev(`{"kind":"completed","owner_thread_id":"owner","worker_thread_id":"worker-123456789","status":"completed","final_text":"x","error":null,"mirror_url":null}`, route))
	// Self-owned (re-attached from a mirror) and unrouted completions post nothing.
	w.Observe(context.Background(), ev(`{"kind":"completed","owner_thread_id":"self","worker_thread_id":"self","status":"failed","final_text":null,"error":"e","mirror_url":null}`, route))
	w.Observe(context.Background(), ev(`{"kind":"completed","owner_thread_id":"owner","worker_thread_id":"worker-123456789","status":"failed","final_text":null,"error":"e","mirror_url":null}`, nil))
	sends := tg.of("sendMessage")
	if len(sends) != 1 || sends[0].body["text"] != "✅ Worker 🛠 fix the build finished · reviewing…" || sends[0].body["message_thread_id"] != float64(5) || sends[0].body["reply_to_message_id"] != nil {
		t.Fatalf("one notice in the owner's topic: %+v", sends)
	}
	if strings.Join(refreshed, ",") != "owner,owner,self,owner" {
		t.Fatalf("headers refresh on created and every completion: %v", refreshed)
	}

	// Without the view the title is the first 8 characters of the id.
	svc.setCaps()
	w.Observe(context.Background(), ev(`{"kind":"completed","owner_thread_id":"owner","worker_thread_id":"worker-123456789","status":"cancelled","final_text":null,"error":null,"mirror_url":null}`, route))
	if last := tg.of("sendMessage")[1].body["text"]; last != "🚫 Worker 🛠 worker-1 was cancelled · reviewing…" {
		t.Fatalf("fallback title: %v", last)
	}
}

func TestQueueCommandMatchesTheRustBot(t *testing.T) {
	cases := map[string]string{
		"/queue run the tests":     "run the tests",
		"/queue@zdx_bot  later ":   "later",
		"  /queue x":               "x",
		"/queue":                   "",
		"/queue   ":                "",
		"/queued x":                "",
		"please /queue x":          "",
		"/queue@ x":                "",
		"/queue\nmulti\nline text": "multi\nline text",
	}
	for text, want := range cases {
		in := userMessage(t, 1, text)
		out := queueCommand(in)
		if want == "" {
			if out.ForceQueue || *out.Message.Text != text {
				t.Errorf("%q must stay unchanged", text)
			}
			continue
		}
		if !out.ForceQueue || *out.Message.Text != want || *in.Message.Text != text {
			t.Errorf("%q -> %q (force %v); the original must not change", text, *out.Message.Text, out.ForceQueue)
		}
	}
}
