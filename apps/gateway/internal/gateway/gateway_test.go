package gateway

import (
	"bufio"
	"bytes"
	"context"
	"crypto/hmac"
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"log/slog"
	"net"
	"net/http"
	"net/http/httptest"
	"net/url"
	"os"
	"os/exec"
	"path/filepath"
	"slices"
	"sort"
	"strconv"
	"strings"
	"sync"
	"testing"
	"time"

	"github.com/tallesborges/zdx/apps/gateway/internal/bot"
	"github.com/tallesborges/zdx/apps/gateway/internal/commands"
	"github.com/tallesborges/zdx/apps/gateway/internal/engine"
	"github.com/tallesborges/zdx/apps/gateway/internal/miniapp"
	"github.com/tallesborges/zdx/apps/gateway/internal/telegram"
	"github.com/tallesborges/zdx/apps/shared/gitview"
	"github.com/tallesborges/zdx/apps/shared/quota"
	"github.com/tallesborges/zdx/apps/shared/version"
)

const token = "4242:SECRET-gateway-test"

var quiet = slog.New(slog.DiscardHandler)

func TestSettingsValidationAndNoTokenInString(t *testing.T) {
	s, err := ReadSettings(strings.NewReader(`{"bot_token":"` + token + `","allowlist_user_ids":[7],"allowlist_chat_ids":[-100,-200],"server":{"enabled":true,"port":4141,"mini_app_url":"https://t.me/x/app"},"root":"/r","profiles":[{"name":"p","chat_id":-200,"cwd":"/p","orchestrator":false}]}`))
	if err != nil {
		t.Fatal(err)
	}
	if strings.Contains(s.String(), "SECRET") || !strings.Contains(s.String(), "4242") {
		t.Fatalf("String must show only the bot id: %s", s)
	}
	if s.RootForChat(-200) != "/p" || s.RootForChat(-100) != "/r" || s.MiniAppURL() != "https://t.me/x/app" {
		t.Fatalf("unexpected roots or url: %+v", s)
	}
	for _, bad := range []string{
		`{"allowlist_user_ids":[7],"root":"/r"}`,
		`{"bot_token":"x","allowlist_user_ids":[],"root":"/r"}`,
		`{"bot_token":"x","allowlist_user_ids":[7]}`,
		`{"bot_token":"x","allowlist_user_ids":[7],"root":"/r","server":{"enabled":true}}`,
		`{"bot_token":"x","allowlist_user_ids":[7],"root":"/r","surprise":1}`,
	} {
		if _, err := ReadSettings(strings.NewReader(bad)); err == nil {
			t.Errorf("%s must be refused", bad)
		}
	}
}

// fakeService is a synthetic engine service.
type fakeService struct {
	socket      string
	mu          sync.Mutex
	caps        []string
	submissions []string
	releases    []string
	feedbackErr map[string]any
	ops         []string
	// quietSubscribe streams no turn on the renderer subscription.
	quietSubscribe bool
	// recordHolders are turn_id/holder of each record_delivery.
	recordHolders []string
	// conflictSubscribe refuses renderer subscriptions (another renderer).
	conflictSubscribe bool
	// holdFinal, when set, delays the submitted turn's final answer until
	// it is closed.
	holdFinal chan struct{}
	// views are the raw view requests; viewReply answers them.
	views     []string
	viewReply func(op string) map[string]any
	// viewsRoot is the status report's views_root, when set.
	viewsRoot string
}

