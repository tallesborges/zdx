package quota

import (
	"math"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"github.com/tallesborges/zdx/apps/shared/sandbox"
)

func TestMain(m *testing.M) { os.Exit(sandbox.Main(m.Run)) }

func fixture(t *testing.T, name string) []byte {
	t.Helper()
	data, err := os.ReadFile(filepath.Join("testdata", name))
	if err != nil {
		t.Fatal(err)
	}
	return data
}

func near(a, b float64) bool { return math.Abs(a-b) < 1e-9 }

func str(s string) *string { return &s }

func mustTime(t *testing.T, s string) time.Time {
	t.Helper()
	v := parseRFC3339(&s)
	if v == nil {
		t.Fatalf("parse %q", s)
	}
	return *v
}

// Ports of the Rust unit tests in subscription_quota.rs (aab08ba4).

func TestParsesClaudeLimitsArray(t *testing.T) {
	q, err := parseClaude(fixture(t, "claude_usage.json"), nil)
	if err != nil || q == nil {
		t.Fatalf("claude quota: %v", err)
	}
	if len(q.Windows) != 3 {
		t.Fatalf("windows = %d", len(q.Windows))
	}
	s, w, scoped := q.Windows[0], q.Windows[1], q.Windows[2]
	if s.Label != "5h" || s.UsedPercent != 0 || s.Scope != nil {
		t.Errorf("session = %+v", s)
	}
	if w.Label != "weekly" || w.UsedPercent != 45 || w.ResetsAt == nil || w.Scope != nil {
		t.Errorf("weekly = %+v", w)
	}
	if scoped.Label != "weekly" || scoped.Scope == nil || *scoped.Scope != "Opus" {
		t.Errorf("scoped = %+v", scoped)
	}
	if got := FormatRFC3339(*s.ResetsAt); got != "2026-07-14T01:09:59.535017+00:00" {
		t.Errorf("reset = %s", got)
	}
}

func TestClaudeFallsBackToFlatFieldsWhenNoLimits(t *testing.T) {
	body := `{"five_hour":{"utilization":12.5,"resets_at":"2026-07-14T01:09:59.535017+00:00"},"seven_day":{"utilization":80.0,"resets_at":null}}`
	q, err := parseClaude([]byte(body), nil)
	if err != nil || q == nil || len(q.Windows) != 2 {
		t.Fatalf("fallback: %+v %v", q, err)
	}
	if q.Windows[0].Label != "5h" || q.Windows[0].UsedPercent != 12.5 || q.Windows[0].ResetsAt == nil {
		t.Errorf("5h = %+v", q.Windows[0])
	}
	if q.Windows[1].Label != "weekly" || q.Windows[1].ResetsAt != nil {
		t.Errorf("weekly = %+v", q.Windows[1])
	}
	// An empty limits array also falls back; missing utilization is 0.
	q, _ = parseClaude([]byte(`{"limits":[],"seven_day":{}}`), nil)
	if q == nil || len(q.Windows) != 1 || q.Windows[0].UsedPercent != 0 {
		t.Errorf("empty limits = %+v", q)
	}
}

func TestParsesCodexWindowsWithUnixReset(t *testing.T) {
	q, err := parseCodex(fixture(t, "codex_usage.json"))
	if err != nil || q == nil {
		t.Fatal(err)
	}
	if *q.Plan != "prolite" || len(q.Windows) != 1 {
		t.Fatalf("%+v", q)
	}
	w := q.Windows[0]
	if w.Label != "weekly" || w.UsedPercent != 6 || w.ResetsAt.Unix() != 1_784_502_675 {
		t.Errorf("%+v", w)
	}
}

