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
import type { FeedbackSubmissionPayload } from "./annotate";
import type {
  ArtifactFeedbackResponse,
  ArtifactImageCopy,
  ArtifactVersion,
  ArtifactsResponse,
  GitDiffResponse,
  GitResponse,
  GitScopeResponse,
  MonitorResponse,
  ThreadListResponse,
  ThreadResponse,
  ThreadTrajectoryReport,
  WorkersResponse,
  WorkspaceSetting,
  WorkspacesResponse,
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
      workers: { total: 5, running: 1, queued: 1, failed: 1, settled: 1, unknown: 1 },
      is_worker: false,
      run: {
        state: "tool",
        label: "bash: just ci-fast",
        started_at: "2026-09-20T17:16:00.000Z",
        turn_elapsed_secs: 38,
        seconds_since_activity: 1,
        seconds_since_heartbeat: 3,
        running_tools: 1,
        finished_tools: 1,
      },
    },
    {
      id: "6a1c0b52-2f4e-4c55-9a7e-0e3b6f1d9c21",
      title: "Fix flaky retry test",
      root_path: "/home/dev/projects/service-api",
      project: "service-api",
      age: "6m",
      telegram_link: null,
      workers: null,
      is_worker: true,
      run: null,
    },
    {
      id: "telegram--1002345678901-topic-412",
      title: "Remote config activates on restart",
      root_path: "/Users/me/projects/work/brevity-dozer",
      project: "brevity-dozer",
      age: "3h",
      telegram_link: "https://t.me/c/2345678901/412",
      workers: null,
      is_worker: false,
      run: null,
    },
    {
      id: "3d6f20e3-78d4-4d1f-9f8e-4d442f3bba31",
      title: "Rework the transcript wrapping",
      root_path: "/Users/me/projects/personal/zdx",
      project: "zdx",
      age: "2d",
      telegram_link: null,
      workers: null,
      is_worker: false,
      run: null,
    },
  ],
};

export const demoWorkers: WorkersResponse = {
  workers: [
    {
      thread_id: "telegram--1001234567890-topic-18101",
      title: "Fix flaky retry test",
      status: "running",
      live: true,
      queue_depth: 0,
      root_path: "/home/dev/projects/service-api",
      project: "service-api",
      run: {
        state: "tool",
        label: "bash: cargo test -p service-api --lib",
        started_at: "2026-09-20T17:13:00.000Z",
        turn_elapsed_secs: 192,
        seconds_since_activity: 4,
        seconds_since_heartbeat: 2,
        running_tools: 1,
        finished_tools: 0,
      },
      last_error: null,
      age: null,
      telegram_link: "https://t.me/c/1234567890/18101",
    },
    {
      thread_id: "telegram--1001234567890-topic-18102",
      title: "Wire retry budget config",
      status: "queued",
      live: true,
      queue_depth: 2,
      root_path: "/home/dev/projects/service-api",
      project: "service-api",
      run: null,
      last_error: null,
      age: null,
      telegram_link: null,
    },
    {
      thread_id: "telegram--1001234567890-topic-18103",
      title: "Migrate schema to v9",
      status: "failed",
      live: true,
      queue_depth: 0,
      root_path: "/home/dev/projects/service-api",
      project: "service-api",
      run: null,
      last_error: "migration aborted: lock held",
      age: null,
      telegram_link: null,
    },
    {
      thread_id: "telegram--1001234567890-topic-18104",
      title: "Add pagination to list endpoint",
      status: "completed",
      live: true,
      queue_depth: 0,
      root_path: "/home/dev/projects/service-api",
      project: "service-api",
      run: null,
      last_error: null,
      age: null,
      telegram_link: null,
    },
    {
      thread_id: "telegram--1001234567890-topic-18099",
      title: "Backfill search index",
      status: "unknown",
      live: false,
      queue_depth: 0,
      root_path: "/home/dev/projects/service-api",
      project: "service-api",
      run: null,
      last_error: null,
      age: "3h",
      telegram_link: null,
    },
  ],
};

const demoNow = Date.now();
const demoLiveInputAt = new Date(demoNow - 38_000).toISOString();
const demoLiveStartedAt = new Date(demoNow - 37_000).toISOString();

