package ui

import (
	"errors"
	"fmt"
	"strings"
	"time"

	"github.com/charmbracelet/x/ansi"

	"github.com/tallesborges/zdx/apps/monitor/internal/source"
	"github.com/tallesborges/zdx/apps/monitor/internal/wire"
)

func count(n int, one, many string) string {
	if n == 1 {
		return "1 " + one
	}
	return fmt.Sprintf("%d %s", n, many)
}

func sec(v uint64) time.Duration { return time.Duration(v) * time.Second }

// Below this size the layout cannot fit; the frame says so instead.
const minFrameWidth, minFrameHeight = 20, 6

// render is the frame, never wider or taller than the terminal.
func (m *Model) render() string {
	if m.width < minFrameWidth || m.height < minFrameHeight {
		return clipFrame("zdx monitor: terminal too small\nresize, or q to quit", m.width, m.height)
	}
	return clipFrame(m.renderFrame(), m.width, m.height)
}

func clipFrame(frame string, width, height int) string {
	if width <= 0 || height <= 0 {
		return ""
	}
	lines := strings.Split(frame, "\n")
	if len(lines) > height {
		lines = lines[:height]
	}
	for i, l := range lines {
		if ansi.StringWidth(l) > width {
			lines[i] = ansi.Truncate(l, width, "")
		}
	}
	return strings.Join(lines, "\n")
}

func (m *Model) renderFrame() string {
	if m.help {
		return m.renderHelp()
	}
	if m.transcript != nil {
		return m.renderTranscript()
	}
	w := m.contentWidth()
	var lines []string
	for _, l := range m.headerLines() {
		lines = append(lines, fit(l, w))
	}
	lines = append(lines, m.tabBar(w))
	if m.spacer() {
		lines = append(lines, "")
	}
	h := m.bodyHeight()
	body := m.renderTab(w, h)
	for i := 0; i < h; i++ {
		if i < len(body) {
			lines = append(lines, fit(body[i], w))
		} else {
			lines = append(lines, "")
		}
	}
	for _, l := range m.footerLines() {
		lines = append(lines, fit(l, w))
	}
	return strings.Join(lines, "\n")
}

func (m *Model) renderTab(w, h int) []string {
	switch m.tab {
	case tabAgents:
		return m.renderAgents(w, h)
	case tabThreads:
		return m.renderThreads(w, h)
	case tabServices:
		return m.renderServices(w, h)
	case tabJobs:
		return m.renderJobs(w, h)
	case tabUsage:
		return m.renderUsage(w, h)
	case tabLogs:
		return m.renderLogs(w, h)
	case tabBackground:
		return m.renderBackground(w, h)
	case tabConfig:
		return m.renderConfig(w, h)
	}
	return nil
}

// spacer is a blank row under the tab bar, on terminals tall enough.
func (m *Model) spacer() bool { return m.height >= 20 }

// pending is the body of a surface with no data yet: loading, or why it
// could not load.
func (m *Model) pending(k fetchKey, what string) []string {
	st := m.fetch[k]
	if st.err != nil {
		out := []string{"", "  " + bad.Render("✗ Could not load "+what+": ") + oneLine(st.err.Error())}
		if errors.Is(st.err, wire.ErrNotRunning) {
			out = append(out, "  "+dim.Render("This needs the orchestrator service. Start it, or run the monitor with --cli-fallback."))
		}
		return out
	}
	if !st.everTried {
		return []string{"", "  " + dim.Render("Not loaded yet.")}
	}
	return []string{"", "  " + dim.Render(spinner(m.now())+" Loading "+what+"…")}
}

// connection is the header's state of the orchestrator service.
func (m *Model) connection() string {
	st := m.fetch[keyStatus]
	switch {
	case m.status != nil:
		busy := 0
		for _, t := range m.status.Threads {
			if t.State != "idle" || t.QueuedInputs > 0 {
				busy++
			}
		}
		if m.status.Settled {
			return ok.Render("●") + " orchestrator idle"
		}
		s := warn.Render("●") + " orchestrator busy"
		if busy > 0 {
			s += dim.Render(" · " + count(busy, "thread", "threads") + " with work")
		}
		return s
	case st.err != nil && errors.Is(st.err, wire.ErrNotRunning):
		return bad.Render("○ orchestrator not running")
	case st.err != nil:
		return bad.Render("○ orchestrator unreachable")
	}
	return dim.Render(spinner(m.now()) + " connecting…")
}

