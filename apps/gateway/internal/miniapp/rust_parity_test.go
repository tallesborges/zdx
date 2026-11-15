package miniapp_test

import (
	"bytes"
	"context"
	"crypto/hmac"
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"errors"
	"fmt"
	"image"
	"image/color"
	"image/png"
	"io"
	"log/slog"
	"net/http"
	"net/http/httptest"
	"net/url"
	"os"
	"os/exec"
	"path/filepath"
	"sort"
	"strconv"
	"strings"
	"sync"
	"syscall"
	"testing"
	"testing/fstest"
	"time"

	"github.com/tallesborges/zdx/apps/gateway/internal/engine"
	"github.com/tallesborges/zdx/apps/gateway/internal/gateway"
	"github.com/tallesborges/zdx/apps/gateway/internal/miniapp"
	"github.com/tallesborges/zdx/apps/gateway/internal/sandbox"
	"github.com/tallesborges/zdx/apps/shared/protocol"
	"github.com/tallesborges/zdx/apps/shared/quota"
	"github.com/tallesborges/zdx/apps/shared/socket"
)

// The real-route checks are opt-in: they run the prebuilt zdx binary named
// by sandbox.RustBinaryEnv (never built here) as an isolated `zdx
// orchestrator serve` on a temp HOME and ZDX_HOME with mock providers, and
// put the Mini App handler and the real gateway adapter in front of its
// socket. Without a binary, or on one that lacks a capability they need,
// they skip; with sandbox.RustStrictEnv set they fail instead.

const (
	parityToken  = "1234567890:PARITY-TEST-TOKEN"
	parityUser   = int64(4242)
	parityThread = "telegram--100-topic-5"
	parityModel  = "anthropic:claude-sonnet-5"
)

// parityEnv is the child environment: an allowlist of parent variables,
// then the temp home and the mock provider, so no real config, credential,
// thread or provider can be reached.
func parityEnv(home string, settings ...string) []string {
	var out []string
	for _, kv := range os.Environ() {
		key, _, _ := strings.Cut(kv, "=")
		if key == "PATH" || key == "TMPDIR" || key == "LANG" || strings.HasPrefix(key, "LC_") {
			out = append(out, kv)
		}
	}
	out = append(out, "HOME="+home, "ZDX_HOME="+home)
	return append(out, settings...)
}

func sseAnswer(text string) string {
	quoted, _ := json.Marshal(text)
	return `event: message_start
data: {"type":"message_start","message":{"id":"msg_1","type":"message","role":"assistant","content":[],"model":"claude-sonnet-5","stop_reason":null,"stop_sequence":null,"usage":{"input_tokens":10,"output_tokens":1}}}

event: content_block_start
data: {"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}

event: content_block_delta
data: {"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":` + string(quoted) + `}}

event: content_block_stop
data: {"type":"content_block_stop","index":0}

event: message_delta
data: {"type":"message_delta","delta":{"stop_reason":"end_turn","stop_sequence":null},"usage":{"output_tokens":5}}

event: message_stop
data: {"type":"message_stop"}

`
}

func initData(userID int64) string {
	date := strconv.FormatInt(time.Now().Unix(), 10)
	user := `{"id":` + strconv.FormatInt(userID, 10) + `}`
	secret := hmac.New(sha256.New, []byte("WebAppData"))
	secret.Write([]byte(parityToken))
	mac := hmac.New(sha256.New, secret.Sum(nil))
	mac.Write([]byte("auth_date=" + date + "\nuser=" + user))
	values := url.Values{}
	values.Set("auth_date", date)
	values.Set("user", user)
	values.Set("hash", hex.EncodeToString(mac.Sum(nil)))
	return values.Encode()
}

type parity struct {
	t        *testing.T
	sockPath string
	bin      string
	home     string
	root     string
	env      []string
	eng      *engine.Client
	http     *httptest.Server
	tracker  *miniapp.DeliveryTracker
	logPath  string
	page     string
	picture  string
	pictureB []byte
	notes    string
	// report is the service's status at start: its capabilities.
	report *engine.StatusReport

	mu     sync.Mutex
	models []string // model request bodies, in order
	goLog  lockedBuffer
}

// lockedBuffer collects the Go handlers' logs for failure messages.
type lockedBuffer struct {
	mu  sync.Mutex
	buf bytes.Buffer
}

func (b *lockedBuffer) Write(p []byte) (int, error) {
	b.mu.Lock()
	defer b.mu.Unlock()
	return b.buf.Write(p)
}

func (b *lockedBuffer) String() string {
	b.mu.Lock()
	defer b.mu.Unlock()
	return b.buf.String()
}

