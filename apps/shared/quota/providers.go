package quota

import (
	"context"
	"encoding/json"
	"math"
	"strconv"
	"strings"
	"sync"
	"time"
)

// --- fetchers: one per provider, each a port of its Rust `fetch_*_quota` ---

func bearer(token string) [2]string { return [2]string{"Authorization", "Bearer " + token} }

func incompatibleUnless(q *Quota, err error) (*Quota, *Error) {
	if err != nil || q == nil {
		return nil, failure(KindIncompatible)
	}
	return q, nil
}

func claudeHeaders(access string) [][2]string {
	return [][2]string{
		bearer(access),
		{"anthropic-beta", "oauth-2025-04-20"},
		{"anthropic-version", "2023-06-01"},
		{"user-agent", "claude-cli/2.1.2 (external, cli)"},
		{"anthropic-dangerous-direct-browser-access", "true"},
		{"x-app", "cli"},
	}
}

// claude reads usage and, at the same time, the OAuth profile for the plan
// label. A failed profile read leaves the plan unset.
func (f *fetcher) claude(ctx context.Context, c *credentials) (*Quota, *Error) {
	if c == nil {
		return nil, failure(KindNotAuthenticated)
	}
	if f.expired(c) {
		return nil, failure(KindExpired)
	}
	var plan *string
	var wg sync.WaitGroup
	wg.Add(1)
	go func() {
		defer wg.Done()
		body, err := f.do(ctx, request{method: "GET", url: f.urls.ClaudeProfile, headers: claudeHeaders(c.access)})
		if err == nil {
			plan = claudePlan(body)
		}
	}()
	body, err := f.do(ctx, request{method: "GET", url: f.urls.ClaudeUsage, headers: claudeHeaders(c.access)})
	wg.Wait()
	if err != nil {
		return nil, err
	}
	return incompatibleUnless(parseClaude(body, plan))
}

func (f *fetcher) codex(ctx context.Context, c *credentials) (*Quota, *Error) {
	if c == nil {
		return nil, failure(KindNotAuthenticated)
	}
	if f.expired(c) {
		return nil, failure(KindExpired)
	}
	if c.accountID == nil {
		return nil, failure(KindNotAuthenticated)
	}
	body, err := f.do(ctx, request{method: "GET", url: f.urls.CodexUsage, headers: [][2]string{
		bearer(c.access),
		{"chatgpt-account-id", *c.accountID},
		{"originator", "zdx"},
		{"user-agent", "zdx/" + f.version},
	}})
	if err != nil {
		return nil, err
	}
	return incompatibleUnless(parseCodex(body))
}

// antigravityUserAgent is the client identity of zdx's Antigravity model
// client (crates/zdx-providers/src/gemini/antigravity.rs). The quota summary
// needs an Antigravity-style UA (a plain one 401s) and gates groups on its
// version: `antigravity/cli/1.0.0`, the Rust reader's UA, gets only the
// Claude and GPT group, so the Gemini-only filter found nothing. Verified
// live 2026-11-14.
const antigravityUserAgent = "antigravity/cli/2.24.0"

func (f *fetcher) antigravity(ctx context.Context, c *credentials) (*Quota, *Error) {
	if c == nil {
		return nil, failure(KindNotAuthenticated)
	}
	if f.expired(c) {
		return nil, failure(KindExpired)
	}
	project := ""
	if c.accountID != nil {
		project = *c.accountID
	}
	body, err := f.do(ctx, request{method: "POST", url: f.urls.AntigravityQuota, headers: [][2]string{
		bearer(c.access),
		{"Accept", "application/json"},
		{"user-agent", antigravityUserAgent},
		{"Content-Type", "application/json"},
	}, body: []byte(`{"project":` + jsonString(project) + `}`)})
	if err != nil {
		return nil, err
	}
	return incompatibleUnless(parseAntigravity(body))
}

