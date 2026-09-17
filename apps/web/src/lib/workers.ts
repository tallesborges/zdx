/**
 * Worker roll-up formatting, shared by the thread list, the Agent summary and
 * the Workers pane so the three never drift.
 *
 * `failed` is always its own bucket. Folding it into "settled" is how a failed
 * worker gets missed, which is the one case the reader has to act on.
 */
import type { WorkerItem, WorkerRollup } from "./types";

export function emptyRollup(): WorkerRollup {
  return { total: 0, running: 0, queued: 0, failed: 0, settled: 0, unknown: 0 };
}

/** Rolls a fetched worker list into the same shape the list endpoint returns. */
export function rollupFrom(workers: WorkerItem[]): WorkerRollup {
  const acc = emptyRollup();
  acc.total = workers.length;
  for (const worker of workers) {
    if (!worker.live) acc.unknown += 1;
    else if (worker.status === "running") acc.running += 1;
    else if (worker.status === "queued") acc.queued += 1;
    else if (worker.status === "failed") acc.failed += 1;
    else acc.settled += 1;
  }
  return acc;
}

/**
 * One line of counts, omitting empty buckets so a quiet orchestrator reads as
 * "3 settled" rather than "0 running · 0 queued · 0 failed · 3 settled".
 *
 * A roll-up with a total but no live state at all (everything predates a
 * restart) says so instead of printing "N unknown", which reads like a fault.
 */
export function formatRollup(rollup: WorkerRollup): string {
  if (rollup.total === 0) return "no workers";
  if (rollup.unknown === rollup.total) {
    return `${rollup.total} worker${rollup.total === 1 ? "" : "s"} · status unavailable`;
  }

  const parts: string[] = [];
  if (rollup.running > 0) parts.push(`${rollup.running} running`);
  if (rollup.queued > 0) parts.push(`${rollup.queued} queued`);
  if (rollup.failed > 0) parts.push(`${rollup.failed} failed`);
  if (rollup.settled > 0) parts.push(`${rollup.settled} settled`);
  if (rollup.unknown > 0) parts.push(`${rollup.unknown} unknown`);
  return parts.join(" · ");
}

/** True when something is actively working, which drives the faster poll. */
export function isBusy(rollup: WorkerRollup): boolean {
  return rollup.running > 0 || rollup.queued > 0;
}

/** True when the tab deserves a dot: work in flight, or a failure to look at. */
export function needsAttention(rollup: WorkerRollup): boolean {
  return rollup.running > 0 || rollup.failed > 0;
}
