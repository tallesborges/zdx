package miniapp

import (
	"compress/gzip"
	"encoding/json"
	"io"
	"net/http"
	"os"
	"path/filepath"
	"regexp"
	"sort"
	"strings"
	"testing"
	"time"
)

// Telegram's documented sample, as used by the Rust tests.
const (
	sampleAuthDate = uint64(1662771648)
	sampleInitData = "query_id=AAHdF6IQAAAAAN0XohDhrOrc&" +
		"user=%7B%22id%22%3A279058397%2C%22first_name%22%3A%22Vladislav%22%2C" +
		"%22last_name%22%3A%22Kibenko%22%2C%22username%22%3A%22vdkfrost%22%2C" +
		"%22language_code%22%3A%22ru%22%2C%22is_premium%22%3Atrue%7D&" +
		"auth_date=1662771648&" +
		"hash=c4d45b6d00cd75400ade8589bba0ac122375ae8594ecc3495de5fb63c3697c17"
)

var sampleAllowlist = map[int64]struct{}{testUserID: {}}

func TestInitDataValidation(t *testing.T) {
	cases := []struct {
		name     string
		data     string
		allowed  map[int64]struct{}
		now      uint64
		wantID   int64
		wantFail error
	}{
		{"official sample", sampleInitData, sampleAllowlist, sampleAuthDate + 60, testUserID, nil},
		{"tampered", strings.Replace(sampleInitData, "Vladislav", "Mallory", 1), sampleAllowlist, sampleAuthDate + 60, 0, initDataInvalidSignature},
		{"expired", sampleInitData, sampleAllowlist, sampleAuthDate + initDataMaxAgeSecs + 1, 0, initDataExpired},
		{"at max age", sampleInitData, sampleAllowlist, sampleAuthDate + initDataMaxAgeSecs, testUserID, nil},
		{"future within skew", sampleInitData, sampleAllowlist, sampleAuthDate - initDataFutureSkewSecs, testUserID, nil},
		{"future past skew", sampleInitData, sampleAllowlist, sampleAuthDate - initDataFutureSkewSecs - 1, 0, initDataExpired},
		{"not allowlisted", sampleInitData, map[int64]struct{}{}, sampleAuthDate + 60, 0, initDataForbidden},
		{"duplicate hash", sampleInitData + "&hash=00", sampleAllowlist, sampleAuthDate + 60, 0, initDataMalformed},
		{"duplicate auth_date", sampleInitData + "&auth_date=1662771648", sampleAllowlist, sampleAuthDate + 60, 0, initDataMalformed},
		{"duplicate user", sampleInitData + "&user=%7B%22id%22%3A1%7D", sampleAllowlist, sampleAuthDate + 60, 0, initDataMalformed},
		{"short hash", strings.Replace(sampleInitData, "hash=c4", "hash=", 1), sampleAllowlist, sampleAuthDate + 60, 0, initDataMalformed},
		{"no hash", strings.Split(sampleInitData, "&hash=")[0], sampleAllowlist, sampleAuthDate + 60, 0, initDataMalformed},
		{"empty", "", sampleAllowlist, sampleAuthDate, 0, initDataMissing},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			id, err := validateInitData(tc.data, testBotToken, tc.allowed, tc.now)
			if err != tc.wantFail || id != tc.wantID {
				t.Fatalf("got (%d, %v), want (%d, %v)", id, err, tc.wantID, tc.wantFail)
			}
		})
	}
}

func TestInitDataUserObject(t *testing.T) {
	for raw, want := range map[string]bool{
		`{"id":279058397,"first_name":"A"}`: true,
		`{"first_name":"A","id":279058397}`: true,
		`{"id":1,"id":279058397}`:           false,
		`{"ID":279058397}`:                  false,
		`{"id":"279058397"}`:                false,
		`{"id":279058397.5}`:                false,
		`{"id":279058397} x`:                false,
		`[279058397]`:                       false,
	} {
		id, ok := userID(raw)
		if ok != want || (ok && id != testUserID) {
			t.Errorf("userID(%s) = %d, %v; want ok=%v", raw, id, ok, want)
		}
	}
}

