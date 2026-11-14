package gateway

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"net/http"
	"net/http/httptest"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"sync"
	"syscall"
	"testing"
	"time"

	"github.com/tallesborges/zdx/apps/gateway/internal/engine"
	"github.com/tallesborges/zdx/apps/gateway/internal/sandbox"
	"github.com/tallesborges/zdx/apps/gateway/internal/telegram"
)

// flowModel is a registry model the mock provider answers for every role
// (turns, titles, recaps, handoffs, prompt builder).
const flowModel = "anthropic:claude-haiku-5-5"

// flowCall is one Bot API call the fake recorded.
type flowCall struct {
	Method string
	Body   map[string]any
	ID     int64 // the message id a send answered with
}

// flowTelegram is a fake Bot API for whole-gateway flows: it hands out
// queued updates, answers sends with fresh ids, opens topics, and remembers
// every message's inline keyboard so a test can tap its buttons.
type flowTelegram struct {
	mu        sync.Mutex
	updates   []string
	calls     []flowCall
	nextID    int64
	nextTopic int64
	keyboards map[int64]any
	srv       *httptest.Server
}

func newFlowTelegram(t *testing.T) *flowTelegram {
	f := &flowTelegram{nextID: 1000, nextTopic: 100, keyboards: map[int64]any{}}
	f.srv = httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		method := r.URL.Path[strings.LastIndex(r.URL.Path, "/")+1:]
		if method == "getUpdates" {
			f.mu.Lock()
			out := f.updates
			f.updates = nil
			f.mu.Unlock()
			if len(out) == 0 {
				select {
				case <-r.Context().Done():
				case <-time.After(20 * time.Millisecond):
				}
			}
			io.WriteString(w, `{"ok":true,"result":[`+strings.Join(out, ",")+`]}`)
			return
		}
		body := map[string]any{}
		if strings.HasPrefix(r.Header.Get("Content-Type"), "multipart/") {
			if r.ParseMultipartForm(32<<20) == nil {
				for k, v := range r.MultipartForm.Value {
					body[k] = v[0]
				}
			}
		} else {
			raw, _ := io.ReadAll(r.Body)
			_ = json.Unmarshal(raw, &body)
		}
		f.mu.Lock()
		f.nextID++
		id := f.nextID
		switch method {
		case "editMessageText", "editMessageReplyMarkup":
			if mid, ok := body["message_id"].(float64); ok {
				if kb, ok := body["reply_markup"]; ok {
					f.keyboards[int64(mid)] = kb
				}
			}
		case "sendMessage":
			if kb, ok := body["reply_markup"]; ok {
				f.keyboards[id] = kb
			}
		case "createForumTopic":
			f.nextTopic++
			body["topic"] = float64(f.nextTopic)
		}
		f.calls = append(f.calls, flowCall{Method: method, Body: body, ID: id})
		topic := body["topic"]
		f.mu.Unlock()
		switch method {
		case "sendMessage", "sendDocument", "sendPhoto", "forwardMessage":
			fmt.Fprintf(w, `{"ok":true,"result":{"message_id":%d,"chat":{"id":-100,"type":"supergroup","is_forum":true},"date":1}}`, id)
		case "createForumTopic":
			fmt.Fprintf(w, `{"ok":true,"result":{"message_thread_id":%v,"name":"x"}}`, topic)
		default:
			io.WriteString(w, `{"ok":true,"result":true}`)
		}
	}))
	t.Cleanup(f.srv.Close)
	return f
}

func (f *flowTelegram) push(update string) {
	f.mu.Lock()
	defer f.mu.Unlock()
	f.updates = append(f.updates, update)
}

func (f *flowTelegram) snapshot() []flowCall {
	f.mu.Lock()
	defer f.mu.Unlock()
	return append([]flowCall(nil), f.calls...)
}

// find returns the last call matching pred after index from.
func (f *flowTelegram) find(from int, pred func(flowCall) bool) (flowCall, bool) {
	calls := f.snapshot()
	for i := len(calls) - 1; i >= from && i >= 0; i-- {
		if pred(calls[i]) {
			return calls[i], true
		}
	}
	return flowCall{}, false
}

