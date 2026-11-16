package ui

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"strings"

	tea "charm.land/bubbletea/v2"

	"github.com/tallesborges/zdx/apps/monitor/internal/data"
	"github.com/tallesborges/zdx/apps/monitor/internal/source"
)

// ConfigWriter is what config edits need from the backend. Rust checks and
// writes every change; the monitor only picks the value.
type ConfigWriter interface {
	Picker(ctx context.Context, root string) (data.Picker, error)
	ChangeSetting(ctx context.Context, ch source.SettingChange) (data.ChangeOutcome, error)
}

type editStage int

const (
	stagePick editStage = iota
	stageLevel
	stageText
	stageConfirm
)

// configEdit is one edit in progress: pick a value, then confirm what Rust
// reports the write would change.
type configEdit struct {
	stage editStage
	gen   uint64
	scope configScope
	key   string
	label string
	// mode is the index of the mode whose primary is edited, or -1.
	mode  int
	modes []data.ModeView

	picker   *data.Picker
	loadErr  error
	filter   string
	list     listState
	levelsOf *data.PickerModel

	text string

	// value is the new value, nil for a reset; shown is its display.
	value    *string
	shown    string
	outcome  *data.ChangeOutcome
	checkErr error
	applying bool
}

type configPickerMsg struct {
	gen    uint64
	picker data.Picker
	err    error
}

type configChangeMsg struct {
	gen     uint64
	dryRun  bool
	outcome data.ChangeOutcome
	err     error
}

// pickEntry is one line of the value picker.
type pickEntry struct {
	label, detail, value string
	model                *data.PickerModel
}

func (m *Model) configWriter() ConfigWriter {
	w, _ := m.backend.(ConfigWriter)
	return w
}

// startConfigEdit opens the edit of the selected row, or resets it.
func (m *Model) startConfigEdit(reset bool) tea.Cmd {
	t := &m.config
	r, s := t.selectedRow(), t.scope()
	if r == nil || s == nil {
		return nil
	}
	if m.configWriter() == nil {
		t.notice, t.noticeBad = "this backend cannot write config", true
		return nil
	}
	st := r.setting
	m.configGen++
	e := &configEdit{gen: m.configGen, scope: *s, key: st.Key, label: st.Label, mode: r.mode, modes: st.Modes}
	t.edit = e
	if reset {
		e.shown = "reset (remove it from this file)"
		if st.Kind == "modes" {
			e.shown = "reset (remove every mode from this file)"
		}
		return m.checkChange(nil)
	}
	switch {
	case r.mode >= 0:
		e.label = "mode " + st.Modes[r.mode].Name + " primary"
		return m.loadPicker()
	case st.Kind == "modes":
		t.edit = nil
		t.notice, t.noticeBad = "pick a mode row to change its model", true
		return nil
	case st.Kind == "toggle":
		var cur string
		_ = json.Unmarshal(st.Value, &cur)
		next := "on"
		if cur == "on" {
			next = "off"
		}
		e.shown = next
		return m.checkChange(&next)
	case st.Kind == "transcription" || st.Kind == "speech":
		e.stage = stageText
		var cur string
		_ = json.Unmarshal(st.Value, &cur)
		e.text = cur
		return nil
	}
	return m.loadPicker()
}

func (m *Model) loadPicker() tea.Cmd {
	e, w := m.config.edit, m.configWriter()
	e.stage = stagePick
	root := e.scope.root
	if root == "" {
		root = m.opts.Root
	}
	ctx, gen := m.ctx, e.gen
	return func() tea.Msg {
		ctx, cancel := context.WithTimeout(ctx, source.CLITimeout)
		defer cancel()
		p, err := w.Picker(ctx, root)
		return configPickerMsg{gen: gen, picker: p, err: err}
	}
}

func (m *Model) checkChange(value *string) tea.Cmd {
	return m.change(value, true)
}

