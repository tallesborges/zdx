package ui

import (
	"fmt"
	"path/filepath"
	"strings"
	"time"

	"charm.land/lipgloss/v2"
	"github.com/charmbracelet/x/ansi"
)

var (
	title     = lipgloss.NewStyle().Bold(true).Foreground(lipgloss.Cyan)
	dim       = lipgloss.NewStyle().Foreground(lipgloss.BrightBlack)
	bold      = lipgloss.NewStyle().Bold(true)
	ok        = lipgloss.NewStyle().Foreground(lipgloss.Green)
	warn      = lipgloss.NewStyle().Foreground(lipgloss.Yellow)
	bad       = lipgloss.NewStyle().Foreground(lipgloss.Red)
	accent    = lipgloss.NewStyle().Foreground(lipgloss.Magenta)
	selected  = lipgloss.NewStyle().Reverse(true)
	activeTab = lipgloss.NewStyle().Bold(true).Reverse(true)
)

// fit cuts a styled line to width cells.
func fit(s string, width int) string {
	if width <= 0 {
		return ""
	}
	return ansi.Truncate(s, width, "…")
}

// pad cuts or pads plain text to width cells.
func pad(s string, width int) string {
	s = ansi.Truncate(s, width, "…")
	if w := ansi.StringWidth(s); w < width {
		s += strings.Repeat(" ", width-w)
	}
	return s
}

// oneLine is external text (cleaned) on one line.
func oneLine(s string) string {
	s = strings.ReplaceAll(cleanText(s), "\n", " ")
	return strings.Join(strings.Fields(s), " ")
}

// ago is a short age: 4s, 3m, 2h, 5d.
func ago(d time.Duration) string {
	switch {
	case d < 0:
		return "0s"
	case d < time.Minute:
		return fmt.Sprintf("%ds", int(d.Seconds()))
	case d < time.Hour:
		return fmt.Sprintf("%dm", int(d.Minutes()))
	case d < 48*time.Hour:
		return fmt.Sprintf("%dh", int(d.Hours()))
	}
	return fmt.Sprintf("%dd", int(d.Hours()/24))
}

func secs(p *uint64) string {
	if p == nil {
		return "?"
	}
	return ago(time.Duration(*p) * time.Second)
}

func str(p *string) string {
	if p == nil {
		return ""
	}
	return *p
}

func orDash(s string) string {
	if s == "" {
		return "—"
	}
	return s
}

// sameDir compares two paths after cleaning and resolving symlinks
// (macOS temp paths live under /private).
func sameDir(a, b string) bool {
	norm := func(p string) string {
		if abs, err := filepath.Abs(p); err == nil {
			p = abs
		}
		if real, err := filepath.EvalSymlinks(p); err == nil {
			p = real
		}
		return filepath.Clean(p)
	}
	return norm(a) == norm(b)
}

// wrapText wraps plain text to width and returns its lines.
func wrapText(s string, width int) []string {
	width = max(width, 10)
	var out []string
	for _, line := range strings.Split(strings.ReplaceAll(s, "\t", "    "), "\n") {
		w := ansi.Wordwrap(line, width, " -")
		w = ansi.Hardwrap(w, width, true)
		out = append(out, strings.Split(w, "\n")...)
	}
	return out
}

// listState is a selection and scroll offset kept stable across refreshes:
// the selection follows the selected row's id.
type listState struct {
	selected int
	offset   int
	id       string
	page     int // rows shown at the last render
}

// pageSize is the rows a page key moves.
func (l *listState) pageSize() int {
	if l.page > 0 {
		return l.page
	}
	return 10
}

// sync keeps the selection on the same id after rows changed, else clamps
// it.
func (l *listState) sync(ids []string) {
	if l.id != "" {
		for i, id := range ids {
			if id == l.id {
				l.selected = i
				return
			}
		}
	}
	l.selected = min(l.selected, max(len(ids)-1, 0))
	if l.selected < len(ids) {
		l.id = ids[l.selected]
	} else {
		l.id = ""
	}
}

// key moves the selection for a navigation key; n is the row count, h the
// visible rows.
func (l *listState) key(key string, n, h int) bool {
	if n == 0 {
		l.selected, l.offset = 0, 0
		return false
	}
	switch key {
	case "j", "down":
		l.selected++
	case "k", "up":
		l.selected--
	case "pgdown", "ctrl+d", "space":
		l.selected += max(h-1, 1)
	case "pgup", "ctrl+u":
		l.selected -= max(h-1, 1)
	case "home":
		l.selected = 0
	case "end", "G":
		l.selected = n - 1
	default:
		return false
	}
	l.selected = min(max(l.selected, 0), n-1)
	l.id = ""
	return true
}

// window clamps the offset so the selection is visible and returns the
// visible range.
func (l *listState) window(n, h int) (start, end int) {
	h = max(h, 1)
	l.page = h
	l.selected = min(max(l.selected, 0), max(n-1, 0))
	if l.selected < l.offset {
		l.offset = l.selected
	}
	if l.selected >= l.offset+h {
		l.offset = l.selected - h + 1
	}
	l.offset = min(max(l.offset, 0), max(n-h, 0))
	return l.offset, min(l.offset+h, n)
}

// remember stores the id of the selected row.
func (l *listState) remember(ids []string) {
	if l.selected >= 0 && l.selected < len(ids) {
		l.id = ids[l.selected]
	}
}

// scrollState is a plain scroll offset for a page without a selection.
type scrollState struct{ offset int }

func (s *scrollState) key(key string, n, h int) {
	switch key {
	case "j", "down":
		s.offset++
	case "k", "up":
		s.offset--
	case "pgdown", "ctrl+d", "space":
		s.offset += max(h-1, 1)
	case "pgup", "ctrl+u":
		s.offset -= max(h-1, 1)
	case "home":
		s.offset = 0
	case "end", "G":
		s.offset = n
	}
	s.clamp(n, h)
}

func (s *scrollState) clamp(n, h int) {
	s.offset = min(max(s.offset, 0), max(n-h, 0))
}
