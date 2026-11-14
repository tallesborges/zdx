package ui

import (
	"fmt"
	"os"
	"strings"
	"time"

	"charm.land/lipgloss/v2"
	"github.com/charmbracelet/x/ansi"
)

// selBg is the background of the selected row. Cells keep their own
// colors on it.
var selBg = lipgloss.ANSIColor(237)

var (
	plainStyle  = lipgloss.NewStyle()
	headerStyle = lipgloss.NewStyle().Foreground(lipgloss.BrightBlack).Bold(true)
	keyStyle    = lipgloss.NewStyle().Bold(true)
	ruleStyle   = lipgloss.NewStyle().Foreground(lipgloss.ANSIColor(239))
)

// cell is one table cell: text and its style.
type cell struct {
	text  string
	style lipgloss.Style
	// shorten, when set, gives the text for a column narrower than it.
	shorten func(width int) string
}

func c(text string) cell                        { return cell{text: text, style: plainStyle} }
func cs(text string, style lipgloss.Style) cell { return cell{text: text, style: style} }

// col is one table column. A column with max > 0 sizes to its widest cell
// (header included), up to max. A flex column takes what is left, at least
// min. minWidth hides the column on tables narrower than it.
type col struct {
	title    string
	max      int
	min      int
	flex     bool
	right    bool
	minWidth int
	// cutLeft keeps the end of a cut value (model specs, thread ids end in
	// their most specific part).
	cutLeft bool
}

// table lays out rows of cells to a width: aligned, cut with "…", and with
// a selection marker and background that keep each cell's color.
type table struct {
	cols   []col
	rows   [][]cell
	widths []int // -1: hidden
}

const colGap = 2

func (t *table) layout(width int) {
	t.widths = make([]int, len(t.cols))
	used := 2 // selection marker
	visible := 0
	for i, cl := range t.cols {
		if cl.minWidth > 0 && width < cl.minWidth {
			t.widths[i] = -1
			continue
		}
		visible++
		if cl.flex {
			continue
		}
		w := ansi.StringWidth(cl.title)
		for _, r := range t.rows {
			if i < len(r) {
				w = max(w, ansi.StringWidth(r[i].text))
			}
		}
		t.widths[i] = min(w, cl.max)
		used += t.widths[i]
	}
	used += max(visible-1, 0) * colGap
	left := width - used
	var flex []int
	for i, cl := range t.cols {
		if cl.flex && t.widths[i] != -1 {
			flex = append(flex, i)
		}
	}
	for n, i := range flex {
		share := left / (len(flex) - n)
		w := ansi.StringWidth(t.cols[i].title)
		for _, r := range t.rows {
			if i < len(r) {
				w = max(w, ansi.StringWidth(r[i].text))
			}
		}
		w = max(min(w, share), t.cols[i].min)
		t.widths[i] = w
		left -= w
	}
}

func (t *table) line(cells []cell, selected bool, header bool) string {
	var b strings.Builder
	bg := func(s lipgloss.Style) lipgloss.Style {
		if selected {
			return s.Background(selBg)
		}
		return s
	}
	if selected {
		b.WriteString(bg(accent).Render("▌ "))
	} else {
		b.WriteString("  ")
	}
	first := true
	for i, cl := range t.cols {
		w := t.widths[i]
		if w < 0 {
			continue
		}
		if !first {
			b.WriteString(bg(plainStyle).Render(strings.Repeat(" ", colGap)))
		}
		first = false
		var cel cell
		if i < len(cells) {
			cel = cells[i]
		}
		style := cel.style
		if header {
			style = headerStyle
		}
		raw := cel.text
		if cel.shorten != nil && !header && ansi.StringWidth(raw) > w {
			raw = cel.shorten(w)
		}
		if selected {
			// Inner styles end with a full reset, which would drop the
			// selection background; the row keeps the cell's own style.
			raw = ansi.Strip(raw)
		}
		text := ansi.Truncate(raw, w, "…")
		if cl.cutLeft && !header && ansi.StringWidth(cel.text) > w {
			text = ansi.TruncateLeft(raw, ansi.StringWidth(raw)-w+1, "…")
		}
		gap := strings.Repeat(" ", max(w-ansi.StringWidth(text), 0))
		if cl.right {
			text = gap + text
		} else {
			text += gap
		}
		b.WriteString(bg(style).Render(text))
	}
	return b.String()
}

// header is the column title row.
func (t *table) header() string {
	cells := make([]cell, len(t.cols))
	for i, cl := range t.cols {
		cells[i] = c(cl.title)
	}
	return t.line(cells, false, true)
}

// row renders row i; a selected row is padded to width on the selection
// background.
func (t *table) row(i int, selected bool, width int) string {
	s := t.line(t.rows[i], selected, false)
	if selected {
		if w := ansi.StringWidth(s); w < width {
			s += lipgloss.NewStyle().Background(selBg).Render(strings.Repeat(" ", width-w))
		}
	}
	return s
}

// render lays the table out in at most h rows (header included), keeping
// the selection visible through l.
func (t *table) render(width, h int, l *listState) []string {
	t.layout(width)
	out := []string{t.header()}
	if l == nil {
		for i := 0; i < min(len(t.rows), max(h-1, 1)); i++ {
			out = append(out, t.row(i, false, width))
		}
		return out
	}
	start, end := l.window(len(t.rows), max(h-1, 1))
	for i := start; i < end; i++ {
		out = append(out, t.row(i, i == l.selected, width))
	}
	return out
}

