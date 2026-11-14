package bot

import (
	"context"
	"encoding/json"
	"fmt"
	"io"
	"net/http"
	"os"
	"path/filepath"
	"regexp"
	"strings"
	"sync/atomic"
	"testing"
	"time"

	"github.com/tallesborges/zdx/apps/gateway/internal/engine"
)

// sseToolUse is one Anthropic streaming answer that calls a tool.
func sseToolUse(id, name, input string) string {
	quoted, _ := json.Marshal(input)
	return `event: message_start
data: {"type":"message_start","message":{"id":"msg_t","type":"message","role":"assistant","content":[],"model":"claude-sonnet-5","stop_reason":null,"stop_sequence":null,"usage":{"input_tokens":10,"output_tokens":1}}}

event: content_block_start
data: {"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"` + id + `","name":"` + name + `","input":{}}}

event: content_block_delta
data: {"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":` + string(quoted) + `}}

event: content_block_stop
data: {"type":"content_block_stop","index":0}

event: message_delta
data: {"type":"message_delta","delta":{"stop_reason":"tool_use","stop_sequence":null},"usage":{"output_tokens":5}}

event: message_stop
data: {"type":"message_stop"}

`
}

// lastTurn is the last message of an Anthropic request: its role, its
// text, and whether it carries a tool result.
func lastTurn(body string) (role, text string, toolResult bool) {
	var req struct {
		Messages []struct {
			Role    string          `json:"role"`
			Content json.RawMessage `json:"content"`
		} `json:"messages"`
	}
	if json.Unmarshal([]byte(body), &req) != nil || len(req.Messages) == 0 {
		return "", "", false
	}
	last := req.Messages[len(req.Messages)-1]
	var s string
	if json.Unmarshal(last.Content, &s) == nil {
		return last.Role, s, false
	}
	var blocks []map[string]any
	_ = json.Unmarshal(last.Content, &blocks)
	for _, b := range blocks {
		switch b["type"] {
		case "text":
			t, _ := b["text"].(string)
			text += t
		case "tool_result":
			toolResult = true
		}
	}
	return last.Role, text, toolResult
}

// workerScript answers the orchestrator topic, its worker (a real child
// process of the service), and the worker-update turn.
func workerScript(w http.ResponseWriter, body, root string) {
	w.Header().Set("Content-Type", "text/event-stream")
	_, text, toolResult := lastTurn(body)
	switch {
	case toolResult && strings.Contains(body, "toolu_wbash"):
		io.WriteString(w, sse("WORKER-MIRRORED"))
	case toolResult && strings.Contains(body, "toolu_send"):
		io.WriteString(w, sse("SENT-TO-WORKER"))
	case strings.Contains(text, "MIRROR-PROMPT"):
		io.WriteString(w, sseToolUse("toolu_wbash", "Bash", `{"command":"sleep 0.3; true"}`))
	case strings.Contains(text, "please prompt the worker "):
		_, rest, _ := strings.Cut(text, "please prompt the worker ")
		input, _ := json.Marshal(map[string]string{"thread_id": strings.Fields(rest)[0], "message": "MIRROR-PROMPT **run** it"})
		io.WriteString(w, sseToolUse("toolu_send", "Send_Thread_Message", string(input)))
	case strings.Contains(text, "WORKER-MIRRORED"):
		io.WriteString(w, sse("UPDATE-ANSWER-2"))
	case strings.Contains(text, "MIRROR-STEER"):
		io.WriteString(w, sse("WORKER-STEERED"))
	case strings.Contains(text, "WORKER-TASK") && !strings.Contains(text, "WORKER-DONE"):
		io.WriteString(w, sse("WORKER-DONE"))
	case toolResult:
		io.WriteString(w, sse("STARTED-WORKER"))
	case strings.Contains(text, "start a worker"):
		input := fmt.Sprintf(`{"prompt":"WORKER-TASK: say done","title":"synthetic worker","model":%q,"root":%q}`, mockModel, root)
		io.WriteString(w, sseToolUse("toolu_1", "Create_Thread", input))
	case strings.Contains(text, "WORKER-DONE") || strings.Contains(text, "WORKER-STEERED"):
		io.WriteString(w, sse("UPDATE-ANSWER"))
	default:
		io.WriteString(w, sse("OK"))
	}
}