// button is the callback data of the button whose text contains label on
// message id.
func (f *flowTelegram) button(id int64, label string) (string, bool) {
	f.mu.Lock()
	kb := f.keyboards[id]
	f.mu.Unlock()
	m, _ := kb.(map[string]any)
	rows, _ := m["inline_keyboard"].([]any)
	for _, row := range rows {
		cells, _ := row.([]any)
		for _, c := range cells {
			b, _ := c.(map[string]any)
			if text, _ := b["text"].(string); strings.Contains(text, label) {
				data, ok := b["callback_data"].(string)
				return data, ok
			}
		}
	}
	return "", false
}

func flowText(c flowCall) string { s, _ := c.Body["text"].(string); return s }

func flowTopic(c flowCall) float64 { v, _ := c.Body["message_thread_id"].(float64); return v }

// skipClock is the wall clock with sleeps skipped: a sleep moves the
// clock forward instead. Telegram pacing (a group's new-message window)
// then costs nothing, so a flow can send more than a minute's worth of
// messages to one fake chat.
type skipClock struct {
	mu     sync.Mutex
	offset time.Duration
}

func (c *skipClock) Now() time.Time {
	c.mu.Lock()
	defer c.mu.Unlock()
	return time.Now().Add(c.offset)
}

func (c *skipClock) Sleep(ctx context.Context, d time.Duration) error {
	if d > 0 {
		c.mu.Lock()
		c.offset += d
		c.mu.Unlock()
	}
	return ctx.Err()
}

// realGateway is the whole gateway (BotComponents, as serve wires it)
// against a real `zdx orchestrator serve` on a temp home, a mock model
// endpoint and the fake Bot API.
type realGateway struct {
	t       *testing.T
	home    string
	root    string
	tg      *flowTelegram
	eng     *engine.Client
	logPath string
	update  int
	msgID   int64

	mu      sync.Mutex
	prompts []string
}

func startRealGateway(t *testing.T, config string) *realGateway {
	t.Helper()
	bin := sandbox.RustBinary(t)
	home, err := os.MkdirTemp("", "zf")
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { os.RemoveAll(home) })
	g := &realGateway{t: t, home: home, root: t.TempDir(), logPath: filepath.Join(home, "service.log"), msgID: 10}
	t.Cleanup(func() {
		if t.Failed() {
			b, _ := os.ReadFile(g.logPath)
			if len(b) > 16<<10 {
				b = b[len(b)-16<<10:]
			}
			t.Logf("service log tail:\n%s", b)
		}
	})
	if err := os.WriteFile(filepath.Join(home, "config.toml"), []byte(config), 0o600); err != nil {
		t.Fatal(err)
	}
	provider := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		b, _ := io.ReadAll(r.Body)
		g.mu.Lock()
		g.prompts = append(g.prompts, string(b))
		n := len(g.prompts)
		g.mu.Unlock()
		w.Header().Set("Content-Type", "text/event-stream")
		io.WriteString(w, e2eSSE(fmt.Sprintf("MOCK-%d", n)))
	}))
	t.Cleanup(provider.Close)

	logFile, _ := os.Create(g.logPath)
	t.Cleanup(func() { logFile.Close() })
	cmd := exec.Command(bin, "--root", g.root, "orchestrator", "serve")
	cmd.Dir = g.root
	cmd.Env = []string{"PATH=" + os.Getenv("PATH"), "TMPDIR=" + os.Getenv("TMPDIR"), "HOME=" + home, "ZDX_HOME=" + home,
		"ANTHROPIC_API_KEY=test-api-key", "ANTHROPIC_BASE_URL=" + provider.URL}
	cmd.Stdout, cmd.Stderr = logFile, logFile
	if err := cmd.Start(); err != nil {
		t.Fatal(err)
	}
	exited := make(chan error, 1)
	go func() { exited <- cmd.Wait() }()
	t.Cleanup(func() {
		_ = cmd.Process.Signal(syscall.SIGTERM)
		select {
		case <-exited:
		case <-time.After(5 * time.Second):
			_ = cmd.Process.Kill()
			<-exited
		}
	})
	socket := engine.SocketPath(home)
	g.eng = engine.NewClient(socket)
	for start := time.Now(); ; time.Sleep(20 * time.Millisecond) {
		_, err := g.eng.StatusReport(context.Background(), nil)
		if err == nil {
			break
		}
		if !errors.Is(err, engine.ErrRequestNotSent) || time.Since(start) > 30*time.Second {
			t.Fatalf("service not ready: %v", err)
		}
	}
	g.tg = newFlowTelegram(t)
	// Chat -200 is an orchestrator chat bound to the same root.
	settings := Settings{BotToken: token, AllowlistUserIDs: []int64{7}, AllowlistChatIDs: []int64{-100, -200}, Root: g.root,
		Profiles: []Profile{{Name: "synthetic", ChatID: -200, Cwd: g.root, Orchestrator: true}}}
	ctx, cancel := context.WithCancel(context.Background())
	done := make(chan error, 1)
	go func() {
		done <- Run(ctx, Config{
			Settings: settings, Socket: socket, Home: home, Log: quiet, DrainTimeout: 20 * time.Second,
			Telegram: telegram.Options{BaseURL: g.tg.srv.URL, Logger: quiet, Clock: &skipClock{}},
			Build: func(d Deps) (Components, error) {
				return BotComponents(d.Work, d, settings, ServiceParts(d, settings)), nil
			},
		})
	}()
	t.Cleanup(func() {
		cancel()
		select {
		case err := <-done:
			if err != nil {
				t.Errorf("gateway: %v", err)
			}
		case <-time.After(30 * time.Second):
			t.Error("gateway did not stop")
		}
	})
	return g
}

