package commands

import (
	"context"
	"errors"
	"strings"
	"testing"

	"github.com/tallesborges/zdx/apps/gateway/internal/telegram"
)

type fakeStatuses struct {
	status ThreadStatus
	err    error
	got    []string
}

func (f *fakeStatuses) ThreadStatus(_ context.Context, _ int64, thread string) (ThreadStatus, error) {
	f.got = append(f.got, thread)
	s := f.status
	s.ThreadID = thread
	return s, f.err
}

func agentStatus() ThreadStatus {
	return ThreadStatus{
		Model: "anthropic:claude-big@high", ModelSource: "topic", Mode: "mode:smart",
		Thinking: "high", ThinkingSource: "topic", Profile: "work", Root: "/srv/a&b", Branch: "main",
		ContextLimit: 200_000, ContextTokens: 45_678,
		Usage:   Usage{Input: 1_234_567, Output: 999, CacheRead: 12_000, CacheWrite: 0},
		Pricing: Pricing{Kind: PricingPriced, Cost: 0.5678, Savings: 0.02, Input: 3, Output: 15, CacheRead: 0.3, CacheWrite: 3.75},
	}
}

func TestStatusText(t *testing.T) {
	got := StatusText(agentStatus(), "<b>Status</b>", "")
	want := strings.Join([]string{
		"<b>Status</b>",
		"Model: <code>anthropic:claude-big@high</code> (topic override, <code>mode:smart</code>)",
		"Thinking: <code>high</code> (topic override)",
		"Thread: <code></code>",
		"Profile: <code>work</code>",
		"Root: <code>/srv/a&amp;b</code>",
		"Branch: <code>main</code>",
		"Context usage: <code>23% of 200k (45.7k)</code>",
		"Usage totals: <code>↑1.2M ↓999 R12.0k W0</code>",
		"Pricing: <code>$0.57</code> total · rates <code>$3/$15/$0.3/$3.75</code>/1M · saved <code>$0.02</code>",
	}, "\n")
	if got != want {
		t.Fatalf("status:\n%s\nwant:\n%s", got, want)
	}

	s := ThreadStatus{
		Orchestrator: true, Refused: "mode `gone` <undefined>", Model: "openai:gpt-6", ModelSource: "orchestrator",
		Thinking: "off", ThinkingSource: "default", ThreadID: "t", Root: "/x",
		Usage: Usage{CacheRead: 5}, Pricing: Pricing{Kind: PricingPriced, Cost: 0.0004},
		Workers: []StatusWorker{
			{ThreadID: "w1", Title: "Build <it>", Status: "running"},
			{ThreadID: "w2-0123456789", Status: "queued", MirrorURL: "https://t.me/c/1/2"},
			{ThreadID: "w3", Status: "completed"}, {ThreadID: "w4", Status: "failed"},
			{ThreadID: "w5", Status: "cancelled"}, {ThreadID: "w6", Status: "completed"},
		},
	}
	got = StatusText(s, "🧵 <b>Thread</b>", "https://app.example/app")
	for _, line := range []string{
		"🎛 <b>Orchestrator</b>\n⚠️ Turns here are refused: mode `gone` &lt;undefined&gt;. Pick another model with /model.",
		"Model: <code>openai:gpt-6</code> (orchestrator override)\nThinking: <code>off</code> (workspace/global default)",
		"Profile: <code>fallback</code>\nContext usage: <code>0</code> (limit unknown)",
		"Pricing: <code>$0.0004</code> total · rates <code>$0/$0/$0/$0</code>/1M · cached",
		"Workers: <code>1 running · 1 queued · 4 settled</code>",
		"• ⚙️ <a href=\"https://app.example/app?startapp=w1\">Build &lt;it&gt;</a> — running",
		"• ⏳ <a href=\"https://t.me/c/1/2\">w2-01234</a> — queued",
		"• 🚫 <a href=\"https://app.example/app?startapp=w5\">w5</a> — cancelled\n• … and 1 more",
	} {
		if !strings.Contains(got, line) {
			t.Errorf("orchestrator card lacks %q:\n%s", line, got)
		}
	}
	if strings.Contains(got, "Root:") {
		t.Error("an orchestrator card shows no root or branch")
	}

	if got := StatusText(ThreadStatus{Workers: nil, Orchestrator: true, Pricing: Pricing{Kind: PricingSubscription}}, "h", ""); !strings.Contains(got, "Pricing: <code>subscription</code> (OAuth provider)") || !strings.HasSuffix(got, "Workers: <i>none yet</i>") {
		t.Fatalf("subscription:\n%s", got)
	}
	if got := StatusText(ThreadStatus{}, "h", ""); !strings.Contains(got, "Pricing: <code>unknown</code> (model registry metadata not found)") {
		t.Fatalf("unknown pricing:\n%s", got)
	}
	if got := StatusText(ThreadStatus{ConfigError: "bad <toml>"}, "<b>Status</b>", ""); got != "<b>Status</b>\n⚠️ <pre>bad &lt;toml&gt;</pre>" {
		t.Fatalf("config error %q", got)
	}
}

