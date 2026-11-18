package ui

import (
	"context"
	"time"

	tea "charm.land/bubbletea/v2"

	"github.com/tallesborges/zdx/apps/monitor/internal/data"
	"github.com/tallesborges/zdx/apps/monitor/internal/source"
	"github.com/tallesborges/zdx/apps/monitor/internal/wire"
)

// Backend is what the monitor reads. source.Source implements it; tests
// use fakes.
type Backend interface {
	Status(ctx context.Context) (*wire.StatusReport, error)
	Services(ctx context.Context) (data.ServiceStatus, error)
	Snapshot(ctx context.Context) (data.Snapshot, source.Origin, error)
	ActiveAgents(ctx context.Context) ([]data.Agent, source.Origin, error)
	Threads(ctx context.Context, kind string) (data.ThreadList, source.Origin, error)
	Thread(ctx context.Context, id string, after *uint64) (data.Thread, source.Origin, error)
	Trajectory(ctx context.Context, id string) (data.Trajectory, source.Origin, error)
	Usage(ctx context.Context, days uint32) (data.Usage, source.Origin, error)
	Quotas(ctx context.Context) ([]data.Subscription, source.Origin, error)
	Workspaces(ctx context.Context) (data.Workspaces, source.Origin, error)
	Jobs(ctx context.Context) (data.SchedulerJobs, error)
	Log(ctx context.Context, index, maxLines int, prev source.LogStamp) (source.LogTail, error)
}

// fetchKey names one refreshable surface. At most one fetch per key runs at
// a time, so a slow backend never piles up requests.
type fetchKey int

const (
	keyStatus fetchKey = iota
	keyServices
	keySnapshot
	keyAgents
	keyThreads
	keyTranscript
	keyUsage
	keyQuotas
	keyWorkspaces
	keyJobs
	keyLogs
	keyTrajectory
	keyParents
	keyCount
)

func (k fetchKey) String() string {
	return [...]string{"status", "services", "snapshot", "agents", "threads", "transcript", "usage", "quotas", "workspaces", "jobs", "logs", "trajectory", "parents"}[k]
}

// fetchBudget bounds one fetch. Quotas ask every provider over the network.
func fetchBudget(k fetchKey) time.Duration {
	switch k {
	case keyQuotas:
		return wire.QuotasTimeout
	case keyJobs, keyServices, keyUsage:
		return source.CLITimeout
	}
	return wire.ReplyTimeout
}

// fetchState is the refresh bookkeeping of one key.
type fetchState struct {
	inflight  bool
	started   time.Time
	fetched   time.Time // last success
	err       error     // last failure, cleared by a success
	errAt     time.Time
	origin    source.Origin
	force     bool
	gen       uint64 // bumped when what the key reads changes (thread, kind, days)
	everTried bool
	cancel    context.CancelFunc
}

// resultMsg carries one fetch's outcome back to Update.
type resultMsg struct {
	key    fetchKey
	gen    uint64
	value  any
	origin source.Origin
	err    error
}

type tickMsg time.Time

const tickEvery = 500 * time.Millisecond

func tick() tea.Cmd {
	return tea.Tick(tickEvery, func(t time.Time) tea.Msg { return tickMsg(t) })
}

// interval is how often a key refreshes while its tab is shown; 0 means
// only on entry and on `r`.
func interval(k fetchKey) time.Duration {
	switch k {
	case keyStatus:
		return 3 * time.Second
	case keyServices:
		return 5 * time.Second
	case keyAgents, keyTranscript, keyTrajectory:
		return 2 * time.Second
	case keySnapshot:
		return 10 * time.Second
	case keyThreads:
		return 15 * time.Second
	case keyUsage:
		return time.Minute
	case keyQuotas:
		return 5 * time.Minute
	case keyWorkspaces, keyJobs:
		return 30 * time.Second
	case keyParents:
		return 15 * time.Second
	case keyLogs:
		return time.Second
	}
	return 0
}

// start returns the command for one fetch of k, or nil when it already runs.
func (m *Model) start(k fetchKey, now time.Time) tea.Cmd {
	st := &m.fetch[k]
	if st.inflight {
		return nil
	}
	run := m.fetcher(k)
	if run == nil {
		return nil
	}
	st.inflight, st.started, st.force, st.everTried = true, now, false, true
	ctx, cancel := context.WithTimeout(m.ctx, fetchBudget(k))
	st.cancel = cancel
	gen := st.gen
	return func() tea.Msg {
		defer cancel()
		value, origin, err := run(ctx)
		return resultMsg{key: k, gen: gen, value: value, origin: origin, err: err}
	}
}

type fetchFunc func(ctx context.Context) (any, source.Origin, error)