// say posts a user message in chat -100 (topic 0: General) and returns
// its id.
func (g *realGateway) say(topic int64, text string) int64 { return g.sayIn(-100, topic, text) }

// sayIn posts a user message in a forum chat.
func (g *realGateway) sayIn(chat, topic int64, text string) int64 {
	g.update++
	g.msgID++
	thread := ""
	if topic != 0 {
		thread = fmt.Sprintf(`"message_thread_id":%d,"is_topic_message":true,`, topic)
	}
	quoted, _ := json.Marshal(text)
	g.tg.push(fmt.Sprintf(`{"update_id":%d,"message":{"message_id":%d,%s"chat":{"id":%d,"type":"supergroup","is_forum":true},"from":{"id":7,"is_bot":false},"text":%s}}`,
		g.update, g.msgID, thread, chat, quoted))
	return g.msgID
}

// sayPrivate posts a user message in a Threaded Mode thread of the bot's
// private chat with user 7.
func (g *realGateway) sayPrivate(thread int64, text string) int64 {
	g.update++
	g.msgID++
	quoted, _ := json.Marshal(text)
	g.tg.push(fmt.Sprintf(`{"update_id":%d,"message":{"message_id":%d,"message_thread_id":%d,"chat":{"id":7,"type":"private"},"from":{"id":7,"is_bot":false},"text":%s}}`,
		g.update, g.msgID, thread, quoted))
	return g.msgID
}

// tap presses the button labelled label on message id in topic of chat
// -100.
func (g *realGateway) tap(id int64, topic int64, label string) { g.tapIn(-100, id, topic, label) }

// tapIn presses a button in a forum chat.
func (g *realGateway) tapIn(chat, id, topic int64, label string) {
	g.t.Helper()
	data, ok := g.tg.button(id, label)
	if !ok {
		g.t.Fatalf("no %q button on message %d", label, id)
	}
	g.update++
	thread := ""
	if topic != 0 {
		thread = fmt.Sprintf(`"message_thread_id":%d,"is_topic_message":true,`, topic)
	}
	quoted, _ := json.Marshal(data)
	g.tg.push(fmt.Sprintf(`{"update_id":%d,"callback_query":{"id":"cb%d","from":{"id":7,"is_bot":false},"data":%s,"message":{"message_id":%d,%s"chat":{"id":%d,"type":"supergroup","is_forum":true},"date":1}}}`,
		g.update, g.update, quoted, id, thread, chat))
}

