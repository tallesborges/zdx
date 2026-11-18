package quota

import (
	"context"
	"encoding/json"
	"flag"
	"math"
	"net/http"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"
)

var update = flag.Bool("update", false, "rewrite testdata goldens")

// golden compares got with testdata/<name>. The goldens are derived from the
// Rust sources (crates/zdx-cli/src/cli/commands/quota.rs and the monitor view
// body at aab08ba4) and reviewed by hand; they were not produced by running
// the Rust binary.
func golden(t *testing.T, name string, got []byte) {
	t.Helper()
	path := filepath.Join("testdata", name)
	if *update {
		if err := os.WriteFile(path, got, 0o644); err != nil {
			t.Fatal(err)
		}
	}
	want, err := os.ReadFile(path)
	if err != nil {
		t.Fatal(err)
	}
	if string(got) != string(want) {
		t.Errorf("%s differs:\n--- got\n%s\n--- want\n%s", name, got, want)
	}
}

// richSnapshot runs Fetch end to end on the Rust fixtures plus failures and
// a named account.
func richSnapshot(t *testing.T) Snapshot {
	t.Helper()
	home := t.TempDir()
	creds := allCreds()
	creds["claude-cli@work"] = cred{Type: "oauth", Refresh: "r", Access: "tok-work", Expires: past}
	creds["grok-build@busy"] = cred{Type: "oauth", Refresh: "r", Access: "tok-busy", Expires: future}
	creds["openai-codex@biz"] = cred{Type: "oauth", Refresh: "r", Access: "tok-biz", Expires: future, AccountID: str("acct-biz")}
	writeCache(t, home, creds)
	routes := happyRoutes(t)
	routes["/grok"] = func(w http.ResponseWriter, r *http.Request) {
		if r.Header.Get("Authorization") == "Bearer tok-busy" {
			status(429, "Retry-After", "30")(w, r)
			return
		}
		serve(string(fixture(t, "grok_usage.json")))(w, r)
	}
	routes["/codex"] = func(w http.ResponseWriter, r *http.Request) {
		if r.Header.Get("chatgpt-account-id") == "acct-biz" {
			serve(string(fixture(t, "codex_usage_business.json")))(w, r)
			return
		}
		serve(string(fixture(t, "codex_usage.json")))(w, r)
	}
	f := newFake(t, routes)
	snap, err := Fetch(context.Background(), options(home, f, nil))
	if err != nil {
		t.Fatal(err)
	}
	return snap
}

func TestCLIJSONGolden(t *testing.T) {
	golden(t, "cli_quota.json", CLIJSON(richSnapshot(t)))
	empty, err := Fetch(context.Background(), options(t.TempDir(), nil, nil))
	if err != nil {
		t.Fatal(err)
	}
	golden(t, "cli_quota_empty_home.json", CLIJSON(empty))
}

func TestCLITextGolden(t *testing.T) {
	now := time.Date(2026, 7, 14, 0, 0, 0, 0, time.UTC)
	golden(t, "cli_quota.txt", []byte(CLIText(richSnapshot(t), now)))
}

func TestSubscriptionsJSONGolden(t *testing.T) {
	subs := Subscriptions(richSnapshot(t))
	golden(t, "subscriptions.json", SubscriptionsJSON(subs))
	// encoding/json of the same values decodes to the same document.
	plain, err := json.Marshal(subs)
	if err != nil {
		t.Fatal(err)
	}
	var a, b any
	if err := json.Unmarshal(plain, &a); err != nil {
		t.Fatal(err)
	}
	if err := json.Unmarshal(SubscriptionsJSON(subs), &b); err != nil {
		t.Fatal(err)
	}
	ja, _ := json.Marshal(a)
	jb, _ := json.Marshal(b)
	if string(ja) != string(jb) {
		t.Errorf("struct tags and SubscriptionsJSON disagree:\n%s\n%s", ja, jb)
	}
}

func TestSubscriptionsViewBody(t *testing.T) {
	reset := time.Date(2026, 8, 22, 10, 0, 0, 0, time.UTC)
	snap := Snapshot{Providers: []Result{
		{Provider: ProviderGrok, Err: &Error{Kind: KindHTTP, Status: 500}},
		{Provider: ProviderClaude, Quota: &Quota{Plan: str("max"), Windows: []Window{
			{Label: "5h", UsedPercent: 140, ResetsAt: &reset, Scope: str("Opus")},
			{Label: "weekly", UsedPercent: -3},
			{Label: "spend", UsedPercent: math.Inf(1)},
			{Label: "nan", UsedPercent: math.NaN()},
			{Label: "neg zero", UsedPercent: math.Copysign(0, -1)},
		}}},
		{Provider: ProviderCodex, Err: &Error{Kind: KindNotAuthenticated}},
		{Provider: ProviderClaude, Account: str("a"), Err: &Error{Kind: KindExpired}},
	}}
	subs := Subscriptions(snap)
	if len(subs) != 3 {
		t.Fatalf("subs = %+v", subs)
	}
	names := []string{subs[0].Name, subs[1].Name, subs[2].Name}
	if strings.Join(names, "|") != "Claude|Claude @a|Grok" {
		t.Errorf("order = %v", names)
	}
	w := subs[0].Windows
	if w[0].UsedPercent != 100 || *w[0].ResetsAt != "2026-08-22T10:00:00+00:00" || *w[0].Scope != "Opus" {
		t.Errorf("clamped = %+v", w[0])
	}
	if w[1].UsedPercent != 0 || w[2].UsedPercent != 0 || w[3].UsedPercent != 0 || !math.Signbit(w[4].UsedPercent) {
		t.Errorf("bounds = %+v", w)
	}
	if subs[1].Error == nil || *subs[1].Error != "expired · re-login in zdx" || subs[1].Windows == nil || len(subs[1].Windows) != 0 {
		t.Errorf("failed = %+v", subs[1])
	}
	if got := string(SubscriptionsJSON(subs[2:])); got != `[{"error":"HTTP 500","name":"Grok","plan":null,"provider":"grok-build","windows":[]}]` {
		t.Errorf("json = %s", got)
	}
}

