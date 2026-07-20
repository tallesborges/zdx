> Stage: drafts. Keep this plan current while working: when a scope item is finished, check its box `[ ]`→`[x]`; when a phase's ✅ demo passes, mark the phase done (with date). The plan file is the source of truth, not memory.

# Mid-stream retry for retryable provider stream errors

## Context

Today the engine only transparently retries a retryable provider error when **nothing visible has streamed yet** (`can_transparently_retry_stream` = `!emitted_visible_content`, `agent.rs:1509-1511`). Retryable errors that arrive mid-stream (e.g. OpenAI Responses `response.failed` with `server_error`, which `is_retryable()` returns true for) are surfaced as terminal failures requiring a manual `continue`.

Goal: auto-retry retryable **mid-stream** errors by replaying the whole provider request, while rolling back the discarded attempt's UI so the live TUI stays consistent with the canonical `messages` snapshot and the persisted JSONL.

Grounding facts (from exploration):
- Retry loop + `consume_stream` are the single convergence point for all providers (`agent.rs:868-1005`, `1453-1507`). Change is centralized, not per-provider.
- Tools execute and `TurnCheckpoint` fires only **after** `consume_stream` succeeds (`agent.rs:1009-1018`, `1885-1938`) — a discarded mid-stream attempt can never execute tools or persist a checkpoint.
- Assistant content persists only from `TurnCheckpoint`/`TurnFinished` snapshots (`persist.rs:74-129`); raw deltas are UI-only. So mid-stream partial text is **not** on disk before retry.
- Provider replay tokens/signatures serialize only from committed `messages` history (OpenAI `responses.rs:211-223,285-299`; Anthropic `types.rs:354-374`; Gemini `shared.rs:260-323`), never from the in-flight attempt. Dropping the failed `StreamState.turn` is the key safety property.
- OpenAI Responses WS clears `previous_response_id`/`last_input` on failed/incomplete turns (`responses_ws.rs:330-343`), so replay rebuilds full input — safe.

# Goals
- Retryable provider stream errors that occur mid-stream auto-retry within the existing budget, instead of surfacing for manual continue.
- On each discarded attempt, the TUI removes only that attempt's transcript cells (assistant/reasoning/tool) and returns to a waiting state.
- Behavior is identical across all providers (Anthropic, OpenAI Responses SSE + WS, OpenAI chat_completions, Gemini).
- Live TUI, canonical `messages`, and reloaded JSONL agree after a retry.

# Non-goals
- SSE resume/cursor continuation of a failed response (we replay the whole request, like Codex).
- Changing which error codes are retryable (`is_retryable()` classification stays as-is).
- Streaming assistant deltas into Telegram (bot already uses `TurnFinished.final_text`).
- Retrying non-provider (tool/bash) failures.

# Design principles
- User journey drives order: land the engine-side auto-retry first (already benefits Telegram + reduces manual `continue`), then the TUI rollback polish.
- Centralize in the engine; never add per-provider retry logic.
- Discard the failed attempt entirely — never merge its text/reasoning/tool IDs/replay tokens into the next attempt.
- Prefer the smallest explicit change; no compatibility shims.

# User journey
1. User sends a prompt; assistant starts streaming text/reasoning.
2. A transient `server_error` arrives mid-stream.
3. Engine drops the partial attempt, waits (backoff), and replays the request automatically.
4. TUI clears the partial attempt's cells and shows a "retrying" notice; the successful attempt streams cleanly.
5. Final transcript on screen matches the saved thread.

# Foundations / Already shipped (✅)

## Retry loop + provider convergence
- What exists: unified `request_stream`→`consume_stream` loop with `MAX_RETRIES=3`, exponential backoff, `ProviderRetry` event (`agent.rs:868-1005`).
- ✅ Demo: pre-visible-content transport errors already auto-retry (`test_consume_stream_treats_sse_transport_error_as_retryable`).
- Gaps: gated off once `emitted_visible_content` is set.

## Snapshot-based persistence
- What exists: assistant content persisted from `TurnCheckpoint`/`TurnFinished` snapshots, idempotent via `last_persisted_index` (`persist.rs:114-155`).
- ✅ Demo: interrupted-turn tests confirm partial deltas aren't persisted incrementally.
- Gaps: none for this change — mid-stream partials are never persisted before a checkpoint.

## TUI cell removal primitive
- What exists: `remove_cell_by_id` and `reset` (`transcript/state.rs:362-384`); runtime folds/flushes pending deltas before any non-delta event (`runtime/mod.rs:1253-1272`).
- ✅ Demo: empty-assistant cleanup already removes a cell (`update.rs:328-331`).
- Gaps: no "truncate all cells created since a marker" API; `AgentState` retains stale `cell_id`/`pending_delta`.

# MVP phases (ship-shaped, demoable)

## Phase 1: Engine auto-retries mid-stream (no UI rollback yet)
- **Goal**: retryable mid-stream errors replay the whole request automatically.
- **Scope checklist**:
  - [ ] Relax the retry gate: retry `is_retryable()` provider errors regardless of `emitted_visible_content`, within `MAX_RETRIES` (`agent.rs:930-983`, `1509-1511`).
  - [ ] On each retry, drop the failed `StreamState` (incl. `turn` and replay tokens) — already the current behavior; confirm no merge path.
  - [ ] Keep terminal path unchanged: on exhausted retries, `build_provider_failed_messages` retains safe partial text/reasoning for manual continue.
  - [ ] Decide + implement usage handling at discard (see Key decisions): flush residual `pending_usage` so it can't bleed into the next attempt; keep additive attempt-level billing.
