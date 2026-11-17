package quota

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
	"path/filepath"
	"strings"
	"sync"
	"testing"
	"time"
)

var testNow = time.Date(2026, 8, 1, 12, 0, 0, 0, time.UTC)

// future/past are synthetic expiries around testNow, in Unix ms.
var (
	future = uint64(testNow.Add(time.Hour).UnixMilli())
	past   = uint64(testNow.Add(-time.Hour).UnixMilli())
)

type seen struct {
	method  string
	path    string
	headers http.Header
	body    string
}

// fakeProviders answers every endpoint from routes (path -> handler) and
// records each request.
type fakeProviders struct {
	server *httptest.Server
	mu     sync.Mutex
	reqs   []seen
	routes map[string]http.HandlerFunc
}

func newFake(t *testing.T, routes map[string]http.HandlerFunc) *fakeProviders {
	t.Helper()
	f := &fakeProviders{routes: routes}
	f.server = httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		body, _ := io.ReadAll(r.Body)
		f.mu.Lock()
		f.reqs = append(f.reqs, seen{method: r.Method, path: r.URL.Path, headers: r.Header.Clone(), body: string(body)})
		h := f.routes[r.URL.Path]
		f.mu.Unlock()
		if h == nil {
			http.NotFound(w, r)
			return
		}
		h(w, r)
	}))
	t.Cleanup(f.server.Close)
	return f
}

func (f *fakeProviders) endpoints() Endpoints {
	u := f.server.URL
	return Endpoints{
		ClaudeUsage: u + "/claude/usage", ClaudeProfile: u + "/claude/profile", CodexUsage: u + "/codex",
		AntigravityQuota: u + "/antigravity", GrokBilling: u + "/grok?format=credits",
		MuseCodeKey: u + "/muse",
		ClaudeToken: u + "/token/claude", CodexToken: u + "/token/codex",
		AntigravityToken: u + "/token/antigravity", GrokToken: u + "/token/grok",
	}
}

func (f *fakeProviders) requests(path string) []seen {
	f.mu.Lock()
	defer f.mu.Unlock()
	var out []seen
	for _, r := range f.reqs {
		if r.path == path {
			out = append(out, r)
		}
	}
	return out
}

func serve(body string) http.HandlerFunc {
	return func(w http.ResponseWriter, _ *http.Request) { io.WriteString(w, body) }
}

func status(code int, headers ...string) http.HandlerFunc {
	return func(w http.ResponseWriter, _ *http.Request) {
		for i := 0; i+1 < len(headers); i += 2 {
			w.Header().Set(headers[i], headers[i+1])
		}
		w.WriteHeader(code)
		io.WriteString(w, `{"error":"leaky body tok-SECRET"}`)
	}
}

type cred struct {
	Type      string  `json:"type"`
	Refresh   string  `json:"refresh"`
	Access    string  `json:"access"`
	Expires   uint64  `json:"expires"`
	AccountID *string `json:"account_id,omitempty"`
}

func writeCache(t *testing.T, home string, entries map[string]cred) {
	t.Helper()
	data, err := json.Marshal(entries)
	if err != nil {
		t.Fatal(err)
	}
	writeRaw(t, home, string(data))
}

func writeRaw(t *testing.T, home, contents string) {
	t.Helper()
	if err := os.WriteFile(filepath.Join(home, "oauth.json"), []byte(contents), 0o600); err != nil {
		t.Fatal(err)
	}
}

func env(vars map[string]string) func(string) (string, bool) {
	return func(k string) (string, bool) {
		v, ok := vars[k]
		return v, ok
	}
}

func options(home string, f *fakeProviders, vars map[string]string) Options {
	o := Options{Home: home, LookupEnv: env(vars), Now: func() time.Time { return testNow }, Version: "9.9.9-test"}
	if f != nil {
		o.Endpoints = f.endpoints()
	}
	return o
}

func allCreds() map[string]cred {
	acct := "acct-synthetic"
	return map[string]cred{
		"claude-cli":         {Type: "oauth", Refresh: "r-claude", Access: "tok-claude", Expires: future},
		"openai-codex":       {Type: "oauth", Refresh: "r-codex", Access: "tok-codex", Expires: future, AccountID: &acct},
		"google-antigravity": {Type: "oauth", Refresh: "r-ag", Access: "tok-ag", Expires: future, AccountID: str("proj-<1>&")},
		"grok-build":         {Type: "oauth", Refresh: "r-grok", Access: "tok-grok", Expires: future},
		"muse-code":          {Type: "oauth", Refresh: "id-muse", Access: "LLM|minted", Expires: past},
	}
}

