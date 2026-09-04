/**
 * DEV-only fixtures.
 *
 * The Mini App can only talk to the real API with signed Telegram initData,
 * which makes iterating on transcript/diff rendering in a desktop browser
 * impossible. Appending `?demo=1` in a dev build serves these instead.
 *
 * Guarded by `import.meta.env.DEV`, so this module is dropped from production
 * bundles entirely.
 */
import type {
  GitDiffResponse,
  GitResponse,
  GitScopeResponse,
  MonitorResponse,
  ThreadListResponse,
  ThreadResponse,
} from "./types";

export function demoEnabled(): boolean {
  return import.meta.env.DEV && new URLSearchParams(window.location.search).has("demo");
}

export const demoScope: GitScopeResponse = {
  scope: "all",
  base: "e655104f491c3f59e6e358e5e00f1914538a5d22",
  base_ref: "master",
  ahead: 3,
  files: [
    { path: "apps/web/src/views/ChangesPane.svelte", status: "M" },
    { path: "apps/web/src/lib/api.ts", status: "M" },
    { path: "crates/zdx-bot/src/server.rs", status: "M" },
    { path: "apps/web/src/views/ThreadListView.svelte", status: "A" },
  ],
  untracked: ["apps/web/src/views/ThreadListView.svelte"],
};

export const demoThreads: ThreadListResponse = {
  threads: [
    {
      id: "telegram--1001234567890-topic-18012",
      title: "Svelte Mini App for the bot",
      root_path: "/Users/me/projects/personal/zdx",
      project: "zdx",
      age: "4m",
      telegram_link: "https://t.me/c/1234567890/18012",
    },
    {
      id: "telegram--1002345678901-topic-412",
      title: "Remote config activates on restart",
      root_path: "/Users/me/projects/work/brevity-dozer",
      project: "brevity-dozer",
      age: "3h",
      telegram_link: "https://t.me/c/2345678901/412",
    },
    {
      id: "3d6f20e3-78d4-4d1f-9f8e-4d442f3bba31",
      title: "Rework the transcript wrapping",
      root_path: "/Users/me/projects/personal/zdx",
      project: "zdx",
      age: "2d",
      telegram_link: null,
    },
  ],
};

export const demoThread: ThreadResponse = {
  id: "telegram--1001234567890-topic-18012",
  title: "Svelte Mini App for the bot",
  total_messages: 4,
  total_events: 9,
  cursor: 9,
  partial: false,
  telegram_link: "https://t.me/c/1234567890/18012",
  activity: [
    {
      type: "message",
      sequence: 0,
      time: "5:12 PM",
      role: "user",
      speaker: "You",
      text: "lets build the mini app in svelte",
    },
    {
      type: "reasoning",
      sequence: 1,
      time: "5:12 PM",
      redacted: false,
      text: "The user wants a Svelte Mini App. I should ground the design in real token values rather than guessing, then scaffold apps/web without touching the working shell.",
    },
    {
      type: "tool_use",
      sequence: 2,
      time: "5:12 PM",
      id: "toolu_01",
      name: "Grep",
      input: { pattern: "--diff-addition", path: "app.css", case_insensitive: false },
    },
    {
      type: "tool_result",
      sequence: 3,
      time: "5:12 PM",
      tool_use_id: "toolu_01",
      ok: true,
      duration_ms: 142,
      output: "--diff-addition-background:light-dark(#e9f5ed,#183323)\n--diff-addition-foreground:light-dark(#2f8052,#4ba86e)",
    },
    {
      type: "message",
      sequence: 4,
      time: "5:13 PM",
      role: "assistant",
      speaker: "Z",
      text: "I recovered the **real** token system from the shipped CSS bundle.\n\nKey findings:\n\n- `--font-sans` is the plain system stack — free to replicate exactly\n- `--font-mono` names a commercial face we do not vendor\n- The whole palette runs through `light-dark()` plus a `color-scheme` switch\n\n```ts\nconst addition = \"light-dark(#e9f5ed, #183323)\";\n```\n\nThat mechanism means no duplicated `dark:` variants anywhere.",
    },
    {
      type: "tool_use",
      sequence: 5,
      time: "5:14 PM",
      id: "toolu_02",
      name: "Bash",
      input: { command: "bun run build" },
    },
    {
      type: "tool_result",
      sequence: 6,
      time: "5:14 PM",
      tool_use_id: "toolu_02",
      ok: false,
      duration_ms: 8530,
      output: "error during build:\nCould not resolve entry module \"@codemirror/view\".",
    },
    {
      type: "notice",
      sequence: 7,
      time: "5:14 PM",
      kind: "goal",
      message: "Ship the Svelte Mini App at visual parity before cutover.",
    },
    {
      type: "usage",
      sequence: 8,
      time: "5:15 PM",
      input_tokens: 48213,
      output_tokens: 3120,
      cache_read_tokens: 41000,
      cache_write_tokens: 7200,
      model: "claude-sonnet-4-6",
      provider: "anthropic",
      duration_ms: 12400,
      ttft_ms: 780,
    },
    {
      type: "tool_running",
      sequence: 9,
      time: "5:15 PM",
      id: "toolu_03",
      name: "bash",
      summary: "just ci-fast",
      input: { command: "just ci-fast" },
      output_tail: "    Checking zdx-bot v0.10.0\n    Checking zdx-cli v0.10.0",
      running_for: "37s",
    },
  ],
};