func TestStatusCommand(t *testing.T) {
	tg := newTG()
	st := &fakeStatuses{status: agentStatus()}
	st.status.Profile = ""
	h := &Handler{Telegram: tg, Statuses: st, Chats: fakeChats{binding: Binding{Profile: "work<1>", Root: "/r"}}}
	h.HandleMessage(context.Background(), topicMsg(10, "/status@zdx_bot"), ordinary())
	if got := tg.lastSend(t); !strings.HasPrefix(got.text, "<b>Status</b>\nModel:") || !strings.Contains(got.text, "Profile: <code>work&lt;1&gt;</code>") || *got.reply != 10 || st.got[0] != threadOf7 {
		t.Fatalf("status %+v", got)
	}
	st.err = errors.New("read thread <x>")
	h.HandleMessage(context.Background(), topicMsg(11, "/status"), ordinary())
	if got := tg.lastSend(t).text; got != "❌ read thread &lt;x&gt;" {
		t.Fatalf("error %q", got)
	}
}

type notModifiedTG struct{ *fakeTG }

func (n notModifiedTG) EditMessageText(ctx context.Context, chat, id int64, text string, markup *telegram.InlineKeyboardMarkup) error {
	_ = n.fakeTG.EditMessageText(ctx, chat, id, text, markup)
	return &telegram.APIError{Method: "editMessageText", Code: 400, Description: "Bad Request: message is not modified"}
}