// headerLines are the title and connection, plus a line for each warning
// the reader must see. Paths and protocol details are in the help screen.
func (m *Model) headerLines() []string {
	left := " " + title.Render("zdx monitor") + "   " + m.connection()
	right := dim.Render("? help ")
	gap := m.contentWidth() - ansi.StringWidth(left) - ansi.StringWidth(right)
	if gap < 1 {
		right, gap = "", 0
	}
	lines := []string{left + strings.Repeat(" ", gap) + right}
	warnLine := func(s string) { lines = append(lines, " "+warn.Render("! ")+s) }
	if m.status != nil {
		if r := m.status.ViewsRoot; r != nil && !sameDir(*r, m.opts.Root) {
			warnLine(fmt.Sprintf("The service reads views for %s, not --root %s: config, usage, and jobs are for its root.", tildePath(*r), tildePath(m.opts.Root)))
		}
		var missing []string
		for _, c := range []string{wire.CapMonitorViews, wire.CapThreadViews} {
			if !m.status.Has(c) {
				missing = append(missing, c)
			}
		}
		if len(missing) > 0 {
			warnLine("The orchestrator does not serve " + strings.Join(missing, ", ") + ": update and restart it.")
		}
	} else if m.fetch[keyStatus].err != nil {
		if m.opts.CLIFallback {
			warnLine("Views run in this process through `zdx views` (--cli-fallback); this refreshes derived caches.")
		} else {
			warnLine("Agents, threads, usage, and config need the orchestrator (or --cli-fallback). Services, jobs, and logs still work.")
		}
	}
	return lines
}

// badge is a tab's short count, when its data is loaded.
func (m *Model) badge(t tab) (string, bool) {
	switch t {
	case tabAgents:
		if m.agents.loaded {
			return fmt.Sprint(len(m.agents.rows)), false
		}
	case tabServices:
		if m.services != nil {
			for _, s := range m.services.Services {
				if serviceAlarm(s) {
					return "!", true
				}
			}
		}
	case tabJobs:
		if n := m.jobs.alarms(); n > 0 {
			return fmt.Sprintf("%d!", n), true
		}
	case tabBackground:
		if m.snapshot != nil {
			return fmt.Sprint(len(m.snapshot.BackgroundProcesses)), false
		}
	}
	return "", false
}

var shortLabels = [...]string{"Agents", "Threads", "Svcs", "Jobs", "Usage", "Logs", "Procs", "Config"}

// tabBar fits the tabs to width: full labels with badges, then without
// badges, then short labels.
func (m *Model) tabBar(width int) string {
	build := func(badges, short bool, numbers bool) string {
		var b strings.Builder
		for t := tab(0); t < tabCount; t++ {
			label := t.label()
			if short {
				label = shortLabels[t]
			}
			text := fmt.Sprintf("%d %s", t+1, label)
			badge, alarm := m.badge(t)
			if t == m.tab {
				if badges && badge != "" {
					text += " " + badge
				}
				b.WriteString(activeTab.Render(" " + text + " "))
				continue
			}
			if numbers {
				b.WriteString(" " + dim.Render(fmt.Sprintf("%d", t+1)) + " ")
				continue
			}
			b.WriteString(" " + dim.Render(fmt.Sprintf("%d", t+1)) + " " + label)
			if badges && badge != "" {
				style := dim
				if alarm {
					style = bad
				}
				b.WriteString(" " + style.Render(badge))
			}
			b.WriteString(" ")
		}
		return b.String()
	}
	for _, v := range []struct{ badges, short bool }{{true, false}, {false, false}, {true, true}, {false, true}} {
		if s := build(v.badges, v.short, false); ansi.StringWidth(s) <= width {
			return s
		}
	}
	return fit(build(true, false, true), width)
}

// footerLines are the key hints and how fresh the shown data is; a stale
// surface gets its own line with the error.
func (m *Model) footerLines() []string {
	keys := tabKeys(m.tab)
	if m.transcript != nil {
		keys = []fetchKey{keyTranscript}
		if m.transcript.traj != nil {
			keys = []fetchKey{keyTrajectory}
		}
	}
	now := m.now()
	var lines []string
	for _, k := range keys {
		st := m.fetch[k]
		if st.err != nil && !st.fetched.IsZero() {
			lines = append(lines, " "+warn.Render(fmt.Sprintf("! Could not refresh %s: %s. Showing data from %s ago.", k, oneLine(st.err.Error()), span(now.Sub(st.fetched)))))
		}
	}
	right := ""
	if len(keys) > 0 {
		right = freshness(m.fetch[keys[0]], now)
	}
	w := m.contentWidth()
	left := " " + m.hints(w-ansi.StringWidth(right)-3)
	gap := w - ansi.StringWidth(left) - ansi.StringWidth(right)
	return append(lines, left+strings.Repeat(" ", max(gap, 1))+right)
}

func freshness(st fetchState, now time.Time) string {
	origin := ""
	if st.origin == source.OriginCLI {
		origin = " via zdx views"
	}
	switch {
	case st.err != nil && st.fetched.IsZero():
		return bad.Render("load failed ")
	case st.err != nil:
		return warn.Render("stale " + span(now.Sub(st.fetched)) + " ")
	case st.fetched.IsZero() && st.inflight:
		return dim.Render(spinner(now) + " loading ")
	case st.fetched.IsZero():
		return ""
	case st.inflight:
		return dim.Render(spinner(now) + " refreshing" + origin + " ")
	}
	return dim.Render("updated " + span(now.Sub(st.fetched)) + " ago" + origin + " ")
}

// hint renders `key action` pairs.
func hint(pairs ...string) string { return hintFit(1<<30, pairs...) }