// wait waits for a call after index from that matches pred and returns it.
func (g *realGateway) wait(what string, from int, pred func(flowCall) bool) flowCall {
	g.t.Helper()
	start := time.Now()
	deadline := start.Add(90 * time.Second)
	for {
		if c, ok := g.tg.find(from, pred); ok {
			if d := time.Since(start); d > 5*time.Second {
				g.t.Logf("waited %v for %s", d.Round(time.Millisecond), what)
			}
			return c
		}
		if time.Now().After(deadline) {
			var lines []string
			for i, c := range g.tg.snapshot() {
				if i >= from {
					lines = append(lines, fmt.Sprintf("%s topic=%v %.160q", c.Method, c.Body["message_thread_id"], flowText(c)))
				}
			}
			g.t.Fatalf("timed out waiting for %s; calls:\n%s", what, strings.Join(lines, "\n"))
		}
		time.Sleep(30 * time.Millisecond)
	}
}

func (g *realGateway) mark() int { return len(g.tg.snapshot()) }

func (g *realGateway) settled(thread string) bool {
	rep, err := g.eng.StatusReport(context.Background(), &thread)
	return err == nil && len(rep.Threads) == 1 && rep.Threads[0].State == "idle" && rep.Threads[0].Settled
}

func (g *realGateway) waitSettled(thread string) {
	g.t.Helper()
	for deadline := time.Now().Add(30 * time.Second); !g.settled(thread); time.Sleep(50 * time.Millisecond) {
		if time.Now().After(deadline) {
			g.t.Fatalf("%s never settled", thread)
		}
	}
}

func sendIn(topic float64, prefix string) func(flowCall) bool {
	return func(c flowCall) bool {
		return c.Method == "sendMessage" && flowTopic(c) == topic && strings.HasPrefix(flowText(c), prefix)
	}
}

func editTo(prefix string) func(flowCall) bool {
	return func(c flowCall) bool { return c.Method == "editMessageText" && strings.HasPrefix(flowText(c), prefix) }
}

