package ui

import (
	"time"

	"charm.land/lipgloss/v2"
	"fmt"
	"github.com/charmbracelet/x/ansi"
	"strings"

	tea "charm.land/bubbletea/v2"

	"github.com/tallesborges/zdx/apps/monitor/internal/source"
)

const defaultLogLines = 2000

var logLevels = []string{"TRACE", "DEBUG", "INFO", "WARN", "ERROR"}

type logsTab struct {
	files     []string
	fileIndex int
	file      string
	lines     []string
	levels    []int // level index per line; a continuation line takes the previous line's level
	visible   []int
	list      listState
	follow    bool
	minLevel  int // 0 = all
	query     string
	editing   bool
	editBuf   string
	stamp     source.LogStamp
	maxLines  int
	truncated bool
	loaded    bool
}

// lineLevel reads the level of a `tracing` compact line
// (`2026-11-14T03:15:27.743524Z  INFO target: message`), or -1.
func lineLevel(line string) int {
	fields := strings.Fields(line)
	if len(fields) < 2 {
		return -1
	}
	for i, l := range logLevels {
		if fields[1] == l {
			return i
		}
	}
	return -1
}

func (t *logsTab) apply(v source.LogTail) {
	t.loaded = true
	t.files = v.Files
	t.file = v.File
	if v.Unchanged {
		return
	}
	t.stamp = v.Stamp
	t.lines = v.Lines
	t.truncated = v.Truncated
	t.levels = make([]int, len(v.Lines))
	prev := 0
	for i, l := range v.Lines {
		if lv := lineLevel(l); lv >= 0 {
			prev = lv
		}
		t.levels[i] = prev
	}
	t.recompute()
}

// recompute filters lines by level and query, keeping the selected line
// (or the bottom, while following).
func (t *logsTab) recompute() {
	keep := -1
	if !t.follow && t.list.selected < len(t.visible) {
		keep = t.visible[t.list.selected]
	}
	t.visible = t.visible[:0]
	q := strings.ToLower(t.query)
	for i, l := range t.lines {
		if t.minLevel > 0 && t.levels[i] < t.minLevel {
			continue
		}
		if q != "" && !strings.Contains(strings.ToLower(l), q) {
			continue
		}
		t.visible = append(t.visible, i)
	}
	switch {
	case t.follow:
		t.list.selected = max(len(t.visible)-1, 0)
	case keep >= 0:
		t.list.selected = 0
		for j, i := range t.visible {
			if i <= keep {
				t.list.selected = j
			}
		}
	}
}

func (m *Model) logsKey(key string) tea.Cmd {
	t := &m.logs
	switch key {
	case "l":
		t.minLevel = (t.minLevel + 1) % len(logLevels)
		t.recompute()
		return nil
	case "/":
		t.editing, t.editBuf = true, t.query
		return nil
	case "esc":
		t.minLevel, t.query = 0, ""
		t.recompute()
		return nil
	case "[", "]":
		next := t.fileIndex + 1 // older
		if key == "]" {
			next = t.fileIndex - 1
		}
		if next < 0 || next >= len(t.files) {
			return nil
		}
		t.fileIndex, t.stamp, t.follow = next, source.LogStamp{}, true
		m.invalidate(keyLogs)
		return m.refresh(m.now())
	case "end", "G":
		t.follow = true
		t.list.selected = max(len(t.visible)-1, 0)
		return nil
	}
	if t.list.key(key, len(t.visible), t.list.pageSize()) {
		t.follow = t.list.selected == len(t.visible)-1
	}
	return nil
}

func (t *logsTab) editKey(msg tea.KeyPressMsg) tea.Cmd {
	switch msg.String() {
	case "enter":
		t.editing, t.query = false, t.editBuf
		t.recompute()
	case "esc":
		t.editing = false
	case "backspace":
		if r := []rune(t.editBuf); len(r) > 0 {
			t.editBuf = string(r[:len(r)-1])
		}
	default:
		if msg.Text != "" {
			t.editBuf += msg.Text
		}
	}
	return nil
}

var levelStyles = []func(...string) string{dim.Render, dim.Render, ok.Render, warn.Render, bad.Render}

// logLine shows a tracing line as local time, level, and the rest; other
// lines (continuations) are indented under the message.
func logLine(line string, level int) string {
	fields := strings.Fields(line)
	if len(fields) >= 2 && lineLevel(line) >= 0 {
		ts := fields[0]
		if t, err := time.Parse(time.RFC3339Nano, ts); err == nil {
			ts = t.Local().Format("15:04:05")
		}
		rest := strings.TrimLeft(line[strings.Index(line, fields[1])+len(fields[1]):], " ")
		body := rest
		if i := strings.Index(rest, "::"); i >= 0 {
			if j := strings.Index(rest[i:], ": "); j >= 0 {
				body = dim.Render(rest[:i+j+1]) + rest[i+j+1:]
			}
		}
		if level >= 3 {
			body = levelStyles[level](rest)
		}
		return dim.Render(ts) + " " + levelStyles[lineLevel(line)](pad(fields[1], 5)) + " " + body
	}
	return strings.Repeat(" ", 15) + levelStyles[max(level, 0)](strings.TrimLeft(line, " "))
}

func (m *Model) renderLogs(width, h int) []string {
	t := &m.logs
	var meta []string
	if t.file != "" {
		meta = append(meta, t.file)
		if len(t.files) > 1 {
			meta[0] += fmt.Sprintf(" (%d of %d)", t.fileIndex+1, len(t.files))
		}
	}
	filter := "all levels"
	if t.minLevel > 0 {
		filter = logLevels[t.minLevel] + " and above"
	}
	meta = append(meta, filter)
	if t.query != "" {
		meta = append(meta, fmt.Sprintf("matching %q", t.query))
	}
	meta = append(meta, fmt.Sprintf("%d of %d lines", len(t.visible), len(t.lines)))
	if t.truncated {
		meta = append(meta, fmt.Sprintf("last %d only", t.maxLines))
	}
	title := sectionTitle("Logs", strings.Join(meta, " · "))
	if t.follow && t.loaded {
		title += "  " + ok.Render("● following")
	}
	out := []string{title}
	if t.editing {
		out = append(out, "  "+accent.Render("/"+t.editBuf+"▏")+dim.Render("  enter apply · esc cancel"))
	}
	if !t.loaded {
		return append(out, m.pending(keyLogs, "logs")...)
	}
	if len(t.files) == 0 {
		return append(out, emptyState("No zdx.log files in "+tildePath(source.LogDir(m.opts.Home))+".")...)
	}
	if len(t.visible) == 0 {
		return append(out, emptyState("No line matches the filter. esc clears it.")...)
	}
	start, end := t.list.window(len(t.visible), h-len(out))
	for j := start; j < end; j++ {
		i := t.visible[j]
		line := logLine(t.lines[i], t.levels[i])
		if j == t.list.selected && !t.follow {
			line = selectedLine(ansi.Strip(logLine(t.lines[i], t.levels[i])), width)
		}
		out = append(out, " "+fit(line, width-1))
	}
	return out
}

// selectedLine is a plain line on the selection background.
func selectedLine(s string, width int) string {
	return lipgloss.NewStyle().Background(selBg).Render(pad(s, width-1))
}