func startService(t *testing.T, f *fakeService) {
	dir, err := os.MkdirTemp("", "gwg")
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { os.RemoveAll(dir) })
	f.socket = filepath.Join(dir, "o.sock")
	ln, err := net.Listen("unix", f.socket)
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { ln.Close() })
	write := func(c net.Conn, v any) {
		b, _ := json.Marshal(v)
		c.Write(append(b, '\n'))
	}
	go func() {
		for {
			conn, err := ln.Accept()
			if err != nil {
				return
			}
			go func() {
				defer conn.Close()
				line, err := bufio.NewReader(conn).ReadBytes('\n')
				if err != nil {
					return
				}
				var req map[string]json.RawMessage
				json.Unmarshal(line, &req)
				var op string
				json.Unmarshal(req["op"], &op)
				f.mu.Lock()
				defer f.mu.Unlock()
				switch op {
				case "status":
					report := map[string]any{"protocol": 2, "capabilities": f.caps, "settled": true, "threads": []any{}}
					if f.viewsRoot != "" {
						report["views_root"] = f.viewsRoot
					}
					write(conn, map[string]any{"reply": "status", "report": report})
				case "subscribe":
					if f.conflictSubscribe {
						f.ops = append(f.ops, "subscribe:conflict")
						write(conn, map[string]any{"reply": "error", "code": "conflict", "message": "another renderer is subscribed"})
						return
					}
					if string(req["replay"]) == "true" {
						f.ops = append(f.ops, "subscribe:replay")
						write(conn, map[string]any{"reply": "subscribed", "holder": "RENDERER"})
					}
					route := map[string]any{"chat": -100, "topic": 5}
					script := []map[string]any{
						{"type": "turn_started", "thread_id": "telegram--100-topic-5", "turn_id": 8, "kind": "worker_update", "inputs": []any{}, "route": route, "followed": false},
						{"type": "agent", "thread_id": "telegram--100-topic-5", "turn_id": 8, "event": map[string]any{"type": "turn_finished", "status": map[string]any{"status": "completed"}, "final_text": "ok", "messages": []any{}}},
						{"type": "turn_ended", "thread_id": "telegram--100-topic-5", "turn_id": 8, "held": true},
					}
					if f.quietSubscribe {
						script = nil
					}
					for _, e := range script {
						write(conn, map[string]any{"reply": "event", "event": e})
					}
					f.mu.Unlock()
					// Hold the subscription open until the client goes away.
					io.Copy(io.Discard, conn)
					f.mu.Lock()
				case "release":
					f.releases = append(f.releases, string(req["thread_id"])+"/"+string(req["turn_id"])+"/"+string(req["holder"]))
					f.ops = append(f.ops, "release:"+string(req["turn_id"]))
					write(conn, map[string]any{"reply": "ok"})
				case "compose_user_text":
					var text string
					json.Unmarshal(req["text"], &text)
					write(conn, map[string]any{"reply": "text", "text": text})
				case "record_delivery":
					var rec struct {
						ID    string `json:"id"`
						Items []struct {
							Status string `json:"status"`
						} `json:"items"`
					}
					json.Unmarshal(req["record"], &rec)
					var holder string
					json.Unmarshal(req["holder"], &holder)
					f.ops = append(f.ops, "record:"+rec.ID+":"+rec.Items[0].Status)
					f.recordHolders = append(f.recordHolders, string(req["turn_id"])+"/"+holder)
					write(conn, map[string]any{"reply": "delivery_recorded"})
				case "submit":
					var ordinary bool
					json.Unmarshal(req["ordinary"], &ordinary)
					f.ops = append(f.ops, fmt.Sprintf("submit:ordinary=%v", ordinary))
					th := "telegram--100-topic-5"
					submitted := map[string]any{"reply": "submitted", "thread_id": th, "input_id": 21, "queued_ahead": 0}
					if string(req["replay"]) == "true" {
						submitted["holder"] = "FOLLOWER"
					}
					write(conn, submitted)
					write(conn, map[string]any{"reply": "event", "event": map[string]any{"type": "turn_started", "thread_id": th, "turn_id": 30, "kind": "user", "inputs": []any{map[string]any{"input_id": 21}}, "route": map[string]any{"chat": -100, "topic": 5}, "followed": true}})
					if hold := f.holdFinal; hold != nil {
						f.ops = append(f.ops, "turn_started")
						f.mu.Unlock()
						<-hold
						f.mu.Lock()
					}
					for _, e := range []map[string]any{
						{"type": "agent", "thread_id": th, "turn_id": 30, "event": map[string]any{"type": "turn_finished", "status": map[string]any{"status": "completed"}, "final_text": "answer", "messages": []any{}}},
						{"type": "turn_ended", "thread_id": th, "turn_id": 30, "held": true},
					} {
						write(conn, map[string]any{"reply": "event", "event": e})
					}
					f.mu.Unlock()
					for range 500 {
						f.mu.Lock()
						n := len(f.releases)
						f.mu.Unlock()
						if n > 0 {
							break
						}
						time.Sleep(10 * time.Millisecond)
					}
					f.mu.Lock()
				case "feedback_submit":
					f.submissions = append(f.submissions, string(req["submission"]))
					if f.feedbackErr != nil {
						write(conn, f.feedbackErr)
						return
					}
					write(conn, map[string]any{"reply": "feedback", "status": "queued", "thread_id": "telegram--100-topic-5", "input_id": 3, "annotations": 2, "stale": false})
				default:
					if engine.ViewCapability(op) != "" && f.viewReply != nil {
						f.views = append(f.views, strings.TrimSpace(string(line)))
						write(conn, f.viewReply(op))
						return
					}
					write(conn, map[string]any{"reply": "error", "message": "invalid request"})
				}
			}()
		}
	}()
}

func TestMiniAppEngineMapsCapabilitiesAndErrors(t *testing.T) {
	f := &fakeService{caps: []string{engine.CapOrdinarySubmit}}
	startService(t, f)
	e := &MiniAppEngine{Client: engine.NewClient(f.socket)}
	ctx := context.Background()
	req := miniapp.FeedbackSubmit{ThreadID: "telegram--100-topic-5", UserID: 7, Submission: `{"submission_id":"s"}`}

	if _, err := e.FeedbackSubmit(ctx, req); !errors.Is(err, miniapp.ErrUnavailable) {
		t.Fatalf("missing capability must be ErrUnavailable, got %v", err)
	}
	if len(f.submissions) != 0 {
		t.Fatal("nothing may be sent without the capability")
	}
	gated := map[string]func() error{
		engine.CapThreadViews + "/threads":   func() error { _, err := e.ThreadList(ctx, "all"); return err },
		engine.CapThreadViews + "/view":      func() error { _, err := e.ThreadView(ctx, "t", nil); return err },
		engine.CapArtifactViews + "/list":    func() error { _, err := e.ArtifactsList(ctx, "t"); return err },
		engine.CapArtifactViews + "/resolve": func() error { _, err := e.ArtifactResolve(ctx, "t", "p"); return err },
		engine.CapMonitorViews + "/monitor":  func() error { _, err := e.MonitorSnapshot(ctx); return err },
		engine.CapMonitorViews + "/agents":   func() error { _, err := e.ActiveAgents(ctx); return err },
		engine.CapMonitorViews + "/usage":    func() error { _, err := e.UsageReport(ctx, 7); return err },
		engine.CapMonitorViews + "/spaces":   func() error { _, err := e.Workspaces(ctx); return err },
		engine.CapThreadRoot + "/git":        func() error { _, err := e.GitStatus(ctx, ""); return err },
		engine.CapThreadRoot + "/diff":       func() error { _, err := e.GitDiff(ctx, miniapp.GitDiff{Path: "a", Kind: "k"}); return err },
	}
	for name, call := range gated {
		capability, _, _ := strings.Cut(name, "/")
		err := call()
		if !errors.Is(err, miniapp.ErrUnavailable) || !strings.Contains(err.Error(), capability) {
			t.Errorf("%s must be unavailable naming %s, got %v", name, capability, err)
		}
	}

	f.mu.Lock()
	f.caps = []string{engine.CapFeedbackSubmit}
	f.mu.Unlock()
	res, err := e.FeedbackSubmit(ctx, req)
	if err != nil {
		t.Fatal(err)
	}
	if res.Status != "queued" || res.InputID == nil || *res.InputID != 3 || res.Annotations != 2 || res.Stale {
		t.Fatalf("unexpected result %+v", res)
	}

	f.mu.Lock()
	f.feedbackErr = map[string]any{"reply": "error", "code": "version_mismatch", "message": "artifact changed"}
	f.mu.Unlock()
	_, err = e.FeedbackSubmit(ctx, req)
	var ee *miniapp.EngineError
	if !errors.As(err, &ee) || ee.Code != "version_mismatch" || ee.Message != "artifact changed" {
		t.Fatalf("want EngineError, got %v", err)
	}
}