func TestParsesAntigravityGroupedWeeklyAnd5h(t *testing.T) {
	q, err := parseAntigravity(fixture(t, "antigravity_usage.json"))
	if err != nil || q == nil || len(q.Windows) != 2 {
		t.Fatalf("%+v %v", q, err)
	}
	for _, w := range q.Windows {
		if w.Scope == nil || *w.Scope != "Gemini Models" {
			t.Errorf("scope = %v", w.Scope)
		}
	}
	if q.Windows[0].Label != "weekly" || !(q.Windows[0].UsedPercent < 1) || q.Windows[0].ResetsAt == nil {
		t.Errorf("weekly = %+v", q.Windows[0])
	}
	if q.Windows[1].Label != "5h" || !near(q.Windows[1].UsedPercent, 60) {
		t.Errorf("5h = %+v", q.Windows[1])
	}
	if q.Plan != nil {
		t.Errorf("plan = %v", *q.Plan)
	}
}

func TestAntigravityNonGeminiGroupsAreDropped(t *testing.T) {
	q, err := parseAntigravity([]byte(`{"groups":[{"displayName":"Claude and GPT models","buckets":[{"window":"weekly","remainingFraction":1.0}]}]}`))
	if err != nil || q != nil {
		t.Errorf("got %+v %v", q, err)
	}
	// GEMINI in any ASCII case counts; a missing remainingFraction is 0% used.
	q, _ = parseAntigravity([]byte(`{"groups":[{"displayName":"GEMINI","buckets":[{"window":"daily"}]}]}`))
	if q == nil || q.Windows[0].Label != "weekly" || q.Windows[0].UsedPercent != 0 {
		t.Errorf("got %+v", q)
	}
}

func TestParsesGrokCreditUsage(t *testing.T) {
	q, err := parseGrok(fixture(t, "grok_usage.json"))
	if err != nil || q == nil {
		t.Fatal(err)
	}
	if *q.Plan != "SuperGrok Heavy" || len(q.Windows) != 1 {
		t.Fatalf("%+v", q)
	}
	if w := q.Windows[0]; w.Label != "weekly" || w.UsedPercent != 12 || w.ResetsAt == nil {
		t.Errorf("%+v", w)
	}
	q, _ = parseGrok([]byte(`{"config":{"currentPeriod":{"type":"USAGE_PERIOD_TYPE_MONTHLY"}}}`))
	if q == nil || q.Windows[0].Label != "monthly" || q.Windows[0].ResetsAt != nil {
		t.Errorf("monthly = %+v", q)
	}
}

func TestGrokMissingUsagePercentIsZeroNotError(t *testing.T) {
	q, err := parseGrok([]byte(`{"config":{}}`))
	if err != nil || q == nil || q.Windows[0].UsedPercent != 0 {
		t.Errorf("%+v %v", q, err)
	}
	if q, err := parseGrok([]byte(`{"config":null}`)); q != nil || err != nil {
		t.Errorf("null config = %+v %v", q, err)
	}
}

func TestParsesCodexBusinessSpendControlWhenRateLimitIsNull(t *testing.T) {
	q, err := parseCodex(fixture(t, "codex_usage_business.json"))
	if err != nil || q == nil {
		t.Fatal(err)
	}
	if *q.Plan != "business" || len(q.Windows) != 1 {
		t.Fatalf("%+v", q)
	}
	w := q.Windows[0]
	if w.Label != "spend" || math.Abs(w.UsedPercent-0.035_721_998) > 1e-6 {
		t.Errorf("%+v", w)
	}
	if w.Scope == nil || *w.Scope != "$0.89 of $2500.00" {
		t.Errorf("scope = %v", w.Scope)
	}
	if w.ResetsAt.Unix() != 1_790_812_801 {
		t.Errorf("reset = %v", w.ResetsAt)
	}
}

func TestCodexPrefersRateLimitWindowsOverSpendControl(t *testing.T) {
	q, _ := parseCodex(fixture(t, "codex_usage.json"))
	if len(q.Windows) != 1 || q.Windows[0].Label != "weekly" {
		t.Errorf("%+v", q)
	}
	q, _ = parseCodex([]byte(`{"rate_limit":{"primary_window":{"used_percent":5}},"spend_control":{"individual_limit":{"limit":"10","used":"5"}}}`))
	if len(q.Windows) != 1 || q.Windows[0].Label != "window" {
		t.Errorf("%+v", q)
	}
}

