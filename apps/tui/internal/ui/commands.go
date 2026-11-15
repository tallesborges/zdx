package ui

import (
	"errors"
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"strings"

	tea "charm.land/bubbletea/v2"

	"github.com/tallesborges/zdx/apps/shared/transcript"
	"github.com/tallesborges/zdx/apps/tui/internal/engine"
)

// openPicker shows p; with load, p shows loading until load's data comes
// back as a pickerMsg, which Update installs. load runs off the event loop
// and must not touch the model or p.
func (m *Model) openPicker(p *picker, load func() (pickerData, error)) tea.Cmd {
	m.overlay = p
	if load == nil {
		return nil
	}
	p.loading = true
	return func() tea.Msg {
		data, err := load()
		return pickerMsg{p: p, data: data, err: err}
	}
}

func (m *Model) openModelPicker() tea.Cmd {
	ctx, be, id := m.threadCtx, m.be, m.threadID
	return m.openPicker(&picker{title: "Model"}, func() (pickerData, error) {
		pk, err := be.Picker(ctx, id)
		if err != nil {
			return pickerData{}, err
		}
		if pk.ConfigError != nil {
			return pickerData{}, errors.New(*pk.ConfigError)
		}
		var items []pickItem
		for _, md := range pk.Modes {
			items = append(items, pickItem{label: "mode " + md.Name, value: md.Ref, current: pk.Override == md.Ref})
		}
		for _, md := range pk.Models {
			cur := !strings.HasPrefix(pk.Override, "mode:") && isCurrent(md, pk)
			items = append(items, pickItem{label: md.Name, detail: levels(md), value: md.ID, current: cur})
		}
		return pickerData{items: items, onPick: func(m *Model, value string) tea.Cmd {
			if strings.HasPrefix(value, "mode:") {
				return m.setModel(value)
			}
			for _, md := range pk.Models {
				if md.ID != value {
					continue
				}
				if len(md.Specs) > 1 {
					return m.levelPicker(md, pk.Thinking)
				}
				for _, spec := range md.Specs {
					return m.setModel(spec)
				}
				return m.setModel(md.ID)
			}
			return nil
		}}, nil
	})
}

// isCurrent reports the picker's current model: its id, or one of its
// per-level specs.
func isCurrent(md engine.PickerModel, pk engine.Picker) bool {
	if md.ID == pk.Model || md.Name == pk.Model {
		return true
	}
	for _, spec := range md.Specs {
		if spec == pk.Model {
			return true
		}
	}
	return false
}

func levels(md engine.PickerModel) string {
	if len(md.Specs) <= 1 {
		return ""
	}
	return strings.Join(levelNames(md), " ")
}

// levelOrder is the display order of thinking levels; unknown names sort
// last, by name.
var levelOrder = map[string]int{"off": 0, "minimal": 1, "low": 2, "medium": 3, "high": 4, "xhigh": 5, "max": 6}

func levelNames(md engine.PickerModel) []string {
	var names []string
	for name := range md.Specs {
		names = append(names, name)
	}
	rank := func(s string) int {
		if r, ok := levelOrder[strings.ToLower(s)]; ok {
			return r
		}
		return 100
	}
	for i := 1; i < len(names); i++ {
		for j := i; j > 0 && (rank(names[j]) < rank(names[j-1]) || rank(names[j]) == rank(names[j-1]) && names[j] < names[j-1]); j-- {
			names[j], names[j-1] = names[j-1], names[j]
		}
	}
	return names
}

func levelItems(md engine.PickerModel, current string) []pickItem {
	var items []pickItem
	for _, name := range levelNames(md) {
		items = append(items, pickItem{label: name, detail: md.Specs[name], value: md.Specs[name], current: strings.EqualFold(name, current)})
	}
	return items
}

func pickSpec(m *Model, value string) tea.Cmd { return m.setModel(value) }

func (m *Model) levelPicker(md engine.PickerModel, current string) tea.Cmd {
	p := &picker{title: "Thinking · " + md.Name}
	p.install(pickerData{items: levelItems(md, current), onPick: pickSpec}, nil)
	return m.openPicker(p, nil)
}