export const demoThread: ThreadResponse = {
  id: "telegram--1001234567890-topic-18012",
  title: "Svelte Mini App for the bot",
  total_messages: 5,
  total_events: 16,
  cursor: 16,
  partial: false,
  parent_thread_id: null,
  parent_title: null,
  worker_count: 5,
  telegram_link: "https://t.me/c/1234567890/18012",
  run: {
    state: "tool",
    label: "bash: just ci-fast",
    started_at: demoLiveInputAt,
    turn_elapsed_secs: 38,
    seconds_since_activity: 1,
    seconds_since_heartbeat: 3,
    running_tools: 1,
    finished_tools: 1,
  },
  activity: [
    {
      type: "message",
      sequence: 0,
      time: "5:12 PM",
      ts: "2026-09-20T17:12:00.000Z",
      role: "user",
      speaker: "You",
      text: "lets build the mini app in svelte",
    },
    {
      type: "reasoning",
      sequence: 1,
      time: "5:12 PM",
      ts: "2026-09-20T17:12:00.420Z",
      redacted: false,
      text: "The user wants a Svelte Mini App. I should ground the design in real token values rather than guessing, then scaffold apps/web without touching the working shell.",
    },
    {
      type: "usage",
      sequence: 2,
      time: "5:12 PM",
      ts: "2026-09-20T17:12:01.980Z",
      input_tokens: 47200,
      output_tokens: 840,
      cache_read_tokens: 41000,
      cache_write_tokens: 6200,
      model: "claude-sonnet-4-6",
      provider: "anthropic",
      duration_ms: 1860,
      ttft_ms: 320,
      started_at: "2026-09-20T17:12:00.120Z",
      completed_at: "2026-09-20T17:12:01.980Z",
    },
    {
      type: "tool_use",
      sequence: 3,
      time: "5:12 PM",
      ts: "2026-09-20T17:12:02.100Z",
      id: "toolu_01",
      name: "Grep",
      input: { pattern: "--diff-addition", path: "app.css", case_insensitive: false },
    },
    {
      type: "tool_use",
      sequence: 4,
      time: "5:12 PM",
      ts: "2026-09-20T17:12:02.100Z",
      id: "toolu_02",
      name: "Read",
      input: { file_path: "apps/web/src/app.css", offset: 1, limit: 180 },
    },
    {
      type: "tool_result",
      sequence: 5,
      time: "5:12 PM",
      ts: "2026-09-20T17:12:02.750Z",
      tool_use_id: "toolu_01",
      ok: true,
      duration_ms: 650,
      started_at: "2026-09-20T17:12:02.100Z",
      completed_at: "2026-09-20T17:12:02.750Z",
      output: "--diff-addition-background:light-dark(#e9f5ed,#183323)\n--diff-addition-foreground:light-dark(#2f8052,#4ba86e)",
    },
    {
      type: "tool_result",
      sequence: 6,
      time: "5:12 PM",
      ts: "2026-09-20T17:12:03.800Z",
      tool_use_id: "toolu_02",
      ok: true,
      duration_ms: 1700,
      started_at: "2026-09-20T17:12:02.100Z",
      completed_at: "2026-09-20T17:12:03.800Z",
      output: "@theme { --color-success: light-dark(#2f8052, #54b879); }",
    },
    {
      type: "usage",
      sequence: 7,
      time: "5:12 PM",
      ts: "2026-09-20T17:12:05.200Z",
      input_tokens: 48213,
      output_tokens: 3120,
      cache_read_tokens: 41000,
      cache_write_tokens: 7200,
      model: "claude-sonnet-4-6",
      provider: "anthropic",
      duration_ms: 1350,
      ttft_ms: 240,
      started_at: "2026-09-20T17:12:03.850Z",
      completed_at: "2026-09-20T17:12:05.200Z",
    },
    {
      type: "message",
      sequence: 8,
      time: "5:12 PM",
      ts: "2026-09-20T17:12:05.220Z",
      role: "assistant",
      speaker: "Z",
      text: "I recovered the **real** token system and verified the palette in parallel.",
      artifacts: [
        {
          name: "provider-comparison.html",
          path: "/Users/me/.zdx/artifacts/threads/demo/provider-comparison.html",
          mime: "text/html",
          kind: "html",
          size_bytes: 48210,
          exists: true,
        },
      ],
    },
    {
      type: "message",
      sequence: 9,
      time: "5:14 PM",
      ts: "2026-08-30T17:14:00Z",
      role: "user",
      speaker: "You",
      text: "run the old build once more",
    },
    {
      type: "tool_use",
      sequence: 10,
      time: "5:14 PM",
      ts: "2026-08-30T17:14:01Z",
      id: "toolu_legacy",
      name: "Bash",
      input: { command: "bun run build" },
    },
    {
      type: "tool_result",
      sequence: 11,
      time: "5:14 PM",
      ts: "2026-08-30T17:14:10Z",
      tool_use_id: "toolu_legacy",
      ok: false,
      duration_ms: 8530,
      output: "error during build:\nCould not resolve entry module \"@codemirror/view\".",
    },
    {
      type: "usage",
      sequence: 12,
      time: "5:15 PM",
      ts: "2026-08-30T17:15:00Z",
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
      type: "message",
      sequence: 13,
      time: "5:15 PM",
      ts: "2026-08-30T17:15:01Z",
      role: "assistant",
      speaker: "Z",
      text: "The legacy build failed before timing spans were recorded.",
    },
    {
      type: "notice",
      sequence: 14,
      time: "5:15 PM",
      ts: "2026-09-20T17:15:01.000Z",
      kind: "output_limit",
      message: "Generation stopped at the output token limit, so this answer may be incomplete.",
    },
    {
      type: "message",
      sequence: 15,
      time: "5:16 PM",
      ts: demoLiveInputAt,
      role: "user",
      speaker: "You",
      text: "run the focused checks",
    },
    {
      type: "tool_running",
      sequence: 16,
      time: "5:16 PM",
      ts: demoLiveStartedAt,
      started_at: demoLiveStartedAt,
      id: "toolu_02b",
      name: "read",
      summary: "justfile",
      input: { path: "justfile" },
      output_tail: "",
      running_for: "0s",
      completed_at: demoLiveStartedAt,
      ok: true,
      duration_ms: 40,
    },
    {
      type: "tool_running",
      sequence: 17,
      time: "5:16 PM",
      ts: demoLiveStartedAt,
      started_at: demoLiveStartedAt,
      id: "toolu_03",
      name: "bash",
      summary: "just ci-fast",
      input: { command: "just ci-fast" },
      output_tail: "    Checking zdx-bot v0.10.0\n    Checking zdx-cli v0.10.0",
      running_for: "37s",
    },
  ],
};