func happyRoutes(t *testing.T) map[string]http.HandlerFunc {
	return map[string]http.HandlerFunc{
		"/claude/usage":   serve(string(fixture(t, "claude_usage.json"))),
		"/claude/profile": serve(`{"organization":{"organization_type":"claude_max","rate_limit_tier":"default_claude_max_20x"}}`),
		"/codex":          serve(string(fixture(t, "codex_usage.json"))),
		"/antigravity":    serve(string(fixture(t, "antigravity_usage.json"))),
		"/grok":           serve(string(fixture(t, "grok_usage.json"))),
		"/muse":           serve(`{"api_key":"LLM|fresh-minted","is_subs_active":true,"subs_tier_name":"Power Usage","subs_usage":{"window":{"used_percent":42,"resets_at":1800000000,"window_duration_mins":300}}}`),
	}
}

func TestFetchAllProvidersHeadersAndBodies(t *testing.T) {
	home := t.TempDir()
	writeCache(t, home, allCreds())
	f := newFake(t, happyRoutes(t))
	snap, err := Fetch(context.Background(), options(home, f, nil))
	if err != nil {
		t.Fatal(err)
	}
	if len(snap.Providers) != 5 {
		t.Fatalf("results = %d", len(snap.Providers))
	}
	for i, r := range snap.Providers {
		if r.Provider != providers[i] || r.Account != nil {
			t.Errorf("row %d = %s %v", i, r.Provider, r.Account)
		}
		if r.Err != nil || r.Quota == nil {
			t.Errorf("%s: %v", r.Provider, r.Err)
		}
	}
	if p := snap.Providers[0].Quota.Plan; p == nil || *p != "claude_max_20x" {
		t.Errorf("claude plan = %v", p)
	}

	check := func(path, method string, want map[string]string, absent ...string) seen {
		t.Helper()
		reqs := f.requests(path)
		if len(reqs) != 1 {
			t.Fatalf("%s: %d requests", path, len(reqs))
		}
		r := reqs[0]
		if r.method != method {
			t.Errorf("%s method = %s", path, r.method)
		}
		for k, v := range want {
			if got := r.headers.Get(k); got != v {
				t.Errorf("%s %s = %q, want %q", path, k, got, v)
			}
		}
		for _, k := range absent {
			if _, ok := r.headers[http.CanonicalHeaderKey(k)]; ok {
				t.Errorf("%s: unexpected %s = %q", path, k, r.headers.Get(k))
			}
		}
		return r
	}
	claude := map[string]string{
		"Authorization": "Bearer tok-claude", "anthropic-beta": "oauth-2025-04-20", "anthropic-version": "2023-06-01",
		"User-Agent": "claude-cli/2.1.2 (external, cli)", "anthropic-dangerous-direct-browser-access": "true", "x-app": "cli", "Accept": "*/*",
	}
	check("/claude/usage", "GET", claude, "Accept-Encoding")
	check("/claude/profile", "GET", claude)
	check("/codex", "GET", map[string]string{
		"Authorization": "Bearer tok-codex", "chatgpt-account-id": "acct-synthetic", "originator": "zdx", "User-Agent": "zdx/9.9.9-test", "Accept": "*/*",
	})
	ag := check("/antigravity", "POST", map[string]string{
		"Authorization": "Bearer tok-ag", "Accept": "application/json", "User-Agent": "antigravity/cli/2.24.0", "Content-Type": "application/json",
	})
	if ag.body != `{"project":"proj-<1>&"}` {
		t.Errorf("antigravity body = %s", ag.body)
	}
	check("/grok", "GET", map[string]string{
		"Authorization": "Bearer tok-grok", "X-XAI-Token-Auth": "xai-grok-cli", "x-grok-client-version": "1.0.0", "x-grok-client-mode": "interactive", "Accept": "*/*",
	}, "User-Agent")
	muse := check("/muse", "POST", map[string]string{
		"Authorization": "Bearer id-muse", "Content-Type": "application/json", "Accept": "application/json", "x-api-version": "1.0.0", "User-Agent": "zdx/9.9.9-test",
	})
	if muse.body != "{}" {
		t.Errorf("muse body = %s", muse.body)
	}
	if got := f.requests("/grok"); len(got) == 1 && got[0].headers.Get("Accept-Encoding") != "" {
		t.Errorf("Accept-Encoding sent")
	}
}