// fetcher captures what k reads now (thread id, kind, days, log file), so a
// result always matches the request that produced it.
func (m *Model) fetcher(k fetchKey) fetchFunc {
	b := m.backend
	switch k {
	case keyStatus:
		return func(ctx context.Context) (any, source.Origin, error) {
			r, err := b.Status(ctx)
			return r, source.OriginSocket, err
		}
	case keyServices:
		return func(ctx context.Context) (any, source.Origin, error) {
			r, err := b.Services(ctx)
			return r, "", err
		}
	case keySnapshot:
		return func(ctx context.Context) (any, source.Origin, error) { return wrap(b.Snapshot(ctx)) }
	case keyAgents:
		return func(ctx context.Context) (any, source.Origin, error) { return wrap(b.ActiveAgents(ctx)) }
	case keyThreads:
		kind := m.threads.kind
		return func(ctx context.Context) (any, source.Origin, error) { return wrap(b.Threads(ctx, kind)) }
	case keyTranscript:
		t := m.transcript
		if t == nil {
			return nil
		}
		id := t.threadID
		var after *uint64
		if t.thread != nil {
			c := t.thread.Cursor
			after = &c
		}
		return func(ctx context.Context) (any, source.Origin, error) {
			th, origin, err := b.Thread(ctx, id, after)
			return transcriptResult{id: id, after: after, thread: th}, origin, err
		}
	case keyParents:
		return m.parentsFetcher()
	case keyTrajectory:
		t := m.transcript
		if t == nil || t.thread == nil {
			return nil
		}
		// thread_trajectory does not follow an alias (thread_view does), so
		// ask for the thread the transcript's events come from; wait for
		// the transcript to know it.
		id := t.threadID
		if t.thread.SourceID != "" {
			id = t.thread.SourceID
		}
		owner := t.threadID
		return func(ctx context.Context) (any, source.Origin, error) {
			tr, origin, err := b.Trajectory(ctx, id)
			return trajectoryResult{id: owner, report: tr}, origin, err
		}
	case keyUsage:
		days := m.usage.days
		return func(ctx context.Context) (any, source.Origin, error) { return wrap(b.Usage(ctx, days)) }
	case keyQuotas:
		historian, _ := b.(QuotaHistorian)
		now := m.now()
		return func(ctx context.Context) (any, source.Origin, error) {
			subs, origin, err := b.Quotas(ctx)
			if err != nil {
				return nil, origin, err
			}
			r := quotaResult{subs: subs}
			if historian != nil {
				// No history (none yet, or unreadable) is no strips: unknown, not zero.
				r.history, _ = historian.QuotaHistory(now)
			}
			return r, origin, nil
		}
	case keyWorkspaces:
		return func(ctx context.Context) (any, source.Origin, error) { return wrap(b.Workspaces(ctx)) }
	case keyJobs:
		return func(ctx context.Context) (any, source.Origin, error) {
			r, err := b.Jobs(ctx)
			return r, "", err
		}
	case keyLogs:
		index, lines, prev := m.logs.fileIndex, m.logs.maxLines, m.logs.stamp
		return func(ctx context.Context) (any, source.Origin, error) {
			r, err := b.Log(ctx, index, lines, prev)
			return r, "", err
		}
	}
	return nil
}

func wrap[T any](v T, origin source.Origin, err error) (any, source.Origin, error) {
	return v, origin, err
}

// QuotaHistorian is a backend that also reads the quota history: the daily
// use of each quota window over the last days (optional).
type QuotaHistorian interface {
	QuotaHistory(now time.Time) ([]data.QuotaHistory, error)
}

type quotaResult struct {
	subs    []data.Subscription
	history []data.QuotaHistory
}

type trajectoryResult struct {
	id     string
	report data.Trajectory
}

type transcriptResult struct {
	id     string
	after  *uint64
	thread data.Thread
}

// tabKeys are the keys a tab shows and refreshes.
func tabKeys(t tab) []fetchKey {
	switch t {
	case tabAgents:
		return []fetchKey{keyAgents, keyParents}
	case tabThreads:
		return []fetchKey{keyThreads}
	case tabServices:
		return []fetchKey{keyServices}
	case tabJobs:
		return []fetchKey{keyJobs}
	case tabUsage:
		return []fetchKey{keyUsage, keyQuotas}
	case tabLogs:
		return []fetchKey{keyLogs}
	case tabBackground:
		return []fetchKey{keySnapshot}
	case tabConfig:
		return []fetchKey{keySnapshot, keyWorkspaces}
	}
	return nil
}

// due lists the keys to fetch now: status always, the shown tab's keys, and
// the open transcript.
func (m *Model) due(now time.Time) []fetchKey {
	keys := append([]fetchKey{keyStatus}, tabKeys(m.tab)...)
	if m.transcript != nil {
		keys = append(keys, keyTranscript)
		if m.transcript.traj != nil {
			keys = append(keys, keyTrajectory)
		}
	}
	var out []fetchKey
	for _, k := range keys {
		st := &m.fetch[k]
		if st.inflight {
			continue
		}
		last := st.started
		if st.force || !st.everTried || (interval(k) > 0 && now.Sub(last) >= interval(k)) {
			out = append(out, k)
		}
	}
	return out
}

// refresh starts every due fetch.
func (m *Model) refresh(now time.Time) tea.Cmd {
	var cmds []tea.Cmd
	for _, k := range m.due(now) {
		if cmd := m.start(k, now); cmd != nil {
			cmds = append(cmds, cmd)
		}
	}
	return tea.Batch(cmds...)
}

// invalidate drops results of requests made before what k reads changed
// and fetches again as soon as the running fetch (cancelled here) returns.
// It never starts a second fetch of the same key.
func (m *Model) invalidate(k fetchKey) {
	st := &m.fetch[k]
	st.gen++
	st.force = true
	if st.inflight && st.cancel != nil {
		st.cancel()
	}
}
