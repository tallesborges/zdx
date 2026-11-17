package transcript

import "charm.land/lipgloss/v2"

var (
	scrollThumb = lipgloss.NewStyle().Bold(true)
	scrollTrack = Dim
)

// Scrollbar returns one cell per visible row for a viewport of rows
// showing offset of total lines, or nil when everything fits. The
// thumb (█) marks the visible span and the track (│) the rest, so a
// long transcript shows where the reader is without the footer note.
// Callers fit each line to width-1 and append the cell.
func Scrollbar(total, offset, rows int) []string {
	if rows <= 0 || total <= rows {
		return nil
	}
	offset = min(max(offset, 0), total-rows)
	thumb := max(rows*rows/total, 1)
	thumb = min(thumb, rows)
	start := 0
	if maxOff := total - rows; maxOff > 0 {
		start = offset * (rows - thumb) / maxOff
	}
	cells := make([]string, rows)
	for i := range cells {
		cells[i] = scrollTrack.Render("│")
	}
	for i := start; i < start+thumb && i < rows; i++ {
		cells[i] = scrollThumb.Render("█")
	}
	return cells
}