func TestCodexSpendWindowFallsBackToUsedPercentWithoutDollarAmounts(t *testing.T) {
	w, err := codexSpendWindow(mustObject(t, `{"used_percent":42}`))
	if err != nil || w == nil || w.Label != "spend" || w.UsedPercent != 42 || w.Scope != nil || w.ResetsAt != nil {
		t.Errorf("%+v %v", w, err)
	}
	w, err = codexSpendWindow(mustObject(t, `{"limit":"0","used":"0"}`))
	if w != nil || err != nil {
		t.Errorf("zero limit = %+v %v", w, err)
	}
	// Rust f64::from_str grammar: "inf" parses, " 5" does not.
	w, _ = codexSpendWindow(mustObject(t, `{"limit":"inf","used":"1"}`))
	if w == nil || w.UsedPercent != 0 || *w.Scope != "$1.00 of $inf" {
		t.Errorf("inf limit = %+v", w)
	}
	w, _ = codexSpendWindow(mustObject(t, `{"limit":" 5","used":"1","used_percent":7}`))
	if w == nil || w.UsedPercent != 7 || w.Scope != nil {
		t.Errorf("space limit = %+v", w)
	}
}

func mustObject(t *testing.T, s string) fields {
	t.Helper()
	o, err := object([]byte(s), "limit", "used", "used_percent", "reset_at")
	if err != nil {
		t.Fatal(err)
	}
	return o
}

func TestCodexWindowLabelsFromSeconds(t *testing.T) {
	v := func(n int64) *int64 { return &n }
	for _, c := range []struct {
		in   *int64
		want string
	}{{v(18_000), "5h"}, {v(604_800), "weekly"}, {v(7_200), "window"}, {nil, "window"}} {
		if got := codexLabel(c.in); got != c.want {
			t.Errorf("%v = %s", c.in, got)
		}
	}
}

func TestEmptyPayloadsReturnNone(t *testing.T) {
	if q, err := parseClaude([]byte(`{}`), nil); q != nil || err != nil {
		t.Errorf("claude = %+v %v", q, err)
	}
	if q, err := parseCodex([]byte(`{}`)); q != nil || err != nil {
		t.Errorf("codex = %+v %v", q, err)
	}
}

func TestParsesMuseCodeRollingAndWeeklyWindows(t *testing.T) {
	body := `{"api_key":"LLM|secret","is_subs_active":true,"subs_tier_name":"Power Usage",
		"subs_usage":{"window":{"used_percent":42,"resets_at":1800000000,"window_duration_mins":300},
		"weekly":{"used_percent":75,"resets_at":"2030-01-08T00:00:00.000Z"}}}`
	q, active, err := parseMuseCode([]byte(body))
	if err != nil || q == nil || active == nil || !*active {
		t.Fatalf("%+v %v %v", q, active, err)
	}
	if *q.Plan != "Power Usage" || len(q.Windows) != 2 {
		t.Fatalf("%+v", q)
	}
	if w := q.Windows[0]; w.Label != "5h" || w.UsedPercent != 42 || w.ResetsAt.Unix() != 1_800_000_000 {
		t.Errorf("%+v", w)
	}
	if w := q.Windows[1]; w.Label != "weekly" || w.UsedPercent != 75 || w.ResetsAt == nil {
		t.Errorf("%+v", w)
	}
}

func TestMuseCodeQuotaWithoutUsageIsIncompatible(t *testing.T) {
	q, _, err := parseMuseCode([]byte(`{"api_key":"LLM|k","is_subs_active":true}`))
	if q != nil || err != nil {
		t.Errorf("%+v %v", q, err)
	}
}

