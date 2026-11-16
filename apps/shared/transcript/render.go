package transcript

import (
	"bytes"
	"encoding/json"
	"fmt"
	"strings"

	"charm.land/lipgloss/v2"
	"github.com/charmbracelet/x/ansi"
)

// Styles of the transcript layout. Surfaces may use them for their own
// chrome so a thread looks the same everywhere.
var (
	Dim    = lipgloss.NewStyle().Foreground(lipgloss.BrightBlack)
	Bold   = lipgloss.NewStyle().Bold(true)
	OK     = lipgloss.NewStyle().Foreground(lipgloss.Green)
	Warn   = lipgloss.NewStyle().Foreground(lipgloss.Yellow)
	Bad    = lipgloss.NewStyle().Foreground(lipgloss.Red)
	Accent = lipgloss.NewStyle().Foreground(lipgloss.Magenta)

	ok = OK
)

// Fit cuts a styled line to width cells.
func Fit(s string, width int) string {
	if width <= 0 {
		return ""
	}
	return ansi.Truncate(s, width, "…")
}

// OneLine is external text (cleaned) on one line.
func OneLine(s string) string {
	s = strings.ReplaceAll(Clean(s), "\n", " ")
	return strings.Join(strings.Fields(s), " ")
}

// WrapText wraps plain text to width cells (at least 10): words first,
// then hard breaks for longer runs. Tabs become four spaces.
func WrapText(s string, width int) []string {
	width = max(width, 10)
	var out []string
	for _, line := range strings.Split(strings.ReplaceAll(s, "\t", "    "), "\n") {
		w := ansi.Wordwrap(line, width, " -")
		w = ansi.Hardwrap(w, width, true)
		out = append(out, strings.Split(w, "\n")...)
	}
	return out
}

// Count is "1 line" or "n lines".
func Count(n int, one, many string) string {
	if n == 1 {
		return "1 " + one
	}
	return fmt.Sprintf("%d %s", n, many)
}

// Tokens is a short token count: 950, 1.2k, 3.4M, 1.0B.
func Tokens(n uint64) string {
	switch {
	case n >= 1_000_000_000:
		return fmt.Sprintf("%.1fB", float64(n)/1e9)
	case n >= 1_000_000:
		return fmt.Sprintf("%.1fM", float64(n)/1e6)
	case n >= 1_000:
		return fmt.Sprintf("%.1fk", float64(n)/1e3)
	}
	return fmt.Sprint(n)
}

func str(p *string) string {
	if p == nil {
		return ""
	}
	return *p
}