func TestAuthorizationHeaderStatuses(t *testing.T) {
	engine := &fakeEngine{workspaces: func() (json.RawMessage, error) { return json.RawMessage(`{"workspaces":[]}`), nil }}
	ts := newTestServer(t, engine)
	cases := []struct {
		header string
		status int
		body   string
	}{
		{"", http.StatusUnauthorized, "Telegram authorization required"},
		{"tma", http.StatusUnauthorized, "Invalid Telegram authorization"},
		{"Bearer " + freshInitData(testUserID, time.Now()), http.StatusUnauthorized, "Invalid Telegram authorization"},
		{"tma " + sampleInitData, http.StatusUnauthorized, "Invalid or expired Telegram authorization"},
		{"tma " + freshInitData(42, time.Now()), http.StatusForbidden, "Telegram user is not allowed"},
		{"tma " + freshInitData(testUserID, time.Now().Add(-2*time.Hour)), http.StatusUnauthorized, "Invalid or expired Telegram authorization"},
		{"TMA " + freshInitData(testUserID, time.Now()), http.StatusOK, `{"workspaces":[]}`},
	}
	for _, tc := range cases {
		headers := map[string]string{}
		if tc.header != "" {
			headers["Authorization"] = tc.header
		}
		expect(t, ts.do(http.MethodGet, "/api/workspaces", nil, headers), tc.status, tc.body)
	}
	if got := engine.count("workspaces"); got != 1 {
		t.Fatalf("engine reached %d times, want only the authorized request", got)
	}
}

// Every API route refuses an unauthenticated request before the engine sees
// it, while the shell pages stay public (server.rs
// `protects_every_api_route_but_not_the_miniapp_page`).
func TestProtectsEveryAPIRouteButNotTheShell(t *testing.T) {
	engine := &fakeEngine{}
	ts := newTestServer(t, engine)
	for _, route := range apiRoutes {
		got := ts.do(route.method, route.path, strings.NewReader("{}"), map[string]string{"Content-Type": "application/json"})
		expect(t, got, http.StatusUnauthorized, "Telegram authorization required")
	}
	for _, path := range []string{"/app", "/threads", "/monitor"} {
		expect(t, ts.do(http.MethodGet, path, nil, nil), http.StatusOK, "")
	}
	expect(t, ts.do(http.MethodGet, "/api/nope", nil, nil), http.StatusNotFound, "")
	if engine.total() != 0 {
		t.Fatalf("engine was called %d times without authorization", engine.total())
	}
}

var apiRoutes = []struct{ method, path string }{
	{"GET", "/api/threads"},
	{"GET", "/api/threads/active"},
	{"GET", "/api/threads/active/trajectory"},
	{"GET", "/api/threads/active/workers"},
	{"GET", "/api/threads/active/artifacts"},
	{"GET", "/api/threads/active/artifacts/file?path=x"},
	{"GET", "/api/threads/active/artifacts/version?path=x"},
	{"GET", "/api/threads/active/artifacts/image?path=x"},
	{"POST", "/api/threads/telegram-1/artifacts/feedback"},
	{"GET", "/api/monitor"},
	{"GET", "/api/workspaces"},
	{"GET", "/api/usage"},
	{"GET", "/api/git?thread_id=active"},
	{"GET", "/api/git/scope?thread_id=active&scope=all"},
	{"GET", "/api/git/diff?thread_id=active&kind=unstaged&path=README.md"},
}

// rustRoutes reads testdata/rust_routes.json: method and path of every route
// of the removed Rust Mini App server (create_router in zdx-bot's server.rs),
// split into API routes (nested under /api) and shell routes.
func rustRoutes(t *testing.T) (api, shell [][2]string) {
	t.Helper()
	source, err := os.ReadFile(filepath.Join("testdata", "rust_routes.json"))
	if err != nil {
		t.Fatal(err)
	}
	var routes struct {
		API   [][2]string `json:"api"`
		Shell [][2]string `json:"shell"`
	}
	if err := json.Unmarshal(source, &routes); err != nil {
		t.Fatal(err)
	}
	return routes.API, routes.Shell
}