func TestMuseCodeIdleWindowIsZeroRolling(t *testing.T) {
	// Shape of a live answer before the first request of the window.
	body := `{"api_key":"LLM|k","base_url":"https://api.meta.ai/v1","has_payment_method":true,
		"require_payment":false,"is_subs_active":true,"action_url":null,
		"subs_tier_id":"1","subs_tier_name":"Power Usage"}`
	q, e := museOutcome([]byte(body))
	if e != nil || q == nil || *q.Plan != "Power Usage" || len(q.Windows) != 1 {
		t.Fatalf("%+v %v", q, e)
	}
	if w := q.Windows[0]; w.Label != "rolling" || w.UsedPercent != 0 || w.ResetsAt != nil {
		t.Errorf("%+v", w)
	}
}

func TestMuseCodeWithoutUsageStaysUnexpectedUnlessIdle(t *testing.T) {
	for _, body := range []string{
		`{"is_subs_active":true,"require_payment":true}`,
		`{"is_subs_active":true,"action_url":"https://example.test/pay"}`,
		`{"is_subs_active":true,"require_payment_action_url":"https://example.test/pay"}`,
		`{"api_key":"LLM|k"}`,
		`{"is_subs_active":true,"subs_usage":{"window":{"resets_at":1}}}`,
	} {
		if q, e := museOutcome([]byte(body)); q != nil || e == nil || e.Kind != KindIncompatible {
			t.Errorf("%s = %+v %v", body, q, e)
		}
	}
}

func TestMuseCodeWindowLabelFallsBackWhenDurationIsAbsent(t *testing.T) {
	f := func(v float64) *float64 { return &v }
	for _, c := range []struct {
		in   *float64
		want string
	}{{f(300), "5h"}, {f(90), "90m"}, {nil, "rolling"}, {f(0), "rolling"}, {f(299.6), "5h"}} {
		if got := museLabel(c.in); got != c.want {
			t.Errorf("%v = %s", c.in, got)
		}
	}
}

func TestMuseCodeResetForms(t *testing.T) {
	for _, c := range []struct {
		raw  string
		want int64 // -1: none
	}{{`1800000000`, 1_800_000_000}, {`1800000000.9`, 1_800_000_000}, {`0`, -1}, {`-5`, -1}, {`true`, -1}, {`"bad"`, -1}, {`0.5`, 0}} {
		got, err := museReset([]byte(c.raw))
		if err != nil || (c.want < 0) != (got == nil) || (got != nil && got.Unix() != c.want) {
			t.Errorf("%s = %v", c.raw, got)
		}
	}
	// A tier name made only of whitespace falls back to nothing, not the id.
	q, _, _ := parseMuseCode([]byte(`{"subs_tier_name":"  ","subs_tier_id":"t1","subs_usage":{"weekly":{"used_percent":1}}}`))
	if q == nil || q.Plan != nil {
		t.Errorf("plan = %+v", q)
	}
}

func TestClaudePlanPrefersThePreciseRateLimitTier(t *testing.T) {
	p := claudePlan([]byte(`{"organization":{"organization_type":"claude_max","rate_limit_tier":"default_claude_max_20x"}}`))
	if p == nil || *p != "claude_max_20x" {
		t.Errorf("%v", p)
	}
	p = claudePlan([]byte(`{"organization":{"organization_type":"claude_pro"}}`))
	if p == nil || *p != "claude_pro" {
		t.Errorf("%v", p)
	}
	for _, body := range []string{`{}`, `{"organization":{}}`, `{"organization":{"rate_limit_tier":""}}`, `not json`} {
		if p := claudePlan([]byte(body)); p != nil {
			t.Errorf("%s = %v", body, *p)
		}
	}
	// An empty tier does not fall back to organization_type (Rust `or` on Option).
	if p := claudePlan([]byte(`{"organization":{"rate_limit_tier":"  ","organization_type":"claude_pro"}}`)); p != nil {
		t.Errorf("blank tier = %v", *p)
	}
}

