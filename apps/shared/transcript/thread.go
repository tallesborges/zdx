// Package transcript is the terminal rendering of a ZDX thread that every Go
// terminal surface shares (the monitor and the chat TUI): the `thread_view`
// body types, their merge, and their layout as styled lines, with the
// Markdown renderer and the text cleaning it relies on. The engine derives
// every value (views.rs, zdx_types::tool_view); this package only lays it
// out.
package transcript

import "encoding/json"

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
// video, html, markdown, text, other).
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
// text). A shell head carries the bash call's `description` as `Label`
// (collapsed rows show it) and the exact `Command` (the detail shows it).
type ToolHead struct {
	Kind    string  `json:"kind"`
	Cwd     *string `json:"cwd,omitempty"`
	Label   *string `json:"label,omitempty"`
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