// parityOptions adjust the isolated service: extra config.toml lines in the
// temp home, extra child environment, and a handler for provider paths the
// default Anthropic mock does not answer (it gets every request first and
// returns false to fall through).
type parityOptions struct {
	config string
	// projectConfig is written to <root>/.zdx/config.toml, a layer only a
	// request with the right root loads.
	projectConfig string
	env           []string
	provider      func(w http.ResponseWriter, r *http.Request, body []byte) bool
	// engine wraps the adapter the handler uses.
	engine func(p *parity) miniapp.Engine
	gate   miniapp.FeedbackGate
	users  []int64
	// caps are capabilities the test needs on top of the views.
	caps []string
}

func run(t *testing.T, dir string, name string, args ...string) {
	t.Helper()
	cmd := exec.Command(name, args...)
	cmd.Dir = dir
	cmd.Env = append(os.Environ(), "GIT_CONFIG_GLOBAL=/dev/null", "GIT_CONFIG_SYSTEM=/dev/null")
	if out, err := cmd.CombinedOutput(); err != nil {
		t.Fatalf("%s %v: %v\n%s", name, args, err, out)
	}
}

func startParity(t *testing.T) *parity { return startParityWith(t, parityOptions{}) }

func startParityWith(t *testing.T, opts parityOptions) *parity {
	t.Helper()
	bin := sandbox.RustBinary(t)
	// Short: Unix socket paths are limited to about 104 bytes.
	home, err := os.MkdirTemp("", "zp")
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { os.RemoveAll(home) })
	p := &parity{t: t, bin: bin, home: home, root: t.TempDir(), logPath: filepath.Join(home, "service.log")}

	// The project root is a small Git repository: one commit, one change,
	// one untracked file, so the Git views have something to show.
	run(t, p.root, "git", "init", "-q", "-b", "main")
	if err := os.WriteFile(filepath.Join(p.root, "README.md"), []byte("one\n"), 0o600); err != nil {
		t.Fatal(err)
	}
	run(t, p.root, "git", "add", "README.md")
	run(t, p.root, "git", "-c", "user.name=Parity", "-c", "user.email=parity@example.invalid", "commit", "-q", "-m", "first")
	if err := os.WriteFile(filepath.Join(p.root, "README.md"), []byte("one\ntwo\n"), 0o600); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(filepath.Join(p.root, "new.txt"), []byte("new\n"), 0o600); err != nil {
		t.Fatal(err)
	}

	files := t.TempDir()
	p.page = filepath.Join(files, "page.html")
	if err := os.WriteFile(p.page, []byte("<html><body>synthetic page</body></html>"), 0o600); err != nil {
		t.Fatal(err)
	}
	img := image.NewRGBA(image.Rect(0, 0, 40, 30))
	for x := range 40 {
		for y := range 30 {
			img.Set(x, y, color.RGBA{uint8(x * 6), uint8(y * 8), 90, 255})
		}
	}
	var buf bytes.Buffer
	if err := png.Encode(&buf, img); err != nil {
		t.Fatal(err)
	}
	p.picture, p.pictureB = filepath.Join(files, "picture.png"), buf.Bytes()
	if err := os.WriteFile(p.picture, p.pictureB, 0o600); err != nil {
		t.Fatal(err)
	}
	p.notes = filepath.Join(files, "notes.txt")
	if err := os.WriteFile(p.notes, []byte("plain notes\n"), 0o600); err != nil {
		t.Fatal(err)
	}
	if opts.projectConfig != "" {
		if err := os.MkdirAll(filepath.Join(p.root, ".zdx"), 0o700); err != nil {
			t.Fatal(err)
		}
		if err := os.WriteFile(filepath.Join(p.root, ".zdx", "config.toml"), []byte(opts.projectConfig), 0o600); err != nil {
			t.Fatal(err)
		}
	}
	if opts.config != "" {
		if err := os.WriteFile(filepath.Join(home, "config.toml"), []byte(opts.config), 0o600); err != nil {
			t.Fatal(err)
		}
	}

	provider := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		body, _ := io.ReadAll(r.Body)
		if opts.provider != nil && opts.provider(w, r, body) {
			return
		}
		p.mu.Lock()
		p.models = append(p.models, string(body))
		p.mu.Unlock()
		w.Header().Set("Content-Type", "text/event-stream")
		io.WriteString(w, sseAnswer("PARITY-ANSWER with <b>&</b>\n<medias><media>"+p.page+"</media><media>"+p.picture+"</media><media>"+p.notes+"</media></medias>"))
	}))
	t.Cleanup(provider.Close)
	extra := make([]string, len(opts.env))
	for i, kv := range opts.env {
		extra[i] = strings.ReplaceAll(kv, "__MOCK__", provider.URL)
	}
	p.env = parityEnv(home, append([]string{"ANTHROPIC_API_KEY=test-api-key", "ANTHROPIC_BASE_URL=" + provider.URL}, extra...)...)

	// The child's home must be inside the test sandbox (TestMain), and its
	// socket path within the macOS limit for Unix sockets.
	if err := sandbox.CheckEnv(p.env); err != nil {
		t.Fatal(err)
	}
	if !filepath.IsAbs(bin) {
		t.Fatalf("%s must be an absolute path, got %q", sandbox.RustBinaryEnv, bin)
	}
	if n := len(engine.SocketPath(home)); n > 100 {
		t.Fatalf("socket path is %d bytes, over the Unix socket limit: %s", n, engine.SocketPath(home))
	}
	logFile, _ := os.Create(p.logPath)
	cmd := exec.Command(bin, "--root", p.root, "orchestrator", "serve")
	cmd.Dir, cmd.Env = p.root, p.env
	cmd.Stdout, cmd.Stderr = logFile, logFile
	if err := cmd.Start(); err != nil {
		t.Fatal(err)
	}
	// exited closes when the process ends; exitErr is its result. A closed
	// channel can be read by both the startup wait and the cleanup.
	exited := make(chan struct{})
	var exitErr error
	go func() { exitErr = cmd.Wait(); close(exited) }()
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
	p.sockPath = socket
	p.eng = engine.NewClient(socket)
	report := p.waitReady(exited, &exitErr)
	p.report = report
	t.Logf("service protocol %d, capabilities %v", report.Version(), report.Capabilities)
	if report.Version() < engine.ProtocolCapabilities {
		sandbox.RustFeatureMissing(t, fmt.Sprintf("binary %s speaks protocol %d, these checks need >= %d", bin, report.Version(), engine.ProtocolCapabilities))
	}
	need := append([]string{engine.CapOrdinarySubmit, engine.CapThreadViews, engine.CapArtifactViews, engine.CapMonitorViews}, opts.caps...)
	for _, capability := range need {
		if !report.Has(capability) {
			sandbox.RustFeatureMissing(t, fmt.Sprintf("binary %s lacks %s (it reports %v)", bin, capability, report.Capabilities))
		}
	}

	p.tracker = &miniapp.DeliveryTracker{}
	p.http = p.newHandler(opts)
	p.turn("show me the page and the picture")
	return p
}

