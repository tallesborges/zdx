package miniapp

import (
	"bytes"
	"context"
	"crypto/hmac"
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"io"
	"log/slog"
	"net/http"
	"net/http/httptest"
	"net/url"
	"strconv"
	"strings"
	"sync"
	"testing"
	"testing/fstest"
	"time"
)

const (
	testBotToken = "1234567890:TEST-BOT-TOKEN-FOR-UNIT-TESTS-ONLY"
	testUserID   = int64(279058397)
)

// fakeEngine answers each operation with its function, or ErrUnavailable
// when the function is nil, like a service without that capability.
type fakeEngine struct {
	mu    sync.Mutex
	calls []string

	threadList   func(kind string) (json.RawMessage, error)
	threadView   func(id string, after *uint64) (ThreadView, error)
	trajectory   func(id string) (json.RawMessage, error)
	workers      func(id string) (json.RawMessage, error)
	artifacts    func(id string) (json.RawMessage, error)
	resolve      func(id, path string) (ResolvedArtifact, error)
	version      func(id, path string) (json.RawMessage, error)
	canonical    func(ctx context.Context, id, path string) (CanonicalImage, error)
	feedback     func(ctx context.Context, req FeedbackSubmit) (FeedbackResult, error)
	usage        func(days int) (json.RawMessage, error)
	monitor      func() (json.RawMessage, error)
	activeAgents func() (json.RawMessage, error)
	quotas       func() (json.RawMessage, error)
	workspaces   func() (json.RawMessage, error)
	gitStatus    func(id string) (json.RawMessage, error)
	gitScope     func(id, scope string) (json.RawMessage, error)
	gitDiff      func(req GitDiff) (json.RawMessage, error)
}

func (f *fakeEngine) record(op string) {
	f.mu.Lock()
	f.calls = append(f.calls, op)
	f.mu.Unlock()
}

func (f *fakeEngine) count(op string) int {
	f.mu.Lock()
	defer f.mu.Unlock()
	n := 0
	for _, call := range f.calls {
		if call == op {
			n++
		}
	}
	return n
}

func (f *fakeEngine) total() int {
	f.mu.Lock()
	defer f.mu.Unlock()
	return len(f.calls)
}

func raw[T any](fn func() (T, error)) (T, error) {
	if fn == nil {
		var zero T
		return zero, ErrUnavailable
	}
	return fn()
}

func (f *fakeEngine) ThreadList(_ context.Context, kind string) (json.RawMessage, error) {
	f.record("threads_list")
	if f.threadList == nil {
		return nil, ErrUnavailable
	}
	return f.threadList(kind)
}

func (f *fakeEngine) ThreadView(_ context.Context, id string, after *uint64) (ThreadView, error) {
	f.record("thread_view")
	if f.threadView == nil {
		return ThreadView{}, ErrUnavailable
	}
	return f.threadView(id, after)
}

func (f *fakeEngine) ThreadTrajectory(_ context.Context, id string) (json.RawMessage, error) {
	f.record("thread_trajectory")
	if f.trajectory == nil {
		return nil, ErrUnavailable
	}
	return f.trajectory(id)
}

func (f *fakeEngine) ThreadWorkers(_ context.Context, id string) (json.RawMessage, error) {
	f.record("thread_workers")
	if f.workers == nil {
		return nil, ErrUnavailable
	}
	return f.workers(id)
}

func (f *fakeEngine) ArtifactsList(_ context.Context, id string) (json.RawMessage, error) {
	f.record("artifacts_list")
	if f.artifacts == nil {
		return nil, ErrUnavailable
	}
	return f.artifacts(id)
}

func (f *fakeEngine) ArtifactResolve(_ context.Context, id, path string) (ResolvedArtifact, error) {
	f.record("artifact_resolve")
	if f.resolve == nil {
		return ResolvedArtifact{}, ErrUnavailable
	}
	return f.resolve(id, path)
}

func (f *fakeEngine) ArtifactVersion(_ context.Context, id, path string) (json.RawMessage, error) {
	f.record("artifact_version")
	if f.version == nil {
		return nil, ErrUnavailable
	}
	return f.version(id, path)
}

