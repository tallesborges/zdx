package ui

import (
	"context"
	"errors"
	"os"
	"strings"
	"sync"
	"sync/atomic"
	"testing"
	"time"

	tea "charm.land/bubbletea/v2"
	"github.com/charmbracelet/x/ansi"

	"github.com/tallesborges/zdx/apps/monitor/internal/data"
	"github.com/tallesborges/zdx/apps/monitor/internal/sandbox"
	"github.com/tallesborges/zdx/apps/monitor/internal/source"
	"github.com/tallesborges/zdx/apps/monitor/internal/wire"
	"github.com/tallesborges/zdx/apps/shared/transcript"
)

func TestMain(m *testing.M) {
	// Snapshots show local times; pin the zone so they match everywhere.
	time.Local = time.UTC
	os.Exit(sandbox.Main(m.Run))
}

// fake is a Backend with per-call hooks and call counters.
type fake struct {
	mu      sync.Mutex
	calls   map[string]int
	agents  func(ctx context.Context) ([]data.Agent, error)
	threads func(ctx context.Context, kind string) (data.ThreadList, error)
	thread  func(ctx context.Context, id string, after *uint64) (data.Thread, error)
	status  func(ctx context.Context) (*wire.StatusReport, error)
	logs    func(index, lines int, prev source.LogStamp) (source.LogTail, error)

	trajectory func(id string) (data.Trajectory, error)
}

func (f *fake) count(name string) {
	f.mu.Lock()
	defer f.mu.Unlock()
	if f.calls == nil {
		f.calls = map[string]int{}
	}
	f.calls[name]++
}

func (f *fake) n(name string) int {
	f.mu.Lock()
	defer f.mu.Unlock()
	return f.calls[name]
}

func (f *fake) Status(ctx context.Context) (*wire.StatusReport, error) {
	f.count("status")
	if f.status != nil {
		return f.status(ctx)
	}
	return modernStatus(), nil
}

// modernStatus is a current service: protocol 2 with both view
// capabilities. Without Protocol a report is protocol 1, which has none.
func modernStatus() *wire.StatusReport {
	return &wire.StatusReport{Protocol: ptr(uint32(2)), Settled: true, Capabilities: []string{wire.CapMonitorViews, wire.CapThreadViews}}
}
func (f *fake) Services(context.Context) (data.ServiceStatus, error) {
	f.count("services")
	return data.ServiceStatus{}, nil
}
func (f *fake) Snapshot(context.Context) (data.Snapshot, source.Origin, error) {
	f.count("snapshot")
	return data.Snapshot{}, source.OriginSocket, nil
}
func (f *fake) ActiveAgents(ctx context.Context) ([]data.Agent, source.Origin, error) {
	f.count("agents")
	if f.agents != nil {
		a, err := f.agents(ctx)
		return a, source.OriginSocket, err
	}
	return nil, source.OriginSocket, nil
}
func (f *fake) Threads(ctx context.Context, kind string) (data.ThreadList, source.Origin, error) {
	f.count("threads")
	if f.threads != nil {
		l, err := f.threads(ctx, kind)
		return l, source.OriginSocket, err
	}
	return data.ThreadList{}, source.OriginSocket, nil
}
func (f *fake) Thread(ctx context.Context, id string, after *uint64) (data.Thread, source.Origin, error) {
	f.count("thread")
	if f.thread != nil {
		th, err := f.thread(ctx, id, after)
		return th, source.OriginSocket, err
	}
	return data.Thread{ID: id}, source.OriginSocket, nil
}
func (f *fake) Trajectory(_ context.Context, id string) (data.Trajectory, source.Origin, error) {
	f.count("trajectory")
	if f.trajectory != nil {
		tr, err := f.trajectory(id)
		return tr, source.OriginSocket, err
	}
	return data.Trajectory{}, source.OriginSocket, nil
}
func (f *fake) Usage(context.Context, uint32) (data.Usage, source.Origin, error) {
	f.count("usage")
	return data.Usage{}, source.OriginSocket, nil
}
func (f *fake) Quotas(context.Context) ([]data.Subscription, source.Origin, error) {
	f.count("quotas")
	return nil, source.OriginSocket, nil
}
func (f *fake) Workspaces(context.Context) (data.Workspaces, source.Origin, error) {
	f.count("workspaces")
	return data.Workspaces{}, source.OriginSocket, nil
}
func (f *fake) Jobs(context.Context) (data.DaemonJobs, error) {
	f.count("jobs")
	return data.DaemonJobs{}, nil
}
func (f *fake) Log(_ context.Context, index, lines int, prev source.LogStamp) (source.LogTail, error) {
	f.count("logs")
	if f.logs != nil {
		return f.logs(index, lines, prev)
	}
	return source.LogTail{}, nil
}