func (m *Model) openThinkingPicker() tea.Cmd {
	ctx, be, id := m.threadCtx, m.be, m.threadID
	return m.openPicker(&picker{title: "Thinking"}, func() (pickerData, error) {
		pk, err := be.Picker(ctx, id)
		if err != nil {
			return pickerData{}, err
		}
		for _, md := range pk.Models {
			if isCurrent(md, pk) && len(md.Specs) > 0 {
				return pickerData{title: "Thinking · " + md.Name, items: levelItems(md, pk.Thinking), onPick: pickSpec}, nil
			}
		}
		return pickerData{}, fmt.Errorf("the current model (%s) has no thinking levels to pick", pk.Label)
	})
}

// setModel stores a selection on the thread; before its first message it
// waits and goes with the first submit.
func (m *Model) setModel(spec string) tea.Cmd {
	if !m.saved {
		m.pendingSel = &spec
		m.setNotice("model "+spec+" (applies to the first message)", true)
		return m.loadStatus()
	}
	ctx, be, id := m.threadCtx, m.be, m.threadID
	return tea.Sequence(func() tea.Msg {
		u, err := be.UpdateThread(ctx, engine.Update{ThreadID: id, Model: ptrPtr(spec)})
		if err != nil {
			return noticeMsg{text: "model: " + errText(err)}
		}
		label := u.ModelLabel
		if label == "" {
			label = spec
		}
		return noticeMsg{text: "model " + label, ok: true}
	}, m.loadStatus())
}

func (m *Model) openThreadPicker() tea.Cmd {
	if m.busy() {
		m.setNotice("this thread has work in flight; wait or interrupt it first (esc)", false)
		return nil
	}
	ctx, be, cur := m.threadCtx, m.be, m.threadID
	return m.openPicker(&picker{title: "Threads (recent 50)"}, func() (pickerData, error) {
		list, err := be.Threads(ctx)
		if err != nil {
			return pickerData{}, err
		}
		var items []pickItem
		for _, t := range list {
			detail := ""
			if t.Project != nil {
				detail = *t.Project
			}
			if t.Age != nil {
				detail = strings.TrimSpace(detail + " · " + *t.Age)
			}
			label := t.Title
			if label == "" {
				label = t.ID
			}
			items = append(items, pickItem{label: label, detail: detail, value: t.ID, current: t.ID == cur})
		}
		return pickerData{items: items, onPick: func(m *Model, value string) tea.Cmd { return m.switchThread(value) }}, nil
	})
}

// command is one palette entry; typing `/name` runs it too.
type command struct {
	name, help string
	// idle: refused while the thread has work in flight.
	idle bool
	run  func(m *Model, arg string) tea.Cmd
}

func commands() []command {
	return []command{
		{"new", "start a new thread (ctrl+n)", true, func(m *Model, _ string) tea.Cmd { return m.switchThread("") }},
		{"threads", "open a recent thread (ctrl+p)", true, func(m *Model, _ string) tea.Cmd { return m.openThreadPicker() }},
		{"model", "pick this thread's model (ctrl+l)", false, func(m *Model, _ string) tea.Cmd { return m.openModelPicker() }},
		{"thinking", "pick the thinking level (ctrl+t)", false, func(m *Model, _ string) tea.Cmd { return m.openThinkingPicker() }},
		{"rename", "rename the thread: /rename TITLE", false, func(m *Model, arg string) tea.Cmd { return m.rename(arg) }},
		{"image", "attach an image: /image PATH", false, func(m *Model, arg string) tea.Cmd {
			path, ok := imagePath(arg)
			if !ok {
				m.setNotice("not an image file: "+arg, false)
				return nil
			}
			return m.attachImage(path)
		}},
		{"retry", "run the last message again", true, func(m *Model, _ string) tea.Cmd {
			if !m.saved {
				m.setNotice("nothing to retry yet", false)
				return nil
			}
			return m.rerun()
		}},
		{"copy", "copy the last reply (OSC 52)", false, func(m *Model, _ string) tea.Cmd { return m.copyLast() }},
		{"copy-id", "copy the thread id", false, func(m *Model, _ string) tea.Cmd {
			m.setNotice("copied "+m.threadID, true)
			return tea.SetClipboard(m.threadID)
		}},
		{"thoughts", "show or hide thinking blocks (alt+t)", false, func(m *Model, _ string) tea.Cmd {
			m.toggleThinking()
			return nil
		}},
		{"voice", "record a voice message (ctrl+space)", false, func(m *Model, _ string) tea.Cmd { return m.toggleVoice() }},
		{"login", "log in to a provider (zdx login)", false, func(m *Model, _ string) tea.Cmd { return m.execZdx("login") }},
		{"logout", "log out of a provider (zdx logout)", false, func(m *Model, _ string) tea.Cmd { return m.execZdx("logout") }},
		{"config", "edit the config in $EDITOR", false, func(m *Model, _ string) tea.Cmd { return m.editConfig() }},
		{"quit", "quit (ctrl+c)", false, func(m *Model, _ string) tea.Cmd { return m.quit() }},
	}
}

