package ui

import (
	tea "charm.land/bubbletea/v2"

	"github.com/tallesborges/zdx/apps/shared/transcript"
)

// Tool detail pane for the TUI: the full-screen Detail of the tool
// under the cursor. Enter or esc closes; j/k, arrows and the wheel
// scroll it. The composer stays below.

func (m *Model) openDetail(ref transcript.ToolRef) {
	full := m.fullThread()
	m.detail = &ref
	m.detailScroll = 0
	m.detailLines = transcript.Detail(full, ref.ID, m.width-1)
	m.sel = nil
	m.selLines = nil
}

// fullThread is the persisted thread plus the live items, so a
// running call has a detail before it is saved.
func (m *Model) fullThread() *transcript.Thread {
	if m.thread == nil {
		return m.liveThread()
	}
	th := *m.thread
	live := m.liveThread()
	if live != nil {
		th.Activity = append(th.Activity, live.Activity...)
	}
	return &th
}

func (m *Model) detailViewport(rows int) []string {
	lines := m.detailLines
	maxOff := max(len(lines)-rows, 0)
	m.detailScroll = min(max(m.detailScroll, 0), maxOff)
	out := make([]string, 0, rows)
	for i := m.detailScroll; i < m.detailScroll+rows && i < len(lines); i++ {
		out = append(out, transcript.Fit(" "+lines[i], m.width))
	}
	for len(out) < rows {
		out = append(out, "")
	}
	return out
}

func (m *Model) closeDetail() {
	m.detail = nil
	m.detailLines = nil
	m.detailScroll = 0
}

// detailKey handles keys while the tool detail is open.
func (m *Model) detailKey(msg tea.KeyPressMsg) tea.Cmd {
	switch msg.String() {
	case "esc", "enter", "q":
		m.closeDetail()
		return nil
	case "down", "j":
		m.detailScroll++
		return nil
	case "up", "k":
		m.detailScroll--
		return nil
	case "pgdown", "ctrl+d", "space":
		m.detailScroll += max(m.bodyRows()-1, 1)
		return nil
	case "pgup", "ctrl+u":
		m.detailScroll -= max(m.bodyRows()-1, 1)
		return nil
	case "home":
		m.detailScroll = 0
		return nil
	case "end", "G":
		m.detailScroll = 1 << 30
		return nil
	case "ctrl+c":
		return m.quit()
	}
	return nil
}