// clock is a settable time source.
type clock struct{ t atomic.Int64 }

func newClock() *clock {
	c := &clock{}
	c.t.Store(time.Date(2026, 11, 14, 12, 0, 0, 0, time.UTC).UnixNano())
	return c
}
func (c *clock) now() time.Time         { return time.Unix(0, c.t.Load()) }
func (c *clock) add(d time.Duration)    { c.t.Add(int64(d)) }
func ptr[T any](v T) *T                 { return &v }
func key(s string) tea.KeyPressMsg      { return tea.KeyPressMsg(tea.Key{Text: s, Code: []rune(s)[0]}) }
func special(code rune) tea.KeyPressMsg { return tea.KeyPressMsg(tea.Key{Code: code}) }
func plain(m *Model) string             { return ansi.Strip(m.render()) }

func newModel(t *testing.T, f *fake) (*Model, *clock) {
	t.Helper()
	ctx, cancel := context.WithCancel(context.Background())
	t.Cleanup(cancel)
	c := newClock()
	m := New(ctx, f, Options{Root: "/r", Home: "/h", Now: c.now})
	m.Update(tea.WindowSizeMsg{Width: 120, Height: 30})
	return m, c
}

// runCmd runs a command tree synchronously and returns the messages it
// produced, skipping ticks.
func runCmd(cmd tea.Cmd) []tea.Msg {
	if cmd == nil {
		return nil
	}
	msg := cmd()
	switch v := msg.(type) {
	case tea.BatchMsg:
		var out []tea.Msg
		var wg sync.WaitGroup
		var mu sync.Mutex
		for _, c := range v {
			wg.Add(1)
			go func() {
				defer wg.Done()
				r := runCmd(c)
				mu.Lock()
				out = append(out, r...)
				mu.Unlock()
			}()
		}
		wg.Wait()
		return out
	case tickMsg:
		return nil
	case nil:
		return nil
	}
	return []tea.Msg{msg}
}

// pump feeds every message a command produces back into the model, until
// none are left.
func pump(m *Model, cmd tea.Cmd) {
	for _, msg := range runCmd(cmd) {
		_, next := m.Update(msg)
		pump(m, next)
	}
}

func agent(thread string, pid uint32, label string) data.Agent {
	return data.Agent{PID: pid, ThreadID: ptr(thread), Role: ptr("orchestrator"), Uptime: "1m", Run: data.RunStatus{State: "tool", Label: label}}
}

func TestFirstRefreshShowsStatusAndAgents(t *testing.T) {
	f := &fake{agents: func(context.Context) ([]data.Agent, error) {
		return []data.Agent{agent("thread-a", 1, "bash: ls")}, nil
	}}
	m, _ := newModel(t, f)
	if !strings.Contains(plain(m), "Not loaded yet") {
		t.Fatal("an unfetched tab must say it is not loaded")
	}
	pump(m, m.Init())
	out := plain(m)
	for _, want := range []string{"● connected · ? help", "Active agents  1 run", "bash: ls", "Thread     thread-a", "updated 0s ago"} {
		if !strings.Contains(out, want) {
			t.Fatalf("screen lacks %q:\n%s", want, out)
		}
	}
}

