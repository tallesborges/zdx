package ui

import (
	"strings"

	tea "charm.land/bubbletea/v2"
	"github.com/charmbracelet/x/ansi"

	"github.com/tallesborges/zdx/apps/shared/transcript"
)

// Mouse drag selection copy on the transcript overlay. The body
// starts at frame row 3 (head, info, rule), so a mouse row r maps
// to body row r-3. Drags starting on chrome are ignored, and a drag
// that copies nothing leaves the footer hints alone.

func (m *Model) mouseClick(mouse tea.Mouse) tea.Cmd {
	t := m.transcript
	if t == nil || m.help {
		return nil
	}
	if mouse.Button != tea.MouseLeft {
		return nil
	}
	pos, ok := m.inTranscriptBody(mouse.X, mouse.Y)
	if !ok {
		t.sel, t.selDrag = nil, false
		return nil
	}
	t.sel, t.selDrag = &transcript.Range{Start: pos, End: pos}, true
	t.selLines = m.transcriptPlain()
	return nil
}

// Tool row under the pointer, as a data line, or -1.
func (m *Model) hoverToolLine(mouse tea.Mouse) int {
	t := m.transcript
	if t == nil || t.pane != nil || t.traj != nil {
		return -1
	}
	pos, ok := m.inTranscriptBody(mouse.X, mouse.Y)
	if !ok {
		return -1
	}
	body, offset := m.transcriptBody()
	if len(body) == 0 {
		return -1
	}
	j := offset + pos.Row
	if j < 0 || j >= len(body) {
		return -1
	}
	if _, hit := transcript.ToolAt(t.tools, j); !hit {
		return -1
	}
	return j
}

// hoverAt updates the hover mark for a motion event.
func (m *Model) hoverAt(mouse tea.Mouse) {
	t := m.transcript
	if t == nil {
		return
	}
	t.hoverLine = m.hoverToolLine(mouse)
}

func (m *Model) mouseMotion(mouse tea.Mouse) tea.Cmd {
	t := m.transcript
	if t == nil || t.sel == nil || !t.selDrag {
		// No drag: this motion is hover. Under AllMotion the app
		// also gets motion with no button pressed, which is exactly
		// the hover signal it needs.
		m.hoverAt(mouse)
		return nil
	}
	pos, ok := m.inTranscriptBody(mouse.X, mouse.Y)
	if !ok {
		return nil
	}
	t.sel.End = pos
	// The content under the drag is the visible window at drag
	// start; re-read it so a drag over scrolled content copies the
	// lines the user actually saw move.
	t.selLines = m.transcriptPlain()
	return nil
}

func (m *Model) mouseRelease(mouse tea.Mouse) tea.Cmd {
	t := m.transcript
	if t == nil {
		return nil
	}
	t.selDrag = false
	if t.sel == nil {
		return nil
	}
	// A press and release on the same tool row with no drag is a
	// click, not a copy: open the detail pane.
	if t.sel.Empty() && t.pane == nil && t.traj == nil {
		if pos, ok := m.inTranscriptBody(mouse.X, mouse.Y); ok {
			body, offset := m.transcriptBody()
			j := offset + pos.Row
			if j >= 0 && j < len(body) {
				if ref, hit := transcript.ToolAt(t.tools, j); hit {
					t.sel, t.selLines = nil, nil
					t.pane = &toolPane{id: ref.ID}
					t.paneLines = t.paneContent(ref.ID, m.contentWidth())
					return nil
				}
			}
		}
	}
	text := strings.TrimRight(t.sel.Text(t.selLines), " \n")
	t.sel = nil
	t.selLines = nil
	if text == "" {
		return nil
	}
	return tea.SetClipboard(text)
}

func (m *Model) inTranscriptBody(x, y int) (transcript.Pos, bool) {
	bodyRow := y - 3
	rows := m.transcriptRows()
	if bodyRow < 0 || bodyRow >= rows {
		return transcript.Pos{}, false
	}
	if x < 1 || x >= m.contentWidth() {
		return transcript.Pos{}, false
	}
	return transcript.Pos{Row: bodyRow, Col: x - 1}, true
}

// transcriptPlain is the transcript body as plain lines, captured at
// drag start so the copy matches what was on screen.
func (m *Model) transcriptPlain() []string {
	t := m.transcript
	if t == nil {
		return nil
	}
	body, offset := m.transcriptBody()
	plain := make([]string, 0, len(body))
	for _, l := range body[offset:] {
		plain = append(plain, ansi.Strip(l))
	}
	return plain
}
