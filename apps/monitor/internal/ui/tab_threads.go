package ui

import (
	"fmt"
	"strings"

	"github.com/tallesborges/zdx/apps/monitor/internal/data"
)

type threadsTab struct {
	kind string // "all" or "workers"
	data *data.ThreadList
	list listState
}

func (t *threadsTab) ids() []string {
	if t.data == nil {
		return nil
	}
	ids := make([]string, len(t.data.Threads))
	for i, th := range t.data.Threads {
		ids[i] = th.ID
	}
	return ids
}

func (t *threadsTab) set(v data.ThreadList) {
	t.data = &v
	t.list.sync(t.ids())
}

func (t *threadsTab) selectedThread() *data.ThreadListItem {
	if t.data != nil && t.list.selected < len(t.data.Threads) {
		return &t.data.Threads[t.list.selected]
	}
	return nil
}

// workersText is a thread's worker roll-up; unknown workers stay unknown.
func workersText(th data.ThreadListItem) cell {
	if th.IsWorker {
		return cs("worker", accent)
	}
	r := th.Workers
	if r == nil {
		return c("")
	}
	parts := []string{}
	for _, p := range []struct {
		n    int
		what string
	}{{r.Running, "running"}, {r.Queued, "queued"}, {r.Failed, "failed"}, {r.Unknown, "unknown"}} {
		if p.n > 0 {
			parts = append(parts, fmt.Sprintf("%d %s", p.n, p.what))
		}
	}
	text := count(r.Total, "worker", "workers")
	if len(parts) > 0 {
		text += ": " + strings.Join(parts, ", ")
	}
	style := dim
	switch {
	case r.Failed > 0:
		style = bad
	case r.Running > 0 || r.Queued > 0:
		style = ok
	}
	return cs(text, style)
}

func (m *Model) renderThreads(width, h int) []string {
	t := &m.threads
	kind := "all threads"
	if t.kind == "workers" {
		kind = "workers only"
	}
	if t.data == nil {
		return append([]string{sectionTitle("Threads", kind)}, m.pending(keyThreads, "threads")...)
	}
	out := []string{sectionTitle("Threads", fmt.Sprintf("%s · latest %d (recent list, no full browse yet)", kind, len(t.data.Threads)))}
	if len(t.data.Threads) == 0 {
		return append(out, emptyState("No threads.")...)
	}
	tb := table{cols: []col{
		{title: "", max: 1},
		{title: "AGE", max: 6, right: true},
		{title: "PROJECT", max: 16, minWidth: 90},
		{title: "TITLE", flex: true, min: 16},
		{title: "WORKERS", max: 44, minWidth: 110},
	}}
	now := m.now()
	for _, th := range t.data.Threads {
		mark := c(" ")
		if th.Run != nil {
			mark = cs(spinner(now), runStyle(th.Run.State).style)
		}
		title := c(th.Title)
		if th.Title == th.ID {
			title = cs(th.Title, dim)
		}
		tb.rows = append(tb.rows, []cell{mark, cs(orDash(compactAge(str(th.Age))), dim), cs(orDash(str(th.Project)), dim), title, workersText(th)})
	}
	var detail []string
	if th := t.selectedThread(); th != nil {
		detail = threadDetail(*th, width)
	}
	return listWithDetail(out, width, h, &tb, &t.list, detail, "Selected thread")
}

func threadDetail(th data.ThreadListItem, width int) []string {
	var lines []string
	for i, l := range wrapText(th.Title, width-14) {
		if i == 2 {
			break
		}
		label := ""
		if i == 0 {
			label = "Title"
		}
		lines = append(lines, kv(label, bold.Render(l)))
	}
	lines = append(lines, kv("Thread", th.ID))
	if th.Age != nil {
		lines = append(lines, kv("Last write", *th.Age+" ago"))
	}
	if th.RootPath != nil {
		lines = append(lines, kv("Root", *th.RootPath))
	}
	if w := workersText(th); w.text != "" {
		lines = append(lines, kv("Workers", w.style.Render(w.text)))
	}
	if th.Run != nil {
		lines = append(lines, kv("Running", runStyle(th.Run.State).style.Render(th.Run.State)+"  "+th.Run.Label))
	}
	if th.TelegramLink != nil {
		lines = append(lines, kv("Telegram", dim.Render(*th.TelegramLink)))
	}
	for i := range lines {
		lines[i] = fit(lines[i], width)
	}
	return lines
}
