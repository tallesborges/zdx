package ui

import (
	"fmt"
	"strings"
	"time"

	"github.com/tallesborges/zdx/apps/monitor/internal/data"
)

var usageWindows = []uint32{1, 7, 30, 90, 365}

type usageTab struct {
	days         uint32
	data         *data.Usage
	quotas       []data.Subscription
	quotasLoaded bool
	scroll       scrollState
}

func (t *usageTab) cycleDays(up bool) {
	i := 0
	for j, d := range usageWindows {
		if d == t.days {
			i = j
		}
	}
	if up {
		i = min(i+1, len(usageWindows)-1)
	} else {
		i = max(i-1, 0)
	}
	t.days = usageWindows[i]
}

func tokens(n uint64) string {
	switch {
	case n >= 1_000_000_000:
		return fmt.Sprintf("%.1fB", float64(n)/1e9)
	case n >= 1_000_000:
		return fmt.Sprintf("%.1fM", float64(n)/1e6)
	case n >= 1_000:
		return fmt.Sprintf("%.1fk", float64(n)/1e3)
	}
	return fmt.Sprint(n)
}

var bars = []rune(" ▁▂▃▄▅▆▇█")

func sparkline(days []data.DailyUsage) string {
	var peak uint64
	for _, d := range days {
		peak = max(peak, d.Tokens)
	}
	var b strings.Builder
	for _, d := range days {
		i := 0
		if peak > 0 {
			i = int((d.Tokens*uint64(len(bars)-1) + peak - 1) / peak)
		}
		b.WriteRune(bars[i])
	}
	return b.String()
}

func dayLabel(day int32) string {
	return time.Unix(int64(day)*86400, 0).UTC().Format("2 Jan")
}

func (m *Model) renderUsage(width, h int) []string {
	lines := m.usageLines(width)
	m.usage.scroll.clamp(len(lines), h)
	return lines[m.usage.scroll.offset:min(m.usage.scroll.offset+h, len(lines))]
}

// quotaStyle colors a window by how much of it is used.
func quotaStyle(used float64) func(...string) string {
	switch {
	case used >= 90:
		return bad.Render
	case used >= 70:
		return warn.Render
	}
	return ok.Render
}

// quotaLines are the subscription quotas: each account's windows as the
// provider reports them. Windows are never merged or compared across
// accounts.
func (m *Model) quotaLines(width int) []string {
	t := &m.usage
	st := m.fetch[keyQuotas]
	now := m.now()
	meta := ""
	switch {
	case st.inflight && !t.quotasLoaded:
		meta = spinner(now) + " fetching from every account…"
	case st.inflight:
		meta = spinner(now) + " refreshing · fetched " + span(now.Sub(st.fetched)) + " ago"
	case t.quotasLoaded:
		meta = "fetched " + span(now.Sub(st.fetched)) + " ago · every 5m · Q refresh"
	}
	out := []string{sectionTitle("Subscription quotas", meta)}
	if st.err != nil {
		// Wrapped, not cut: the error can say how to fix it (an unknown
		// zdx version names the build flag and --zdx-version).
		if t.quotasLoaded {
			for _, l := range wrapText("Last refresh failed: "+oneLine(st.err.Error())+". Values below are from "+span(now.Sub(st.fetched))+" ago.", width-2) {
				out = append(out, "  "+warn.Render(l))
			}
		} else {
			for _, l := range wrapText("Could not fetch quotas: "+oneLine(st.err.Error()), width-2) {
				out = append(out, "  "+bad.Render(l))
			}
			return out
		}
	}
	if !t.quotasLoaded {
		if !st.everTried {
			return append(out, "  "+dim.Render("Not fetched yet."))
		}
		return out
	}
	if len(t.quotas) == 0 {
		return append(out, "  "+dim.Render("No quotas reported."))
	}
	barW := 24
	switch {
	case width < 70:
		barW = 8
	case width < 100:
		barW = 12
	}
	// One table for every account, so columns line up and the header
	// names each figure once.
	tb := table{cols: []col{
		{title: "WINDOW", max: 10},
		{title: "", max: barW},
		{title: "USED", max: 6, right: true},
		{title: "REMAINING", max: 9, right: true},
		{title: "RESETS", flex: true, min: 6},
		{title: "SCOPE", max: 16, minWidth: 90},
	}}
	type rowAt struct{ account, row int }
	var rows []rowAt
	for a, s := range t.quotas {
		for _, w := range s.Windows {
			reset := cs("unknown", dim)
			if w.ResetsAt != nil {
				if width < 110 {
					reset = c(relTime(*w.ResetsAt, now))
				} else {
					reset = c(when(*w.ResetsAt, now))
				}
			}
			style := quotaStyle(w.UsedPercent)
			tb.rows = append(tb.rows, []cell{
				c(w.Label), c(meter(w.UsedPercent, barW)),
				c(style(fmt.Sprintf("%.1f%%", w.UsedPercent))), cs(fmt.Sprintf("%.1f%%", max(100-w.UsedPercent, 0)), dim),
				reset, cs(str(w.Scope), dim),
			})
			rows = append(rows, rowAt{a, len(tb.rows) - 1})
		}
	}
	tb.layout(width - 2)
	if len(tb.rows) > 0 {
		out = append(out, "  "+tb.header())
	}
	for a, s := range t.quotas {
		if a > 0 {
			out = append(out, "")
		}
		head := "  " + bold.Render(s.Name) + dim.Render("  "+s.Provider)
		if s.Plan != nil {
			head += dim.Render(" · " + *s.Plan + " plan")
		}
		out = append(out, head)
		if s.Error != nil {
			out = append(out, "    "+bad.Render("✗ "+oneLine(*s.Error)))
			continue
		}
		if len(s.Windows) == 0 {
			out = append(out, "    "+dim.Render("No quota windows reported."))
			continue
		}
		for _, r := range rows {
			if r.account == a {
				out = append(out, fit("  "+tb.row(r.row, false, width-2), width))
			}
		}
	}
	return out
}