func (f *fetcher) grok(ctx context.Context, c *credentials) (*Quota, *Error) {
	if c == nil {
		return nil, failure(KindNotAuthenticated)
	}
	if f.expired(c) {
		return nil, failure(KindExpired)
	}
	body, err := f.do(ctx, request{method: "GET", url: f.urls.GrokBilling, headers: [][2]string{
		bearer(c.access),
		{"X-XAI-Token-Auth", "xai-grok-cli"},
		{"x-grok-client-version", "1.0.0"},
		{"x-grok-client-mode", "interactive"},
	}})
	if err != nil {
		return nil, err
	}
	return incompatibleUnless(parseGrok(body))
}

// museCode reads the allowance from the key-mint call, sent with the stored
// identity token (the `refresh` field). The minted key in the answer is
// discarded and nothing is written. No expiry check: the stored expiry is
// the minted key's, not the identity token's. The key-mint call is itself
// the refresh: every quota fetch re-mints, so Muse Code is auto-refreshed
// on each monitor poll without a separate refresh-token flow. A 401 from
// the mint means the identity token was rejected and the user must re-login.
func (f *fetcher) museCode(ctx context.Context, c *credentials) (*Quota, *Error) {
	if c == nil {
		return nil, failure(KindNotAuthenticated)
	}
	body, err := f.do(ctx, request{method: "POST", url: f.urls.MuseCodeKey, headers: [][2]string{
		bearer(c.refresh),
		{"Content-Type", "application/json"},
		{"Accept", "application/json"},
		{"x-api-version", "1.0.0"},
		{"user-agent", "zdx/" + f.version},
	}, body: []byte("{}")})
	if err != nil {
		if err.Kind == KindUnauthorized {
			return nil, failure(KindExpired)
		}
		return nil, err
	}
	return museOutcome(body)
}

// Copilot identity: VS Code Copilot Chat, as zdx's Copilot model client sends
// it (crates/zdx-providers/src/oauth.rs, github_copilot::identity_headers).
// Keep the versions in step with that file.
const (
	copilotUserAgent           = "GitHubCopilotChat/0.35.0"
	copilotEditorVersion       = "vscode/1.107.0"
	copilotEditorPluginVersion = "copilot-chat/0.35.0"
)

// copilot reads copilot_internal/user with the stored GitHub OAuth token
// (the `refresh` field). That token has no expiry of its own; the stored
// expiry is the short-lived Copilot API token's (`access`), which this call
// does not use, so there is no expiry check and no refresh.
func (f *fetcher) copilot(ctx context.Context, c *credentials) (*Quota, *Error) {
	if c == nil || c.refresh == "" {
		return nil, failure(KindNotAuthenticated)
	}
	body, err := f.do(ctx, request{method: "GET", url: f.urls.CopilotUser, headers: [][2]string{
		{"Authorization", "token " + c.refresh},
		{"Accept", "application/json"},
		{"user-agent", copilotUserAgent},
		{"editor-version", copilotEditorVersion},
		{"editor-plugin-version", copilotEditorPluginVersion},
	}})
	if err != nil {
		return nil, err
	}
	return incompatibleUnless(parseCopilot(body))
}

// museOutcome is the Muse Code answer after a 2xx: an inactive subscription
// is unauthorized, an idle one is a 0% rolling window, else the parsed
// allowance.
func museOutcome(body []byte) (*Quota, *Error) {
	q, active, err := parseMuseCode(body)
	if err == nil && active != nil && !*active {
		return nil, failure(KindUnauthorized)
	}
	if err == nil && q == nil {
		q = museIdle(body)
	}
	return incompatibleUnless(q, err)
}

