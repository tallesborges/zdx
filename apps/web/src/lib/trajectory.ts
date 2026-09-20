import type {
  Json,
  MessageActivity,
  ThreadActivity,
  ToolResultActivity,
  ToolRunningActivity,
  ToolUseActivity,
  UsageActivity,
} from "./types";

export type TrajectoryLane = "input" | "model" | "tools";
export type TimingQuality = "exact" | "duration" | "unavailable";
export type TurnTimingQuality = "exact" | "partial" | "legacy";

export interface TrajectorySpan {
  key: string;
  kind: "input" | "model" | "tool";
  lane: TrajectoryLane;
  turn: number;
  sequence: number;
  label: string;
  detail: string;
  status: "ok" | "failed" | "running" | "unknown";
  timing: TimingQuality;
  startedAt?: string;
  completedAt?: string;
  startMs?: number;
  endMs?: number;
  durationMs?: number;
  track: number;
  input?: Json | string;
  output?: Json | string;
  model?: string;
  provider?: string;
  ttftMs?: number;
  inputTokens?: number;
  outputTokens?: number;
  cacheReadTokens?: number;
  cacheWriteTokens?: number;
}

export interface TrajectoryTurn {
  index: number;
  input: MessageActivity | null;
  spans: TrajectorySpan[];
  timedSpans: TrajectorySpan[];
  unanchoredSpans: TrajectorySpan[];
  timing: TurnTimingQuality;
  measured: number;
  total: number;
  domainStartMs?: number;
  domainEndMs?: number;
  elapsedMs?: number;
  activeWallMs?: number;
  workMs?: number;
  overlapMs?: number;
  modelTracks: number;
  toolTracks: number;
}

export interface Trajectory {
  turns: TrajectoryTurn[];
  spans: TrajectorySpan[];
  bottlenecks: TrajectorySpan[];
  slowest: TrajectorySpan | null;
  measured: number;
  total: number;
  exact: number;
  activeWallMs?: number;
  workMs?: number;
  overlapMs?: number;
}

interface MutableTurn {
  index: number;
  input: MessageActivity | null;
  spans: TrajectorySpan[];
  requestCount: number;
}

interface PendingUsage {
  sequence: number;
  inputTokens: number;
  outputTokens: number;
  cacheReadTokens: number;
  cacheWriteTokens: number;
  activity: UsageActivity;
}

function parseTime(value: string | undefined): number | undefined {
  if (!value) return undefined;
  const parsed = Date.parse(value);
  return Number.isFinite(parsed) ? parsed : undefined;
}

function preciseEventTime(value: string): number | undefined {
  if (!/\.\d+(?:Z|[+-]\d\d:\d\d)$/.test(value)) return undefined;
  return parseTime(value);
}

function compact(text: string, max = 64): string {
  const flat = text.replace(/\s+/g, " ").trim();
  return flat.length > max ? `${flat.slice(0, max - 1)}…` : flat;
}

function object(value: Json): Record<string, Json> {
  return value && typeof value === "object" && !Array.isArray(value)
    ? (value as Record<string, Json>)
    : {};
}

function string(value: Json | undefined): string {
  return typeof value === "string" ? value : "";
}

function toolDetail(name: string, input: Json): string {
  const args = object(input);
  return compact(
    string(args.command) ||
      string(args.file_path) ||
      string(args.path) ||
      string(args.pattern) ||
      string(args.query) ||
      name,
  );
}

function exactBounds(
  startedAt: string | undefined,
  completedAt: string | undefined,
): { startMs: number; endMs: number; durationMs: number } | undefined {
  const startMs = parseTime(startedAt);
  const endMs = parseTime(completedAt);
  if (startMs === undefined || endMs === undefined || endMs < startMs) return undefined;
  return { startMs, endMs, durationMs: endMs - startMs };
}

