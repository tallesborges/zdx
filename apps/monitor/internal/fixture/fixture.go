// Package fixture is a synthetic Backend for previews and snapshot tests:
// realistic engine answers (long ids, models, paths, failures) in a few
// scenarios. It never touches a ZDX home, a socket, or a binary.
package fixture

import (
	"context"
	"errors"
	"fmt"
	"sync"
	"time"

	"github.com/tallesborges/zdx/apps/monitor/internal/data"
	"github.com/tallesborges/zdx/apps/monitor/internal/source"
	"github.com/tallesborges/zdx/apps/monitor/internal/wire"
)

// Scenario names a backend state.
type Scenario string

const (
	// Full: a busy system with every surface populated.
	Full Scenario = "full"
	// Empty: everything answers, with nothing to show.
	Empty Scenario = "empty"
	// Down: the engine is not running; CLI and logs still answer.
	Down Scenario = "down"
	// Loading: no fetch ever answers.
	Loading Scenario = "loading"
	// Stale: the first answer of each surface works, later ones fail.
	Stale Scenario = "stale"
	// Scope: Full, with the service reading views for another root than
	// the monitor's --root.
	Scope Scenario = "scope"
)

// Scenarios lists every scenario.
var Scenarios = []Scenario{Full, Empty, Down, Loading, Stale, Scope}

// ScopeRoot is the views root of the Scope scenario.
const ScopeRoot = "/Users/example/Documents/projects/example-app"

// Base is the fixture clock's start.
var Base = time.Date(2026, 11, 14, 14, 30, 0, 0, time.UTC)

// Backend answers from fixture data.
type Backend struct {
	Scenario Scenario
	Now      func() time.Time

	mu    sync.Mutex
	calls map[string]int
}

// New returns a backend for s.
func New(s Scenario, now func() time.Time) *Backend {
	if now == nil {
		now = func() time.Time { return Base }
	}
	return &Backend{Scenario: s, Now: now, calls: map[string]int{}}
}

var errDown = fmt.Errorf("%w (no listener at /Users/example/.zdx/run/engine.sock)", wire.ErrNotRunning)

// gate applies the scenario: Loading blocks until ctx ends, Stale fails
// after the first call of a surface, Down fails socket calls.
func (b *Backend) gate(ctx context.Context, name string, socket bool) error {
	b.mu.Lock()
	b.calls[name]++
	n := b.calls[name]
	b.mu.Unlock()
	switch b.Scenario {
	case Loading:
		<-ctx.Done()
		return ctx.Err()
	case Down:
		if socket {
			return errDown
		}
	case Stale:
		if n > 1 {
			return errors.New("the engine service did not answer within 30s")
		}
	}
	return nil
}

func ptr[T any](v T) *T { return &v }

func (b *Backend) at(d time.Duration) string { return b.Now().Add(d).UTC().Format(time.RFC3339) }

// Status is the socket status report.
func (b *Backend) Status(ctx context.Context) (*wire.StatusReport, error) {
	if err := b.gate(ctx, "status", true); err != nil {
		return nil, err
	}
	r := &wire.StatusReport{Protocol: ptr(uint32(2)), Capabilities: []string{wire.CapThreadViews, wire.CapMonitorViews},
		ViewsRoot: ptr("/Users/example/Documents")}
	if b.Scenario == Scope {
		r.ViewsRoot = ptr(ScopeRoot)
	}
	if b.Scenario == Empty {
		r.Settled = true
		return r, nil
	}
	r.Threads = []wire.ThreadStatus{
		{ThreadID: "telegram--1001234567890-topic-42", Profile: ptr("orchestrator"), State: "running", RunningTurn: ptr(uint64(12)), QueuedInputs: 1,
			Workers: []wire.WorkerSnapshot{{ThreadID: "w-1", Status: "running"}, {ThreadID: "w-2", Status: "completed"}, {ThreadID: "w-3", Status: "failed"}}},
		{ThreadID: "telegram--1001234567890-topic-512", Profile: ptr("orchestrator"), State: "delivering"},
		{ThreadID: "telegram--1001234567890-topic-9", Profile: ptr("orchestrator"), State: "idle"},
	}
	return r, nil
}

