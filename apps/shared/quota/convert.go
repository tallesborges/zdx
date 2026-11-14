package quota

import (
	"math"
	"sort"
	"strconv"
	"strings"
	"time"
	"unicode/utf8"
)

// Subscription is one account in the monitor and Mini App views (the Rust
// MonitorSubscription body). Field order and names follow it.
type Subscription struct {
	Provider string               `json:"provider"`
	Name     string               `json:"name"`
	Plan     *string              `json:"plan"`
	Windows  []SubscriptionWindow `json:"windows"`
	Error    *string              `json:"error"`
}

// SubscriptionWindow is one window of a Subscription.
type SubscriptionWindow struct {
	Label       string  `json:"label"`
	UsedPercent float64 `json:"used_percent"`
	ResetsAt    *string `json:"resets_at"`
	Scope       *string `json:"scope"`
}

// Subscriptions is the view body Rust built from a snapshot: accounts that
// are not logged in are dropped, used percent is clamped to 0..=100 (a
// non-finite value is 0), resets are chrono RFC 3339, a failed account has
// no windows and its reason, and the list is stably sorted by name
// (bytewise). Windows is never nil, so it encodes as [].
func Subscriptions(s Snapshot) []Subscription {
	out := []Subscription{}
	for _, r := range s.Providers {
		sub := Subscription{Provider: r.Provider, Name: AccountDisplay(r.Provider, r.Account), Windows: []SubscriptionWindow{}}
		switch {
		case r.Quota != nil:
			sub.Plan = r.Quota.Plan
			for _, w := range r.Quota.Windows {
				sub.Windows = append(sub.Windows, SubscriptionWindow{
					Label: w.Label, UsedPercent: clampPercent(w.UsedPercent), ResetsAt: formatReset(w.ResetsAt), Scope: w.Scope,
				})
			}
		case r.Err != nil && r.Err.Kind == KindNotAuthenticated:
			continue
		case r.Err != nil:
			reason := r.Err.Reason()
			sub.Error = &reason
		}
		out = append(out, sub)
	}
	sort.SliceStable(out, func(i, j int) bool { return out[i].Name < out[j].Name })
	return out
}

// clampPercent is Rust's `f64::clamp(0.0, 100.0)` for finite values (a
// negative zero stays as is), else 0.
func clampPercent(v float64) float64 {
	switch {
	case math.IsNaN(v) || math.IsInf(v, 0):
		return 0
	case v < 0:
		return 0
	case v > 100:
		return 100
	}
	return v
}

func formatReset(t *time.Time) *string {
	if t == nil {
		return nil
	}
	s := FormatRFC3339(*t)
	return &s
}

// SubscriptionsJSON is the compact serde_json text of a view body as the
// Rust service sent it: object keys sorted, Rust float text.
func SubscriptionsJSON(subs []Subscription) []byte {
	items := make([]jsonValue, 0, len(subs))
	for _, s := range subs {
		windows := make([]jsonValue, 0, len(s.Windows))
		for _, w := range s.Windows {
			windows = append(windows, jsonObject{
				{"label", jsonText(w.Label)},
				{"resets_at", optText(w.ResetsAt)},
				{"scope", optText(w.Scope)},
				{"used_percent", jsonNum(w.UsedPercent)},
			})
		}
		items = append(items, jsonObject{
			{"error", optText(s.Error)},
			{"name", jsonText(s.Name)},
			{"plan", optText(s.Plan)},
			{"provider", jsonText(s.Provider)},
			{"windows", jsonArray(windows)},
		})
	}
	var b strings.Builder
	writeJSON(&b, jsonArray(items), -1)
	return []byte(b.String())
}

// CLIJSON is the stdout of `zdx quota --json`: serde_json pretty text with
// sorted keys, the default account as null, and a trailing newline.
func CLIJSON(s Snapshot) []byte {
	items := make([]jsonValue, 0, len(s.Providers))
	for _, r := range s.Providers {
		if r.Quota != nil {
			windows := make([]jsonValue, 0, len(r.Quota.Windows))
			for _, w := range r.Quota.Windows {
				windows = append(windows, jsonObject{
					{"label", jsonText(w.Label)},
					{"resets_at", optText(formatReset(w.ResetsAt))},
					{"scope", optText(w.Scope)},
					{"used_percent", jsonNum(w.UsedPercent)},
				})
			}
			items = append(items, jsonObject{
				{"account", optText(r.Account)},
				{"plan", optText(r.Quota.Plan)},
				{"provider", jsonText(r.Provider)},
				{"windows", jsonArray(windows)},
			})
			continue
		}
		items = append(items, jsonObject{
			{"account", optText(r.Account)},
			{"error", jsonText(errReason(r.Err))},
			{"provider", jsonText(r.Provider)},
		})
	}
	var b strings.Builder
	writeJSON(&b, jsonObject{{"providers", jsonArray(items)}}, 0)
	b.WriteByte('\n')
	return []byte(b.String())
}

func errReason(e *Error) string {
	if e == nil {
		return failure(KindIncompatible).Reason()
	}
	return e.Reason()
}

// CLIText is the stdout of `zdx quota`. now is the clock for "resets in".
func CLIText(s Snapshot, now time.Time) string {
	var b strings.Builder
	b.WriteString("Subscriptions (live quota)\n")
	for _, r := range s.Providers {
		name := AccountDisplay(r.Provider, r.Account)
		if r.Quota == nil {
			b.WriteString("  " + name + "   " + errReason(r.Err) + "\n")
			continue
		}
		plan := ""
		if r.Quota.Plan != nil {
			plan = " [" + *r.Quota.Plan + "]"
		}
		b.WriteString("  " + name + plan + "\n")
		for _, w := range r.Quota.Windows {
			reset := ""
			if w.ResetsAt != nil {
				reset = "   " + resetIn(*w.ResetsAt, now)
			}
			scope := ""
			if w.Scope != nil {
				scope = "   · " + *w.Scope
			}
			b.WriteString("    " + padRight(w.Label, 7) + " " + padLeft(rustFixed(w.UsedPercent, 0), 4) + "%" + reset + scope + "\n")
		}
	}
	return b.String()
}