func TestClaudeQuotaCarriesThePlanThrough(t *testing.T) {
	q, _ := parseClaude(fixture(t, "claude_usage.json"), str("claude_max_20x"))
	if q == nil || *q.Plan != "claude_max_20x" || len(q.Windows) != 3 {
		t.Errorf("%+v", q)
	}
}

// Decoding strictness kept from serde: these fail the whole payload.
func TestWireStrictnessMatchesSerde(t *testing.T) {
	incompatible := map[string]func([]byte) error{
		"claude limits null":        func(b []byte) error { _, err := parseClaude(b, nil); return err },
		"claude duplicate field":    func(b []byte) error { _, err := parseClaude(b, nil); return err },
		"claude percent string":     func(b []byte) error { _, err := parseClaude(b, nil); return err },
		"codex float reset":         func(b []byte) error { _, err := parseCodex(b); return err },
		"codex spend type error":    func(b []byte) error { _, err := parseCodex(b); return err },
		"antigravity buckets null":  func(b []byte) error { _, err := parseAntigravity(b); return err },
		"antigravity non-gemini ty": func(b []byte) error { _, err := parseAntigravity(b); return err },
		"muse user_id number":       func(b []byte) error { _, _, err := parseMuseCode(b); return err },
		"top-level array":           func(b []byte) error { _, err := parseGrok(b); return err },
		"trailing data":             func(b []byte) error { _, err := parseGrok(b); return err },
		"invalid utf8":              func(b []byte) error { _, err := parseGrok(b); return err },
	}
	bodies := map[string]string{
		"claude limits null":        `{"limits":null,"five_hour":{"utilization":1}}`,
		"claude duplicate field":    `{"five_hour":{"utilization":1},"five_hour":{"utilization":2}}`,
		"claude percent string":     `{"limits":[{"group":"session","percent":"5"}]}`,
		"codex float reset":         `{"rate_limit":{"primary_window":{"used_percent":1,"reset_at":1784502675.5}}}`,
		"codex spend type error":    `{"rate_limit":{"primary_window":{"used_percent":1}},"spend_control":{"individual_limit":{"limit":2500}}}`,
		"antigravity buckets null":  `{"groups":[{"displayName":"Gemini","buckets":null}]}`,
		"antigravity non-gemini ty": `{"groups":[{"displayName":"GPT","buckets":[{"remainingFraction":"x"}]}]}`,
		"muse user_id number":       `{"user_id":5,"subs_usage":{"weekly":{"used_percent":1}}}`,
		"top-level array":           `[{"config":{}}]`,
		"trailing data":             `{"config":{}} {}`,
		"invalid utf8":              "{\"config\":{\"subscriptionTier\":\"\xff\"}}",
	}
	for name, parse := range incompatible {
		if err := parse([]byte(bodies[name])); err == nil {
			t.Errorf("%s: want a decode error", name)
		}
	}
	// Field names are case-sensitive and unknown fields are ignored.
	q, err := parseGrok([]byte(`{"Config":{"creditUsagePercent":50},"config":{"CreditUsagePercent":9,"extra":[1,{"a":null}]}}`))
	if err != nil || q == nil || q.Windows[0].UsedPercent != 0 {
		t.Errorf("case = %+v %v", q, err)
	}
	// A repeated undeclared field is ignored, as serde's IgnoredAny.
	if _, err := parseGrok([]byte(`{"x":1,"x":2,"config":{}}`)); err != nil {
		t.Errorf("dup unknown: %v", err)
	}
}