// The live quota summary dropped the Gemini groups for the legacy client UA
// `antigravity/cli/1.0.0` and returned only the Claude and GPT group, which
// the Gemini filter reads as unexpected response (observed 2026-11-14).
// This fake answers any `antigravity/cli/1.` UA that way.
func TestFetchAntigravityGetsGeminiGroupsWithCurrentClientUA(t *testing.T) {
	home := t.TempDir()
	writeCache(t, home, map[string]cred{
		"google-antigravity": {Type: "oauth", Refresh: "r-ag", Access: "tok-ag", Expires: future, AccountID: str("proj-synthetic")},
	})
	legacy := `{"groups":[{"displayName":"Claude and GPT models","buckets":[{"window":"weekly","remainingFraction":1}]}]}`
	current := string(fixture(t, "antigravity_usage_argon.json"))
	f := newFake(t, map[string]http.HandlerFunc{
		"/antigravity": func(w http.ResponseWriter, r *http.Request) {
			if strings.HasPrefix(r.Header.Get("User-Agent"), "antigravity/cli/1.") {
				io.WriteString(w, legacy)
				return
			}
			io.WriteString(w, current)
		},
	})
	snap, err := Fetch(context.Background(), options(home, f, nil))
	if err != nil {
		t.Fatal(err)
	}
	var ag *Result
	for i := range snap.Providers {
		if snap.Providers[i].Provider == ProviderAntigravity {
			ag = &snap.Providers[i]
		}
	}
	if ag == nil || ag.Err != nil || ag.Quota == nil {
		t.Fatalf("antigravity = %+v", ag)
	}
	var got []string
	for _, w := range ag.Quota.Windows {
		got = append(got, fmt.Sprintf("%s %s %g %s", *w.Scope, w.Label, w.UsedPercent, FormatRFC3339(*w.ResetsAt)))
	}
	want := []string{
		"Gemini Models weekly 25 2026-08-07T10:00:00+00:00",
		"Gemini Models 5h 50 2026-08-01T15:00:00+00:00",
		"Gemini 4 Argon weekly 100 2026-08-06T10:00:00+00:00",
	}
	if strings.Join(got, "|") != strings.Join(want, "|") {
		t.Errorf("windows =\n%v\nwant\n%v", got, want)
	}
}

func TestFetchWritesNothingAndKeepsCache(t *testing.T) {
	home := t.TempDir()
	writeCache(t, home, allCreds())
	before, _ := os.ReadFile(filepath.Join(home, "oauth.json"))
	stat, _ := os.Stat(filepath.Join(home, "oauth.json"))
	f := newFake(t, happyRoutes(t))
	if _, err := Fetch(context.Background(), options(home, f, nil)); err != nil {
		t.Fatal(err)
	}
	after, _ := os.ReadFile(filepath.Join(home, "oauth.json"))
	stat2, _ := os.Stat(filepath.Join(home, "oauth.json"))
	if string(before) != string(after) || !stat.ModTime().Equal(stat2.ModTime()) {
		t.Error("oauth.json changed")
	}
	entries, _ := os.ReadDir(home)
	if len(entries) != 1 {
		t.Errorf("home has %d entries; Fetch must not create files (no oauth.lock)", len(entries))
	}
}

func TestMissingCacheGivesDefaultNotLoggedInRows(t *testing.T) {
	snap, err := Fetch(context.Background(), options(t.TempDir(), nil, nil))
	if err != nil {
		t.Fatal(err)
	}
	if len(snap.Providers) != 5 {
		t.Fatalf("rows = %d", len(snap.Providers))
	}
	for i, r := range snap.Providers {
		if r.Provider != providers[i] || r.Account != nil || r.Quota != nil || r.Err == nil || r.Err.Kind != KindNotAuthenticated {
			t.Errorf("row %d = %+v", i, r)
		}
	}
	if subs := Subscriptions(snap); len(subs) != 0 || subs == nil {
		t.Errorf("subscriptions = %#v", subs)
	}
}

func TestAccountDiscoveryOrderAndKeys(t *testing.T) {
	home := t.TempDir()
	c := cred{Type: "oauth", Refresh: "r", Access: "a", Expires: future}
	writeCache(t, home, map[string]cred{
		"claude-cli@work": c, "claude-cli@Alpha": c, "claude-cli": c, "claude-cli@": c, "claude-cli@a@b": c,
		"grok-build@solo": c, "unknown-provider": c,
	})
	accounts, err := StoredAccounts(options(home, nil, nil))
	if err != nil {
		t.Fatal(err)
	}
	var got []string
	for _, a := range accounts {
		got = append(got, AccountDisplay(a.Provider, a.Account))
	}
	want := []string{"Claude", "Claude @Alpha", "Claude @a@b", "Claude @work", "Codex", "Antigravity", "Grok @solo", "Muse Code"}
	if strings.Join(got, "|") != strings.Join(want, "|") {
		t.Errorf("accounts =\n%v\nwant\n%v", got, want)
	}
}

