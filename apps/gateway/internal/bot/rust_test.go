package bot

import (
	"context"
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
	"github.com/tallesborges/zdx/apps/gateway/internal/render"
	"github.com/tallesborges/zdx/apps/gateway/internal/sandbox"
)

// rustBinary is the zdx binary for the opt-in check against the real Rust
// service. Unset skips it; the test never builds Rust.
const rustBinaryEnv = "ZDX_GATEWAY_RUST_ZDX"

const mockModel = "anthropic:claude-sonnet-5"

func sse(text string) string {
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

// realCaps are the capabilities the full plain-text slice needs.
var realCaps = []string{
	engine.CapOrdinarySubmit, engine.CapResolveThread, engine.CapComposeUserText,
	engine.CapRecordDelivery, engine.CapReplyParts,
}

// pinnedModel sets the model of a thread the real service creates, so it
// answers from the mock provider whatever config the machine has. Rust
// accepts a model only when it creates the thread, so later submits to an
// existing thread send none.
type pinnedModel struct{ *engine.Client }

func (p pinnedModel) pin(ctx context.Context, s engine.Submit) engine.Submit {
	if s.ThreadID != nil && s.Route != nil {
		route, err := p.Client.ResolveThread(ctx, s.Route.Chat, s.Route.Topic)
		if err == nil && route.Exists {
			return s
		}
	}
	m := mockModel
	s.Model = &m
	return s
}

func (p pinnedModel) SubmitOrdinary(ctx context.Context, s engine.Submit) (*engine.Submission, error) {
	return p.Client.SubmitOrdinary(ctx, p.pin(ctx, s))
}

func (p pinnedModel) SubmitFollow(ctx context.Context, s engine.Submit) (*engine.Submission, error) {
	return p.Client.SubmitFollow(ctx, p.pin(ctx, s))
}

// TestRealRustPlainTextSlice runs one plain-text message through the Go
// intake, presenter and delivery against a real `zdx orchestrator serve`
// (temp home, mock Anthropic endpoint, fake Bot API). Every engine call is
// real: resolve_thread, compose_user_text, ordinary submit, the followed
// turn, reply_parts, record_delivery, release. Telegram is fake, and only
// plain text is covered (no media, commands, or Mini App).
func TestRealRustPlainTextSlice(t *testing.T) {
	bin := sandbox.RustBinary(t)
	home, err := os.MkdirTemp("", "zr")
	if err != nil {
		t.Fatal(err)
	}
	defer os.RemoveAll(home)
	root := t.TempDir()

	var mu sync.Mutex
	var modelRequests []string
	provider := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		body, _ := io.ReadAll(r.Body)
		mu.Lock()
		modelRequests = append(modelRequests, string(body))
		mu.Unlock()
		w.Header().Set("Content-Type", "text/event-stream")
		io.WriteString(w, sse(`REAL-RUST-ANSWER with **bold**\n\n<followups><followup>Run the tests</followup></followups>`))
	}))
	defer provider.Close()

	logPath := filepath.Join(home, "service.log")
	logFile, _ := os.Create(logPath)
	cmd := exec.Command(bin, "--root", root, "orchestrator", "serve")
	cmd.Dir = root
	cmd.Env = childEnv(os.Environ(), home, "ANTHROPIC_API_KEY=test-api-key", "ANTHROPIC_BASE_URL="+provider.URL)
	cmd.Stdout, cmd.Stderr = logFile, logFile
	start := time.Now()
	if err := cmd.Start(); err != nil {
		t.Fatal(err)
	}
	exited := make(chan error, 1)
	go func() { exited <- cmd.Wait() }()
	defer stopService(t, cmd, exited)
	dumpLog := func() string { b, _ := os.ReadFile(logPath); return string(b) }
	socket := engine.SocketPath(home)
	waitListening(t, socket, exited, dumpLog)
	listened := time.Since(start)

	eng := engine.NewClient(socket)
	report, err := eng.StatusReport(context.Background(), nil)
	if err != nil {
		t.Fatal(err)
	}
	if err := eng.RequireAll(context.Background(), realCaps...); err != nil {
		t.Fatalf("stale binary %s (rebuild it): %v; it reports %v", bin, err, report.Capabilities)
	}

	tg := newFakeTG(t)
	client := tg.client()
	client.Repair = render.Sanitize
	threadID := "telegram--100-topic-5"
	pres := &Presenter{
		Telegram: client, Planner: ServicePlanner{Engine: eng}, Cancels: &Cancels{}, Log: quiet,
		Deliverer: &Deliverer{Telegram: client, Store: eng, Followups: &Followups{}, Log: quiet},
	}
	// With hold tokens the follower runs as in production: its submit's
	// token on every record and the release, opted in to replay.
	holds := report.Has(engine.CapHoldTokens)
	turnCaps := []string{engine.CapRecordDelivery, engine.CapReplyParts}
	if holds {
		turnCaps = append(turnCaps, engine.CapHoldTokens)
	}
	h := &Handler{
		Telegram: client, Engine: eng, Resolver: ServiceResolver{Engine: eng}, Composer: eng,
		Turns: pinnedModel{eng}, Queues: NewTopicQueues(context.Background()), Presenter: pres, Acks: &SteerAcks{},
		Artifacts: UnavailableArtifacts{}, RootForChat: func(int64) string { return root }, Log: quiet,
		TurnReady: func(ctx context.Context) error {
			return eng.RequireAll(ctx, turnCaps...)
		},
		Replay: holds,
	}
	dispatched := time.Now()
	h.Dispatch(context.Background(), userMessage(t, 42, "hello from go"))
	var reply map[string]any
	tg.waitWithin(t, 20*time.Second, "the reply", func() bool {
		for _, c := range tg.of("sendMessage") {
			if text, _ := c.body["text"].(string); strings.Contains(text, "REAL-RUST-ANSWER") {
				reply = c.body
				return true
			}
		}
		return false
	})
	replied := time.Since(dispatched)
	h.Wait()

	if text := reply["text"].(string); !strings.Contains(text, "<b>bold</b>") || strings.Contains(text, "followups") {
		t.Fatalf("reply parts must strip follow-ups and render Markdown: %q", text)
	}
	if kb, _ := reply["reply_markup"].(map[string]any); kb == nil || !strings.Contains(fmt.Sprint(kb), "Run the tests") {
		t.Fatalf("the reply carries the follow-up buttons: %v", reply["reply_markup"])
	}

	deadline := time.Now().Add(10 * time.Second)
	for {
		report, err := eng.StatusReport(context.Background(), &threadID)
		if err != nil {
			t.Fatal(err)
		}
		if len(report.Threads) == 1 && report.Threads[0].State == "idle" && report.Threads[0].Settled {
			break
		}
		if time.Now().After(deadline) {
			t.Fatalf("thread not released: %+v\n%s", report.Threads, dumpLog())
		}
		time.Sleep(100 * time.Millisecond)
	}
	transcript, err := os.ReadFile(filepath.Join(home, "threads", threadID+".jsonl"))
	if err != nil {
		t.Fatal(err)
	}
	var deliveries []string
	for _, line := range strings.Split(string(transcript), "\n") {
		if strings.Contains(line, `"surface":"telegram"`) {
			deliveries = append(deliveries, line)
		}
	}
	if !strings.Contains(string(transcript), "hello from go") || len(deliveries) != 2 ||
		!strings.Contains(deliveries[0], `"pending"`) || !strings.Contains(deliveries[1], `"sent"`) {
		t.Fatalf("the service must persist the turn and both delivery records:\n%s", transcript)
	}
	t.Logf("reply text: %q", reply["text"])
	t.Logf("reply keyboard: %v", reply["reply_markup"])
	t.Logf("persisted pending: %s", deliveries[0])
	t.Logf("persisted final:   %s", deliveries[1])
	mu.Lock()
	defer mu.Unlock()
	if len(modelRequests) != 1 || !strings.Contains(modelRequests[0], "hello from go") {
		t.Fatalf("model requests: %d", len(modelRequests))
	}
	t.Logf("real Rust plain-text slice: protocol %d; listen %v, reply %v, total %v",
		report.Version(), listened.Round(time.Millisecond), replied.Round(time.Millisecond), time.Since(start).Round(time.Millisecond))
}