// newHandler is one Mini App HTTP server in front of the service. Each has
// its own in-process state (caches, feedback guard), like a second gateway.
func (p *parity) newHandler(opts parityOptions) *httptest.Server {
	p.t.Helper()
	var eng miniapp.Engine = &gateway.MiniAppEngine{Client: p.eng, Root: p.root}
	if opts.engine != nil {
		eng = opts.engine(p)
	}
	users := append([]int64{parityUser}, opts.users...)
	handler, err := miniapp.New(miniapp.Config{
		BotToken:       parityToken,
		AllowedUserIDs: users,
		Assets:         fstest.MapFS{"index.html": {Data: []byte("shell")}},
		Engine:         eng,
		Deliveries:     p.tracker,
		Feedback:       opts.gate,
		Logger:         slog.New(slog.NewTextHandler(&p.goLog, nil)),
	})
	if err != nil {
		p.t.Fatal(err)
	}
	srv := httptest.NewServer(handler)
	p.t.Cleanup(srv.Close)
	return srv
}

func (p *parity) modelRequests() []string {
	p.mu.Lock()
	defer p.mu.Unlock()
	return append([]string(nil), p.models...)
}

// waitReady waits until the service answers a status request. The socket
// file exists before the service accepts on it, so its presence proves
// nothing. Only a request that was never sent (no listener yet, refused
// connect) is retried, within 30 seconds; any other failure, or the process
// exiting, fails the test with the service log.
func (p *parity) waitReady(exited <-chan struct{}, exitErr *error) *engine.StatusReport {
	p.t.Helper()
	deadline := time.Now().Add(30 * time.Second)
	for {
		ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
		report, err := p.eng.StatusReport(ctx, nil)
		cancel()
		if err == nil {
			return report
		}
		if !errors.Is(err, engine.ErrRequestNotSent) {
			p.t.Fatalf("service status: %v\n%s", err, p.log())
		}
		select {
		case <-exited:
			p.t.Fatalf("service exited early (%v):\n%s", *exitErr, p.log())
		case <-time.After(20 * time.Millisecond):
		}
		if time.Now().After(deadline) {
			p.t.Fatalf("service never answered status in 30s: %v\n%s", err, p.log())
		}
	}
}

