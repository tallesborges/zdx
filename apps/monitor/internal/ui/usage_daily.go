package ui

import (
	"fmt"
	"math"
	"sort"
	"strings"
	"time"

	"charm.land/lipgloss/v2"
	"github.com/charmbracelet/x/ansi"

	"github.com/tallesborges/zdx/apps/monitor/internal/data"
	"github.com/tallesborges/zdx/apps/shared/quota"
)

// usageMetric is what the daily chart measures (`m` cycles it).
type usageMetric int

const (
	metricTokens usageMetric = iota
	metricUncached
	metricRequests
	metricCount
)

func (u usageMetric) name() string {
	switch u {
	case metricUncached:
		return "tokens without cache reads"
	case metricRequests:
		return "requests"
	}
	return "tokens"
}

func (u usageMetric) of(c data.UsageCounts) uint64 {
	switch u {
	case metricUncached:
		return c.UncachedTokens
	case metricRequests:
		return c.Requests
	}
	return c.Tokens
}

// maxDailyBars is the longest window drawn as one bar per day; longer
// windows are one compact line.
const maxDailyBars = 31

// maxChartProviders is how many providers get their own color; the rest are
// one "other" part.
const maxChartProviders = 5

var (
	providerColors = map[string]lipgloss.Style{
		"claude-cli":         lipgloss.NewStyle().Foreground(lipgloss.ANSIColor(173)),
		"anthropic":          lipgloss.NewStyle().Foreground(lipgloss.ANSIColor(173)),
		"openai-codex":       lipgloss.NewStyle().Foreground(lipgloss.ANSIColor(79)),
		"openai":             lipgloss.NewStyle().Foreground(lipgloss.ANSIColor(79)),
		"github-copilot":     lipgloss.NewStyle().Foreground(lipgloss.ANSIColor(75)),
		"google-antigravity": lipgloss.NewStyle().Foreground(lipgloss.ANSIColor(177)),
		"gemini":             lipgloss.NewStyle().Foreground(lipgloss.ANSIColor(177)),
		"grok-build":         lipgloss.NewStyle().Foreground(lipgloss.ANSIColor(251)),
		"xai":                lipgloss.NewStyle().Foreground(lipgloss.ANSIColor(251)),
		"muse-code":          lipgloss.NewStyle().Foreground(lipgloss.ANSIColor(186)),
	}
	spareColors = []lipgloss.Style{
		lipgloss.NewStyle().Foreground(lipgloss.ANSIColor(143)),
		lipgloss.NewStyle().Foreground(lipgloss.ANSIColor(110)),
		lipgloss.NewStyle().Foreground(lipgloss.ANSIColor(210)),
		lipgloss.NewStyle().Foreground(lipgloss.ANSIColor(109)),
		lipgloss.NewStyle().Foreground(lipgloss.ANSIColor(180)),
	}
	otherColor = lipgloss.NewStyle().Foreground(lipgloss.ANSIColor(244))
	barColor   = lipgloss.NewStyle().Foreground(lipgloss.Cyan)
)

// chartGroup is one colored part of the bars: a provider, or the rest.
type chartGroup struct {
	name      string
	style     lipgloss.Style
	providers map[string]bool // nil for "other"
	total     uint64
}

// chartGroups are the providers of the window by total, largest first; past
// maxChartProviders the rest are one "other" group.
func chartGroups(days []data.DailyUsage, metric usageMetric) []chartGroup {
	totals := map[string]uint64{}
	for _, d := range days {
		for _, p := range d.ByProvider {
			totals[p.Provider] += metric.of(p.UsageCounts)
		}
	}
	ids := make([]string, 0, len(totals))
	for id, v := range totals {
		if v > 0 {
			ids = append(ids, id)
		}
	}
	sort.Slice(ids, func(i, j int) bool {
		if totals[ids[i]] != totals[ids[j]] {
			return totals[ids[i]] > totals[ids[j]]
		}
		return ids[i] < ids[j]
	})
	var groups []chartGroup
	spare := 0
	for i, id := range ids {
		if i == maxChartProviders && len(ids) > maxChartProviders+1 {
			other := chartGroup{name: "other", style: otherColor}
			for _, rest := range ids[i:] {
				other.total += totals[rest]
			}
			return append(groups, other)
		}
		style, known := providerColors[id]
		if !known {
			style = spareColors[spare%len(spareColors)]
			spare++
		}
		groups = append(groups, chartGroup{name: quota.ProviderDisplay(id), style: style, providers: map[string]bool{id: true}, total: totals[id]})
	}
	return groups
}

