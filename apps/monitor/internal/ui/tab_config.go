package ui

import (
	"charm.land/lipgloss/v2"
	"encoding/json"
	"fmt"
	"github.com/charmbracelet/x/ansi"
	"strings"

	"github.com/tallesborges/zdx/apps/monitor/internal/data"
)

type configTab struct {
	data  *data.Workspaces
	index int
	list  listState
}

func (t *configTab) set(v data.Workspaces) {
	var keep string
	if t.data != nil && t.index < len(t.data.Workspaces) {
		keep = t.data.Workspaces[t.index].Root
	}
	t.data = &v
	t.index = 0
	for i, w := range v.Workspaces {
		if (keep == "" && w.Current) || (keep != "" && w.Root == keep) {
			t.index = i
		}
	}
	t.list.sync(t.ids())
}

func (t *configTab) workspace() *data.Workspace {
	if t.data != nil && t.index < len(t.data.Workspaces) {
		return &t.data.Workspaces[t.index]
	}
	return nil
}

func (t *configTab) ids() []string {
	w := t.workspace()
	if w == nil {
		return nil
	}
	ids := make([]string, len(w.Settings))
	for i, s := range w.Settings {
		ids[i] = s.Key
	}
	return ids
}

func (t *configTab) rowCount() int { return len(t.ids()) }

func (t *configTab) cycle(up bool) {
	if t.data == nil || len(t.data.Workspaces) == 0 {
		return
	}
	n := len(t.data.Workspaces)
	if up {
		t.index = (t.index + 1) % n
	} else {
		t.index = (t.index + n - 1) % n
	}
	t.list = listState{}
}

// jsonValue shows a setting value: a string bare, null as unset, anything
// else as compact JSON.
func jsonValue(raw json.RawMessage) string {
	if len(raw) == 0 || string(raw) == "null" {
		return dim.Render("(unset)")
	}
	var s string
	if json.Unmarshal(raw, &s) == nil {
		return cleanText(s)
	}
	return oneLine(string(raw))
}

func (m *Model) renderConfig(width, h int) []string {
	t := &m.config
	out := []string{sectionTitle("Config", "read only")}
	if sn := m.snapshot; sn != nil {
		c0 := sn.Config
		model := c0.Model
		if c0.ModelMode != nil {
			model += dim.Render(" via " + *c0.ModelMode)
		}
		out = append(out,
			kv("Default", model+dim.Render(fmt.Sprintf(" · thinking %s · %s · subagents %s", c0.Thinking, count(c0.ModeCount, "mode", "modes"), onOff(c0.SubagentsEnabled)))))
	}
	ws := t.workspace()
	if ws == nil {
		return append(out, m.pending(keyWorkspaces, "workspaces")...)
	}
	cur := ""
	if ws.Current {
		cur = ok.Render("  current")
	}
	out = append(out, "", rule(fmt.Sprintf("Workspace %d of %d · [ ] switch", t.index+1, len(t.data.Workspaces)), width),
		"  "+bold.Render(ws.Name)+cur+dim.Render("  "+tildePath(ws.Root)),
		kv("Overlay", tildePath(ws.OverlayPath)+dim.Render(map[bool]string{true: "", false: " (none yet)"}[ws.HasOverlay])))
	if ws.Error != nil {
		out = append(out, kv("Problem", bad.Render(oneLine(*ws.Error))))
	}
	tb := table{cols: []col{
		{title: "GROUP", max: 14, minWidth: 100},
		{title: "SETTING", max: 22},
		{title: "VALUE", flex: true, min: 12},
		{title: "FROM", max: 12},
		{title: "NOTE", max: 48, minWidth: 120},
	}}
	group := ""
	for _, st := range ws.Settings {
		g := st.Group
		if g == group {
			g = ""
		}
		group = st.Group
		note := ""
		if st.InheritedSource != nil && st.InheritedSource.Label != st.Source.Label {
			note = "overrides " + ansi.Strip(jsonValue(st.InheritedValue)) + " from " + st.InheritedSource.Label
		}
		if len(st.ResolvesTo) > 0 && string(st.ResolvesTo) != "null" {
			note = strings.TrimSpace(note + " → " + ansi.Strip(jsonValue(st.ResolvesTo)))
		}
		value := c(jsonValue(st.Value))
		if len(st.Value) == 0 || string(st.Value) == "null" {
			value = cs("unset", dim)
		}
		tb.rows = append(tb.rows, []cell{cs(g, dim), c(st.Label), value, cs(st.Source.Label, sourceStyle(st.Source.Layer)), cs(note, dim)})
	}
	out = append(out, "")
	return append(out, tb.render(width, h-len(out), &t.list)...)
}

func sourceStyle(layer string) lipgloss.Style {
	switch layer {
	case "default":
		return dim
	case "global":
		return plainStyle
	}
	return accent
}

func onOff(b bool) string {
	if b {
		return "on"
	}
	return "off"
}