func TestSlowFetchNeverRunsTwice(t *testing.T) {
	release := make(chan struct{})
	f := &fake{agents: func(ctx context.Context) ([]data.Agent, error) {
		select {
		case <-release:
		case <-ctx.Done():
		}
		return nil, nil
	}}
	m, c := newModel(t, f)
	first := m.start(keyAgents, c.now())
	if first == nil {
		t.Fatal("first fetch must start")
	}
	for i := 0; i < 10; i++ {
		c.add(5 * time.Second)
		for _, k := range m.due(c.now()) {
			if k == keyAgents {
				t.Fatal("agents must not be due while a fetch runs")
			}
		}
	}
	done := make(chan tea.Msg)
	go func() { done <- first() }()
	close(release)
	m.Update(<-done)
	if f.n("agents") != 1 {
		t.Fatalf("agents called %d times", f.n("agents"))
	}
	c.add(3 * time.Second)
	if !containsKey(m.due(c.now()), keyAgents) {
		t.Fatal("agents must be due again after the interval")
	}
}

func containsKey(keys []fetchKey, k fetchKey) bool {
	for _, x := range keys {
		if x == k {
			return true
		}
	}
	return false
}

func TestSelectionFollowsTheRowAcrossRefreshes(t *testing.T) {
	rows := []data.Agent{agent("a", 1, "one"), agent("b", 2, "two"), agent("c", 3, "three")}
	f := &fake{agents: func(context.Context) ([]data.Agent, error) { return rows, nil }}
	m, c := newModel(t, f)
	pump(m, m.Init())
	m.Update(key("j"))
	if a := m.agents.selectedAgent(); a == nil || *a.ThreadID != "b" {
		t.Fatalf("selected %+v", a)
	}
	rows = []data.Agent{agent("new", 9, "zero"), agent("a", 1, "one"), agent("b", 2, "two")}
	c.add(3 * time.Second)
	pump(m, m.refresh(c.now()))
	if a := m.agents.selectedAgent(); a == nil || *a.ThreadID != "b" {
		t.Fatalf("selection moved to %+v", a)
	}
	rows = rows[:1]
	c.add(3 * time.Second)
	pump(m, m.refresh(c.now()))
	if a := m.agents.selectedAgent(); a == nil || *a.ThreadID != "new" {
		t.Fatalf("a vanished row must clamp, got %+v", a)
	}
}

func TestFailedRefreshKeepsDataAndSaysStale(t *testing.T) {
	fail := false
	f := &fake{agents: func(context.Context) ([]data.Agent, error) {
		if fail {
			return nil, errors.New("socket gone")
		}
		return []data.Agent{agent("a", 1, "busy")}, nil
	}}
	m, c := newModel(t, f)
	pump(m, m.Init())
	fail = true
	c.add(20 * time.Second)
	pump(m, m.refresh(c.now()))
	out := plain(m)
	if !strings.Contains(out, "busy") || !strings.Contains(out, "Could not refresh agents: socket gone. Showing data from 20s ago.") || !strings.Contains(out, "stale 20s") {
		t.Fatalf("screen:\n%s", out)
	}
}

func TestUnavailableServiceSaysSoAndNamesTheFallback(t *testing.T) {
	f := &fake{status: func(context.Context) (*wire.StatusReport, error) {
		return nil, wire.ErrNotRunning
	}, agents: func(context.Context) ([]data.Agent, error) { return nil, wire.ErrNotRunning }}
	m, _ := newModel(t, f)
	pump(m, m.Init())
	out := plain(m)
	if !strings.Contains(lastLine(out), "○ disconnected · ? help") || !strings.Contains(out, "--cli-fallback") {
		t.Fatalf("screen:\n%s", out)
	}
	m.Update(tea.WindowSizeMsg{Width: 200, Height: 80})
	m.Update(key("?"))
	help := plain(m)
	for _, want := range []string{"state                   ○ disconnected", "need the orchestrator (or --cli-fallback)"} {
		if !strings.Contains(help, want) {
			t.Fatalf("help lacks %q:\n%s", want, help)
		}
	}
}

func lastLine(s string) string {
	lines := strings.Split(strings.TrimRight(s, "\n"), "\n")
	return lines[len(lines)-1]
}