export const demoTrajectory: ThreadTrajectoryReport = {
  source_event_count: 17,
  observed_at: new Date(demoNow).toISOString(),
  measured: 7,
  total: 7,
  exact: 5,
  active_wall_ms: 41_910,
  span_work_ms: 42_560,
  concurrent_work_ms: 650,
  has_unavailable: false,
  bottlenecks: [
    {
      key: "tool:3:16:toolu_03",
      kind: "tool",
      turn: 3,
      label: "bash",
      detail: "just ci-fast",
      status: "running",
      timing: "exact",
      duration_ms: 37_000,
    },
    {
      key: "model:2:12",
      kind: "model",
      turn: 2,
      label: "Request 1",
      detail: "anthropic:claude-sonnet-4-6",
      status: "ok",
      timing: "duration",
      duration_ms: 12_400,
    },
    {
      key: "tool:2:10:toolu_legacy",
      kind: "tool",
      turn: 2,
      label: "bash",
      detail: "bun run build",
      status: "failed",
      timing: "duration",
      duration_ms: 8_530,
    },
    {
      key: "model:1:2",
      kind: "model",
      turn: 1,
      label: "Request 1",
      detail: "anthropic:claude-sonnet-4-6",
      status: "ok",
      timing: "exact",
      duration_ms: 1_860,
    },
    {
      key: "tool:1:4:toolu_02",
      kind: "tool",
      turn: 1,
      label: "read",
      detail: "apps/web/src/app.css",
      status: "ok",
      timing: "exact",
      duration_ms: 1_700,
    },
    {
      key: "model:1:7",
      kind: "model",
      turn: 1,
      label: "Request 2",
      detail: "anthropic:claude-sonnet-4-6",
      status: "ok",
      timing: "exact",
      duration_ms: 1_350,
    },
    {
      key: "tool:1:3:toolu_01",
      kind: "tool",
      turn: 1,
      label: "grep",
      detail: "app.css",
      status: "ok",
      timing: "exact",
      duration_ms: 650,
    },
  ],
  turns: [
    {
      index: 1,
      input: {
        sequence: 0,
        ts: "2026-09-20T17:12:00.000Z",
        text: "lets build the mini app in svelte",
      },
      timing: "exact",
      measured: 4,
      total: 4,
      domain_start_ms: Date.parse("2026-09-20T17:12:00.000Z"),
      domain_end_ms: Date.parse("2026-09-20T17:12:05.200Z"),
      elapsed_ms: 5_200,
      active_wall_ms: 4_910,
      span_work_ms: 5_560,
      concurrent_work_ms: 650,
      model_tracks: 1,
      tool_tracks: 2,
      spans: [
        {
          key: "input:0",
          kind: "input",
          lane: "input",
          turn: 1,
          sequence: 0,
          label: "Input",
          detail: "lets build the mini app in svelte",
          status: "ok",
          timing: "exact",
          started_at: "2026-09-20T17:12:00.000Z",
          start_ms: Date.parse("2026-09-20T17:12:00.000Z"),
          end_ms: Date.parse("2026-09-20T17:12:00.000Z"),
          duration_ms: 0,
          wall_ms: 0,
          track: 0,
        },
        {
          key: "model:1:2",
          kind: "model",
          lane: "model",
          turn: 1,
          sequence: 2,
          label: "Request 1",
          detail: "anthropic:claude-sonnet-4-6",
          status: "ok",
          timing: "exact",
          started_at: "2026-09-20T17:12:00.120Z",
          completed_at: "2026-09-20T17:12:01.980Z",
          start_ms: Date.parse("2026-09-20T17:12:00.120Z"),
          end_ms: Date.parse("2026-09-20T17:12:01.980Z"),
          duration_ms: 1_860,
          wall_ms: 1_860,
          track: 0,
          model: "claude-sonnet-4-6",
          provider: "anthropic",
          ttft_ms: 320,
          input_tokens: 47_200,
          output_tokens: 840,
          cache_read_tokens: 41_000,
          cache_write_tokens: 6_200,
        },
        {
          key: "tool:1:3:toolu_01",
          kind: "tool",
          lane: "tools",
          turn: 1,
          sequence: 3,
          result_sequence: 5,
          tool_use_id: "toolu_01",
          label: "grep",
          detail: "app.css",
          status: "ok",
          timing: "exact",
          started_at: "2026-09-20T17:12:02.100Z",
          completed_at: "2026-09-20T17:12:02.750Z",
          start_ms: Date.parse("2026-09-20T17:12:02.100Z"),
          end_ms: Date.parse("2026-09-20T17:12:02.750Z"),
          duration_ms: 650,
          wall_ms: 650,
          track: 0,
        },
        {
          key: "tool:1:4:toolu_02",
          kind: "tool",
          lane: "tools",
          turn: 1,
          sequence: 4,
          result_sequence: 6,
          tool_use_id: "toolu_02",
          label: "read",
          detail: "apps/web/src/app.css",
          status: "ok",
          timing: "exact",
          started_at: "2026-09-20T17:12:02.100Z",
          completed_at: "2026-09-20T17:12:03.800Z",
          start_ms: Date.parse("2026-09-20T17:12:02.100Z"),
          end_ms: Date.parse("2026-09-20T17:12:03.800Z"),
          duration_ms: 1_700,
          wall_ms: 1_700,
          track: 1,
        },
        {
          key: "model:1:7",
          kind: "model",
          lane: "model",
          turn: 1,
          sequence: 7,
          label: "Request 2",
          detail: "anthropic:claude-sonnet-4-6",
          status: "ok",
          timing: "exact",
          started_at: "2026-09-20T17:12:03.850Z",
          completed_at: "2026-09-20T17:12:05.200Z",
          start_ms: Date.parse("2026-09-20T17:12:03.850Z"),
          end_ms: Date.parse("2026-09-20T17:12:05.200Z"),
          duration_ms: 1_350,
          wall_ms: 1_350,
          track: 0,
          model: "claude-sonnet-4-6",
          provider: "anthropic",
          ttft_ms: 240,
          input_tokens: 48_213,
          output_tokens: 3_120,
          cache_read_tokens: 41_000,
          cache_write_tokens: 7_200,
        },
      ],
    },
    {
      index: 2,
      input: {
        sequence: 9,
        ts: "2026-08-30T17:14:00Z",
        text: "run the old build once more",
      },
      timing: "legacy",
      measured: 2,
      total: 2,
      model_tracks: 1,
      tool_tracks: 1,
      spans: [
        {
          key: "input:9",
          kind: "input",
          lane: "input",
          turn: 2,
          sequence: 9,
          label: "Input",
          detail: "run the old build once more",
          status: "ok",
          timing: "unavailable",
          started_at: "2026-08-30T17:14:00Z",
          duration_ms: 0,
          wall_ms: 0,
          track: 0,
        },
        {
          key: "tool:2:10:toolu_legacy",
          kind: "tool",
          lane: "tools",
          turn: 2,
          sequence: 10,
          result_sequence: 11,
          tool_use_id: "toolu_legacy",
          label: "bash",
          detail: "bun run build",
          status: "failed",
          timing: "duration",
          duration_ms: 8_530,
          track: 0,
        },
        {
          key: "model:2:12",
          kind: "model",
          lane: "model",
          turn: 2,
          sequence: 12,
          label: "Request 1",
          detail: "anthropic:claude-sonnet-4-6",
          status: "ok",
          timing: "duration",
          duration_ms: 12_400,
          ttft_ms: 780,
          track: 0,
          model: "claude-sonnet-4-6",
          provider: "anthropic",
          input_tokens: 48_213,
          output_tokens: 3_120,
          cache_read_tokens: 41_000,
          cache_write_tokens: 7_200,
        },
      ],
    },
    {
      index: 3,
      input: { sequence: 15, ts: demoLiveInputAt, text: "run the focused checks" },
      timing: "exact",
      measured: 1,
      total: 1,
      domain_start_ms: Date.parse(demoLiveInputAt),
      domain_end_ms: demoNow,
      elapsed_ms: 38_000,
      active_wall_ms: 37_000,
      span_work_ms: 37_000,
      concurrent_work_ms: 0,
      model_tracks: 1,
      tool_tracks: 1,
      spans: [
        {
          key: "input:15",
          kind: "input",
          lane: "input",
          turn: 3,
          sequence: 15,
          label: "Input",
          detail: "run the focused checks",
          status: "ok",
          timing: "exact",
          started_at: demoLiveInputAt,
          start_ms: Date.parse(demoLiveInputAt),
          end_ms: Date.parse(demoLiveInputAt),
          duration_ms: 0,
          wall_ms: 0,
          track: 0,
        },
        {
          key: "tool:3:16:toolu_03",
          kind: "tool",
          lane: "tools",
          turn: 3,
          sequence: 16,
          tool_use_id: "toolu_03",
          label: "bash",
          detail: "just ci-fast",
          status: "running",
          timing: "exact",
          started_at: demoLiveStartedAt,
          start_ms: Date.parse(demoLiveStartedAt),
          end_ms: demoNow,
          wall_ms: 37_000,
          track: 0,
          live_input: { command: "just ci-fast" },
          live_output_tail: "Checking zdx-bot v0.11.0",
        },
      ],
    },
  ],
};

