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
  activity: ThreadActivity[];
}

export interface ActivityBase {
  /** Source-event index. Has gaps — do not use as an array index. */
  sequence: number;
  /** Preformatted local "HH:MM AM/PM", or "--:--". */
  time: string;
}

export interface MessageActivity extends ActivityBase {
  type: "message";
  role: string;
  speaker: "You" | "Z";
  text: string;
  phase?: string;
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
  id: string;
  name: string;
  summary: string;
  input: Json;
  output_tail: string;
  running_for: string;
}

export interface ToolResultActivity extends ActivityBase {
  type: "tool_result";
  tool_use_id: string;
  ok: boolean;
  duration_ms?: number;
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
  /** Context window of the model that served this request. */
  context_limit?: number;
}

export interface NoticeActivity extends ActivityBase {
  type: "notice";
  kind: "refusal" | "context_window_exceeded" | "goal";
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

/* ---------------------------------- monitor --------------------------------- */

export interface MonitorResponse {
  generated_at: string;
  services: MonitorService[];
  active_agents: MonitorAgent[];
  background_processes: MonitorBackgroundProcess[];
  automations: MonitorAutomation[];
  config: MonitorConfig;
  usage: MonitorUsage | null;
  subscriptions: MonitorSubscription[];
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
  current_tool: string | null;
  phase: string | null;
}

export interface MonitorBackgroundProcess {
  id: string;
  pid: number;
  thread_id: string | null;
  command: string;
  uptime: string;
}

export interface MonitorAutomation {
  name: string;
  schedule: string | null;
}

export interface MonitorConfigModel {
  role: string;
  model: string;
}

export interface MonitorConfig {
  model: string;
  thinking: string;
  max_tokens: number | null;
  tool_timeout_secs: number;
  subagents_enabled: boolean;
  favorite_count: number;
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

export type GitFileKind = "staged" | "unstaged" | "untracked";

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
