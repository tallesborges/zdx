package bot

import (
	"context"
	"encoding/json"
	"fmt"
	"strings"
	"sync"
	"testing"

	"github.com/tallesborges/zdx/apps/gateway/internal/engine"
	"github.com/tallesborges/zdx/apps/gateway/internal/telegram"
)

func TestParseCommandMatchesTheRustBot(t *testing.T) {
	cases := map[string]command{
		"/new":                      {kind: "new"},
		" /new@zdx_bot ":            {kind: "new"},
		"/model set openai:gpt-5 x": {kind: "model_set", model: "openai:gpt-5"},
		"/model@zdx_bot set m":      {kind: "model_set", model: "m"},
		"/model reset":              {kind: "model_reset"},
	}
	for text, want := range cases {
		if got, ok := parseCommand(text); !ok || got != want {
			t.Errorf("%q: got %+v %v", text, got, ok)
		}
	}
	for _, text := range []string{"/new please", "/newer", "/model", "/model list", "/model set", "/status", "hello"} {
		if _, ok := parseCommand(text); ok {
			t.Errorf("%q must not be handled", text)
		}
	}
}

func commandIn(t *testing.T, chat string, topic, text string) Incoming {
	raw := fmt.Sprintf(`{"message_id":42,"chat":%s,"from":{"id":7,"is_bot":false},"text":%q`, chat, text)
	if topic != "" {
		raw += `,"message_thread_id":` + topic
	}
	return Incoming{Message: msg(t, raw+"}")}
}

const privateChat = `{"id":7,"type":"private"}`
const forumChat = `{"id":-100,"type":"supergroup","is_forum":true}`

func lastText(t *testing.T, tg *fakeTG) string {
	t.Helper()
	sends := tg.of("sendMessage")
	return sends[len(sends)-1].body["text"].(string)
}

func TestNewResetsTheEffectiveThread(t *testing.T) {
	tg := newFakeTG(t)
	svc := newFakeSvc(t, []string{engine.CapResetThread})
	h, _ := newHandler(tg, svc, TopicInfo{ThreadID: "telegram-7", Kind: TopicOrdinary})
	h.Dispatch(context.Background(), commandIn(t, privateChat, "", "/new"))
	tg.waitFor(t, "reply", func() bool { return len(tg.of("sendMessage")) == 1 })
	if r := svc.requests("reset_thread"); len(r) != 1 || r[0]["thread_id"] != "telegram-7" {
		t.Fatalf("unexpected reset %v", r)
	}
	if got := lastText(t, tg); got != "History cleared. Start a new conversation anytime." {
		t.Fatalf("unexpected reply %q", got)
	}
	if len(svc.requests("submit")) != 0 || len(svc.requests("compose_user_text")) != 0 {
		t.Fatal("a command starts no turn")
	}
}

func TestNewRefusals(t *testing.T) {
	cases := []struct {
		name, chat, topic string
		info              TopicInfo
		want              string
	}{
		{"forum topic", forumChat, "5", TopicInfo{ThreadID: thread, Kind: TopicOrdinary}, "/new is not allowed in topics."},
		{"orchestrator", privateChat, "", TopicInfo{ThreadID: "telegram-7", Kind: TopicProfile}, "/new is not available in an orchestrator thread. Start a new thread instead."},
		{"busy", privateChat, "", TopicInfo{ThreadID: "busy", Kind: TopicOrdinary}, "❌ thread 'busy' has work queued or running; cancel it first"},
	}
	for _, c := range cases {
		tg := newFakeTG(t)
		svc := newFakeSvc(t, []string{engine.CapResetThread})
		h, _ := newHandler(tg, svc, c.info)
		h.Dispatch(context.Background(), commandIn(t, c.chat, c.topic, "/new"))
		tg.waitFor(t, c.name, func() bool { return len(tg.of("sendMessage")) == 1 })
		if got := lastText(t, tg); got != c.want {
			t.Errorf("%s: got %q", c.name, got)
		}
		if c.name != "busy" && len(svc.requests("reset_thread")) != 0 {
			t.Errorf("%s: nothing may be reset", c.name)
		}
	}
}

