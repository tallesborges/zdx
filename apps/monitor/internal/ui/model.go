// Package ui is the Bubble Tea model of the monitor: tabs, lists, the
// transcript overlay, and the refresh loop. Data comes from a Backend; the
// UI only lays it out.
package ui

import (
	"context"
	"errors"
	"time"

	tea "charm.land/bubbletea/v2"

	"github.com/tallesborges/zdx/apps/monitor/internal/data"
	"github.com/tallesborges/zdx/apps/monitor/internal/source"
	"github.com/tallesborges/zdx/apps/monitor/internal/wire"
	"github.com/tallesborges/zdx/apps/shared/transcript"
)

type tab int

const (
	tabAgents tab = iota
	tabThreads
	tabServices
	tabJobs
	tabUsage
	tabLogs
	tabBackground
	tabConfig
	tabCount
)

func (t tab) label() string {
	return [...]string{"Agents", "Threads", "Services", "Jobs", "Usage", "Logs", "Background", "Config"}[t]
}

// Options are the monitor's settings, shown in the header.
type Options struct {
	Root        string
	Home        string
	CLIFallback bool
	// Now is the clock; nil means time.Now.
	Now func() time.Time
}

// Model is the monitor's state.
type Model struct {
	ctx     context.Context
	backend Backend
	opts    Options

	width, height int
	tab           tab
	fetch         [keyCount]fetchState
	gPending      bool
	quitting      bool
	help          bool
	helpScroll    scrollState

	status   *wire.StatusReport
	services *data.ServiceStatus
	snapshot *data.Snapshot

	agents     agentsTab
	threads    threadsTab
	jobs       jobsTab
	usage      usageTab
	logs       logsTab
	background listState
	config     configTab
	configGen  uint64
	svcList    listState

	transcript *transcriptState
}

// New returns a model that reads from b. ctx bounds every fetch: cancel it
// to stop them.
func New(ctx context.Context, b Backend, opts Options) *Model {
	if opts.Now == nil {
		opts.Now = time.Now
	}
	m := &Model{ctx: ctx, backend: b, opts: opts, width: 80, height: 24}
	m.threads.kind = "all"
	m.usage.days = 7
	m.logs.maxLines = defaultLogLines
	m.logs.follow = true
	return m
}

func (m *Model) now() time.Time { return m.opts.Now() }

// Init starts the first fetches and the tick.
func (m *Model) Init() tea.Cmd {
	return tea.Batch(m.refresh(m.now()), tick())
}

// Update applies one message.
func (m *Model) Update(msg tea.Msg) (tea.Model, tea.Cmd) {
	switch msg := msg.(type) {
	case tea.WindowSizeMsg:
		m.width, m.height = msg.Width, msg.Height
		m.layoutChanged()
		return m, nil
	case tickMsg:
		if m.quitting {
			return m, nil
		}
		return m, tea.Batch(m.refresh(m.now()), tick())
	case resultMsg:
		return m, m.applyResult(msg)
	case configPickerMsg:
		m.applyConfigPicker(msg)
		return m, nil
	case configChangeMsg:
		return m, m.applyConfigChange(msg)
	case tea.KeyPressMsg:
		return m, m.handleKey(msg)
	case tea.MouseWheelMsg:
		m.handleWheel(msg.Mouse())
		return m, nil
	case tea.MouseClickMsg:
		return m, m.mouseClick(msg.Mouse())
	case tea.MouseMotionMsg:
		return m, m.mouseMotion(msg.Mouse())
	case tea.MouseReleaseMsg:
		return m, m.mouseRelease(msg.Mouse())
	}
	return m, nil
}