export const demoArtifacts: ArtifactsResponse = {
  artifacts: [
    {
      key: "1a2b3c4d5e6f7a8b",
      name: "provider-comparison.html",
      path: "/Users/me/.zdx/artifacts/threads/demo/provider-comparison.html",
      rel: "provider-comparison.html",
      mime: "text/html",
      kind: "html",
      size_bytes: 48210,
      exists: true,
      source: "both",
      message_sequences: [8],
      modified_at: "2026-09-20T17:12:05.220Z",
    },
    {
      key: "2b3c4d5e6f7a8b9c",
      name: "latency-chart.png",
      path: "/Users/me/.zdx/artifacts/threads/demo/latency-chart.png",
      rel: "latency-chart.png",
      mime: "image/png",
      kind: "image",
      size_bytes: 61234,
      exists: true,
      source: "generated",
      message_sequences: [],
      modified_at: "2026-09-20T17:11:00.000Z",
    },
    {
      key: "3c4d5e6f7a8b9c0d",
      name: "logo.svg",
      path: "/Users/me/.zdx/artifacts/threads/demo/logo.svg",
      rel: "logo.svg",
      mime: "image/svg+xml",
      kind: "image",
      size_bytes: 412,
      exists: true,
      source: "generated",
      message_sequences: [],
      modified_at: "2026-09-20T17:09:00.000Z",
    },
    {
      key: "4d5e6f7a8b9c0d1e",
      name: "voice-note.ogg",
      path: "/Users/me/.zdx/artifacts/threads/demo/voice-note.ogg",
      rel: "voice-note.ogg",
      mime: "audio/ogg",
      kind: "audio",
      size_bytes: 182400,
      exists: true,
      source: "generated",
      message_sequences: [],
      modified_at: "2026-09-20T17:10:00.000Z",
    },
    {
      key: "5e6f7a8b9c0d1e2f",
      name: "release-notes.md",
      path: "/Users/me/.zdx/artifacts/threads/demo/release-notes.md",
      rel: "release-notes.md",
      mime: "text/markdown",
      kind: "markdown",
      size_bytes: 620,
      exists: true,
      source: "generated",
      message_sequences: [],
      modified_at: "2026-09-20T17:08:00.000Z",
    },
    {
      key: "6f7a8b9c0d1e2f3a",
      name: "retry.rs",
      path: "/Users/me/.zdx/artifacts/threads/demo/retry.rs",
      rel: "retry.rs",
      mime: "text/plain",
      kind: "text",
      size_bytes: 540,
      exists: true,
      source: "generated",
      message_sequences: [],
      modified_at: "2026-09-20T17:07:00.000Z",
    },
  ],
};