// Strings and nesting, checked where serde_json checks them and nowhere else.
func TestStringAndNestingChecksFollowSerde(t *testing.T) {
	grok := func(body string) error {
		_, err := parseGrok([]byte(body))
		return err
	}
	deep := func(n int, leaf string) string { return strings.Repeat("[", n) + leaf + strings.Repeat("]", n) }
	rejected := map[string]string{
		"lone leading in field":          `{"config":{"subscriptionTier":"a\ud800b"}}`,
		"lone trailing in field":         `{"config":{"subscriptionTier":"\udc00"}}`,
		"leading then leading":           `{"config":{"subscriptionTier":"\ud800\ud800\udc00"}}`,
		"leading at end":                 `{"config":{"subscriptionTier":"\ud83d"}}`,
		"leading then other escape":      `{"config":{"subscriptionTier":"\ud83d\n"}}`,
		"lone surrogate in unknown key":  `{"config":{"x\udfffy":1}}`,
		"lone surrogate in top key":      `{"\ud800":1,"config":{}}`,
		"invalid UTF-8 in declared":      "{\"config\":{\"subscriptionTier\":\"\xc3\x28\"}}",
		"invalid UTF-8 in unknown key":   "{\"config\":{\"\xff\":1}}",
		"lone surrogate in declared key": `{"config":{"subscription\ud800Tier":"x"}}`,
	}
	for name, body := range rejected {
		if err := grok(body); err == nil {
			t.Errorf("%s: accepted", name)
		} else if strings.Contains(err.Error(), "ud8") || strings.Contains(err.Error(), "\xff") {
			t.Errorf("%s: error quotes input: %v", name, err)
		}
	}
	accepted := map[string]string{
		"pair":                          `{"config":{"subscriptionTier":"\ud83d\ude00"}}`,
		"lone surrogate in skipped":     `{"config":{"note":"\ud800"},"x":["\udc00",{"\ud800":1}]}`,
		"invalid UTF-8 in skipped":      "{\"config\":{\"note\":\"\xff\"},\"x\":{\"\xfe\":\"\xff\"}}",
		"out-of-range number skipped":   `{"config":{"extra":1e400},"x":-1e999}`,
		"deep skipped value":            `{"config":{},"x":` + deep(5000, "1") + `}`,
		"escaped quote and backslash":   `{"config":{"subscriptionTier":"a\"b\\c"}}`,
		"raw non-BMP in declared field": `{"config":{"subscriptionTier":"😀"}}`,
	}
	for name, body := range accepted {
		if err := grok(body); err != nil {
			t.Errorf("%s: %v", name, err)
		}
	}
	if q, _ := parseGrok([]byte(`{"config":{"subscriptionTier":"\ud83d\ude00 a\"b"}}`)); q == nil || *q.Plan != "😀 a\"b" {
		t.Errorf("decoded plan = %+v", q)
	}
	// encoding/json's own nesting cap (10000) is the only limit on skipped
	// values; Rust has none there.
	if err := grok(`{"config":{},"x":` + deep(10001, "1") + `}`); err == nil {
		t.Error("10001 levels accepted")
	}

	// Muse Code resets_at is a serde_json::Value: decoded in full, so its
	// strings, numbers, and nesting are checked. Root, subs_usage, and the
	// window are open around it: 124 more levels pass, 125 fail.
	muse := func(reset string) (*Quota, error) {
		q, _, err := parseMuseCode([]byte(`{"subs_usage":{"weekly":{"used_percent":1,"resets_at":` + reset + `}}}`))
		return q, err
	}
	if q, err := muse(deep(124, "1")); err != nil || q == nil || q.Windows[0].ResetsAt != nil {
		t.Errorf("124 levels: %+v %v", q, err)
	}
	if _, err := muse(deep(125, "1")); err == nil {
		t.Error("125 levels accepted")
	}
	for _, reset := range []string{`"\ud800"`, `["\udc00"]`, `{"\ud800":1}`, `[1e400]`, `1e400`, `{"a":-1e999}`} {
		if _, err := muse(reset); err == nil {
			t.Errorf("resets_at %s accepted", reset)
		}
	}
	if q, err := muse(`{"a":["\ud83d\ude00",1e300,null]}`); err != nil || q.Windows[0].ResetsAt != nil {
		t.Errorf("valid Value: %+v %v", q, err)
	}
	// serde decodes a window's resets_at before the window is skipped, so a
	// bad one fails the payload even where there is no usage to show.
	for _, window := range []string{`{"resets_at":1e400}`, `{"used_percent":-1,"resets_at":"\ud800"}`, `{"resets_at":` + deep(125, "1") + `}`} {
		body := `{"subs_usage":{"window":` + window + `,"weekly":{"used_percent":5}}}`
		if q, _, err := parseMuseCode([]byte(body)); err == nil {
			t.Errorf("skipped window %s: accepted as %+v", window, q)
		}
	}
	if q, _, err := parseMuseCode([]byte(`{"subs_usage":{"window":{"resets_at":"2026-01-01T00:00:00Z"},"weekly":{"used_percent":5}}}`)); err != nil || len(q.Windows) != 1 || q.Windows[0].Label != "weekly" {
		t.Errorf("valid skipped window: %+v %v", q, err)
	}
}

