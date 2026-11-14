package ui

import (
	"fmt"
	"strings"

	"github.com/charmbracelet/x/ansi"

	"github.com/tallesborges/zdx/apps/monitor/internal/data"
)

type agentsTab struct {
	rows   []data.Agent
	tree   []agentNode
	loaded bool
	list   listState
}

// agentID keys a run: a thread runs at most one turn at a time, and the
// pid separates runs without a thread.
func agentID(a data.Agent) string { return fmt.Sprintf("%s/%d", str(a.ThreadID), a.PID) }

func (t *agentsTab) ids() []string {
	ids := make([]string, len(t.tree))
	for i, n := range t.tree {
		ids[i] = n.id(t.rows)
	}
	return ids
}

func (t *agentsTab) set(rows []data.Agent) {
	t.rows, t.loaded = rows, true
	t.tree = agentTree(rows)
	t.list.sync(t.ids())
}

func (t *agentsTab) selectedNode() *agentNode {
	if t.list.selected >= 0 && t.list.selected < len(t.tree) {
		return &t.tree[t.list.selected]
	}
	return nil
}

func (t *agentsTab) selectedAgent() *data.Agent {
	if n := t.selectedNode(); n != nil && n.run >= 0 {
		return &t.rows[n.run]
	}
	return nil
}

// runStyle colors a run state.
func runStyle(state string) cell {
	switch state {
	case "tool":
		return cs(state, accent)
	case "thinking", "answering":
		return cs(state, ok)
	case "retrying":
		return cs(state, warn)
	}
	return cs(state, dim)
}

func (m *Model) renderAgents(width, h int) []string {
	t := &m.agents
	if !t.loaded {
		return m.pending(keyAgents, "active agents")
	}
	out := []string{sectionTitle("Active agents", count(len(t.rows), "run", "runs"))}
	if len(t.rows) == 0 {
		return append(out, emptyState("No agent is running.")...)
	}
	runMax := 34
	if width < 110 {
		runMax = 26
	}
	if width < 80 {
		runMax = 18
	}
	tb := table{cols: []col{
		{title: "", max: 1},
		{title: "RUN", max: runMax},
		{title: "STATE", max: 9},
		{title: "SURFACE", max: 9, minWidth: 150},
		{title: "MODEL", flex: true, min: 12},
		{title: "UP", max: 6, right: true, minWidth: 80},
		{title: "THREAD", max: 38, minWidth: 170, cutLeft: true},
	}}
	now := m.now()
	for _, n := range t.tree {
		if n.run < 0 {
			inModel := n.thread
			if width >= 170 {
				inModel = ""
			}
			tb.rows = append(tb.rows, []cell{
				cs("◌", dim), cs("parent, idle", dim), c(""), c(""), cs(inModel, dim), c(""), cs(n.thread, dim),
			})
			continue
		}
		a := t.rows[n.run]
		state := runStyle(a.Run.State)
		tb.rows = append(tb.rows, []cell{
			cs(spinner(now), state.style), c(ruleStyle.Render(n.prefix) + runName(a)), state, cs(orDash(str(a.Surface)), dim),
			specCell(a), cs(compactAge(a.Uptime), dim), cs(orDash(str(a.ThreadID)), dim),
		})
	}
	var detail []string
	if n := t.selectedNode(); n != nil && n.run >= 0 {
		detail = agentDetail(t.rows[n.run], width)
	} else if n != nil {
		detail = []string{
			kv("Thread", n.thread),
			kv("Activity", dim.Render("not running")),
			"  " + dim.Render(fmt.Sprintf("Parent of %s below; shown for context, not counted as a run.", count(n.kids, "active run", "active runs"))),
		}
	}
	return listWithDetail(out, width, h, &tb, &t.list, detail, "Selected run")
}

// runName is the run's role (subagent name or kind), else its surface.
func runName(a data.Agent) string {
	if r := str(a.Role); r != "" {
		return r
	}
	if s := str(a.Surface); s != "" {
		return dim.Render(s)
	}
	return "—"
}

