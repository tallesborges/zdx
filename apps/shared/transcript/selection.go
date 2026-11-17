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
// blank. Columns are display cells; grapheme clusters are never
// split.
func (r Range) Text(lines []string) string {
	o := r.Ordered()
	// Clamp to the visible rows: an absurd exported range must not
	// build a giant join.
	startRow := max(o.Start.Row, 0)
	endRow := min(o.End.Row, len(lines)-1)
	if startRow > endRow || len(lines) == 0 {
		return ""
	}
	var out []string
	for row := startRow; row <= endRow; row++ {
		plain := ansi.Strip(lines[row])
		from, to := 0, ansi.StringWidth(plain)
		if row == o.Start.Row {
			from = o.Start.Col
		}
		if row == o.End.Row {
			to = o.End.Col + 1
		}
		out = append(out, strings.TrimRight(sliceRunes(plain, from, to), " "))
	}
	return strings.Join(out, "\n")
}

// Highlight returns the line with the selected span reversed. The
// span is cell columns [start, end]; the line's own styles outside
// the span are dropped (the eye follows the span), so callers apply
// it to plain or to already-styled lines alike. A row outside the
// span, or a span that does not intersect the line, is returned
// unchanged. A double-width rune straddling a span edge lands in
// exactly one side.
func Highlight(line string, r Range, row int) string {
	if r.Empty() || !r.Contains(row) {
		return line
	}
	o := r.Ordered()
	width := ansi.StringWidth(line)
	start, end := 0, width
	if row == o.Start.Row {
		start = o.Start.Col
	}
	if row == o.End.Row {
		end = o.End.Col + 1
	}
	// Clamp the span to the line; a span entirely outside changes
	// nothing (and must not strip styles by re-cutting).
	start = max(start, 0)
	end = min(end, width)
	if end <= start {
		return line
	}
	// ansi.Cut assigns a straddling cluster to one side only, so
	// the three segments never duplicate a wide rune.
	mid := selectionStyle.Render(sliceRunes(line, start, end))
	return sliceRunes(line, 0, start) + mid + sliceRunes(line, end, width)
}

// sliceRunes cuts a styled line to display cells [from, to),
// grapheme-aware via ansi.Cut. Styles are lost.
func sliceRunes(s string, from, to int) string {
	if from < 0 {
		from = 0
	}
	if to < from {
		to = from
	}
	if from == to {
		return ""
	}
	return ansi.Cut(ansi.Strip(s), from, to)
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

// HoverStyle marks the tool row under the pointer: bold text, so
// one row reads as a pointer and a tool group never lights up as a
// block.
var HoverStyle = lipgloss.NewStyle().Bold(true)

// Hover returns the line with only the hovered data line marked.
// Every other line is unchanged, even when it is also a tool row, so
// a tool group never lights up as a block.
func Hover(line string, tools []ToolRef, dataLine, hoverLine int) string {
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
// padding it to full width so the reverse bar spans that width. The
// row's own text, including its outcome mark, is kept: the reverse
// spans the text from cell 0 and the tail is padded instead of
// truncating the head.
func Selected(line string, tools []ToolRef, dataLine, selectedLine, width int) string {
	if width < 2 {
		width = 2
	}
	if dataLine != selectedLine {
		return line
	}
	if _, ok := ToolAt(tools, dataLine); !ok {
		return line
	}
	plain := ansi.Strip(line)
	// Keep the text from cell 0; drop from the tail to fit beside
	// the 2-cell bar. The outcome mark at the head survives.
	bar := SelectedBar.Render("▌ ")
	text := sliceRunes(plain, 0, max(width-2, 0))
	tail := max(width-2-ansi.StringWidth(text), 0)
	return bar + SelectedStyle.Render(text) + SelectedStyle.Render(strings.Repeat(" ", tail))
}