// A real worker, end to end: an orchestrator topic's turn creates a worker
// through the Create_Thread tool, the worker runs as a child process of
// the service, and its result comes back as a worker_update turn that the
// Go renderer bridge delivers to the topic. Then a legacy mirror topic for
// that worker (a thread fixture with worker_topic and alias_to, read and
// validated by Rust's resolve_thread) queues a message to the worker
// through enqueue_from_topic.
func TestRealRustWorkerUpdateAndMirrorTopic(t *testing.T) {
	var r *realRust
	root := ""
	r = startRealRust(t, func(_ context.Context, n int, w http.ResponseWriter) {
		r.mu.Lock()
		body := r.requests[n]
		r.mu.Unlock()
		workerScript(w, body, root)
	})
	root = r.h.RootForChat(-100)
	threadID := realThread
	m := mockModel
	setup, err := r.eng.SubmitFollow(context.Background(), engine.Submit{ThreadID: &threadID, Root: r.h.RootForChat(-100), Model: &m, Text: "setup"})
	if err != nil {
		t.Fatalf("create the orchestrator thread: %v\n%s", err, r.log())
	}
	setup.Turn.Wait()
	r.waitFor("setup settled", 20*time.Second, func() bool { return r.settled(realThread) })

	r.h.Presenter.Planner = ServicePlanner{Engine: r.eng, MiniAppBase: "https://t.me/x/app"}
	r.h.Dispatch(context.Background(), userMessage(t, 42, "please start a worker"))
	r.waitFor("the orchestrator answer", 30*time.Second, func() bool { return r.sent("STARTED-WORKER") })
	deadline := time.Now().Add(60 * time.Second)
	for !r.sent("UPDATE-ANSWER") {
		if time.Now().After(deadline) {
			r.mu.Lock()
			for i, b := range r.requests {
				role, text, tr := lastTurn(b)
				t.Logf("request %d: %s tool_result=%v %.300q", i, role, tr, text)
			}
			r.mu.Unlock()
			log := r.log()
			if len(log) > 6000 {
				log = log[len(log)-6000:]
			}
			t.Fatalf("no worker update delivered\nservice log tail:\n%s", log)
		}
		time.Sleep(50 * time.Millisecond)
	}
	r.waitFor("settled", 30*time.Second, func() bool { return r.settled(realThread) })

	workers, err := r.eng.ListWorkers(context.Background(), &threadID)
	if err != nil || len(workers) != 1 {
		t.Fatalf("workers: %+v %v", workers, err)
	}
	worker := workers[0].ThreadID
	// The turn that created the worker links it under its reply
	// (turn_ended.touched_workers), with the title Rust gave it.
	if r.eng.Require(context.Background(), engine.CapTouchedWorkers) == nil {
		link := "\n\n🛠 <a href=\"https://t.me/x/app?startapp=" + worker + "\">synthetic worker</a>"
		found := false
		for _, c := range r.tg.of("sendMessage") {
			if s, _ := c.body["text"].(string); strings.HasPrefix(s, "STARTED-WORKER") {
				found = strings.Contains(s, link)
				if !found {
					t.Fatalf("worker link missing: %q", s)
				}
			}
		}
		if !found {
			t.Fatal("no STARTED-WORKER reply")
		}
	}
	// The worker's notice precedes the review turn's answer, in the topic.
	notice, answerAt := -1, -1
	for i, c := range r.tg.of("sendMessage") {
		text, _ := c.body["text"].(string)
		if strings.HasPrefix(text, "✅ Worker 🛠 ") && c.body["message_thread_id"] == float64(5) {
			notice = i
			want := `✅ Worker 🛠 <a href="https://t.me/x/app?startapp=` + worker + `">synthetic worker</a> finished · reviewing…`
			if text != want {
				t.Fatalf("notice:\n got %q\nwant %q", text, want)
			}
		}
		if strings.Contains(text, "UPDATE-ANSWER") {
			answerAt = i
		}
	}
	if notice < 0 || answerAt < notice {
		t.Fatalf("the worker notice must come before the review answer: notice=%d answer=%d", notice, answerAt)
	}
	if d := r.events(realThread, "delivery"); len(d) != 4 {
		t.Fatalf("the user turn and the worker update each record pending and final: %d", len(d))
	}

	// A legacy mirror topic (topic 9) for the worker, as the Rust bot wrote
	// them: a thin thread with worker_topic and alias_to.
	mirror := "telegram--100-topic-9"
	meta := fmt.Sprintf(`{"type":"meta","schema_version":1,"alias_to":%q,"worker_topic":true,"ts":"2026-01-01T00:00:00Z"}`, worker)
	if err := os.WriteFile(filepath.Join(r.home, "threads", mirror+".jsonl"), []byte(meta+"\n"), 0o600); err != nil {
		t.Fatal(err)
	}
	topic := int64(9)
	route, err := r.eng.ResolveThread(context.Background(), -100, &topic)
	if err != nil || route.Kind != engine.KindWorkerMirror {
		t.Fatalf("Rust must read the fixture as a worker mirror: %+v %v", route, err)
	}
	before := len(r.tg.of("sendMessage"))
	in := userMessage(t, 50, "MIRROR-STEER please")
	in.Message.ThreadID = &topic
	r.h.Resolver = ServiceResolver{Engine: r.eng}
	r.h.Dispatch(context.Background(), in)
	r.waitFor("the queued notice", 20*time.Second, func() bool {
		for _, c := range r.tg.of("sendMessage")[before:] {
			if s, _ := c.body["text"].(string); strings.HasPrefix(s, "📨 Queued to the worker") && c.body["message_thread_id"] == float64(9) {
				return true
			}
		}
		return false
	})
	r.waitFor("the worker ran the mirror message", 60*time.Second, func() bool {
		for _, ev := range r.events(worker, "message") {
			if s, _ := ev["text"].(string); ev["role"] == "user" && strings.Contains(s, "MIRROR-STEER") {
				return true
			}
		}
		return false
	})
	if n := len(r.events(mirror, "message")); n != 0 {
		t.Fatalf("a mirror topic never gets its own messages: %d", n)
	}
	if r.eng.Require(context.Background(), engine.CapWorkerMirrorRoute) != nil {
		return
	}

	// The legacy feed (capability worker_mirror_route). The service reads
	// mirror topics at start, so restart it; then the orchestrator prompts
	// the worker, which runs a tool and answers.
	r.waitFor("worker idle", 60*time.Second, func() bool {
		w, err := r.eng.GetWorker(context.Background(), worker)
		return err == nil && w != nil && w.Status != "running" && w.Status != "queued" && w.QueueDepth == 0
	})
	r.waitFor("settled", 30*time.Second, func() bool { return r.settled(realThread) })
	r.restartService()
	r.waitFor("the service again", 30*time.Second, func() bool { return r.settled(realThread) })
	time.Sleep(300 * time.Millisecond) // the bridge resubscribes
	before = len(r.tg.calls)
	r.h.Dispatch(context.Background(), userMessage(t, 60, "please prompt the worker "+worker))
	r.waitFor("the second worker update", 60*time.Second, func() bool { return r.sent("UPDATE-ANSWER-2") })
	r.waitFor("settled", 30*time.Second, func() bool { return r.settled(realThread) })
	r.tg.mu.Lock()
	calls := append([]tgCall(nil), r.tg.calls[before:]...)
	r.tg.mu.Unlock()
	var inMirror []string
	statusID := int64(0)
	for _, c := range calls {
		if c.body["message_thread_id"] == float64(9) || (c.method != "sendMessage" && c.body["chat_id"] == float64(-100) && statusID != 0 && c.body["message_id"] == float64(statusID)) {
			text, _ := c.body["text"].(string)
			inMirror = append(inMirror, c.method+":"+text)
			if c.method == "sendMessage" && strings.HasPrefix(text, "🔧 Running") {
				statusID = c.id
			}
		}
	}
	// Rust's Prompted carries the whole message, the orchestrator's header
	// included.
	if len(inMirror) < 4 || !strings.HasPrefix(inMirror[0], "sendMessage:<b>📤 Prompt from the orchestrator</b>\n\n") ||
		!strings.HasSuffix(inMirror[0], "MIRROR-PROMPT <b>run</b> it") || inMirror[1] != "sendMessage:"+ToolRunningStatus("bash") ||
		inMirror[len(inMirror)-2] != "deleteMessage:" ||
		inMirror[len(inMirror)-1] != "sendMessage:WORKER-MIRRORED\n\n<a href=\"https://t.me/x/app?startapp="+worker+"\">↗ Open thread</a>" {
		t.Fatalf("mirror feed:\n%s", strings.Join(inMirror, "\n"))
	}
	// The prompting turn's reply links the worker it touched.
	for _, c := range calls {
		if s, _ := c.body["text"].(string); strings.HasPrefix(s, "SENT-TO-WORKER") && !strings.Contains(s, "startapp="+worker+"\">synthetic worker</a>") {
			t.Fatalf("touched worker link: %q", s)
		}
	}
}