// sectionTitle is a tab's first line: a bold title and dim details.
func sectionTitle(title, meta string) string {
	s := " " + bold.Render(title)
	if meta != "" {
		s += "  " + dim.Render(meta)
	}
	return s
}

// rule is a dim divider with an optional label.
func rule(label string, width int) string {
	if label == "" {
		return ruleStyle.Render(strings.Repeat("─", max(width, 0)))
	}
	head := "── " + label + " "
	return ruleStyle.Render("── ") + dim.Render(label+" ") + ruleStyle.Render(strings.Repeat("─", max(width-ansi.StringWidth(head), 0)))
}

// kv is an aligned detail line.
func kv(label, value string) string {
	return "  " + dim.Render(pad(label, 11)) + value
}

// stack fits a list and a detail pane into h rows: the list gets what it
// needs up to most of the space, the detail gets the rest after a rule.
func stack(h, listRows, detailRows int) (list, detail int) {
	if detailRows == 0 {
		return h, 0
	}
	list = min(listRows, max(h-detailRows-1, h*3/5))
	list = max(list, min(listRows, 3))
	detail = max(h-list-1, 0)
	return list, detail
}

// emptyState is the body of a tab with nothing to show.
func emptyState(text string) []string {
	return []string{"", "  " + dim.Render(text)}
}

// span is a duration in at most two units: 45s, 12m 3s, 3h 12m, 2d 4h.
func span(d time.Duration) string {
	if d < 0 {
		d = -d
	}
	s := int64(d / time.Second)
	switch {
	case s < 60:
		return fmt.Sprintf("%ds", s)
	case s < 3600:
		if s%60 == 0 || s >= 600 {
			return fmt.Sprintf("%dm", s/60)
		}
		return fmt.Sprintf("%dm %ds", s/60, s%60)
	case s < 86400:
		if (s/60)%60 == 0 {
			return fmt.Sprintf("%dh", s/3600)
		}
		return fmt.Sprintf("%dh %dm", s/3600, (s/60)%60)
	}
	if (s/3600)%24 == 0 {
		return fmt.Sprintf("%dd", s/86400)
	}
	return fmt.Sprintf("%dd %dh", s/86400, (s/3600)%24)
}

// relative is "in 3h 12m" or "5m ago" from now.
func relative(t, now time.Time) string {
	d := t.Sub(now)
	switch {
	case d > -time.Second && d < time.Second:
		return "now"
	case d > 0:
		return "in " + span(d)
	}
	return span(d) + " ago"
}

// when shows an RFC 3339 instant as local time plus relative time; text
// that does not parse is shown as given.
func when(s string, now time.Time) string {
	t, err := time.Parse(time.RFC3339, s)
	if err != nil {
		return s
	}
	local := t.Local()
	layout := "Mon 2 Jan 15:04"
	if y, d := local.YearDay(), now.Local().YearDay(); local.Year() == now.Local().Year() && y == d {
		layout = "15:04"
	}
	return local.Format(layout) + dim.Render(" ("+relative(t, now)+")")
}

// relTime is only the relative part of when.
func relTime(s string, now time.Time) string {
	t, err := time.Parse(time.RFC3339, s)
	if err != nil {
		return s
	}
	return relative(t, now)
}

// tildePath shortens the user's home to ~.
func tildePath(p string) string {
	home, err := os.UserHomeDir()
	if err != nil || home == "" || home == "/" {
		return p
	}
	if p == home {
		return "~"
	}
	if strings.HasPrefix(p, home+"/") {
		return "~" + p[len(home):]
	}
	return p
}

var spinnerFrames = []string{"⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"}

func spinner(now time.Time) string {
	return spinnerFrames[(now.UnixMilli()/int64(tickEvery/time.Millisecond))%int64(len(spinnerFrames))]
}

// dot is a status bullet.
func dot(style lipgloss.Style) string { return style.Render("●") }

// listWithDetail appends a table and, under a rule, the selected row's
// detail, within h rows in total (out included).
func listWithDetail(out []string, width, h int, tb *table, l *listState, detail []string, label string) []string {
	listH, detailH := stack(h-len(out), len(tb.rows)+1, len(detail))
	out = append(out, tb.render(width, listH, l)...)
	if detailH > 0 {
		out = append(out, "", rule(label, width))
		out = append(out, detail[:min(len(detail), max(detailH-1, 0))]...)
	}
	return out
}

// compactAge is the leading unit of an engine duration such as "1h 23m"
// or "13m 25s" (service::format_uptime, agent_activity::uptime_since):
// "1h", "13m". Lists show it so the column never cuts a duration in the
// middle; details show the full value.
func compactAge(s string) string {
	if head, _, found := strings.Cut(strings.TrimSpace(s), " "); found {
		return head
	}
	return s
}

// relShort is relTime in one unit ("in 17h", "6h ago"), for list columns.
func relShort(s string, now time.Time) string {
	t, err := time.Parse(time.RFC3339, s)
	if err != nil {
		return s
	}
	d := t.Sub(now)
	switch {
	case d > -time.Second && d < time.Second:
		return "now"
	case d > 0:
		return "in " + compactAge(span(d))
	}
	return compactAge(span(d)) + " ago"
}