// specParts splits the run's reported model at the provider boundary:
// prefix is `provider[@account]` and rest is the model with its thinking
// level. A model that already starts with its provider (`provider:` or
// `provider@account:`) keeps its own prefix; otherwise the marker's provider
// and account are put in front. The reported thinking level is added unless
// the model already carries it as an `@level` modifier. Other punctuation
// in the model is opaque, and missing parts are left out, never guessed.
func specParts(a data.Agent) (prefix, rest string) {
	model := str(a.Model)
	if model == "" {
		return "", ""
	}
	p, acc := str(a.Provider), str(a.Account)
	rest = model
	switch {
	case p == "":
	case strings.HasPrefix(model, p+":"):
		prefix, rest = p, model[len(p)+1:]
	case strings.HasPrefix(model, p+"@") && strings.Contains(model, ":"):
		i := strings.Index(model, ":")
		prefix, rest = model[:i], model[i+1:]
	default:
		prefix = p
		if acc != "" {
			prefix += "@" + acc
		}
	}
	if th := str(a.Thinking); th != "" && !hasModifier(rest, th) {
		rest += "@" + th
	}
	return prefix, rest
}

// hasModifier reports whether model has `@level` as one of its
// `@`-separated suffixes.
func hasModifier(model, level string) bool {
	parts := strings.Split(model, "@")
	for _, m := range parts[1:] {
		if strings.EqualFold(m, level) {
			return true
		}
	}
	return false
}

// modelSpec is the run's model as one spec,
// `provider[@account]:model[@thinking]`; the detail pane lists each part.
func modelSpec(a data.Agent) string {
	prefix, rest := specParts(a)
	if prefix == "" {
		return rest
	}
	return prefix + ":" + rest
}

// specCell is the spec, or a dash when the run reported no model.
func specCell(a data.Agent) cell {
	spec := modelSpec(a)
	if spec == "" {
		return cs("—", dim)
	}
	cl := c(spec)
	cl.shorten = func(w int) string { return shortSpec(a, w) }
	return cl
}

// shortSpec fits the spec to w cells: first without the account (the
// detail still shows it), then with the provider cut, keeping the model and
// its thinking level whole while they fit; a longer model keeps its end.
func shortSpec(a data.Agent, w int) string {
	prefix, rest := specParts(a)
	if p := str(a.Provider); prefix != "" && p != "" {
		prefix = p
	}
	spec := rest
	if prefix != "" {
		spec = prefix + ":" + rest
	}
	if ansi.StringWidth(spec) <= w {
		return spec
	}
	if prefix == "" || ansi.StringWidth(rest)+3 > w {
		return ansi.TruncateLeft(spec, ansi.StringWidth(spec)-w+1, "…")
	}
	room := w - ansi.StringWidth(rest) - 1
	return ansi.Truncate(prefix, room, "…") + ":" + rest
}

func unknown(p *uint64, f func(uint64) string) string {
	if p == nil {
		return dim.Render("unknown")
	}
	return f(*p)
}

func agentDetail(a data.Agent, width int) []string {
	secsSpan := func(v uint64) string { return span(sec(v)) }
	ago := func(v uint64) string { return span(sec(v)) + " ago" }
	model := orDash(modelSpec(a))
	part := func(p *string) string {
		if p == nil || *p == "" {
			return dim.Render("not reported")
		}
		return *p
	}
	parts := dim.Render("provider ") + part(a.Provider) + dim.Render(" · account ") + part(a.Account) + dim.Render(" · thinking ") + part(a.Thinking)
	if a.Account == nil || *a.Account == "" {
		parts = dim.Render("provider ") + part(a.Provider) + dim.Render(" · thinking ") + part(a.Thinking)
	}
	activity := runStyle(a.Run.State).style.Render(a.Run.State)
	if a.Run.Label != "" && a.Run.Label != a.Run.State {
		activity += "  " + a.Run.Label
	}
	lines := []string{
		kv("Activity", activity),
		kv("Thread", orDash(str(a.ThreadID))),
		kv("Turn", unknown(a.Run.TurnElapsedSecs, secsSpan)+dim.Render(" · last event ")+unknown(a.Run.SecondsSinceActivity, ago)+dim.Render(" · heartbeat ")+unknown(a.Run.SecondsSinceBeat, ago)),
		kv("Tools", fmt.Sprintf("%d running · %d finished", a.Run.RunningTools, a.Run.FinishedTools)),
		kv("Model", model),
		kv("", parts),
		kv("Process", fmt.Sprintf("pid %d", a.PID)+dim.Render(" · "+orDash(str(a.Surface))+" · up "+a.Uptime)),
	}
	if a.ParentThreadID != nil {
		lines = append(lines[:2], append([]string{kv("Parent", *a.ParentThreadID)}, lines[2:]...)...)
	}
	if a.ThreadID == nil {
		lines = append(lines, "  "+dim.Render("No thread: there is no transcript to open."))
	}
	for i := range lines {
		lines[i] = fit(lines[i], width)
	}
	return lines
}