// The Go API answers exactly the Rust routes, with the same method rules:
// the right method needs authorization; a wrong method is authorized first
// and then 405 with Allow; a trailing slash is another path (404).
func TestServesEveryRouteOfTheRustRouterWithItsMethod(t *testing.T) {
	api, shell := rustRoutes(t)
	if len(api) < 15 || len(shell) < 4 {
		t.Fatalf("parsed only %d API and %d shell routes", len(api), len(shell))
	}

	srv := newServer(Config{BotToken: testBotToken, Engine: &fakeEngine{}})
	var goRoutes []string
	for _, route := range srv.apiRoutes() {
		goRoutes = append(goRoutes, route.method+" /"+strings.Join(route.segments, "/"))
	}
	var want []string
	for _, route := range api {
		want = append(want, route[0]+" "+route[1])
	}
	sort.Strings(goRoutes)
	sort.Strings(want)
	if strings.Join(goRoutes, "\n") != strings.Join(want, "\n") {
		t.Fatalf("Go API routes differ from server.rs:\n go: %v\nrust: %v", goRoutes, want)
	}

	ts := newTestServer(t, &fakeEngine{})
	auth := map[string]string{"Authorization": "tma " + freshInitData(testUserID, time.Now())}
	for _, route := range api {
		method, path := route[0], "/api"+strings.ReplaceAll(route[1], "{id}", "telegram-1")
		wrong, allow := http.MethodPost, "GET,HEAD"
		if method == http.MethodPost {
			wrong, allow = http.MethodGet, "POST"
		}
		got := ts.do(method, path, nil, nil)
		expect(t, got, http.StatusUnauthorized, "Telegram authorization required")
		if got.header.Get("Allow") != "" {
			t.Errorf("%s %s: Allow on the right method", method, path)
		}
		got = ts.do(wrong, path, nil, nil)
		expect(t, got, http.StatusUnauthorized, "Telegram authorization required")
		if got.header.Get("Allow") != allow {
			t.Errorf("%s %s: Allow = %q, want %q", wrong, path, got.header.Get("Allow"), allow)
		}
		got = ts.do(wrong, path, nil, auth)
		expect(t, got, http.StatusMethodNotAllowed, "")
		if got.header.Get("Allow") != allow {
			t.Errorf("%s %s with auth: Allow = %q, want %q", wrong, path, got.header.Get("Allow"), allow)
		}
		expect(t, ts.do(method, path+"/", nil, auth), http.StatusNotFound, "")
	}
	for _, route := range shell {
		path := strings.ReplaceAll(route[1], "{*path}", "index.html")
		expect(t, ts.do(route[0], path, nil, nil), http.StatusOK, "")
		got := ts.do(http.MethodPost, path, nil, nil)
		expect(t, got, http.StatusMethodNotAllowed, "")
		if got.header.Get("Allow") != "GET,HEAD" {
			t.Errorf("POST %s: Allow = %q", path, got.header.Get("Allow"))
		}
	}
}

// Every API path the Svelte client calls is served.
func TestServesEveryPathTheFrontendCalls(t *testing.T) {
	source, err := os.ReadFile(filepath.Join("..", "..", "..", "web", "src", "lib", "api.ts"))
	if err != nil {
		t.Skipf("frontend source not present: %v", err)
	}
	pathRe := regexp.MustCompile("[\"`](/api/[^\"`]*)[\"`]")
	ts := newTestServer(t, &fakeEngine{})
	found := 0
	for _, match := range pathRe.FindAllStringSubmatch(string(source), -1) {
		if strings.Contains(match[1], "*") {
			continue
		}
		path := strings.ReplaceAll(match[1], "${encodeURIComponent(id)}", "telegram-1")
		method := http.MethodGet
		if strings.HasSuffix(path, "/feedback") {
			method = http.MethodPost
		}
		expect(t, ts.do(method, path, nil, nil), http.StatusUnauthorized, "Telegram authorization required")
		// The method the client uses is the one the route takes.
		got := ts.do(method, path, nil, map[string]string{"Authorization": "tma " + freshInitData(testUserID, time.Now())})
		if got.status == http.StatusMethodNotAllowed || got.status == http.StatusNotFound {
			t.Errorf("%s %s: %d with authorization", method, path, got.status)
		}
		found++
	}
	if found < 14 {
		t.Fatalf("found only %d API paths in api.ts", found)
	}
}