// museIdle reads an answer without `subs_usage`. Meta leaves it out while no
// rolling window is open (seen 2026-11-15: absent before the first request
// of the day, present right after it), so an active subscription with no
// payment demand is a 0% rolling window with no reset. The weekly figure is
// unknown then and is not reported. Any other answer stays nil. Only called
// after parseMuseCode accepted the body.
func museIdle(body []byte) *Quota {
	doc, _ := document(body)
	root, _ := object(doc, museFields...)
	active, _ := root.optBool("is_subs_active")
	payment, _ := root.optBool("require_payment")
	action, _ := root.optString("action_url")
	paymentAction, _ := root.optString("require_payment_action_url")
	if active == nil || !*active || (payment != nil && *payment) ||
		trimmedNonEmpty(action, "") != nil || trimmedNonEmpty(paymentAction, "") != nil {
		return nil
	}
	if _, ok := root["subs_usage"]; ok && !isNull(root["subs_usage"]) {
		return nil
	}
	plan, _ := root.optString("subs_tier_name")
	if plan == nil {
		plan, _ = root.optString("subs_tier_id")
	}
	return &Quota{Plan: trimmedNonEmpty(plan, ""), Windows: []Window{{Label: museLabel(nil), UsedPercent: 0}}}
}

// --- parsers: each a port of its Rust `parse_*` and wire structs ---

func window(label string, used float64, reset *string, scope *string) Window {
	return Window{Label: label, UsedPercent: used, ResetsAt: parseRFC3339(reset), Scope: scope}
}

func orZero(v *float64) float64 {
	if v == nil {
		return 0
	}
	return *v
}

// claudePlan is ClaudeProfileWire::plan_label: rate_limit_tier, else
// organization_type, trimmed, without a leading `default_`. Any decode
// failure is no plan.
func claudePlan(body []byte) *string {
	doc, err := document(body)
	if err != nil {
		return nil
	}
	root, err := object(doc, "organization")
	if err != nil {
		return nil
	}
	org, err := root.optObject("organization", "rate_limit_tier", "organization_type")
	if err != nil || org == nil {
		return nil
	}
	tier, err1 := org.optString("rate_limit_tier")
	kind, err2 := org.optString("organization_type")
	if err1 != nil || err2 != nil {
		return nil
	}
	if tier == nil {
		tier = kind
	}
	return trimmedNonEmpty(tier, "default_")
}

func trimmedNonEmpty(v *string, stripPrefix string) *string {
	if v == nil {
		return nil
	}
	s := rustTrim(*v)
	if s == "" {
		return nil
	}
	if stripPrefix != "" {
		s = strings.TrimPrefix(s, stripPrefix)
	}
	return &s
}

// rustTrim is Rust's `str::trim` (Unicode White_Space).
func rustTrim(s string) string {
	return strings.TrimFunc(s, func(r rune) bool {
		switch r {
		case '\t', '\n', '\v', '\f', '\r', ' ', 0x85, 0xA0, 0x1680, 0x2028, 0x2029, 0x202F, 0x205F, 0x3000:
			return true
		}
		return r >= 0x2000 && r <= 0x200A
	})
}

// parseClaude prefers the `limits[]` array (unscoped session, weekly, and
// model-scoped weekly), else the legacy five_hour/seven_day fields.
func parseClaude(body []byte, plan *string) (*Quota, error) {
	doc, err := document(body)
	if err != nil {
		return nil, err
	}
	root, err := object(doc, "five_hour", "seven_day", "limits")
	if err != nil {
		return nil, err
	}
	flat := func(name string) (*Window, error) {
		o, err := root.optObject(name, "utilization", "resets_at")
		if err != nil || o == nil {
			return nil, err
		}
		used, err := o.optFloat("utilization")
		if err != nil {
			return nil, err
		}
		reset, err := o.optString("resets_at")
		if err != nil {
			return nil, err
		}
		w := window("", orZero(used), reset, nil)
		return &w, nil
	}
	fiveHour, err := flat("five_hour")
	if err != nil {
		return nil, err
	}
	sevenDay, err := flat("seven_day")
	if err != nil {
		return nil, err
	}
	limits, err := root.defaultArray("limits")
	if err != nil {
		return nil, err
	}
	var windows []Window
	for _, raw := range limits {
		l, err := object(raw, "group", "percent", "resets_at", "scope")
		if err != nil {
			return nil, err
		}
		group, err := l.optString("group")
		if err != nil {
			return nil, err
		}
		percent, err := l.optFloat("percent")
		if err != nil {
			return nil, err
		}
		reset, err := l.optString("resets_at")
		if err != nil {
			return nil, err
		}
		var model *string
		scope, err := l.optObject("scope", "model")
		if err != nil {
			return nil, err
		}
		if scope != nil {
			m, err := scope.optObject("model", "display_name")
			if err != nil {
				return nil, err
			}
			if m != nil {
				if model, err = m.optString("display_name"); err != nil {
					return nil, err
				}
			}
		}
		var label string
		var windowScope *string
		switch {
		case group != nil && *group == "session" && model == nil:
			label = "5h"
		case group != nil && *group == "weekly" && model == nil:
			label = "weekly"
		case group != nil && *group == "weekly":
			label, windowScope = "weekly", model
		default:
			continue
		}
		windows = append(windows, window(label, orZero(percent), reset, windowScope))
	}
	if len(windows) == 0 {
		if fiveHour != nil {
			fiveHour.Label = "5h"
			windows = append(windows, *fiveHour)
		}
		if sevenDay != nil {
			sevenDay.Label = "weekly"
			windows = append(windows, *sevenDay)
		}
	}
	if len(windows) == 0 {
		return nil, nil
	}
	return &Quota{Plan: plan, Windows: windows}, nil
}