func (f *fakeEngine) ArtifactCanonical(ctx context.Context, id, path string) (CanonicalImage, error) {
	f.record("artifact_canonical")
	if f.canonical == nil {
		return CanonicalImage{}, ErrUnavailable
	}
	return f.canonical(ctx, id, path)
}

func (f *fakeEngine) FeedbackSubmit(ctx context.Context, req FeedbackSubmit) (FeedbackResult, error) {
	f.record("feedback_submit")
	if f.feedback == nil {
		return FeedbackResult{}, ErrUnavailable
	}
	return f.feedback(ctx, req)
}

func (f *fakeEngine) UsageReport(_ context.Context, days int) (json.RawMessage, error) {
	f.record("usage_report")
	if f.usage == nil {
		return nil, ErrUnavailable
	}
	return f.usage(days)
}

func (f *fakeEngine) MonitorSnapshot(context.Context) (json.RawMessage, error) {
	f.record("monitor_snapshot")
	return raw(f.monitor)
}

func (f *fakeEngine) ActiveAgents(context.Context) (json.RawMessage, error) {
	f.record("active_agents")
	return raw(f.activeAgents)
}

func (f *fakeEngine) SubscriptionQuotas(context.Context) (json.RawMessage, error) {
	f.record("quotas")
	return raw(f.quotas)
}

func (f *fakeEngine) Workspaces(context.Context) (json.RawMessage, error) {
	f.record("workspaces")
	return raw(f.workspaces)
}

func (f *fakeEngine) GitStatus(_ context.Context, id string) (json.RawMessage, error) {
	f.record("git_status")
	if f.gitStatus == nil {
		return nil, ErrUnavailable
	}
	return f.gitStatus(id)
}

func (f *fakeEngine) GitScope(_ context.Context, id, scope string) (json.RawMessage, error) {
	f.record("git_scope")
	if f.gitScope == nil {
		return nil, ErrUnavailable
	}
	return f.gitScope(id, scope)
}

func (f *fakeEngine) GitDiff(_ context.Context, req GitDiff) (json.RawMessage, error) {
	f.record("git_diff")
	if f.gitDiff == nil {
		return nil, ErrUnavailable
	}
	return f.gitDiff(req)
}

type gate struct {
	allow  bool
	routes *[]FeedbackRoute
}

func (g gate) Allows(_ int64, route FeedbackRoute) bool {
	if g.routes != nil {
		*g.routes = append(*g.routes, route)
	}
	return g.allow
}

var testAssets = fstest.MapFS{
	"index.html":             {Data: []byte(`<!doctype html><html><head><script type="module" src="/app/assets/index-abc123.js"></script></head><body><div id="app"></div></body></html>`)},
	"assets/index-abc123.js": {Data: []byte(strings.Repeat("export const value = 'compressible text';\n", 400))},
	"assets/logo-1.png":      {Data: []byte(strings.Repeat("\x89PNG not really", 100))},
}

type testServer struct {
	t      *testing.T
	engine *fakeEngine
	cfg    Config
	srv    *httptest.Server
}

func newTestServer(t *testing.T, engine *fakeEngine, configure ...func(*Config)) *testServer {
	t.Helper()
	cfg := Config{
		BotToken:       testBotToken,
		AllowedUserIDs: []int64{testUserID},
		Assets:         testAssets,
		Engine:         engine,
		Feedback:       gate{allow: true},
		Logger:         slog.New(slog.NewTextHandler(io.Discard, nil)),
	}
	for _, fn := range configure {
		fn(&cfg)
	}
	handler, err := New(cfg)
	if err != nil {
		t.Fatalf("New: %v", err)
	}
	srv := httptest.NewServer(handler)
	t.Cleanup(srv.Close)
	return &testServer{t: t, engine: engine, cfg: cfg, srv: srv}
}

