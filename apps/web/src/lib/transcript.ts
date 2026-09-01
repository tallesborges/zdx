/**
 * Transcript projection: flat activity stream → a collapsible tree.
 *
 * Three levels:
 *   1. `work`  — everything the agent did between two assistant messages,
 *                hidden behind a single "Worked for 5m" divider.
 *   2. `group` — a run of related calls folded into one line
 *                ("Explored 8 files, 3 searches", "Ran 7 commands, 1 failed").
 *   3. `tool`  — one call, expanding to full arguments and output.
 *
 * Summaries are derived from the real tool payload shapes: `edit` carries
 * `old_string`/`new_string` so line deltas are computed here rather than being
 * reported by the backend.
 */
import type {
  ThreadActivity,
  ToolResultActivity,
  ToolRunningActivity,
  ToolUseActivity,
} from "./types";

export interface ToolNode {
  kind: "tool";
  key: string;
  sequence: number;
  name: string;
  input: unknown;
  result?: ToolResultActivity;
  running?: ToolRunningActivity;
  orphaned: boolean;
  summary: ToolSummary;
}

export interface ToolSummary {
  /** Leading verb or `$` for shell. */
  verb: string;
  /** Primary subject — file name, command, pattern. */
  subject: string;
  /** Render the subject in monospace (commands, paths, patterns). */
  mono: boolean;
  additions?: number;
  deletions?: number;
  failed: boolean;
}

export interface GroupNode {
  kind: "group";
  key: string;
  label: string;
  failed: boolean;
  items: ToolNode[];
}

export interface ReasoningNode {
  kind: "reasoning";
  key: string;
  text: string;
  redacted: boolean;
}

export interface UsageNode {
  kind: "usage";
  key: string;
  activity: Extract<ThreadActivity, { type: "usage" }>;
}

export type WorkItem = ToolNode | GroupNode | ReasoningNode | UsageNode;

export interface WorkNode {
  kind: "work";
  key: string;
  durationMs: number;
  running: boolean;
  items: WorkItem[];
}

export interface MessageNode {
  kind: "message";
  key: string;
  activity: Extract<ThreadActivity, { type: "message" }>;
}

export interface BannerNode {
  kind: "banner";
  key: string;
  activity: Extract<ThreadActivity, { type: "notice" | "interrupted" }>;
}

export type Node = WorkNode | MessageNode | BannerNode;

/* --------------------------------- summaries -------------------------------- */

const EXPLORE = new Set([
  "read",
  "glob",
  "grep",
  "web_search",
  "fetch_webpage",
  "memory_search",
  "thread_search",
  "read_thread",
]);

const SEARCHY = new Set([
  "glob",
  "grep",
  "web_search",
  "memory_search",
  "thread_search",
]);

function obj(input: unknown): Record<string, unknown> {
  return input && typeof input === "object" ? (input as Record<string, unknown>) : {};
}

function str(value: unknown): string {
  return typeof value === "string" ? value : "";
}

function basename(path: string): string {
  const clean = path.replace(/\/+$/, "");
  return clean.split("/").pop() || clean;
}

function lineCount(text: string): number {
  if (!text) return 0;
  return text.split("\n").length;
}

function truncate(text: string, max = 80): string {
  const flat = text.replace(/\s+/g, " ").trim();
  return flat.length > max ? `${flat.slice(0, max - 1)}…` : flat;
}

function failedOf(node: { result?: ToolResultActivity }): boolean {
  if (!node.result) return false;
  if (!node.result.ok) return true;
  // bash reports a non-zero exit inside a successful tool call.
  const out = obj(node.result.output);
  const data = obj(out.data);
  return typeof data.exit_code === "number" && data.exit_code !== 0;
}