func (m *Model) change(value *string, dryRun bool) tea.Cmd {
	e, w := m.config.edit, m.configWriter()
	e.stage, e.value = stageConfirm, value
	if dryRun {
		e.outcome, e.checkErr = nil, nil
	} else {
		e.applying = true
	}
	ch := source.SettingChange{Key: e.key, Value: value, Workspace: e.scope.root, DryRun: dryRun}
	ctx, gen := m.ctx, e.gen
	return func() tea.Msg {
		ctx, cancel := context.WithTimeout(ctx, source.CLITimeout)
		defer cancel()
		out, err := w.ChangeSetting(ctx, ch)
		return configChangeMsg{gen: gen, dryRun: dryRun, outcome: out, err: err}
	}
}

func (m *Model) applyConfigPicker(msg configPickerMsg) {
	e := m.config.edit
	if e == nil || e.gen != msg.gen {
		return
	}
	if msg.err == nil && msg.picker.ConfigError != nil {
		msg.err = errors.New(*msg.picker.ConfigError)
	}
	e.picker, e.loadErr = &msg.picker, msg.err
}

func (m *Model) applyConfigChange(msg configChangeMsg) tea.Cmd {
	t := &m.config
	e := t.edit
	if e == nil || e.gen != msg.gen {
		return nil
	}
	if msg.dryRun {
		e.outcome, e.checkErr = &msg.outcome, msg.err
		return nil
	}
	e.applying = false
	if msg.err != nil {
		e.checkErr = msg.err
		return nil
	}
	t.edit = nil
	verb := "wrote"
	if !msg.outcome.Changed {
		verb = "no change to"
	}
	t.notice, t.noticeBad = fmt.Sprintf("%s %s in %s", verb, e.key, tildePath(msg.outcome.File)), false
	m.invalidate(keyWorkspaces)
	m.fetch[keySnapshot].force = true
	return m.refresh(m.now())
}

// entries are the picker lines that match the filter.
func (e *configEdit) entries() []pickEntry {
	var out []pickEntry
	if e.stage == stageLevel && e.levelsOf != nil {
		md := e.levelsOf
		for _, name := range []string{"off", "minimal", "low", "medium", "high", "xhigh", "max"} {
			if spec, ok := md.Specs[name]; ok {
				out = append(out, pickEntry{label: name, detail: spec, value: spec})
			}
		}
		return out
	}
	if e.picker == nil {
		return nil
	}
	f := strings.ToLower(e.filter)
	match := func(s string) bool { return f == "" || strings.Contains(strings.ToLower(s), f) }
	if e.mode < 0 {
		for _, md := range e.picker.Modes {
			if match(md.Ref) {
				out = append(out, pickEntry{label: md.Ref, detail: "model mode", value: md.Ref})
			}
		}
	}
	for i := range e.picker.Models {
		md := &e.picker.Models[i]
		if !match(md.ID) {
			continue
		}
		detail := "no thinking levels"
		if len(md.Specs) > 0 {
			detail = count(len(md.Specs), "level", "levels")
		}
		out = append(out, pickEntry{label: md.ID, detail: detail, value: md.ID, model: md})
	}
	return out
}

// picked turns a chosen spec into the value to write: the spec itself, or
// for a mode's primary the whole mode list with that primary replaced.
func (m *Model) picked(spec string) tea.Cmd {
	e := m.config.edit
	if e.mode < 0 {
		e.shown = spec
		return m.checkChange(&spec)
	}
	type mode struct {
		Name         string   `json:"name"`
		Description  string   `json:"description"`
		Primary      string   `json:"primary"`
		Alternatives []string `json:"alternatives"`
	}
	modes := make([]mode, len(e.modes))
	for i, md := range e.modes {
		modes[i] = mode{Name: md.Name, Description: md.Description, Primary: md.Primary, Alternatives: md.Alternatives}
		if modes[i].Alternatives == nil {
			modes[i].Alternatives = []string{}
		}
	}
	modes[e.mode].Primary = spec
	raw, err := json.Marshal(modes)
	if err != nil {
		e.stage, e.checkErr = stageConfirm, err
		return nil
	}
	value := string(raw)
	e.shown = spec
	return m.checkChange(&value)
}