type sink struct {
	mu       sync.Mutex
	messages []int64
	rendered []string
	got      chan struct{}
}

func (s *sink) Dispatch(_ context.Context, in bot.Incoming) {
	s.mu.Lock()
	s.messages = append(s.messages, in.Message.ID)
	s.mu.Unlock()
	s.got <- struct{}{}
}

func (s *sink) HandleCallback(context.Context, *telegram.CallbackQuery) {}

func (s *sink) RenderTurn(_ context.Context, t bot.RoutedTurn) {
	for range t.Turn.Events {
	}
	s.mu.Lock()
	s.rendered = append(s.rendered, t.ThreadID)
	s.mu.Unlock()
	s.got <- struct{}{}
}

func initData(userID int64, now time.Time) string {
	values := map[string]string{
		"auth_date": strconv.FormatInt(now.Unix(), 10),
		"user":      `{"id":` + strconv.FormatInt(userID, 10) + `}`,
	}
	keys := make([]string, 0, len(values))
	for k := range values {
		keys = append(keys, k)
	}
	sort.Strings(keys)
	lines := make([]string, len(keys))
	for i, k := range keys {
		lines[i] = k + "=" + values[k]
	}
	secret := hmac.New(sha256.New, []byte("WebAppData"))
	secret.Write([]byte(token))
	mac := hmac.New(sha256.New, secret.Sum(nil))
	mac.Write([]byte(strings.Join(lines, "\n")))
	q := url.Values{}
	for k, v := range values {
		q.Set(k, v)
	}
	q.Set("hash", hex.EncodeToString(mac.Sum(nil)))
	return q.Encode()
}

func TestRunWiresPollingBridgeAndMiniApp(t *testing.T) {
	f := &fakeService{caps: []string{engine.CapFeedbackSubmit}}
	startService(t, f)

	var polls sync.Mutex
	polled := 0
	tg := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if strings.HasSuffix(r.URL.Path, "/getUpdates") {
			polls.Lock()
			polled++
			n := polled
			polls.Unlock()
			if n == 1 {
				io.WriteString(w, `{"ok":true,"result":[
					{"update_id":1,"message":{"message_id":11,"chat":{"id":-100,"type":"supergroup"},"from":{"id":7,"is_bot":false},"text":"hi"}},
					{"update_id":2,"message":{"message_id":12,"chat":{"id":-100,"type":"supergroup"},"from":{"id":9,"is_bot":false},"text":"intruder"}}]}`)
				return
			}
			select {
			case <-r.Context().Done():
			case <-time.After(20 * time.Millisecond):
			}
			io.WriteString(w, `{"ok":true,"result":[]}`)
			return
		}
		io.WriteString(w, `{"ok":true,"result":true}`)
	}))
	defer tg.Close()

	s := &sink{got: make(chan struct{}, 8)}
	settings := Settings{BotToken: token, AllowlistUserIDs: []int64{7}, AllowlistChatIDs: []int64{-100}, Root: "/r"}
	settings.Server.Enabled = true
	settings.Server.Port = 1
	addr := make(chan net.Addr, 1)
	ctx, cancel := context.WithCancel(context.Background())
	done := make(chan error, 1)
	go func() {
		done <- Run(ctx, Config{
			Settings: settings,
			Socket:   f.socket,
			Telegram: telegram.Options{BaseURL: tg.URL, Logger: quiet},
			HTTPAddr: "127.0.0.1:0",
			Build: func(Deps) (Components, error) {
				return Components{Messages: s, Callbacks: s, Renderer: s}, nil
			},
			Log:   quiet,
			Ready: func(a net.Addr) { addr <- a },
		})
	}()

	for range 2 {
		select {
		case <-s.got:
		case <-time.After(5 * time.Second):
			t.Fatal("message intake and turn rendering did not happen")
		}
	}

	submission := `{"submission_id":"s1","note":"a <b> \ud800"}`
	req, _ := http.NewRequest(http.MethodPost, "http://"+(<-addr).String()+"/api/threads/telegram--100-topic-5/artifacts/feedback", strings.NewReader(submission))
	req.Header.Set("Authorization", "tma "+initData(7, time.Now()))
	req.Header.Set("Content-Type", "application/json")
	resp, err := http.DefaultClient.Do(req)
	if err != nil {
		t.Fatal(err)
	}
	b, _ := io.ReadAll(resp.Body)
	resp.Body.Close()
	if resp.StatusCode >= 300 {
		t.Fatalf("feedback answered %d: %s", resp.StatusCode, b)
	}

	cancel()
	if err := <-done; err != nil {
		t.Fatal(err)
	}
	s.mu.Lock()
	defer s.mu.Unlock()
	if len(s.messages) != 1 || s.messages[0] != 11 {
		t.Fatalf("only the allowlisted sender's message may pass: %v", s.messages)
	}
	if len(s.rendered) != 1 || s.rendered[0] != "telegram--100-topic-5" {
		t.Fatalf("rendered %v", s.rendered)
	}
	f.mu.Lock()
	defer f.mu.Unlock()
	// Components without RendererReady (a test build): no replay opt-in and
	// a release without a token, the path of services before hold tokens.
	if len(f.releases) != 1 || f.releases[0] != `"telegram--100-topic-5"/8/` || slices.Contains(f.ops, "subscribe:replay") {
		t.Fatalf("releases %v ops %v", f.releases, f.ops)
	}
	var sent string
	if len(f.submissions) == 1 {
		json.Unmarshal([]byte(f.submissions[0]), &sent)
	}
	if sent != submission {
		t.Fatalf("submission must reach the engine unchanged:\n got %s\nwant %s", f.submissions, submission)
	}
}

type fixedResolver struct{}

func (fixedResolver) ResolveTopic(context.Context, int64, *int64) (bot.TopicInfo, error) {
	return bot.TopicInfo{ThreadID: "telegram--100-topic-5", Kind: bot.TopicOrdinary}, nil
}

