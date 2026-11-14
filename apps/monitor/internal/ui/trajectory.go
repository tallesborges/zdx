package ui

import (
	"fmt"
	"time"

	"github.com/tallesborges/zdx/apps/monitor/internal/data"
)

// trajectoryState is the `i` overlay of a transcript: the engine's
// thread_trajectory projection, polled while it is open. Only the fields
// the view states are shown; nothing is summed or derived here.
type trajectoryState struct {
	report *data.Trajectory
	lines  []string
	scroll scrollState
}

// ms shows a millisecond duration; nil is unknown.
func ms(p *uint64) string {
	if p == nil {
		return "unknown"
	}
	return msText(*p)
}

func msText(v uint64) string {
	d := time.Duration(v) * time.Millisecond
	switch {
	case d < time.Second:
		return fmt.Sprintf("%dms", v)
	case d < time.Minute:
		return fmt.Sprintf("%.1fs", d.Seconds())
	}
	return fmt.Sprintf("%dm %ds", int(d.Minutes()), int(d.Seconds())%60)
}

func statusMark(status string) string {
	switch status {
	case "ok":
		return ok.Render("✓")
	case "failed":
		return bad.Render("✗")
	case "running":
		return accent.Render("…")
	}
	return dim.Render("?")
}

func timingNote(timing string) string {
	if timing == "exact" {
		return ""
	}
	return dim.Render(" (" + timing + ")")
}

func trajectoryLines(r data.Trajectory, width int) []string {
	out := []string{
		bold.Render(" Client-observed trajectory") + dim.Render(fmt.Sprintf(" · %d events · observed %s", r.SourceEventCount, localTime(r.ObservedAt))),
		fmt.Sprintf(" coverage %d/%d durations measured · %d exact", r.Measured, r.Total, r.Exact),
		fmt.Sprintf(" active wall %s · span work %s · concurrent %s", ms(r.ActiveWallMS), ms(r.SpanWorkMS), ms(r.ConcurrentWorkMS)),
	}
	if r.HasUnavailable {
		out = append(out, warn.Render(" some spans have no timing: totals cover measured spans only"))
	}
	if len(r.Bottlenecks) == 0 {
		out = append(out, dim.Render(" bottlenecks: none measured"))
	} else {
		out = append(out, "", bold.Render(" Bottlenecks"))
		for i, b := range r.Bottlenecks {
			if i == 5 {
				break
			}
			out = append(out, fmt.Sprintf("  %d. %s %s · turn %d · %s%s", i+1, statusMark(b.Status), b.Label, b.Turn, msText(b.DurationMS), timingNote(b.Timing)))
		}
	}
	if len(r.Turns) == 0 {
		return append(out, "", dim.Render(" no user turns"))
	}
	for _, t := range r.Turns {
		out = append(out, "", bold.Render(fmt.Sprintf(" Turn %d", t.Index))+dim.Render(fmt.Sprintf(" · %s · %d/%d measured · elapsed %s · active %s · work %s", t.Timing, t.Measured, t.Total, ms(t.ElapsedMS), ms(t.ActiveWallMS), ms(t.SpanWorkMS))))
		if t.Input != nil {
			for i, l := range wrapText(oneLine(t.Input.Text), width-6) {
				if i == 2 {
					out = append(out, dim.Render("    …"))
					break
				}
				out = append(out, accent.Render("    "+l))
			}
		}
		for _, s := range t.Spans {
			if s.Kind == "input" {
				continue
			}
			line := fmt.Sprintf("    %s %s %s", statusMark(s.Status), pad(s.Kind, 5), s.Label)
			if s.Detail != "" && s.Detail != s.Label {
				line += dim.Render(" " + oneLine(s.Detail))
			}
			dur := s.DurationMS
			if dur == nil {
				dur = s.WallMS
			}
			if dur == nil {
				line += dim.Render(" · no timing")
			} else {
				line += " · " + ms(dur) + timingNote(s.Timing)
			}
			if s.Kind == "model" {
				line += dim.Render(" · TTFT " + ms(s.TTFTMS))
				if s.InputTokens != nil || s.OutputTokens != nil {
					line += dim.Render(fmt.Sprintf(" · in %s out %s", tokensOpt(s.InputTokens), tokensOpt(s.OutputTokens)))
				}
			}
			out = append(out, line)
		}
	}
	return out
}

func tokensOpt(p *uint64) string {
	if p == nil {
		return "?"
	}
	return tokens(*p)
}

func (m *Model) renderTrajectory(rows int) []string {
	t := m.transcript.traj
	if t.report == nil {
		if err := m.fetch[keyTrajectory].err; err != nil {
			return []string{bad.Render(" " + oneLine(err.Error()))}
		}
		return []string{dim.Render(" loading trajectory…")}
	}
	t.scroll.clamp(len(t.lines), rows)
	return t.lines[t.scroll.offset:min(t.scroll.offset+rows, len(t.lines))]
}