function applyTiming(
  span: TrajectorySpan,
  durationMs: number | undefined,
  startedAt: string | undefined,
  completedAt: string | undefined,
): void {
  span.startedAt = startedAt;
  span.completedAt = completedAt;
  const bounds = exactBounds(startedAt, completedAt);
  if (bounds) {
    span.startMs = bounds.startMs;
    span.endMs = bounds.endMs;
    span.durationMs = bounds.durationMs;
    span.timing = "exact";
  } else if (durationMs !== undefined) {
    span.durationMs = durationMs;
    span.timing = "duration";
  }
}

function toolSpan(turn: MutableTurn, use: ToolUseActivity): TrajectorySpan {
  return {
    key: `tool:${turn.index}:${use.sequence}:${use.id}`,
    kind: "tool",
    lane: "tools",
    turn: turn.index,
    sequence: use.sequence,
    label: use.name.toLowerCase(),
    detail: toolDetail(use.name, use.input),
    status: "unknown",
    timing: "unavailable",
    track: 0,
    input: use.input,
  };
}

function applyToolResult(span: TrajectorySpan, result: ToolResultActivity): void {
  span.status = result.ok ? "ok" : "failed";
  span.output = result.output;
  applyTiming(span, result.duration_ms, result.started_at, result.completed_at);
}

function runningToolSpan(turn: MutableTurn, running: ToolRunningActivity): TrajectorySpan {
  const span: TrajectorySpan = {
    key: `tool:${turn.index}:${running.sequence}:${running.id}`,
    kind: "tool",
    lane: "tools",
    turn: turn.index,
    sequence: running.sequence,
    label: running.name.toLowerCase(),
    detail: running.summary || toolDetail(running.name, running.input),
    status: "running",
    timing: "unavailable",
    track: 0,
    input: running.input,
    output: running.output_tail,
  };
  const startMs = parseTime(running.started_at);
  if (startMs !== undefined) {
    const endMs = Math.max(startMs, Date.now());
    span.startedAt = running.started_at;
    span.startMs = startMs;
    span.endMs = endMs;
    span.durationMs = endMs - startMs;
    span.timing = "exact";
  }
  return span;
}

function modelSpan(turn: MutableTurn, pending: PendingUsage): TrajectorySpan {
  turn.requestCount += 1;
  const activity = pending.activity;
  const identity = [activity.provider, activity.model].filter(Boolean).join(":");
  const span: TrajectorySpan = {
    key: `model:${turn.index}:${pending.sequence}`,
    kind: "model",
    lane: "model",
    turn: turn.index,
    sequence: pending.sequence,
    label: `Request ${turn.requestCount}`,
    detail: identity || "model request",
    status: "ok",
    timing: "unavailable",
    track: 0,
    model: activity.model,
    provider: activity.provider,
    ttftMs: activity.ttft_ms,
    inputTokens: pending.inputTokens,
    outputTokens: pending.outputTokens,
    cacheReadTokens: pending.cacheReadTokens,
    cacheWriteTokens: pending.cacheWriteTokens,
  };
  applyTiming(span, activity.duration_ms, activity.started_at, activity.completed_at);
  return span;
}

function mergeUsage(pending: PendingUsage | null, activity: UsageActivity): PendingUsage {
  return {
    sequence: pending?.sequence ?? activity.sequence,
    inputTokens: (pending?.inputTokens ?? 0) + activity.input_tokens,
    outputTokens: (pending?.outputTokens ?? 0) + activity.output_tokens,
    cacheReadTokens: (pending?.cacheReadTokens ?? 0) + activity.cache_read_tokens,
    cacheWriteTokens: (pending?.cacheWriteTokens ?? 0) + activity.cache_write_tokens,
    activity,
  };
}

function packTracks(spans: TrajectorySpan[]): number {
  const ends: number[] = [];
  const sorted = spans
    .filter((span) => span.startMs !== undefined && span.endMs !== undefined)
    .sort((a, b) => (a.startMs ?? 0) - (b.startMs ?? 0) || a.sequence - b.sequence);
  for (const span of sorted) {
    const start = span.startMs ?? 0;
    let track = ends.findIndex((end) => end <= start);
    if (track < 0) {
      track = ends.length;
      ends.push(span.endMs ?? start);
    } else {
      ends[track] = span.endMs ?? start;
    }
    span.track = track;
  }
  return Math.max(1, ends.length);
}