// freshInitData builds initData the server accepts now for userID.
func freshInitData(userID int64, authDate time.Time) string {
	date := strconv.FormatInt(authDate.Unix(), 10)
	user := `{"id":` + strconv.FormatInt(userID, 10) + `}`
	secret := hmac.New(sha256.New, []byte("WebAppData"))
	secret.Write([]byte(testBotToken))
	mac := hmac.New(sha256.New, secret.Sum(nil))
	mac.Write([]byte("auth_date=" + date + "\nuser=" + user))
	values := url.Values{}
	values.Set("auth_date", date)
	values.Set("user", user)
	values.Set("hash", hex.EncodeToString(mac.Sum(nil)))
	return values.Encode()
}

type response struct {
	status int
	header http.Header
	body   string
}

func (ts *testServer) do(method, path string, body io.Reader, headers map[string]string) response {
	ts.t.Helper()
	req, err := http.NewRequest(method, ts.srv.URL+path, body)
	if err != nil {
		ts.t.Fatalf("request %s: %v", path, err)
	}
	for key, value := range headers {
		req.Header.Set(key, value)
	}
	// Raw bytes: the transport would decode gzip transparently otherwise.
	transport := &http.Transport{DisableCompression: true}
	defer transport.CloseIdleConnections()
	res, err := (&http.Client{Transport: transport, CheckRedirect: func(*http.Request, []*http.Request) error {
		return http.ErrUseLastResponse
	}}).Do(req)
	if err != nil {
		ts.t.Fatalf("%s %s: %v", method, path, err)
	}
	defer res.Body.Close()
	data, err := io.ReadAll(res.Body)
	if err != nil {
		ts.t.Fatalf("read %s: %v", path, err)
	}
	return response{res.StatusCode, res.Header, string(data)}
}

func (ts *testServer) get(path string) response {
	return ts.do(http.MethodGet, path, nil, map[string]string{
		"Authorization": "tma " + freshInitData(testUserID, time.Now()),
	})
}

func (ts *testServer) post(path, body string) response {
	return ts.do(http.MethodPost, path, strings.NewReader(body), map[string]string{
		"Authorization": "tma " + freshInitData(testUserID, time.Now()),
		"Content-Type":  "application/json",
	})
}

func expect(t *testing.T, got response, status int, body string) {
	t.Helper()
	if got.status != status {
		t.Fatalf("status = %d, want %d (body %q)", got.status, status, got.body)
	}
	if body != "" && got.body != body {
		t.Fatalf("body = %q, want %q", got.body, body)
	}
}

func jsonOf(t *testing.T, body string) map[string]any {
	t.Helper()
	var decoded map[string]any
	if err := json.Unmarshal([]byte(body), &decoded); err != nil {
		t.Fatalf("decode %q: %v", body, err)
	}
	return decoded
}

func discardLogger() *slog.Logger { return slog.New(slog.NewTextHandler(io.Discard, nil)) }

func newHTTPServer(t *testing.T, handler http.Handler) *httptest.Server {
	t.Helper()
	srv := httptest.NewServer(handler)
	t.Cleanup(srv.Close)
	return srv
}

// doRaw sends the path exactly as given (no client-side normalization).
func (ts *testServer) doRaw(method, rawPath string, body []byte, headers map[string]string) response {
	ts.t.Helper()
	req, err := http.NewRequest(method, ts.srv.URL, bytes.NewReader(body))
	if err != nil {
		ts.t.Fatal(err)
	}
	path, query, hasQuery := strings.Cut(rawPath, "?")
	req.URL.Opaque = path
	req.URL.RawQuery = query
	req.URL.ForceQuery = hasQuery && query == ""
	for key, value := range headers {
		req.Header.Set(key, value)
	}
	transport := &http.Transport{DisableCompression: true}
	defer transport.CloseIdleConnections()
	res, err := (&http.Client{Transport: transport}).Do(req)
	if err != nil {
		ts.t.Fatalf("%s %s: %v", method, rawPath, err)
	}
	defer res.Body.Close()
	data, err := io.ReadAll(res.Body)
	if err != nil {
		ts.t.Fatal(err)
	}
	return response{res.StatusCode, res.Header, string(data)}
}