// waitListening waits until the service at socket answers a status call.
// The socket file alone is not enough: Rust creates it before the listener
// accepts, and a connect in between is refused (seen in about 1 of 20
// starts under parallel load).
func waitListening(t *testing.T, socket string, exited <-chan error, log func() string) {
	t.Helper()
	c := engine.NewClient(socket)
	deadline := time.Now().Add(30 * time.Second)
	for {
		ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
		_, err := c.StatusReport(ctx, nil)
		cancel()
		if err == nil {
			return
		}
		if !errors.Is(err, engine.ErrRequestNotSent) {
			t.Fatalf("service status: %v\n%s", err, log())
		}
		select {
		case err := <-exited:
			t.Fatalf("service exited early (%v):\n%s", err, log())
		case <-time.After(20 * time.Millisecond):
		}
		if time.Now().After(deadline) {
			t.Fatalf("service never listened: %v\n%s", err, log())
		}
	}
}

// stopService asks the service to stop, then kills it after 5 seconds.
func stopService(t *testing.T, cmd *exec.Cmd, exited <-chan error) {
	_ = cmd.Process.Signal(syscall.SIGTERM)
	select {
	case <-exited:
	case <-time.After(5 * time.Second):
		t.Log("service did not stop on SIGTERM; killing it")
		_ = cmd.Process.Kill()
		<-exited
	}
}