function unionDuration(spans: TrajectorySpan[]): number {
  const intervals = spans
    .filter((span) => span.startMs !== undefined && span.endMs !== undefined)
    .map((span) => [span.startMs as number, span.endMs as number] as const)
    .sort((a, b) => a[0] - b[0]);
  if (intervals.length === 0) return 0;
  let total = 0;
  let start = intervals[0][0];
  let end = intervals[0][1];
  for (const [nextStart, nextEnd] of intervals.slice(1)) {
    if (nextStart <= end) {
      end = Math.max(end, nextEnd);
    } else {
      total += end - start;
      start = nextStart;
      end = nextEnd;
    }
  }
  return total + end - start;
}

function finalizeTurn(turn: MutableTurn): TrajectoryTurn {
  const candidates = turn.spans.filter((span) => span.kind !== "input");
  const timedSpans = candidates.filter((span) => span.timing === "exact");
  const unanchoredSpans = candidates.filter((span) => span.timing !== "exact");
  const modelTracks = packTracks(timedSpans.filter((span) => span.lane === "model"));
  const toolTracks = packTracks(timedSpans.filter((span) => span.lane === "tools"));
  const measured = candidates.filter((span) => span.durationMs !== undefined).length;
  const exact = timedSpans.length;
  const timing: TurnTimingQuality =
    candidates.length > 0 && exact === candidates.length ? "exact" : exact > 0 ? "partial" : "legacy";

  const inputTime = turn.input ? preciseEventTime(turn.input.ts) : undefined;
  const starts = timedSpans.map((span) => span.startMs as number);
  if (inputTime !== undefined) starts.push(inputTime);
  const ends = timedSpans.map((span) => span.endMs as number);
  const domainStartMs = starts.length > 0 ? Math.min(...starts) : undefined;
  const domainEndMs = ends.length > 0 ? Math.max(...ends) : domainStartMs;
  const activeWallMs = timedSpans.length > 0 ? unionDuration(timedSpans) : undefined;
  const workMs =
    timedSpans.length > 0
      ? timedSpans.reduce((total, span) => total + (span.durationMs ?? 0), 0)
      : undefined;

  return {
    index: turn.index,
    input: turn.input,
    spans: turn.spans,
    timedSpans,
    unanchoredSpans,
    timing,
    measured,
    total: candidates.length,
    domainStartMs,
    domainEndMs,
    elapsedMs:
      domainStartMs !== undefined && domainEndMs !== undefined
        ? Math.max(0, domainEndMs - domainStartMs)
        : undefined,
    activeWallMs,
    workMs,
    overlapMs:
      activeWallMs !== undefined && workMs !== undefined ? Math.max(0, workMs - activeWallMs) : undefined,
    modelTracks,
    toolTracks,
  };
}