func (p *parity) log() string {
	b, _ := os.ReadFile(p.logPath)
	return "service log:\n" + string(b) + "\ngo log:\n" + p.goLog.String()
}

// turn runs one real ordinary turn on the topic thread and releases it.
func (p *parity) turn(text string) {
	p.t.Helper()
	ctx := context.Background()
	thread, model, topic := parityThread, parityModel, int64(5)
	sub, err := p.eng.SubmitOrdinary(ctx, engine.Submit{
		ThreadID: &thread, Root: p.root, Model: &model,
		Route: &engine.Route{Chat: -100, Topic: &topic}, Text: text, Delivery: engine.DeliveryQueue,
	})
	if err != nil {
		p.t.Fatalf("submit: %v\n%s", err, p.log())
	}
	for range sub.Turn.Events {
	}
	end := sub.Turn.Wait()
	if !end.Ran {
		p.t.Fatalf("the turn did not run:\n%s", p.log())
	}
	p.t.Logf("turn %d held=%v holder token present=%v", end.TurnID, end.Held, sub.Holder != "")
	if end.Held {
		// The hold token comes from the submit reply (a service before
		// hold_tokens sends none, and then the release goes without one).
		if err := p.eng.ReleaseHeld(ctx, thread, end.TurnID, sub.Holder); err != nil {
			p.t.Fatalf("release: %v\n%s", err, p.log())
		}
	}
	for deadline := time.Now().Add(10 * time.Second); ; {
		rep, err := p.eng.StatusReport(ctx, &thread)
		if err == nil && len(rep.Threads) == 1 && rep.Threads[0].Settled {
			return
		}
		if time.Now().After(deadline) {
			p.t.Fatalf("thread never settled: %+v %v\n%s", rep, err, p.log())
		}
		time.Sleep(20 * time.Millisecond)
	}
}

type answer struct {
	status int
	header http.Header
	body   []byte
}

func (p *parity) get(path string) answer {
	p.t.Helper()
	req, _ := http.NewRequest(http.MethodGet, p.http.URL+path, nil)
	req.Header.Set("Authorization", "tma "+initData(parityUser))
	res, err := http.DefaultClient.Do(req)
	if err != nil {
		p.t.Fatal(err)
	}
	defer res.Body.Close()
	body, _ := io.ReadAll(res.Body)
	return answer{res.StatusCode, res.Header, body}
}

// socket asks the service for a view directly.
func (p *parity) socket(v engine.View) (engine.ViewOutput, error) {
	return p.eng.View(context.Background(), v)
}

// cli runs the `zdx views` twin of a view on the same home and returns the
// view's body.
func (p *parity) cli(request string) []byte {
	p.t.Helper()
	cmd := exec.Command(p.bin, "--root", p.root, "views")
	cmd.Dir, cmd.Env = p.root, p.env
	cmd.Stdin = strings.NewReader(request)
	var stdout, stderr bytes.Buffer
	cmd.Stdout, cmd.Stderr = &stdout, &stderr
	if err := cmd.Run(); err != nil {
		p.t.Fatalf("zdx views %s: %v\n%s", request, err, stderr.String())
	}
	var out struct {
		Body json.RawMessage `json:"body"`
	}
	if err := json.Unmarshal(stdout.Bytes(), &out); err != nil || len(out.Body) == 0 {
		p.t.Fatalf("zdx views %s: no body: %v %.300s", request, err, stdout.Bytes())
	}
	return out.Body
}

// volatile are values that change between two reads of the same state:
// times since now and the dashboard's build time.
var volatile = map[string]bool{
	"age": true, "uptime": true, "generated_at": true, "subscriptions_age": true,
	"observed_at": true, "relative_time": true,
}

func normalized(t *testing.T, raw []byte, drop ...string) string {
	t.Helper()
	var value any
	dec := json.NewDecoder(bytes.NewReader(raw))
	dec.UseNumber()
	if err := dec.Decode(&value); err != nil {
		t.Fatalf("not JSON: %v: %.300s", err, raw)
	}
	skip := map[string]bool{}
	for _, key := range drop {
		skip[key] = true
	}
	var clean func(any) any
	clean = func(v any) any {
		switch v := v.(type) {
		case map[string]any:
			out := map[string]any{}
			for key, value := range v {
				if !volatile[key] && !skip[key] {
					out[key] = clean(value)
				}
			}
			return out
		case []any:
			for i := range v {
				v[i] = clean(v[i])
			}
		}
		return v
	}
	out, _ := json.Marshal(clean(value))
	return string(out)
}