function summarize(name: string, input: unknown, result?: ToolResultActivity): ToolSummary {
  const args = obj(input);
  const failed = failedOf({ result });
  const base: ToolSummary = { verb: name, subject: "", mono: false, failed };

  switch (name) {
    case "bash": {
      return { ...base, verb: "$", subject: truncate(str(args.command), 72), mono: true };
    }
    case "edit": {
      const before = lineCount(str(args.old_string));
      const after = lineCount(str(args.new_string));
      return {
        ...base,
        verb: "Edited",
        subject: basename(str(args.file_path)),
        mono: true,
        additions: Math.max(0, after - Math.min(before, after)),
        deletions: Math.max(0, before - Math.min(before, after)),
      };
    }
    case "write": {
      const created = obj(obj(result?.output).data).created === true;
      return {
        ...base,
        verb: created ? "Created" : "Wrote",
        subject: basename(str(args.file_path)),
        mono: true,
        additions: lineCount(str(args.content)),
      };
    }
    case "read":
      return { ...base, verb: "Read", subject: basename(str(args.file_path)), mono: true };
    case "glob":
      return { ...base, verb: "Globbed", subject: str(args.pattern), mono: true };
    case "grep":
      return { ...base, verb: "Searched", subject: str(args.pattern), mono: true };
    case "web_search":
      return { ...base, verb: "Searched web", subject: truncate(str(args.objective), 60) };
    case "fetch_webpage":
      return { ...base, verb: "Fetched", subject: truncate(str(args.url), 60), mono: true };
    case "invoke_subagent":
      return { ...base, verb: "Delegated to", subject: str(args.subagent) || "subagent" };
    case "todo_write":
      return { ...base, verb: "Updated todos", subject: "" };
    default:
      return { ...base, subject: truncate(str(args.path ?? args.query ?? args.pattern), 60) };
  }
}

/* ---------------------------------- pairing --------------------------------- */

function pairTools(activity: ThreadActivity[]): Map<string, ToolNode> {
  const byId = new Map<string, ToolNode>();

  for (const item of activity) {
    if (item.type === "tool_use") {
      const use = item as ToolUseActivity;
      byId.set(use.id, {
        kind: "tool",
        key: `tool:${use.id}`,
        sequence: use.sequence,
        name: use.name.toLowerCase(),
        input: use.input,
        orphaned: false,
        summary: summarize(use.name.toLowerCase(), use.input),
      });
    } else if (item.type === "tool_running") {
      const run = item as ToolRunningActivity;
      const existing = byId.get(run.id);
      if (existing) existing.running = run;
      else
        byId.set(run.id, {
          kind: "tool",
          key: `tool:${run.id}`,
          sequence: run.sequence,
          name: run.name,
          input: run.input,
          running: run,
          orphaned: false,
          summary: summarize(run.name, run.input),
        });
    } else if (item.type === "tool_result") {
      const res = item as ToolResultActivity;
      const target = byId.get(res.tool_use_id);
      if (target) {
        target.result = res;
        target.running = undefined;
        target.summary = summarize(target.name, target.input, res);
      } else {
        byId.set(`orphan:${res.tool_use_id}:${res.sequence}`, {
          kind: "tool",
          key: `orphan:${res.tool_use_id}:${res.sequence}`,
          sequence: res.sequence,
          name: "tool result",
          input: null,
          result: res,
          orphaned: true,
          summary: { verb: "Tool result", subject: "", mono: false, failed: !res.ok },
        });
      }
    }
  }

  return byId;
}

/* --------------------------------- grouping --------------------------------- */