// A polled message runs through the real wiring: gate, intake, ordinary
// submit, rendering, delivery records, reply, release.
func TestRunAnswersAPolledMessageThroughTheService(t *testing.T) {
	f := &fakeService{caps: []string{engine.CapOrdinarySubmit, engine.CapComposeUserText, engine.CapRecordDelivery, engine.CapReplyParts, engine.CapHoldTokens}, quietSubscribe: true}
	startService(t, f)
	var mu sync.Mutex
	var sent []map[string]any
	polled := 0
	tg := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		method := r.URL.Path[strings.LastIndex(r.URL.Path, "/")+1:]
		raw, _ := io.ReadAll(r.Body)
		switch method {
		case "getUpdates":
			mu.Lock()
			polled++
			n := polled
			mu.Unlock()
			if n == 1 {
				io.WriteString(w, `{"ok":true,"result":[{"update_id":1,"message":{"message_id":11,"chat":{"id":-100,"type":"supergroup","is_forum":true},"message_thread_id":5,"from":{"id":7,"is_bot":false},"text":"question"}},{"update_id":2,"message":{"message_id":12,"chat":{"id":-100,"type":"supergroup","is_forum":true},"message_thread_id":6,"from":{"id":7,"is_bot":false},"text":"/threadid"}}]}`)
				return
			}
			select {
			case <-r.Context().Done():
			case <-time.After(20 * time.Millisecond):
			}
			io.WriteString(w, `{"ok":true,"result":[]}`)
		case "sendMessage":
			var body map[string]any
			json.Unmarshal(raw, &body)
			mu.Lock()
			sent = append(sent, body)
			id := len(sent) + 500
			mu.Unlock()
			fmt.Fprintf(w, `{"ok":true,"result":{"message_id":%d,"chat":{"id":-100,"type":"supergroup"},"date":1}}`, id)
		default:
			io.WriteString(w, `{"ok":true,"result":true}`)
		}
	}))
	defer tg.Close()

	settings := Settings{BotToken: token, AllowlistUserIDs: []int64{7}, AllowlistChatIDs: []int64{-100}, Root: "/r"}
	ctx, cancel := context.WithCancel(context.Background())
	done := make(chan error, 1)
	go func() {
		done <- Run(ctx, Config{
			Settings: settings, Socket: f.socket, Log: quiet,
			Telegram: telegram.Options{BaseURL: tg.URL, Logger: quiet},
			Build: func(d Deps) (Components, error) {
				return BotComponents(ctx, d, settings, Parts{Planner: bot.TextPlanner{}, Resolver: fixedResolver{}, Artifacts: bot.UnavailableArtifacts{}}), nil
			},
		})
	}()
	deadline := time.After(10 * time.Second)
	for {
		f.mu.Lock()
		n := 0
		for _, op := range f.ops {
			if op == "release:30" {
				n++
			}
		}
		f.mu.Unlock()
		if n > 0 {
			break
		}
		select {
		case <-deadline:
			t.Fatalf("no release; ops %v", f.ops)
		case <-time.After(20 * time.Millisecond):
		}
	}
	cancel()
	if err := <-done; err != nil {
		t.Fatal(err)
	}
	f.mu.Lock()
	defer f.mu.Unlock()
	var mine []string
	for _, op := range f.ops {
		if strings.HasPrefix(op, "submit") || strings.HasPrefix(op, "record") || op == "release:30" {
			mine = append(mine, op)
		}
	}
	if len(mine) != 4 || mine[0] != "submit:ordinary=true" || !strings.HasSuffix(mine[1], ":pending") || !strings.HasSuffix(mine[2], ":sent") || mine[3] != "release:30" {
		t.Fatalf("unexpected order %v", mine)
	}
	mu.Lock()
	defer mu.Unlock()
	var texts []any
	for _, b := range sent {
		texts = append(texts, b["text"])
	}
	// The answer and the /threadid reply (internal/commands through the
	// bot hook) race; both must arrive.
	if !slices.Contains(texts, any("answer")) || !slices.Contains(texts, any(commands.ThreadIDText("telegram--100-topic-5"))) {
		t.Fatalf("sent %v", texts)
	}
}

// Feedback failures keep the write outcome, by stage.
func TestFeedbackFailuresAreClassifiedByStage(t *testing.T) {
	ctx := context.Background()
	req := miniapp.FeedbackSubmit{ThreadID: "telegram--100-topic-5", UserID: 7, Submission: `{"submission_id":"s"}`}

	// No listener: nothing was written.
	dir, _ := os.MkdirTemp("", "gwf")
	defer os.RemoveAll(dir)
	none := &MiniAppEngine{Client: engine.NewClient(filepath.Join(dir, "none.sock"))}
	if _, err := none.FeedbackSubmit(ctx, req); !errors.Is(err, miniapp.ErrNotSent) || errors.Is(err, miniapp.ErrOutcomeUnknown) {
		t.Fatalf("no listener must be ErrNotSent, got %v", err)
	}

	cases := map[string]struct {
		reply func(conn net.Conn)
		want  error
	}{
		"closed without reply": {func(conn net.Conn) {}, miniapp.ErrOutcomeUnknown},
		"unreadable reply":     {func(conn net.Conn) { conn.Write([]byte("{not json\n")) }, miniapp.ErrOutcomeUnknown},
		"unexpected reply":     {func(conn net.Conn) { conn.Write([]byte(`{"reply":"ok"}` + "\n")) }, miniapp.ErrOutcomeUnknown},
		"malformed fields": {func(conn net.Conn) {
			conn.Write([]byte(`{"reply":"feedback","status":"queued","thread_id":"t","annotations":"two","stale":false}` + "\n"))
		}, miniapp.ErrOutcomeUnknown},
	}
	for name, c := range cases {
		dir, _ := os.MkdirTemp("", "gwf")
		sock := filepath.Join(dir, "o.sock")
		ln, err := net.Listen("unix", sock)
		if err != nil {
			t.Fatal(err)
		}
		go func() {
			for {
				conn, err := ln.Accept()
				if err != nil {
					return
				}
				line, _ := bufio.NewReader(conn).ReadBytes('\n')
				if strings.Contains(string(line), `"op":"status"`) {
					conn.Write([]byte(`{"reply":"status","report":{"protocol":2,"capabilities":["feedback_submit"],"settled":true,"threads":[]}}` + "\n"))
				} else {
					c.reply(conn)
				}
				conn.Close()
			}
		}()
		_, err = (&MiniAppEngine{Client: engine.NewClient(sock)}).FeedbackSubmit(ctx, req)
		if !errors.Is(err, c.want) || errors.Is(err, miniapp.ErrNotSent) {
			t.Errorf("%s: want %v, got %v", name, c.want, err)
		}
		ln.Close()
		os.RemoveAll(dir)
	}
}