// dayParts is a day's value per group, in group order.
func dayParts(d data.DailyUsage, groups []chartGroup, metric usageMetric) []uint64 {
	parts := make([]uint64, len(groups))
	for _, p := range d.ByProvider {
		v := metric.of(p.UsageCounts)
		for i, g := range groups {
			if g.providers == nil || g.providers[p.Provider] {
				parts[i] += v
				break
			}
		}
	}
	return parts
}

// splitCells shares n cells between parts by size (largest remainder), with
// at least one cell for every part above zero while n allows it.
func splitCells(parts []uint64, n int) []int {
	cells := make([]int, len(parts))
	var total uint64
	nonzero := 0
	for _, p := range parts {
		total += p
		if p > 0 {
			nonzero++
		}
	}
	if total == 0 || n <= 0 {
		return cells
	}
	type rem struct {
		i    int
		frac float64
	}
	var rems []rem
	used := 0
	for i, p := range parts {
		exact := float64(p) / float64(total) * float64(n)
		cells[i] = int(exact)
		if p > 0 && cells[i] == 0 && nonzero <= n {
			cells[i] = 1
		}
		used += cells[i]
		rems = append(rems, rem{i, exact - math.Floor(exact)})
	}
	sort.SliceStable(rems, func(a, b int) bool { return rems[a].frac > rems[b].frac })
	for k := 0; used < n && k < len(rems)*n; k++ {
		r := rems[k%len(rems)]
		if parts[r.i] > 0 {
			cells[r.i]++
			used++
		}
	}
	for used > n {
		big := 0
		for i := range cells {
			if cells[i] > cells[big] {
				big = i
			}
		}
		cells[big]--
		used--
	}
	return cells
}

var eighths = []string{"", "▏", "▎", "▍", "▌", "▋", "▊", "▉"}