func (m *Model) usageLines(width int) []string {
	t := &m.usage
	out := m.quotaLines(width)
	out = append(out, "", rule(fmt.Sprintf("Token usage · last %d days · [ ] change", t.days), width))
	u := t.data
	if u == nil {
		return append(out, m.pending(keyUsage, "usage")...)
	}
	cost := fmt.Sprintf("$%.2f billed", u.BilledUSD)
	out = append(out,
		fmt.Sprintf("  %s tokens · %s requests · %s · %s on subscriptions", bold.Render(tokens(u.Tokens)), tokens(u.Requests), cost, tokens(u.SubscriptionTokens)),
		dim.Render(fmt.Sprintf("  input %s · output %s · cache read %s · cache write %s · %d threads", tokens(u.Input), tokens(u.Output), tokens(u.CacheRead), tokens(u.CacheWrite), u.ThreadsScanned)))
	if u.UnknownPricingRows > 0 {
		out = append(out, "  "+warn.Render(fmt.Sprintf("%d rows have no known price: the cost is a lower bound.", u.UnknownPricingRows)))
	}
	if u.UnindexedThreads > 0 {
		out = append(out, "  "+warn.Render(fmt.Sprintf("%d threads are not indexed: their project and run kind are unknown.", u.UnindexedThreads)))
	}
	if len(u.Daily) > 0 {
		out = append(out, "", "  "+dim.Render("Daily  ")+accent.Render(sparkline(u.Daily))+dim.Render(fmt.Sprintf("  %s – %s", dayLabel(u.Daily[0].Day), dayLabel(u.Daily[len(u.Daily)-1].Day))))
	}
	section := func(title string, tb table) {
		out = append(out, "", "  "+bold.Render(title))
		if len(tb.rows) == 0 {
			out = append(out, "  "+dim.Render("none"))
			return
		}
		tb.layout(width)
		out = append(out, tb.header())
		for i := range tb.rows {
			out = append(out, tb.row(i, false, width))
		}
	}
	rowsOf := func(rows []data.UsageRow, withModel bool) table {
		tb := table{cols: []col{{title: "TOKENS", max: 8, right: true}, {title: "REQUESTS", max: 8, right: true}, {title: "COST", max: 13}, {title: "NAME", flex: true, min: 10}}}
		for _, r := range rows {
			name := c(r.Provider)
			if withModel && r.Model != nil {
				name = c(*r.Model + "  " + dim.Render(r.Provider))
			}
			cost := c(fmt.Sprintf("$%.2f", r.CostUSD))
			if r.Subscription {
				cost = cs("subscription", dim)
			}
			if r.Estimated {
				cost.text += "~"
			}
			tb.rows = append(tb.rows, []cell{c(tokens(r.Tokens)), cs(fmt.Sprint(r.Requests), dim), cost, name})
		}
		return tb
	}
	section("By provider", rowsOf(u.ByProvider, false))
	section("By model (top 8)", rowsOf(u.ByModel, true))
	proj := table{cols: []col{{title: "TOKENS", max: 8, right: true}, {title: "THREADS", max: 8, right: true}, {title: "PROJECT", flex: true, min: 10}}}
	for _, p := range u.ByProject {
		name := c(tildePath(str(p.Project)))
		if p.Project == nil {
			name = cs("(unknown)", dim)
		}
		proj.rows = append(proj.rows, []cell{c(tokens(p.Tokens)), cs(fmt.Sprint(p.Threads), dim), name})
	}
	section("By project", proj)
	runs := table{cols: []col{{title: "TOKENS", max: 8, right: true}, {title: "THREADS", max: 8, right: true}, {title: "KIND", max: 14}, {title: "NAME", flex: true, min: 6}}}
	for _, r := range u.ByRun {
		runs.rows = append(runs.rows, []cell{c(tokens(r.Tokens)), cs(fmt.Sprint(r.Threads), dim), c(r.Kind), cs(str(r.Name), dim)})
	}
	section("By run kind", runs)
	return out
}

func meter(percent float64, width int) string {
	filled := int(percent/100*float64(width) + 0.5)
	filled = min(max(filled, 0), width)
	return quotaStyle(percent)(strings.Repeat("█", filled)) + ruleStyle.Render(strings.Repeat("░", width-filled))
}