func codexLabel(seconds *int64) string {
	if seconds != nil {
		switch *seconds {
		case 18_000:
			return "5h"
		case 604_800:
			return "weekly"
		}
	}
	return "window"
}

func timestampPtr(secs *int64) *time.Time {
	if secs == nil {
		return nil
	}
	return timestamp(*secs)
}

// parseCodex reads the rolling rate_limit windows, else (business plans,
// `rate_limit: null`) the dollar budget under spend_control.individual_limit.
func parseCodex(body []byte) (*Quota, error) {
	doc, err := document(body)
	if err != nil {
		return nil, err
	}
	root, err := object(doc, "plan_type", "rate_limit", "spend_control")
	if err != nil {
		return nil, err
	}
	plan, err := root.optString("plan_type")
	if err != nil {
		return nil, err
	}
	rate, err := root.optObject("rate_limit", "primary_window", "secondary_window")
	if err != nil {
		return nil, err
	}
	var spendWindow *Window
	spendPresent := false
	spend, err := root.optObject("spend_control", "individual_limit")
	if err != nil {
		return nil, err
	}
	if spend != nil {
		limit, err := spend.optObject("individual_limit", "limit", "used", "used_percent", "reset_at")
		if err != nil {
			return nil, err
		}
		if limit != nil {
			spendPresent = true
			if spendWindow, err = codexSpendWindow(limit); err != nil {
				return nil, err
			}
		}
	}
	var windows []Window
	if rate != nil {
		for _, name := range []string{"primary_window", "secondary_window"} {
			w, err := rate.optObject(name, "used_percent", "limit_window_seconds", "reset_at")
			if err != nil {
				return nil, err
			}
			if w == nil {
				continue
			}
			used, err := w.optFloat("used_percent")
			if err != nil {
				return nil, err
			}
			seconds, err := w.optInt64("limit_window_seconds")
			if err != nil {
				return nil, err
			}
			reset, err := w.optInt64("reset_at")
			if err != nil {
				return nil, err
			}
			windows = append(windows, Window{Label: codexLabel(seconds), UsedPercent: orZero(used), ResetsAt: timestampPtr(reset)})
		}
	}
	if len(windows) == 0 {
		if !spendPresent || spendWindow == nil {
			return nil, nil
		}
		windows = append(windows, *spendWindow)
	}
	return &Quota{Plan: plan, Windows: windows}, nil
}