const DEMO_MARKDOWN = `# Release notes

Changes since **v0.4**:

- Text, Markdown, and code artifacts open in the preview.
- Long files show the first part, with a download for the rest.

| Kind | Preview |
| --- | --- |
| Markdown | rendered, with a Source view |
| Code | line numbers and syntax colours |

\`\`\`rust
fn main() { println!("hi"); }
\`\`\`

See [the docs](https://example.com/docs).
`;

const DEMO_RUST = `use std::time::Duration;

/// Retries \`op\` up to \`attempts\` times with a fixed delay.
pub fn retry<T, E>(attempts: u32, delay: Duration, mut op: impl FnMut() -> Result<T, E>) -> Result<T, E> {
    let mut last = op();
    for _ in 1..attempts {
        if last.is_ok() {
            break;
        }
        std::thread::sleep(delay);
        last = op();
    }
    last
}
`;

/**
 * A page that exercises annotation: a fixed header, an inner scroll container,
 * a transformed card, a canvas chart, a link, and a button.
 */
function demoArtifactPage(path: string): string {
  const rows = Array.from(
    { length: 12 },
    (_, i) => `<tr><td>Provider ${i + 1}</td><td>${((i * 7) % 40) + 10} ms</td><td>$${(i * 0.13 + 0.4).toFixed(2)}</td></tr>`,
  ).join("");
  return `<!doctype html><html><head><meta charset="utf-8"><meta name="viewport" content="width=device-width, initial-scale=1"><title>Demo</title>
<style>body{font-family:system-ui,sans-serif;margin:0;color:#1b1f1b}header{position:fixed;top:0;left:0;right:0;background:#183323;color:#e9f5ed;padding:10px 16px;z-index:5}
main{padding:56px 16px 16px}h1{font-size:22px}.card{border:1px solid #ccd;border-radius:8px;padding:12px;margin:12px 0}.tilt{transform:rotate(-1.5deg)}
.scroll{max-height:140px;overflow:auto}.grid{display:grid;grid-template-columns:1fr 1fr;gap:8px}.plan h3{margin:0 0 4px}.price{font-weight:600}table{width:100%;border-collapse:collapse}td{border-bottom:1px solid #eee;padding:6px}button{padding:8px 12px}</style></head>
<body><header id="top">Provider report · ünïcødé ✓</header><main><h1 id="title">Provider comparison</h1><p class="lede">Latency and cost per provider, from ${path}.</p>
<div class="card" id="summary"><h2>Summary</h2><p>Provider 3 is fastest. <a href="https://example.com/details" id="details">Details</a></p>
<button id="refresh" onclick="document.getElementById('clicked').textContent='clicked'">Refresh</button> <span id="clicked"></span></div>
<section id="plans" class="plans"><h2>Plans</h2><div class="grid"><div class="card plan"><h3>Basic</h3><p class="price">$9 per month</p><ul><li>1 project</li><li>Email support</li></ul></div><div class="card plan"><h3>Pro</h3><p class="price">$29 per month</p><ul><li>10 projects</li><li>Priority support</li></ul></div></div></section>
<div class="card tilt" id="chart-card"><h2>Chart</h2><canvas id="chart" width="320" height="120"></canvas></div>
<div class="card"><h2>Table</h2><div class="scroll" id="table-scroll"><table>${rows}</table></div></div><footer><p>Generated for the demo.</p></footer></main>
<script>const c=document.getElementById('chart').getContext('2d');[40,80,30,100,60].forEach((h,i)=>{c.fillStyle='#2f8052';c.fillRect(20+i*60,120-h,40,h)});</script></body></html>`;
}

