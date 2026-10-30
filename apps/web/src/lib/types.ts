/**
 * Response types for the zdx bot's embedded Mini App API.
 * Mirrors crates/zdx-bot/src/server.rs — all endpoints are GET and read-only.
 */

export type Json = null | boolean | number | string | Json[] | { [k: string]: Json };

/* ---------------------------------- threads --------------------------------- */

export interface ThreadResponse {
  id: string;
  title: string;
  total_messages: number;
  /** Counted before live `tool_running` entries are appended, so this can be
   *  lower than `activity.length` while tools are in flight. */
  total_events: number;
  /** `t.me` link to the bound Telegram topic, derived from the resolved id so
   *  it is present even when the client asked for `active`. Null for TUI/CLI
   *  threads and plain DMs. */
  telegram_link: string | null;
  /** Cursor to send as `after` on the next poll; items at or above it have not
   *  been delivered yet. */
  cursor: number;
  /** True when `activity` holds only items at or after the requested `after`
   *  cursor, so it must be merged into the existing transcript. */
  partial: boolean;
  /** A reply delivery started recently and has no outcome yet; keep polling
   *  so its pending warning is replaced by the outcome. */
  delivering?: boolean;
  /** Orchestrator that owns this thread as a worker, when it has one. */
  parent_thread_id: string | null;
  /** Title of `parent_thread_id`, resolved server-side. */
  parent_title: string | null;
  /** Workers owned by this thread (live plus recovered from lineage). Gates the
   *  Workers tab without fetching the list first. */
  worker_count: number;
  /** Shared live status of a turn running on this thread in any process, or
   *  null when none runs. Keep polling while it is set. */
  run: RunStatus | null;
  activity: ThreadActivity[];
}

/**
 * Shared live status of one running turn (`zdx_engine::agent_activity::RunStatus`).
 * The ages are observations: a quiet tool or a long model stream can leave
 * `seconds_since_activity` high while the run is healthy. Never label a run
 * stuck from them.
 */
export interface RunStatus {
  /** `tool` while a call runs, else `waiting` | `thinking` | `answering` |
   *  `retrying` | `working`. */
  state: string;
  /** One display line, e.g. `bash: cargo test (+1 more)` or `waiting for model`. */
  label: string;
  started_at: string;
  turn_elapsed_secs: number | null;
  /** Seconds since the run emitted any agent event. Null for older markers. */
  seconds_since_activity: number | null;
  /** Seconds since the run's process last wrote its heartbeat. Shows the
   *  process is alive, not that the run makes progress. */
  seconds_since_heartbeat: number | null;
  running_tools: number;
  finished_tools: number;
}

export type WorkerStatus =
  | "queued"
  | "running"
  | "completed"
  | "failed"
  | "cancelled"
  | "unknown";

export interface WorkersResponse {
  workers: WorkerItem[];
}

export interface WorkerItem {
  thread_id: string;
  title: string;
  status: WorkerStatus;
  /** False when the row was recovered from persisted lineage after a restart.
   *  Those rows know the worker existed and nothing else: `status` is
   *  "unknown" and every progress field is null. Never render them as settled. */
  live: boolean;
  queue_depth: number;
  root_path: string | null;
  /** Trailing component of `root_path`, for a compact project label. */
  project: string | null;
  /** Live status of the worker's running turn, from its run marker. */
  run: RunStatus | null;
  last_error: string | null;
  /** Time since the worker thread's last write ("12m", "3h"), when known. */
  age: string | null;
  /** `t.me` link to the worker's mirror topic, when it has one. */
  telegram_link: string | null;
}

export interface ThreadListResponse {
  threads: ThreadListItem[];
}

export interface ThreadListItem {
  id: string;
  title: string;
  root_path: string | null;
  /** Trailing component of `root_path`, for a compact project label. */
  project: string | null;
  /** Time since the thread's last write ("12m", "3h", "2d"). */
  age: string | null;
  /** `t.me` link to the bound Telegram topic. Null for TUI/CLI threads and
   *  plain DMs, which have no linkable topic. */
  telegram_link: string | null;
  /** Worker counts when this thread is an orchestrator, else null. */
  workers: WorkerRollup | null;
  /** Whether this thread is an orchestrator worker. Drives the `worker` badge. */
  is_worker: boolean;
  /** Live status when a turn is running on this thread. */
  run: RunStatus | null;
}

/** `/api/threads?kind=` filter: every top-level thread, or workers only. */
export type ThreadListKind = "all" | "workers";

/** Worker counts for one orchestrator. `failed` is its own bucket on purpose:
 *  it is the state the reader has to act on, so it is never folded into
 *  `settled`. `unknown` is lineage-discovered with no live state. */