func (m *Model) runCommand(text string) (tea.Cmd, bool) {
	name, arg, _ := strings.Cut(strings.TrimPrefix(text, "/"), " ")
	for _, c := range commands() {
		if c.name != name {
			continue
		}
		if c.idle && m.busy() {
			m.setNotice("this thread has work in flight; wait or interrupt it first (esc)", false)
			return nil, true
		}
		return c.run(m, strings.TrimSpace(arg)), true
	}
	return nil, false
}

func (m *Model) openPalette() {
	p := &picker{title: "Commands"}
	var items []pickItem
	for _, c := range commands() {
		items = append(items, pickItem{label: "/" + c.name, detail: c.help, value: c.name})
	}
	p.install(pickerData{items: items, onPick: func(m *Model, value string) tea.Cmd {
		cmd, _ := m.runCommand("/" + value)
		return cmd
	}}, nil)
	m.overlay = p
}

// openFollowups lists the last reply's suggested replies; a pick goes to
// the composer.
func (m *Model) openFollowups() {
	if len(m.followups) == 0 {
		m.setNotice("no suggested replies", false)
		return
	}
	p := &picker{title: "Suggested replies"}
	var items []pickItem
	for _, f := range m.followups {
		items = append(items, pickItem{label: f, value: f})
	}
	p.install(pickerData{items: items, onPick: func(m *Model, value string) tea.Cmd {
		m.composer.SetValue(value)
		return nil
	}}, nil)
	m.overlay = p
}

func (m *Model) rename(name string) tea.Cmd {
	if name == "" {
		m.setNotice("usage: /rename TITLE", false)
		return nil
	}
	if !m.saved {
		m.setNotice("send a message first; the thread does not exist yet", false)
		return nil
	}
	ctx, be, id, gen := m.threadCtx, m.be, m.threadID, m.gen
	return func() tea.Msg {
		u, err := be.UpdateThread(ctx, engine.Update{ThreadID: id, Title: ptrPtr(name)})
		if err != nil {
			return noticeMsg{text: "rename: " + errText(err)}
		}
		if u.Title != "" {
			name = u.Title
		}
		return titledMsg{gen: gen, title: name}
	}
}

func (m *Model) copyLast() tea.Cmd {
	var acts []transcript.Activity
	if m.thread != nil {
		acts = append(acts, m.thread.Activity...)
	}
	for _, t := range m.turns {
		acts = append(acts, t.items...)
	}
	for i := len(acts) - 1; i >= 0; i-- {
		if a := acts[i]; a.Type == "message" && a.Role == "assistant" && a.Text != "" {
			m.setNotice("copied the last reply", true)
			return tea.SetClipboard(a.Text)
		}
	}
	m.setNotice("no reply to copy", false)
	return nil
}

func (m *Model) execZdx(sub string) tea.Cmd {
	if m.opts.Zdx == "" {
		m.setNotice("no zdx binary (set ZDX_BIN or --zdx)", false)
		return nil
	}
	cmd := exec.Command(m.opts.Zdx, sub)
	// The chat's home, not whatever the shell had.
	cmd.Env = append(os.Environ(), "ZDX_HOME="+m.opts.Home)
	return tea.ExecProcess(cmd, func(err error) tea.Msg { return execDoneMsg{what: "zdx " + sub, err: err} })
}

func (m *Model) editConfig() tea.Cmd {
	editor := os.Getenv("VISUAL")
	if editor == "" {
		editor = os.Getenv("EDITOR")
	}
	if editor == "" {
		editor = "vi"
	}
	fields := strings.Fields(editor)
	cmd := exec.Command(fields[0], append(fields[1:], filepath.Join(m.opts.Home, "config.toml"))...)
	return tea.ExecProcess(cmd, func(err error) tea.Msg { return execDoneMsg{what: "config", err: err} })
}