/** Demo submit: keeps the last payload on `window` so a browser test can read it. */
export function demoSendFeedback(payload: FeedbackSubmissionPayload): ArtifactFeedbackResponse {
  const w = window as unknown as { __zdxFeedback?: FeedbackSubmissionPayload[] };
  w.__zdxFeedback = [...(w.__zdxFeedback ?? []), payload];
  return { status: "queued", annotations: payload.annotations.length, stale: payload.allow_stale };
}

let demoChart: Promise<Blob> | null = null;

/**
 * A landscape PNG chart, so a portrait phone shows it letterboxed. Encoded
 * once: the same bytes every time, like a file on disk.
 */
function demoChartPng(): Promise<Blob> {
  demoChart ??= drawDemoChart();
  return demoChart;
}

async function drawDemoChart(): Promise<Blob> {
  const canvas = document.createElement("canvas");
  canvas.width = 1200;
  canvas.height = 800;
  const ctx = canvas.getContext("2d")!;
  ctx.fillStyle = "#ffffff";
  ctx.fillRect(0, 0, 1200, 800);
  ctx.fillStyle = "#1b1f1b";
  ctx.font = "600 44px system-ui, sans-serif";
  ctx.fillText("Latency by provider (ms)", 60, 90);
  [220, 480, 160, 600, 340, 420].forEach((h, i) => {
    ctx.fillStyle = i === 3 ? "#c0392b" : "#2f8052";
    ctx.fillRect(100 + i * 170, 720 - h, 110, h);
    ctx.fillStyle = "#4b504a";
    ctx.font = "28px system-ui, sans-serif";
    ctx.fillText(`P${i + 1}`, 130 + i * 170, 765);
  });
  ctx.strokeStyle = "#8a8f89";
  ctx.lineWidth = 3;
  ctx.strokeRect(2, 2, 1196, 796);
  return await new Promise<Blob>((resolve) => canvas.toBlob((blob) => resolve(blob!), "image/png"));
}