// TestViewsRootMismatchIsShown: no banner; the tabs that answer for the
// views root say so in the footer, and help has both full paths.
func TestViewsRootMismatchIsShown(t *testing.T) {
	f := &fake{status: func(context.Context) (*wire.StatusReport, error) {
		r := modernStatus()
		r.ViewsRoot = ptr("/elsewhere/Documents")
		return r, nil
	}}
	m, _ := newModel(t, f)
	pump(m, m.Init())
	if out := plain(m); strings.Contains(out, "scope") || strings.Contains(out, "/elsewhere") {
		t.Fatalf("the Agents tab does not depend on the views root:\n%s", out)
	}
	for _, k := range []string{"5", "8"} {
		_, cmd := m.Update(key(k))
		pump(m, cmd)
		if foot := lastLine(plain(m)); !strings.Contains(foot, "! scope /elsewhere/Documents · ● connected · ? help") {
			t.Fatalf("tab %s footer: %q", k, foot)
		}
	}
	m.Update(tea.WindowSizeMsg{Width: 200, Height: 80})
	m.Update(key("?"))
	help := plain(m)
	if !strings.Contains(help, "The service reads views for /elsewhere/Documents, not --root /r.") || !strings.Contains(help, "--root /elsewhere/Documents") {
		t.Fatalf("help:\n%s", help)
	}
}

func TestConnectionStatesInTheFooter(t *testing.T) {
	cases := []struct {
		name   string
		status func(context.Context) (*wire.StatusReport, error)
		want   string
	}{
		{"connected", func(context.Context) (*wire.StatusReport, error) { return modernStatus(), nil }, "● connected · ? help"},
		{"busy is still connected", func(context.Context) (*wire.StatusReport, error) {
			r := modernStatus()
			r.Settled = false
			return r, nil
		}, "● connected · ? help"},
		{"old service", func(context.Context) (*wire.StatusReport, error) {
			return &wire.StatusReport{Settled: true}, nil
		}, "● connected, views missing · ? help"},
		{"stopped", func(context.Context) (*wire.StatusReport, error) { return nil, wire.ErrNotRunning }, "○ disconnected · ? help"},
		{"unreachable", func(context.Context) (*wire.StatusReport, error) { return nil, errors.New("permission denied") }, "○ unreachable · ? help"},
	}
	for _, tc := range cases {
		m, _ := newModel(t, &fake{status: tc.status})
		pump(m, m.Init())
		out := plain(m)
		if foot := lastLine(out); !strings.Contains(foot, tc.want) {
			t.Errorf("%s: footer %q", tc.name, foot)
		}
		if strings.Count(out, "? help") != 1 || strings.Contains(out, "orchestrator busy") || strings.Contains(out, "zdx monitor") {
			t.Errorf("%s: help or a header repeated:\n%s", tc.name, out)
		}
		if first := strings.Split(out, "\n")[0]; !strings.Contains(first, "1 Agents") {
			t.Errorf("%s: the tab bar must be the first line: %q", tc.name, first)
		}
	}
	// Before the first answer.
	m, _ := newModel(t, &fake{})
	if foot := lastLine(plain(m)); !strings.Contains(foot, "connecting · ? help") {
		t.Fatalf("footer %q", foot)
	}
}

func TestThreadKindToggleDropsTheOlderAnswer(t *testing.T) {
	gate := make(chan struct{})
	f := &fake{threads: func(ctx context.Context, kind string) (data.ThreadList, error) {
		if kind == "all" {
			<-gate
			return data.ThreadList{Threads: []data.ThreadListItem{{ID: "all-1", Title: "From all"}}}, nil
		}
		return data.ThreadList{Threads: []data.ThreadListItem{{ID: "w-1", Title: "A worker", IsWorker: true}}}, nil
	}}
	m2, _ := newModel(t, f)
	m2.tab = tabThreads
	allCmd := m2.start(keyThreads, m2.now())
	m2.Update(key("t"))
	if m2.threads.kind != "workers" {
		t.Fatal("t must switch to workers")
	}
	close(gate)
	_, next := m2.Update(allCmd())
	if m2.threads.data != nil {
		t.Fatal("an answer for the old kind must be dropped")
	}
	pump(m2, next)
	if m2.threads.data == nil || m2.threads.data.Threads[0].ID != "w-1" {
		t.Fatalf("got %+v", m2.threads.data)
	}
	if !strings.Contains(plain(m2), "A worker") {
		t.Fatalf("screen:\n%s", plain(m2))
	}
}