func TestMultiAccountFetchUsesEachAccountsToken(t *testing.T) {
	home := t.TempDir()
	writeCache(t, home, map[string]cred{
		"claude-cli":      {Type: "oauth", Refresh: "r", Access: "tok-default", Expires: future},
		"claude-cli@work": {Type: "oauth", Refresh: "r", Access: "tok-work", Expires: future},
		"claude-cli@old":  {Type: "oauth", Refresh: "r", Access: "tok-old", Expires: past},
	})
	f := newFake(t, map[string]http.HandlerFunc{
		"/claude/usage": func(w http.ResponseWriter, r *http.Request) {
			if r.Header.Get("Authorization") == "Bearer tok-work" {
				io.WriteString(w, `{"five_hour":{"utilization":77}}`)
				return
			}
			io.WriteString(w, `{"five_hour":{"utilization":11}}`)
		},
		"/claude/profile": status(500),
	})
	snap, err := Fetch(context.Background(), options(home, f, nil))
	if err != nil {
		t.Fatal(err)
	}
	rows := snap.Providers[:3]
	if rows[0].Account != nil || rows[0].Quota.Windows[0].UsedPercent != 11 || rows[0].Quota.Plan != nil {
		t.Errorf("default = %+v", rows[0])
	}
	if *rows[1].Account != "old" || rows[1].Err.Kind != KindExpired {
		t.Errorf("old = %+v", rows[1])
	}
	if *rows[2].Account != "work" || rows[2].Quota.Windows[0].UsedPercent != 77 {
		t.Errorf("work = %+v", rows[2])
	}
	if n := len(f.requests("/claude/usage")); n != 2 {
		t.Errorf("expired account was fetched: %d usage requests", n)
	}
}

func TestCredentialRules(t *testing.T) {
	home := t.TempDir()
	writeCache(t, home, map[string]cred{
		"claude-cli":         {Type: "oauth", Refresh: "r", Access: "a", Expires: uint64(testNow.UnixMilli())}, // expiry == now is expired
		"openai-codex":       {Type: "oauth", Refresh: "r", Access: "a", Expires: future},                      // no account_id
		"google-antigravity": {Type: "oauth", Refresh: "r", Access: "tok-ag", Expires: future},                 // no project: ""
		"muse-code":          {Type: "oauth", Refresh: "id", Access: "k", Expires: 0},                          // muse ignores expiry
	})
	f := newFake(t, map[string]http.HandlerFunc{
		"/antigravity": serve(string(fixture(t, "antigravity_usage.json"))),
		"/muse":        serve(`{"is_subs_active":false,"subs_usage":{"weekly":{"used_percent":1}}}`),
	})
	snap, err := Fetch(context.Background(), options(home, f, nil))
	if err != nil {
		t.Fatal(err)
	}
	kinds := map[string]*Error{}
	for _, r := range snap.Providers {
		kinds[r.Provider] = r.Err
	}
	if kinds[ProviderClaude].Kind != KindExpired || kinds[ProviderCodex].Kind != KindNotAuthenticated {
		t.Errorf("claude %v codex %v", kinds[ProviderClaude], kinds[ProviderCodex])
	}
	if kinds[ProviderAntigravity] != nil {
		t.Errorf("antigravity %v", kinds[ProviderAntigravity])
	}
	if body := f.requests("/antigravity")[0].body; body != `{"project":""}` {
		t.Errorf("project body = %s", body)
	}
	if kinds[ProviderMuseCode] == nil || kinds[ProviderMuseCode].Kind != KindUnauthorized {
		t.Errorf("muse inactive = %v", kinds[ProviderMuseCode])
	}
	if len(f.requests("/claude/usage")) != 0 || len(f.requests("/codex")) != 0 {
		t.Error("expired or incomplete credentials were sent")
	}
}