// The command and helper flows of the whole gateway against the real
// service (contracts v9, v10): a General message opens a titled topic,
// /status, /tldr, /commands with a custom command, /model in a topic and
// in General (the chat default written to the temp workspace config),
// /btw, /handoff and the prompt builder.
func TestRealGatewayCommandFlows(t *testing.T) {
	cfg := fmt.Sprintf("model = %q\ntitle_model = %q\ntldr_model = %q\nhandoff_model = %q\nprompt_builder_model = %q\n"+
		"[[model_modes]]\nname = \"swift\"\ndescription = \"synthetic fast tier\"\nprimary = \"anthropic:claude-haiku-5-5@low\"\n"+
		"[telegram]\nbot_token = \"1:SYNTHETIC\"\nallowlist_user_ids = [7]\nallowlist_chat_ids = [-100, -200]\n",
		flowModel, flowModel, flowModel, flowModel, flowModel)
	g := startRealGateway(t, cfg)
	if err := g.eng.RequireAll(context.Background(), engine.CapGenerate, engine.CapBtwSeed, engine.CapTopicStart, engine.CapCatalog, engine.CapSetChatModel); err != nil {
		t.Fatalf("binary lacks the v9/v10 capabilities: %v", err)
	}
	cmds := filepath.Join(g.root, ".zdx", "commands")
	if err := os.MkdirAll(cmds, 0o700); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(filepath.Join(cmds, "ship.md"), []byte("---\ndescription: Ship it\n---\nSHIP-THE-THING\n"), 0o600); err != nil {
		t.Fatal(err)
	}

	t.Run("preflight is ready against this service", func(t *testing.T) {
		env := []string{"PATH=" + os.Getenv("PATH"), "TMPDIR=" + os.Getenv("TMPDIR"), "HOME=" + g.home, "ZDX_HOME=" + g.home}
		r := Preflight(context.Background(), PreflightOptions{Home: g.home, Root: g.root, Zdx: os.Getenv("ZDX_GATEWAY_RUST_ZDX"), Env: env})
		if !r.Ready || len(r.Problems) != 0 || len(r.Warnings) != 0 {
			t.Fatalf("preflight %+v", r)
		}
	})

	t.Run("general message opens a titled topic", func(t *testing.T) {
		from := g.mark()
		original := g.say(0, "plan the synthetic trip")
		created := g.wait("createForumTopic", from, func(c flowCall) bool { return c.Method == "createForumTopic" })
		if created.Body["name"] != "plan the synthetic trip" {
			t.Fatalf("topic name %v", created.Body["name"])
		}
		topic := created.Body["topic"].(float64)
		g.wait("the header pin", from, func(c flowCall) bool { return c.Method == "pinChatMessage" })
		fwd := g.wait("the forward", from, func(c flowCall) bool { return c.Method == "forwardMessage" })
		if fwd.Body["message_id"] != float64(original) || flowTopic(fwd) != topic {
			t.Fatalf("forward %v", fwd.Body)
		}
		reply := g.wait("the answer in the new topic", from, sendIn(topic, "MOCK-"))
		rp, _ := reply.Body["reply_parameters"].(map[string]any)
		if rp["message_id"] != float64(original) || rp["chat_id"] != float64(-100) || reply.Body["reply_to_message_id"] != nil {
			t.Fatalf("the answer quotes the General message across topics: %v", reply.Body)
		}
		rename := g.wait("the topic title", from, func(c flowCall) bool { return c.Method == "editForumTopic" })
		name, _ := rename.Body["name"].(string)
		if flowTopic(rename) != topic || !strings.HasPrefix(name, "MOCK-") {
			t.Fatalf("rename %v", rename.Body)
		}
		thread := fmt.Sprintf("telegram--100-topic-%d", int64(topic))
		g.waitSettled(thread)
		for deadline := time.Now().Add(10 * time.Second); ; time.Sleep(50 * time.Millisecond) {
			b, _ := os.ReadFile(filepath.Join(g.home, "threads", thread+".jsonl"))
			if strings.Contains(string(b), name) {
				break
			}
			if time.Now().After(deadline) {
				t.Fatalf("the thread stores the title %q:\n%s", name, b)
			}
		}

		from = g.mark()
		g.say(int64(topic), "/status")
		st := g.wait("the status card", from, sendIn(topic, "<b>Status</b>"))
		if !strings.Contains(flowText(st), "claude-haiku-5-5") || !strings.Contains(flowText(st), thread) {
			t.Fatalf("status %q", flowText(st))
		}

		from = g.mark()
		g.say(int64(topic), "/tldr")
		recap := g.wait("the recap", from, func(c flowCall) bool {
			return c.Method == "editMessageText" && strings.Contains(flowText(c), "MOCK-")
		})
		_ = recap

		from = g.mark()
		g.say(int64(topic), "/commands")
		picker := g.wait("the command picker", from, func(c flowCall) bool {
			return c.Method == "sendMessage" && strings.Contains(flowText(c), "ship")
		})
		g.tap(picker.ID, int64(topic), "ship")
		g.wait("the custom command's turn", from, func(c flowCall) bool {
			return c.Method == "sendMessage" && flowTopic(c) == topic && strings.HasPrefix(flowText(c), "MOCK-")
		})
		g.waitSettled(thread)
		g.mu.Lock()
		last := g.prompts[len(g.prompts)-1]
		g.mu.Unlock()
		if !strings.Contains(last, "SHIP-THE-THING") {
			t.Fatalf("the tap runs the command's content: %.400s", last)
		}

		from = g.mark()
		g.say(int64(topic), "/model")
		card := g.wait("the topic model picker", from, sendIn(topic, "Current model"))
		g.tap(card.ID, int64(topic), "Anthropic")
		g.wait("the models", from, func(c flowCall) bool {
			_, ok := g.tg.button(card.ID, "claude-sonnet-5-5")
			return c.Method == "editMessageText" && ok
		})
		g.tap(card.ID, int64(topic), "claude-sonnet-5-5")
		g.wait("the thinking levels", from, func(c flowCall) bool {
			_, ok := g.tg.button(card.ID, "low")
			return c.Method == "editMessageText" && ok
		})
		picked := g.mark()
		g.tap(card.ID, int64(topic), "low")
		done := g.wait("the set model", picked, func(c flowCall) bool {
			return c.Method == "editMessageText" && strings.Contains(flowText(c), "claude-sonnet-5-5")
		})
		t.Logf("model set: %q", flowText(done))
		var status struct {
			Model       string `json:"model"`
			ModelSource string `json:"model_source"`
		}
		if err := g.eng.Catalog(context.Background(), engine.Catalog{Kind: engine.CatalogThreadStatus, Root: g.root, ThreadID: &thread}, &status); err != nil ||
			!strings.Contains(status.Model, "claude-sonnet-5-5") || status.ModelSource != "topic" {
			t.Fatalf("the topic's model is stored: %+v %v", status, err)
		}

		from = g.mark()
		g.say(int64(topic), "/btw")
		g.wait("the /btw ask", from, func(c flowCall) bool { return c.Method == "sendMessage" && flowTopic(c) == topic })
		g.say(int64(topic), "what is the synthetic weather")
		side := g.wait("the side topic", from, func(c flowCall) bool { return c.Method == "createForumTopic" })
		sideTopic := side.Body["topic"].(float64)
		g.wait("the side answer", from, sendIn(sideTopic, "MOCK-"))
		g.wait("the side notice", from, sendIn(topic, "💬 <b>Side question created</b>"))
		g.waitSettled(fmt.Sprintf("telegram--100-topic-%d", int64(sideTopic)))

		from = g.mark()
		g.say(int64(topic), "/handoff")
		g.wait("the /handoff ask", from, func(c flowCall) bool { return c.Method == "sendMessage" && flowTopic(c) == topic })
		g.say(int64(topic), "continue the synthetic plan")
		hand := g.wait("the handoff topic", from, func(c flowCall) bool { return c.Method == "createForumTopic" })
		handTopic := hand.Body["topic"].(float64)
		g.wait("the handoff answer", from, sendIn(handTopic, "MOCK-"))
		g.wait("the handoff notice", from, sendIn(topic, "🔀 <b>Handoff created</b>"))
		g.waitSettled(fmt.Sprintf("telegram--100-topic-%d", int64(handTopic)))

		from = g.mark()
		g.say(int64(topic), "/prompt_builder")
		g.wait("the prompt builder ask", from, func(c flowCall) bool { return c.Method == "sendMessage" && flowTopic(c) == topic })
		g.say(int64(topic), "a synthetic intent")
		draft := g.wait("the draft", from, func(c flowCall) bool {
			_, ok := g.tg.button(c.ID, "Accept")
			if c.Method == "editMessageText" {
				id, _ := c.Body["message_id"].(float64)
				_, ok = g.tg.button(int64(id), "Accept")
			}
			return ok && strings.Contains(flowText(c), "MOCK-")
		})
		id := draft.ID
		if v, ok := draft.Body["message_id"].(float64); ok {
			id = int64(v)
		}
		g.tap(id, int64(topic), "Accept")
		g.wait("the accepted prompt's turn", from, func(c flowCall) bool {
			return c.Method == "sendMessage" && flowTopic(c) == topic && strings.HasPrefix(flowText(c), "MOCK-")
		})
		g.waitSettled(thread)
	})

	t.Run("new topic titles itself from its first message", func(t *testing.T) {
		from := g.mark()
		g.say(0, "/new")
		created := g.wait("the empty topic", from, func(c flowCall) bool { return c.Method == "createForumTopic" })
		topic := created.Body["topic"].(float64)
		thread := fmt.Sprintf("telegram--100-topic-%d", int64(topic))
		g.wait("its header", from, func(c flowCall) bool { return c.Method == "pinChatMessage" })
		route, err := g.eng.ResolveThread(context.Background(), -100, ptrTo(int64(topic)))
		if err != nil || !route.PendingTopicTitle {
			t.Fatalf("the title is pending: %+v %v", route, err)
		}
		g.say(int64(topic), "first synthetic question")
		rename := g.wait("the title", from, func(c flowCall) bool { return c.Method == "editForumTopic" && flowTopic(c) == topic })
		g.wait("the answer", from, sendIn(topic, "MOCK-"))
		g.waitSettled(thread)
		if route, err := g.eng.ResolveThread(context.Background(), -100, ptrTo(int64(topic))); err != nil || route.PendingTopicTitle {
			t.Fatalf("the pending title is cleared: %+v %v", route, err)
		}
		t.Logf("titled %q", rename.Body["name"])
	})

	t.Run("launcher mode opens a topic on that mode", func(t *testing.T) {
		from := g.mark()
		g.say(0, "/launcher")
		card := g.wait("the launcher", from, func(c flowCall) bool {
			_, ok := g.tg.button(c.ID, "swift")
			return c.Method == "sendMessage" && ok
		})
		g.tap(card.ID, 0, "swift")
		created := g.wait("the launched topic", from, func(c flowCall) bool { return c.Method == "createForumTopic" })
		topic := created.Body["topic"].(float64)
		thread := fmt.Sprintf("telegram--100-topic-%d", int64(topic))
		g.wait("its header", from, func(c flowCall) bool { return c.Method == "pinChatMessage" })
		var status struct {
			Mode        string `json:"mode"`
			ModelSource string `json:"model_source"`
		}
		if err := g.eng.Catalog(context.Background(), engine.Catalog{Kind: engine.CatalogThreadStatus, Root: g.root, ThreadID: &thread}, &status); err != nil ||
			status.Mode != "mode:swift" || status.ModelSource != "topic" {
			t.Fatalf("the topic runs on the mode: %+v %v", status, err)
		}
		g.say(int64(topic), "hello swift")
		g.wait("the answer", from, sendIn(topic, "MOCK-"))
		g.waitSettled(thread)
	})

	t.Run("a new private thread becomes an orchestrator", func(t *testing.T) {
		from := g.mark()
		g.sayPrivate(900, "hello from a private thread")
		g.wait("the header", from, func(c flowCall) bool {
			return c.Method == "sendMessage" && c.Body["chat_id"] == float64(7) && flowTopic(c) == 900 && strings.Contains(flowText(c), "<b>")
		})
		g.wait("the answer", from, func(c flowCall) bool {
			return c.Method == "sendMessage" && c.Body["chat_id"] == float64(7) && flowTopic(c) == 900 && strings.HasPrefix(flowText(c), "MOCK-")
		})
		g.wait("the title", from, func(c flowCall) bool {
			return c.Method == "editForumTopic" && c.Body["chat_id"] == float64(7) && flowTopic(c) == 900
		})
		thread := "telegram-7-topic-900"
		g.waitSettled(thread)
		route, err := g.eng.ResolveThread(context.Background(), 7, ptrTo(int64(900)))
		if err != nil || route.Kind != engine.KindProfile || route.Profile == nil || *route.Profile != "orchestrator" {
			t.Fatalf("the thread is an orchestrator: %+v %v", route, err)
		}
	})

	t.Run("orchestrator chat opens orchestrator topics", func(t *testing.T) {
		isOrchestrator := func(topic float64) {
			t.Helper()
			thread := fmt.Sprintf("telegram--200-topic-%d", int64(topic))
			g.waitSettled(thread)
			route, err := g.eng.ResolveThread(context.Background(), -200, ptrTo(int64(topic)))
			if err != nil || route.Kind != engine.KindProfile || route.Profile == nil || *route.Profile != "orchestrator" {
				t.Fatalf("%s is an orchestrator: %+v %v", thread, route, err)
			}
		}
		from := g.mark()
		g.sayIn(-200, 0, "coordinate the synthetic work")
		created := g.wait("the topic", from, func(c flowCall) bool { return c.Method == "createForumTopic" })
		if created.Body["name"] != "🎛️ coordinate the synthetic work" {
			t.Fatalf("name %v", created.Body["name"])
		}
		topic := created.Body["topic"].(float64)
		g.wait("the answer", from, func(c flowCall) bool {
			return c.Method == "sendMessage" && c.Body["chat_id"] == float64(-200) && flowTopic(c) == topic && strings.HasPrefix(flowText(c), "MOCK-")
		})
		rename := g.wait("the title", from, func(c flowCall) bool { return c.Method == "editForumTopic" && flowTopic(c) == topic })
		if name, _ := rename.Body["name"].(string); !strings.HasPrefix(name, "🎛️ MOCK-") {
			t.Fatalf("an orchestrator title keeps its mark: %q", name)
		}
		isOrchestrator(topic)

		from = g.mark()
		g.sayIn(-200, 0, "/launcher")
		card := g.wait("the launcher", from, func(c flowCall) bool {
			_, ok := g.tg.button(c.ID, "swift")
			return c.Method == "sendMessage" && c.Body["chat_id"] == float64(-200) && ok
		})
		g.tapIn(-200, card.ID, 0, "swift")
		launched := g.wait("the launched orchestrator", from, func(c flowCall) bool { return c.Method == "createForumTopic" })
		lt := launched.Body["topic"].(float64)
		g.wait("its header", from, func(c flowCall) bool { return c.Method == "pinChatMessage" })
		// Setup checks the profile against the chat root but stores no
		// root; the first turn records it, as in the Rust bot.
		metaPath := filepath.Join(g.home, "threads", fmt.Sprintf("telegram--200-topic-%d.jsonl", int64(lt)))
		if b, err := os.ReadFile(metaPath); err != nil || strings.Contains(string(b), `"root_path"`) {
			t.Fatalf("no root before the first turn: %s %v", b, err)
		}
		g.sayIn(-200, int64(lt), "hello orchestrator")
		g.wait("the answer", from, func(c flowCall) bool {
			return c.Method == "sendMessage" && c.Body["chat_id"] == float64(-200) && flowTopic(c) == lt && strings.HasPrefix(flowText(c), "MOCK-")
		})
		isOrchestrator(lt)
		if b, _ := os.ReadFile(metaPath); !strings.Contains(string(b), `"root_path"`) {
			t.Fatalf("the first turn records the root: %s", b)
		}
	})

	t.Run("general model picker writes the chat default", func(t *testing.T) {
		from := g.mark()
		g.say(0, "/model")
		card := g.wait("the chat model picker", from, sendIn(0, "Current model"))
		g.tap(card.ID, 0, "Anthropic")
		g.wait("the models", from, func(c flowCall) bool {
			_, ok := g.tg.button(card.ID, "claude-opus-5-5")
			return c.Method == "editMessageText" && ok
		})
		g.tap(card.ID, 0, "claude-opus-5-5")
		g.wait("the thinking levels", from, func(c flowCall) bool {
			_, ok := g.tg.button(card.ID, "high")
			return c.Method == "editMessageText" && ok
		})
		picked := g.mark()
		g.tap(card.ID, 0, "high")
		done := g.wait("the chat default set", picked, func(c flowCall) bool {
			return c.Method == "editMessageText" && strings.Contains(flowText(c), "claude-opus-5-5")
		})
		t.Logf("chat default set: %q", flowText(done))
		b, err := os.ReadFile(filepath.Join(g.root, ".zdx", "config.toml"))
		if err != nil || !strings.Contains(string(b), "claude-opus-5-5") {
			t.Fatalf("the workspace config of the temp root holds the default: %s %v", b, err)
		}
	})
}

func ptrTo[T any](v T) *T { return &v }
