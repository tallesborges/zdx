package ui

import (
	"fmt"
	"os"
	"strings"
	"time"

	tea "charm.land/bubbletea/v2"
	"github.com/charmbracelet/x/ansi"

	"github.com/tallesborges/zdx/apps/shared/transcript"
)

// View renders the screen.
func (m *Model) View() tea.View {
	v := tea.NewView(m.render())
	v.AltScreen = true
	v.MouseMode = tea.MouseModeCellMotion
	name := m.title
	if name == "" {
		name = "new thread"
	}
	v.WindowTitle = "zdx · " + name
	return v
}

const minWidth, minHeight = 20, 8

func (m *Model) bodyRows() int {
	rows := m.height - 2 - m.composer.Height() - 1
	if len(m.attachments) > 0 {
		rows--
	}
	return max(rows, 1)
}

func (m *Model) render() string {
	if m.width < minWidth || m.height < minHeight {
		return "zdx: terminal too small"
	}
	w := m.width
	out := []string{transcript.Fit(m.header(), w)}
	rows := m.bodyRows()
	var body []string
	if m.overlay != nil {
		body = m.overlay.render(w, rows)
		if len(body) > rows {
			body = body[:rows]
		}
		for len(body) < rows {
			body = append(body, "")
		}
	} else {
		body = m.viewport(rows)
	}
	out = append(out, body...)
	if len(m.attachments) > 0 {
		var names []string
		for _, a := range m.attachments {
			names = append(names, "⎘ "+a.name)
		}
		out = append(out, transcript.Fit(" "+transcript.Accent.Render(strings.Join(names, "  ")), w))
	}
	out = append(out, rule.Render(strings.Repeat("─", w)))
	for _, l := range strings.Split(m.composer.View(), "\n") {
		out = append(out, transcript.Fit(" "+l, w))
	}
	out = append(out, transcript.Fit(m.footer(), w))
	if len(out) > m.height {
		out = out[len(out)-m.height:]
	}
	return strings.Join(out, "\n")
}

func (m *Model) header() string {
	name := m.title
	if name == "" {
		if m.saved {
			name = m.threadID
		} else {
			name = "new thread"
		}
	}
	parts := []string{title.Render(name)}
	if m.hasStat {
		model := m.status.Model
		if m.status.Thinking != "" && !strings.Contains(model, "@") {
			model += " · " + m.status.Thinking
		}
		parts = append(parts, transcript.Accent.Render(model))
		if m.status.ContextLimit > 0 && m.status.ContextTokens > 0 {
			pct := m.status.ContextTokens * 100 / m.status.ContextLimit
			parts = append(parts, transcript.Dim.Render(fmt.Sprintf("ctx %s/%s %d%%",
				transcript.Tokens(m.status.ContextTokens), transcript.Tokens(m.status.ContextLimit), pct)))
		}
		root := m.status.Root
		if home, err := os.UserHomeDir(); err == nil && strings.HasPrefix(root, home) {
			root = "~" + strings.TrimPrefix(root, home)
		}
		if m.status.Branch != nil {
			root += " (" + *m.status.Branch + ")"
		}
		parts = append(parts, transcript.Dim.Render(root))
		if m.status.Refused != nil {
			parts = append(parts, transcript.Bad.Render(*m.status.Refused))
		}
	}
	return " " + strings.Join(parts, transcript.Dim.Render(" · "))
}

func (m *Model) footer() string {
	var left string
	switch {
	case m.rec != nil:
		left = transcript.Bad.Render(fmt.Sprintf("● rec %ds", int(time.Since(m.rec.start).Seconds())))
	case m.transcribe:
		left = transcript.Accent.Render("transcribing…")
	case m.turnID != 0:
		left = transcript.Accent.Render("● running") + transcript.Dim.Render("  enter steer · tab queue · esc interrupt")
	}
	if m.notice != "" {
		style := transcript.Bad
		if m.noticeOK {
			style = transcript.Dim
		}
		if left != "" {
			left += "  "
		}
		left += style.Render(transcript.OneLine(m.notice))
	}
	if left == "" {
		left = transcript.Dim.Render("enter send · ctrl+o commands · ctrl+l model · ctrl+p threads · ctrl+n new · ctrl+c quit")
	}
	if m.scroll > 0 {
		left = transcript.Warn.Render(fmt.Sprintf("↑ %d lines (ctrl+end)", m.scroll)) + "  " + left
	}
	return " " + left
}