// Services is `zdx service status --json`.
func (b *Backend) Services(ctx context.Context) (data.ServiceStatus, error) {
	if err := b.gate(ctx, "services", false); err != nil {
		return data.ServiceStatus{}, err
	}
	if b.Scenario == Empty {
		return data.ServiceStatus{Services: []data.ServiceState{
			{Service: "bot", Label: "dev.zdx.bot", Status: "stopped"},
			{Service: "scheduler", Label: "dev.zdx.scheduler", Status: "stopped"},
			{Service: "engine", Label: "dev.zdx.engine", Status: "stopped"},
		}}, nil
	}
	orch := data.ServiceState{Service: "engine", Label: "dev.zdx.engine", Installed: true, Status: "running", PID: ptr(uint32(48213)), UptimeSecs: ptr(uint64(3*3600 + 12*60))}
	if b.Scenario == Down {
		orch = data.ServiceState{Service: "engine", Label: "dev.zdx.engine", Installed: true, Status: "stopped"}
	}
	return data.ServiceStatus{Services: []data.ServiceState{
		{Service: "bot", Label: "dev.zdx.bot", Installed: true, Status: "running", PID: ptr(uint32(48190)), UptimeSecs: ptr(uint64(3*3600 + 14*60))},
		{Service: "scheduler", Label: "dev.zdx.scheduler", Installed: false, Status: "stopped"},
		orch,
	}, QueuedRestart: []byte("null")}, nil
}

func (b *Backend) snapshot() data.Snapshot {
	s := data.Snapshot{GeneratedAt: b.at(0), Config: data.MonitorConfig{
		Model: "claude-cli:claude-opus-5-5@high", ModelMode: ptr("mode:deep"), Thinking: "high", ModeCount: 3, SubagentsEnabled: true, ServerPort: 4141,
		HelperModels: []data.HelperModel{{Role: "title", Model: "gemini:gemini-3.5-flash-lite"}},
	}}
	if b.Scenario == Empty {
		return s
	}
	s.BackgroundProcesses = []data.BackgroundProcess{
		{ID: "bg-7f3a91", PID: 51022, ThreadID: ptr("telegram--1001234567890-topic-42"), Command: "cd apps/web && bun run dev --host 127.0.0.1 --port 5173", Uptime: "42m 10s"},
		{ID: "bg-02cc1e", PID: 51877, ThreadID: ptr("4f6c2d0e-9b1a-4c55-8f3e-0a7d2b9e1c44"), Command: "cargo watch -x 'test -p zdx-engine --lib'", Uptime: "6m"},
		{ID: "bg-91d0aa", PID: 52001, Command: "tail -f /Users/example/Library/Logs/some-very-long-application-name/output.log", Uptime: "1h"},
	}
	return s
}

// Snapshot is `monitor_snapshot`.
func (b *Backend) Snapshot(ctx context.Context) (data.Snapshot, source.Origin, error) {
	if err := b.gate(ctx, "snapshot", true); err != nil {
		return data.Snapshot{}, source.OriginSocket, err
	}
	return b.snapshot(), source.OriginSocket, nil
}

func secs(v uint64) *uint64 { return &v }

// Agents are the active runs of Full, oldest first as the engine lists
// them: workers and subagents can start before their orchestrator's
// current turn.
func (b *Backend) Agents() []data.Agent {
	orch := "telegram--1001234567890-topic-42"
	worker := "9c3fef80-e9c8-4eb3-be2f-6d5cf0eaf22e"
	return []data.Agent{
		{PID: 48902, ThreadID: ptr(worker), ParentThreadID: ptr(orch),
			Surface: ptr("worker"), Role: ptr("worker"), Model: ptr("claude-cli@work:claude-opus-5-5"), Provider: ptr("claude-cli"), Thinking: ptr("high"), Uptime: "100h 3m",
			Run: data.RunStatus{State: "thinking", Label: "thinking", StartedAt: b.at(-3 * time.Minute), TurnElapsedSecs: secs(180), SecondsSinceActivity: secs(0), SecondsSinceBeat: secs(2)}},
		{PID: 49110, ThreadID: ptr("4f6c2d0e-9b1a-4c55-8f3e-0a7d2b9e1c44"), ParentThreadID: ptr(worker),
			Surface: ptr("exec"), Role: ptr("explorer"), Model: ptr("gemini-3.8-flash-preview-thinking-exp-2026-11"), Provider: ptr("google-vertex-anthropic-compatible"), Account: ptr("personal-research-account"), Thinking: ptr("minimal"), Uptime: "2m",
			Run: data.RunStatus{State: "retrying", Label: "retrying after 529 overloaded (attempt 2 of 5)", StartedAt: b.at(-2 * time.Minute), TurnElapsedSecs: secs(120), SecondsSinceActivity: secs(48)}},
		{PID: 48213, ThreadID: ptr(orch), Surface: ptr("telegram"), Role: ptr("orchestrator"),
			Model: ptr("gpt-6-astra"), Provider: ptr("openai-codex"), Account: ptr("work"), Thinking: ptr("high"), Uptime: "13m 25s",
			Run: data.RunStatus{State: "tool", Label: "bash: go test -race ./... (+1 more)", StartedAt: b.at(-14 * time.Minute),
				TurnElapsedSecs: secs(840), SecondsSinceActivity: secs(3), SecondsSinceBeat: secs(1), RunningTools: 2, FinishedTools: 5}},
		{PID: 49205, ThreadID: ptr("a7c1e2d3-5b6f-4a80-9c1d-2e3f4a5b6c7d"), ParentThreadID: ptr(orch),
			Surface: ptr("exec"), Role: ptr("oracle"), Model: ptr("gpt-6-astra"), Provider: ptr("openai-codex"), Thinking: ptr("xhigh"), Uptime: "41s",
			Run: data.RunStatus{State: "answering", Label: "answering", StartedAt: b.at(-41 * time.Second)}},
		{PID: 50311, ThreadID: ptr("3e9d8c7b-6a5f-4e3d-2c1b-0a9f8e7d6c5b"), ParentThreadID: ptr("telegram--1009876543210-topic-3"),
			Surface: ptr("worker"), Role: ptr("worker"), Model: ptr("claude-cli:claude-sonnet-5"), Provider: ptr("claude-cli"), Thinking: ptr("medium"), Uptime: "1h 2m",
			Run: data.RunStatus{State: "tool", Label: "edit: apps/web/src/lib/types.ts", StartedAt: b.at(-62 * time.Minute)}},
		{PID: 49377, Surface: ptr("tui"), Model: ptr("openrouter:moonshotai/kimi-k3-0905-preview-extended-context"), Uptime: "9s",
			Run: data.RunStatus{State: "waiting", Label: "waiting for model", StartedAt: b.at(-9 * time.Second)}},
	}
}