export interface WorkerRollup {
  total: number;
  running: number;
  queued: number;
  failed: number;
  settled: number;
  unknown: number;
}

export interface ActivityBase {
  /** Source-event index. Has gaps — do not use as an array index. */
  sequence: number;
  /** Preformatted local "HH:MM AM/PM", or "--:--". */
  time: string;
  /** Raw RFC3339 source timestamp. New events retain millisecond precision. */
  ts: string;
}

export interface MessageActivity extends ActivityBase {
  type: "message";
  role: string;
  speaker: "You" | "Z";
  text: string;
  phase?: string;
  /** Files this message sent. Absent when it sent nothing. */
  artifacts?: ArtifactAttachment[];
}

export type ArtifactKind = "image" | "audio" | "html" | "markdown" | "text" | "other";
export type ArtifactSource = "generated" | "sent" | "both";

/** Inline file on a transcript message. `path` is passed back as `?path=`. */
export interface ArtifactAttachment {
  name: string;
  path: string;
  mime: string;
  kind: ArtifactKind;
  size_bytes: number;
  exists: boolean;
}

/** One row of the thread Artifacts tab. */
export interface ArtifactItem extends ArtifactAttachment {
  /** Opaque id of the file in this thread (`artifact_key`): a lookup id for
   *  `Open artifact` links, not a path and not an authorization. */
  key: string;
  /** Relative to the thread artifact dir when inside it, else null. */
  rel: string | null;
  source: ArtifactSource;
  /** Transcript sequences that referenced this file; empty when never sent. */
  message_sequences: number[];
  /** RFC3339 mtime, when the file exists. */
  modified_at?: string;
}

/** `GET /api/threads/{id}/artifacts/version`: the file as it is now.
 *  `sha256` is the hash of its bytes, the version feedback names. */
export interface ArtifactVersion {
  sha256: string;
  size_bytes: number;
  modified_at: string | null;
}

export interface ArtifactsResponse {
  artifacts: ArtifactItem[];
}

/**
 * `POST /api/threads/{id}/artifacts/feedback` (`FeedbackResponse` in server.rs).
 * The request body is `FeedbackSubmissionPayload` in `lib/annotate.ts`, which
 * mirrors `zdx_engine::core::artifact_feedback::FeedbackSubmission`.
 */
/**
 * `GET /api/threads/{id}/artifacts/image` (`get_thread_artifact_image` in
 * server.rs): the canonical copy's bytes plus its identity headers.
 */
export interface ArtifactImageCopy {
  blob: Blob;
  /** SHA-256 of the original file: the artifact version. */
  sourceSha256: string;
  /** SHA-256 of the copy's bytes: the frame marks refer to. */
  canonicalSha256: string;
  width: number;
  height: number;
  /** The original's upright size; larger than the copy when it was shrunk. */
  sourceWidth: number;
  sourceHeight: number;
}

export interface ArtifactFeedbackResponse {
  /**
   * `queued`: on the chat's queue. `duplicate`: already queued earlier. The
   * queue is in memory: neither means a model has read it yet.
   */
  status: "queued" | "duplicate";
  annotations: number;
  /** Sent although the file changed after annotation. */
  stale: boolean;
}

export interface ReasoningActivity extends ActivityBase {
  type: "reasoning";
  text: string;
  redacted: boolean;
}

export interface ToolUseActivity extends ActivityBase {
  type: "tool_use";
  id: string;
  name: string;
  input: Json;
}

export interface ToolRunningActivity extends ActivityBase {
  type: "tool_running";
  started_at: string;
  id: string;
  name: string;
  summary: string;
  input: Json;
  output_tail: string;
  /** Elapsed time while running; the duration once finished. */
  running_for: string;
  /** Set when the call finished but its round is not in the thread file yet. */
  completed_at?: string;
  ok?: boolean;
  duration_ms?: number;
}

export interface ToolResultActivity extends ActivityBase {
  type: "tool_result";
  tool_use_id: string;
  ok: boolean;
  duration_ms?: number;
  started_at?: string;
  completed_at?: string;
  output: Json;
}

export interface UsageActivity extends ActivityBase {
  type: "usage";
  input_tokens: number;
  output_tokens: number;
  cache_read_tokens: number;
  cache_write_tokens: number;
  model?: string;
  provider?: string;
  duration_ms?: number;
  ttft_ms?: number;
  started_at?: string;
  completed_at?: string;
  /** Context window of the model that served this request. */
  context_limit?: number;
}

export interface NoticeActivity extends ActivityBase {
  type: "notice";
  kind: "refusal" | "context_window_exceeded" | "output_limit" | "output_loop" | "delivery" | "posted";
  message: string;
}

export interface InterruptedActivity extends ActivityBase {
  type: "interrupted";
  role: string;
  text: string;
}

