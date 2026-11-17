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
	// click, not a copy: open the detail pane. A press and release
	// with no drag anywhere else copies nothing.
	if t.sel.Empty() {
		press := t.sel.Start
		release := press
		if pos, ok := m.inTranscriptBody(mouse.X, mouse.Y); ok {
			release = pos
		}
		// Normalize the release into the range so click vs drag
		// uses the actual release cell, not the last motion.
		t.sel.End = release
		t.selLines = m.transcriptPlain()
		if t.sel.Empty() && t.pane == nil && t.traj == nil {
			body, offset := m.transcriptBody()
			j := offset + release.Row
			if j >= 0 && j < len(body) {
				if ref, hit := transcript.ToolAt(t.tools, j); hit {
					t.sel, t.selLines = nil, nil
					t.pane = &toolPane{id: ref.ID}
					t.paneLines = t.paneContent(ref.ID, m.contentWidth())
					return nil
				}
			}
		}
		if t.sel.Empty() {
			t.sel, t.selLines = nil, nil
			return nil
		}
		// Release moved: treat as a drag and fall through to copy.
	} else {
		// Refresh end from the release cell so a drag that ended
		// without a final motion still copies to the release.
		if pos, ok := m.inTranscriptBody(mouse.X, mouse.Y); ok {
			t.sel.End = pos
			t.selLines = m.transcriptPlain()
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
	w := m.contentWidth()
	t := m.transcript
	if t != nil {
		body, offset := m.transcriptBody()
		total, mapped := len(body), offset
		if t.traj != nil && t.traj.report != nil {
			total = len(t.traj.lines)
			mapped = t.traj.scroll.offset
		}
		if transcript.Scrollbar(total, mapped, rows) != nil {
			w--
		}
	}
	if x < 0 || x >= w {
		return transcript.Pos{}, false
	}
	return transcript.Pos{Row: bodyRow, Col: x}, true
}

// transcriptPlain is the transcript body as plain rendered lines
// (leading space included), captured at drag start so the copy
// matches what was on screen. Col == x.
func (m *Model) transcriptPlain() []string {
	t := m.transcript
	if t == nil {
		return nil
	}
	body, offset := m.transcriptBody()
	plain := make([]string, 0, len(body))
	for _, l := range body[offset:] {
		plain = append(plain, " "+ansi.Strip(l))
	}
	return plain
}
