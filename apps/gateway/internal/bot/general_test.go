package bot

import (
	"context"
	"strings"
	"sync"
	"testing"
	"time"

	"github.com/tallesborges/zdx/apps/gateway/internal/engine"
)

func TestTopicNameMatchesTheRustBot(t *testing.T) {
	now := time.Date(2026, 1, 2, 3, 4, 5, 0, time.FixedZone("x", 3600))
	long := strings.Repeat("word ", 20)
	cases := map[string]string{
		"  plan the trip \nsecond line": "plan the trip ",
		"":                              "Chat 2026-01-02 02:04",
		"   ":                           "Chat 2026-01-02 02:04",
		long:                            strings.TrimSpace(long[:60]) + "…",
		strings.Repeat("x", 70):         strings.Repeat("x", 64) + "…",
		strings.Repeat("é", 64):         strings.Repeat("é", 64),
	}
	for in, want := range cases {
		if got := TopicName(in, now); got != want {
			t.Errorf("TopicName(%q) = %q, want %q", in, got, want)
		}
	}
}

type fakeTopicHooks struct {
	mu           sync.Mutex
	orchestrator bool
	headers      []string
	reposts      int
}

func (f *fakeTopicHooks) OrchestratorChat(int64) bool              { return f.orchestrator }
func (f *fakeTopicHooks) IsCommand(text string) bool               { return text == "/status" }
func (f *fakeTopicHooks) OrchestratorTopicName(name string) string { return "🎛️ " + name }
func (f *fakeTopicHooks) PostThreadHeader(_ context.Context, _, _ int64, threadID string) error {
	f.mu.Lock()
	defer f.mu.Unlock()
	f.headers = append(f.headers, threadID)
	return nil
}
func (f *fakeTopicHooks) ScheduleLauncherRepost(context.Context, int64) {
	f.mu.Lock()
	defer f.mu.Unlock()
	f.reposts++
}

func generalMessage(t *testing.T, id int64, text string) Incoming {
	return Incoming{Message: msg(t, `{"message_id":`+itoa(id)+`,"chat":{"id":-100,"type":"supergroup","is_forum":true},"from":{"id":7,"is_bot":false},"text":"`+text+`"}`)}
}

// A General message whose topic cannot be created gets the Rust notice in
// General; one whose orchestrator profile cannot be stored gets it in the
// new topic. Neither runs a turn or posts a header.
func TestGeneralTopicFailuresRunNoTurn(t *testing.T) {
	tg := newFakeTG(t)
	svc := newFakeSvc(t, []string{engine.CapOrdinarySubmit, engine.CapComposeUserText, engine.CapRecordDelivery})
	hooks := &fakeTopicHooks{}
	h, _ := newHandler(tg, svc, TopicInfo{ThreadID: "telegram--100", Kind: TopicOrdinary})
	h.Topics = hooks
	h.Blocks = &TopicBlocks{Dir: t.TempDir()}
	tg.queue("createForumTopic", `{"ok":false,"error_code":400,"description":"Bad Request: not enough rights"}`)
	h.Dispatch(context.Background(), generalMessage(t, 40, "hello"))
	tg.waitFor(t, "the create failure notice", func() bool {
		for _, c := range tg.of("sendMessage") {
			if c.body["text"] == EscapeHTML(TopicCreateFailedNotice) && c.body["reply_to_message_id"] == float64(40) && c.body["message_thread_id"] == nil {
				return true
			}
		}
		return false
	})

	hooks.orchestrator = true
	tg.queue("createForumTopic", `{"ok":true,"result":{"message_thread_id":77,"name":"x"}}`)
	h.Dispatch(context.Background(), generalMessage(t, 41, "hello"))
	tg.waitFor(t, "the orchestrator failure notice", func() bool {
		for _, c := range tg.of("sendMessage") {
			if c.body["text"] == EscapeHTML(OrchestratorInitFailedNotice) && c.body["message_thread_id"] == float64(77) {
				return true
			}
		}
		return false
	})
	h.Wait()
	creates := tg.of("createForumTopic")
	if len(creates) != 2 || creates[1].body["name"] != "🎛️ hello" {
		t.Fatalf("creates %v", creates)
	}
	if len(svc.requests("submit")) != 0 || len(hooks.headers) != 0 || len(tg.of("forwardMessage")) != 0 {
		t.Fatalf("no turn, header or forward after a failure: submits %d headers %v", len(svc.requests("submit")), hooks.headers)
	}
	// Later messages in the topic are refused too, across restarts.
	if !h.Blocks.Blocked("telegram--100-topic-77") || !(&TopicBlocks{Dir: h.Blocks.Dir}).Blocked("telegram--100-topic-77") {
		t.Fatal("the topic whose orchestrator setup failed is blocked")
	}
	// The service lacks update_thread here, so storing the profile fails.
	if u := svc.requests("update_thread"); len(u) != 0 {
		t.Fatalf("update_thread %v", u)
	}
}