func TestThreadHeaderPostRefreshAndButton(t *testing.T) {
	tg := newTG()
	topics := &fakeTopics{}
	st := &fakeStatuses{status: agentStatus()}
	st.status.ParentOrchestrator = "telegram--1001234567890-topic-3"
	h := &Handler{Telegram: tg, Topics: topics, Statuses: st, Resolver: fakeResolver{thread: ordinary()}, Chats: fakeChats{url: "https://app.example/app"}}
	if err := h.PostThreadHeader(context.Background(), forumChat, topicID, threadOf7); err != nil {
		t.Fatal(err)
	}
	post := tg.lastSend(t)
	if !strings.HasPrefix(post.text, "🧵 <b>Thread</b>\nModel:") || !strings.HasSuffix(post.text, "\n\n<i>Tap Refresh for current usage.</i>") || *post.topic != topicID {
		t.Fatalf("header %q", post.text)
	}
	want := "💬 Open Thread=https://app.example/app?startapp=" + threadOf7 + " | 🎛 Open orchestrator=https://t.me/c/1234567890/3 | ↻ Refresh=thread_header:refresh"
	if got := buttons(post.markup); got != want {
		t.Fatalf("keyboard %s", got)
	}
	if len(topics.pinned) != 1 || topics.pinned[0] != post.id {
		t.Fatalf("pinned %v", topics.pinned)
	}

	h.RefreshHeader(context.Background(), threadOf7)
	if len(tg.of("edit")) != 0 {
		t.Fatal("an unchanged header sends no edit")
	}
	st.status.ContextTokens = 150_000
	h.RefreshHeader(context.Background(), threadOf7)
	if e := lastEdit(t, tg); e.id != post.id || !strings.Contains(e.text, "75% of 200k") {
		t.Fatalf("refresh %+v", e)
	}
	h.RefreshHeader(context.Background(), "unknown")
	if len(tg.of("edit")) != 1 {
		t.Fatal("an unknown header is left alone")
	}

	if a := tap(t, h, tg, ptr(topicID), post.id, "thread_header:refresh"); a != "Thread status refreshed ✓" {
		t.Fatalf("button %q", a)
	}
	if a := tap(t, h, tg, nil, post.id, "thread_header:refresh"); a != "This header is not inside a topic" {
		t.Fatalf("outside a topic %q", a)
	}
	h.Telegram = notModifiedTG{tg}
	if a := tap(t, h, tg, ptr(topicID), post.id, "thread_header:refresh"); a != "Thread status refreshed ✓" {
		t.Fatalf("not modified is success: %q", a)
	}
	st.err = errors.New("boom")
	if a := tap(t, h, tg, ptr(topicID), post.id, "thread_header:refresh"); a != "Couldn't refresh thread status" {
		t.Fatalf("failure %q", a)
	}

	st.err = nil
	st.status.ParentOrchestrator = "orchestrator-thread"
	if got := buttons(HeaderKeyboard("", "x", "orchestrator-thread")); got != "↻ Refresh=thread_header:refresh" {
		t.Fatalf("no link form %s", got)
	}
	if got := buttons(HeaderKeyboard("https://a", "x", "orchestrator-thread")); !strings.Contains(got, "🎛 Open orchestrator=https://a?startapp=orchestrator-thread") {
		t.Fatalf("mini app parent %s", got)
	}
}

func TestSeededTopicsGetAHeader(t *testing.T) {
	th := ordinary()
	r := newStaging(th)
	r.h.Statuses = &fakeStatuses{status: agentStatus()}
	r.msg(t, 10, "/btw", th)
	r.msg(t, 11, "why?", th)
	sends := r.tg.of("send")
	// ask card, header, record, created link
	if len(sends) != 4 || !strings.HasPrefix(sends[1].text, "🧵 <b>Thread</b>") || *sends[1].topic != 501 || len(r.topics.pinned) != 1 {
		t.Fatalf("sends %d, header %q", len(sends), sends[1].text)
	}
}

// The StartModel contract (v5 decision; today's Rust bot hands the mode on
// instead): a source pinned to a mode the new topic's config lacks is
// refused, so nothing opens and nothing is written, and the session stays
// for a retry. Go never substitutes a default.
func TestUndefinedSourceModeFailsClosed(t *testing.T) {
	for _, command := range []string{"/handoff", "/btw"} {
		th := ordinary()
		r := newStaging(th)
		r.gen.text = "ctx"
		r.seeds.startErr = errors.New("source selection `mode:gone` is not configured here")
		r.msg(t, 10, command, th)
		r.msg(t, 11, "go", th)
		if len(r.topics.names) != 0 || len(r.threads.setups) != 0 || len(r.d.sent) != 0 {
			t.Fatalf("%s: a refused start model opens nothing: %v %+v", command, r.topics.names, r.threads.setups)
		}
		var shown string
		if command == "/handoff" {
			shown = lastEdit(t, r.tg).text
		} else {
			shown = r.tg.lastSend(t).text
		}
		if !strings.Contains(shown, "<pre>source selection `mode:gone` is not configured here</pre>") {
			t.Fatalf("%s: failure %q", command, shown)
		}
		if !r.h.HasLiveSession(threadOf7) {
			t.Fatalf("%s: the session stays for a retry", command)
		}
	}
}
