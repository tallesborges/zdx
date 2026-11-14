package ui

import (
	"fmt"
	"strings"
	"time"

	"github.com/tallesborges/zdx/apps/monitor/internal/data"
)

type jobsTab struct {
	data *data.DaemonJobs
	list listState
}

func (t *jobsTab) count() int {
	if t.data == nil {
		return 0
	}
	return len(t.data.Jobs)
}

func (t *jobsTab) ids() []string {
	ids := make([]string, t.count())
	for i := range ids {
		ids[i] = t.data.Jobs[i].Name
	}
	return ids
}

func (t *jobsTab) set(v data.DaemonJobs) {
	t.data = &v
	t.list.sync(t.ids())
}

// alarms counts the rows the reader must act on.
func (t *jobsTab) alarms() int {
	n := 0
	if t.data != nil {
		for _, j := range t.data.Jobs {
			if jobAlarm(j) {
				n++
			}
		}
	}
	return n
}

func scheduleText(s *data.JobSchedule) string {
	if s == nil {
		return "—"
	}
	switch s.Kind {
	case "recurring":
		return str(s.Cron)
	case "once":
		return "once"
	}
	return s.Kind
}

// localTime shows an RFC 3339 instant in local time, or the text as given
// when it does not parse.
func localTime(s string) string {
	t, err := time.Parse(time.RFC3339, s)
	if err != nil {
		return s
	}
	return t.Local().Format("Mon 2 Jan 15:04")
}

// jobAlarm marks rows the reader must act on: the engine's invalid,
// expired, and interrupted states, and a last run that did not succeed.
func jobAlarm(j data.JobOverview) bool {
	switch j.Status {
	case "invalid", "expired", "interrupted":
		return true
	}
	return j.LastRun != nil && j.LastRun.Outcome != "succeeded"
}

func jobStatus(j data.JobOverview) cell {
	switch j.Status {
	case "invalid", "expired", "interrupted":
		return cs(j.Status, bad)
	case "running", "due":
		return cs(j.Status, accent)
	case "scheduled", "pending":
		return c(j.Status)
	}
	return cs(j.Status, dim)
}

func lastRun(r *data.LastRun, now time.Time) cell {
	if r == nil {
		return cs("never", dim)
	}
	if r.Outcome == "succeeded" {
		return cs("✓ "+relShort(r.FinishedAt, now), ok)
	}
	return cs("✗ "+r.Outcome+" "+relShort(r.FinishedAt, now), bad)
}

func (m *Model) renderJobs(width, h int) []string {
	t := &m.jobs
	if t.data == nil {
		return append([]string{sectionTitle("Jobs", "")}, m.pending(keyJobs, "jobs")...)
	}
	meta := ""
	d := t.data.Daemon
	switch {
	case d.Context != nil:
		meta = fmt.Sprintf("resolved as the %s daemon does · root %s", d.Context.Source, tildePath(d.Context.Root))
	case d.Reason != nil:
		meta = "daemon " + d.Status + ": " + *d.Reason
	}
	out := []string{sectionTitle("Jobs", meta)}
	if t.data.Error != nil {
		out = append(out, "  "+bad.Render(oneLine(*t.data.Error)))
	}
	if len(t.data.Jobs) == 0 {
		return append(out, emptyState("No jobs in $ZDX_HOME/jobs.")...)
	}
	now := m.now()
	tb := table{cols: []col{
		{title: "NAME", max: 26},
		{title: "STATUS", max: 11},
		{title: "SCHEDULE", max: 14, minWidth: 80},
		{title: "NEXT", max: 9},
		{title: "LAST RUN", max: 24, minWidth: 70},
		{title: "MODEL", flex: true, min: 12, minWidth: 100, cutLeft: true},
	}}
	for _, j := range t.data.Jobs {
		next := cs("—", dim)
		if j.NextRunAt != nil {
			next = c(relShort(*j.NextRunAt, now))
		}
		model := cs(orDash(str(j.DeclaredModel)), dim)
		if s := j.Settings; s != nil {
			model = c(s.Model.Spec)
		}
		tb.rows = append(tb.rows, []cell{c(j.Name), jobStatus(j), cs(scheduleText(j.Schedule), dim), next, lastRun(j.LastRun, now), model})
	}
	var detail []string
	if i := t.list.selected; i < len(t.data.Jobs) {
		detail = jobDetail(t.data.Jobs[i], width, now)
	}
	return listWithDetail(out, width, h, &tb, &t.list, detail, "Selected job")
}

func jobDetail(j data.JobOverview, width int, now time.Time) []string {
	var sched []string
	sched = append(sched, scheduleText(j.Schedule))
	if j.Schedule != nil && j.Schedule.RunAt != nil {
		sched[0] = "once at " + when(*j.Schedule.RunAt, now)
	}
	if j.GraceSecs != nil {
		sched = append(sched, "grace "+span(time.Duration(*j.GraceSecs)*time.Second))
	}
	if j.TimeoutSecs != nil {
		sched = append(sched, "timeout "+span(time.Duration(*j.TimeoutSecs)*time.Second))
	}
	sched = append(sched, fmt.Sprintf("%d retries", j.MaxRetries))
	lines := []string{
		kv("File", tildePath(j.Path)),
		kv("Schedule", strings.Join(sched, dim.Render(" · "))),
	}
	status := jobStatus(j)
	st := status.style.Render(status.text)
	if j.NextRunAt != nil {
		st += dim.Render(" · next ") + when(*j.NextRunAt, now)
	}
	lines = append(lines, kv("Status", st))
	if r := j.LastRun; r != nil {
		line := lastRun(r, now)
		text := line.style.Render(r.Outcome) + dim.Render(" · "+r.Trigger+" · ") + when(r.FinishedAt, now)
		if r.ThreadID != nil {
			text += dim.Render(" · thread " + *r.ThreadID)
		}
		lines = append(lines, kv("Last run", text))
		if r.Error != nil {
			lines = append(lines, kv("", bad.Render(oneLine(*r.Error))))
		}
	} else {
		lines = append(lines, kv("Last run", dim.Render("never")))
	}
	if s := j.Settings; s != nil {
		lines = append(lines,
			kv("Model", s.Model.Spec+dim.Render(" from "+s.Model.Origin+" · thinking "+s.Model.Thinking+" from "+s.Model.ThinkingOrigin)),
			kv("Root", tildePath(s.Root.Path)+dim.Render(" from "+s.Root.Origin)))
	}
	var declared []string
	for _, d := range []struct {
		k string
		v *string
	}{{"model", j.DeclaredModel}, {"root", j.DeclaredRoot}, {"subagent", j.Subagent}} {
		if d.v != nil {
			declared = append(declared, d.k+" "+*d.v)
		}
	}
	if len(declared) > 0 {
		lines = append(lines, kv("Declared", dim.Render(strings.Join(declared, " · "))))
	}
	if j.Error != nil {
		lines = append(lines, kv("Problem", bad.Render(oneLine(*j.Error))))
	}
	for i := range lines {
		lines[i] = fit(lines[i], width)
	}
	return lines
}