// resetIn is the CLI's `format_reset_in`: whole seconds toward zero, as
// chrono's `num_seconds`.
func resetIn(at, now time.Time) string {
	secs := chronoSeconds(at, now)
	if secs <= 0 {
		return "reset due"
	}
	days, hours, mins := secs/86_400, (secs%86_400)/3_600, (secs%3_600)/60
	switch {
	case days > 0:
		return "resets in " + strconv.FormatInt(days, 10) + "d " + strconv.FormatInt(hours, 10) + "h"
	case hours > 0:
		return "resets in " + strconv.FormatInt(hours, 10) + "h " + strconv.FormatInt(mins, 10) + "m"
	}
	return "resets in " + strconv.FormatInt(mins, 10) + "m"
}

// chronoSeconds is chrono's `(at - now).num_seconds()`: whole seconds
// toward zero. A leap second (stored as the next second, see leapSecond)
// counts as part of second :59, and chrono's time-of-day subtraction takes
// one second off when that :59 is earlier in its day than now is in its
// day.
func chronoSeconds(at, now time.Time) int64 {
	if at.Location() == leapSecond && secondOfDay(at.Add(-time.Second)) < secondOfDay(now) {
		at = at.Add(-time.Second)
	}
	secs := at.Unix() - now.Unix()
	nanos := at.Nanosecond() - now.Nanosecond()
	if nanos < 0 {
		secs--
		nanos += 1_000_000_000
	}
	if secs < 0 && nanos > 0 {
		secs++
	}
	return secs
}

func secondOfDay(t time.Time) int {
	h, m, s := t.UTC().Clock()
	return h*3600 + m*60 + s
}

// padRight and padLeft are Rust's `{:<N}` and `{:>N}`: width in chars.
func padRight(s string, width int) string {
	if n := utf8.RuneCountInString(s); n < width {
		return s + strings.Repeat(" ", width-n)
	}
	return s
}

func padLeft(s string, width int) string {
	if n := utf8.RuneCountInString(s); n < width {
		return strings.Repeat(" ", width-n) + s
	}
	return s
}

// --- a minimal serde_json writer (keys are written in the given order) ---

type jsonValue interface{}

type (
	jsonText   string
	jsonNum    float64
	jsonNull   struct{}
	jsonArray  []jsonValue
	jsonObject []jsonMember
)

type jsonMember struct {
	key   string
	value jsonValue
}

func optText(s *string) jsonValue {
	if s == nil {
		return jsonNull{}
	}
	return jsonText(*s)
}

// writeJSON writes compact text when indent < 0, else serde_json's pretty
// form (two spaces, `": "`, empty containers as [] and {}).
func writeJSON(b *strings.Builder, v jsonValue, indent int) {
	newline := func(level int) {
		if indent >= 0 {
			b.WriteByte('\n')
			b.WriteString(strings.Repeat("  ", level))
		}
	}
	switch v := v.(type) {
	case jsonText:
		b.WriteString(jsonString(string(v)))
	case jsonNum:
		b.WriteString(jsonFloat(float64(v)))
	case jsonNull:
		b.WriteString("null")
	case jsonArray:
		if len(v) == 0 {
			b.WriteString("[]")
			return
		}
		b.WriteByte('[')
		for i, item := range v {
			if i > 0 {
				b.WriteByte(',')
			}
			newline(indent + 1)
			writeJSON(b, item, next(indent))
		}
		newline(indent)
		b.WriteByte(']')
	case jsonObject:
		if len(v) == 0 {
			b.WriteString("{}")
			return
		}
		b.WriteByte('{')
		for i, m := range v {
			if i > 0 {
				b.WriteByte(',')
			}
			newline(indent + 1)
			b.WriteString(jsonString(m.key))
			if indent >= 0 {
				b.WriteString(": ")
			} else {
				b.WriteByte(':')
			}
			writeJSON(b, m.value, next(indent))
		}
		newline(indent)
		b.WriteByte('}')
	}
}

func next(indent int) int {
	if indent < 0 {
		return indent
	}
	return indent + 1
}

// jsonString escapes as serde_json: `"` `\` and the short forms \b \f \n \r
// \t, other control bytes as lowercase \u00XX, everything else raw (no
// HTML or U+2028 escaping).
func jsonString(s string) string {
	const hex = "0123456789abcdef"
	var b strings.Builder
	b.WriteByte('"')
	for i := 0; i < len(s); i++ {
		c := s[i]
		switch c {
		case '"':
			b.WriteString(`\"`)
		case '\\':
			b.WriteString(`\\`)
		case '\b':
			b.WriteString(`\b`)
		case '\f':
			b.WriteString(`\f`)
		case '\n':
			b.WriteString(`\n`)
		case '\r':
			b.WriteString(`\r`)
		case '\t':
			b.WriteString(`\t`)
		default:
			if c < 0x20 {
				b.WriteString(`\u00`)
				b.WriteByte(hex[c>>4])
				b.WriteByte(hex[c&0xf])
			} else {
				b.WriteByte(c)
			}
		}
	}
	b.WriteByte('"')
	return b.String()
}
