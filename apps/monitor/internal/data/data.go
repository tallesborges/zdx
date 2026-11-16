// Package data holds the JSON bodies the monitor reads, decoded as the
// engine writes them (views.rs bodies, `zdx service status --json`,
// `zdx jobs list --scheduler --json`), and the quota rows of the shared quota
// package. Types only: every value is derived in Rust or the shared Go
// module, and the monitor shows it without re-deriving it.
package data

import (
	"encoding/json"

	"github.com/tallesborges/zdx/apps/shared/quota"
	"github.com/tallesborges/zdx/apps/shared/transcript"
)

// Thread views and their parts are the shared transcript types.
type (
	RunStatus      = transcript.RunStatus
	ThreadList     = transcript.ThreadList
	WorkerRollup   = transcript.WorkerRollup
	ThreadListItem = transcript.ThreadListItem
	Thread         = transcript.Thread
	Activity       = transcript.Activity
	Attachment     = transcript.Attachment
	ToolView       = transcript.ToolView
	ToolFact       = transcript.ToolFact
	ToolHead       = transcript.ToolHead
	ToolBody       = transcript.ToolBody
	OutputStream   = transcript.OutputStream
	MatchFile      = transcript.MatchFile
	MatchLine      = transcript.MatchLine
	PathEntry      = transcript.PathEntry
	DiffLine       = transcript.DiffLine
)

// Agent is one active run (`active_agents`).
type Agent struct {
	PID            uint32    `json:"pid"`
	ThreadID       *string   `json:"thread_id"`
	ParentThreadID *string   `json:"parent_thread_id"`
	Surface        *string   `json:"surface"`
	Role           *string   `json:"role"`
	Model          *string   `json:"model"`
	Provider       *string   `json:"provider"`
	Account        *string   `json:"account"`
	Thinking       *string   `json:"thinking"`
	Uptime         string    `json:"uptime"`
	Run            RunStatus `json:"run"`
}

// Snapshot is `monitor_snapshot` (active agents come from their own view,
// quotas from the shared quota package).
type Snapshot struct {
	GeneratedAt         string              `json:"generated_at"`
	Services            []SnapshotService   `json:"services"`
	BackgroundProcesses []BackgroundProcess `json:"background_processes"`
	Config              MonitorConfig       `json:"config"`
}

// SnapshotService is a service row of the snapshot.
type SnapshotService struct {
	Name      string  `json:"name"`
	Running   bool    `json:"running"`
	Installed bool    `json:"installed"`
	PID       *uint32 `json:"pid"`
	Uptime    *string `json:"uptime"`
}

// BackgroundProcess is a running `background: true` process.
type BackgroundProcess struct {
	ID       string  `json:"id"`
	PID      uint32  `json:"pid"`
	ThreadID *string `json:"thread_id"`
	Command  string  `json:"command"`
	Uptime   string  `json:"uptime"`
}

// MonitorConfig is the config summary of the views root.
type MonitorConfig struct {
	Model            string        `json:"model"`
	ModelMode        *string       `json:"model_mode"`
	Thinking         string        `json:"thinking"`
	MaxTokens        *uint32       `json:"max_tokens"`
	SubagentsEnabled bool          `json:"subagents_enabled"`
	ModeCount        int           `json:"mode_count"`
	HelperModels     []HelperModel `json:"helper_models"`
	ServerEnabled    bool          `json:"server_enabled"`
	ServerPort       int           `json:"server_port"`
}

// HelperModel is one helper role and its model.
type HelperModel struct {
	Role  string `json:"role"`
	Model string `json:"model"`
}

// JobSchedule is a job's schedule (`kind`: manual, recurring, once; apps/shared/jobs).
type JobSchedule struct {
	Kind  string  `json:"kind"`
	Cron  *string `json:"cron,omitempty"`
	RunAt *string `json:"run_at,omitempty"`
}

// LastRun is a job's latest final outcome.
type LastRun struct {
	RunID        string  `json:"run_id"`
	Trigger      string  `json:"trigger"`
	Outcome      string  `json:"outcome"`
	ScheduledFor *string `json:"scheduled_for"`
	FinishedAt   string  `json:"finished_at"`
	Error        *string `json:"error"`
	ThreadID     *string `json:"thread_id"`
}

// Subscription is one account's quota row, as quota.Subscriptions builds it
// from a quota.Fetch snapshot (the row contract the shared quota package
// records).
type Subscription = quota.Subscription

// QuotaWindow is one quota window.
type QuotaWindow = quota.SubscriptionWindow