// Views go out in the views.rs wire shape and their bodies pass through
// unchanged; refusals keep Rust's code and text.
func TestMiniAppEngineViews(t *testing.T) {
	f := &fakeService{caps: []string{engine.CapThreadViews, engine.CapArtifactViews, engine.CapMonitorViews}}
	f.viewReply = func(op string) map[string]any {
		switch op {
		case engine.ViewThreadView:
			return map[string]any{"reply": "view", "body": map[string]any{"id": "t", "delivering": false}, "source_id": "src"}
		case engine.ViewArtifactResolve:
			return map[string]any{"reply": "view", "body": map[string]any{"path": "/abs/a.html", "mime": "text/html", "filename": "a.html"}}
		case engine.ViewUsageReport:
			return map[string]any{"reply": "error", "code": "invalid", "message": "days must be between 1 and 365"}
		case engine.ViewArtifactCanonical:
			return map[string]any{"reply": "view", "body": map[string]any{
				"mime": "image/png", "data": "iVBORw0K", "source_sha256": "s", "sha256": "c",
				"width": 10, "height": 20, "source_width": 30, "source_height": 40,
			}}
		case engine.ViewArtifactVersion:
			return map[string]any{"reply": "view", "body": map[string]any{"sha256": "v", "size": 3}}
		}
		return map[string]any{"reply": "view", "body": map[string]any{"op": op}}
	}
	startService(t, f)
	e := &MiniAppEngine{Client: engine.NewClient(f.socket)}
	ctx := context.Background()

	view, err := e.ThreadView(ctx, "t", ptr(uint64(4)))
	if err != nil || view.SourceID != "src" || string(view.Body) != `{"delivering":false,"id":"t"}` {
		t.Fatalf("thread view: %+v %v", view, err)
	}
	art, err := e.ArtifactResolve(ctx, "t", "a.html")
	if err != nil || art.Path != "/abs/a.html" || art.Mime != "text/html" || art.Filename != "a.html" {
		t.Fatalf("resolve: %+v %v", art, err)
	}
	img, err := e.ArtifactCanonical(ctx, "t", "a.png")
	if err != nil || string(img.Data) != "\x89PNG\r\n" || img.Mime != "image/png" || img.SourceSHA256 != "s" || img.SHA256 != "c" ||
		img.Width != 10 || img.Height != 20 || img.SourceWidth != 30 || img.SourceHeight != 40 {
		t.Fatalf("canonical: %+v %v", img, err)
	}
	if body, err := e.ArtifactVersion(ctx, "t", "a.png"); err != nil || string(body) != `{"sha256":"v","size":3}` {
		t.Fatalf("version: %s %v", body, err)
	}
	if body, err := e.ThreadList(ctx, "workers"); err != nil || string(body) != `{"op":"threads_list"}` {
		t.Fatalf("threads: %s %v", body, err)
	}
	if _, err := e.MonitorSnapshot(ctx); err != nil {
		t.Fatal(err)
	}
	_, err = e.UsageReport(ctx, 30)
	var ee *miniapp.EngineError
	if !errors.As(err, &ee) || ee.Code != "invalid" || ee.Message != "days must be between 1 and 365" {
		t.Fatalf("want the Rust refusal, got %v", err)
	}
	f.mu.Lock()
	defer f.mu.Unlock()
	want := []string{
		`{"op":"thread_view","thread_id":"t","after":4}`,
		`{"op":"artifact_resolve","thread_id":"t","path":"a.html"}`,
		`{"op":"artifact_canonical","thread_id":"t","path":"a.png"}`,
		`{"op":"artifact_version","thread_id":"t","path":"a.png"}`,
		`{"op":"threads_list","kind":"workers"}`,
		`{"op":"monitor_snapshot"}`,
		`{"op":"usage_report","days":30}`,
	}
	if strings.Join(f.views, "\n") != strings.Join(want, "\n") {
		t.Fatalf("requests:\n%s", strings.Join(f.views, "\n"))
	}
}
func ptr[T any](v T) *T { return &v }

