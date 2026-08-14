> Stage: done. Completed 2026-08-14. The plan file is the source of truth, not memory.

# Goals
- Persist exact client-observed execution duration for completed tool calls in saved thread JSONL.
- Let a user inspect one saved thread and see where recorded model requests and tools spent time.
- Keep metric labels honest: request duration is not model-internal thinking time, TTFT means first streamed content, and summed parallel tool durations are work rather than wall time.

# Non-goals
- Exact reasoning, final-text, queue, turn-wall, or parallel tool-batch wall time.
- Cross-thread latency aggregation in `zdx stats` or Monitor, percentiles, tokens/second, charts, or slowest-tool rankings.
- Backfilling exact tool durations for old threads.
- A new metrics database, thread-index schema, `turn_metrics` event, or standalone profiling subsystem.
- JSON output or changing the transcript-oriented `zdx threads show` output.

# User journey
1. The user runs a normal turn containing one or more tools, including parallel tools.
2. ZDX saves exact duration with each completed tool result while preserving canonical request order.
3. The user runs `zdx threads inspect <THREAD_ID>`.
4. ZDX shows recorded request and tool timings grouped into transcript-derived user turns, and marks old or incomplete measurements unavailable.
5. The user can open the same timing report from the Monitor's Threads tab without leaving the dashboard.

# Phase 1 — Saved threads retain exact tool durations ✅ 2026-08-14
- [x] Add optional `duration_ms` to `ThreadEvent::ToolResult` with serde defaults and omission when absent; keep the existing schema version so old JSONL remains readable (`crates/zdx-engine/src/core/thread_persistence/event.rs`).
- [x] Measure elapsed execution centrally for every real tool path, including `todo_write`, using a monotonic clock and a saturating `u64` millisecond conversion; leave malformed, rejected-before-execution, and synthetic/cancelled results unmeasured unless an honest execution boundary exists (`crates/zdx-engine/src/core/agent.rs`, `crates/zdx-engine/src/tools/mod.rs`).
- [x] Carry the optional duration on `AgentEvent::ToolCompleted`, cache it by tool-use ID in the persistence path, and attach it when checkpoint snapshots emit canonical `ToolResult` events (`crates/zdx-types/src/events.rs`, `crates/zdx-engine/src/core/thread_persistence/persist.rs`). Do not persist completion events directly: parallel tools finish in completion order while checkpoints restore request order.
- [x] Update the persistence constructors, replay conversion, matches, and fixtures affected by the additive field without changing provider-facing tool-result payloads.
- [x] Add regression coverage for old JSONL without the field, reverse completion order, original persisted result order, `todo_write`, and synthetic results remaining unmeasured.
- [x] Update the thread format contract in `docs/SPEC.md` and any scoped file indexes required by changed files.
✅ **Demo**: Run a turn with two deterministic parallel tools that finish in reverse order; the saved JSONL keeps request order, gives each real result its own exact `duration_ms`, and still loads a legacy thread with no duration field.

# Phase 2 — Inspect one thread's recorded timings ✅ 2026-08-14
- [x] Add `zdx threads inspect <THREAD_ID>` as a focused read-only command; keep CLI glue thin and derive the report from canonical `load_thread_events` data rather than `threads.sqlite` (`crates/zdx-cli/src/cli/mod.rs`, `crates/zdx-cli/src/cli/commands/threads.rs`).
- [x] Group events by transcript-derived user turns and pair tool uses/results within their tool batch so repeated or synthetic IDs cannot be matched across the whole thread.
- [x] Show each successful model request's **client-observed request duration** and **TTFT to first streamed content**, plus each measured tool's **client-observed tool duration**.
- [x] Show only honest aggregates: **recorded successful request time** and **tool work (sum)**. If any matched completed tool lacks duration, render tool work as unavailable with the measured count instead of presenting a partial total. Never label these values as turn time, wall time, thinking time, or tool-batch time.
- [x] Render old, interrupted, unmatched, and otherwise incomplete measurements as unavailable rather than zero; include one concise compatibility note when timing fields are missing.
- [x] Add CLI integration coverage for help, multiple model requests in one user turn, parallel tools, missing duration fields, unmatched tools, and unknown thread IDs (`crates/zdx-cli/tests/integration/`).
✅ **Demo**: Inspect a newly recorded parallel-tool thread and see per-request and per-tool durations with `tool work (sum)` clearly distinguished from wall time; inspect a legacy fixture and see unavailable measurements without an error or invented estimate.

# Phase 3 — Inspect thread timings in Monitor ✅ 2026-08-14
- [x] Keep timing reduction and metric labels in a shared UI-agnostic engine API used by both the CLI and Monitor; move Phase 2's reducer into shared code now that a second consumer exists rather than duplicating grouping or arithmetic (`crates/zdx-engine/src/core/`).
- [x] In the Monitor's Threads tab, let `i` open a timing overlay for the selected saved thread while `Enter` continues to open the transcript (`crates/zdx-monitor/src/app.rs`).
- [x] Render the same transcript-derived turns, request durations, TTFT values, tool durations, completeness states, and honest aggregate labels as `zdx threads inspect`; keep only layout and key handling in Monitor (`crates/zdx-monitor/src/ui.rs`).
- [x] Preserve the selected thread and active Threads-tab filters when opening and closing the timing overlay; show a clear empty/unavailable state for legacy threads rather than falling back to timestamp estimates.
- [x] Add reducer parity tests and focused Monitor state/render tests for opening, closing, legacy data, multiple requests, and parallel tools; verify the overlay remains usable at the Monitor's minimum terminal size.
- [x] Update `crates/zdx-monitor/AGENTS.md` when the new overlay changes its architecture map.
✅ **Demo**: Select the same new and legacy fixtures in Monitor, press `i`, and see timing values and unavailable states that match `zdx threads inspect`; close the overlay and return to the same selected, filtered Threads view.

# Later
- Add exact parallel tool-batch wall time when a real user question requires separating concurrency savings from summed tool work.
- Add first-reasoning, first-text, and stream-window milestones when provider stream semantics can support consistent client-observed labels.
- Add `--json` when the text report schema has stabilized and external benchmarking needs machine-readable output.
- Add model/provider percentiles, throughput, and slowest-tool cross-thread aggregation to `zdx stats` and Monitor when per-thread inspection proves useful across enough recorded data.
- Add live timing details to Monitor's Active Agents tab when checkpoint visibility can support useful updates without implying exact in-progress thinking time.
- Add turn-level metrics only when they can be measured directly rather than inferred from checkpoint timestamps.

# Open questions
- None for the first usable version. `inspect` is timing-focused without a redundant `--timings` flag, and all new persisted fields are optional.