func TestNewWithoutCapabilityResetsNothing(t *testing.T) {
	tg := newFakeTG(t)
	svc := newFakeSvc(t, nil)
	h, _ := newHandler(tg, svc, TopicInfo{ThreadID: "telegram-7", Kind: TopicOrdinary})
	h.Dispatch(context.Background(), commandIn(t, privateChat, "", "/new"))
	tg.waitFor(t, "reply", func() bool { return len(tg.of("sendMessage")) == 1 })
	if got := lastText(t, tg); !strings.Contains(got, engine.CapResetThread) || len(svc.requests("reset_thread")) != 0 {
		t.Fatalf("got %q, resets %d", got, len(svc.requests("reset_thread")))
	}
}

func TestTopicModelSetAndResetGoThroughRust(t *testing.T) {
	tg := newFakeTG(t)
	svc := newFakeSvc(t, []string{engine.CapUpdateThread})
	h, _ := newHandler(tg, svc, TopicInfo{ThreadID: "effective", Kind: TopicOrdinary})
	ctx := context.Background()

	h.Dispatch(ctx, commandIn(t, forumChat, "5", "/model set openai:gpt-5"))
	tg.waitFor(t, "set", func() bool { return len(tg.of("sendMessage")) == 1 })
	if got := lastText(t, tg); got != "✅ Model set to <code>label:openai:gpt-5</code> for this topic." {
		t.Fatalf("unexpected reply %q", got)
	}
	h.Dispatch(ctx, commandIn(t, forumChat, "5", "/model reset"))
	tg.waitFor(t, "reset", func() bool { return len(tg.of("sendMessage")) == 2 })
	if got := lastText(t, tg); got != "✅ Model reset to default: <code>default-model</code>" {
		t.Fatalf("unexpected reply %q", got)
	}
	h.Dispatch(ctx, commandIn(t, forumChat, "5", "/model set bogus"))
	tg.waitFor(t, "invalid", func() bool { return len(tg.of("sendMessage")) == 3 })
	if got := lastText(t, tg); got != "❌ unknown model &lt;bogus&gt;" {
		t.Fatalf("Rust's refusal is shown escaped: %q", got)
	}

	reqs := svc.requests("update_thread")
	if len(reqs) != 3 || reqs[0]["thread_id"] != "effective" || reqs[0]["config_root"] != "/project" || reqs[0]["model"] != "openai:gpt-5" {
		t.Fatalf("unexpected updates %v", reqs)
	}
	if m, ok := reqs[1]["model"]; !ok || m != nil {
		t.Fatalf("reset must send model:null, got %v", reqs[1])
	}
}

// In a chat without topics /model set|reset sets the chat thread's own
// model, as in the Rust bot; in General it sets the chat default.
func TestModelSetOutsideTopicsAndInGeneral(t *testing.T) {
	tg := newFakeTG(t)
	svc := newFakeSvc(t, []string{engine.CapUpdateThread, engine.CapSetChatModel})
	h, _ := newHandler(tg, svc, TopicInfo{ThreadID: "telegram-7", Kind: TopicOrdinary})
	h.Dispatch(context.Background(), commandIn(t, privateChat, "", "/model set m"))
	tg.waitFor(t, "reply", func() bool { return len(tg.of("sendMessage")) == 1 })
	if got := lastText(t, tg); !strings.Contains(got, "Model set to <code>label:m</code> for this topic") {
		t.Fatalf("got %q", got)
	}
	if u := svc.requests("update_thread"); len(u) != 1 || u[0]["thread_id"] != "telegram-7" || u[0]["config_root"] != "/project" {
		t.Fatalf("update_thread %v", u)
	}
	h.Dispatch(context.Background(), commandIn(t, forumChat, "", "/model set p:m@high"))
	tg.waitFor(t, "reply", func() bool { return len(tg.of("sendMessage")) == 2 })
	if got := lastText(t, tg); got != "✅ Default model set to <code>p:m@high</code>." {
		t.Fatalf("got %q", got)
	}
	if r := svc.requests("set_chat_model"); len(r) != 1 || r[0]["root"] != "/project" || r[0]["model"] != "p:m@high" {
		t.Fatalf("set_chat_model %v", r)
	}
}

func TestCommandsInMirrorTopicsGoToTheWorker(t *testing.T) {
	tg := newFakeTG(t)
	svc := newFakeSvc(t, []string{engine.CapComposeUserText, engine.CapResetThread})
	h, _ := newHandler(tg, svc, TopicInfo{Kind: TopicWorkerMirror, WorkerThreadID: "w"})
	h.Dispatch(context.Background(), commandIn(t, forumChat, "5", "/new"))
	tg.waitFor(t, "reply", func() bool { return len(tg.of("sendMessage")) == 1 })
	if len(svc.requests("reset_thread")) != 0 || len(svc.requests("enqueue_from_topic")) != 1 {
		t.Fatalf("ops %v", svc.ops())
	}
}