export const demoGit: GitResponse = {
  repository: {
    name: "zdx",
    root: "/Users/me/projects/personal/zdx",
    source: "bot_root",
    thread_id: null,
    branch: "master",
    head: "a1b2c3d4",
    upstream: "origin/master",
    ahead: 2,
    behind: 0,
    detached: false,
    clean: false,
  },
  worktrees: [],
  files: {
    staged: [{ path: "apps/web/src/app.css", status: "A" }],
    unstaged: [{ path: "crates/zdx-bot/src/server.rs", status: "M" }],
    untracked: [{ path: "apps/web/src/lib/diff.ts", status: "?" }],
  },
  commits: [
    {
      hash: "a1b2c3d4e5f6",
      short_hash: "a1b2c3d",
      author: "Talles Borges",
      authored_at: "2026-08-31",
      relative_time: "2 hours ago",
      refs: "HEAD -> master",
      subject: "Scaffold Svelte Mini App with the new theme",
    },
  ],
};

export const demoDiff: GitDiffResponse = {
  path: "crates/zdx-bot/src/server.rs",
  kind: "unstaged",
  content: `diff --git a/crates/zdx-bot/src/server.rs b/crates/zdx-bot/src/server.rs
index 3f8a1c2..9d4e7b1 100644
--- a/crates/zdx-bot/src/server.rs
+++ b/crates/zdx-bot/src/server.rs
@@ -31,9 +31,11 @@ use crate::telegram::Client;
 
-const APP_HTML: &str = include_str!("app.html");
+#[derive(RustEmbed)]
+#[folder = "../../apps/web/dist"]
+struct Assets;
 
 pub(crate) fn create_router(state: Arc<ServerState>) -> Router {
     let api = Router::new()
         .route("/threads/{id}", get(thread_handler))
-        .route("/monitor", get(monitor_handler));
+        .route("/monitor", get(monitor_handler))
+        .route("/activity", get(activity_handler));
 
     Router::new().nest("/api", api)
 }
@@ -88,7 +90,7 @@ async fn thread_handler(
     // Cache the snapshot so the hot path never rebuilds it.
-    let ttl = Duration::from_secs(30);
+    let ttl = Duration::from_secs(15);
     let mut cache = state.monitor_cache.lock().await;
     if let Some(hit) = cache.get("monitor") {
         return Ok(Json(hit.clone()));`,
  bytes: 981,
  lines: 30,
  truncated: false,
  limit_bytes: 262144,
};

export const demoMonitor: MonitorResponse = {
  generated_at: new Date().toISOString(),
  services: [
    { name: "bot", running: true, installed: true, pid: 4821, uptime: "3d 4h" },
    { name: "scheduler", running: true, installed: true, pid: 4822, uptime: "3d 4h" },
    { name: "indexer", running: false, installed: true, pid: null, uptime: null },
  ],
  active_agents: [
    {
      pid: 91234,
      thread_id: "telegram--1001234567890-topic-18012",
      parent_thread_id: null,
      surface: "telegram",
      role: "main",
      model: "claude-sonnet-4-6",
      provider: "anthropic",
      account: "max",
      thinking: "medium",
      uptime: "2m 14s",
      current_tool: "bash",
      phase: "answering",
    },
  ],
  background_processes: [],
  automations: [
    { name: "daily-review", schedule: "0 9 * * *" },
    { name: "inbox-triage", schedule: null },
  ],
  config: {
    model: "claude-sonnet-4-6",
    thinking: "medium",
    max_tokens: 64000,
    tool_timeout_secs: 120,
    subagents_enabled: true,
    favorite_count: 7,
    helper_models: [{ role: "title", model: "claude-haiku-4-5" }],
    server_enabled: true,
    server_port: 4141,
  },
  usage: {
    span: "30 days",
    requests: 1842,
    tokens: 48_200_000,
    input: 41_000_000,
    output: 7_200_000,
    cache_read: 33_000_000,
    cache_write: 5_100_000,
    billed_usd: 12.44,
    subscription_tokens: 44_000_000,
    unknown_pricing_rows: 0,
    threads_scanned: 312,
    by_provider: [],
    by_model: [],
    daily: Array.from({ length: 30 }, (_, i) => ({
      day: i + 1,
      tokens: Math.round(400_000 + Math.sin(i / 2.5) * 300_000 + Math.random() * 400_000),
    })),
  },
  subscriptions: [
    {
      provider: "anthropic",
      name: "Claude Max",
      plan: "max-20x",
      error: null,
      windows: [
        { label: "5h", used_percent: 34, resets_at: null, scope: "session" },
        { label: "7d", used_percent: 78, resets_at: null, scope: "weekly" },
      ],
    },
  ],
};