// ActiveAgents is `active_agents`.
func (b *Backend) ActiveAgents(ctx context.Context) ([]data.Agent, source.Origin, error) {
	if err := b.gate(ctx, "agents", true); err != nil {
		return nil, source.OriginSocket, err
	}
	if b.Scenario == Empty {
		return []data.Agent{}, source.OriginSocket, nil
	}
	return b.Agents(), source.OriginSocket, nil
}

// Threads is `threads_list`.
func (b *Backend) Threads(ctx context.Context, kind string) (data.ThreadList, source.Origin, error) {
	if err := b.gate(ctx, "threads", true); err != nil {
		return data.ThreadList{}, source.OriginSocket, err
	}
	if b.Scenario == Empty {
		return data.ThreadList{Threads: []data.ThreadListItem{}}, source.OriginSocket, nil
	}
	run := &data.RunStatus{State: "tool", Label: "bash: go test"}
	all := []data.ThreadListItem{
		{ID: "telegram--1001234567890-topic-42", Title: "Go monitor: polish the Bubble Tea dashboard", Project: ptr("zdx"), RootPath: ptr("/Users/example/Documents/projects/zdx"), Age: ptr("37s"),
			Workers: &data.WorkerRollup{Total: 4, Running: 1, Queued: 1, Failed: 1, Settled: 1}, Run: run},
		{ID: "9c3fef80-e9c8-4eb3-be2f-6d5cf0eaf22e", Title: "Remove the Rust bot crate and move every caller to the Go gateway", Project: ptr("zdx"), Age: ptr("12m"), IsWorker: true, Run: &data.RunStatus{State: "thinking", Label: "thinking"}},
		{ID: "telegram--1001234567890-topic-512", Title: "Weekly English practice", Project: ptr("Documents"), Age: ptr("1h 23m")},
		{ID: "6e1b7a2c-0f34-4d8e-9a61-3c5b8d2e7f90", Title: "A very long thread title that goes on and on to check how the list cuts long titles at narrow widths", Project: ptr("a-project-with-a-long-directory-name"), Age: ptr("17h 58m"),
			Workers: &data.WorkerRollup{Total: 2, Unknown: 2}},
		{ID: "b0d7e6f5-1a2b-4c3d-8e9f-0a1b2c3d4e5f", Title: "b0d7e6f5-1a2b-4c3d-8e9f-0a1b2c3d4e5f", Age: ptr("12d 4h")},
		{ID: "telegram--1009876543210-topic-3", Title: "Plan a weekend trip: hotels and flights", Project: ptr("Documents"), Age: ptr("365d 23h")},
	}
	if kind == "workers" {
		return data.ThreadList{Threads: all[1:2]}, source.OriginSocket, nil
	}
	return data.ThreadList{Threads: all}, source.OriginSocket, nil
}