func TestMergeThreadKeepsPersistedAndReplacesRunningTools(t *testing.T) {
	old := &data.Thread{Cursor: 3, Activity: []data.Activity{
		{Type: "message", Sequence: 1, Text: "hi"},
		{Type: "tool_use", Sequence: 2, ID: "c1", Name: "bash"},
		{Type: "tool_running", Sequence: 3, ID: "c1", Name: "bash", RunningFor: "2s"},
	}}
	delta := data.Thread{Partial: true, Cursor: 4, Activity: []data.Activity{
		{Type: "tool_result", Sequence: 3, ToolUseID: "c1"},
	}}
	got := transcript.Merge(old, delta)
	var types []string
	for _, a := range got.Activity {
		types = append(types, a.Type)
	}
	if strings.Join(types, ",") != "message,tool_use,tool_result" || got.Cursor != 4 {
		t.Fatalf("got %v cursor %d", types, got.Cursor)
	}
	full := transcript.Merge(old, data.Thread{Activity: []data.Activity{{Type: "message", Sequence: 1}}})
	if len(full.Activity) != 1 {
		t.Fatal("a full answer replaces")
	}
}

func TestTranscriptPollsWithCursorAndOpensToolDetail(t *testing.T) {
	var afters []string
	var mu sync.Mutex
	f := &fake{
		agents: func(context.Context) ([]data.Agent, error) { return []data.Agent{agent("thread-a", 1, "bash")}, nil },
		thread: func(_ context.Context, id string, after *uint64) (data.Thread, error) {
			mu.Lock()
			defer mu.Unlock()
			if after == nil {
				afters = append(afters, "nil")
				return data.Thread{ID: id, Title: "Fixing tests", Cursor: 3, Activity: []data.Activity{
					{Type: "message", Sequence: 0, Role: "user", Speaker: "user", Text: "run the tests"},
					{Type: "reasoning", Sequence: 1, Text: "secret plan line"},
					{Type: "tool_use", Sequence: 2, ID: "c1", Name: "bash", Input: []byte(`{"command":"go test"}`)},
					{Type: "tool_running", Sequence: 3, ID: "c1", Name: "bash", RunningFor: "4s", OutputTail: "=== RUN",
						View: &data.ToolView{Outcome: "pending", Head: &data.ToolHead{Kind: "shell", Command: "go test ./..."}}},
				}}, nil
			}
			afters = append(afters, "3")
			return data.Thread{ID: id, Title: "Fixing tests", Partial: true, Cursor: 4, Activity: []data.Activity{
				{Type: "tool_result", Sequence: 3, ToolUseID: "c1", OK: ptr(true), View: &data.ToolView{
					Outcome: "ok", Head: &data.ToolHead{Kind: "shell", Command: "go test ./..."},
					Body:   data.ToolBody{Kind: "streams", Streams: []data.OutputStream{{Name: "stdout", Text: "ok  pkg 0.1s"}}},
					Status: []data.ToolFact{{Text: "exit 0", Kind: "outcome"}},
				}},
			}}, nil
		},
	}
	m, c := newModel(t, f)
	pump(m, m.Init())
	_, cmd := m.Update(special(tea.KeyEnter))
	pump(m, cmd)
	out := plain(m)
	for _, want := range []string{"Fixing tests", "run the tests", "∴ thinking · 1 line", "bash $ go test ./... running 4s"} {
		if !strings.Contains(out, want) {
			t.Fatalf("transcript lacks %q:\n%s", want, out)
		}
	}
	if strings.Contains(out, "secret plan line") {
		t.Fatal("thinking must start collapsed")
	}
	m.Update(key("t"))
	if !strings.Contains(plain(m), "secret plan line") {
		t.Fatal("t must expand thinking")
	}
	c.add(3 * time.Second)
	pump(m, m.refresh(c.now()))
	out = plain(m)
	if !strings.Contains(out, "✓ bash $ go test ./...  exit 0") || strings.Contains(out, "running 4s") {
		t.Fatalf("the result must replace the running row:\n%s", out)
	}
	m.Update(special(tea.KeyTab))
	m.Update(special(tea.KeyEnter))
	out = plain(m)
	for _, want := range []string{"── stdout ──", "ok  pkg 0.1s", "input", `"command": "go test"`} {
		if !strings.Contains(out, want) {
			t.Fatalf("tool detail lacks %q:\n%s", want, out)
		}
	}
	m.Update(special(tea.KeyEscape))
	m.Update(special(tea.KeyEscape))
	if m.transcript != nil {
		t.Fatal("esc twice must close detail, then the transcript")
	}
	mu.Lock()
	defer mu.Unlock()
	if strings.Join(afters, ",") != "nil,3" {
		t.Fatalf("polls %v", afters)
	}
}