// configEditKey handles every key while an edit is open.
func (m *Model) configEditKey(msg tea.KeyPressMsg) tea.Cmd {
	t := &m.config
	e := t.edit
	key := msg.String()
	if key == "esc" {
		if e.stage == stageLevel {
			e.stage, e.list = stagePick, listState{}
			return nil
		}
		t.edit = nil
		return nil
	}
	switch e.stage {
	case stagePick, stageLevel:
		entries := e.entries()
		switch key {
		case "enter":
			if e.list.selected >= len(entries) {
				return nil
			}
			en := entries[e.list.selected]
			if e.stage == stagePick && en.model != nil && len(en.model.Specs) > 0 {
				e.stage, e.levelsOf, e.list = stageLevel, en.model, listState{}
				return nil
			}
			return m.picked(en.value)
		case "up", "down", "pgup", "pgdown", "home", "end", "ctrl+u", "ctrl+d":
			e.list.key(key, len(entries), e.list.pageSize())
		case "backspace":
			if e.stage == stagePick && e.filter != "" {
				e.filter = e.filter[:len(e.filter)-1]
				e.list = listState{}
			}
		default:
			if e.stage == stagePick && len(msg.Text) > 0 {
				e.filter += msg.Text
				e.list = listState{}
			}
		}
	case stageText:
		switch key {
		case "enter":
			v := strings.TrimSpace(e.text)
			if v == "" {
				return nil
			}
			e.shown = v
			return m.checkChange(&v)
		case "backspace":
			if e.text != "" {
				r := []rune(e.text)
				e.text = string(r[:len(r)-1])
			}
		default:
			if len(msg.Text) > 0 {
				e.text += msg.Text
			}
		}
	case stageConfirm:
		if key == "enter" && !e.applying && e.checkErr == nil && e.outcome != nil {
			return m.change(e.value, false)
		}
	}
	return nil
}

func (m *Model) renderConfigEdit(width, h int) []string {
	e := m.config.edit
	out := []string{"", rule("Edit "+e.label, width)}
	switch e.stage {
	case stagePick, stageLevel:
		title := "Pick a model or a mode (type to filter)"
		if e.mode >= 0 {
			title = "Pick the mode's model (type to filter)"
		}
		if e.stage == stageLevel {
			title = "Thinking level for " + e.levelsOf.ID
		} else {
			title += "  " + accent.Render("/"+e.filter+"▏")
		}
		out = append(out, "  "+dim.Render(title))
		switch {
		case e.loadErr != nil:
			return append(out, "  "+bad.Render(oneLine(e.loadErr.Error())))
		case e.picker == nil:
			return append(out, "  "+dim.Render("loading models…"))
		}
		tb := table{cols: []col{{title: "VALUE", flex: true, min: 12}, {title: "", max: 24}}}
		for _, en := range e.entries() {
			tb.rows = append(tb.rows, []cell{c(specValue(en.label)), cs(en.detail, dim)})
		}
		if len(tb.rows) == 0 {
			return append(out, "  "+dim.Render("nothing matches"))
		}
		return append(out, tb.render(width, h-len(out), &e.list)...)
	case stageText:
		return append(out, "  "+dim.Render("Model id (enter check · esc cancel)"), "  "+accent.Render(e.text+"▏"))
	}
	out = append(out, kv("Change", specValue(e.shown)), kv("File", tildePath(e.scope.file)))
	switch {
	case e.checkErr != nil:
		return append(out, kv("Refused", bad.Render(oneLine(e.checkErr.Error()))), "", "  "+dim.Render("esc back"))
	case e.outcome == nil:
		return append(out, "  "+dim.Render("checking…"))
	}
	o := e.outcome
	if !o.Changed {
		out = append(out, kv("Result", dim.Render("the file already says this")))
	}
	if len(o.Reach.Reads) > 0 {
		out = append(out, kv("Applies to", strings.Join(o.Reach.Reads, ", ")))
	}
	for _, sk := range o.Reach.Skips {
		out = append(out, kv("Not to", sk.Workspace+dim.Render(" (set in "+sk.By+")")))
	}
	if len(o.Reach.Unknown) > 0 {
		out = append(out, kv("Unknown", warn.Render(strings.Join(o.Reach.Unknown, ", ")+" (config does not load)")))
	}
	action := "enter write · esc cancel"
	if e.applying {
		action = "writing…"
	}
	return append(out, "", "  "+accent.Render(action))
}