// Thread is `thread_view` of any thread: one realistic transcript.
func (b *Backend) Thread(ctx context.Context, id string, after *uint64) (data.Thread, source.Origin, error) {
	if err := b.gate(ctx, "thread", true); err != nil {
		return data.Thread{}, source.OriginSocket, err
	}
	th := Transcript(id)
	if id == IdleParent {
		th = data.Thread{ID: id, SourceID: id, Title: "Plan a weekend trip: hotels and flights", TotalMessages: 4, TotalEvents: 9, Cursor: 9,
			Activity: []data.Activity{
				{Type: "message", Sequence: 0, Time: "09:02", Role: "user", Speaker: "You", Text: "Compare the hotels."},
				{Type: "usage", Sequence: 1, Time: "09:03", InputTokens: 12000, OutputTokens: 900, Model: ptr("claude-opus-5-5"), Provider: ptr("claude-cli")},
				{Type: "message", Sequence: 2, Time: "09:03", Role: "assistant", Speaker: "Z", Text: "I sent a worker to check prices."},
			}}
	}
	if after != nil {
		th.Partial = true
		th.Activity = nil
	}
	return th, source.OriginSocket, nil
}

// IdleParent is the orchestrator thread of Full whose worker runs while
// it has no run of its own.
const IdleParent = "telegram--1009876543210-topic-3"

// Transcript is a thread with every activity kind.
func Transcript(id string) data.Thread {
	ok := true
	failed := false
	dur := uint64(4200)
	return data.Thread{
		ID: id, SourceID: id, Title: "Go monitor: polish the Bubble Tea dashboard", TotalMessages: 3, TotalEvents: 14, Cursor: 14,
		ParentThreadID: ptr("telegram--1001234567890-topic-42"), ParentTitle: ptr("zdx orchestrator"), WorkerCount: 0,
		Run: &data.RunStatus{State: "tool", Label: "bash: go test -race ./..."},
		Activity: []data.Activity{
			{Type: "message", Sequence: 1, Time: "14:16", Role: "user", Speaker: "You", Text: "Polish the monitor. Check the real terminal output first, then fix layout, alignment, and key hints."},
			{Type: "reasoning", Sequence: 2, Time: "14:16", Text: "I should render every tab with fixture data first.\nThen compare compact and wide sizes.\nThen fix the header and the tab bar."},
			{Type: "tool_use", Sequence: 3, Time: "14:16", ID: "c1", Name: "read", Input: []byte(`{"path":"apps/monitor/internal/ui/model.go","offset":379,"limit":4}`)},
			{Type: "tool_result", Sequence: 4, Time: "14:16", ToolUseID: "c1", OK: &ok, View: &data.ToolView{Outcome: "ok",
				Head:   &data.ToolHead{Kind: "path", Path: "apps/monitor/internal/ui/model.go"},
				Body:   data.ToolBody{Kind: "code", FirstLine: 379, Text: "func (m *Model) render() string {\n\tif m.transcript != nil {\n\t\treturn m.renderTranscript()\n\t}"},
				Status: []data.ToolFact{{Text: "lines 379-382 of 531", Kind: "detail"}}}},
			{Type: "tool_use", Sequence: 5, Time: "14:17", ID: "c2", Name: "grep", Input: []byte(`{"pattern":"func render","paths":["apps/monitor"]}`)},
			{Type: "tool_result", Sequence: 6, Time: "14:17", ToolUseID: "c2", OK: &ok, View: &data.ToolView{Outcome: "ok",
				Head:   &data.ToolHead{Kind: "search", Pattern: "func render", Scope: ptr("apps/monitor")},
				Body:   data.ToolBody{Kind: "matches", Files: []data.MatchFile{{Path: "internal/ui/model.go", Lines: []data.MatchLine{{Line: 379, Text: "func (m *Model) render() string {"}}}}},
				Status: []data.ToolFact{{Text: "1 match in 1 file", Kind: "detail"}}}},
			{Type: "tool_use", Sequence: 7, Time: "14:18", ID: "c3", Name: "edit", Input: []byte(`{"path":"apps/monitor/internal/ui/util.go"}`)},
			{Type: "tool_result", Sequence: 8, Time: "14:18", ToolUseID: "c3", OK: &ok, View: &data.ToolView{Outcome: "ok",
				Head:   &data.ToolHead{Kind: "path", Path: "apps/monitor/internal/ui/util.go"},
				Body:   data.ToolBody{Kind: "diff", Additions: 2, Deletions: 1, Lines: []data.DiffLine{{Op: "ctx", Text: "func fit(s string, width int) string {"}, {Op: "del", Text: "\treturn ansi.Truncate(s, width, \"…\")"}, {Op: "add", Text: "\tif width <= 0 {"}, {Op: "add", Text: "\t\treturn \"\""}}},
				Status: []data.ToolFact{{Text: "+2 -1", Kind: "detail"}}}},
			{Type: "usage", Sequence: 9, Time: "14:18", InputTokens: 48211, OutputTokens: 1830, CacheReadTokens: 41000, Model: ptr("claude-opus-5-5")},
			{Type: "tool_use", Sequence: 10, Time: "14:19", ID: "c4", Name: "bash", Input: []byte(`{"command":"go vet ./..."}`)},
			{Type: "tool_result", Sequence: 11, Time: "14:19", ToolUseID: "c4", OK: &failed, DurationMS: &dur, View: &data.ToolView{Outcome: "failed",
				Head:   &data.ToolHead{Kind: "shell", Command: "go vet ./..."},
				Body:   data.ToolBody{Kind: "streams", Streams: []data.OutputStream{{Name: "stderr", Text: "internal/ui/trajectory.go:5:2: \"strings\" imported and not used\n"}}},
				Status: []data.ToolFact{{Text: "exit 1", Bad: true, Kind: "outcome"}, {Text: "4.2s", Kind: "duration"}}}},
			{Type: "message", Sequence: 12, Time: "14:20", Role: "assistant", Speaker: "Z", Text: MarkdownReply},
			{Type: "notice", Sequence: 13, Time: "14:20", Kind: "context", Message: "Context is 81% full; older tool output will be compacted."},
			{Type: "tool_running", Sequence: 14, Time: "14:21", ID: "c5", Name: "bash", RunningFor: "38s", OutputTail: "=== RUN   TestProgramQuitsCleanly\n--- PASS: TestProgramQuitsCleanly (2.77s)",
				Input: []byte(`{"command":"go test -race ./..."}`), View: &data.ToolView{Outcome: "pending", Head: &data.ToolHead{Kind: "shell", Command: "go test -race ./..."}, Body: data.ToolBody{Kind: "empty"}}},
		},
	}
}