func TestDiscoveryFailuresAreBoundedAndRedacted(t *testing.T) {
	cases := map[string]string{
		"not json":          `{"claude-cli": tok-SECRET`,
		"not an object":     `["tok-SECRET"]`,
		"null":              `null`,
		"missing field":     `{"claude-cli":{"type":"oauth","access":"tok-SECRET","expires":1}}`,
		"wrong type":        `{"claude-cli":{"type":"oauth","refresh":"r","access":"a","expires":"tok-SECRET"}}`,
		"float expires":     `{"claude-cli":{"type":"oauth","refresh":"r","access":"a","expires":1.5e12}}`,
		"negative expires":  `{"claude-cli":{"type":"oauth","refresh":"r","access":"a","expires":-1}}`,
		"duplicate field":   `{"claude-cli":{"type":"oauth","refresh":"tok-SECRET","refresh":"x","access":"a","expires":1}}`,
		"entry not object":  `{"claude-cli":"tok-SECRET"}`,
		"account_id number": `{"claude-cli":{"type":"oauth","refresh":"r","access":"a","expires":1,"account_id":7}}`,
		"invalid utf8":      "{\"claude-cli\":{\"type\":\"oauth\",\"refresh\":\"\xff tok-SECRET\",\"access\":\"a\",\"expires\":1}}",
		"empty":             ``,
	}
	for name, contents := range cases {
		home := t.TempDir()
		writeRaw(t, home, contents)
		_, err := Fetch(context.Background(), options(home, nil, nil))
		if !errors.Is(err, ErrDiscovery) {
			t.Errorf("%s: err = %v", name, err)
			continue
		}
		if strings.Contains(err.Error(), "SECRET") || strings.Contains(err.Error(), "tok-") {
			t.Errorf("%s: error leaks contents: %v", name, err)
		}
		if !strings.Contains(err.Error(), filepath.Join(home, "oauth.json")) {
			t.Errorf("%s: error does not name the file: %v", name, err)
		}
	}
	// A repeated entry key keeps the last value, as serde's HashMap.
	home := t.TempDir()
	writeRaw(t, home, `{"claude-cli":{"type":"oauth","refresh":"r","access":"a","expires":1},"claude-cli":{"type":"x","refresh":"r","access":"a","expires":`+fmt.Sprint(future)+`,"extra":true}}`)
	f := newFake(t, map[string]http.HandlerFunc{"/claude/usage": serve(`{"five_hour":{}}`)})
	snap, err := Fetch(context.Background(), options(home, f, nil))
	if err != nil || snap.Providers[0].Err != nil {
		t.Errorf("last duplicate wins: %v %v", err, snap.Providers[0].Err)
	}
	// An unreadable file (a directory) is a read failure.
	home = t.TempDir()
	if err := os.Mkdir(filepath.Join(home, "oauth.json"), 0o700); err != nil {
		t.Fatal(err)
	}
	if _, err := Fetch(context.Background(), options(home, nil, nil)); !errors.Is(err, ErrDiscovery) {
		t.Errorf("directory: %v", err)
	}
	// A store that cannot be reached is not an empty one: a symlink loop and
	// a home that is a file fail discovery instead of reading as
	// "not logged in". A missing file or a dangling link is no accounts.
	home = t.TempDir()
	if err := os.Symlink("oauth.json", filepath.Join(home, "oauth.json")); err != nil {
		t.Fatal(err)
	}
	if _, err := Fetch(context.Background(), options(home, nil, nil)); !errors.Is(err, ErrDiscovery) {
		t.Errorf("symlink loop: %v", err)
	}
	file := filepath.Join(t.TempDir(), "home-file")
	if err := os.WriteFile(file, nil, 0o600); err != nil {
		t.Fatal(err)
	}
	if _, err := Fetch(context.Background(), options(file, nil, nil)); !errors.Is(err, ErrDiscovery) {
		t.Errorf("home is a file: %v", err)
	}
	home = t.TempDir()
	if err := os.Symlink(filepath.Join(home, "gone"), filepath.Join(home, "oauth.json")); err != nil {
		t.Fatal(err)
	}
	for _, h := range []string{home, filepath.Join(t.TempDir(), "missing")} {
		if accounts, err := StoredAccounts(options(h, nil, nil)); err != nil || len(accounts) != len(providers) {
			t.Errorf("%s: %d accounts, %v", h, len(accounts), err)
		}
	}
}