export function buildTrajectory(activity: ThreadActivity[]): Trajectory {
  const turns: MutableTurn[] = [];
  let current: MutableTurn | null = null;
  let pendingUsage: PendingUsage | null = null;
  let pendingTools = new Map<string, TrajectorySpan[]>();

  const ensureTurn = (): MutableTurn => {
    if (!current) {
      current = { index: turns.length + 1, input: null, spans: [], requestCount: 0 };
      turns.push(current);
    }
    return current;
  };

  const flushUsage = () => {
    if (!pendingUsage) return;
    const turn = ensureTurn();
    turn.spans.push(modelSpan(turn, pendingUsage));
    pendingUsage = null;
  };

  for (const item of activity) {
    if (item.type === "message" && item.role === "user") {
      flushUsage();
      current = {
        index: turns.length + 1,
        input: item,
        spans: [
          {
            key: `input:${item.sequence}`,
            kind: "input",
            lane: "input",
            turn: turns.length + 1,
            sequence: item.sequence,
            label: "Input",
            detail: compact(item.text, 96),
            status: "ok",
            timing: preciseEventTime(item.ts) === undefined ? "unavailable" : "exact",
            startedAt: item.ts,
            startMs: preciseEventTime(item.ts),
            endMs: preciseEventTime(item.ts),
            durationMs: 0,
            track: 0,
            input: item.text,
          },
        ],
        requestCount: 0,
      };
      turns.push(current);
      pendingTools = new Map();
      continue;
    }

    if (item.type === "usage") {
      pendingUsage = mergeUsage(pendingUsage, item);
      if (
        item.duration_ms !== undefined ||
        (item.started_at !== undefined && item.completed_at !== undefined)
      ) {
        flushUsage();
      }
      continue;
    }

    if (item.type === "tool_use") {
      flushUsage();
      const turn = ensureTurn();
      const span = toolSpan(turn, item);
      turn.spans.push(span);
      const queue = pendingTools.get(item.id) ?? [];
      queue.push(span);
      pendingTools.set(item.id, queue);
      continue;
    }

    if (item.type === "tool_result") {
      const turn = ensureTurn();
      const queue = pendingTools.get(item.tool_use_id);
      const span = queue?.shift();
      if (queue?.length === 0) pendingTools.delete(item.tool_use_id);
      if (span) {
        applyToolResult(span, item);
      } else {
        const orphan: TrajectorySpan = {
          key: `tool-result:${turn.index}:${item.sequence}`,
          kind: "tool",
          lane: "tools",
          turn: turn.index,
          sequence: item.sequence,
          label: "tool result",
          detail: item.tool_use_id,
          status: item.ok ? "ok" : "failed",
          timing: "unavailable",
          track: 0,
          output: item.output,
        };
        applyToolResult(orphan, item);
        turn.spans.push(orphan);
      }
      continue;
    }

    if (item.type === "tool_running") {
      flushUsage();
      const turn = ensureTurn();
      const queue = pendingTools.get(item.id);
      const existing = queue?.[queue.length - 1];
      if (existing) {
        const live = runningToolSpan(turn, item);
        Object.assign(existing, live, { key: existing.key, sequence: existing.sequence });
      } else {
        turn.spans.push(runningToolSpan(turn, item));
      }
    }
  }
  flushUsage();

  const finalized = turns.map(finalizeTurn);
  const spans = finalized.flatMap((turn) => turn.spans.filter((span) => span.kind !== "input"));
  const bottlenecks = spans
    .filter((span) => span.durationMs !== undefined)
    .sort((a, b) => (b.durationMs ?? 0) - (a.durationMs ?? 0) || a.sequence - b.sequence);
  const exactSpans = spans.filter((span) => span.timing === "exact");
  const activeWallMs = finalized.some((turn) => turn.activeWallMs !== undefined)
    ? finalized.reduce((total, turn) => total + (turn.activeWallMs ?? 0), 0)
    : undefined;
  const workMs = finalized.some((turn) => turn.workMs !== undefined)
    ? finalized.reduce((total, turn) => total + (turn.workMs ?? 0), 0)
    : undefined;

  return {
    turns: finalized,
    spans,
    bottlenecks,
    slowest: bottlenecks[0] ?? null,
    measured: bottlenecks.length,
    total: spans.length,
    exact: exactSpans.length,
    activeWallMs,
    workMs,
    overlapMs:
      activeWallMs !== undefined && workMs !== undefined ? Math.max(0, workMs - activeWallMs) : undefined,
  };
}

export function formatTrajectoryDuration(ms: number | undefined): string {
  if (ms === undefined) return "—";
  if (ms < 1_000) return `${Math.round(ms)}ms`;
  if (ms < 10_000) return `${(ms / 1_000).toFixed(1)}s`;
  if (ms < 60_000) return `${Math.round(ms / 1_000)}s`;
  const minutes = Math.floor(ms / 60_000);
  const seconds = Math.round((ms % 60_000) / 1_000);
  return seconds ? `${minutes}m ${seconds}s` : `${minutes}m`;
}