// MarkdownReply is an assistant answer with the Markdown models write:
// bold, inline code, nested lists, a fenced block with a long line, and a
// link.
const MarkdownReply = "**Vet found an unused import in `trajectory.go`.**\n\n" +
	"- `go vet` checks the **whole** module.\n" +
	"- The import was `strings`; nothing used it after the refactor.\n" +
	"  - The race tests run next.\n\n" +
	"**How to check it**\n\n" +
	"Run this in your terminal:\n\n" +
	"```sh\n" +
	"go vet ./... && go test -race -count=1 ./internal/ui ./internal/source ./internal/wire\n" +
	"```\n\n" +
	"See [the vet docs](https://pkg.go.dev/cmd/vet) for the full list of checks."

// Trajectory is `thread_trajectory`.
func (b *Backend) Trajectory(ctx context.Context, id string) (data.Trajectory, source.Origin, error) {
	if err := b.gate(ctx, "trajectory", true); err != nil {
		return data.Trajectory{}, source.OriginSocket, err
	}
	d := func(v uint64) *uint64 { return &v }
	return data.Trajectory{SourceEventCount: 14, ObservedAt: b.at(0), Measured: 5, Total: 6, Exact: 4, HasUnavailable: true,
		ActiveWallMS: d(312000), SpanWorkMS: d(286000), ConcurrentWorkMS: d(0),
		Bottlenecks: []data.TrajectoryBottleneck{{Kind: "model", Turn: 1, Label: "claude-opus-5-5", Status: "ok", Timing: "exact", DurationMS: 182000}, {Kind: "tool", Turn: 1, Label: "bash", Status: "failed", Timing: "exact", DurationMS: 4200}},
		Turns: []data.TrajectoryTurn{{Index: 1, Timing: "partial", Measured: 5, Total: 6, ElapsedMS: d(312000), ActiveWallMS: d(300000), SpanWorkMS: d(286000),
			Input: &data.TrajectoryInput{Text: "Polish the monitor. Check the real terminal output first."},
			Spans: []data.TrajectorySpan{
				{Kind: "model", Label: "claude-opus-5-5", Detail: "claude-cli", Status: "ok", Timing: "exact", DurationMS: d(182000), TTFTMS: d(2400), InputTokens: d(48211), OutputTokens: d(1830)},
				{Kind: "tool", Label: "read", Detail: "apps/monitor/internal/ui/model.go", Status: "ok", Timing: "exact", DurationMS: d(12)},
				{Kind: "tool", Label: "bash", Detail: "go vet ./...", Status: "failed", Timing: "exact", DurationMS: d(4200)},
				{Kind: "tool", Label: "bash", Detail: "go test -race ./...", Status: "running", Timing: "unavailable"},
			}}}}, source.OriginSocket, nil
}