func sameJSON(t *testing.T, what string, got, want []byte, drop ...string) {
	t.Helper()
	if g, w := normalized(t, got, drop...), normalized(t, want, drop...); g != w {
		t.Fatalf("%s differs\n  go: %.600s\nrust: %.600s", what, g, w)
	}
}

func sha(b []byte) string {
	sum := sha256.Sum256(b)
	return hex.EncodeToString(sum[:])
}

// TestRealRouteParity compares every Mini App read route, served by Go
// through the real adapter, with the same view read straight from the
// isolated Rust service (and, for some, from the `zdx views` CLI), on one
// real thread made by a real turn. Rust's own HTTP server is not part of
// it: the reference is the view provider server.rs installs in the
// service, which calls the route handlers.
func TestRealRouteParity(t *testing.T) {
	p := startParity(t)
	thread := parityThread
	str := func(s string) *string { return &s }

	t.Run("thread list", func(t *testing.T) {
		got := p.get("/api/threads")
		want, err := p.socket(engine.View{Op: engine.ViewThreadsList})
		if err != nil || got.status != http.StatusOK {
			t.Fatalf("%d %s / %v", got.status, got.body, err)
		}
		sameJSON(t, "threads", got.body, want.Body)
		if !bytes.Contains(got.body, []byte(parityThread)) {
			t.Fatalf("the turn's thread is not listed: %s", got.body)
		}
		got = p.get("/api/threads?kind=workers")
		want, _ = p.socket(engine.View{Op: engine.ViewThreadsList, Kind: str("workers")})
		sameJSON(t, "worker threads", got.body, want.Body)
	})

	t.Run("thread view and delivering", func(t *testing.T) {
		got := p.get("/api/threads/" + parityThread)
		want, err := p.socket(engine.View{Op: engine.ViewThreadView, ThreadID: &thread})
		if err != nil || got.status != http.StatusOK {
			t.Fatalf("%d %s / %v", got.status, got.body, err)
		}
		if want.SourceID == nil || *want.SourceID != parityThread {
			t.Fatalf("source_id %v", want.SourceID)
		}
		sameJSON(t, "thread view", got.body, want.Body)
		sameJSON(t, "thread view (CLI)", got.body, p.cli(`{"op":"thread_view","thread_id":"`+parityThread+`"}`))
		for _, needle := range []string{"show me the page", "PARITY-ANSWER with <b>&</b>", "page.html", "picture.png", `"delivering":false`} {
			if !bytes.Contains(got.body, []byte(needle)) {
				t.Fatalf("thread view lacks %q: %.800s", needle, got.body)
			}
		}

		var full struct{ Cursor uint64 }
		_ = json.Unmarshal(got.body, &full)
		after := full.Cursor
		delta := p.get(fmt.Sprintf("/api/threads/%s?after=%d", parityThread, after))
		wantDelta, _ := p.socket(engine.View{Op: engine.ViewThreadView, ThreadID: &thread, After: &after})
		sameJSON(t, "thread delta", delta.body, wantDelta.Body)

		done := p.tracker.Start(*want.SourceID)
		running := p.get("/api/threads/" + parityThread)
		done()
		if !bytes.Contains(running.body, []byte(`"delivering":true`)) {
			t.Fatalf("a running delivery is not reported: %.300s", running.body)
		}
		sameJSON(t, "thread view while delivering", running.body, want.Body, "delivering")
	})

	t.Run("trajectory and workers", func(t *testing.T) {
		for _, c := range []struct {
			path string
			view engine.View
		}{
			{"/api/threads/" + parityThread + "/trajectory", engine.View{Op: engine.ViewThreadTrajectory, ThreadID: &thread}},
			{"/api/threads/" + parityThread + "/workers", engine.View{Op: engine.ViewThreadWorkers, ThreadID: &thread}},
		} {
			got := p.get(c.path)
			want, err := p.socket(c.view)
			if err != nil || got.status != http.StatusOK {
				t.Fatalf("%s: %d %s / %v", c.path, got.status, got.body, err)
			}
			sameJSON(t, c.path, got.body, want.Body)
		}
	})

	// Whatever the engine answers, Go answers the same: the body on success,
	// the code's status and the engine's text on a refusal.
	t.Run("outcomes come from the engine", func(t *testing.T) {
		missing := "telegram--100-topic-999"
		cases := []struct {
			path string
			view engine.View
		}{
			{"/api/threads/" + missing, engine.View{Op: engine.ViewThreadView, ThreadID: &missing}},
			{"/api/threads/" + missing + "/trajectory", engine.View{Op: engine.ViewThreadTrajectory, ThreadID: &missing}},
			{"/api/threads/" + parityThread + "/artifacts/file?path=%2Fetc%2Fhosts", engine.View{Op: engine.ViewArtifactResolve, ThreadID: &thread, Path: str("/etc/hosts")}},
			{"/api/threads/" + parityThread + "/artifacts/version?path=nope.txt", engine.View{Op: engine.ViewArtifactVersion, ThreadID: &thread, Path: str("nope.txt")}},
			{"/api/threads/" + parityThread + "/artifacts/image?path=" + url.QueryEscape(p.page), engine.View{Op: engine.ViewArtifactCanonical, ThreadID: &thread, Path: str(p.page)}},
		}
		status := map[string]int{"invalid": 400, "not_found": 404, "artifact_not_found": 404, "unsupported_artifact": 422}
		for _, c := range cases {
			got := p.get(c.path)
			out, err := p.socket(c.view)
			var se *engine.ServiceError
			switch {
			case err == nil:
				if got.status != http.StatusOK {
					t.Errorf("%s: engine answered, Go %d %q", c.path, got.status, got.body)
					continue
				}
				sameJSON(t, c.path, got.body, out.Body)
				t.Logf("%s: engine answered 200", c.path)
			case errors.As(err, &se):
				if got.status != status[se.Code] || string(got.body) != se.Message {
					t.Errorf("%s: go %d %q, engine %s %q", c.path, got.status, got.body, se.Code, se.Message)
				}
				t.Logf("%s: %d %s %q", c.path, got.status, se.Code, se.Message)
			default:
				t.Fatalf("%s: %v", c.path, err)
			}
		}
	})

	t.Run("artifacts", func(t *testing.T) {
		got := p.get("/api/threads/" + parityThread + "/artifacts")
		want, err := p.socket(engine.View{Op: engine.ViewArtifactsList, ThreadID: &thread})
		if err != nil || got.status != http.StatusOK {
			t.Fatalf("%d %s / %v", got.status, got.body, err)
		}
		sameJSON(t, "artifacts", got.body, want.Body)
		sameJSON(t, "artifacts (CLI)", got.body, p.cli(`{"op":"artifacts_list","thread_id":"`+parityThread+`"}`))
		var list struct {
			Artifacts []struct{ Path, Name string } `json:"artifacts"`
		}
		_ = json.Unmarshal(got.body, &list)
		paths := map[string]string{}
		for _, a := range list.Artifacts {
			paths[filepath.Base(a.Path)] = a.Path
		}
		if paths["page.html"] == "" || paths["picture.png"] == "" {
			t.Fatalf("listed %+v", list.Artifacts)
		}
		listed := url.QueryEscape(paths["page.html"])

		file := p.get("/api/threads/" + parityThread + "/artifacts/file?path=" + listed)
		resolved, _ := p.socket(engine.View{Op: engine.ViewArtifactResolve, ThreadID: &thread, Path: str(paths["page.html"])})
		var r struct{ Path, Mime, Filename string }
		_ = json.Unmarshal(resolved.Body, &r)
		page, _ := os.ReadFile(p.page)
		if file.status != http.StatusOK || !bytes.Equal(file.body, page) {
			t.Fatalf("file: %d %q", file.status, file.body)
		}
		if file.header.Get("Content-Type") != r.Mime || file.header.Get("Content-Disposition") != `inline; filename="`+r.Filename+`"` ||
			r.Filename != "page.html" || file.header.Get("Cache-Control") != "private, no-store" {
			t.Fatalf("file headers %v, engine %+v", file.header, r)
		}

		version := p.get("/api/threads/" + parityThread + "/artifacts/version?path=" + listed)
		wantVersion, _ := p.socket(engine.View{Op: engine.ViewArtifactVersion, ThreadID: &thread, Path: str(paths["page.html"])})
		if version.status != http.StatusOK || !bytes.Equal(version.body, wantVersion.Body) {
			t.Fatalf("version: %d %s, engine %s", version.status, version.body, wantVersion.Body)
		}
		var v struct {
			SHA256    string `json:"sha256"`
			SizeBytes int    `json:"size_bytes"`
		}
		_ = json.Unmarshal(version.body, &v)
		if v.SHA256 != sha(page) || v.SizeBytes != len(page) {
			t.Fatalf("version %+v is not the file's (%s, %d)", v, sha(page), len(page))
		}

		img := p.get("/api/threads/" + parityThread + "/artifacts/image?path=" + url.QueryEscape(paths["picture.png"]))
		if img.status != http.StatusOK {
			t.Fatalf("image: %d %s\n%s", img.status, img.body, p.log())
		}
		decoded, format, err := image.Decode(bytes.NewReader(img.body))
		if err != nil {
			t.Fatalf("the canonical copy is not an image: %v", err)
		}
		h := img.header
		if h.Get("Content-Type") != "image/"+format ||
			h.Get("X-Zdx-Source-Sha256") != sha(p.pictureB) ||
			h.Get("X-Zdx-Canonical-Sha256") != sha(img.body) ||
			h.Get("X-Zdx-Canonical-Width") != strconv.Itoa(decoded.Bounds().Dx()) ||
			h.Get("X-Zdx-Canonical-Height") != strconv.Itoa(decoded.Bounds().Dy()) ||
			h.Get("X-Zdx-Source-Width") != "40" || h.Get("X-Zdx-Source-Height") != "30" {
			t.Fatalf("image identity headers %v (decoded %s %v)", h, format, decoded.Bounds())
		}
	})

	t.Run("subscription_quotas is retired", func(t *testing.T) {
		// The Rust view is gone: the socket refuses the op as an invalid
		// request and `zdx views` fails, so no caller can read it by accident.
		line, err := socket.NewClient(p.sockPath).Call(context.Background(), []byte(`{"op":"subscription_quotas"}`), 0)
		if err != nil {
			t.Fatal(err)
		}
		var reply protocol.Reply
		if err := json.Unmarshal(line, &reply); err != nil {
			t.Fatalf("%s: %v", line, err)
		}
		if reply.Kind != protocol.ReplyError || reply.Code != "invalid" || !strings.Contains(reply.Message, "invalid request") {
			t.Fatalf("want an invalid-request refusal, got %s", line)
		}
		cmd := exec.Command(p.bin, "--root", p.root, "views")
		cmd.Dir, cmd.Env = p.root, p.env
		cmd.Stdin = strings.NewReader(`{"op":"subscription_quotas"}`)
		var stdout, stderr bytes.Buffer
		cmd.Stdout, cmd.Stderr = &stdout, &stderr
		if err := cmd.Run(); err == nil || stdout.Len() != 0 || !strings.Contains(stderr.String(), "parse view request") {
			t.Fatalf("zdx views must refuse the retired view: err %v, stdout %q, stderr %q", err, stdout.String(), stderr.String())
		}

		// The Mini App reads quotas in process instead. The home has no
		// credentials and the reader no OPENCODE_API_KEY: every account is
		// not logged in, which the recorded body drops, so it is `[]`.
		noEnv := func(string) (string, bool) { return "", false }
		goEngine := &gateway.MiniAppEngine{Quota: quota.Options{Home: p.home, LookupEnv: noEnv, Version: "0.0.0-test"}}
		got, err := goEngine.SubscriptionQuotas(context.Background())
		if err != nil || string(got) != "[]" {
			t.Fatalf("Go quotas %s %v", got, err)
		}
	})

	t.Run("monitor usage workspaces", func(t *testing.T) {
		got := p.get("/api/monitor")
		snapshot, err := p.socket(engine.View{Op: engine.ViewMonitorSnapshot})
		if err != nil || got.status != http.StatusOK {
			t.Fatalf("%d %s / %v", got.status, got.body, err)
		}
		overlay := []string{"active_agents", "subscriptions", "subscriptions_status"}
		sameJSON(t, "monitor without the overlay", got.body, snapshot.Body, overlay...)
		agents, _ := p.socket(engine.View{Op: engine.ViewActiveAgents})
		var monitor map[string]json.RawMessage
		_ = json.Unmarshal(got.body, &monitor)
		sameJSON(t, "active agents", monitor["active_agents"], agents.Body)
		if s := string(monitor["subscriptions_status"]); s != `"pending"` && s != `"ready"` {
			t.Fatalf("subscriptions_status %s", s)
		}
		var keys, rustKeys []string
		var rust map[string]json.RawMessage
		_ = json.Unmarshal(snapshot.Body, &rust)
		for k := range monitor {
			keys = append(keys, k)
		}
		for k := range rust {
			rustKeys = append(rustKeys, k)
		}
		sort.Strings(keys)
		sort.Strings(rustKeys)
		if strings.Join(keys, ",") != strings.Join(rustKeys, ",") {
			t.Fatalf("monitor fields %v, Rust %v", keys, rustKeys)
		}

		usage := p.get("/api/usage?days=30")
		days := uint32(30)
		wantUsage, _ := p.socket(engine.View{Op: engine.ViewUsageReport, Days: &days})
		sameJSON(t, "usage", usage.body, wantUsage.Body)

		ws := p.get("/api/workspaces")
		wantWS, _ := p.socket(engine.View{Op: engine.ViewWorkspaces})
		sameJSON(t, "workspaces", ws.body, wantWS.Body)
	})

	// The Git routes are Go (the shared gitview package) on Rust's
	// thread_root view; Rust has no Git views of its own, so the answers are
	// checked here and the bodies by gitview's recorded Rust replies.
	t.Run("git", func(t *testing.T) {
		if !p.report.Has(engine.CapThreadRoot) {
			sandbox.RustFeatureMissing(t, fmt.Sprintf("binary %s lacks %s (it reports %v)", p.bin, engine.CapThreadRoot, p.report.Capabilities))
		}
		for _, retired := range []string{
			`{"op":"git_status","thread_id":"` + thread + `"}`,
			`{"op":"git_scope","thread_id":"` + thread + `","scope":"all"}`,
			`{"op":"git_diff","thread_id":"` + thread + `","path":"README.md","kind":"unstaged"}`,
		} {
			reply, err := socket.NewClient(p.sockPath).Call(context.Background(), []byte(retired), 0)
			if err != nil || !bytes.Contains(reply, []byte(`"reply":"error"`)) {
				t.Errorf("retired %s: %v %s", retired, err, reply)
			}
		}
		expect := func(path string, status int, body string) {
			t.Helper()
			got := p.get(path)
			if got.status != status || (body != "" && string(got.body) != body) {
				t.Errorf("%s: %d %.300q, want %d %q", path, got.status, got.body, status, body)
			}
		}
		missing, alias := "telegram--100-topic-404", "telegram--100-topic-6"
		expect("/api/git", http.StatusOK, "")
		expect("/api/git?thread_id=active", http.StatusOK, "")
		expect("/api/git?thread_id="+missing, http.StatusOK, "")
		expect("/api/git/scope?thread_id="+thread+"&scope=all", http.StatusOK, "")
		expect("/api/git/scope?thread_id="+thread+"&scope=zzz", http.StatusBadRequest, "Invalid Git scope")
		expect("/api/git/diff?thread_id="+thread+"&kind=unstaged&path=README.md", http.StatusOK, "")
		expect("/api/git/diff?thread_id="+thread+"&kind=untracked&path=new.txt", http.StatusOK, "")
		expect("/api/git/diff?thread_id="+thread+"&kind=all&path=README.md", http.StatusOK, "")
		expect("/api/git/diff?thread_id="+thread+"&kind=staged&path=new.txt", http.StatusNotFound, "Git file is no longer in that category")
		expect("/api/git/diff?thread_id="+thread+"&kind=worktree&path=new.txt", http.StatusBadRequest, "Invalid Git file category")
		expect("/api/git/diff?thread_id="+thread+"&kind=staged&path=..%2Fx", http.StatusBadRequest, "Invalid Git file path")
		status := p.get("/api/git?thread_id=" + parityThread)
		for _, needle := range []string{`"source":"thread"`, `"branch":"main"`, "README.md", "new.txt"} {
			if !bytes.Contains(status.body, []byte(needle)) {
				t.Fatalf("git status lacks %s: %s", needle, status.body)
			}
		}
		if body := p.get("/api/git?thread_id=" + missing).body; !bytes.Contains(body, []byte(`"source":"bot_root"`)) {
			t.Fatalf("missing thread status: %s", body)
		}

		// A mirror topic aliasing the thread: Rust resolves the alias, the
		// repository is the thread's, and the reported thread is the topic.
		meta := fmt.Sprintf(`{"type":"meta","schema_version":1,"alias_to":%q,"worker_topic":true,"ts":"2026-01-02T00:00:00Z"}`+"\n", parityThread)
		if err := os.WriteFile(filepath.Join(p.home, "threads", alias+".jsonl"), []byte(meta), 0o600); err != nil {
			t.Fatal(err)
		}
		if body := p.get("/api/git?thread_id=" + alias).body; !bytes.Contains(body, []byte(`"thread_id":"`+alias+`"`)) || !bytes.Contains(body, []byte(`"source":"thread"`)) {
			t.Fatalf("alias status: %s", body)
		}

		// thread_root has a `zdx views` twin with the socket's body.
		root, err := p.socket(engine.View{Op: engine.ViewThreadRoot, ThreadID: &alias})
		if err != nil {
			t.Fatal(err)
		}
		sameJSON(t, "thread_root (CLI)", root.Body, p.cli(`{"op":"thread_root","thread_id":"`+alias+`"}`))
		if !bytes.Contains(root.Body, []byte(`"effective_id":"`+parityThread+`"`)) {
			t.Fatalf("thread_root %s", root.Body)
		}
	})
}