func TestClosedTranscriptDropsItsLateAnswer(t *testing.T) {
	gate := make(chan struct{})
	f := &fake{thread: func(ctx context.Context, id string, _ *uint64) (data.Thread, error) {
		select {
		case <-gate:
		case <-ctx.Done():
			return data.Thread{}, ctx.Err()
		}
		return data.Thread{ID: id, Title: "late"}, nil
	}}
	m, _ := newModel(t, f)
	cmd := m.openTranscript("t-1", "t-1")
	m.closeTranscript()
	close(gate)
	pump(m, cmd)
	if m.transcript != nil {
		t.Fatal("a late answer must not reopen the transcript")
	}
	if m.fetch[keyTranscript].inflight {
		t.Fatal("the fetch must be finished")
	}
}

func TestLogsFilterByLevelAndQuery(t *testing.T) {
	f := &fake{logs: func(int, int, source.LogStamp) (source.LogTail, error) {
		return source.LogTail{Files: []string{"zdx.log.2026-11-14"}, File: "zdx.log.2026-11-14", Stamp: source.LogStamp{Size: 1}, Lines: []string{
			"2026-11-14T03:15:27Z  INFO zdx_engine::skills: Loaded skills",
			"2026-11-14T03:15:28Z  WARN zdx_engine::core: slow sync",
			"   continuation of the warning",
			"2026-11-14T03:15:29Z ERROR zdx_engine::turn: turn failed",
		}}, nil
	}}
	m, _ := newModel(t, f)
	_, cmd := m.Update(key("6"))
	pump(m, cmd)
	if got := len(m.logs.visible); got != 4 {
		t.Fatalf("visible %d", got)
	}
	m.Update(key("l"))
	m.Update(key("l"))
	m.Update(key("l")) // WARN+
	if got := len(m.logs.visible); got != 3 {
		t.Fatalf("WARN+ must keep the warning, its continuation, and the error; got %d", got)
	}
	m.Update(key("/"))
	for _, r := range "failed" {
		m.Update(key(string(r)))
	}
	m.Update(special(tea.KeyEnter))
	if got := len(m.logs.visible); got != 1 || !strings.Contains(plain(m), "turn failed") {
		t.Fatalf("query: %d visible\n%s", got, plain(m))
	}
	m.Update(key("q")) // a letter outside the editor quits; check the editor took the earlier ones
	if !m.quitting {
		t.Fatal("q must quit outside the editor")
	}
}

func TestQuitCancelsRunningFetches(t *testing.T) {
	started := make(chan struct{})
	var cancelled atomic.Bool
	f := &fake{agents: func(ctx context.Context) ([]data.Agent, error) {
		close(started)
		<-ctx.Done()
		cancelled.Store(true)
		return nil, ctx.Err()
	}}
	m, c := newModel(t, f)
	cmd := m.start(keyAgents, c.now())
	done := make(chan tea.Msg)
	go func() { done <- cmd() }()
	<-started
	m.Update(key("q"))
	select {
	case <-done:
	case <-time.After(2 * time.Second):
		t.Fatal("quit did not cancel the fetch")
	}
	if !cancelled.Load() {
		t.Fatal("the fetch context was not cancelled")
	}
}