// Usage is `usage_report`.
func (b *Backend) Usage(ctx context.Context, days uint32) (data.Usage, source.Origin, error) {
	if err := b.gate(ctx, "usage", true); err != nil {
		return data.Usage{}, source.OriginSocket, err
	}
	if b.Scenario == Empty {
		return data.Usage{Days: days}, source.OriginSocket, nil
	}
	day := int32(Base.Unix() / 86400)
	var daily []data.DailyUsage
	for i := int32(int(days) - 1); i >= 0; i-- {
		daily = append(daily, data.DailyUsage{Day: day - i, Tokens: uint64(4_000_000 + (int(i)*7919)%9*1_300_000)})
	}
	return data.Usage{Days: days, Requests: 18342, Tokens: 412_880_311, Input: 391_002_118, Output: 3_918_220, CacheRead: 301_440_000, CacheWrite: 17_960_000,
		BilledUSD: 38.27, SubscriptionTokens: 380_112_004, UnknownPricingRows: 12, ThreadsScanned: 1844, UnindexedThreads: 3,
		ByProvider: []data.UsageRow{{Provider: "claude-cli", Requests: 9211, Tokens: 251_000_000, Subscription: true}, {Provider: "openai-codex", Requests: 6120, Tokens: 129_112_004, Subscription: true},
			{Provider: "gemini", Requests: 3011, Tokens: 32_768_307, CostUSD: 38.27, Estimated: true}},
		ByModel: []data.UsageRow{{Provider: "claude-cli", Model: ptr("claude-opus-5-5"), Requests: 7000, Tokens: 210_000_000, Subscription: true}, {Provider: "openai-codex", Model: ptr("gpt-6-astra"), Requests: 6120, Tokens: 129_112_004, Subscription: true},
			{Provider: "gemini", Model: ptr("gemini-3.5-flash-lite"), Requests: 2900, Tokens: 30_000_000, CostUSD: 31.02}},
		ByProject: []data.ProjectUsage{{Project: ptr("/Users/example/Documents/projects/zdx"), Threads: 911, Tokens: 300_000_000}, {Project: ptr("/Users/example/Documents"), Threads: 820, Tokens: 100_000_000}, {Threads: 113, Tokens: 12_880_311}},
		ByRun:     []data.RunUsage{{Kind: "orchestrator", Name: ptr("orchestrator"), Threads: 120, Tokens: 200_000_000}, {Kind: "worker", Threads: 300, Tokens: 150_000_000}, {Kind: "subagent", Name: ptr("explorer"), Threads: 700, Tokens: 50_000_000}, {Kind: "helper", Name: ptr("title"), Threads: 724, Tokens: 12_880_311}},
		Daily:     daily}, source.OriginSocket, nil
}

// Quotas is the direct quota fetch; it needs no service.
func (b *Backend) Quotas(ctx context.Context) ([]data.Subscription, source.Origin, error) {
	if err := b.gate(ctx, "quotas", false); err != nil {
		return nil, source.OriginDirect, err
	}
	if b.Scenario == Empty {
		return []data.Subscription{}, source.OriginDirect, nil
	}
	return []data.Subscription{
		{Provider: "claude-cli", Name: "Claude (work)", Plan: ptr("max"), Windows: []data.QuotaWindow{{Label: "5h", UsedPercent: 42.5, ResetsAt: ptr(b.at(2 * time.Hour))}, {Label: "week", UsedPercent: 91, ResetsAt: ptr(b.at(50 * time.Hour)), Scope: ptr("Opus")}}},
		{Provider: "openai-codex", Name: "ChatGPT", Plan: ptr("pro"), Windows: []data.QuotaWindow{{Label: "5h", UsedPercent: 7}}},
		{Provider: "google-antigravity", Name: "Antigravity", Windows: []data.QuotaWindow{}, Error: ptr("expired · re-login in zdx")},
	}, source.OriginDirect, nil
}