- **✅ Demo**: unit test — provider stream emits text delta then a retryable `server_error`; assert a second `request_stream` occurs and the turn completes from attempt 2 (mirror `stream_no_completed`-style). On Telegram, a mid-stream `server_error` now completes without a manual `continue`.
- **Risks / failure modes**:
  - Without UI rollback, the TUI will visibly show attempt A's partial text then attempt B's text (append), diverging from canonical `messages`. Acceptable *only* as an intermediate on non-TUI surfaces; gate Phase 1 dogfood to Telegram/exec, land Phase 2 before TUI users rely on it.
  - Usage semantics change if we start flushing residual pending usage — must be a conscious decision.

## Phase 2: TUI discards the failed attempt's cells
- **Goal**: live TUI matches canonical state after a retry.
- **Scope checklist**:
  - [ ] Add attempt-lifecycle `AgentEvent`s: `StreamAttemptStarted { attempt }` (before each `request_stream`) and `StreamAttemptDiscarded { attempt }` (before `ProviderRetry`, after dropping `StreamState`) — `events.rs:11-164`, emitted in `agent.rs:868-1005`.
  - [ ] Add a transcript "truncate suffix from marker" op: record cell-index at `StreamAttemptStarted`; on `StreamAttemptDiscarded` remove all cells added since, reset `AgentState` to waiting, clear `pending_delta`/`cell_id`, invalidate line cache (`transcript/state.rs`, `transcript/update.rs`).
  - [ ] Ensure ordering: runtime already flushes pending deltas before non-delta events (`runtime/mod.rs:1253-1272`); place the retry notice after discard so it survives.
  - [ ] Persistence: `from_agent` returns `None` for the new variants (default `_ => None`, `event.rs:392-413`) — confirm they're not persisted.
  - [ ] CLI exec: explicitly drop the new variants in `sanitize_exec_event` (`exec.rs:313-333`) and add names in `event_type_name` (`exec.rs:286-309`).
  - [ ] Subagent reader ignores unknown/known-but-unhandled variants (`subagent.rs:303-334`, `_ => {}`) — confirm no choke.
- **✅ Demo**: in TUI, trigger a mid-stream retry (fault-inject a `server_error`); the partial cell disappears, a "retrying" notice shows, and the final transcript equals the reloaded thread. Reasoning + running-tool cells from the failed attempt are also gone.
- **Risks / failure modes**:
  - Truncation must also remove reasoning/tool cells, not just the active assistant cell.
  - Line-info/cache invalidation after suffix removal (scroll/height correctness).

# Contracts (guardrails)
- Retry stays restricted to `ProviderError::is_retryable()`; semantic failures (e.g. Gemini `blocked`, context-length, quota) never auto-retry.
- No per-provider retry logic; all providers flow through `consume_stream`.
- A discarded attempt never contributes text/reasoning/tool IDs/replay tokens to `messages` or the next request.
- Tools execute exactly once, only from the successful attempt (already guaranteed: execution is post-`consume_stream`).
- `last_persisted_index` monotonicity preserved: no partial attempt is ever persisted, so no JSONL rewrite is attempted.
- Telegram behavior unchanged (uses `TurnFinished.final_text`).

# Key decisions (decide early)
- **Usage accounting on discard**: today the retry path drops the failed attempt's `pending_usage` (`agent.rs:980-982`). If a mid-visible attempt already flushed some usage, decide: (a) additive attempt-level billing (recommended by Oracle — provider billed two real requests) + flush residual at discard, or (b) preserve current "drop pending" to minimize scope. Pick before touching Phase 1 usage code.
- **Retry budget**: keep `3`/2s, or align closer to Codex (`5`/200ms)? Independent of correctness; decide for UX.
- **CLI exec exposure**: confirm the new lifecycle events are TUI-only and filtered from exec JSONL.

# Testing
- Manual smoke: fault-inject `server_error` mid-stream on each provider (Anthropic, OpenAI SSE, OpenAI WS, Gemini) → completes via replay.
- Regression tests for contracts:
  - Partial text → retry → UI + JSONL contain only replacement text.
  - Reasoning summary/completion → retry → old reasoning cell + replay token gone.
  - Streamed tool start/input → retry → old running-tool cell gone; tool executes once.
  - Prior completed tool checkpoint → later request retries → checkpoint remains.
  - Usage before/after visible content → distinct per-attempt records, no cross-attempt pending merge.
  - Exhausted retries → latest partial remains via failed-turn recovery.
- Run `just ci-fast` during iteration; `just test` (or `cargo nextest run -p zdx-engine -p zdx-tui`) on behavior change.

# Polish rounds (after MVP)

## Polish round 1: retry UX
- Improve the retry notice copy/placement; optionally show attempt count.
- ✅ Check-in demo: mid-stream retry shows a clear, non-duplicated "retrying (n/N)" notice.

# Later / Deferred
- SSE resume/continuation instead of whole-request replay — revisit only if replay cost becomes a problem on long turns.
- Aligning retry budget/backoff with Codex defaults — revisit if transient errors remain frequent.