func (m *Model) renderOpts() transcript.Options {
	return transcript.Options{Thinking: m.thinking, ExpandKey: "alt+t"}
}

// liveThread is what the saved thread does not have yet: the turns seen
// on the subscription, then the messages still on their way.
func (m *Model) liveThread() *transcript.Thread {
	var acts []transcript.Activity
	for _, t := range m.turns {
		for _, it := range t.items {
			// The engine strips the suggested replies from saved text;
			// a streaming reply hides them the same way.
			if it.Type == "message" && it.Role == "assistant" {
				if i := strings.Index(it.Text, "<followups>"); i >= 0 {
					it.Text = strings.TrimRight(it.Text[:i], " \n")
				}
			}
			acts = append(acts, it)
		}
	}
	for _, o := range m.outbox {
		speaker := "you (sending)"
		switch {
		case o.steer && o.acked:
			speaker = "you (steer, waiting for the turn to read it)"
		case o.acked:
			speaker = "you (queued)"
		}
		a, ok := userItem(speaker, o.text, o.images)
		if !ok {
			a, ok = userItem(speaker, "(retry)", 0)
		}
		if ok {
			a.Sequence = liveBase - 1 - o.local
			acts = append(acts, a)
		}
	}
	return &transcript.Thread{Activity: acts}
}

// transcriptLines is the persisted thread's lines (cached per width and
// thinking toggle) followed by the live items.
func (m *Model) transcriptLines(width int) []string {
	k := fmt.Sprintf("%d/%v/%p/%d", width, m.thinking, m.thread, m.gen)
	if k != m.linesKey {
		m.lines, _ = transcript.Render(m.thread, width, m.renderOpts(), &m.md)
		m.md.Done()
		m.linesKey = k
		m.liveDirty = true
	}
	if m.liveDirty {
		m.cachedLive, _ = transcript.Render(m.liveThread(), width, m.renderOpts(), &m.liveMD)
		m.liveMD.Done()
		m.liveDirty = false
	}
	if len(m.cachedLive) == 0 && len(m.followups) == 0 {
		return m.lines
	}
	out := make([]string, 0, len(m.lines)+len(m.cachedLive)+1)
	out = append(out, m.lines...)
	if len(out) > 0 {
		out = append(out, "")
	}
	out = append(out, m.cachedLive...)
	return append(out, m.followupLines(width)...)
}

// followupLines are the last reply's suggested replies: a header that names
// the key that picks one, then one reply per line.
func (m *Model) followupLines(width int) []string {
	if len(m.followups) == 0 {
		return nil
	}
	out := []string{"", "  " + transcript.Accent.Bold(true).Render("↳ Suggested replies") + transcript.Dim.Render("  ctrl+f pick")}
	for _, f := range m.followups {
		out = append(out, transcript.Fit("    "+transcript.Accent.Render("›")+" "+f, width))
	}
	return out
}

func (m *Model) viewport(rows int) []string {
	w := m.width - 1
	lines := m.transcriptLines(w)
	if len(lines) == 0 {
		lines = m.emptyLines()
	}
	maxScroll := max(len(lines)-rows, 0)
	m.scroll = min(m.scroll, maxScroll)
	end := len(lines) - m.scroll
	start := max(end-rows, 0)
	body := make([]string, 0, rows)
	for _, l := range lines[start:end] {
		body = append(body, transcript.Fit(" "+l, m.width))
	}
	for len(body) < rows {
		body = append([]string{""}, body...)
	}
	return body
}

func (m *Model) emptyLines() []string {
	if m.saved && m.thread == nil {
		return []string{transcript.Dim.Render("loading…")}
	}
	return []string{
		transcript.Dim.Render("New thread in " + m.opts.Root),
	}
}

// plainLines is the screen without styles (tests).
func plainLines(s string) []string { return strings.Split(ansi.Strip(s), "\n") }