// Per-turn environment (reviewer H3), through the real service: two topics'
// turns run at the same time, each calls Bash with an overlapping sleep,
// and each tool sees its own ZDX_THREAD_ID and ZDX_ARTIFACT_DIR.
func TestRealRustConcurrentTurnsSeeTheirOwnEnvironment(t *testing.T) {
	var r *realRust
	var calls, results atomic.Int32
	overlapped := atomic.Bool{}
	r = startRealRust(t, func(_ context.Context, n int, w http.ResponseWriter) {
		r.mu.Lock()
		body := r.requests[n]
		r.mu.Unlock()
		w.Header().Set("Content-Type", "text/event-stream")
		if _, _, toolResult := lastTurn(body); toolResult {
			results.Add(1)
			io.WriteString(w, sse("ENV-DONE"))
			return
		}
		// Both tool calls go out before either result comes back: the
		// two Bash commands run at the same time.
		if calls.Add(1) == 2 && results.Load() == 0 {
			overlapped.Store(true)
		}
		io.WriteString(w, sseToolUse("toolu_env", "Bash", `{"command":"sleep 1; printf 'ENV[%s|%s]' \"$ZDX_THREAD_ID\" \"$ZDX_ARTIFACT_DIR\""}`))
	})
	in := func(id, topic int64) Incoming {
		m := userMessage(t, id, fmt.Sprintf("env check %d", topic))
		m.Message.ThreadID = &topic
		return m
	}
	r.h.Dispatch(context.Background(), in(42, 5))
	r.h.Dispatch(context.Background(), in(43, 6))
	r.waitFor("both answers", 60*time.Second, func() bool {
		n := 0
		for _, c := range r.tg.of("sendMessage") {
			if s, _ := c.body["text"].(string); strings.Contains(s, "ENV-DONE") {
				n++
			}
		}
		return n == 2
	})
	r.mu.Lock()
	defer r.mu.Unlock()
	seen := map[string]string{}
	for _, body := range r.requests {
		if _, _, tr := lastTurn(body); !tr {
			continue
		}
		// The request repeats the command (ENV[%s|%s]); the tool result
		// holds the values.
		m := envResult.FindStringSubmatch(body)
		if m == nil {
			t.Fatalf("tool output missing:\n%.2000s", body)
		}
		seen[m[1]] = m[2]
	}
	if !overlapped.Load() {
		t.Fatal("the two turns did not overlap; the check proves nothing")
	}
	for _, thread := range []string{"telegram--100-topic-5", "telegram--100-topic-6"} {
		dir, ok := seen[thread]
		if !ok || !strings.Contains(dir, thread) {
			t.Fatalf("each turn sees its own thread and artifact dir: %v", seen)
		}
	}
}

var envResult = regexp.MustCompile(`ENV\[(telegram-[^|\]]+)\|([^\]]*)\]`)