// codexSpendWindow is a dollar budget, labelled `spend`. The payload's
// used_percent is integer-rounded, so the percentage comes from used/limit
// whenever both parse and limit > 0; the scope then shows the dollars.
func codexSpendWindow(o fields) (*Window, error) {
	limitText, err := o.optString("limit")
	if err != nil {
		return nil, err
	}
	usedText, err := o.optString("used")
	if err != nil {
		return nil, err
	}
	usedPercent, err := o.optFloat("used_percent")
	if err != nil {
		return nil, err
	}
	reset, err := o.optInt64("reset_at")
	if err != nil {
		return nil, err
	}
	parse := func(p *string) (float64, bool) {
		if p == nil {
			return 0, false
		}
		return parseRustFloat(*p)
	}
	limit, okLimit := parse(limitText)
	used, okUsed := parse(usedText)
	w := Window{Label: "spend", ResetsAt: timestampPtr(reset)}
	if okUsed && okLimit && limit > 0 {
		w.UsedPercent = used / limit * 100
		scope := "$" + rustFixed(used, 2) + " of $" + rustFixed(limit, 2)
		w.Scope = &scope
	} else {
		if usedPercent == nil {
			return nil, nil
		}
		w.UsedPercent = *usedPercent
	}
	return &w, nil
}

// parseAntigravity keeps only the Gemini model group (name contains
// "gemini", ASCII case-insensitive); the group name is the window scope.
func parseAntigravity(body []byte) (*Quota, error) {
	doc, err := document(body)
	if err != nil {
		return nil, err
	}
	root, err := object(doc, "groups")
	if err != nil {
		return nil, err
	}
	groups, err := root.defaultArray("groups")
	if err != nil {
		return nil, err
	}
	var windows []Window
	for _, raw := range groups {
		g, err := object(raw, "displayName", "buckets")
		if err != nil {
			return nil, err
		}
		name, err := g.optString("displayName")
		if err != nil {
			return nil, err
		}
		buckets, err := g.defaultArray("buckets")
		if err != nil {
			return nil, err
		}
		type bucket struct {
			label string
			used  float64
			reset *string
		}
		var parsed []bucket
		for _, braw := range buckets {
			b, err := object(braw, "window", "remainingFraction", "resetTime")
			if err != nil {
				return nil, err
			}
			win, err := b.optString("window")
			if err != nil {
				return nil, err
			}
			remaining, err := b.optFloat("remainingFraction")
			if err != nil {
				return nil, err
			}
			reset, err := b.optString("resetTime")
			if err != nil {
				return nil, err
			}
			label := "weekly"
			if win != nil && *win == "5h" {
				label = "5h"
			}
			fraction := 1.0
			if remaining != nil {
				fraction = *remaining
			}
			parsed = append(parsed, bucket{label: label, used: (1.0 - fraction) * 100.0, reset: reset})
		}
		if name == nil || !strings.Contains(asciiLower(*name), "gemini") {
			continue
		}
		for _, b := range parsed {
			scope := *name
			windows = append(windows, window(b.label, b.used, b.reset, &scope))
		}
	}
	if len(windows) == 0 {
		return nil, nil
	}
	return &Quota{Windows: windows}, nil
}

func asciiLower(s string) string {
	b := []byte(s)
	for i, c := range b {
		if c >= 'A' && c <= 'Z' {
			b[i] = c + 32
		}
	}
	return string(b)
}

// parseGrok reads one credit-usage window. proto3 JSON omits zero values,
// so a missing creditUsagePercent is 0%.
func parseGrok(body []byte) (*Quota, error) {
	doc, err := document(body)
	if err != nil {
		return nil, err
	}
	root, err := object(doc, "config")
	if err != nil {
		return nil, err
	}
	config, err := root.optObject("config", "creditUsagePercent", "currentPeriod", "subscriptionTier")
	if err != nil || config == nil {
		return nil, err
	}
	used, err := config.optFloat("creditUsagePercent")
	if err != nil {
		return nil, err
	}
	tier, err := config.optString("subscriptionTier")
	if err != nil {
		return nil, err
	}
	period, err := config.optObject("currentPeriod", "type", "end")
	if err != nil {
		return nil, err
	}
	label := "weekly"
	var end *string
	if period != nil {
		kind, err := period.optString("type")
		if err != nil {
			return nil, err
		}
		if end, err = period.optString("end"); err != nil {
			return nil, err
		}
		if kind != nil && *kind == "USAGE_PERIOD_TYPE_MONTHLY" {
			label = "monthly"
		}
	}
	return &Quota{Plan: tier, Windows: []Window{window(label, orZero(used), end, nil)}}, nil
}

