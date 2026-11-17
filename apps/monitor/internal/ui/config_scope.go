package ui

import (
	"strings"

	tea "charm.land/bubbletea/v2"
)

// scopePicker is the Config tab's scope list with a typed filter (`/`):
// the global config and each workspace, matched by name, path, or the
// file an edit writes.
type scopePicker struct {
	filter string
	list   listState
}

// matches are the indexes of the scopes the filter keeps, in order.
func (t *configTab) scopeMatches() []int {
	f := strings.ToLower(strings.TrimSpace(t.pick.filter))
	var out []int
	for i, s := range t.scopes {
		text := strings.ToLower(s.name + " " + s.root + " " + s.file)
		if s.global {
			text += " global"
		}
		if f == "" || strings.Contains(text, f) {
			out = append(out, i)
		}
	}
	return out
}

func (t *configTab) openScopePicker() {
	if len(t.scopes) == 0 {
		return
	}
	t.pick = &scopePicker{}
	t.pick.list.selected = t.index
}

func (t *configTab) scopeKey(msg tea.KeyPressMsg) tea.Cmd {
	p := t.pick
	matches := t.scopeMatches()
	switch key := msg.String(); key {
	case "esc":
		t.pick = nil
	case "enter":
		if p.list.selected < len(matches) {
			if i := matches[p.list.selected]; i != t.index {
				t.index, t.list = i, listState{}
			}
			t.pick = nil
		}
	case "up", "down", "pgup", "pgdown", "home", "end", "ctrl+u", "ctrl+d":
		p.list.key(key, len(matches), p.list.pageSize())
	case "backspace":
		if r := []rune(p.filter); len(r) > 0 {
			p.filter = string(r[:len(r)-1])
			p.list = listState{}
		}
	default:
		if msg.Text != "" {
			p.filter += msg.Text
			p.list = listState{}
		}
	}
	return nil
}

func (m *Model) renderScopePicker(width, h int) []string {
	t := &m.config
	out := []string{"", "  " + accent.Render("/"+t.pick.filter+"▏") + dim.Render("  type to filter · ↑↓ select · enter switch · esc cancel")}
	matches := t.scopeMatches()
	if len(matches) == 0 {
		return append(out, "  "+dim.Render("no scope matches"))
	}
	tb := table{cols: []col{
		{title: "SCOPE", max: 24},
		{title: "PATH", flex: true, min: 12, cutLeft: true},
		{title: "", max: 10},
	}}
	for _, i := range matches {
		s := t.scopes[i]
		name, path, mark := cs(s.name, bold), cs(tildePath(s.root), dim), c("")
		if s.global {
			name, path = cs("global", globalStyle), cs(tildePath(s.file), dim)
		}
		switch {
		case i == t.index:
			mark = cs("shown", accent)
		case s.current:
			mark = cs("current", ok)
		}
		tb.rows = append(tb.rows, []cell{name, path, mark})
	}
	return append(out, tb.render(width, h-len(out), &t.pick.list)...)
}