func (m *Model) applyResult(r resultMsg) tea.Cmd {
	st := &m.fetch[r.key]
	st.inflight, st.cancel = false, nil
	if r.gen != st.gen {
		// Asked before what this key reads changed; fetch the new one.
		return m.refresh(m.now())
	}
	if r.err != nil {
		if errors.Is(r.err, context.Canceled) && m.ctx.Err() != nil {
			return nil
		}
		st.err, st.errAt = r.err, m.now()
		if r.key == keyStatus {
			m.status = nil
		}
		return nil
	}
	st.err, st.fetched, st.origin = nil, m.now(), r.origin
	switch v := r.value.(type) {
	case *wire.StatusReport:
		transcript.CleanAll(v)
		m.status = v
	case data.ServiceStatus:
		transcript.CleanAll(&v)
		m.services = &v
		m.svcList.sync(serviceIDs(v.Services))
	case data.Snapshot:
		transcript.CleanAll(&v)
		m.snapshot = &v
		m.background.sync(backgroundIDs(v.BackgroundProcesses))
	case []data.Agent:
		transcript.CleanAll(&v)
		m.agents.set(v)
		if m.agents.syncParents() {
			m.fetch[keyParents].force = true
			return m.refresh(m.now())
		}
	case parentsAnswer:
		m.applyParents(v.result, v.lookups)
	case data.ThreadList:
		transcript.CleanAll(&v)
		m.threads.set(v)
	case transcriptResult:
		transcript.CleanAll(&v.thread)
		if m.transcript != nil && m.transcript.threadID == v.id && !m.transcript.apply(v, m.contentWidth()) {
			m.invalidate(keyTranscript)
			m.invalidate(keyTrajectory)
			return m.refresh(m.now())
		}
	case trajectoryResult:
		transcript.CleanAll(&v.report)
		if t := m.transcript; t != nil && t.threadID == v.id && t.traj != nil {
			t.traj.report = &v.report
			t.traj.lines = trajectoryLines(v.report, m.contentWidth())
		}
	case data.Usage:
		transcript.CleanAll(&v)
		m.usage.data = &v
	case []data.Subscription:
		transcript.CleanAll(&v)
		m.usage.quotas = v
		m.usage.quotasLoaded = true
	case data.Workspaces:
		transcript.CleanAll(&v)
		m.config.set(v)
	case data.SchedulerJobs:
		transcript.CleanAll(&v)
		m.jobs.set(v)
	case source.LogTail:
		transcript.CleanAll(&v.Files)
		transcript.CleanAll(&v.Lines)
		m.logs.apply(v)
	default:
		return nil
	}
	return nil
}

// layoutChanged re-wraps width-dependent content.
func (m *Model) layoutChanged() {
	if t := m.transcript; t != nil {
		t.rebuild(m.contentWidth())
		if t.traj != nil && t.traj.report != nil {
			t.traj.lines = trajectoryLines(*t.traj.report, m.contentWidth())
		}
	}
}

func (m *Model) contentWidth() int { return max(m.width, 20) }

// bodyHeight is the rows left for a tab below the header and above the
// footer.
func (m *Model) bodyHeight() int {
	spacer := 0
	if m.spacer() {
		spacer = 1
	}
	return max(m.height-1-spacer-len(m.footerLines()), 3)
}

func (m *Model) switchTab(t tab) tea.Cmd {
	m.tab = t
	m.gPending = false
	now := m.now()
	for _, k := range tabKeys(t) {
		st := &m.fetch[k]
		if k == keyQuotas && st.everTried {
			continue
		}
		if st.everTried && now.Sub(st.started) < 2*time.Second {
			continue
		}
		st.force = true
	}
	return m.refresh(now)
}

func (m *Model) handleKey(msg tea.KeyPressMsg) tea.Cmd {
	key := msg.String()
	if key == "ctrl+c" {
		return m.quit()
	}
	if m.logs.editing {
		return m.logs.editKey(msg)
	}
	if m.tab == tabConfig && m.config.pick != nil && !m.help {
		return m.config.scopeKey(msg)
	}
	if m.tab == tabConfig && m.config.edit != nil && !m.help {
		return m.configEditKey(msg)
	}
	// Vim `gg`: the first g waits, a second goes to the top.
	if key == "g" {
		if m.gPending {
			m.gPending = false
			key = "home"
		} else {
			m.gPending = true
			return nil
		}
	} else if m.gPending {
		m.gPending = false
		return nil
	}
	if m.help {
		switch key {
		case "?", "esc", "q", "enter":
			m.help = false
		default:
			m.helpScroll.key(key, len(m.helpLines()), m.height-2)
		}
		return nil
	}
	if key == "?" {
		m.help = true
		m.helpScroll = scrollState{}
		return nil
	}
	if m.transcript != nil {
		return m.transcriptKey(key)
	}
	switch key {
	case "q":
		return m.quit()
	case "tab", "right":
		return m.switchTab((m.tab + 1) % tabCount)
	case "shift+tab", "left":
		return m.switchTab((m.tab + tabCount - 1) % tabCount)
	case "r":
		for _, k := range tabKeys(m.tab) {
			m.fetch[k].force = true
		}
		m.fetch[keyStatus].force = true
		return m.refresh(m.now())
	}
	if len(key) == 1 && key[0] >= '1' && key[0] < '1'+byte(tabCount) {
		return m.switchTab(tab(key[0] - '1'))
	}
	return m.tabKey(key)
}

