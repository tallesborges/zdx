package gateway

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"net"
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

const e2eModel = "anthropic:claude-sonnet-5"

func e2eSSE(text string) string {
	return `event: message_start
data: {"type":"message_start","message":{"id":"msg_1","type":"message","role":"assistant","content":[],"model":"claude-sonnet-5","stop_reason":null,"stop_sequence":null,"usage":{"input_tokens":10,"output_tokens":1}}}

event: content_block_start
data: {"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}

event: content_block_delta
data: {"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"` + text + `"}}

event: content_block_stop
data: {"type":"content_block_stop","index":0}

event: message_delta
data: {"type":"message_delta","delta":{"stop_reason":"end_turn","stop_sequence":null},"usage":{"output_tokens":5}}

event: message_stop
data: {"type":"message_stop"}

`
}

// e2eTelegram is a fake Bot API: it hands out queued updates once and
// records every other call.
type e2eTelegram struct {
	mu      sync.Mutex
	updates []string
	calls   []map[string]any
	nextID  int64
	srv     *httptest.Server
	polls   int
}

func (f *e2eTelegram) push(update string) {
	f.mu.Lock()
	defer f.mu.Unlock()
	f.updates = append(f.updates, update)
}

func (f *e2eTelegram) sent(pred func(map[string]any) bool) bool {
	f.mu.Lock()
	defer f.mu.Unlock()
	for _, c := range f.calls {
		if pred(c) {
			return true
		}
	}
	return false
}

func textIn(topic float64, prefix string) func(map[string]any) bool {
	return func(c map[string]any) bool {
		s, _ := c["text"].(string)
		return c["method"] == "sendMessage" && c["message_thread_id"] == topic && strings.HasPrefix(s, prefix)
	}
}