var museFields = []string{
	"api_key", "is_subs_active", "require_payment", "action_url", "require_payment_action_url",
	"user_email", "user_id", "subs_tier_id", "subs_tier_name", "subs_usage",
}

// parseMuseCode reads the `subs_usage` allowance (a rolling window and a
// weekly window) and the tier name. It also returns is_subs_active, which
// the caller checks first. Every declared field of the Rust response struct
// is type-checked, as serde did; the minted api_key is never kept.
func parseMuseCode(body []byte) (*Quota, *bool, error) {
	doc, err := document(body)
	if err != nil {
		return nil, nil, err
	}
	root, err := object(doc, museFields...)
	if err != nil {
		return nil, nil, err
	}
	for _, name := range []string{"api_key", "action_url", "require_payment_action_url", "user_email", "user_id"} {
		if _, err := root.optString(name); err != nil {
			return nil, nil, err
		}
	}
	if _, err := root.optBool("require_payment"); err != nil {
		return nil, nil, err
	}
	active, err := root.optBool("is_subs_active")
	if err != nil {
		return nil, nil, err
	}
	tierID, err := root.optString("subs_tier_id")
	if err != nil {
		return nil, nil, err
	}
	tierName, err := root.optString("subs_tier_name")
	if err != nil {
		return nil, nil, err
	}
	usage, err := root.optObject("subs_usage", "window", "weekly")
	if err != nil {
		return nil, nil, err
	}
	if usage == nil {
		return nil, active, nil
	}
	var windows []Window
	for _, item := range [][2]string{{"window", ""}, {"weekly", "weekly"}} {
		w, err := usage.optObject(item[0], "used_percent", "resets_at", "window_duration_mins")
		if err != nil {
			return nil, nil, err
		}
		if w == nil {
			continue
		}
		used, err := w.optFloat("used_percent")
		if err != nil {
			return nil, nil, err
		}
		mins, err := w.optFloat("window_duration_mins")
		if err != nil {
			return nil, nil, err
		}
		// Decoded before the window may be skipped: serde decoded every
		// field, so a bad reset fails even a window without usage.
		reset, err := museReset(w["resets_at"])
		if err != nil {
			return nil, nil, err
		}
		if used == nil || math.IsInf(*used, 0) || math.IsNaN(*used) || *used < 0 {
			continue
		}
		label := item[1]
		if label == "" {
			label = museLabel(mins)
		}
		windows = append(windows, Window{Label: label, UsedPercent: *used, ResetsAt: reset})
	}
	if len(windows) == 0 {
		return nil, active, nil
	}
	plan := tierName
	if plan == nil {
		plan = tierID
	}
	return &Quota{Plan: trimmedNonEmpty(plan, ""), Windows: windows}, active, nil
}

// museLabel names a rolling window by its length (300 min is `5h`, 90 is
// `90m`); no positive length is `rolling`.
func museLabel(mins *float64) string {
	if mins == nil || !(*mins > 0) {
		return "rolling"
	}
	m := math.Round(*mins)
	if math.Abs(math.Mod(m, 60)) < 2.220446049250313e-16 {
		return strconv.FormatInt(saturatingInt64(m/60), 10) + "h"
	}
	return strconv.FormatInt(saturatingInt64(m), 10) + "m"
}

// museReset reads `resets_at`, a serde_json::Value: RFC 3339 text or
// positive epoch seconds (fraction truncated) is a reset, any other value is
// unknown. The Value is decoded in full first, so an invalid string or
// number anywhere in it, or nesting past the limit (root, subs_usage, and
// the window are already open), fails the payload as in Rust.
func museReset(raw json.RawMessage) (*time.Time, error) {
	if isNull(raw) {
		return nil, nil
	}
	if err := checkValue(raw, 3); err != nil {
		return nil, err
	}
	switch c := raw[0]; {
	case c == '"':
		s, err := decodeString(raw)
		if err != nil {
			return nil, err
		}
		return parseRFC3339(&s), nil
	case c == '-' || (c >= '0' && c <= '9'):
		n, err := number(raw)
		if err != nil {
			return nil, err
		}
		secs := n.float()
		if secs <= 0 {
			return nil, nil
		}
		return timestamp(saturatingInt64(secs)), nil
	}
	return nil, nil
}