// Workspaces is `workspaces`.
func (b *Backend) Workspaces(ctx context.Context) (data.Workspaces, source.Origin, error) {
	if err := b.gate(ctx, "workspaces", true); err != nil {
		return data.Workspaces{}, source.OriginSocket, err
	}
	src := func(label, layer string) data.SettingSource { return data.SettingSource{Label: label, Layer: layer} }
	set := func(group, key, value string, s data.SettingSource) data.Setting {
		return data.Setting{Group: group, Key: key, Label: key, Kind: "chat", Value: []byte(value), Source: s, InheritedValue: []byte("null"), ResolvesTo: []byte("null")}
	}
	global := src("global", "global")
	ws := src("workspace", "workspace")
	model := set("core", "model", `"claude-cli:claude-opus-5-5@high"`, ws)
	model.InheritedValue, model.InheritedSource = []byte(`"gpt-6-astra"`), &global
	mode := set("core", "model_mode", `"mode:deep"`, global)
	mode.ResolvesTo = []byte(`"claude-cli:claude-opus-5-5@max"`)
	modes := set("model modes", "model_modes", `"fast → claude-cli:claude-sonnet-5-5@medium, deep → claude-cli:claude-opus-5-5@max"`, global)
	modes.Label, modes.Kind = "modes", "modes"
	modes.Modes = []data.ModeView{
		{Name: "fast", Description: "quick checks and exploration", Primary: "claude-cli:claude-sonnet-5-5@medium", Thinking: "medium", Alternatives: []string{"openai-codex:gpt-6@low"}},
		{Name: "deep", Description: "hard debugging and design", Primary: "claude-cli:claude-opus-5-5@max", Thinking: "max"},
	}
	subagents := set("subagents", "subagents.enabled", `"on"`, global)
	subagents.Label, subagents.Kind = "enabled", "toggle"
	globalModel := set("core", "model", `"gpt-6-astra"`, global)
	return data.Workspaces{Global: &data.GlobalConfig{Path: "/Users/example/.zdx/config.toml", Settings: []data.Setting{globalModel, modes, subagents}}, Workspaces: []data.Workspace{
		{Name: "Documents", Root: "/Users/example/Documents", Current: true, OverlayPath: "/Users/example/Documents/.zdx/config.toml", HasOverlay: true,
			Settings: []data.Setting{model, mode,
				set("helper models", "title_model", `"gemini:gemini-3.5-flash-lite"`, src("default", "default")),
				set("helper models", "handoff_model", `"gemini:gemini-3.8-flash"`, global),
				set("audio", "transcription.model", "null", src("default", "default")),
				modes, subagents,
			}},
		{Name: "zdx", Root: "/Users/example/Documents/projects/zdx", OverlayPath: "/Users/example/Documents/projects/zdx/.zdx/config.toml",
			Error: ptr("parse .zdx/config.toml: expected `=` at line 4"), Settings: []data.Setting{set("core", "model", `"gpt-6-astra"`, global)}},
	}}, source.OriginSocket, nil
}

// Picker is the catalog model picker.
func (b *Backend) Picker(ctx context.Context, _ string) (data.Picker, error) {
	if err := b.gate(ctx, "picker", false); err != nil {
		return data.Picker{}, err
	}
	levels := func(id string) map[string]string {
		out := map[string]string{}
		for _, l := range []string{"off", "low", "medium", "high", "max"} {
			out[l] = id + "@" + l
		}
		return out
	}
	return data.Picker{
		Models: []data.PickerModel{
			{ID: "claude-cli:claude-opus-5-5", Name: "claude-cli:claude-opus-5-5", Specs: levels("claude-cli:claude-opus-5-5")},
			{ID: "claude-cli:claude-sonnet-5-5", Name: "claude-cli:claude-sonnet-5-5", Specs: levels("claude-cli:claude-sonnet-5-5")},
			{ID: "google-antigravity:gemini-4-argon-high", Name: "google-antigravity:gemini-4-argon-high", Specs: map[string]string{}},
		},
		Modes: []data.PickerMode{{Name: "fast", Ref: "mode:fast"}, {Name: "deep", Ref: "mode:deep"}},
	}, nil
}

// ChangeSetting reports a change it does not write.
func (b *Backend) ChangeSetting(ctx context.Context, ch source.SettingChange) (data.ChangeOutcome, error) {
	if err := b.gate(ctx, "change", false); err != nil {
		return data.ChangeOutcome{}, err
	}
	file := "/Users/example/.zdx/config.toml"
	if ch.Workspace != "" {
		file = ch.Workspace + "/.zdx/config.toml"
	}
	out := data.ChangeOutcome{Key: ch.Key, File: file, DryRun: ch.DryRun, Changed: true}
	out.Reach.Reads = []string{"Documents", "zdx"}
	return out, nil
}

