package ui

import (
	"fmt"
	"strings"
	"time"

	"github.com/tallesborges/zdx/apps/monitor/internal/data"
	"github.com/tallesborges/zdx/apps/monitor/internal/wire"
)

func serviceIDs(s []data.ServiceState) []string {
	ids := make([]string, len(s))
	for i, v := range s {
		ids[i] = v.Service
	}
	return ids
}

func backgroundIDs(p []data.BackgroundProcess) []string {
	ids := make([]string, len(p))
	for i, v := range p {
		ids[i] = v.ID
	}
	return ids
}

func serviceState(s data.ServiceState) (cell, cell) {
	switch {
	case s.Status == "running":
		return cs("●", ok), cs("running", ok)
	case !s.Installed:
		return cs("○", dim), cs(s.Status, dim)
	}
	return cs("●", bad), cs(s.Status, bad)
}

// serviceAlarm: an installed service that is not running.
func serviceAlarm(s data.ServiceState) bool { return s.Installed && s.Status != "running" }

func (m *Model) renderServices(width, h int) []string {
	out := []string{sectionTitle("Services", "launchd · read only")}
	if m.services == nil {
		out = append(out, m.pending(keyServices, "services")...)
	} else {
		tb := table{cols: []col{
			{title: "", max: 1},
			{title: "SERVICE", max: 14},
			{title: "STATUS", max: 9},
			{title: "PID", max: 7, right: true},
			{title: "UPTIME", max: 10, right: true},
			{title: "LAUNCHD", max: 13},
			{title: "LABEL", max: 24, minWidth: 90},
		}}
		for _, s := range m.services.Services {
			mark, state := serviceState(s)
			pid, up := cs("—", dim), cs("—", dim)
			if s.PID != nil {
				pid = c(fmt.Sprint(*s.PID))
			}
			if s.UptimeSecs != nil {
				up = c(span(time.Duration(*s.UptimeSecs) * time.Second))
			}
			installed := c("installed")
			if !s.Installed {
				installed = cs("not installed", dim)
			}
			tb.rows = append(tb.rows, []cell{mark, c(s.Service), state, pid, up, installed, cs(s.Label, dim)})
		}
		out = append(out, tb.render(width, len(tb.rows)+1, &m.svcList)...)
		if q := strings.TrimSpace(string(m.services.QueuedRestart)); q != "" && q != "null" {
			out = append(out, "  "+warn.Render("Restart queued: ")+dim.Render(oneLine(q)))
		}
	}
	out = append(out, "", rule("Engine work", width))
	return append(out, m.engineWork(width, h-len(out))...)
}

// engineWork lists the threads the service holds work for, from its
// status report.
func (m *Model) engineWork(width, h int) []string {
	if m.status == nil {
		if err := m.fetch[keyStatus].err; err != nil {
			return []string{"  " + bad.Render("Not reachable: ") + oneLine(err.Error())}
		}
		return []string{"  " + dim.Render("Loading…")}
	}
	tb := table{cols: []col{
		{title: "STATE", max: 10},
		{title: "THREAD", flex: true, min: 16, cutLeft: true},
		{title: "TURN", max: 5, right: true},
		{title: "QUEUED", max: 6, right: true},
		{title: "WORKERS", max: 34, minWidth: 100},
		{title: "PROFILE", max: 14, minWidth: 130},
	}}
	idle := 0
	for _, t := range m.status.Threads {
		if t.State == "idle" && t.QueuedInputs == 0 && len(t.Workers) == 0 {
			idle++
			continue
		}
		turn := cs("—", dim)
		if t.RunningTurn != nil {
			turn = c(fmt.Sprint(*t.RunningTurn))
		}
		state := cs(t.State, dim)
		switch t.State {
		case "running":
			state = cs(t.State, ok)
		case "queued", "delivering":
			state = cs(t.State, warn)
		}
		queued := cs("0", dim)
		if t.QueuedInputs > 0 {
			queued = c(fmt.Sprint(t.QueuedInputs))
		}
		tb.rows = append(tb.rows, []cell{state, c(t.ThreadID), turn, queued, workerCounts(t.Workers), cs(orDash(str(t.Profile)), dim)})
	}
	var out []string
	if len(tb.rows) == 0 {
		out = append(out, "  "+dim.Render(fmt.Sprintf("Nothing in flight (%s idle).", count(idle, "thread", "threads"))))
	} else {
		out = append(out, tb.render(width, h-1, nil)...)
		if idle > 0 {
			out = append(out, "  "+dim.Render(fmt.Sprintf("+ %s idle", count(idle, "thread", "threads"))))
		}
	}
	return out
}

func workerCounts(ws []wire.WorkerSnapshot) cell {
	if len(ws) == 0 {
		return cs("—", dim)
	}
	counts := map[string]int{}
	for _, w := range ws {
		counts[w.Status]++
	}
	var parts []string
	for _, s := range []string{"running", "queued", "failed", "completed", "cancelled"} {
		if counts[s] > 0 {
			parts = append(parts, fmt.Sprintf("%d %s", counts[s], s))
			delete(counts, s)
		}
	}
	for s, n := range counts {
		parts = append(parts, fmt.Sprintf("%d %s", n, s))
	}
	style := plainStyle
	if strings.Contains(strings.Join(parts, " "), "failed") {
		style = bad
	}
	return cs(strings.Join(parts, ", "), style)
}

func (m *Model) renderBackground(width, h int) []string {
	if m.snapshot == nil {
		return append([]string{sectionTitle("Background processes", "")}, m.pending(keySnapshot, "background processes")...)
	}
	p := m.snapshot.BackgroundProcesses
	out := []string{sectionTitle("Background processes", count(len(p), "running", "running"))}
	if len(p) == 0 {
		return append(out, emptyState("No background process is running.")...)
	}
	tb := table{cols: []col{
		{title: "ID", max: 10},
		{title: "PID", max: 7, right: true},
		{title: "UP", max: 6, right: true},
		{title: "COMMAND", flex: true, min: 20},
		{title: "THREAD", max: 38, minWidth: 130, cutLeft: true},
	}}
	for _, v := range p {
		tb.rows = append(tb.rows, []cell{cs(v.ID, dim), c(fmt.Sprint(v.PID)), cs(compactAge(v.Uptime), dim), c(oneLine(v.Command)), cs(orDash(str(v.ThreadID)), dim)})
	}
	var detail []string
	if i := m.background.selected; i >= 0 && i < len(p) {
		v := p[i]
		for j, l := range wrapText(v.Command, width-14) {
			if j == 3 {
				break
			}
			label := ""
			if j == 0 {
				label = "Command"
			}
			detail = append(detail, kv(label, l))
		}
		detail = append(detail, kv("Thread", orDash(str(v.ThreadID))), kv("Process", fmt.Sprintf("pid %d · up %s", v.PID, v.Uptime)))
	}
	return listWithDetail(out, width, h, &tb, &m.background, detail, "Selected process")
}