func (m *Model) quit() tea.Cmd {
	m.quitting = true
	for k := range m.fetch {
		if c := m.fetch[k].cancel; c != nil {
			c()
		}
	}
	return tea.Quit
}

// tabKey routes a key to the shown tab.
func (m *Model) tabKey(key string) tea.Cmd {
	switch m.tab {
	case tabAgents:
		if key == "enter" {
			if a := m.agents.selectedAgent(); a != nil && a.ThreadID != nil {
				return m.openTranscript(*a.ThreadID, "agent")
			}
			if n := m.agents.selectedNode(); n != nil && n.run < 0 {
				return m.openTranscript(n.thread, n.thread)
			}
			return nil
		}
		m.agents.list.key(key, len(m.agents.tree), m.agents.list.pageSize())
		m.agents.list.remember(m.agents.ids())
	case tabThreads:
		switch key {
		case "enter":
			if t := m.threads.selectedThread(); t != nil {
				return m.openTranscript(t.ID, t.Title)
			}
			return nil
		case "t":
			if m.threads.kind == "all" {
				m.threads.kind = "workers"
			} else {
				m.threads.kind = "all"
			}
			m.threads.data = nil
			m.invalidate(keyThreads)
			return m.refresh(m.now())
		}
		n := 0
		if m.threads.data != nil {
			n = len(m.threads.data.Threads)
		}
		m.threads.list.key(key, n, m.threads.list.pageSize())
		m.threads.list.remember(m.threads.ids())
	case tabServices:
		n := 0
		if m.services != nil {
			n = len(m.services.Services)
		}
		m.svcList.key(key, n, m.svcList.pageSize())
		if m.services != nil {
			m.svcList.remember(serviceIDs(m.services.Services))
		}
	case tabJobs:
		m.jobs.list.key(key, m.jobs.count(), m.jobs.list.pageSize())
		m.jobs.list.remember(m.jobs.ids())
	case tabUsage:
		switch key {
		case "]", "[":
			m.usage.cycleDays(key == "]")
			m.usage.data = nil
			m.invalidate(keyUsage)
			return m.refresh(m.now())
		case "Q":
			m.fetch[keyQuotas].force = true
			return m.refresh(m.now())
		}
		m.usage.scroll.key(key, len(m.usageLines(m.contentWidth())), m.bodyHeight())
	case tabLogs:
		return m.logsKey(key)
	case tabBackground:
		n := 0
		if m.snapshot != nil {
			n = len(m.snapshot.BackgroundProcesses)
		}
		m.background.key(key, n, m.background.pageSize())
		if m.snapshot != nil {
			m.background.remember(backgroundIDs(m.snapshot.BackgroundProcesses))
		}
	case tabConfig:
		switch key {
		case "]", "[":
			m.config.cycle(key == "]")
			return nil
		case "/":
			m.config.openScopePicker()
			return nil
		case "enter":
			return m.startConfigEdit(false)
		case "x", "delete":
			return m.startConfigEdit(true)
		}
		m.config.list.key(key, m.config.rowCount(), m.config.list.pageSize())
		m.config.list.remember(m.config.ids())
	}
	return nil
}

func (m *Model) handleWheel(mouse tea.Mouse) {
	key := ""
	switch mouse.Button {
	case tea.MouseWheelUp:
		key = "up"
	case tea.MouseWheelDown:
		key = "down"
	default:
		return
	}
	if m.transcript != nil {
		m.transcriptKey(key)
		return
	}
	m.tabKey(key)
}

// View renders the screen.
func (m *Model) View() tea.View {
	v := tea.NewView(m.render())
	v.AltScreen = true
	v.MouseMode = tea.MouseModeAllMotion
	v.WindowTitle = "zdx monitor"
	return v
}