// Git views run in process with the shared gitview package. Rust's
// thread_root view names the thread and its stored root, and is the only
// request sent; its refusals keep their code and text, and gitview's
// refusals come back as the EngineError their route answers with.
func TestMiniAppEngineGit(t *testing.T) {
	repo, err := filepath.EvalSymlinks(t.TempDir())
	if err != nil {
		t.Fatal(err)
	}
	git := func(args ...string) {
		t.Helper()
		cmd := exec.Command("git", args...)
		cmd.Dir = repo
		cmd.Env = append(os.Environ(), "GIT_CONFIG_GLOBAL=/dev/null", "GIT_CONFIG_NOSYSTEM=1",
			"GIT_AUTHOR_NAME=A", "GIT_AUTHOR_EMAIL=a@example.invalid", "GIT_COMMITTER_NAME=C", "GIT_COMMITTER_EMAIL=c@example.invalid")
		if out, err := cmd.CombinedOutput(); err != nil {
			t.Fatalf("git %v: %v\n%s", args, err, out)
		}
	}
	git("init", "-q", "-b", "main")
	if err := os.WriteFile(filepath.Join(repo, "a.go"), []byte("package a\n"), 0o600); err != nil {
		t.Fatal(err)
	}
	git("add", "a.go")
	git("commit", "-q", "-m", "first")
	if err := os.WriteFile(filepath.Join(repo, "a.go"), []byte("package a\n\nvar B = 1\n"), 0o600); err != nil {
		t.Fatal(err)
	}

	f := &fakeService{caps: []string{engine.CapThreadRoot}}
	reply := map[string]any{"reply": "view", "body": map[string]any{
		"thread_id": "t", "effective_id": "w", "root_path": repo, "fallback_root": t.TempDir(),
	}}
	f.viewReply = func(string) map[string]any { return reply }
	startService(t, f)
	e := &MiniAppEngine{Client: engine.NewClient(f.socket)}
	ctx := context.Background()

	body, err := e.GitStatus(ctx, "")
	if err != nil {
		t.Fatal(err)
	}
	var status gitview.StatusResponse
	if err := json.Unmarshal(body, &status); err != nil || status.Repository.Root != repo || status.Repository.Source != "thread" ||
		*status.Repository.ThreadID != "t" || len(status.Files.Unstaged) != 1 || len(status.Commits) != 1 {
		t.Fatalf("status %s %v", body, err)
	}
	if body, err := e.GitScope(ctx, "t", "all"); err != nil || !strings.Contains(string(body), `"files":[{"path":"a.go","status":"M"}]`) {
		t.Fatalf("scope %s %v", body, err)
	}
	body, err = e.GitDiff(ctx, miniapp.GitDiff{ThreadID: "t", Path: "a.go", Kind: "unstaged"})
	if err != nil || !strings.Contains(string(body), `+var B = 1`) {
		t.Fatalf("diff %s %v", body, err)
	}
	refused := func(err error, code, message string) {
		t.Helper()
		var ee *miniapp.EngineError
		if !errors.As(err, &ee) || ee.Code != code || ee.Message != message {
			t.Fatalf("want %s %q, got %v", code, message, err)
		}
	}
	_, err = e.GitDiff(ctx, miniapp.GitDiff{ThreadID: "t", Path: "a.go", Kind: "staged"})
	refused(err, "not_found", "Git file is no longer in that category")
	_, err = e.GitDiff(ctx, miniapp.GitDiff{ThreadID: "t", Path: "../a.go", Kind: "staged"})
	refused(err, "invalid", "Invalid Git file path")
	_, err = e.GitScope(ctx, "t", "HEAD~1")
	refused(err, "invalid", "Invalid Git scope")

	f.mu.Lock()
	reply = map[string]any{"reply": "error", "code": "invalid", "message": "Invalid thread ID"}
	f.mu.Unlock()
	_, err = e.GitStatus(ctx, "x")
	refused(err, "invalid", "Invalid thread ID")
	f.mu.Lock()
	reply = map[string]any{"reply": "error", "code": "internal", "message": "Thread root resolution failed"}
	f.mu.Unlock()
	_, err = e.GitStatus(ctx, "x")
	refused(err, "internal", "Git repository resolution failed")
	f.mu.Lock()
	reply = map[string]any{"reply": "view", "body": map[string]any{"thread_id": nil, "effective_id": nil, "root_path": nil, "fallback_root": t.TempDir()}}
	f.mu.Unlock()
	_, err = e.GitStatus(ctx, "")
	refused(err, "not_found", "Git repository not found")

	f.mu.Lock()
	defer f.mu.Unlock()
	want := []string{
		`{"op":"thread_root"}`,
		`{"op":"thread_root","thread_id":"t"}`,
		`{"op":"thread_root","thread_id":"t"}`,
		`{"op":"thread_root","thread_id":"t"}`,
		`{"op":"thread_root","thread_id":"t"}`,
		`{"op":"thread_root","thread_id":"x"}`,
		`{"op":"thread_root","thread_id":"x"}`,
		`{"op":"thread_root"}`,
	}
	if strings.Join(f.views, "\n") != strings.Join(want, "\n") {
		t.Fatalf("requests:\n%s", strings.Join(f.views, "\n"))
	}
}

// Quotas are read in process with the shared quota package: nothing goes to
// the socket, and a failure is an error (the Mini App cache then keeps its
// last known quotas), never a healthy empty list.
func TestMiniAppEngineReadsQuotasInProcess(t *testing.T) {
	f := &fakeService{caps: []string{engine.CapMonitorViews}}
	startService(t, f)
	home := t.TempDir()
	noEnv := func(string) (string, bool) { return "", false }
	e := &MiniAppEngine{Client: engine.NewClient(f.socket), Quota: quota.Options{Home: home, LookupEnv: noEnv}}
	ctx := context.Background()

	if _, err := e.SubscriptionQuotas(ctx); !errors.Is(err, version.ErrUnknown) {
		t.Fatalf("no version must be version.ErrUnknown, got %v", err)
	}
	e.Quota.Version = "0.0.0-test"
	body, err := e.SubscriptionQuotas(ctx)
	if err != nil || string(body) != "[]" {
		t.Fatalf("no stored accounts are no subscriptions: %s %v", body, err)
	}

	const secret = "test-access-SECRET"
	if err := os.WriteFile(filepath.Join(home, "oauth.json"), []byte(`{"claude-cli":{"access":"`+secret+`"`), 0o600); err != nil {
		t.Fatal(err)
	}
	_, err = e.SubscriptionQuotas(ctx)
	if !errors.Is(err, quota.ErrDiscovery) || strings.Contains(err.Error(), secret) {
		t.Fatalf("an unreadable account store must be ErrDiscovery without its contents, got %v", err)
	}

	cancelled, cancel := context.WithCancel(ctx)
	cancel()
	os.Remove(filepath.Join(home, "oauth.json"))
	if _, err := e.SubscriptionQuotas(cancelled); err == nil {
		t.Fatal("a cancelled read must fail")
	}

	f.mu.Lock()
	defer f.mu.Unlock()
	if len(f.views) != 0 {
		t.Fatalf("quotas must not use the socket: %v", f.views)
	}
}