function foldRuns(items: WorkItem[]): WorkItem[] {
  const out: WorkItem[] = [];
  let i = 0;

  while (i < items.length) {
    const item = items[i];

    if (item.kind !== "tool") {
      out.push(item);
      i++;
      continue;
    }

    // Fold a consecutive run of read-only exploration into one line.
    if (EXPLORE.has(item.name)) {
      let j = i;
      const run: ToolNode[] = [];
      while (j < items.length) {
        const next = items[j];
        if (next.kind !== "tool" || !EXPLORE.has(next.name)) break;
        run.push(next);
        j++;
      }
      if (run.length > 1) {
        const files = run.filter((r) => !SEARCHY.has(r.name)).length;
        const searches = run.filter((r) => SEARCHY.has(r.name)).length;
        const parts: string[] = [];
        if (files) parts.push(`${files} file${files === 1 ? "" : "s"}`);
        if (searches) parts.push(`${searches} search${searches === 1 ? "" : "es"}`);
        out.push({
          kind: "group",
          key: `group:explore:${run[0].key}`,
          label: `Explored ${parts.join(", ")}`,
          failed: run.some((r) => r.summary.failed),
          items: run,
        });
        i = j;
        continue;
      }
    }

    // Fold a consecutive run of shell commands.
    if (item.name === "bash") {
      let j = i;
      const run: ToolNode[] = [];
      while (j < items.length) {
        const next = items[j];
        if (next.kind !== "tool" || next.name !== "bash") break;
        run.push(next);
        j++;
      }
      if (run.length > 1) {
        const failedCount = run.filter((r) => r.summary.failed).length;
        out.push({
          kind: "group",
          key: `group:bash:${run[0].key}`,
          label: `Ran ${run.length} commands`,
          failed: failedCount > 0,
          items: run,
        });
        i = j;
        continue;
      }
    }

    out.push(item);
    i++;
  }

  return out;
}

/** Wall-clock estimate for a turn: model time plus tool time. */
function workDuration(items: WorkItem[]): number {
  let total = 0;
  for (const item of items) {
    if (item.kind === "usage") total += item.activity.duration_ms ?? 0;
    else if (item.kind === "tool") total += item.result?.duration_ms ?? 0;
    else if (item.kind === "group")
      for (const t of item.items) total += t.result?.duration_ms ?? 0;
  }
  return total;
}

export function buildNodes(activity: ThreadActivity[]): Node[] {
  const tools = pairTools(activity);
  const nodes: Node[] = [];
  let pending: WorkItem[] = [];

  const flush = () => {
    if (pending.length === 0) return;
    const items = foldRuns(pending);
    nodes.push({
      kind: "work",
      key: `work:${pending[0].key}`,
      durationMs: workDuration(items),
      running: pending.some((p) => p.kind === "tool" && !!p.running),
      items,
    });
    pending = [];
  };

  for (const item of activity) {
    switch (item.type) {
      case "message":
        flush();
        nodes.push({ kind: "message", key: `msg:${item.sequence}`, activity: item });
        break;

      case "notice":
      case "interrupted":
        flush();
        nodes.push({ kind: "banner", key: `${item.type}:${item.sequence}`, activity: item });
        break;

      case "reasoning":
        pending.push({
          kind: "reasoning",
          key: `reasoning:${item.sequence}`,
          text: item.text,
          redacted: item.redacted,
        });
        break;

      case "usage":
        pending.push({ kind: "usage", key: `usage:${item.sequence}`, activity: item });
        break;

      case "tool_use":
      case "tool_running": {
        const node = tools.get(item.id);
        // A tool can appear as both tool_use and tool_running; emit it once.
        if (node && !pending.includes(node)) pending.push(node);
        break;
      }

      case "tool_result": {
        const orphan = tools.get(`orphan:${item.tool_use_id}:${item.sequence}`);
        if (orphan) pending.push(orphan);
        break;
      }
    }
  }

  flush();
  return nodes;
}

export function formatDuration(ms: number): string {
  const secs = Math.round(ms / 1000);
  // Sub-second turns get no duration at all rather than a meaningless "0s".
  if (secs < 1) return "";
  if (secs < 60) return `${secs}s`;
  const mins = Math.floor(secs / 60);
  const rest = secs % 60;
  if (mins < 60) return rest ? `${mins}m ${rest}s` : `${mins}m`;
  const hours = Math.floor(mins / 60);
  return `${hours}h ${mins % 60}m`;
}