func TestVersionAndHomeResolution(t *testing.T) {
	o := options(t.TempDir(), nil, nil)
	o.Version = ""
	if _, err := Fetch(context.Background(), o); !errors.Is(err, ErrVersion) {
		t.Errorf("version: %v", err)
	}
	zdxHome, userHome := t.TempDir(), t.TempDir()
	writeCache(t, zdxHome, map[string]cred{"grok-build@z": {Type: "oauth", Refresh: "r", Access: "a", Expires: future}})
	if err := os.Mkdir(filepath.Join(userHome, ".zdx"), 0o700); err != nil {
		t.Fatal(err)
	}
	writeCache(t, filepath.Join(userHome, ".zdx"), map[string]cred{"grok-build@h": {Type: "oauth", Refresh: "r", Access: "a", Expires: future}})
	grokAccount := func(vars map[string]string) string {
		o := options("", nil, vars)
		accounts, err := StoredAccounts(o)
		if err != nil {
			return "error: " + err.Error()
		}
		for _, a := range accounts {
			if a.Provider == ProviderGrok && a.Account != nil {
				return *a.Account
			}
		}
		return "none"
	}
	if got := grokAccount(map[string]string{"ZDX_HOME": zdxHome, "HOME": userHome}); got != "z" {
		t.Errorf("ZDX_HOME = %s", got)
	}
	if got := grokAccount(map[string]string{"HOME": userHome}); got != "h" {
		t.Errorf("HOME = %s", got)
	}
	if got := grokAccount(map[string]string{"USERPROFILE": userHome}); got != "h" {
		t.Errorf("USERPROFILE = %s", got)
	}
	if _, err := StoredAccounts(options("", nil, map[string]string{})); !errors.Is(err, ErrDiscovery) {
		t.Errorf("no home: %v", err)
	}
}

func TestHTTPFailureKinds(t *testing.T) {
	home := t.TempDir()
	writeCache(t, home, map[string]cred{"grok-build": {Type: "oauth", Refresh: "r", Access: "tok-grok-SECRET", Expires: future}})
	grok := func(h http.HandlerFunc, o func(*Options)) *Error {
		t.Helper()
		f := newFake(t, map[string]http.HandlerFunc{"/grok": h})
		opts := options(home, f, nil)
		if o != nil {
			o(&opts)
		}
		snap, err := Fetch(context.Background(), opts)
		if err != nil {
			t.Fatal(err)
		}
		r := snap.Providers[3]
		if r.Provider != ProviderGrok {
			t.Fatalf("row = %s", r.Provider)
		}
		if r.Err != nil && (strings.Contains(r.Err.Error(), "SECRET") || strings.Contains(r.Err.Error(), "http")) {
			t.Errorf("error leaks: %v", r.Err)
		}
		return r.Err
	}
	u := func(n uint64) *uint64 { return &n }
	for name, c := range map[string]struct {
		h    http.HandlerFunc
		want Error
	}{
		"401":          {status(401), Error{Kind: KindUnauthorized}},
		"403":          {status(403), Error{Kind: KindUnauthorized}},
		"429":          {status(429), Error{Kind: KindRateLimited}},
		"429 seconds":  {status(429, "Retry-After", "42"), Error{Kind: KindRateLimited, RetryAfterSecs: u(42)}},
		"429 plus":     {status(429, "Retry-After", "+7"), Error{Kind: KindRateLimited, RetryAfterSecs: u(7)}},
		"429 date":     {status(429, "Retry-After", "Wed, 21 Oct 2026 07:28:00 GMT"), Error{Kind: KindRateLimited}},
		"500":          {status(500), Error{Kind: KindHTTP, Status: 500}},
		"404":          {status(404), Error{Kind: KindHTTP, Status: 404}},
		"304":          {status(304), Error{Kind: KindHTTP, Status: 304}},
		"html":         {serve("<html>tok-SECRET</html>"), Error{Kind: KindIncompatible}},
		"empty":        {serve(""), Error{Kind: KindIncompatible}},
		"no config":    {serve(`{}`), Error{Kind: KindIncompatible}},
		"unknown only": {serve(`{"config":{"brandNewField":{"x":[1,2]}}}`), Error{}},
	} {
		got := grok(c.h, nil)
		if name == "unknown only" {
			if got != nil {
				t.Errorf("%s: %v", name, got)
			}
			continue
		}
		if got == nil || got.Kind != c.want.Kind || got.Status != c.want.Status || fmt.Sprint(deref(got.RetryAfterSecs)) != fmt.Sprint(deref(c.want.RetryAfterSecs)) {
			t.Errorf("%s: got %+v, want %+v", name, got, c.want)
		}
	}

	// Timeout: the server answers after the client's deadline.
	release := make(chan struct{})
	t.Cleanup(func() { close(release) })
	slow := func(w http.ResponseWriter, r *http.Request) {
		select {
		case <-release:
		case <-r.Context().Done():
		}
	}
	got := grok(slow, func(o *Options) { o.HTTP = &http.Client{Timeout: 50 * time.Millisecond} })
	if got == nil || got.Kind != KindTimeout || got.Reason() != "timed out" {
		t.Errorf("timeout = %v", got)
	}

	// A body cut off mid-read is unexpected, as a failed resp.json().
	cut := func(w http.ResponseWriter, r *http.Request) {
		w.Header().Set("Content-Length", "100")
		io.WriteString(w, `{"config":`)
	}
	if got := grok(cut, nil); got == nil || got.Kind != KindIncompatible {
		t.Errorf("cut body = %v", got)
	}

	// Transport: nothing listens.
	ln, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatal(err)
	}
	addr := ln.Addr().String()
	ln.Close()
	got = grok(serve("{}"), func(o *Options) { o.Endpoints.GrokBilling = "http://" + addr + "/grok" })
	if got == nil || got.Kind != KindTransport || got.Reason() != "network error" {
		t.Errorf("transport = %v", got)
	}
}