// Malformed resolve and canonical bodies are errors, never half-filled
// values.
func TestMiniAppEngineRefusesIncompleteBodies(t *testing.T) {
	bodies := map[string]map[string]any{}
	f := &fakeService{caps: []string{engine.CapArtifactViews}}
	f.viewReply = func(op string) map[string]any { return map[string]any{"reply": "view", "body": bodies[op]} }
	startService(t, f)
	e := &MiniAppEngine{Client: engine.NewClient(f.socket)}
	ctx := context.Background()
	for _, b := range []map[string]any{
		{"path": "rel/a.html", "mime": "text/html", "filename": "a.html"},
		{"path": "/abs/a.html", "mime": "", "filename": "a.html"},
		{"path": "/abs/a.html", "mime": "text/html"},
	} {
		bodies[engine.ViewArtifactResolve] = b
		if _, err := e.ArtifactResolve(ctx, "t", "a"); err == nil {
			t.Errorf("resolve %v must fail", b)
		}
	}
	for _, b := range []map[string]any{
		{"mime": "image/png", "data": "", "source_sha256": "s", "sha256": "c"},
		{"mime": "image/png", "data": "!!", "source_sha256": "s", "sha256": "c"},
		{"mime": "image/png", "data": "iVBORw0K", "sha256": "c"},
	} {
		bodies[engine.ViewArtifactCanonical] = b
		if _, err := e.ArtifactCanonical(ctx, "t", "a"); err == nil {
			t.Errorf("canonical %v must fail", b)
		}
	}
}

// The token never reaches logs or encoders, whatever formats them.
func TestSettingsNeverPrintTheToken(t *testing.T) {
	s, err := ReadSettings(strings.NewReader(`{"bot_token":"` + token + `","allowlist_user_ids":[7],"root":"/r"}`))
	if err != nil {
		t.Fatal(err)
	}
	var text, js bytes.Buffer
	slog.New(slog.NewTextHandler(&text, nil)).Info("x", "settings", s, "ptr", &s)
	slog.New(slog.NewJSONHandler(&js, nil)).Info("x", "settings", s, "ptr", &s)
	enc, _ := json.Marshal(s)
	encPtr, _ := json.Marshal(&s)
	out := map[string]string{
		"text": text.String(), "json": js.String(), "marshal": string(enc), "marshal ptr": string(encPtr),
		"%v": fmt.Sprintf("%v", s), "%+v": fmt.Sprintf("%+v", s), "%#v": fmt.Sprintf("%#v", s), "%s": fmt.Sprintf("%s", s),
		"%v ptr": fmt.Sprintf("%v", &s),
	}
	for name, got := range out {
		if strings.Contains(got, "SECRET") {
			t.Errorf("%s leaks the token: %s", name, got)
		}
	}
	if s.BotToken != token {
		t.Fatal("redaction must not change the value")
	}
	// Redaction is outbound only: reading settings keeps the token, and
	// encoded (redacted) settings are refused as input, never used.
	again, err := ReadSettings(strings.NewReader(`{"bot_token":"` + token + `","allowlist_user_ids":[7],"allowlist_chat_ids":[-100],"server":{"enabled":true,"port":4141},"root":"/r","profiles":[]}`))
	if err != nil || again.BotToken != token || again.Server.Port != 4141 {
		t.Fatalf("settings input is unchanged: %+v %v", again.Server, err)
	}
	if _, err := ReadSettings(bytes.NewReader(enc)); err == nil || !strings.Contains(err.Error(), "redacted") {
		t.Fatalf("redacted settings must be refused as input: %v", err)
	}
}

// drainRig runs Run against the fake service and a fake Bot API that
// delivers one message, then reports every poll's offset.
type drainRig struct {
	mu      sync.Mutex
	sent    []string
	offsets []string
	tg      *httptest.Server
}

func newDrainRig(t *testing.T) *drainRig {
	r := &drainRig{}
	polled := 0
	r.tg = httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, req *http.Request) {
		method := req.URL.Path[strings.LastIndex(req.URL.Path, "/")+1:]
		raw, _ := io.ReadAll(req.Body)
		var body map[string]any
		json.Unmarshal(raw, &body)
		switch method {
		case "getUpdates":
			r.mu.Lock()
			polled++
			n := polled
			r.offsets = append(r.offsets, fmt.Sprint(body["offset"], "/", body["timeout"]))
			r.mu.Unlock()
			if n == 1 {
				io.WriteString(w, `{"ok":true,"result":[{"update_id":7,"message":{"message_id":11,"chat":{"id":-100,"type":"supergroup","is_forum":true},"message_thread_id":5,"from":{"id":7,"is_bot":false},"text":"question"}}]}`)
				return
			}
			select {
			case <-req.Context().Done():
			case <-time.After(20 * time.Millisecond):
			}
			io.WriteString(w, `{"ok":true,"result":[]}`)
		case "sendMessage":
			r.mu.Lock()
			r.sent = append(r.sent, fmt.Sprint(body["text"]))
			id := len(r.sent) + 500
			r.mu.Unlock()
			fmt.Fprintf(w, `{"ok":true,"result":{"message_id":%d,"chat":{"id":-100,"type":"supergroup"},"date":1}}`, id)
		default:
			io.WriteString(w, `{"ok":true,"result":true}`)
		}
	}))
	t.Cleanup(r.tg.Close)
	return r
}

func (r *drainRig) sentText(s string) bool {
	r.mu.Lock()
	defer r.mu.Unlock()
	return slices.Contains(r.sent, s)
}

func startDrain(t *testing.T, f *fakeService, r *drainRig, drain time.Duration) (cancel func(), done chan error) {
	settings := Settings{BotToken: token, AllowlistUserIDs: []int64{7}, AllowlistChatIDs: []int64{-100}, Root: "/r"}
	ctx, cancelFn := context.WithCancel(context.Background())
	done = make(chan error, 1)
	go func() {
		done <- Run(ctx, Config{
			Settings: settings, Socket: f.socket, Log: quiet, DrainTimeout: drain,
			Telegram: telegram.Options{BaseURL: r.tg.URL, Logger: quiet},
			Build: func(d Deps) (Components, error) {
				return BotComponents(d.Work, d, settings, Parts{Planner: bot.TextPlanner{}, Resolver: fixedResolver{}, Artifacts: bot.UnavailableArtifacts{}}), nil
			},
		})
	}()
	return cancelFn, done
}