// hintFit renders as many whole `key action` pairs as fit in width.
func hintFit(width int, pairs ...string) string {
	out := ""
	for i := 0; i+1 < len(pairs); i += 2 {
		part := keyStyle.Render(pairs[i]) + " " + dim.Render(pairs[i+1])
		next := part
		if out != "" {
			next = out + "  " + part
		}
		if ansi.StringWidth(next) > width {
			break
		}
		out = next
	}
	return out
}

func (m *Model) hints(width int) string {
	if m.transcript != nil {
		switch {
		case m.transcript.traj != nil:
			return hintFit(width, "esc", "back", "j/k", "scroll", "r", "refresh")
		case m.transcript.pane != nil:
			return hintFit(width, "esc", "back", "j/k", "scroll")
		}
		return hintFit(width, "esc", "back", "n/p", "tools", "↵", "detail", "t", "thinking", "i", "timing", "G", "follow", "?", "help")
	}
	var tabHints []string
	switch m.tab {
	case tabAgents:
		tabHints = []string{"↵", "transcript"}
	case tabThreads:
		other := "workers"
		if m.threads.kind == "workers" {
			other = "all"
		}
		tabHints = []string{"↵", "transcript", "t", "show " + other}
	case tabUsage:
		tabHints = []string{"Q", "refresh quotas", "[ ]", "days"}
	case tabLogs:
		tabHints = []string{"l", "level", "/", "filter", "esc", "clear", "[ ]", "day", "G", "follow"}
	case tabConfig:
		tabHints = []string{"[ ]", "workspace"}
	}
	return hintFit(width, append(tabHints, "?", "help", "q", "quit", "1-8", "tabs")...)
}

// helpLines list every key and the connection details.
func (m *Model) helpLines() []string {
	key := func(k, what string) string { return "    " + keyStyle.Render(pad(k, 24)) + what }
	out := []string{
		" " + title.Render("zdx monitor") + dim.Render(" · read-only dashboard"),
		"",
		"  " + bold.Render("Everywhere"),
		key("1-8  tab  ← →", "switch tabs"),
		key("j k  ↑ ↓  wheel", "move"),
		key("pgup pgdn  ctrl+u/d", "page"),
		key("gg  G", "top, bottom"),
		key("r", "refresh this tab now"),
		key("?", "this help (esc closes)"),
		key("q  ctrl+c", "quit"),
		"",
		"  " + bold.Render("Tabs"),
		key("Agents  ↵", "open the selected run's transcript"),
		key("Threads  ↵  t", "open transcript · all threads or workers only"),
		key("Usage  Q  [ ]", "fetch quotas now · token window 1/7/30/90/365 days"),
		key("Logs  l / esc [ ] G", "level · filter text · clear · older/newer day · follow"),
		key("Config  [ ]", "previous/next workspace"),
		"",
		"  " + bold.Render("Transcript"),
		key("n p  tab", "next/previous tool"),
		key("↵", "tool detail (output, input)"),
		key("t", "expand/collapse thinking"),
		key("i", "timing (trajectory)"),
		key("G", "follow new activity"),
		key("esc", "back"),
		"",
		"  " + bold.Render("Connection"),
	}
	line := func(k, v string) { out = append(out, "    "+dim.Render(pad(k, 24))+v) }
	line("project root", m.opts.Root)
	line("ZDX home", m.opts.Home)
	line("socket", source.SocketPath(m.opts.Home))
	if m.status != nil {
		line("protocol", fmt.Sprint(m.status.Version()))
		line("views root", orDash(str(m.status.ViewsRoot)))
		line("capabilities", strings.Join(m.status.Capabilities, ", "))
		line("tracked threads", fmt.Sprintf("%d (threads the service holds state for)", len(m.status.Threads)))
	} else if err := m.fetch[keyStatus].err; err != nil {
		line("status", bad.Render(oneLine(err.Error())))
	}
	fallback := "off"
	if m.opts.CLIFallback {
		fallback = "on: views run through `zdx views` while the service is down"
	}
	line("cli fallback", fallback)
	out = append(out, "",
		"  "+bold.Render("Data sources"),
		"    "+dim.Render("Views come from the orchestrator socket; Services and Jobs from `zdx service status --json`"),
		"    "+dim.Render("and `zdx jobs list --daemon --json`; Logs from $ZDX_HOME/logs. Nothing here writes."),
		"    "+dim.Render("Not here yet: service start/stop/restart, bg kill, job model edit (use the zdx CLI),"),
		"    "+dim.Render("full thread browse and filters, config edits."),
	)
	return out
}

func (m *Model) renderHelp() string {
	lines := m.helpLines()
	h := max(m.height-1, 1)
	m.helpScroll.clamp(len(lines), h)
	view := lines[m.helpScroll.offset:min(m.helpScroll.offset+h, len(lines))]
	out := make([]string, 0, m.height)
	for _, l := range view {
		out = append(out, fit(l, m.contentWidth()))
	}
	for len(out) < h {
		out = append(out, "")
	}
	out = append(out, " "+hint("esc", "close", "j/k", "scroll"))
	return strings.Join(out, "\n")
}