func TestGeneralTopicCommandsStayUnsupported(t *testing.T) {
	tg := newFakeTG(t)
	svc := newFakeSvc(t, []string{engine.CapResetThread})
	h, _ := newHandler(tg, svc, TopicInfo{})
	h.Dispatch(context.Background(), commandIn(t, forumChat, "", "/new"))
	tg.waitFor(t, "reply", func() bool { return len(tg.of("sendMessage")) == 1 })
	if got := lastText(t, tg); !strings.Contains(got, "General topic") || len(svc.ops()) != 0 {
		t.Fatalf("got %q ops %v", got, svc.ops())
	}
}

func TestUpdateThreadEncodesThreeStateModel(t *testing.T) {
	m := "x"
	for want, u := range map[string]engine.UpdateThread{
		`{"model":"x","op":"update_thread","thread_id":"t"}`:  {ThreadID: "t", Model: &m},
		`{"model":null,"op":"update_thread","thread_id":"t"}`: {ThreadID: "t", ClearModel: true},
		`{"op":"update_thread","thread_id":"t"}`:              {ThreadID: "t"},
	} {
		line, err := engine.EncodeRequest(u)
		if err != nil {
			t.Fatal(err)
		}
		var a, b map[string]any
		json.Unmarshal(line, &a)
		json.Unmarshal([]byte(want), &b)
		if fmt.Sprint(a) != fmt.Sprint(b) {
			t.Errorf("got %s want %s", line, want)
		}
	}
}

type hookLog struct {
	mu   sync.Mutex
	reqs []CommandRequest
	taps []string
}

func (l *hookLog) HandleCommand(_ context.Context, req CommandRequest) bool {
	l.mu.Lock()
	defer l.mu.Unlock()
	l.reqs = append(l.reqs, req)
	return strings.HasPrefix(req.Text, "/status")
}

func (l *hookLog) HandleCallback(_ context.Context, q *telegram.CallbackQuery) bool {
	l.mu.Lock()
	defer l.mu.Unlock()
	l.taps = append(l.taps, *q.Data)
	return strings.HasPrefix(*q.Data, "cmd:")
}

func TestCommandHookRunsAfterTheBuiltInsAndUnclaimedTextGoesToTheAgent(t *testing.T) {
	tg := newFakeTG(t)
	svc := newFakeSvc(t, []string{engine.CapComposeUserText, engine.CapRecordDelivery, engine.CapResetThread})
	h, _ := newHandler(tg, svc, TopicInfo{ThreadID: thread, Kind: TopicOrdinary})
	hook := &hookLog{}
	h.Commands, h.CommandCallbacks = hook, hook
	h.Dispatch(context.Background(), userMessage(t, 42, " /status now "))
	h.Dispatch(context.Background(), userMessage(t, 43, "/unknown thing"))
	h.Queues.Wait()
	hook.mu.Lock()
	reqs := append([]CommandRequest(nil), hook.reqs...)
	hook.mu.Unlock()
	if len(reqs) != 2 || reqs[0].Text != "/status now" || reqs[0].Topic.ThreadID != thread || reqs[0].Root != "/project" || *reqs[0].ReplyTo != 42 {
		t.Fatalf("unexpected hook requests %+v", reqs)
	}
	if c := svc.requests("compose_user_text"); len(c) != 1 || c[0]["text"] != "/unknown thing" {
		t.Fatalf("an unclaimed command goes on as text: %v", c)
	}

	for _, data := range []string{"cmd:x", "nt:y"} {
		d := data
		h.HandleCallback(context.Background(), &telegram.CallbackQuery{ID: data, From: telegram.User{ID: 7}, Data: &d, Message: msg(t, `{"message_id":9,"chat":{"id":-100,"type":"supergroup"}}`)})
	}
	tg.waitFor(t, "the fallback answer", func() bool { return len(tg.of("answerCallbackQuery")) == 1 })
	if a := tg.of("answerCallbackQuery")[0].body; a["callback_query_id"] != "nt:y" {
		t.Fatalf("only the unclaimed tap gets the fallback answer: %v", a)
	}
}
