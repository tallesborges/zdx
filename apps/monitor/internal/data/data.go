// Package data holds the JSON bodies the monitor reads, decoded as the
// engine writes them (views.rs bodies, `zdx service status --json`,
// `zdx jobs list --daemon --json`), and the quota rows of the shared quota
// package. Types only: every value is derived in Rust or the shared Go
// module, and the monitor shows it without re-deriving it.
package data

import (
	"encoding/json"

	"github.com/tallesborges/zdx/apps/shared/quota"
)

// RunStatus is agent_activity::RunStatus.
type RunStatus struct {
	State                string  `json:"state"`
	Label                string  `json:"label"`
	StartedAt            string  `json:"started_at"`
	TurnElapsedSecs      *uint64 `json:"turn_elapsed_secs"`
	SecondsSinceActivity *uint64 `json:"seconds_since_activity"`
	SecondsSinceBeat     *uint64 `json:"seconds_since_heartbeat"`
	RunningTools         int     `json:"running_tools"`
	FinishedTools        int     `json:"finished_tools"`
}

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

// ThreadList is `threads_list`: the Mini App's recent list (at most 50).
type ThreadList struct {
	Threads []ThreadListItem `json:"threads"`
}

// WorkerRollup is the worker count of an orchestrator row.
type WorkerRollup struct {
	Total   int `json:"total"`
	Running int `json:"running"`
	Queued  int `json:"queued"`
	Failed  int `json:"failed"`
	Settled int `json:"settled"`
	Unknown int `json:"unknown"`
}

// ThreadListItem is one thread row.
type ThreadListItem struct {
	ID           string        `json:"id"`
	Title        string        `json:"title"`
	RootPath     *string       `json:"root_path"`
	Project      *string       `json:"project"`
	Age          *string       `json:"age"`
	TelegramLink *string       `json:"telegram_link"`
	Workers      *WorkerRollup `json:"workers"`
	IsWorker     bool          `json:"is_worker"`
	Run          *RunStatus    `json:"run"`
}

// Thread is `thread_view`.
type Thread struct {
	ID             string     `json:"id"`
	Title          string     `json:"title"`
	TotalMessages  int        `json:"total_messages"`
	TotalEvents    int        `json:"total_events"`
	Cursor         uint64     `json:"cursor"`
	Partial        bool       `json:"partial"`
	ParentThreadID *string    `json:"parent_thread_id"`
	ParentTitle    *string    `json:"parent_title"`
	WorkerCount    int        `json:"worker_count"`
	Run            *RunStatus `json:"run"`
	Activity       []Activity `json:"activity"`
	// SourceID is the thread the events come from (the reply's
	// `source_id`): the requested id, or the thread an alias points to.
	SourceID string `json:"-"`
}

// Activity is one ThreadActivity item. Type is the tag (`message`,
// `reasoning`, `tool_use`, `tool_running`, `tool_result`, `usage`,
// `notice`, `interrupted`); unknown types are kept and shown by name.
type Activity struct {
	Type     string `json:"type"`
	Sequence uint64 `json:"sequence"`
	Time     string `json:"time"`

	Role    string `json:"role,omitempty"`
	Speaker string `json:"speaker,omitempty"`
	Text    string `json:"text,omitempty"`
	Phase   string `json:"phase,omitempty"`
	// Artifacts are a message's attachments; the engine strips their tags
	// from Text.
	Artifacts []Attachment `json:"artifacts,omitempty"`

	Redacted bool `json:"redacted,omitempty"`

	ID         string          `json:"id,omitempty"`
	Name       string          `json:"name,omitempty"`
	Input      json.RawMessage `json:"input,omitempty"`
	View       *ToolView       `json:"view,omitempty"`
	Summary    string          `json:"summary,omitempty"`
	OutputTail string          `json:"output_tail,omitempty"`
	RunningFor string          `json:"running_for,omitempty"`
	// CompletedAt is set on a tool_running item whose call already finished
	// (OK and DurationMS then say how) while its round is not yet in the
	// thread file.
	CompletedAt *string `json:"completed_at,omitempty"`

	ToolUseID  string  `json:"tool_use_id,omitempty"`
	OK         *bool   `json:"ok,omitempty"`
	DurationMS *uint64 `json:"duration_ms,omitempty"`

	InputTokens      uint64  `json:"input_tokens,omitempty"`
	OutputTokens     uint64  `json:"output_tokens,omitempty"`
	CacheReadTokens  uint64  `json:"cache_read_tokens,omitempty"`
	CacheWriteTokens uint64  `json:"cache_write_tokens,omitempty"`
	Model            *string `json:"model,omitempty"`
	Provider         *string `json:"provider,omitempty"`
	ContextLimit     *uint64 `json:"context_limit,omitempty"`

	Kind    string `json:"kind,omitempty"`
	Message string `json:"message,omitempty"`
}

