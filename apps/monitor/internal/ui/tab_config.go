package ui

import (
	"encoding/json"
	"fmt"
	"strings"

	"charm.land/lipgloss/v2"
	"github.com/charmbracelet/x/ansi"

	"github.com/tallesborges/zdx/apps/monitor/internal/data"
	"github.com/tallesborges/zdx/apps/shared/transcript"
)

// Subtle colors of the Config tab, on top of the shared palette.
var (
	groupStyle     = lipgloss.NewStyle().Foreground(lipgloss.ANSIColor(109))
	globalStyle    = lipgloss.NewStyle().Foreground(lipgloss.ANSIColor(110))
	ancestorStyle  = lipgloss.NewStyle().Foreground(lipgloss.ANSIColor(179))
	modeStyle      = lipgloss.NewStyle().Foreground(lipgloss.ANSIColor(176))
	thinkingStyles = map[string]lipgloss.Style{
		"off":     dim,
		"minimal": lipgloss.NewStyle().Foreground(lipgloss.ANSIColor(108)),
		"low":     lipgloss.NewStyle().Foreground(lipgloss.ANSIColor(108)),
		"medium":  lipgloss.NewStyle().Foreground(lipgloss.ANSIColor(179)),
		"high":    lipgloss.NewStyle().Foreground(lipgloss.ANSIColor(173)),
		"xhigh":   lipgloss.NewStyle().Foreground(lipgloss.ANSIColor(167)),
		"max":     lipgloss.NewStyle().Foreground(lipgloss.ANSIColor(167)),
	}
)

// configScope is what the tab shows and edits: the global config on its
// own, or one workspace through all its layers. Edits go to its own file.
type configScope struct {
	name     string
	root     string // empty for the global scope
	global   bool
	current  bool
	file     string
	hasFile  bool
	err      *string
	settings []data.Setting
}

// configRow is one table row: a setting, or one mode of `model_modes`.
type configRow struct {
	id      string
	setting *data.Setting
	mode    int // index into setting.Modes, or -1
}

type configTab struct {
	data   *data.Workspaces
	scopes []configScope
	index  int
	list   listState
	edit   *configEdit
	// notice is the result of the last write, shown under the header.
	notice    string
	noticeBad bool
}

func (t *configTab) set(v data.Workspaces) {
	keep, had := "", false
	if s := t.scope(); s != nil {
		keep, had = s.root, true
	}
	t.data = &v
	t.scopes = t.scopes[:0]
	if g := v.Global; g != nil {
		t.scopes = append(t.scopes, configScope{name: "global", global: true, file: g.Path, hasFile: true, err: g.Error, settings: g.Settings})
	}
	for _, w := range v.Workspaces {
		file := w.OverlayPath
		if w.OverlayIsGlobal && v.Global != nil {
			file = v.Global.Path
		}
		t.scopes = append(t.scopes, configScope{name: w.Name, root: w.Root, current: w.Current, file: file, hasFile: w.HasOverlay || w.OverlayIsGlobal, err: w.Error, settings: w.Settings})
	}
	t.index = 0
	for i, s := range t.scopes {
		if (had && s.root == keep) || (!had && s.current) {
			t.index = i
			break
		}
	}
	t.list.sync(t.ids())
}

func (t *configTab) scope() *configScope {
	if t.index < len(t.scopes) {
		return &t.scopes[t.index]
	}
	return nil
}

func (t *configTab) rows() []configRow {
	s := t.scope()
	if s == nil {
		return nil
	}
	var rows []configRow
	for i := range s.settings {
		st := &s.settings[i]
		rows = append(rows, configRow{id: st.Key, setting: st, mode: -1})
		for j := range st.Modes {
			rows = append(rows, configRow{id: st.Key + "#" + st.Modes[j].Name, setting: st, mode: j})
		}
	}
	return rows
}

func (t *configTab) ids() []string {
	rows := t.rows()
	ids := make([]string, len(rows))
	for i, r := range rows {
		ids[i] = r.id
	}
	return ids
}

func (t *configTab) rowCount() int { return len(t.rows()) }

func (t *configTab) selectedRow() *configRow {
	rows := t.rows()
	if t.list.selected < len(rows) {
		return &rows[t.list.selected]
	}
	return nil
}

func (t *configTab) cycle(up bool) {
	if len(t.scopes) == 0 {
		return
	}
	n := len(t.scopes)
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
		return transcript.Clean(s)
	}
	return oneLine(string(raw))
}