// parseCopilot reads the metered monthly allowances of copilot_internal/user:
// premium requests (`premium`) and chat (`chat`, metered only on Free). An
// allowance that is unlimited or has no positive entitlement has no window.
// The plan is copilot_plan. The reset is quota_reset_date_utc, else
// quota_reset_date (a plain date is 00:00 UTC).
func parseCopilot(body []byte) (*Quota, error) {
	doc, err := document(body)
	if err != nil {
		return nil, err
	}
	root, err := object(doc, "copilot_plan", "quota_reset_date", "quota_reset_date_utc", "quota_snapshots")
	if err != nil {
		return nil, err
	}
	plan, err := root.optString("copilot_plan")
	if err != nil {
		return nil, err
	}
	resetUTC, err := root.optString("quota_reset_date_utc")
	if err != nil {
		return nil, err
	}
	resetDate, err := root.optString("quota_reset_date")
	if err != nil {
		return nil, err
	}
	reset := copilotReset(resetUTC, resetDate)
	snapshots, err := root.optObject("quota_snapshots", "premium_interactions", "chat")
	if err != nil || snapshots == nil {
		return nil, err
	}
	var windows []Window
	for _, item := range [][2]string{{"premium_interactions", "premium"}, {"chat", "chat"}} {
		s, err := snapshots.optObject(item[0], "entitlement", "remaining", "percent_remaining", "unlimited", "overage_count")
		if err != nil {
			return nil, err
		}
		if s == nil {
			continue
		}
		w, err := copilotWindow(item[1], s, reset)
		if err != nil {
			return nil, err
		}
		if w != nil {
			windows = append(windows, *w)
		}
	}
	if len(windows) == 0 {
		return nil, nil
	}
	return &Quota{Plan: trimmedNonEmpty(plan, ""), Windows: windows}, nil
}

// copilotWindow is one quota snapshot. Used percent is 100 -
// percent_remaining (else derived from remaining); the scope is the used
// count of the entitlement, with any overage as ` +N`.
func copilotWindow(label string, s fields, reset *time.Time) (*Window, error) {
	unlimited, err := s.optBool("unlimited")
	if err != nil {
		return nil, err
	}
	var nums [4]*float64
	for i, name := range []string{"entitlement", "remaining", "percent_remaining", "overage_count"} {
		if nums[i], err = s.optFloat(name); err != nil {
			return nil, err
		}
	}
	entitlement, remaining, percent, overage := nums[0], nums[1], nums[2], nums[3]
	if (unlimited != nil && *unlimited) || entitlement == nil || !(*entitlement > 0) {
		return nil, nil
	}
	var used, count float64
	switch {
	case percent != nil:
		used = 100 - *percent
		count = *entitlement * used / 100
		if remaining != nil {
			count = *entitlement - *remaining
		}
	case remaining != nil:
		count = *entitlement - *remaining
		used = count / *entitlement * 100
	default:
		return nil, nil
	}
	scope := rustFixed(math.Max(count, 0), 0) + " of " + rustFixed(*entitlement, 0)
	if overage != nil && *overage > 0 {
		scope += " +" + rustFixed(*overage, 0)
	}
	return &Window{Label: label, UsedPercent: used, ResetsAt: reset, Scope: &scope}, nil
}

// copilotReset is the first reset text that parses: the UTC timestamp, the
// date as sent, then the date at 00:00 UTC.
func copilotReset(utc, date *string) *time.Time {
	if t := parseRFC3339(utc); t != nil {
		return t
	}
	if date == nil {
		return nil
	}
	if t := parseRFC3339(date); t != nil {
		return t
	}
	midnight := *date + "T00:00:00Z"
	return parseRFC3339(&midnight)
}