func waitUntil(t *testing.T, what string, limit time.Duration, pred func() bool) {
	t.Helper()
	deadline := time.Now().Add(limit)
	for !pred() {
		if time.Now().After(deadline) {
			t.Fatalf("timed out waiting for %s", what)
		}
		time.Sleep(10 * time.Millisecond)
	}
}

func (f *fakeService) has(op string) bool {
	f.mu.Lock()
	defer f.mu.Unlock()
	return slices.Contains(f.ops, op)
}

// A shutdown while a turn runs stops polling, confirms the handed-off
// update, and still delivers and releases the turn before Run returns.
func TestShutdownDrainsTheRunningTurn(t *testing.T) {
	f := &fakeService{caps: []string{engine.CapOrdinarySubmit, engine.CapComposeUserText, engine.CapRecordDelivery, engine.CapReplyParts, engine.CapHoldTokens}, quietSubscribe: true, holdFinal: make(chan struct{})}
	startService(t, f)
	r := newDrainRig(t)
	cancel, done := startDrain(t, f, r, 10*time.Second)
	waitUntil(t, "the turn to start", 5*time.Second, func() bool { return f.has("turn_started") })
	cancel()
	time.Sleep(100 * time.Millisecond)
	select {
	case err := <-done:
		t.Fatalf("Run returned before the turn finished: %v", err)
	default:
	}
	r.mu.Lock()
	polls := len(r.offsets)
	confirmed := slices.Contains(r.offsets, "8/0")
	r.mu.Unlock()
	if confirmed {
		t.Fatalf("nothing is confirmed before the drain ends: %v", r.offsets)
	}
	close(f.holdFinal)
	if err := <-done; err != nil {
		t.Fatal(err)
	}
	if !r.sentText("answer") || !f.has("release:30") {
		t.Fatalf("the drained turn is delivered and released: sent=%v ops=%v", r.sent, f.ops)
	}
	r.mu.Lock()
	defer r.mu.Unlock()
	if len(r.offsets) != polls+1 || r.offsets[polls] != "8/0" {
		t.Fatalf("after the drain, exactly one confirm poll (offset 8, no wait): %v", r.offsets)
	}
}

// Past the drain timeout the remaining work is cancelled: Run returns, no
// reply is sent, and nothing is resent.
func TestShutdownForcesAfterTheDrainTimeout(t *testing.T) {
	f := &fakeService{caps: []string{engine.CapOrdinarySubmit, engine.CapComposeUserText, engine.CapRecordDelivery, engine.CapReplyParts, engine.CapHoldTokens}, quietSubscribe: true, holdFinal: make(chan struct{})}
	startService(t, f)
	defer close(f.holdFinal)
	r := newDrainRig(t)
	cancel, done := startDrain(t, f, r, 300*time.Millisecond)
	waitUntil(t, "the turn to start", 5*time.Second, func() bool { return f.has("turn_started") })
	start := time.Now()
	cancel()
	select {
	case err := <-done:
		if err != nil {
			t.Fatal(err)
		}
	case <-time.After(10 * time.Second):
		t.Fatal("Run did not return after the drain timeout")
	}
	if took := time.Since(start); took < 300*time.Millisecond {
		t.Fatalf("Run returned before the drain timeout: %v", took)
	}
	if r.sentText("answer") {
		t.Fatal("a cancelled turn sends no reply")
	}
	r.mu.Lock()
	defer r.mu.Unlock()
	if slices.Contains(r.offsets, "8/0") {
		t.Fatalf("a drain that timed out confirms nothing (update 7 comes again): %v", r.offsets)
	}
}

// A polled message through the real wiring with hold tokens: the follower
// opts in to replay, and its records and release carry the token from the
// submit reply.
func TestRunFollowsAHeldTurnWithItsSubmitToken(t *testing.T) {
	f := &fakeService{caps: []string{engine.CapOrdinarySubmit, engine.CapComposeUserText, engine.CapRecordDelivery, engine.CapReplyParts, engine.CapHoldTokens}, quietSubscribe: true}
	startService(t, f)
	r := newDrainRig(t)
	cancel, done := startDrain(t, f, r, 5*time.Second)
	waitUntil(t, "the release", 10*time.Second, func() bool { return f.has("release:30") })
	cancel()
	if err := <-done; err != nil {
		t.Fatal(err)
	}
	f.mu.Lock()
	defer f.mu.Unlock()
	if len(f.releases) != 1 || f.releases[0] != `"telegram--100-topic-5"/30/"FOLLOWER"` {
		t.Fatalf("release %v", f.releases)
	}
	if len(f.recordHolders) != 2 || f.recordHolders[0] != "30/FOLLOWER" || f.recordHolders[1] != "30/FOLLOWER" {
		t.Fatalf("records %v", f.recordHolders)
	}
}

// Another renderer that stays subscribed stops the gateway with an error
// (not a restart): retried 3 times, then Run returns ErrRendererConflict.
func TestRunStopsWhenAnotherRendererIsLive(t *testing.T) {
	f := &fakeService{caps: []string{engine.CapRecordDelivery, engine.CapReplyParts, engine.CapHoldTokens}, conflictSubscribe: true}
	startService(t, f)
	r := newDrainRig(t)
	_, done := startDrain(t, f, r, 5*time.Second)
	select {
	case err := <-done:
		if !errors.Is(err, bot.ErrRendererConflict) {
			t.Fatalf("want ErrRendererConflict, got %v", err)
		}
	case <-time.After(10 * time.Second):
		t.Fatal("the gateway must stop")
	}
	f.mu.Lock()
	defer f.mu.Unlock()
	n := 0
	for _, op := range f.ops {
		if op == "subscribe:conflict" {
			n++
		}
	}
	if n != 4 {
		t.Fatalf("one try and 3 retries: %d", n)
	}
}
