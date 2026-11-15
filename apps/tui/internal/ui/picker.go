package ui

import (
	"strings"

	tea "charm.land/bubbletea/v2"
	"github.com/charmbracelet/x/ansi"

	"github.com/tallesborges/zdx/apps/shared/transcript"
)

// pickItem is one row of a picker: what it shows and what a pick returns.
type pickItem struct {
	label  string
	detail string
	value  string
	// current marks the active choice.
	current bool
}

// pickerData is what a picker shows and does: built off the event loop,
// installed by Update.
type pickerData struct {
	// title replaces the picker's title when set.
	title  string
	items  []pickItem
	onPick func(m *Model, value string) tea.Cmd
}

// install shows data (or err) and selects the current item.
func (p *picker) install(data pickerData, err error) {
	p.loading = false
	if err != nil {
		p.err = errText(err)
		return
	}
	if data.title != "" {
		p.title = data.title
	}
	p.items, p.onPick = data.items, data.onPick
	p.selectCurrent()
}

// picker is a filterable list overlay. Typing filters by every word of
// the query; enter picks, esc closes.
type picker struct {
	title  string
	items  []pickItem
	query  string
	sel    int
	offset int
	// onPick turns the picked value into the next step.
	onPick func(m *Model, value string) tea.Cmd
	// err replaces the list while loading failed.
	err     string
	loading bool
}

func (p *picker) visible() []int {
	words := strings.Fields(strings.ToLower(p.query))
	var out []int
	for i, it := range p.items {
		hay := strings.ToLower(it.label + " " + it.detail + " " + it.value)
		match := true
		for _, w := range words {
			if !strings.Contains(hay, w) {
				match = false
				break
			}
		}
		if match {
			out = append(out, i)
		}
	}
	return out
}

func (p *picker) move(d int) {
	n := len(p.visible())
	if n == 0 {
		p.sel = 0
		return
	}
	p.sel = (p.sel + d + n) % n
}

// picked is the selected item, or nil.
func (p *picker) picked() *pickItem {
	vis := p.visible()
	if p.sel < 0 || p.sel >= len(vis) {
		return nil
	}
	return &p.items[vis[p.sel]]
}

// selectCurrent moves the selection to the current item.
func (p *picker) selectCurrent() {
	for i, idx := range p.visible() {
		if p.items[idx].current {
			p.sel = i
			return
		}
	}
}

func (p *picker) render(width, rows int) []string {
	out := []string{
		" " + transcript.Bold.Render(p.title) + transcript.Dim.Render("  type to filter · ↑↓ select · enter pick · esc close"),
		" " + transcript.Accent.Render("› ") + p.query,
		"",
	}
	rows -= len(out)
	switch {
	case p.loading:
		return append(out, transcript.Dim.Render(" loading…"))
	case p.err != "":
		for _, l := range transcript.WrapText(p.err, width-2) {
			out = append(out, " "+transcript.Bad.Render(l))
		}
		return out
	}
	vis := p.visible()
	if len(vis) == 0 {
		return append(out, transcript.Dim.Render(" no match"))
	}
	if p.sel >= len(vis) {
		p.sel = len(vis) - 1
	}
	rows = max(rows, 1)
	if p.sel < p.offset {
		p.offset = p.sel
	}
	if p.sel >= p.offset+rows {
		p.offset = p.sel - rows + 1
	}
	p.offset = min(p.offset, max(len(vis)-rows, 0))
	for i := p.offset; i < min(p.offset+rows, len(vis)); i++ {
		it := p.items[vis[i]]
		mark := "  "
		if it.current {
			mark = transcript.OK.Render("● ")
		}
		line := mark + transcript.OneLine(it.label)
		if it.detail != "" {
			line += "  " + transcript.Dim.Render(transcript.OneLine(it.detail))
		}
		if i == p.sel {
			plain := ansi.Strip(line)
			line = selected.Render(" " + pad(plain, width-1))
		} else {
			line = " " + line
		}
		out = append(out, transcript.Fit(line, width))
	}
	return out
}

func pad(s string, width int) string {
	s = ansi.Truncate(s, width, "…")
	if w := ansi.StringWidth(s); w < width {
		s += strings.Repeat(" ", width-w)
	}
	return s
}

func (m *Model) overlayKey(msg tea.KeyPressMsg) tea.Cmd {
	p := m.overlay
	switch msg.String() {
	case "esc", "ctrl+c":
		m.overlay = nil
	case "up", "ctrl+p":
		p.move(-1)
	case "down", "ctrl+n", "tab":
		p.move(1)
	case "enter":
		it := p.picked()
		if it == nil || p.onPick == nil {
			return nil
		}
		m.overlay = nil
		return p.onPick(m, it.value)
	case "backspace":
		if p.query != "" {
			r := []rune(p.query)
			p.query = string(r[:len(r)-1])
			p.sel = 0
		}
	default:
		if t := msg.Key().Text; t != "" {
			p.query += t
			p.sel = 0
		}
	}
	return nil
}