export type ThreadActivity =
  | MessageActivity
  | ReasoningActivity
  | ToolUseActivity
  | ToolRunningActivity
  | ToolResultActivity
  | UsageActivity
  | NoticeActivity
  | InterruptedActivity;

export type TrajectorySpanKind = "input" | "model" | "tool";
export type TrajectoryLane = "input" | "model" | "tools";
export type TrajectoryStatus = "ok" | "failed" | "running" | "unknown";
export type TimingQuality = "exact" | "duration" | "unavailable";
export type TurnTimingQuality = "exact" | "partial" | "legacy";

export interface TrajectoryInput {
  sequence: number;
  ts: string;
  text: string;
}

export interface TrajectorySpan {
  key: string;
  kind: TrajectorySpanKind;
  lane: TrajectoryLane;
  turn: number;
  sequence: number;
  result_sequence?: number;
  tool_use_id?: string;
  label: string;
  detail: string;
  status: TrajectoryStatus;
  timing: TimingQuality;
  started_at?: string;
  completed_at?: string;
  start_ms?: number;
  end_ms?: number;
  /** Independently recorded request/tool execution duration. */
  duration_ms?: number;
  /** Width of explicit wall-clock boundaries; provisional for live spans. */
  wall_ms?: number;
  track: number;
  model?: string;
  provider?: string;
  ttft_ms?: number;
  input_tokens?: number;
  output_tokens?: number;
  cache_read_tokens?: number;
  cache_write_tokens?: number;
  live_input?: Json;
  live_output_tail?: string;
}

export interface TrajectoryBottleneck {
  key: string;
  kind: TrajectorySpanKind;
  turn: number;
  label: string;
  detail: string;
  status: TrajectoryStatus;
  timing: TimingQuality;
  duration_ms: number;
}

export interface TrajectoryTurn {
  index: number;
  input?: TrajectoryInput;
  spans: TrajectorySpan[];
  timing: TurnTimingQuality;
  measured: number;
  total: number;
  domain_start_ms?: number;
  domain_end_ms?: number;
  elapsed_ms?: number;
  active_wall_ms?: number;
  span_work_ms?: number;
  concurrent_work_ms?: number;
  model_tracks: number;
  tool_tracks: number;
}

export interface ThreadTrajectoryReport {
  source_event_count: number;
  observed_at: string;
  turns: TrajectoryTurn[];
  bottlenecks: TrajectoryBottleneck[];
  measured: number;
  total: number;
  exact: number;
  active_wall_ms?: number;
  span_work_ms?: number;
  concurrent_work_ms?: number;
  has_unavailable: boolean;
}

/* -------------------------------- workspaces -------------------------------- */

/** Mirrors `zdx_engine::workspace_config` as served by `GET /api/workspaces`. */
export interface WorkspacesResponse {
  workspaces: WorkspaceReport[];
}

export interface WorkspaceReport {
  root: string;
  name: string;
  /** Telegram profile names bound to this directory. */
  profiles: string[];
  /** The bot's own root. */
  current: boolean;
  overlay_path: string;
  has_overlay: boolean;
  /** The overlay is the global config file itself. */
  overlay_is_global: boolean;
  settings: WorkspaceSetting[];
  error: string | null;
}

/**
 * `workspace` = set in this workspace's own overlay; `ancestor` = inherited
 * from a parent directory's overlay.
 */
export type ConfigLayer = "default" | "global" | "workspace" | "ancestor";

export interface ConfigSource {
  layer: ConfigLayer;
  path: string | null;
  label: string;
}

export interface WorkspaceSetting {
  key: string;
  group: string;
  label: string;
  kind: "chat" | "transcription" | "speech" | "toggle" | "modes";
  /** `null` when unset: the runtime default applies. */
  value: string | null;
  source: ConfigSource;
  /** What resetting this workspace's own overlay would fall back to. */
  inherited_value: string | null;
  inherited_source: ConfigSource;
}

/* ---------------------------------- monitor --------------------------------- */

export interface MonitorResponse {
  generated_at: string;
  services: MonitorService[];
  active_agents: MonitorAgent[];
  background_processes: MonitorBackgroundProcess[];
  /** Shared job summaries (`zdx_engine::jobs::JobSummary`). */
  jobs: MonitorJob[];
  /** Set when jobs could not be listed at all (for example before `zdx jobs migrate`). */
  jobs_error: string | null;
  config: MonitorConfig;
  usage: MonitorUsage | null;
  subscriptions: MonitorSubscription[];
  /**
   * Quotas are fetched off the request path, so an empty `subscriptions` means
   * "not fetched yet" under `pending` and "none" only under `ready`/`stale`.
   */
  subscriptions_status: "ready" | "pending" | "stale";
  subscriptions_age: string | null;
}

