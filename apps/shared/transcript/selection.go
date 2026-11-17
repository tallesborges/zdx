package transcript

import (
	"strings"

	"charm.land/lipgloss/v2"
	"github.com/charmbracelet/x/ansi"
)

var selectionStyle = lipgloss.NewStyle().Reverse(true)

// Pos is a mouse position in a transcript viewport: row and cell
// column, both zero-based within the viewport body (the header and
// chrome sit above it, the scrollbar cell after it).
type Pos struct {
	Row, Col int
}

// Range is a drag selection normalized so End is at or after Start.
type Range struct {
	Start, End Pos
}

// Ordered returns the range with the start before the end.
func (r Range) Ordered() Range {
	if r.Start.Row > r.End.Row || (r.Start.Row == r.End.Row && r.Start.Col > r.End.Col) {
		return Range{Start: r.End, End: r.Start}
	}
	return r
}

// Empty reports a zero-width selection (one cell or nothing).
func (r Range) Empty() bool {
	o := r.Ordered()
	return o.Start == o.End
}

// Contains reports whether the row shows any selection.
func (r Range) Contains(row int) bool {
	if r.Empty() {
		return false
	}
	o := r.Ordered()
	return row >= o.Start.Row && row <= o.End.Row
}

// Text extracts the selected plain text from the visible lines,
// which are indexed by viewport row. Styles are stripped, trailing
// spaces on each row are trimmed, and blank rows in between stay
// blank.
func (r Range) Text(lines []string) string {
	o := r.Ordered()
	var out []string
	for row := o.Start.Row; row <= o.End.Row; row++ {
		if row < 0 || row >= len(lines) {
			out = append(out, "")
			continue
		}
		plain := ansi.Strip(lines[row])
		runes := []rune(plain)
		col := 0
		from, to := 0, max(ansi.StringWidth(plain), 0)
		if row == o.Start.Row {
			from = o.Start.Col
		}
		if row == o.End.Row {
			to = o.End.Col + 1
		}
		var b strings.Builder
		for _, r2 := range runes {
			next := col + ansi.StringWidth(string(r2))
			if next > from && col < to {
				b.WriteRune(r2)
			}
			col = next
			if col >= to {
				break
			}
		}
		out = append(out, strings.TrimRight(b.String(), " "))
	}
	return strings.Join(out, "\n")
}

// Highlight returns the line with the selected span reversed. The
// span is cell columns [start, end]; the line's own styles outside
// the span are dropped (the eye follows the span), so callers apply
// it to plain or to already-styled lines alike. A row outside the
// span is returned unchanged.
func Highlight(line string, r Range, row int) string {
	if r.Empty() || !r.Contains(row) {
		return line
	}
	o := r.Ordered()
	start, end := 0, ansi.StringWidth(line)
	if row == o.Start.Row {
		start = o.Start.Col
	}
	if row == o.End.Row {
		end = o.End.Col + 1
	}
	if end <= start {
		return line
	}
	mid := selectionStyle.Render(sliceRunes(line, start, end))
	return sliceRunes(line, 0, start) + mid + sliceRunes(line, end, ansi.StringWidth(line))
}

// sliceRunes cuts a styled line to display cells [from, to), losing
// its styles. Callers strip first when they need the styles kept.
func sliceRunes(s string, from, to int) string {
	if from < 0 {
		from = 0
	}
	if to < from {
		to = from
	}
	plain := ansi.Strip(s)
	var b strings.Builder
	col := 0
	for _, r := range []rune(plain) {
		w := ansi.StringWidth(string(r))
		if w == 0 {
			continue
		}
		next := col + w
		if next > from && col < to {
			b.WriteRune(r)
		}
		col = next
		if col >= to {
			break
		}
	}
	return b.String()
}

// ToolAt returns the tool call on a data line, or the zero ToolRef
// and false. Lines are data indices (Render output), and Start/End
// are the call's span, so any line inside it counts.
func ToolAt(tools []ToolRef, line int) (ToolRef, bool) {
	for _, t := range tools {
		if line >= t.Start && line < t.End {
			return t, true
		}
	}
	return ToolRef{}, false
}

// HoverStyle marks the tool row under the pointer: a strong block
// bar on the left edge plus a bold line, so one row reads as a
// pointer and a tool group never lights up as a block.
var HoverStyle = lipgloss.NewStyle().Bold(true)

// Hover returns the line with only the hovered data line marked.
// Every other line is unchanged, even when it is also a tool row, so
// a tool group never lights up as a block.
func Hover(line string, row int, tools []ToolRef, dataLine, hoverLine int) string {
	if dataLine != hoverLine {
		return line
	}
	if _, ok := ToolAt(tools, dataLine); !ok {
		return line
	}
	plain := ansi.Strip(line)
	if plain == "" {
		return line
	}
	return HoverStyle.Render(sliceRunes(plain, 0, ansi.StringWidth(plain)))
}

// SelectedStyle is the polished chosen row: a full-width reverse
// bar with an accent left edge. It reads as one row, not as bold
// text, and keeps the row's own text.
var SelectedStyle = lipgloss.NewStyle().Reverse(true)
var SelectedBar = lipgloss.NewStyle().Bold(true).Foreground(lipgloss.Magenta)

// Selected marks only the selected data line as the chosen row,
// padding it to full width so the reverse bar spans the viewport.
// Every other line is unchanged.
func Selected(line string, row int, tools []ToolRef, dataLine, selectedLine, width int) string {
	if dataLine != selectedLine {
		return line
	}
	if _, ok := ToolAt(tools, dataLine); !ok {
		return line
	}
	padded := line
	if wd := ansi.StringWidth(line); wd < width {
		padded = line + strings.Repeat(" ", width-wd)
	}
	// Reverse the text and keep the polished edge separate.
	bar := SelectedBar.Render("▌ ")
	text := sliceRunes(padded, 2, ansi.StringWidth(padded))
	return bar + SelectedStyle.Render(text) + SelectedStyle.Render(strings.Repeat(" ", max(width-ansi.StringWidth(text)-2, 0)))
}

// VisibleRows lists the rows a drag touches, in order.
func (r Range) VisibleRows() []int {
	o := r.Ordered()
	rows := make([]int, 0, o.End.Row-o.Start.Row+1)
	for row := o.Start.Row; row <= o.End.Row; row++ {
		rows = append(rows, row)
	}
	return rows
}