// The whole gateway against a real `zdx engine serve` (temp home,
// mock model endpoint, fake Bot API), wired as serve wires it: a polled
// message runs a followed turn and its reply goes out; Mini App feedback
// posted over HTTP gets the chat notice, and its unfollowed turn is
// delivered by the renderer bridge.
func TestRealGatewayEndToEnd(t *testing.T) {
	bin := sandbox.RustBinary(t)
	home, err := os.MkdirTemp("", "zg")
	if err != nil {
		t.Fatal(err)
	}
	defer os.RemoveAll(home)
	root := t.TempDir()
	cfg := fmt.Sprintf("model = %q\n[telegram]\nallowlist_user_ids = [7]\nallowlist_chat_ids = [-100]\n", e2eModel)
	if err := os.WriteFile(filepath.Join(home, "config.toml"), []byte(cfg), 0o600); err != nil {
		t.Fatal(err)
	}
	var mu sync.Mutex
	var prompts []string
	provider := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		b, _ := io.ReadAll(r.Body)
		mu.Lock()
		prompts = append(prompts, string(b))
		n := len(prompts)
		mu.Unlock()
		w.Header().Set("Content-Type", "text/event-stream")
		io.WriteString(w, e2eSSE(fmt.Sprintf("E2E-ANSWER-%d", n)))
	}))
	defer provider.Close()

	logPath := filepath.Join(home, "service.log")
	logFile, _ := os.Create(logPath)
	defer logFile.Close()
	serviceLog := func() string { b, _ := os.ReadFile(logPath); return string(b) }
	socket := engine.SocketPath(home)
	// startService runs the service and returns its stop.
	startService := func() (stop func()) {
		cmd := exec.Command(bin, "--root", root, "engine", "serve")
		cmd.Dir = root
		cmd.Env = []string{"PATH=" + os.Getenv("PATH"), "TMPDIR=" + os.Getenv("TMPDIR"), "HOME=" + home, "ZDX_HOME=" + home,
			"ANTHROPIC_API_KEY=test-api-key", "ANTHROPIC_BASE_URL=" + provider.URL}
		cmd.Stdout, cmd.Stderr = logFile, logFile
		if err := cmd.Start(); err != nil {
			t.Fatal(err)
		}
		exited := make(chan error, 1)
		go func() { exited <- cmd.Wait() }()
		var once sync.Once
		return func() {
			once.Do(func() {
				_ = cmd.Process.Signal(syscall.SIGTERM)
				select {
				case <-exited:
				case <-time.After(5 * time.Second):
					_ = cmd.Process.Kill()
					<-exited
				}
			})
		}
	}
	// Ready when a status call succeeds: Rust creates the socket file
	// before its listener accepts.
	waitService := func() {
		for start := time.Now(); ; time.Sleep(20 * time.Millisecond) {
			_, err := engine.NewClient(socket).StatusReport(context.Background(), nil)
			if err == nil {
				return
			}
			if !errors.Is(err, engine.ErrRequestNotSent) || time.Since(start) > 30*time.Second {
				t.Fatalf("service not ready: %v\n%s", err, serviceLog())
			}
		}
	}
	stopService := startService()
	defer func() { stopService() }()
	waitService()
	if err := engine.NewClient(socket).RequireAll(context.Background(), append(TurnCapabilities, engine.CapFeedbackSubmit, engine.CapArtifactViews)...); err != nil {
		t.Fatalf("binary %s lacks capabilities: %v", bin, err)
	}

	tg := &e2eTelegram{nextID: 500}
	tg.srv = httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		method := r.URL.Path[strings.LastIndex(r.URL.Path, "/")+1:]
		if method == "getUpdates" {
			tg.mu.Lock()
			tg.polls++
			out := tg.updates
			tg.updates = nil
			tg.mu.Unlock()
			if len(out) == 0 {
				select {
				case <-r.Context().Done():
				case <-time.After(20 * time.Millisecond):
				}
			}
			io.WriteString(w, `{"ok":true,"result":[`+strings.Join(out, ",")+`]}`)
			return
		}
		raw, _ := io.ReadAll(r.Body)
		body := map[string]any{}
		_ = json.Unmarshal(raw, &body)
		body["method"] = method
		tg.mu.Lock()
		tg.nextID++
		id := tg.nextID
		tg.calls = append(tg.calls, body)
		tg.mu.Unlock()
		if method == "sendMessage" {
			fmt.Fprintf(w, `{"ok":true,"result":{"message_id":%d,"chat":{"id":-100,"type":"supergroup"},"date":1}}`, id)
			return
		}
		io.WriteString(w, `{"ok":true,"result":true}`)
	}))
	defer tg.srv.Close()

	settings := Settings{BotToken: token, AllowlistUserIDs: []int64{7}, AllowlistChatIDs: []int64{-100}, Root: root}
	settings.Server.Enabled, settings.Server.Port = true, 1
	addr := make(chan net.Addr, 1)
	ctx, cancel := context.WithCancel(context.Background())
	done := make(chan error, 1)
	go func() {
		done <- Run(ctx, Config{
			Settings: settings, Socket: socket, Home: home, Log: quiet, DrainTimeout: 20 * time.Second,
			Telegram: telegram.Options{BaseURL: tg.srv.URL, Logger: quiet}, HTTPAddr: "127.0.0.1:0",
			Ready: func(a net.Addr) { addr <- a },
			Build: func(d Deps) (Components, error) {
				return BotComponents(d.Work, d, settings, ServiceParts(d, settings)), nil
			},
		})
	}()
	base := "http://" + (<-addr).String()
	wait := func(what string, pred func() bool) {
		t.Helper()
		for deadline := time.Now().Add(45 * time.Second); !pred(); time.Sleep(50 * time.Millisecond) {
			if time.Now().After(deadline) {
				tg.mu.Lock()
				calls := fmt.Sprint(tg.calls)
				tg.mu.Unlock()
				t.Fatalf("timed out waiting for %s\ncalls: %s\nservice log:\n%s", what, calls, serviceLog())
			}
		}
	}

	tg.push(`{"update_id":1,"message":{"message_id":11,"message_thread_id":5,"is_topic_message":true,"chat":{"id":-100,"type":"supergroup","is_forum":true},"from":{"id":7,"is_bot":false},"text":"hello e2e"}}`)
	wait("the reply", func() bool { return tg.sent(textIn(5, "E2E-ANSWER-1")) })

	thread := "telegram--100-topic-5"
	art := filepath.Join(home, "artifacts", "threads", thread)
	os.MkdirAll(art, 0o700)
	if err := os.WriteFile(filepath.Join(art, "report.html"), []byte("<html><body><h1>Hi</h1></body></html>"), 0o600); err != nil {
		t.Fatal(err)
	}
	get := func(path string) (int, []byte) {
		req, _ := http.NewRequest(http.MethodGet, base+path, nil)
		req.Header.Set("Authorization", "tma "+initData(7, time.Now()))
		resp, err := http.DefaultClient.Do(req)
		if err != nil {
			t.Fatal(err)
		}
		defer resp.Body.Close()
		b, _ := io.ReadAll(resp.Body)
		return resp.StatusCode, b
	}
	status, vb := get("/api/threads/" + thread + "/artifacts/version?path=report.html")
	var version struct {
		SHA256 string `json:"sha256"`
	}
	if status != 200 || json.Unmarshal(vb, &version) != nil || version.SHA256 == "" {
		t.Fatalf("version %d %s", status, vb)
	}
	submission, _ := json.Marshal(map[string]any{
		"submission_id": "sub-e2e-0001", "artifact_path": "report.html", "artifact_sha256": version.SHA256, "note": "/new tweak it",
		"annotations": []any{map[string]any{"intent": "change", "comment": "Make the heading blue", "target": map[string]any{"kind": "page"}}},
	})
	post := func() (int, []byte) {
		req, _ := http.NewRequest(http.MethodPost, base+"/api/threads/"+thread+"/artifacts/feedback", strings.NewReader(string(submission)))
		req.Header.Set("Authorization", "tma "+initData(7, time.Now()))
		req.Header.Set("Content-Type", "application/json")
		resp, err := http.DefaultClient.Do(req)
		if err != nil {
			t.Fatal(err)
		}
		defer resp.Body.Close()
		b, _ := io.ReadAll(resp.Body)
		return resp.StatusCode, b
	}
	if status, b := post(); status >= 300 || !strings.Contains(string(b), "queued") {
		t.Fatalf("feedback %d %s", status, b)
	}
	wait("the feedback notice", func() bool {
		return tg.sent(textIn(5, "📝 Feedback on <b>report.html</b> from the Mini App (1 note)."))
	})
	wait("the feedback answer", func() bool { return tg.sent(textIn(5, "E2E-ANSWER-2")) })
	if status, b := post(); status >= 300 || !strings.Contains(string(b), "duplicate") {
		t.Fatalf("again %d %s", status, b)
	}
	mu.Lock()
	last := prompts[len(prompts)-1]
	mu.Unlock()
	if !strings.Contains(last, "Make the heading blue") || !strings.Contains(last, "/new tweak it") {
		t.Fatalf("the model sees the feedback as text: %.500s", last)
	}
	notices := 0
	tg.mu.Lock()
	for _, c := range tg.calls {
		if s, _ := c["text"].(string); strings.HasPrefix(s, "📝 Feedback on") {
			notices++
		}
	}
	tg.mu.Unlock()
	if notices != 1 {
		t.Fatalf("one notice; a duplicate posts none: %d", notices)
	}

	// While the service is down a polled message is held: no further poll
	// confirms it, nothing is answered; once the service runs again it is
	// handled once.
	stopService()
	tg.mu.Lock()
	before := tg.polls
	tg.mu.Unlock()
	tg.push(`{"update_id":2,"message":{"message_id":12,"message_thread_id":6,"is_topic_message":true,"chat":{"id":-100,"type":"supergroup","is_forum":true},"from":{"id":7,"is_bot":false},"text":"while down"}}`)
	wait("the held batch", func() bool { tg.mu.Lock(); defer tg.mu.Unlock(); return len(tg.updates) == 0 })
	time.Sleep(1500 * time.Millisecond)
	tg.mu.Lock()
	held := tg.polls - before
	tg.mu.Unlock()
	if held > 2 || tg.sent(func(c map[string]any) bool { return c["message_thread_id"] == float64(6) }) {
		t.Fatalf("a held batch is neither confirmed nor answered: %d polls", held)
	}
	stopService = startService()
	waitService()
	wait("the held message's reply", func() bool { return tg.sent(textIn(6, "E2E-ANSWER-3")) })
	errorNotice := tg.sent(func(c map[string]any) bool {
		s, _ := c["text"].(string)
		return c["message_thread_id"] == float64(6) && strings.Contains(s, "not running")
	})
	if errorNotice {
		t.Fatal("a held message gets no error notice")
	}

	cancel()
	if err := <-done; err != nil {
		t.Fatal(err)
	}
	b, _ := os.ReadFile(filepath.Join(home, "threads", thread+".jsonl"))
	if n := strings.Count(string(b), `"type":"delivery"`); n != 4 {
		t.Fatalf("both replies record pending and final: %d", n)
	}
}