type DemoOverride = Blob | Omit<ArtifactImageCopy, "blob"> & { blob: Blob } | { error: string; original: Blob };

function demoOverride(path: string): DemoOverride | undefined {
  return (window as unknown as { __zdxDemoArtifacts?: Record<string, DemoOverride> }).__zdxDemoArtifacts?.[path];
}

async function sha256Of(blob: Blob): Promise<string> {
  const digest = await crypto.subtle.digest("SHA-256", await blob.arrayBuffer());
  return Array.from(new Uint8Array(digest), (b) => b.toString(16).padStart(2, "0")).join("");
}

/**
 * Stands in for `GET /artifacts/image`. The demo chart is already a small
 * upright PNG, so it is its own canonical copy. A browser test serves a copy
 * the engine made through `window.__zdxDemoArtifacts[path]` (`{ blob,
 * sourceSha256, canonicalSha256, width, height, sourceWidth, sourceHeight }`), or a refusal
 * (`{ error, original }`). Other images (the SVG) are refused like the server.
 */
export async function demoArtifactImage(path: string): Promise<ArtifactImageCopy | { error: string }> {
  const override = demoOverride(path);
  if (override && "error" in override) return { error: override.error };
  if (override && "canonicalSha256" in override) return override;
  if (!path.endsWith(".png")) return { error: "Annotation works on PNG, JPEG, and WebP images, not SVG files." };
  const blob = await demoChartPng();
  const sha = await sha256Of(blob);
  return { blob, sourceSha256: sha, canonicalSha256: sha, width: 1200, height: 800, sourceWidth: 1200, sourceHeight: 800 };
}

/** Stands in for `GET /artifacts/version`: the hash of what the demo would
 *  serve for `path` now (an image override names its source hash). */
export async function demoArtifactVersion(path: string): Promise<ArtifactVersion> {
  const override = demoOverride(path);
  if (override && "sourceSha256" in override) {
    return { sha256: override.sourceSha256, size_bytes: override.blob.size, modified_at: null };
  }
  const blob = await demoArtifactBlob(path);
  return { sha256: await sha256Of(blob), size_bytes: blob.size, modified_at: null };
}

/**
 * Demo bytes for artifact previews: a page, a PNG chart, a passive SVG,
 * Markdown and Rust text, and silence audio. A browser test can serve other bytes for a path through
 * `window.__zdxDemoArtifacts`.
 */
