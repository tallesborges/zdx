import { describe, expect, test } from "bun:test";

import { buildTrajectory } from "./src/lib/trajectory";
import type { ThreadActivity } from "./src/lib/types";

describe("buildTrajectory", () => {
  test("preserves exact parallel overlap and packs tool tracks", () => {
    const activity: ThreadActivity[] = [
      {
        type: "message",
        sequence: 0,
        time: "10:00 AM",
        ts: "2026-09-20T10:00:00.000Z",
        role: "user",
        speaker: "You",
        text: "inspect two files",
      },
      {
        type: "usage",
        sequence: 1,
        time: "10:00 AM",
        ts: "2026-09-20T10:00:01.100Z",
        input_tokens: 10,
        output_tokens: 2,
        cache_read_tokens: 0,
        cache_write_tokens: 0,
        duration_ms: 1_000,
        ttft_ms: 100,
        started_at: "2026-09-20T10:00:00.100Z",
        completed_at: "2026-09-20T10:00:01.100Z",
      },
      {
        type: "tool_use",
        sequence: 2,
        time: "10:00 AM",
        ts: "2026-09-20T10:00:01.100Z",
        id: "a",
        name: "read",
        input: { file_path: "a.txt" },
      },
      {
        type: "tool_use",
        sequence: 3,
        time: "10:00 AM",
        ts: "2026-09-20T10:00:01.100Z",
        id: "b",
        name: "read",
        input: { file_path: "b.txt" },
      },
      {
        type: "tool_result",
        sequence: 4,
        time: "10:00 AM",
        ts: "2026-09-20T10:00:03.100Z",
        tool_use_id: "a",
        ok: true,
        duration_ms: 2_000,
        started_at: "2026-09-20T10:00:01.100Z",
        completed_at: "2026-09-20T10:00:03.100Z",
        output: "a",
      },
      {
        type: "tool_result",
        sequence: 5,
        time: "10:00 AM",
        ts: "2026-09-20T10:00:02.100Z",
        tool_use_id: "b",
        ok: true,
        duration_ms: 1_000,
        started_at: "2026-09-20T10:00:01.100Z",
        completed_at: "2026-09-20T10:00:02.100Z",
        output: "b",
      },
      {
        type: "usage",
        sequence: 6,
        time: "10:00 AM",
        ts: "2026-09-20T10:00:03.600Z",
        input_tokens: 12,
        output_tokens: 3,
        cache_read_tokens: 0,
        cache_write_tokens: 0,
        duration_ms: 500,
        ttft_ms: 80,
        started_at: "2026-09-20T10:00:03.100Z",
        completed_at: "2026-09-20T10:00:03.600Z",
      },
    ];

    const trajectory = buildTrajectory(activity);
    const turn = trajectory.turns[0];

    expect(turn.timing).toBe("exact");
    expect(turn.toolTracks).toBe(2);
    expect(turn.activeWallMs).toBe(3_500);
    expect(turn.workMs).toBe(4_500);
    expect(turn.overlapMs).toBe(1_000);
    expect(trajectory.slowest?.label).toBe("read");
  });

  test("ranks recorded legacy durations without inventing overlap", () => {
    const activity: ThreadActivity[] = [
      {
        type: "message",
        sequence: 0,
        time: "10:00 AM",
        ts: "2025-01-01T10:00:00Z",
        role: "user",
        speaker: "You",
        text: "build",
      },
      {
        type: "tool_use",
        sequence: 1,
        time: "10:00 AM",
        ts: "2025-01-01T10:00:01Z",
        id: "a",
        name: "bash",
        input: { command: "run checks" },
      },
      {
        type: "tool_result",
        sequence: 2,
        time: "10:00 AM",
        ts: "2025-01-01T10:00:09Z",
        tool_use_id: "a",
        ok: true,
        duration_ms: 8_000,
        output: "ok",
      },
    ];

    const trajectory = buildTrajectory(activity);

    expect(trajectory.turns[0].timing).toBe("legacy");
    expect(trajectory.turns[0].activeWallMs).toBeUndefined();
    expect(trajectory.slowest?.durationMs).toBe(8_000);
    expect(trajectory.slowest?.timing).toBe("duration");
  });

  test("keeps incomplete spans unavailable", () => {
    const activity: ThreadActivity[] = [
      {
        type: "message",
        sequence: 0,
        time: "10:00 AM",
        ts: "2025-01-01T10:00:00Z",
        role: "user",
        speaker: "You",
        text: "inspect",
      },
      {
        type: "tool_use",
        sequence: 1,
        time: "10:00 AM",
        ts: "2025-01-01T10:00:01Z",
        id: "a",
        name: "read",
        input: { file_path: "a.txt" },
      },
    ];

    const trajectory = buildTrajectory(activity);

    expect(trajectory.measured).toBe(0);
    expect(trajectory.total).toBe(1);
    expect(trajectory.turns[0].unanchoredSpans[0].timing).toBe("unavailable");
  });
});