func TestParsesCopilotPremiumAllowance(t *testing.T) {
	q, err := parseCopilot(fixture(t, "copilot_user.json"))
	if err != nil || q == nil {
		t.Fatalf("%+v %v", q, err)
	}
	if *q.Plan != "individual_pro" || len(q.Windows) != 1 {
		t.Fatalf("unlimited chat must have no window: %+v", q)
	}
	w := q.Windows[0]
	if w.Label != "premium" || w.UsedPercent != 25 || *w.Scope != "375 of 1500" || FormatRFC3339(*w.ResetsAt) != "2026-08-01T00:00:00+00:00" {
		t.Errorf("%+v", w)
	}
}

func TestCopilotFreePlanMetersChatAndOverage(t *testing.T) {
	body := `{"copilot_plan":"free","quota_reset_date":"2026-08-15","quota_snapshots":{
		"premium_interactions":{"entitlement":50,"remaining":0,"percent_remaining":0,"unlimited":false,"overage_count":3},
		"chat":{"entitlement":50,"remaining":38,"unlimited":false},
		"completions":{"entitlement":2000,"remaining":1000,"percent_remaining":50,"unlimited":false}}}`
	q, err := parseCopilot([]byte(body))
	if err != nil || q == nil || len(q.Windows) != 2 {
		t.Fatalf("%+v %v", q, err)
	}
	if w := q.Windows[0]; w.Label != "premium" || w.UsedPercent != 100 || *w.Scope != "50 of 50 +3" || FormatRFC3339(*w.ResetsAt) != "2026-08-15T00:00:00+00:00" {
		t.Errorf("premium = %+v", w)
	}
	if w := q.Windows[1]; w.Label != "chat" || w.UsedPercent != 24 || *w.Scope != "12 of 50" {
		t.Errorf("chat = %+v", w)
	}
}

func TestCopilotWithoutMeteredAllowanceIsNone(t *testing.T) {
	for _, body := range []string{
		`{}`,
		`{"quota_snapshots":null}`,
		`{"quota_snapshots":{"premium_interactions":{"unlimited":true,"entitlement":0},"chat":{"unlimited":true}}}`,
		`{"quota_snapshots":{"premium_interactions":{"entitlement":300}}}`,
	} {
		if q, err := parseCopilot([]byte(body)); q != nil || err != nil {
			t.Errorf("%s = %+v %v", body, q, err)
		}
	}
	// A wrong type in a read field fails the payload; an unknown reset is nil.
	if _, err := parseCopilot([]byte(`{"quota_snapshots":{"premium_interactions":{"entitlement":"300"}}}`)); err == nil {
		t.Error("string entitlement accepted")
	}
	q, _ := parseCopilot([]byte(`{"quota_reset_date":"soon","quota_snapshots":{"premium_interactions":{"entitlement":300,"percent_remaining":90}}}`))
	if q == nil || q.Windows[0].ResetsAt != nil || *q.Windows[0].Scope != "30 of 300" {
		t.Errorf("%+v", q)
	}
}