export interface MonitorService {
  name: string;
  running: boolean;
  installed: boolean;
  pid: number | null;
  uptime: string | null;
}

export interface MonitorAgent {
  pid: number;
  thread_id: string | null;
  parent_thread_id: string | null;
  surface: string | null;
  role: string | null;
  model: string | null;
  provider: string | null;
  account: string | null;
  thinking: string | null;
  uptime: string;
  run: RunStatus;
}

export interface MonitorBackgroundProcess {
  id: string;
  pid: number;
  thread_id: string | null;
  command: string;
  uptime: string;
}

export type JobSchedule =
  | { kind: "manual" }
  | { kind: "recurring"; cron: string }
  | { kind: "once"; run_at: string };

export type JobStatus =
  | "invalid"
  | "manual"
  | "scheduled"
  | "pending"
  | "due"
  | "expired"
  | "running"
  | "interrupted"
  | "finished";

export type JobOutcome = "succeeded" | "failed" | "retrying" | "missed" | "interrupted";

export interface MonitorJobLastRun {
  run_id: string;
  trigger: "manual" | "daemon";
  outcome: JobOutcome;
  scheduled_for: string | null;
  finished_at: string;
  error: string | null;
  thread_id: string | null;
}

export interface MonitorJob {
  name: string;
  /** `null` when the job file is invalid; `error` says why. */
  schedule: JobSchedule | null;
  status: JobStatus;
  next_run_at: string | null;
  last_run: MonitorJobLastRun | null;
  error: string | null;
}

export interface MonitorConfigModel {
  role: string;
  model: string;
}

export interface MonitorConfig {
  model: string;
  thinking: string;
  max_tokens: number | null;
  subagents_enabled: boolean;
  mode_count: number;
  helper_models: MonitorConfigModel[];
  server_enabled: boolean;
  server_port: number;
}

export interface MonitorUsageRow {
  provider: string;
  model: string | null;
  requests: number;
  tokens: number;
  cost_usd: number;
  subscription: boolean;
  estimated: boolean;
}

export interface MonitorDailyUsage {
  day: number;
  tokens: number;
}

export interface MonitorUsage {
  span: string;
  requests: number;
  tokens: number;
  input: number;
  output: number;
  cache_read: number;
  cache_write: number;
  billed_usd: number;
  subscription_tokens: number;
  unknown_pricing_rows: number;
  threads_scanned: number;
  by_provider: MonitorUsageRow[];
  by_model: MonitorUsageRow[];
  daily: MonitorDailyUsage[];
}

export interface MonitorQuotaWindow {
  label: string;
  used_percent: number;
  resets_at: string | null;
  scope: string | null;
}

export interface MonitorSubscription {
  provider: string;
  name: string;
  plan: string | null;
  windows: MonitorQuotaWindow[];
  error: string | null;
}

/* ------------------------------------ git ----------------------------------- */

export type GitFileKind = "staged" | "unstaged" | "untracked" | "commit" | "all";

/** Files touched by a history scope (`all` or one commit). */
export interface GitScopeResponse {
  scope: "all" | "commit";
  /** Resolved base revision for `all`, or the commit itself. */
  base: string | null;
  /** For `all`: the main branch the base was taken from (`master`,
   *  `origin/main`). Null for commits, or when no main branch was found. */
  base_ref: string | null;
  /** For `all`: commits on this branch since the base. */
  ahead: number;
  files: GitFile[];
  /** Subset of `files` that are untracked — request those diffs with
   *  `kind=untracked`, since they are not part of any diff. */
  untracked: string[];
}

export interface GitResponse {
  repository: GitRepository;
  worktrees: GitWorktree[];
  files: GitFiles;
  commits: GitCommit[];
}

export interface GitRepository {
  name: string;
  root: string;
  source: "thread" | "bot_root";
  thread_id: string | null;
  branch: string;
  head: string | null;
  upstream: string | null;
  ahead: number;
  behind: number;
  detached: boolean;
  clean: boolean;
}

export interface GitFile {
  path: string;
  status: string;
  original_path?: string;
}

export interface GitFiles {
  staged: GitFile[];
  unstaged: GitFile[];
  untracked: GitFile[];
}

export interface GitWorktree {
  path: string;
  head: string | null;
  branch: string | null;
  current: boolean;
  flags: Array<"detached" | "bare" | "locked" | "prunable">;
}

export interface GitCommit {
  hash: string;
  short_hash: string;
  author: string;
  authored_at: string;
  relative_time: string;
  refs: string;
  subject: string;
}

export interface GitDiffResponse {
  path: string;
  kind: GitFileKind;
  content: string;
  bytes: number;
  lines: number;
  truncated: boolean;
  limit_bytes: number;
}
