package ui

import (
	"strings"

	tea "charm.land/bubbletea/v2"
	"github.com/charmbracelet/x/ansi"

	"github.com/tallesborges/zdx/apps/shared/transcript"
)

// Mouse drag selection over the transcript viewport. The body starts
// one row below the header, so a mouse row r maps to body row r-1.
// The composer, rule, attachments, and scrollbar cell are chrome:
// a drag starting there is ignored.

func (m *Model) bodyTop() int { return 1 }

func (m *Model) inBody(x, y int) (transcript.Pos, bool) {
	bodyRow := y - m.bodyTop()
	rows := m.bodyRows()
	if bodyRow < 0 || bodyRow >= rows {
		return transcript.Pos{}, false
	}
	// Col == x: selLines and the highlighted body lines are
	// rendered lines (leading space included), so mouse columns
	// index them directly. The scrollbar column, when shown, is
	// chrome and starts no selection.
	linesLen := 0
	if lines, _ := m.transcriptLinesAndTools(m.width - 1); len(lines) > 0 {
		linesLen = len(lines)
	} else {
		linesLen = len(m.emptyLines())
	}
	width := m.width
	if transcript.Scrollbar(linesLen, 0, rows) != nil {
		width--
	}
	if x < 0 || x >= width {
		return transcript.Pos{}, false
	}
	return transcript.Pos{Row: bodyRow, Col: x}, true
}

func (m *Model) mouseClick(mouse tea.Mouse) tea.Cmd {
	if m.overlay != nil || m.detail != nil || mouse.Button != tea.MouseLeft {
		return nil
	}
	pos, ok := m.inBody(mouse.X, mouse.Y)
	if !ok {
		m.sel, m.selDrag = nil, false
		return nil
	}
	m.sel, m.selDrag = &transcript.Range{Start: pos, End: pos}, true
	m.selLines = m.viewportPlain()
	return nil
}

func (m *Model) mouseMotion(mouse tea.Mouse) tea.Cmd {
	if m.detail != nil {
		return nil
	}
	if m.sel == nil || !m.selDrag {
		m.hoverAt(mouse)
		return nil
	}
	pos, ok := m.inBody(mouse.X, mouse.Y)
	if !ok {
		return nil
	}
	m.sel.End = pos
	return nil
}

func (m *Model) mouseRelease(mouse tea.Mouse) tea.Cmd {
	m.selDrag = false
	if m.sel == nil {
		return nil
	}
	// A press and release on the same tool row with no drag is a
	// click, not a copy: open the detail pane. A press and release
	// with no drag anywhere else copies nothing.
	press := m.sel.Start
	release := press
	if pos, ok := m.inBody(mouse.X, mouse.Y); ok {
		release = pos
	}
	m.sel.End = release
	if m.sel.Empty() {
		m.sel, m.selLines = nil, nil
		if m.detail == nil {
			// Map the release body row to a data line the
			// same way hover does (front padding aware).
			lines, tools := m.transcriptLinesAndTools(m.width - 1)
			if len(lines) == 0 {
				lines = m.emptyLines()
			}
			rows := m.bodyRows()
			maxScroll := max(len(lines)-rows, 0)
			scroll := min(m.scroll, maxScroll)
			end := len(lines) - scroll
			start := max(end-rows, 0)
			pad := rows - (end - start)
			if release.Row >= pad {
				j := start + (release.Row - pad)
				if j >= start && j < end {
					if ref, hit := transcript.ToolAt(tools, j); hit {
						m.openDetail(ref)
						return nil
					}
				}
			}
		}
		return nil
	}
	text := strings.TrimRight(m.sel.Text(m.selLines), " \n")
	m.sel = nil
	m.selLines = nil
	if text == "" {
		return nil
	}
	m.setNotice("copied "+transcript.Count(strings.Count(text, "\n")+1, "line", "lines"), true)
	return tea.SetClipboard(text)
}

func (m *Model) hoverToolLine(mouse tea.Mouse) int {
	if m.detail != nil {
		return -1
	}
	pos, ok := m.inBody(mouse.X, mouse.Y)
	if !ok {
		return -1
	}
	lines, tools := m.transcriptLinesAndTools(m.width - 1)
	if len(lines) == 0 {
		lines = m.emptyLines()
	}
	rows := m.bodyRows()
	maxScroll := max(len(lines)-rows, 0)
	scroll := min(m.scroll, maxScroll)
	end := len(lines) - scroll
	start := max(end-rows, 0)
	// The viewport front-pads a short transcript with empty rows,
	// so a body row maps to data line start+(row-pad).
	pad := rows - (end - start)
	if pos.Row < pad {
		return -1
	}
	j := start + (pos.Row - pad)
	if j < start || j >= end {
		return -1
	}
	if _, hit := transcript.ToolAt(tools, j); !hit {
		return -1
	}
	return j
}

func (m *Model) hoverAt(mouse tea.Mouse) {
	if m.detail != nil {
		m.hoverLine = -1
		return
	}
	m.hoverLine = m.hoverToolLine(mouse)
}

// viewportPlain is the current viewport body as plain rendered
// lines (leading space and scrollbar cell included), so mouse
// columns index it directly: Col == x.
func (m *Model) viewportPlain() []string {
	body := m.viewport(m.bodyRows())
	plain := make([]string, 0, len(body))
	for _, l := range body {
		plain = append(plain, ansi.Strip(l))
	}
	return plain
}