// Jobs is `zdx jobs list --scheduler --json`.
func (b *Backend) Jobs(ctx context.Context) (data.SchedulerJobs, error) {
	if err := b.gate(ctx, "jobs", false); err != nil {
		return data.SchedulerJobs{}, err
	}
	if b.Scenario == Empty {
		return data.SchedulerJobs{Scheduler: data.SchedulerContextState{Status: "unavailable", Reason: ptr("scheduler is not running and no scheduler service is installed")}, Jobs: []data.JobOverview{}}, nil
	}
	root := data.ResolvedRoot{Path: "/Users/example/Documents", Origin: "scheduler"}
	model := func(spec, origin string) data.ResolvedModel {
		return data.ResolvedModel{Spec: spec, Origin: origin, Thinking: "high", ThinkingOrigin: origin}
	}
	return data.SchedulerJobs{
		Scheduler: data.SchedulerContextState{Status: "available", Context: &data.SchedulerContext{Source: "running", Root: "/Users/example/Documents", PID: ptr(uint32(47001))}},
		Jobs: []data.JobOverview{
			{Name: "daily-report", Path: "/Users/example/.zdx/jobs/daily-report.md", Schedule: &data.JobSchedule{Kind: "recurring", Cron: ptr("0 8 * * 1-5")}, GraceSecs: ptr(uint32(600)), Status: "scheduled",
				NextRunAt: ptr(b.at(17*time.Hour + 30*time.Minute)), LastRun: &data.LastRun{Outcome: "succeeded", Trigger: "schedule", FinishedAt: b.at(-6*time.Hour - 28*time.Minute), ThreadID: ptr("job-daily-report-20261114")},
				Settings: &data.JobRunSettings{Root: root, Model: model("gemini:gemini-3.8-flash@low", "job")}, DeclaredModel: ptr("gemini:gemini-3.8-flash@low")},
			{Name: "english-coach", Path: "/Users/example/.zdx/jobs/english-coach.md", Schedule: &data.JobSchedule{Kind: "recurring", Cron: ptr("30 19 * * *")}, GraceSecs: ptr(uint32(600)), Status: "running",
				NextRunAt: ptr(b.at(5 * time.Hour)), LastRun: &data.LastRun{Outcome: "failed", Trigger: "schedule", FinishedAt: b.at(-19 * time.Hour), Error: ptr("provider error: 529 overloaded after 5 retries")},
				Settings: &data.JobRunSettings{Root: root, Model: model("claude-cli@work:claude-opus-5-5@high", "config")}},
			{Name: "disk-space-housekeeping-with-a-long-name", Path: "/Users/example/.zdx/jobs/disk-space-housekeeping-with-a-long-name.md", Schedule: &data.JobSchedule{Kind: "once", RunAt: ptr(b.at(26 * time.Hour))}, Status: "pending",
				NextRunAt: ptr(b.at(26 * time.Hour)), Settings: &data.JobRunSettings{Root: data.ResolvedRoot{Path: "/Users/example/Documents/projects/a-project-with-a-long-directory-name", Origin: "job"}, Model: model("openai:gpt-5.6-terra@low", "subagent")}},
			{Name: "broken", Path: "/Users/example/.zdx/jobs/broken.md", Status: "invalid", Error: ptr("Invalid schedule 'bogus': expected 5 cron fields (minute hour day month weekday)")},
		}}, nil
}

// LogLines are the Full scenario's log tail.
var LogLines = []string{
	"2026-11-14T14:12:01.104522Z  INFO zdx_engine::skills: Loaded skills count=24 warnings=0",
	"2026-11-14T14:12:01.448120Z  INFO turn: zdx_engine::core::agent: Turn started thinking=\"high\" tools=20 kind=\"orchestrator\" thread=\"telegram--1001234567890-topic-42\" model=\"gpt-6-astra\"",
	"2026-11-14T14:12:09.002311Z DEBUG zdx_engine::providers::openai: stream opened status=200 ttft_ms=812",
	"2026-11-14T14:13:40.771002Z  WARN zdx_engine::core::thread_index: Slow thread index sync on a read path elapsed_ms=526 scope=Meta files=26151",
	"2026-11-14T14:14:02.300110Z ERROR zdx_engine::providers::anthropic: request failed status=529 body=\"overloaded\" attempt=2",
	"    retrying in 4s",
	"2026-11-14T14:14:06.512000Z  INFO turn: zdx_engine::core::agent: Turn finished model_turns=3 thread=\"telegram--1001234567890-topic-42\"",
}

// Log is the log tail.
func (b *Backend) Log(ctx context.Context, index, maxLines int, prev source.LogStamp) (source.LogTail, error) {
	if err := b.gate(ctx, "logs", false); err != nil {
		return source.LogTail{}, err
	}
	if b.Scenario == Empty {
		return source.LogTail{}, nil
	}
	files := []string{"zdx.log.2026-11-14", "zdx.log.2026-11-13", "zdx.log.2026-11-12"}
	stamp := source.LogStamp{Path: files[index], Size: int64(len(LogLines)), Lines: maxLines}
	if stamp == prev {
		return source.LogTail{Files: files, File: files[index], Stamp: stamp, Unchanged: true}, nil
	}
	return source.LogTail{Files: files, File: files[index], Stamp: stamp, Lines: LogLines}, nil
}