// childEnvKeys are the only parent variables the child service gets.
var childEnvKeys = map[string]bool{"PATH": true, "TMPDIR": true, "LANG": true}

// childEnv builds the child service environment from an allowlist of parent
// variables (PATH, TMPDIR, LANG, LC_*), then the test's own settings. HOME
// and ZDX_HOME point at the temp home, so no real config, credentials,
// threads, token or provider can be reached. Settings come last, so a
// parent value can never win.
func childEnv(parent []string, home string, settings ...string) []string {
	var out []string
	for _, kv := range parent {
		key, _, _ := strings.Cut(kv, "=")
		if childEnvKeys[key] || strings.HasPrefix(key, "LC_") {
			out = append(out, kv)
		}
	}
	out = append(out, "HOME="+home, "ZDX_HOME="+home)
	return append(out, settings...)
}

func TestChildEnvKeepsOnlyTheAllowlistAndTestSettings(t *testing.T) {
	parent := []string{
		"PATH=/bin", "TMPDIR=/tmp/x", "LANG=C", "LC_ALL=C",
		"HOME=/Users/real", "ZDX_HOME=/Users/real/.zdx", "ZDX_THREAD_ID=t", "ZDX_ARTIFACT_DIR=/a", "ZDX_MEMORY_ROOT=/m",
		"ZDX_TELEGRAM_BOT_TOKEN=1:SECRET", "TELEGRAM_BOT_TOKEN=1:SECRET", "ANTHROPIC_API_KEY=real", "ANTHROPIC_BASE_URL=https://api.anthropic.com",
		"OPENAI_API_KEY=real", "GH_TOKEN=real", "AWS_SECRET_ACCESS_KEY=real", "XDG_CONFIG_HOME=/Users/real/.config", "SSH_AUTH_SOCK=/s",
	}
	env := childEnv(parent, "/tmp/home", "ANTHROPIC_API_KEY=test-api-key", "ANTHROPIC_BASE_URL=http://127.0.0.1:1")
	got := map[string][]string{}
	for _, kv := range env {
		k, v, _ := strings.Cut(kv, "=")
		got[k] = append(got[k], v)
	}
	want := map[string]string{
		"PATH": "/bin", "TMPDIR": "/tmp/x", "LANG": "C", "LC_ALL": "C",
		"HOME": "/tmp/home", "ZDX_HOME": "/tmp/home",
		"ANTHROPIC_API_KEY": "test-api-key", "ANTHROPIC_BASE_URL": "http://127.0.0.1:1",
	}
	if len(got) != len(want) {
		t.Fatalf("unexpected keys: %v", got)
	}
	for k, v := range want {
		if vs := got[k]; len(vs) != 1 || vs[0] != v {
			t.Errorf("%s = %v, want exactly %q", k, vs, v)
		}
	}
	for _, kv := range env {
		if strings.Contains(kv, "SECRET") || strings.Contains(kv, "/Users/real") || strings.Contains(kv, "anthropic.com") {
			t.Errorf("leaked %q", kv)
		}
	}
}