func TestServesTheShellAndCacheableAssets(t *testing.T) {
	ts := newTestServer(t, &fakeEngine{})
	shell := ts.do(http.MethodGet, "/app", nil, nil)
	expect(t, shell, http.StatusOK, "")
	if got := shell.header.Get("Cache-Control"); got != "no-cache" {
		t.Fatalf("shell Cache-Control = %q", got)
	}
	if got := shell.header.Get("Content-Type"); got != "text/html; charset=utf-8" {
		t.Fatalf("shell Content-Type = %q", got)
	}
	if !strings.Contains(shell.body, `<div id="app">`) {
		t.Fatalf("not the Svelte shell: %q", shell.body)
	}

	asset := ts.do(http.MethodGet, "/app/assets/index-abc123.js", nil, nil)
	expect(t, asset, http.StatusOK, "")
	if got := asset.header.Get("Cache-Control"); got != "public, max-age=31536000, immutable" {
		t.Fatalf("asset Cache-Control = %q", got)
	}
	if got := asset.header.Get("Content-Type"); !strings.Contains(got, "javascript") {
		t.Fatalf("asset Content-Type = %q", got)
	}

	for _, path := range []string{
		"/app/assets/nope.js",
		"/app/assets",
		"/app/",
		"/app/%2e%2e/secret",
		"/app/assets/..%2findex.html",
		"/app//etc/hosts",
	} {
		if got := ts.do(http.MethodGet, path, nil, nil); got.status != http.StatusNotFound {
			t.Errorf("GET %s: status %d, want 404", path, got.status)
		}
	}
	expect(t, ts.do(http.MethodPost, "/app", nil, nil), http.StatusMethodNotAllowed, "")
}

func TestShellWithoutAssetsNamesTheBuildStep(t *testing.T) {
	ts := newTestServer(t, &fakeEngine{}, func(cfg *Config) { cfg.Assets = nil })
	expect(t, ts.do(http.MethodGet, "/app", nil, nil), http.StatusInternalServerError,
		"Mini App assets are missing; run `just web-build`")
}

func TestCompressesWhenTheClientAcceptsGzip(t *testing.T) {
	ts := newTestServer(t, &fakeEngine{})
	compressed := ts.do(http.MethodGet, "/app/assets/index-abc123.js", nil, map[string]string{"Accept-Encoding": "gzip"})
	if got := compressed.header.Get("Content-Encoding"); got != "gzip" {
		t.Fatalf("Content-Encoding = %q, want gzip", got)
	}
	reader, err := gzip.NewReader(strings.NewReader(compressed.body))
	if err != nil {
		t.Fatalf("gzip body: %v", err)
	}
	plain, err := io.ReadAll(reader)
	if err != nil {
		t.Fatalf("gunzip: %v", err)
	}
	identity := ts.do(http.MethodGet, "/app/assets/index-abc123.js", nil, map[string]string{"Accept-Encoding": "identity"})
	if identity.header.Get("Content-Encoding") != "" {
		t.Fatal("identity response was encoded")
	}
	if string(plain) != identity.body {
		t.Fatal("gzip body differs from the identity body")
	}
	if len(compressed.body)*2 >= len(identity.body) {
		t.Fatalf("gzip should at least halve the bundle: %d vs %d", len(compressed.body), len(identity.body))
	}

	refused := ts.do(http.MethodGet, "/app/assets/index-abc123.js", nil, map[string]string{"Accept-Encoding": "gzip;q=0"})
	if refused.header.Get("Content-Encoding") != "" {
		t.Fatal("gzip;q=0 was compressed")
	}
	image := ts.do(http.MethodGet, "/app/assets/logo-1.png", nil, map[string]string{"Accept-Encoding": "gzip"})
	if image.header.Get("Content-Encoding") != "" {
		t.Fatal("PNG was compressed")
	}
}