func deref(p *uint64) any {
	if p == nil {
		return nil
	}
	return *p
}

func TestRedirectsDropCredentialsAcrossHosts(t *testing.T) {
	home := t.TempDir()
	writeCache(t, home, map[string]cred{
		"grok-build": {Type: "oauth", Refresh: "r", Access: "tok-grok", Expires: future},
		"muse-code":  {Type: "oauth", Refresh: "id-muse-SECRET", Access: "k", Expires: future},
	})
	// One server answers every host name; the client dials it for all of them.
	var mu sync.Mutex
	leaked := map[string]string{}
	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		host := r.Host
		switch {
		case r.URL.Path == "/landing":
			mu.Lock()
			if a := r.Header.Get("Authorization"); a != "" {
				leaked[host] = a
			}
			mu.Unlock()
			if strings.HasPrefix(host, "api.quota.test") && r.Header.Get("Authorization") == "" {
				w.WriteHeader(401) // same origin must keep the token
				return
			}
			io.WriteString(w, `{"config":{"creditUsagePercent":5},"subs_usage":{"weekly":{"used_percent":5}}}`)
		case r.URL.Path == "/loop":
			http.Redirect(w, r, "/loop", http.StatusFound)
		case r.URL.Path == "/hop":
			// A second hop on the same origin: Go copies the first
			// request's headers again, so the drop must be sticky.
			http.Redirect(w, r, "/landing", http.StatusFound)
		default:
			http.Redirect(w, r, r.URL.Query().Get("to"), http.StatusFound)
		}
	}))
	t.Cleanup(srv.Close)
	_, srvPort, _ := net.SplitHostPort(strings.TrimPrefix(srv.URL, "http://"))
	dial := func(ctx context.Context, network, _ string) (net.Conn, error) {
		return (&net.Dialer{}).DialContext(ctx, network, strings.TrimPrefix(srv.URL, "http://"))
	}
	client := &http.Client{Transport: &http.Transport{DialContext: dial}}
	origin := "http://api.quota.test:" + srvPort
	run := func(to string) []Result {
		o := options(home, nil, nil)
		o.HTTP = client
		o.Endpoints.GrokBilling = origin + "/start?to=" + to
		o.Endpoints.MuseCodeKey = origin + "/start?to=" + to
		snap, err := Fetch(context.Background(), o)
		if err != nil {
			t.Fatal(err)
		}
		return []Result{snap.Providers[3], snap.Providers[4]}
	}
	for name, to := range map[string]string{
		"subdomain":                  "http://cdn.api.quota.test:" + srvPort + "/landing",
		"other host":                 "http://other.test:" + srvPort + "/landing",
		"other port":                 "http://api.quota.test:1/landing",
		"subdomain, then same host":  "http://cdn.api.quota.test:" + srvPort + "/hop",
		"other port, then same host": "http://api.quota.test:1/hop",
	} {
		if strings.HasPrefix(name, "other port") {
			// Dialing ignores the port, so this reaches the same server.
			run(to)
			continue
		}
		for _, r := range run(to) {
			if r.Err != nil {
				t.Errorf("%s %s: %v", name, r.Provider, r.Err)
			}
		}
	}
	mu.Lock()
	for host, auth := range leaked {
		t.Errorf("credential sent to %s: %q", host, auth)
	}
	mu.Unlock()
	for _, r := range run(origin + "/landing") {
		if r.Err != nil {
			t.Errorf("same origin lost the token: %s %v", r.Provider, r.Err)
		}
	}
	o := options(home, nil, nil)
	o.HTTP = client
	o.Endpoints.GrokBilling = origin + "/loop"
	snap, err := Fetch(context.Background(), o)
	if err != nil {
		t.Fatal(err)
	}
	if e := snap.Providers[3].Err; e == nil || e.Kind != KindTransport {
		t.Errorf("redirect loop = %v", e)
	}
}

func TestRedirectLimitIsTen(t *testing.T) {
	via := make([]*http.Request, 0, 11)
	req := httptest.NewRequest("GET", "http://a.test/x", nil)
	for i := 1; i <= 11; i++ {
		via = append(via, req)
		err := checkRedirect(req, via)
		if (i <= 10) != (err == nil) {
			t.Errorf("redirect %d: err = %v", i, err)
		}
	}
}