// Attachment is core::artifacts::ArtifactAttachment (`kind`: image, audio,
// html, markdown, text, other).
type Attachment struct {
	Name      string `json:"name"`
	Path      string `json:"path"`
	Mime      string `json:"mime"`
	Kind      string `json:"kind"`
	SizeBytes uint64 `json:"size_bytes"`
	Exists    bool   `json:"exists"`
}

// ToolView is zdx_types::tool_view::ToolView.
type ToolView struct {
	Outcome  string     `json:"outcome"`
	Head     *ToolHead  `json:"head,omitempty"`
	Body     ToolBody   `json:"body"`
	Status   []ToolFact `json:"status"`
	Hint     *ToolFact  `json:"hint,omitempty"`
	Notes    []string   `json:"notes"`
	Info     []string   `json:"info,omitempty"`
	ThreadID *string    `json:"thread_id,omitempty"`
}

// ToolFact is one short fact.
type ToolFact struct {
	Text string `json:"text"`
	Bad  bool   `json:"bad"`
	Kind string `json:"kind"`
}

// ToolHead is the command or target (`kind`: shell, path, search, prompt,
// text).
type ToolHead struct {
	Kind    string  `json:"kind"`
	Cwd     *string `json:"cwd,omitempty"`
	Command string  `json:"command,omitempty"`
	Path    string  `json:"path,omitempty"`
	Pattern string  `json:"pattern,omitempty"`
	Scope   *string `json:"scope,omitempty"`
	Agent   *string `json:"agent,omitempty"`
	Prompt  string  `json:"prompt,omitempty"`
	Text    string  `json:"text,omitempty"`
}

// ToolBody is the tool's output projection (`kind`: empty, streams, code,
// matches, paths, diff, markdown, error, text).
type ToolBody struct {
	Kind      string         `json:"kind"`
	Streams   []OutputStream `json:"streams,omitempty"`
	Text      string         `json:"text,omitempty"`
	FirstLine uint64         `json:"first_line,omitempty"`
	Files     []MatchFile    `json:"files,omitempty"`
	Entries   []PathEntry    `json:"entries,omitempty"`
	Lines     []DiffLine     `json:"lines,omitempty"`
	Additions int            `json:"additions,omitempty"`
	Deletions int            `json:"deletions,omitempty"`
	Code      string         `json:"code,omitempty"`
	Message   string         `json:"message,omitempty"`
	Details   *string        `json:"details,omitempty"`
}

// OutputStream is stdout or stderr text.
type OutputStream struct {
	Name string `json:"name"`
	Text string `json:"text"`
}

// MatchFile is one file of a search.
type MatchFile struct {
	Path  string      `json:"path"`
	Lines []MatchLine `json:"lines"`
}

// MatchLine is one matched or context line.
type MatchLine struct {
	Line    uint64 `json:"line"`
	Text    string `json:"text"`
	Context bool   `json:"context,omitempty"`
}

// PathEntry is one listed path.
type PathEntry struct {
	Path string `json:"path"`
	Dir  bool   `json:"dir"`
}

// DiffLine is one diff line (`op`: add, del, ctx).
type DiffLine struct {
	Op   string `json:"op"`
	Text string `json:"text"`
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

// DaemonJobs is `zdx jobs list --daemon --json` (the Go jobs program): jobs
// resolved the way the scheduled daemon resolves them.
type DaemonJobs struct {
	Daemon DaemonContextState `json:"daemon"`
	Jobs   []JobOverview      `json:"jobs"`
	Error  *string            `json:"error"`
}

// DaemonContextState is `available` with a context or `unavailable` with
// a reason.
type DaemonContextState struct {
	Status  string         `json:"status"`
	Reason  *string        `json:"reason,omitempty"`
	Context *DaemonContext `json:"context,omitempty"`
}

// DaemonContext is the root and config the daemon resolves jobs with.
type DaemonContext struct {
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
