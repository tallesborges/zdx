package ui

import (
	"strings"
	"time"

	tea "charm.land/bubbletea/v2"
	"github.com/charmbracelet/x/ansi"

	"github.com/tallesborges/zdx/apps/monitor/internal/data"
	"github.com/tallesborges/zdx/apps/shared/transcript"
)

// transcriptState is the full-screen transcript of one thread, polled with
// the `thread_view` cursor and merged as the Mini App merges it.
type transcriptState struct {
	threadID string
	label    string
	thread   *data.Thread

	width     int
	lines     []string
	tools     []transcript.ToolRef
	selTool   int
	thinking  bool // every thinking block expanded
	scroll    scrollState
	follow    bool
	pane      *toolPane
	viewRows  int
	paneLines []string
	traj      *trajectoryState
	md        transcript.Cache

	// mouse drag selection over the transcript body.
	sel      *transcript.Range
	selDrag  bool
	selLines []string

	// mouse hover over the transcript body (a data line index, or -1).
	hoverLine int
}

type toolPane struct {
	id     string
	scroll scrollState
}

func (m *Model) openTranscript(id, label string) tea.Cmd {
	m.transcript = &transcriptState{threadID: id, label: label, selTool: -1, follow: true, width: m.contentWidth(), hoverLine: -1}
	m.invalidate(keyTranscript)
	st := &m.fetch[keyTranscript]
	st.fetched, st.err, st.origin = time.Time{}, nil, ""
	return m.refresh(m.now())
}

func (m *Model) closeTranscript() {
	m.transcript = nil
	for _, k := range []fetchKey{keyTranscript, keyTrajectory} {
		m.invalidate(k)
		m.fetch[k].force = false
	}
}

// apply takes one poll answer. It reports false when a delta cannot be
// merged: the history was reset (the cursor went back) or the alias now
// points at another thread. The caller then asks for a full load.
func (t *transcriptState) apply(r transcriptResult, width int) bool {
	old := t.thread
	if r.after == nil || old == nil {
		t.thread = &r.thread
		t.rebuild(width)
		return true
	}
	if r.thread.Partial && (r.thread.Cursor < *r.after || r.thread.SourceID != old.SourceID) {
		t.thread, t.lines, t.tools, t.selTool, t.pane = nil, nil, nil, -1, nil
		if t.traj != nil {
			t.traj = &trajectoryState{}
		}
		return false
	}
	merged := transcript.Merge(old, r.thread)
	t.thread = &merged
	if r.thread.Partial && len(r.thread.Activity) == 0 && !transcript.HasRunning(old) {
		// Nothing the lines show changed.
		return true
	}
	t.rebuild(width)
	return true
}

func (t *transcriptState) rebuild(width int) {
	t.width = width
	t.lines, t.tools = transcript.Render(t.thread, width, transcript.Options{Thinking: t.thinking}, &t.md)
	t.md.Done()
	if t.selTool >= len(t.tools) {
		t.selTool = len(t.tools) - 1
	}
	if t.follow {
		t.scroll.offset = len(t.lines)
	}
	t.scroll.clamp(len(t.lines), max(t.viewRows, 1))
	if t.pane != nil {
		t.paneLines = t.paneContent(t.pane.id, width)
	}
}

func (m *Model) transcriptKey(key string) tea.Cmd {
	t := m.transcript
	rows := m.transcriptRows()
	if t.traj != nil {
		switch key {
		case "esc", "q", "i":
			t.traj = nil
			// Keep inflight and cancel: the cancelled fetch still owns the
			// key until its result comes back.
			m.invalidate(keyTrajectory)
			st := &m.fetch[keyTrajectory]
			st.force, st.fetched, st.err, st.origin = false, time.Time{}, nil, ""
		case "r":
			m.fetch[keyTrajectory].force = true
			return m.refresh(m.now())
		default:
			t.traj.scroll.key(key, len(t.traj.lines), rows)
		}
		return nil
	}
	if t.pane != nil {
		switch key {
		case "esc", "q", "enter":
			t.pane = nil
		default:
			t.pane.scroll.key(key, len(t.paneLines), rows)
		}
		return nil
	}
	switch key {
	case "esc", "q":
		m.closeTranscript()
		return nil
	case "i":
		t.traj = &trajectoryState{}
		m.invalidate(keyTrajectory)
		st := &m.fetch[keyTrajectory]
		st.fetched, st.err, st.origin = time.Time{}, nil, ""
		return m.refresh(m.now())
	case "t":
		t.thinking = !t.thinking
		t.rebuild(m.contentWidth())
		return nil
	case "tab", "n":
		t.selectTool(1, rows)
		return nil
	case "shift+tab", "p":
		t.selectTool(-1, rows)
		return nil
	case "enter":
		if t.selTool >= 0 && t.selTool < len(t.tools) {
			id := t.tools[t.selTool].ID
			t.pane = &toolPane{id: id}
			t.paneLines = t.paneContent(id, m.contentWidth())
		}
		return nil
	case "r":
		m.fetch[keyTranscript].force = true
		return m.refresh(m.now())
	}
	t.scroll.key(key, len(t.lines), rows)
	t.follow = t.scroll.offset >= max(len(t.lines)-rows, 0)
	return nil
}

func (t *transcriptState) selectTool(dir, rows int) {
	if len(t.tools) == 0 {
		return
	}
	t.selTool += dir
	if t.selTool < 0 {
		t.selTool = len(t.tools) - 1
	}
	if t.selTool >= len(t.tools) {
		t.selTool = 0
	}
	line := t.tools[t.selTool].Start
	if line < t.scroll.offset || line >= t.scroll.offset+rows {
		t.scroll.offset = max(line-rows/2, 0)
	}
	t.scroll.clamp(len(t.lines), rows)
	t.follow = false
}