func TestContextCancellationPassesThrough(t *testing.T) {
	home := t.TempDir()
	writeCache(t, home, allCreds())
	started := make(chan struct{}, 8)
	block := func(w http.ResponseWriter, r *http.Request) {
		select {
		case started <- struct{}{}:
		default:
		}
		<-r.Context().Done()
	}
	f := newFake(t, map[string]http.HandlerFunc{"/claude/usage": block, "/claude/profile": block, "/codex": block, "/antigravity": block, "/grok": block, "/muse": block})
	ctx, cancel := context.WithCancel(context.Background())
	done := make(chan error, 1)
	go func() {
		_, err := Fetch(ctx, options(home, f, nil))
		done <- err
	}()
	<-started
	cancel()
	select {
	case err := <-done:
		if !errors.Is(err, context.Canceled) {
			t.Errorf("err = %v", err)
		}
	case <-time.After(5 * time.Second):
		t.Fatal("Fetch did not return after cancel")
	}
	ctx, cancel = context.WithTimeout(context.Background(), 30*time.Millisecond)
	defer cancel()
	if _, err := Fetch(ctx, options(home, f, nil)); !errors.Is(err, context.DeadlineExceeded) {
		t.Errorf("deadline err = %v", err)
	}
}

func TestFetchesRunConcurrently(t *testing.T) {
	home := t.TempDir()
	writeCache(t, home, allCreds())
	var mu sync.Mutex
	inFlight, peak := 0, 0
	gate := make(chan struct{})
	slow := func(body string) http.HandlerFunc {
		return func(w http.ResponseWriter, r *http.Request) {
			mu.Lock()
			inFlight++
			if inFlight > peak {
				peak = inFlight
			}
			if inFlight == 6 {
				close(gate)
			}
			mu.Unlock()
			select {
			case <-gate:
			case <-time.After(3 * time.Second):
			}
			io.WriteString(w, body)
		}
	}
	routes := happyRoutes(t)
	for k, h := range routes {
		_ = h
		routes[k] = slow(bodyOf(t, k))
	}
	f := newFake(t, routes)
	if _, err := Fetch(context.Background(), options(home, f, nil)); err != nil {
		t.Fatal(err)
	}
	if peak != 6 {
		t.Errorf("peak concurrent requests = %d, want 6 (5 providers + Claude profile)", peak)
	}
}

func bodyOf(t *testing.T, path string) string {
	rec := httptest.NewRecorder()
	happyRoutes(t)[path](rec, httptest.NewRequest("GET", path, nil))
	return rec.Body.String()
}

// serde flatten buffers the whole OAuth cache, so every string, number, and
// nesting level in it is checked, unknown fields included; errors never
// quote the file.
func TestOAuthCacheStringAndNestingChecks(t *testing.T) {
	entry := func(extra string) string {
		return `{"claude-cli":{"type":"oauth","refresh":"tok-SECRET-r","access":"tok-SECRET-a","expires":1` + extra + `}}`
	}
	deep := func(n int) string { return strings.Repeat("[", n) + strings.Repeat("]", n) }
	for name, contents := range map[string]string{
		"lone surrogate in token":     `{"claude-cli":{"type":"oauth","refresh":"tok-SECRET\ud800","access":"a","expires":1}}`,
		"lone surrogate unknown":      entry(`,"note":"\udc00 tok-SECRET"`),
		"lone surrogate in key":       `{"claude-cli\ud800":{"type":"oauth","refresh":"tok-SECRET","access":"a","expires":1}}`,
		"out-of-range number unknown": entry(`,"n":1e400`),
		"nesting 128":                 entry(`,"x":` + deep(126)),
	} {
		home := t.TempDir()
		writeRaw(t, home, contents)
		_, err := Fetch(context.Background(), options(home, nil, nil))
		if !errors.Is(err, ErrDiscovery) {
			t.Errorf("%s: err = %v", name, err)
		} else if strings.Contains(err.Error(), "SECRET") || strings.Contains(err.Error(), "tok-") {
			t.Errorf("%s: leaks: %v", name, err)
		}
	}
	// Root, entry, and 125 more levels are 127: accepted.
	home := t.TempDir()
	writeRaw(t, home, entry(`,"x":`+deep(125)+`,"emoji":"\ud83d\ude00"`))
	if _, err := Fetch(context.Background(), options(home, nil, nil)); err != nil {
		t.Errorf("127 levels: %v", err)
	}
}