func TestTrajectoryOverlayAsksForTheSourceThread(t *testing.T) {
	var asked []string
	var mu sync.Mutex
	f := &fake{
		thread: func(_ context.Context, id string, _ *uint64) (data.Thread, error) {
			return data.Thread{ID: id, Title: "Aliased", SourceID: "source-thread"}, nil
		},
		trajectory: func(id string) (data.Trajectory, error) {
			mu.Lock()
			asked = append(asked, id)
			mu.Unlock()
			d := uint64(1500)
			return data.Trajectory{SourceEventCount: 4, Measured: 1, Total: 2, Exact: 1, HasUnavailable: true,
				Bottlenecks: []data.TrajectoryBottleneck{{Label: "bash", Turn: 1, Status: "ok", Timing: "exact", DurationMS: 1500}},
				Turns: []data.TrajectoryTurn{{Index: 1, Timing: "partial", Measured: 1, Total: 2,
					Input: &data.TrajectoryInput{Text: "do the thing"},
					Spans: []data.TrajectorySpan{
						{Kind: "tool", Label: "bash", Detail: "go test", Status: "ok", Timing: "exact", DurationMS: &d},
						{Kind: "model", Label: "model", Status: "unknown", Timing: "unavailable"},
					}}}}, nil
		},
	}
	m, _ := newModel(t, f)
	pump(m, m.openTranscript("alias-thread", "alias-thread"))
	if !strings.Contains(plain(m), "alias of source-thread") {
		t.Fatalf("screen:\n%s", plain(m))
	}
	_, cmd := m.Update(key("i"))
	pump(m, cmd)
	out := plain(m)
	for _, want := range []string{"coverage 1/2 durations measured · 1 exact", "some spans have no timing", "1. ✓ bash · turn 1 · 1.5s", "Turn 1 · partial", "do the thing", "✓ tool  bash go test · 1.5s", "? model model · no timing · TTFT unknown"} {
		if !strings.Contains(out, want) {
			t.Fatalf("trajectory lacks %q:\n%s", want, out)
		}
	}
	mu.Lock()
	defer mu.Unlock()
	if len(asked) != 1 || asked[0] != "source-thread" {
		t.Fatalf("asked %v", asked)
	}
	m.Update(special(tea.KeyEscape))
	if m.transcript == nil || m.transcript.traj != nil {
		t.Fatal("esc closes the trajectory only")
	}
}

// Closing and reopening the timing overlay while its fetch runs must not
// start a second fetch or let the cancelled answer land.
func TestTrajectoryReopenKeepsOneFetch(t *testing.T) {
	gate, started := make(chan struct{}), make(chan struct{})
	var calls atomic.Int32
	f := &fake{trajectory: func(string) (data.Trajectory, error) {
		if calls.Add(1) == 1 {
			close(started)
			select {
			case <-gate:
			case <-time.After(5 * time.Second):
			}
			return data.Trajectory{Bottlenecks: []data.TrajectoryBottleneck{{Label: "stale-span", Status: "ok"}}}, nil
		}
		return data.Trajectory{Bottlenecks: []data.TrajectoryBottleneck{{Label: "fresh-span", Status: "ok"}}}, nil
	}}
	m, _ := newModel(t, f)
	pump(m, m.openTranscript("t-1", "t-1"))
	_, first := m.Update(key("i"))
	done := make(chan []tea.Msg)
	go func() { done <- runCmd(first) }()
	<-started
	m.Update(special(tea.KeyEscape))
	_, second := m.Update(key("i"))
	if st := m.fetch[keyTrajectory]; !st.inflight || st.cancel == nil {
		t.Errorf("the running fetch must keep the key: %+v", st)
	}
	pump(m, second)
	if n := calls.Load(); n != 1 {
		t.Errorf("a second fetch started while the first ran: %d calls", n)
	}
	close(gate)
	for _, msg := range <-done {
		_, next := m.Update(msg)
		pump(m, next)
	}
	if n := calls.Load(); n != 2 {
		t.Fatalf("trajectory fetched %d times, want 2", n)
	}
	out := plain(m)
	if !strings.Contains(out, "fresh-span") || strings.Contains(out, "stale-span") {
		t.Fatalf("the reopened overlay must show only the new answer:\n%s", out)
	}
}