// Merge applies a `thread_view` poll: a full answer replaces, a partial one
// keeps the persisted items, drops running tools (always resent), and
// appends what is new (apps/web ThreadView.svelte poll).
func Merge(old *Thread, delta Thread) Thread {
	if old == nil || !delta.Partial {
		return delta
	}
	seen := map[uint64]bool{}
	var activity []Activity
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

// HasRunning reports a running tool item.
func HasRunning(th *Thread) bool {
	for _, a := range th.Activity {
		if a.Type == "tool_running" {
			return true
		}
	}
	return false
}

// HeadText is a tool head on one line: the command, path, pattern, or prompt.
func HeadText(h *ToolHead) string {
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

// OutcomeMark is the glyph of a tool outcome; an unknown outcome is pending.
func OutcomeMark(outcome string) string {
	switch outcome {
	case "ok":
		return OK.Render("✓")
	case "failed":
		return Bad.Render("✗")
	case "timed_out":
		return Bad.Render("⏱")
	case "canceled":
		return Warn.Render("⊘")
	case "unknown":
		return Dim.Render("?")
	}
	return Accent.Render("…")
}

// Facts is a view's status facts and hint on one line.
func Facts(v *ToolView) string {
	if v == nil {
		return ""
	}
	var parts []string
	for _, f := range v.Status {
		if f.Bad {
			parts = append(parts, Bad.Render(f.Text))
		} else {
			parts = append(parts, Dim.Render(f.Text))
		}
	}
	if v.Hint != nil {
		parts = append(parts, Dim.Render(v.Hint.Text))
	}
	return strings.Join(parts, Dim.Render(" · "))
}

// Call gathers the items of one tool call: its use, its result, and its
// running marker.
type Call struct {
	Use, Result, Running *Activity
}

// View is the most complete view of the call.
func (c Call) View() *ToolView {
	switch {
	case c.Result != nil && c.Result.View != nil:
		return c.Result.View
	case c.Running != nil && c.Running.View != nil:
		return c.Running.View
	case c.Use != nil:
		return c.Use.View
	}
	return nil
}

// FinishedLive is a call the run marker reports finished while its round
// is not yet in the thread file.
func (c Call) FinishedLive() bool {
	return c.Result == nil && c.Running != nil && c.Running.CompletedAt != nil
}

// Outcome is the view's outcome; a finished live call has only an input
// view, so its outcome comes from the marker's `ok` (unknown when absent).
func (c Call) Outcome() string {
	if c.FinishedLive() {
		switch {
		case c.Running.OK == nil:
			return "unknown"
		case *c.Running.OK:
			return "ok"
		}
		return "failed"
	}
	if v := c.View(); v != nil {
		return v.Outcome
	}
	return "pending"
}

// Name is the tool name.
func (c Call) Name() string {
	switch {
	case c.Use != nil:
		return c.Use.Name
	case c.Running != nil:
		return c.Running.Name
	}
	return "tool"
}

// Calls indexes a thread's tool items by call id.
func Calls(th *Thread) map[string]*Call {
	out := map[string]*Call{}
	get := func(id string) *Call {
		if out[id] == nil {
			out[id] = &Call{}
		}
		return out[id]
	}
	for i := range th.Activity {
		a := &th.Activity[i]
		switch a.Type {
		case "tool_use":
			get(a.ID).Use = a
		case "tool_running":
			get(a.ID).Running = a
		case "tool_result":
			get(a.ToolUseID).Result = a
		}
	}
	return out
}

// ToolRef is one tool row: its call id and its rendered line span.
type ToolRef struct {
	ID         string
	Start, End int
}

// IsToolLine reports whether line is the last tool row of tools.
func IsToolLine(tools []ToolRef, line int) bool {
	return len(tools) > 0 && tools[len(tools)-1].End-1 == line
}

// ToolLine is the one-line row of a call.
func ToolLine(c *Call) string {
	v := c.View()
	head := ""
	if v != nil {
		head = HeadText(v.Head)
	}
	line := fmt.Sprintf("%s %s %s", OutcomeMark(c.Outcome()), Bold.Render(c.Name()), OneLine(head))
	if c.FinishedLive() {
		line += Dim.Render(" finished " + c.Running.RunningFor + ", not saved yet")
	} else if c.Running != nil && c.Result == nil {
		line += Accent.Render(" running " + c.Running.RunningFor)
	} else if c.Result == nil && c.Use != nil {
		line += Dim.Render(" (no result)")
	}
	if f := Facts(v); f != "" {
		line += "  " + f
	}
	return line
}

// Options shape a layout.
type Options struct {
	// Thinking expands every thinking block.
	Thinking bool
	// ExpandKey is the key a collapsed thinking block names (default "t").
	ExpandKey string
}

// Render lays the transcript out as lines and records each tool row's span.
// Message, thinking, and interruption text is Markdown; tool rows and their
// output are not. md may be nil.
func Render(th *Thread, width int, opts Options, md *Cache) ([]string, []ToolRef) {
	if th == nil {
		return nil, nil
	}
	var lines []string
	var tools []ToolRef
	byID := Calls(th)
	shown := map[string]bool{}
	gap := func() {
		if len(lines) > 0 && lines[len(lines)-1] != "" {
			lines = append(lines, "")
		}
	}
	prose := func(a *Activity, theme *Theme) []string {
		return md.Render(a.Sequence, a.Text, width-4, theme)
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
			style := OK
			if a.Role == "user" {
				style = Accent
			}
			lines = append(lines, style.Bold(true).Render("▌ "+who)+Dim.Render("  "+a.Time))
			if a.Text != "" {
				indent(prose(a, Normal))
			}
			for _, f := range a.Artifacts {
				note := f.Kind
				if !f.Exists {
					note += ", missing"
				}
				lines = append(lines, Fit("  "+Accent.Render("⎘ "+f.Name)+Dim.Render(" ("+note+") "+f.Path), width))
			}
		case "reasoning":
			gap()
			switch {
			case a.Redacted:
				lines = append(lines, Dim.Render("∴ thinking (redacted)"))
			case opts.Thinking:
				lines = append(lines, Dim.Render("∴ thinking"))
				indent(prose(a, Muted))
			default:
				n := len(prose(a, Muted))
				key := opts.ExpandKey
				if key == "" {
					key = "t"
				}
				lines = append(lines, Dim.Render(fmt.Sprintf("∴ thinking · %s (%s to expand)", Count(n, "line", "lines"), key)))
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
			if prev := len(lines); prev > 0 && !IsToolLine(tools, prev-1) {
				gap()
			}
			tools = append(tools, ToolRef{ID: id, Start: len(lines), End: len(lines) + 1})
			lines = append(lines, ToolLine(byID[id]))
		case "usage":
			lines = append(lines, Dim.Render(fmt.Sprintf("  · usage in %s out %s cache %s/%s %s",
				Tokens(a.InputTokens), Tokens(a.OutputTokens), Tokens(a.CacheReadTokens), Tokens(a.CacheWriteTokens), str(a.Model))))
		case "notice":
			gap()
			if a.Kind == "turn_failed" {
				lines = append(lines, Bad.Render("✗ turn failed: "+OneLine(a.Message)))
			} else {
				lines = append(lines, Warn.Render("! "+a.Kind+": "+OneLine(a.Message)))
			}
		case "interrupted":
			gap()
			lines = append(lines, Bad.Render("⨯ interrupted ("+a.Role+")"))
			if a.Text != "" {
				indent(prose(a, Muted))
			}
		default:
			gap()
			lines = append(lines, Dim.Render("["+a.Type+"]"))
		}
	}
	return lines, tools
}

// Detail renders one call's full detail: head, facts, notes, body, and
// input. A call that is no longer in the thread says so.
func Detail(th *Thread, id string, width int) []string {
	if th == nil {
		return nil
	}
	c := Calls(th)[id]
	if c == nil {
		return []string{Dim.Render(" this call is no longer in the transcript")}
	}
	v := c.View()
	var out []string
	add := func(s ...string) { out = append(out, s...) }
	add(fmt.Sprintf(" %s %s %s", OutcomeMark(c.Outcome()), Bold.Render(c.Name()), Dim.Render(id)))
	if c.FinishedLive() {
		add(Dim.Render(" finished " + c.Running.RunningFor + "; the result is shown once its round is saved"))
	}
	if v != nil {
		if v.Head != nil {
			if v.Head.Cwd != nil {
				add(Dim.Render(" cwd " + *v.Head.Cwd))
			}
			for _, l := range WrapText(HeadText(v.Head), width-2) {
				add(" " + Bold.Render(l))
			}
		}
		if f := Facts(v); f != "" {
			add(" " + f)
		}
		for _, n := range v.Notes {
			add(Warn.Render(" ! " + n))
		}
		for _, n := range v.Info {
			add(Dim.Render(" " + n))
		}
		if v.ThreadID != nil {
			add(Dim.Render(" ran in thread " + *v.ThreadID))
		}
		add("")
		add(BodyLines(v.Body, width)...)
	}
	if c.Running != nil && c.Result == nil && c.Running.OutputTail != "" {
		label := " output so far"
		if c.FinishedLive() {
			label = " output tail"
		}
		add("", Bold.Render(label))
		for _, l := range WrapText(c.Running.OutputTail, width-2) {
			add(" " + l)
		}
	}
	var input json.RawMessage
	switch {
	case c.Use != nil:
		input = c.Use.Input
	case c.Running != nil:
		input = c.Running.Input
	}
	if len(input) > 0 && string(input) != "null" {
		var buf bytes.Buffer
		if json.Indent(&buf, input, " ", "  ") == nil {
			add("", Bold.Render(" input"))
			for _, l := range WrapText(Clean(buf.String()), width-2) {
				add(Dim.Render(" " + l))
			}
		}
	}
	return out
}

// BodyLines lays out a tool body.
func BodyLines(b ToolBody, width int) []string {
	var out []string
	wrap := func(s string, style func(...string) string) {
		for _, l := range WrapText(s, width-2) {
			out = append(out, " "+style(l))
		}
	}
	plain := func(s ...string) string { return strings.Join(s, " ") }
	switch b.Kind {
	case "empty":
		out = append(out, Dim.Render(" (no output)"))
	case "streams":
		if len(b.Streams) == 0 {
			out = append(out, Dim.Render(" (no output)"))
		}
		for _, s := range b.Streams {
			out = append(out, Dim.Render(" ── "+s.Name+" ──"))
			if s.Name == "stderr" {
				wrap(s.Text, Warn.Render)
			} else {
				wrap(s.Text, plain)
			}
		}
	case "code":
		for i, l := range strings.Split(strings.TrimRight(b.Text, "\n"), "\n") {
			out = append(out, Fit(Dim.Render(fmt.Sprintf(" %5d ", int(b.FirstLine)+i))+l, width))
		}
	case "matches":
		for _, f := range b.Files {
			out = append(out, " "+Bold.Render(f.Path))
			for _, l := range f.Lines {
				line := fmt.Sprintf("   %5d: %s", l.Line, l.Text)
				if l.Context {
					line = Dim.Render(line)
				}
				out = append(out, Fit(line, width))
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
		out = append(out, Dim.Render(fmt.Sprintf(" +%d -%d", b.Additions, b.Deletions)))
		for _, l := range b.Lines {
			switch l.Op {
			case "add":
				out = append(out, Fit(OK.Render(" +"+l.Text), width))
			case "del":
				out = append(out, Fit(Bad.Render(" -"+l.Text), width))
			default:
				out = append(out, Fit(Dim.Render("  "+l.Text), width))
			}
		}
	case "markdown":
		for _, l := range Markdown(b.Text, width-2, Normal) {
			out = append(out, " "+l)
		}
	case "error":
		out = append(out, Bad.Render(" "+b.Code+": "+b.Message))
		if b.Details != nil {
			wrap(*b.Details, Dim.Render)
		}
	default:
		wrap(b.Text, plain)
	}
	return out
}

// pad cuts or pads plain text to width cells.
func pad(s string, width int) string {
	s = ansi.Truncate(s, width, "…")
	if w := ansi.StringWidth(s); w < width {
		s += strings.Repeat(" ", width-w)
	}
	return s
}