export async function demoArtifactBlob(path: string): Promise<Blob> {
  const override = demoOverride(path);
  if (override instanceof Blob) return override;
  if (override && "original" in override) return override.original;
  if (path.endsWith(".html")) {
    return new Blob([demoArtifactPage(path)], { type: "text/html" });
  }
  if (path.endsWith(".png")) return await demoChartPng();
  if (path.endsWith(".md")) return new Blob([DEMO_MARKDOWN], { type: "text/markdown" });
  if (path.endsWith(".rs")) return new Blob([DEMO_RUST], { type: "text/plain" });
  if (/\.(png|jpg|jpeg|gif|webp|svg)$/.test(path)) {
    return new Blob(
      [
        `<svg xmlns="http://www.w3.org/2000/svg" width="400" height="200"><rect width="400" height="200" fill="#183323"/><text x="20" y="110" fill="#e9f5ed" font-size="24">Demo image</text></svg>`,
      ],
      { type: "image/svg+xml" },
    );
  }
  return new Blob(["demo"], { type: "text/plain" });
}

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
      run: {
        state: "tool",
        label: "bash: just ci-fast",
        started_at: "2026-09-20T17:14:00.000Z",
        turn_elapsed_secs: 134,
        seconds_since_activity: 1,
        seconds_since_heartbeat: 3,
        running_tools: 1,
        finished_tools: 0,
      },
    },
  ],
  background_processes: [],
  jobs: [
    {
      name: "daily-review",
      schedule: { kind: "recurring", cron: "0 9 * * *" },
      status: "scheduled",
      next_run_at: "2026-09-21T12:00:00Z",
      last_run: {
        run_id: "demo-run-1",
        trigger: "daemon",
        outcome: "succeeded",
        scheduled_for: "2026-09-20T12:00:00Z",
        finished_at: "2026-09-20T12:04:10Z",
        error: null,
        thread_id: "job-daily-review-20260920-120000",
      },
      error: null,
    },
    {
      name: "renew-domain",
      schedule: { kind: "once", run_at: "2026-09-22T09:00:00-03:00" },
      status: "pending",
      next_run_at: "2026-09-22T12:00:00Z",
      last_run: null,
      error: null,
    },
    {
      name: "weekly-digest",
      schedule: { kind: "recurring", cron: "0 8 * * 1" },
      status: "scheduled",
      next_run_at: "2026-09-21T11:00:00Z",
      last_run: {
        run_id: "demo-run-2",
        trigger: "daemon",
        outcome: "failed",
        scheduled_for: "2026-09-14T11:00:00Z",
        finished_at: "2026-09-14T11:00:03Z",
        error: "execute prompt: HTTP 401: Invalid API Key",
        thread_id: "job-weekly-digest-20260914-110000",
      },
      error: null,
    },
    { name: "inbox-triage", schedule: { kind: "manual" }, status: "manual", next_run_at: null, last_run: null, error: null },
    {
      name: "broken",
      schedule: null,
      status: "invalid",
      next_run_at: null,
      last_run: null,
      error: "unknown field `schedul`, expected one of `schedule`, `run_at`, `grace_secs`, ...",
    },
  ],
  jobs_error: null,
  config: {
    model: "claude-sonnet-4-6",
    thinking: "medium",
    max_tokens: 64000,
    subagents_enabled: true,
    mode_count: 3,
    helper_models: [{ role: "title", model: "claude-haiku-5-5" }],
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
        {
          label: "5h",
          used_percent: 34,
          resets_at: new Date(Date.now() + 2 * 3_600_000 + 14 * 60_000).toISOString(),
          scope: "session",
        },
        {
          label: "7d",
          used_percent: 78,
          resets_at: new Date(Date.now() + 3 * 86_400_000 + 5 * 3_600_000).toISOString(),
          scope: "weekly",
        },
      ],
    },
  ],
  subscriptions_status: "ready",
  subscriptions_age: "12s",
};

const src = (layer: "default" | "global" | "workspace" | "ancestor", label: string) => ({
  layer,
  path: layer === "default" ? null : `/home/user/${label}/.zdx/config.toml`,
  label,
});

function demoSetting(
  key: string,
  group: string,
  label: string,
  value: string | null,
  source: ReturnType<typeof src>,
  inherited: [string | null, ReturnType<typeof src>] = [value, source],
): WorkspaceSetting {
  return {
    key,
    group,
    label,
    kind: "chat",
    value,
    source,
    inherited_value: inherited[0],
    inherited_source: inherited[1],
  };
}

export const demoWorkspaces: WorkspacesResponse = {
  workspaces: [
    {
      root: "/home/user/projects/example-app",
      name: "example-app",
      profiles: ["example-app"],
      current: true,
      overlay_path: "/home/user/projects/example-app/.zdx/config.toml",
      has_overlay: true,
      overlay_is_global: false,
      error: null,
      settings: [
        demoSetting("model", "core", "model", "anthropic:claude-opus@high", src("workspace", "example-app"), [
          "anthropic:claude-sonnet@medium",
          src("ancestor", "projects"),
        ]),
        demoSetting("title_model", "helper models", "title_model", "gemini:flash-lite", src("global", "global")),
        demoSetting("tldr_model", "helper models", "tldr_model", "gemini:flash-lite", src("default", "default")),
        demoSetting(
          "subagents.overrides.orchestrator.model",
          "subagents",
          "orchestrator",
          "openai:gpt-5@high",
          src("global", "global"),
        ),
        demoSetting("subagents.overrides.explorer.model", "subagents", "explorer", null, src("default", "default")),
      ],
    },
    {
      root: "/home/user/projects/other",
      name: "other",
      profiles: ["other", "other-orchestrator"],
      current: false,
      overlay_path: "/home/user/projects/other/.zdx/config.toml",
      has_overlay: false,
      overlay_is_global: false,
      error: null,
      settings: [
        demoSetting("model", "core", "model", "anthropic:claude-sonnet@medium", src("ancestor", "projects")),
      ],
    },
  ],
};