func TestErrorReasonsAreBoundedAndExact(t *testing.T) {
	secs := uint64(30)
	for e, want := range map[*Error]string{
		{Kind: KindNotAuthenticated}:                   "not logged in",
		{Kind: KindExpired}:                            "expired · re-login in zdx",
		{Kind: KindUnauthorized}:                       "unauthorized",
		{Kind: KindRateLimited, RetryAfterSecs: &secs}: "rate limited",
		{Kind: KindTimeout}:                            "timed out",
		{Kind: KindHTTP, Status: 500}:                  "HTTP 500",
		{Kind: KindIncompatible}:                       "unexpected response",
		{Kind: KindTransport}:                          "network error",
	} {
		if e.Reason() != want || e.Error() != "quota: "+want {
			t.Errorf("%+v = %q", e, e.Reason())
		}
	}
}

func TestDisplayNames(t *testing.T) {
	want := []string{"Claude", "Codex", "Antigravity", "Grok", "Muse Code", "Copilot"}
	for i, p := range Providers() {
		if ProviderDisplay(p) != want[i] {
			t.Errorf("%s = %s", p, ProviderDisplay(p))
		}
	}
	if ProviderDisplay("other") != "other" || AccountDisplay(ProviderClaude, str("work")) != "Claude @work" {
		t.Error("display fallback")
	}
}

func TestResetInMatchesCLI(t *testing.T) {
	now := time.Date(2026, 7, 14, 0, 0, 0, 500_000_000, time.UTC)
	for d, want := range map[time.Duration]string{
		0:                           "reset due",
		-time.Hour:                  "reset due",
		900 * time.Millisecond:      "reset due",
		1500 * time.Millisecond:     "resets in 0m",
		59 * time.Minute:            "resets in 59m",
		2*time.Hour + 5*time.Minute: "resets in 2h 5m",
		3*24*time.Hour + 4*time.Hour + 59*time.Minute: "resets in 3d 4h",
	} {
		if got := resetIn(now.Add(d), now); got != want {
			t.Errorf("%v = %s, want %s", d, got, want)
		}
	}
	// Far resets do not overflow time.Duration.
	if got := resetIn(*timestamp(maxTimestamp), now); !strings.HasPrefix(got, "resets in ") || !strings.HasSuffix(got, "h") {
		t.Errorf("far = %s", got)
	}
}

// The seconds chrono 0.4.45 gives for `(reset - now).num_seconds()` when
// the reset is a leap second (printed by a Rust program on these inputs).
func TestLeapSecondCountdownMatchesChrono(t *testing.T) {
	for _, c := range []struct {
		reset, now string
		secs       int64
	}{
		{"2026-07-22T00:00:60Z", "2026-07-21T23:59:00Z", 119},
		{"2026-07-22T00:00:60Z", "2026-07-22T00:00:10Z", 50},
		{"2026-07-22T00:00:60Z", "2026-07-22T00:00:59.5Z", 0},
		{"2026-07-22T00:00:60Z", "2026-07-22T00:00:59Z", 1},
		{"2026-07-22T00:00:60.5Z", "2026-07-21T23:59:00Z", 119},
		{"2026-07-22T03:00:60+03:00", "2026-07-21T23:59:00Z", 119},
		{"2026-07-21T23:59:60Z", "2026-07-21T23:58:00Z", 120},
		{"2026-07-21T23:59:60Z", "2026-07-21T23:59:59.5Z", 0},
		{"2026-07-21T23:59:60Z", "2026-07-21T12:00:00Z", 43200},
		{"2026-07-22T00:00:60Z", "2026-07-22T00:01:30Z", -31},
		{"2026-07-22T00:00:60Z", "2026-07-22T00:01:00Z", -1},
		{"2026-07-22T00:00:60Z", "2026-07-22T00:01:00.5Z", -1},
		{"2026-07-25T10:30:60Z", "2026-07-21T23:59:00Z", 297119},
	} {
		reset, now := parseRFC3339(&c.reset), parseRFC3339(&c.now)
		if reset == nil || now == nil {
			t.Fatalf("parse %s / %s", c.reset, c.now)
		}
		if got := chronoSeconds(*reset, *now); got != c.secs {
			t.Errorf("%s - %s = %ds, chrono %ds", c.reset, c.now, got, c.secs)
		}
	}
	reset := "2026-07-22T00:00:60Z"
	at := parseRFC3339(&reset)
	if got := resetIn(*at, time.Date(2026, 7, 21, 23, 59, 0, 0, time.UTC)); got != "resets in 1m" {
		t.Errorf("leap countdown = %s, want resets in 1m", got)
	}
}