// A tool_running item with completed_at is a finished call whose round is
// not saved yet: it must not read as running.
func TestFinishedLiveToolIsNotRunning(t *testing.T) {
	f := &fake{thread: func(_ context.Context, id string, _ *uint64) (data.Thread, error) {
		pending := &data.ToolView{Outcome: "pending", Head: &data.ToolHead{Kind: "shell", Command: "make"}}
		return data.Thread{ID: id, Cursor: 3, Activity: []data.Activity{
			{Type: "tool_running", Sequence: 1, ID: "c1", Name: "bash", RunningFor: "3s", CompletedAt: ptr("2026-01-01T00:00:03Z"), OK: ptr(false), View: pending},
			{Type: "tool_running", Sequence: 2, ID: "c2", Name: "bash", RunningFor: "1s", CompletedAt: ptr("2026-01-01T00:00:01Z"), View: pending},
			{Type: "tool_running", Sequence: 3, ID: "c3", Name: "bash", RunningFor: "9s", View: pending},
		}}, nil
	}}
	m, _ := newModel(t, f)
	pump(m, m.openTranscript("t-1", "t-1"))
	out := plain(m)
	for _, want := range []string{"✗ bash $ make finished 3s, not saved yet", "? bash $ make finished 1s, not saved yet", "… bash $ make running 9s"} {
		if !strings.Contains(out, want) {
			t.Fatalf("transcript lacks %q:\n%s", want, out)
		}
	}
	if strings.Count(out, "make running") != 1 {
		t.Fatalf("only the unfinished call runs:\n%s", out)
	}
}

// A delta whose cursor went back (the thread was reset) or whose source
// changed (the alias moved) is not merged: the transcript loads again.
func TestTranscriptReloadsAfterHistoryReset(t *testing.T) {
	var mu sync.Mutex
	var polls []string
	reset := false
	f := &fake{thread: func(_ context.Context, id string, after *uint64) (data.Thread, error) {
		mu.Lock()
		defer mu.Unlock()
		switch {
		case after == nil && !reset:
			polls = append(polls, "full")
			return data.Thread{ID: id, Cursor: 10, Activity: []data.Activity{{Type: "message", Sequence: 9, Role: "user", Speaker: "user", Text: "old history"}}}, nil
		case after == nil:
			polls = append(polls, "full")
			return data.Thread{ID: id, Cursor: 2, Activity: []data.Activity{{Type: "message", Sequence: 1, Role: "user", Speaker: "user", Text: "new history"}}}, nil
		}
		polls = append(polls, "delta")
		reset = true
		return data.Thread{ID: id, Partial: true, Cursor: 2}, nil
	}}
	m, c := newModel(t, f)
	pump(m, m.openTranscript("t-1", "t-1"))
	c.add(3 * time.Second)
	pump(m, m.refresh(c.now()))
	out := plain(m)
	if !strings.Contains(out, "new history") || strings.Contains(out, "old history") {
		t.Fatalf("a reset thread must load again:\n%s", out)
	}
	mu.Lock()
	defer mu.Unlock()
	if strings.Join(polls, ",") != "full,delta,full" {
		t.Fatalf("polls %v", polls)
	}
	m.transcript.thread.SourceID = "other"
	if m.transcript.apply(transcriptResult{id: "t-1", after: ptr(uint64(2)), thread: data.Thread{Partial: true, Cursor: 3, SourceID: "moved"}}, 80) {
		t.Fatal("a delta from another source must not merge")
	}
}

func TestAttachmentOnlyMessageShowsItsFiles(t *testing.T) {
	f := &fake{thread: func(_ context.Context, id string, _ *uint64) (data.Thread, error) {
		return data.Thread{ID: id, Cursor: 1, Activity: []data.Activity{{Type: "message", Sequence: 0, Role: "assistant", Speaker: "zdx",
			Artifacts: []data.Attachment{{Name: "report.html", Path: "/tmp/a/report.html", Kind: "html", Exists: true}, {Name: "gone.png", Path: "/tmp/a/gone.png", Kind: "image"}}}}}, nil
	}}
	m, _ := newModel(t, f)
	pump(m, m.openTranscript("t-1", "t-1"))
	out := plain(m)
	for _, want := range []string{"⎘ report.html (html) /tmp/a/report.html", "⎘ gone.png (image, missing)"} {
		if !strings.Contains(out, want) {
			t.Fatalf("transcript lacks %q:\n%s", want, out)
		}
	}
}