// specValue colors a model spec: the provider dim, the model plain, a
// thinking level by its weight, a `mode:` reference in the mode color.
func specValue(spec string) string {
	if spec == "on" {
		return ok.Render("on")
	}
	if spec == "off" {
		return dim.Render("off")
	}
	if name, isMode := strings.CutPrefix(spec, "mode:"); isMode {
		return dim.Render("mode:") + modeStyle.Render(name)
	}
	provider, rest, hasProvider := strings.Cut(spec, ":")
	if !hasProvider {
		provider, rest = "", spec
	}
	parts := strings.Split(rest, "@")
	out := ""
	if hasProvider {
		out = dim.Render(provider + ":")
	}
	out += parts[0]
	for _, mod := range parts[1:] {
		style, isLevel := thinkingStyles[strings.ToLower(mod)]
		if !isLevel {
			style = dim
		}
		out += dim.Render("@") + style.Render(mod)
	}
	return out
}

func settingValue(st *data.Setting) string {
	if st.Kind == "modes" && len(st.Modes) > 0 {
		return dim.Render(count(len(st.Modes), "mode", "modes"))
	}
	var s string
	if len(st.Value) == 0 || string(st.Value) == "null" || json.Unmarshal(st.Value, &s) != nil {
		return jsonValue(st.Value)
	}
	return specValue(transcript.Clean(s))
}

func (m *Model) renderConfig(width, h int) []string {
	t := &m.config
	meta := "read only"
	if s := t.scope(); s != nil {
		meta = "↵ edit · x reset · writes " + tildePath(s.file)
	}
	out := []string{sectionTitle("Config", meta)}
	if sn := m.snapshot; sn != nil {
		c0 := sn.Config
		model := specValue(c0.Model)
		if c0.ModelMode != nil {
			model += dim.Render(" via ") + modeStyle.Render(*c0.ModelMode)
		}
		out = append(out,
			kv("Default", model+dim.Render(fmt.Sprintf(" · thinking %s · %s · subagents %s", c0.Thinking, count(c0.ModeCount, "mode", "modes"), onOff(c0.SubagentsEnabled)))))
	}
	s := t.scope()
	if s == nil {
		return append(out, m.pending(keyWorkspaces, "workspaces")...)
	}
	cur := ""
	if s.current {
		cur = ok.Render("  current")
	}
	name := bold.Render(s.name)
	where := dim.Render("  " + tildePath(s.root))
	if s.global {
		name = globalStyle.Bold(true).Render("global")
		where = dim.Render("  every workspace inherits it")
	}
	out = append(out, "", rule(fmt.Sprintf("Scope %d of %d · [ ] switch", t.index+1, len(t.scopes)), width),
		"  "+name+cur+where,
		kv("Writes", tildePath(s.file)+dim.Render(map[bool]string{true: "", false: " (created on the first edit)"}[s.hasFile])))
	if s.err != nil {
		out = append(out, kv("Problem", bad.Render(oneLine(*s.err))))
	}
	if t.notice != "" {
		style := ok
		if t.noticeBad {
			style = bad
		}
		out = append(out, kv("Last edit", style.Render(oneLine(t.notice))))
	}
	if t.edit != nil {
		return append(out, m.renderConfigEdit(width, h-len(out))...)
	}
	tb := table{cols: []col{
		{title: "GROUP", max: 14, minWidth: 100},
		{title: "SETTING", max: 22},
		{title: "VALUE", flex: true, min: 12},
		{title: "FROM", max: 12},
		{title: "NOTE", max: 30, minWidth: 120},
	}}
	group := ""
	for _, r := range t.rows() {
		st := r.setting
		if r.mode >= 0 {
			md := st.Modes[r.mode]
			note := md.Description
			if len(md.Alternatives) > 0 {
				note = strings.TrimSpace(note + " · or " + strings.Join(md.Alternatives, ", "))
			}
			tb.rows = append(tb.rows, []cell{c(""), cs("  "+md.Name, modeStyle), c(specValue(md.Primary)), c(""), cs(note, dim)})
			continue
		}
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
		value := c(settingValue(st))
		if len(st.Value) == 0 || string(st.Value) == "null" {
			value = cs("unset", dim)
		}
		tb.rows = append(tb.rows, []cell{cs(g, groupStyle), c(st.Label), value, cs(st.Source.Label, sourceStyle(st.Source.Layer)), cs(note, dim)})
	}
	out = append(out, "")
	return append(out, tb.render(width, h-len(out), &t.list)...)
}

func sourceStyle(layer string) lipgloss.Style {
	switch layer {
	case "default":
		return dim
	case "global":
		return globalStyle
	case "ancestor":
		return ancestorStyle
	}
	return accent
}

func onOff(b bool) string {
	if b {
		return "on"
	}
	return "off"
}