// transcriptBody returns the lines a transcript overlay shows and
// the scroll offset into them.
func (m *Model) transcriptBody() ([]string, int) {
	t := m.transcript
	if t == nil {
		return nil, 0
	}
	rows := m.transcriptRows()
	var body []string
	var offset int
	switch {
	case t.traj != nil:
		body = m.renderTrajectory(rows)
	case t.pane != nil:
		body = t.paneLines
		t.pane.scroll.clamp(len(body), rows)
		offset = t.pane.scroll.offset
	default:
		body = t.lines
		t.scroll.clamp(len(body), rows)
		offset = t.scroll.offset
	}
	return body, offset
}

// transcriptRows is the body height under the two header lines and the
// rule, above the footer.
func (m *Model) transcriptRows() int { return max(m.height-3-len(m.footerLines()), 3) }

func (m *Model) renderTranscript() string {
	t := m.transcript
	w := m.contentWidth()
	rows := m.transcriptRows()
	t.viewRows = rows
	name := t.label
	if th := t.thread; th != nil {
		name = th.Title
	}
	crumb := "Transcript"
	switch {
	case t.traj != nil:
		crumb = "Timing"
	case t.pane != nil:
		crumb = "Tool detail"
	}
	head := " " + dim.Render("‹ "+crumb+"  ") + title.Render(name)
	lines := []string{fit(head, w), fit(m.transcriptInfo(), w), rule("", w)}
	var body []string
	var offset, barTotal, barOffset int
	switch {
	case t.traj != nil:
		body = m.renderTrajectory(rows)
		// renderTrajectory already windows the lines, so the index
		// offset stays 0; the scrollbar needs the full count and
		// offset behind that window.
		barTotal, barOffset = len(body), 0
		if t.traj.report != nil {
			barTotal = len(t.traj.lines)
			barOffset = min(t.traj.scroll.offset, max(barTotal-rows, 0))
		}
	case t.pane != nil:
		body = t.paneLines
		t.pane.scroll.clamp(len(body), rows)
		offset = t.pane.scroll.offset
		barTotal, barOffset = len(body), offset
	default:
		body = t.lines
		t.scroll.clamp(len(body), rows)
		offset = t.scroll.offset
		barTotal, barOffset = len(body), offset
	}
	bar := transcript.Scrollbar(barTotal, barOffset, rows)
	sel := -1
	if t.traj == nil && t.pane == nil && t.selTool >= 0 && t.selTool < len(t.tools) {
		sel = t.tools[t.selTool].Start
	}
	for i := 0; i < rows; i++ {
		j := offset + i
		var line string
		switch {
		case j < len(body) && j == sel:
			line = selectedLine(" "+ansi.Strip(body[j]), w+1)
		case j < len(body):
			line = fit(" "+body[j], w)
		case j == 0 && t.thread == nil:
			line = fit(m.pending(keyTranscript, "the transcript")[1], w)
		}
		if bar != nil {
			// The scrollbar overlays the last column: cut it
			// cleanly (no ellipsis) instead of re-wrapping, and
			// pad short lines so the cell sits at the edge.
			line = ansi.Truncate(line, w-1, "")
			if wd := ansi.StringWidth(line); wd < w-1 {
				line += strings.Repeat(" ", w-1-wd)
			}
			line += bar[i]
		}
		lines = append(lines, line)
		if t.sel != nil {
			// Highlight the drag in body coordinates (row i is
			// body row i, not j, so the highlight moves with the
			// viewport, not the data).
			lines[len(lines)-1] = transcript.Highlight(lines[len(lines)-1], *t.sel, i)
		}
		if t.hoverLine >= 0 && j == t.hoverLine && t.pane == nil && t.traj == nil {
			lines[len(lines)-1] = transcript.Hover(lines[len(lines)-1], i, t.tools, j, t.hoverLine)
		}
		if j == sel && t.pane == nil && t.traj == nil {
			lines[len(lines)-1] = transcript.Selected(lines[len(lines)-1], i, t.tools, j, sel, w)
		}
	}
	for _, l := range m.footerLines() {
		lines = append(lines, fit(l, w))
	}
	return strings.Join(lines, "\n")
}

func (m *Model) transcriptInfo() string {
	t := m.transcript
	th := t.thread
	if th == nil {
		return " " + dim.Render(t.threadID)
	}
	var parts []string
	if th.Run != nil {
		parts = append(parts, runStyle(th.Run.State).style.Render(spinner(m.now())+" "+th.Run.State)+" "+th.Run.Label)
	} else {
		parts = append(parts, dim.Render("not running"))
	}
	if th.SourceID != "" && th.SourceID != t.threadID {
		parts = append(parts, warn.Render("alias of "+th.SourceID))
	}
	parts = append(parts, dim.Render(count(th.TotalMessages, "message", "messages")+" · "+count(th.TotalEvents, "event", "events")))
	if th.ParentThreadID != nil {
		parts = append(parts, dim.Render("worker of "+orDash(str(th.ParentTitle))))
	}
	if th.WorkerCount > 0 {
		parts = append(parts, dim.Render(count(th.WorkerCount, "worker", "workers")))
	}
	if t.follow && t.traj == nil && t.pane == nil {
		parts = append(parts, ok.Render("following"))
	}
	parts = append(parts, dim.Render(t.threadID))
	return " " + strings.Join(parts, dim.Render(" · "))
}

// paneContent renders one call's full detail (transcript.Detail).
func (t *transcriptState) paneContent(id string, width int) []string {
	return transcript.Detail(t.thread, id, width)
}