// dailyLines is the daily chart of the usage window.
func (m *Model) dailyLines(u *data.Usage, width int) []string {
	t := &m.usage
	if len(u.Daily) == 0 {
		return nil
	}
	metric := t.metric
	var sum, peak uint64
	for _, d := range u.Daily {
		v := metric.of(d.UsageCounts)
		sum += v
		peak = max(peak, v)
	}
	head := "  " + bold.Render("Daily "+metric.name())
	if u.Previous != nil {
		if prev := metric.of(*u.Previous); prev > 0 {
			change := (float64(sum)/float64(prev) - 1) * 100
			arrow := "▲"
			if change < 0 {
				arrow = "▼"
			}
			before := fmt.Sprintf("the %d days before", u.Days)
			if u.Days == 1 {
				before = "the day before"
			}
			head += "  " + title.Render(fmt.Sprintf("%s %.0f%%", arrow, math.Abs(change))) + dim.Render(" vs "+tokens(prev)+" "+before)
		}
	}
	out := []string{fit(head, width)}
	if len(u.Daily) > maxDailyBars {
		return append(out, compactDaily(u.Daily, metric, width))
	}
	var groups []chartGroup
	if t.split {
		groups = chartGroups(u.Daily, metric)
		if len(groups) > 0 {
			out = append(out, legendLines(groups, width)...)
		}
	}
	const labW, valW = 6, 6
	secW := 10
	if width < 66 {
		secW = 0
	}
	barW := min(max(width-(2+labW+1+1)-(1+1+valW)-secW-2, 8), 46)
	avg := float64(sum) / float64(len(u.Daily))
	avgCell := -1
	if peak > 0 {
		avgCell = min(int(avg/float64(peak)*float64(barW)), barW-1)
	}
	today := int32(m.now().UTC().Unix() / 86400)
	peakSeen := false
	for _, d := range u.Daily {
		v := metric.of(d.UsageCounts)
		day := time.Unix(int64(d.Day)*86400, 0).UTC()
		label := dim.Render(pad(day.Format("Mon 2"), labW))
		if d.Day == today {
			label = bold.Render(pad(day.Format("Mon 2"), labW))
		}
		var bar strings.Builder
		used := 0
		if v > 0 {
			if len(groups) > 0 {
				n := max(int(math.Round(float64(v)/float64(peak)*float64(barW))), 1)
				for i, c := range splitCells(dayParts(d, groups, metric), n) {
					bar.WriteString(groups[i].style.Render(strings.Repeat("█", c)))
					used += c
				}
			} else {
				e := int(math.Round(float64(v) / float64(peak) * float64(barW*8)))
				e = max(e, 1)
				full, part := e/8, e%8
				bar.WriteString(barColor.Render(strings.Repeat("█", full) + eighths[part]))
				used = full
				if part > 0 {
					used++
				}
			}
		}
		for j := used; j < barW; j++ {
			if j == avgCell {
				bar.WriteString(dim.Render("┊"))
			} else {
				bar.WriteString(ruleStyle.Render("░"))
			}
		}
		value := dim.Render(fmt.Sprintf("%*s", valW, "·"))
		if v > 0 {
			value = fmt.Sprintf("%*s", valW, tokens(v))
		}
		line := "  " + label + " " + ruleStyle.Render("▕") + bar.String() + ruleStyle.Render("▏") + " " + value
		if secW > 0 {
			sec := ""
			if v > 0 {
				if metric == metricRequests {
					sec = tokens(d.Tokens) + " tok"
				} else {
					sec = tokens(d.Requests) + " req"
				}
			}
			line += dim.Render(fmt.Sprintf("%*s", secW, sec))
		}
		if v > 0 && v == peak && !peakSeen {
			peakSeen = true
			line += " " + title.Render("◆")
		}
		out = append(out, fit(line, width))
	}
	if avgCell >= 0 {
		pos := 2 + labW + 1 + 1 + avgCell
		label := "▲ avg " + tokens(uint64(math.Round(avg))) + "/day"
		if pos+ansi.StringWidth(label) > width {
			label = "avg " + tokens(uint64(math.Round(avg))) + "/day ▲"
			pos = max(pos-ansi.StringWidth(label)+1, 0)
		}
		out = append(out, fit(strings.Repeat(" ", pos)+dim.Render(label), width))
	}
	return out
}

// legendLines are the group colors with their window totals, wrapped.
func legendLines(groups []chartGroup, width int) []string {
	var out []string
	line, w := "  ", 2
	for i, g := range groups {
		item := g.style.Render("■") + " " + g.name + " " + dim.Render(tokens(g.total))
		iw := ansi.StringWidth(item)
		if i > 0 && w+2+iw > width {
			out = append(out, line)
			line, w = "  ", 2
		} else if i > 0 {
			line += "  "
			w += 2
		}
		line += item
		w += iw
	}
	return append(out, fit(line, width))
}

// compactDaily is a long window on one line: each mark is one or more days.
func compactDaily(days []data.DailyUsage, metric usageMetric, width int) string {
	first := time.Unix(int64(days[0].Day)*86400, 0).UTC().Format("2 Jan")
	last := time.Unix(int64(days[len(days)-1].Day)*86400, 0).UTC().Format("2 Jan")
	room := max(width-2-2-len(first)-3-len(last), 8)
	per := (len(days) + room - 1) / room
	var buckets []uint64
	for i := 0; i < len(days); i += per {
		var v uint64
		for _, d := range days[i:min(i+per, len(days))] {
			v += metric.of(d.UsageCounts)
		}
		buckets = append(buckets, v)
	}
	var peak uint64
	for _, v := range buckets {
		peak = max(peak, v)
	}
	var b strings.Builder
	for _, v := range buckets {
		i := 0
		if peak > 0 {
			i = int((v*uint64(len(bars)-1) + peak - 1) / peak)
		}
		b.WriteRune(bars[i])
	}
	return fit("  "+accent.Render(b.String())+dim.Render("  "+first+" – "+last), width)
}
