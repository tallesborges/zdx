package ui

import (
	"bytes"
	"encoding/json"
	"fmt"
	"strings"
	"time"

	tea "charm.land/bubbletea/v2"
	"github.com/charmbracelet/x/ansi"

	"github.com/tallesborges/zdx/apps/monitor/internal/data"
)

// transcriptState is the full-screen transcript of one thread, polled with
// the `thread_view` cursor and merged as the Mini App merges it.
type transcriptState struct {
	threadID string
	label    string
	thread   *data.Thread

	width     int
	lines     []string
	tools     []toolRef
	selTool   int
	thinking  bool // every thinking block expanded
	scroll    scrollState
	follow    bool
	pane      *toolPane
	viewRows  int
	paneLines []string
	traj      *trajectoryState
	md        mdCache
}

// toolRef is one tool row: its call id and its rendered line span.
type toolRef struct {
	id         string
	start, end int
}

type toolPane struct {
	id     string
	scroll scrollState
}

func (m *Model) openTranscript(id, label string) tea.Cmd {
	m.transcript = &transcriptState{threadID: id, label: label, selTool: -1, follow: true, width: m.contentWidth()}
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

// mergeThread applies a poll: a full answer replaces, a partial one keeps
// the persisted items, drops running tools (always resent), and appends
// what is new (apps/web ThreadView.svelte poll).
func mergeThread(old *data.Thread, delta data.Thread) data.Thread {
	if old == nil || !delta.Partial {
		return delta
	}
	seen := map[uint64]bool{}
	var activity []data.Activity
	for _, a := range old.Activity {
		if a.Type == "tool_running" {
			continue
		}
		seen[a.Sequence] = true
		activity = append(activity, a)
	}
	for _, a := range delta.Activity {
		if !seen[a.Sequence] {
			activity = append(activity, a)
		}
	}
	delta.Activity = activity
	return delta
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
	merged := mergeThread(old, r.thread)
	t.thread = &merged
	if r.thread.Partial && len(r.thread.Activity) == 0 && !hasRunning(old) {
		// Nothing the lines show changed.
		return true
	}
	t.rebuild(width)
	return true
}

func hasRunning(th *data.Thread) bool {
	for _, a := range th.Activity {
		if a.Type == "tool_running" {
			return true
		}
	}
	return false
}

func (t *transcriptState) rebuild(width int) {
	t.width = width
	t.lines, t.tools = renderActivity(t.thread, width, t.thinking, &t.md)
	t.md.done()
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

func headText(h *data.ToolHead) string {
	if h == nil {
		return ""
	}
	switch h.Kind {
	case "shell":
		return "$ " + h.Command
	case "path":
		return h.Path
	case "search":
		if h.Scope != nil {
			return h.Pattern + " in " + *h.Scope
		}
		return h.Pattern
	case "prompt":
		if h.Agent != nil {
			return *h.Agent + ": " + h.Prompt
		}
		return h.Prompt
	}
	return h.Text
}

func outcomeMark(outcome string) string {
	switch outcome {
	case "ok":
		return ok.Render("✓")
	case "failed":
		return bad.Render("✗")
	case "timed_out":
		return bad.Render("⏱")
	case "canceled":
		return warn.Render("⊘")
	case "unknown":
		return dim.Render("?")
	}
	return accent.Render("…")
}

func facts(v *data.ToolView) string {
	if v == nil {
		return ""
	}
	var parts []string
	for _, f := range v.Status {
		if f.Bad {
			parts = append(parts, bad.Render(f.Text))
		} else {
			parts = append(parts, dim.Render(f.Text))
		}
	}
	if v.Hint != nil {
		parts = append(parts, dim.Render(v.Hint.Text))
	}
	return strings.Join(parts, dim.Render(" · "))
}

// toolCall gathers the items of one call: its use, its result, and its
// running marker.
type toolCall struct {
	use, result, running *data.Activity
}

func (c toolCall) view() *data.ToolView {
	switch {
	case c.result != nil && c.result.View != nil:
		return c.result.View
	case c.running != nil && c.running.View != nil:
		return c.running.View
	case c.use != nil:
		return c.use.View
	}
	return nil
}

// finishedLive is a call the run marker reports finished while its round
// is not yet in the thread file.
func (c toolCall) finishedLive() bool {
	return c.result == nil && c.running != nil && c.running.CompletedAt != nil
}

// outcome is the view's outcome; a finished live call has only an
// input view, so its outcome comes from the marker's `ok` (unknown when
// absent).
func (c toolCall) outcome() string {
	if c.finishedLive() {
		switch {
		case c.running.OK == nil:
			return "unknown"
		case *c.running.OK:
			return "ok"
		}
		return "failed"
	}
	if v := c.view(); v != nil {
		return v.Outcome
	}
	return "pending"
}

func (c toolCall) name() string {
	switch {
	case c.use != nil:
		return c.use.Name
	case c.running != nil:
		return c.running.Name
	}
	return "tool"
}

func calls(th *data.Thread) map[string]*toolCall {
	out := map[string]*toolCall{}
	get := func(id string) *toolCall {
		if out[id] == nil {
			out[id] = &toolCall{}
		}
		return out[id]
	}
	for i := range th.Activity {
		a := &th.Activity[i]
		switch a.Type {
		case "tool_use":
			get(a.ID).use = a
		case "tool_running":
			get(a.ID).running = a
		case "tool_result":
			get(a.ToolUseID).result = a
		}
	}
	return out
}

// renderActivity lays the transcript out as lines and records each tool
// row's span. Message, thinking, and interruption text is Markdown; tool
// rows and their output are not. md may be nil.
func renderActivity(th *data.Thread, width int, thinking bool, md *mdCache) ([]string, []toolRef) {
	if th == nil {
		return nil, nil
	}
	var lines []string
	var tools []toolRef
	byID := calls(th)
	shown := map[string]bool{}
	gap := func() {
		if len(lines) > 0 && lines[len(lines)-1] != "" {
			lines = append(lines, "")
		}
	}
	prose := func(a *data.Activity, theme *mdTheme) []string {
		return md.render(a.Sequence, a.Text, width-4, theme)
	}
	indent := func(ls []string) {
		for _, l := range ls {
			if l == "" {
				lines = append(lines, "")
			} else {
				lines = append(lines, "  "+l)
			}
		}
	}
	for i := range th.Activity {
		a := &th.Activity[i]
		switch a.Type {
		case "message":
			gap()
			who := a.Speaker
			if who == "" {
				who = a.Role
			}
			style := ok
			if a.Role == "user" {
				style = accent
			}
			lines = append(lines, style.Bold(true).Render("▌ "+who)+dim.Render("  "+a.Time))
			if a.Text != "" {
				indent(prose(a, mdNormal))
			}
			for _, f := range a.Artifacts {
				note := f.Kind
				if !f.Exists {
					note += ", missing"
				}
				lines = append(lines, fit("  "+accent.Render("⎘ "+f.Name)+dim.Render(" ("+note+") "+f.Path), width))
			}
		case "reasoning":
			gap()
			switch {
			case a.Redacted:
				lines = append(lines, dim.Render("∴ thinking (redacted)"))
			case thinking:
				lines = append(lines, dim.Render("∴ thinking"))
				indent(prose(a, mdMuted))
			default:
				n := len(prose(a, mdMuted))
				lines = append(lines, dim.Render(fmt.Sprintf("∴ thinking · %s (t to expand)", count(n, "line", "lines"))))
			}
		case "tool_use", "tool_running", "tool_result":
			id := a.ID
			if a.Type == "tool_result" {
				id = a.ToolUseID
			}
			if shown[id] {
				continue
			}
			shown[id] = true
			c := byID[id]
			if prev := len(lines); prev > 0 && !isToolLine(tools, prev-1) {
				gap()
			}
			v := c.view()
			head := ""
			if v != nil {
				head = headText(v.Head)
			}
			line := fmt.Sprintf("%s %s %s", outcomeMark(c.outcome()), bold.Render(c.name()), oneLine(head))
			if c.finishedLive() {
				line += dim.Render(" finished " + c.running.RunningFor + ", not saved yet")
			} else if c.running != nil && c.result == nil {
				line += accent.Render(" running " + c.running.RunningFor)
			} else if c.result == nil && c.use != nil {
				line += dim.Render(" (no result)")
			}
			if f := facts(v); f != "" {
				line += "  " + f
			}
			tools = append(tools, toolRef{id: id, start: len(lines), end: len(lines) + 1})
			lines = append(lines, line)
		case "usage":
			lines = append(lines, dim.Render(fmt.Sprintf("  · usage in %s out %s cache %s/%s %s",
				tokens(a.InputTokens), tokens(a.OutputTokens), tokens(a.CacheReadTokens), tokens(a.CacheWriteTokens), str(a.Model))))
		case "notice":
			gap()
			lines = append(lines, warn.Render("! "+a.Kind+": "+oneLine(a.Message)))
		case "interrupted":
			gap()
			lines = append(lines, bad.Render("⨯ interrupted ("+a.Role+")"))
			if a.Text != "" {
				indent(prose(a, mdMuted))
			}
		default:
			gap()
			lines = append(lines, dim.Render("["+a.Type+"]"))
		}
	}
	return lines, tools
}

func isToolLine(tools []toolRef, line int) bool {
	return len(tools) > 0 && tools[len(tools)-1].end-1 == line
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
			id := t.tools[t.selTool].id
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
	line := t.tools[t.selTool].start
	if line < t.scroll.offset || line >= t.scroll.offset+rows {
		t.scroll.offset = max(line-rows/2, 0)
	}
	t.scroll.clamp(len(t.lines), rows)
	t.follow = false
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
	sel := -1
	if t.traj == nil && t.pane == nil && t.selTool >= 0 && t.selTool < len(t.tools) {
		sel = t.tools[t.selTool].start
	}
	for i := 0; i < rows; i++ {
		j := offset + i
		switch {
		case j < len(body) && j == sel:
			lines = append(lines, selectedLine(" "+ansi.Strip(body[j]), w+1))
		case j < len(body):
			lines = append(lines, fit(" "+body[j], w))
		case j == 0 && t.thread == nil:
			lines = append(lines, fit(m.pending(keyTranscript, "the transcript")[1], w))
		default:
			lines = append(lines, "")
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

// paneContent renders one call's full detail: head, facts, notes, body, and
// input.
func (t *transcriptState) paneContent(id string, width int) []string {
	if t.thread == nil {
		return nil
	}
	c := calls(t.thread)[id]
	if c == nil {
		return []string{dim.Render(" this call is no longer in the transcript")}
	}
	v := c.view()
	var out []string
	add := func(s ...string) { out = append(out, s...) }
	add(fmt.Sprintf(" %s %s %s", outcomeMark(c.outcome()), bold.Render(c.name()), dim.Render(id)))
	if c.finishedLive() {
		add(dim.Render(" finished " + c.running.RunningFor + "; the result is shown once its round is saved"))
	}
	if v != nil {
		if v.Head != nil {
			if v.Head.Cwd != nil {
				add(dim.Render(" cwd " + *v.Head.Cwd))
			}
			for _, l := range wrapText(headText(v.Head), width-2) {
				add(" " + bold.Render(l))
			}
		}
		if f := facts(v); f != "" {
			add(" " + f)
		}
		for _, n := range v.Notes {
			add(warn.Render(" ! " + n))
		}
		for _, n := range v.Info {
			add(dim.Render(" " + n))
		}
		if v.ThreadID != nil {
			add(dim.Render(" ran in thread " + *v.ThreadID))
		}
		add("")
		add(bodyLines(v.Body, width)...)
	}
	if c.running != nil && c.result == nil && c.running.OutputTail != "" {
		label := " output so far"
		if c.finishedLive() {
			label = " output tail"
		}
		add("", bold.Render(label))
		for _, l := range wrapText(c.running.OutputTail, width-2) {
			add(" " + l)
		}
	}
	var input json.RawMessage
	switch {
	case c.use != nil:
		input = c.use.Input
	case c.running != nil:
		input = c.running.Input
	}
	if len(input) > 0 && string(input) != "null" {
		var buf bytes.Buffer
		if json.Indent(&buf, input, " ", "  ") == nil {
			add("", bold.Render(" input"))
			for _, l := range wrapText(cleanText(buf.String()), width-2) {
				add(dim.Render(" " + l))
			}
		}
	}
	return out
}

func bodyLines(b data.ToolBody, width int) []string {
	var out []string
	wrap := func(s string, style func(...string) string) {
		for _, l := range wrapText(s, width-2) {
			out = append(out, " "+style(l))
		}
	}
	plain := func(s ...string) string { return strings.Join(s, " ") }
	switch b.Kind {
	case "empty":
		out = append(out, dim.Render(" (no output)"))
	case "streams":
		if len(b.Streams) == 0 {
			out = append(out, dim.Render(" (no output)"))
		}
		for _, s := range b.Streams {
			out = append(out, dim.Render(" ── "+s.Name+" ──"))
			if s.Name == "stderr" {
				wrap(s.Text, warn.Render)
			} else {
				wrap(s.Text, plain)
			}
		}
	case "code":
		for i, l := range strings.Split(strings.TrimRight(b.Text, "\n"), "\n") {
			out = append(out, fit(dim.Render(fmt.Sprintf(" %5d ", int(b.FirstLine)+i))+l, width))
		}
	case "matches":
		for _, f := range b.Files {
			out = append(out, " "+bold.Render(f.Path))
			for _, l := range f.Lines {
				line := fmt.Sprintf("   %5d: %s", l.Line, l.Text)
				if l.Context {
					line = dim.Render(line)
				}
				out = append(out, fit(line, width))
			}
		}
	case "paths":
		for _, e := range b.Entries {
			p := e.Path
			if e.Dir {
				p += "/"
			}
			out = append(out, " "+p)
		}
	case "diff":
		out = append(out, dim.Render(fmt.Sprintf(" +%d -%d", b.Additions, b.Deletions)))
		for _, l := range b.Lines {
			switch l.Op {
			case "add":
				out = append(out, fit(ok.Render(" +"+l.Text), width))
			case "del":
				out = append(out, fit(bad.Render(" -"+l.Text), width))
			default:
				out = append(out, fit(dim.Render("  "+l.Text), width))
			}
		}
	case "markdown":
		for _, l := range renderMarkdown(b.Text, width-2, mdNormal) {
			out = append(out, " "+l)
		}
	case "error":
		out = append(out, bad.Render(" "+b.Code+": "+b.Message))
		if b.Details != nil {
			wrap(*b.Details, dim.Render)
		}
	default:
		wrap(b.Text, plain)
	}
	return out
}