// Usage is `usage_report`.
type Usage struct {
	Days               uint32         `json:"days"`
	Requests           uint64         `json:"requests"`
	Tokens             uint64         `json:"tokens"`
	Input              uint64         `json:"input"`
	Output             uint64         `json:"output"`
	CacheRead          uint64         `json:"cache_read"`
	CacheWrite         uint64         `json:"cache_write"`
	BilledUSD          float64        `json:"billed_usd"`
	SubscriptionTokens uint64         `json:"subscription_tokens"`
	UnknownPricingRows uint64         `json:"unknown_pricing_rows"`
	ThreadsScanned     int            `json:"threads_scanned"`
	ByProvider         []UsageRow     `json:"by_provider"`
	ByModel            []UsageRow     `json:"by_model"`
	Daily              []DailyUsage   `json:"daily"`
	ByProject          []ProjectUsage `json:"by_project"`
	ByRun              []RunUsage     `json:"by_run"`
	UnindexedThreads   int            `json:"unindexed_threads"`
}

// UsageRow is one provider or model row.
type UsageRow struct {
	Provider     string  `json:"provider"`
	Model        *string `json:"model"`
	Requests     uint64  `json:"requests"`
	Tokens       uint64  `json:"tokens"`
	CostUSD      float64 `json:"cost_usd"`
	Subscription bool    `json:"subscription"`
	Estimated    bool    `json:"estimated"`
}

// ProjectUsage is one project's usage; Project is nil when unknown.
type ProjectUsage struct {
	Project *string `json:"project"`
	Threads int     `json:"threads"`
	Tokens  uint64  `json:"tokens"`
}

// RunUsage is one run kind's usage.
type RunUsage struct {
	Kind    string  `json:"kind"`
	Name    *string `json:"name"`
	Threads int     `json:"threads"`
	Tokens  uint64  `json:"tokens"`
}

// DailyUsage is one UTC day's tokens; Day counts days since the Unix epoch.
type DailyUsage struct {
	Day    int32  `json:"day"`
	Tokens uint64 `json:"tokens"`
}

// Workspaces is the `workspaces` view.
type Workspaces struct {
	Workspaces []Workspace `json:"workspaces"`
}

// Workspace is one workspace's settings with provenance.
type Workspace struct {
	Name            string    `json:"name"`
	Root            string    `json:"root"`
	Current         bool      `json:"current"`
	Error           *string   `json:"error"`
	HasOverlay      bool      `json:"has_overlay"`
	OverlayIsGlobal bool      `json:"overlay_is_global"`
	OverlayPath     string    `json:"overlay_path"`
	Settings        []Setting `json:"settings"`
}

// Setting is one setting with the layer that supplied it.
type Setting struct {
	Group           string          `json:"group"`
	Key             string          `json:"key"`
	Label           string          `json:"label"`
	Value           json.RawMessage `json:"value"`
	Source          SettingSource   `json:"source"`
	InheritedValue  json.RawMessage `json:"inherited_value"`
	InheritedSource *SettingSource  `json:"inherited_source"`
	ResolvesTo      json.RawMessage `json:"resolves_to"`
}

// SettingSource names a config layer.
type SettingSource struct {
	Label string  `json:"label"`
	Layer string  `json:"layer"`
	Path  *string `json:"path"`
}

// ServiceStatus is `zdx service status --json`.
type ServiceStatus struct {
	Services      []ServiceState  `json:"services"`
	QueuedRestart json.RawMessage `json:"queued_restart"`
}

// ServiceState is one launchd service.
type ServiceState struct {
	Service    string  `json:"service"`
	Label      string  `json:"label"`
	Installed  bool    `json:"installed"`
	Status     string  `json:"status"`
	PID        *uint32 `json:"pid"`
	UptimeSecs *uint64 `json:"uptime_secs"`
}

// SchedulerJobs is `zdx jobs list --scheduler --json` (the Go jobs program): jobs
// resolved the way the scheduled scheduler resolves them.
type SchedulerJobs struct {
	Scheduler SchedulerContextState `json:"scheduler"`
	Jobs      []JobOverview         `json:"jobs"`
	Error     *string               `json:"error"`
}

// SchedulerContextState is `available` with a context or `unavailable` with
// a reason.
type SchedulerContextState struct {
	Status  string            `json:"status"`
	Reason  *string           `json:"reason,omitempty"`
	Context *SchedulerContext `json:"context,omitempty"`
}

// SchedulerContext is the root and config the scheduler resolves jobs with.
type SchedulerContext struct {
	Source    string  `json:"source"`
	Root      string  `json:"root"`
	ConfigDir string  `json:"config_dir"`
	PID       *uint32 `json:"pid"`
	StartedAt *string `json:"started_at"`
	Note      *string `json:"note"`
}

// JobOverview is jobs::JobOverview.
type JobOverview struct {
	Name          string          `json:"name"`
	Path          string          `json:"path"`
	Schedule      *JobSchedule    `json:"schedule"`
	GraceSecs     *uint32         `json:"grace_secs"`
	Status        string          `json:"status"`
	NextRunAt     *string         `json:"next_run_at"`
	LastRun       *LastRun        `json:"last_run"`
	DeclaredRoot  *string         `json:"declared_root"`
	DeclaredModel *string         `json:"declared_model"`
	Subagent      *string         `json:"subagent"`
	TimeoutSecs   *uint32         `json:"timeout_secs"`
	MaxRetries    uint32          `json:"max_retries"`
	Settings      *JobRunSettings `json:"settings"`
	Error         *string         `json:"error"`
}

// JobRunSettings is the resolved root and model of a job.
type JobRunSettings struct {
	Root     ResolvedRoot  `json:"root"`
	Model    ResolvedModel `json:"model"`
	Subagent *string       `json:"subagent"`
}

// ResolvedRoot is the root a job runs in and where it came from.
type ResolvedRoot struct {
	Path   string `json:"path"`
	Origin string `json:"origin"`
}

// ResolvedModel is the model a job runs with and where it came from.
type ResolvedModel struct {
	Spec           string  `json:"spec"`
	Provider       string  `json:"provider"`
	Account        *string `json:"account"`
	Model          string  `json:"model"`
	Thinking       string  `json:"thinking"`
	Fast           bool    `json:"fast"`
	Origin         string  `json:"origin"`
	ThinkingOrigin string  `json:"thinking_origin"`
}

// Trajectory is `thread_trajectory` (core::thread_trajectory
// ThreadTrajectoryReport): client-observed spans per turn. Every number is
// computed in Rust; nil means unknown.
type Trajectory struct {
	SourceEventCount int                    `json:"source_event_count"`
	ObservedAt       string                 `json:"observed_at"`
	Turns            []TrajectoryTurn       `json:"turns"`
	Bottlenecks      []TrajectoryBottleneck `json:"bottlenecks"`
	Measured         int                    `json:"measured"`
	Total            int                    `json:"total"`
	Exact            int                    `json:"exact"`
	ActiveWallMS     *uint64                `json:"active_wall_ms"`
	SpanWorkMS       *uint64                `json:"span_work_ms"`
	ConcurrentWorkMS *uint64                `json:"concurrent_work_ms"`
	HasUnavailable   bool                   `json:"has_unavailable"`
}

// TrajectoryTurn is one user turn.
type TrajectoryTurn struct {
	Index            int              `json:"index"`
	Input            *TrajectoryInput `json:"input"`
	Spans            []TrajectorySpan `json:"spans"`
	Timing           string           `json:"timing"` // exact, partial, legacy
	Measured         int              `json:"measured"`
	Total            int              `json:"total"`
	ElapsedMS        *uint64          `json:"elapsed_ms"`
	ActiveWallMS     *uint64          `json:"active_wall_ms"`
	SpanWorkMS       *uint64          `json:"span_work_ms"`
	ConcurrentWorkMS *uint64          `json:"concurrent_work_ms"`
	ModelTracks      int              `json:"model_tracks"`
	ToolTracks       int              `json:"tool_tracks"`
}

// TrajectoryInput is the message that started a turn.
type TrajectoryInput struct {
	Sequence int    `json:"sequence"`
	Ts       string `json:"ts"`
	Text     string `json:"text"`
}

// TrajectorySpan is one model request or tool call.
type TrajectorySpan struct {
	Key          string  `json:"key"`
	Kind         string  `json:"kind"` // input, model, tool
	Lane         string  `json:"lane"`
	Turn         int     `json:"turn"`
	Sequence     int     `json:"sequence"`
	ToolUseID    *string `json:"tool_use_id"`
	Label        string  `json:"label"`
	Detail       string  `json:"detail"`
	Status       string  `json:"status"` // ok, failed, running, unknown
	Timing       string  `json:"timing"` // exact, duration, unavailable
	StartedAt    *string `json:"started_at"`
	CompletedAt  *string `json:"completed_at"`
	DurationMS   *uint64 `json:"duration_ms"`
	WallMS       *uint64 `json:"wall_ms"`
	Track        int     `json:"track"`
	Model        *string `json:"model"`
	Provider     *string `json:"provider"`
	TTFTMS       *uint64 `json:"ttft_ms"`
	InputTokens  *uint64 `json:"input_tokens"`
	OutputTokens *uint64 `json:"output_tokens"`
}

// TrajectoryBottleneck is one of the longest measured spans.
type TrajectoryBottleneck struct {
	Key        string `json:"key"`
	Kind       string `json:"kind"`
	Turn       int    `json:"turn"`
	Label      string `json:"label"`
	Detail     string `json:"detail"`
	Status     string `json:"status"`
	Timing     string `json:"timing"`
	DurationMS uint64 `json:"duration_ms"`
}
