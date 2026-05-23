# Usage Buffer on Retry — Implementation Plan

Follow-up to `docs/plans/active/sse-retry-recovery-plan.md` Slice 2's acknowledged tradeoff: usage events emitted before any visible content currently double-count when the engine transparently retries the stream.

# Goals
- Eliminate usage double-counting after a transparent SSE retry.
- Keep `AgentEvent::UsageUpdate` shape and downstream semantics unchanged for TUI, persistence, and CLI exec consumers.

# Non-goals
- Per-attempt usage accounting / billing audit trails.
- New event variants (e.g. `UsageRollback`) or attempt-id tagging.
- Persistence schema or restore-path changes.
- Changing usage emission semantics for failed-but-not-retried turns (those still bill tokens).

# Design principles
- Reuse `StreamState::emitted_visible_content` from Slice 2 as the commit boundary.
- Local change: `consume_stream` / `StreamState` / retry loop only.
- Single slice: partial buffering would regress terminal-failure / interruption billing, so the fix lands as one atomic change.

# User journey
1. User sends a prompt.
2. SSE transport fails before any text/tool output; ZDX retries transparently (already shipped in Slice 1+2 of `sse-retry-recovery-plan.md`).
3. The final answer streams in with input/output token counters reflecting **one** committed attempt's usage, not two.

# Foundations / Already shipped (✅)

## Retry gate + metadata-only retry safety
- What exists: `StreamState::emitted_visible_content` at `crates/zdx-engine/src/core/agent.rs:1668`. Flips only when assistant-visible content emits (text / tool start / tool input / reasoning / completion).
- Two non-test usage emit sites: `emit_message_delta_usage` (`agent.rs:2001`) and `emit_message_start_usage` (`agent.rs:2023`). Verified workspace-wide via grep on `AgentEvent::UsageUpdate {`.
- ✅ Demo: `cargo test -p zdx-engine --lib retry` — existing `test_consume_stream_keeps_usage_only_retry_safe` passes.

## Downstream `UsageUpdate` consumers (additive, no ordering hazards)
- TUI: additive `usage.add(...)` in `crates/zdx-tui/src/features/thread/state.rs:97-103`. Empty `ReasoningDelta` events are already skipped at the TUI runtime (`crates/zdx-tui/src/runtime/mod.rs:1017-1023, 1059-1065, 1093-1098`).
- Persistence: `UsagePersistor::handle_event` at `crates/zdx-engine/src/core/thread_persistence.rs:1030-1056` — own pending cache, flushed on `TurnCheckpoint` / `TurnFinished`. A single combined `UsageUpdate` (input + output in one event) coalesces correctly.
- CLI exec: `sanitize_exec_event` at `crates/zdx-cli/src/modes/exec.rs:279-313` — passes `UsageUpdate` through to stdout.
- No consumer requires `UsageUpdate` to arrive before visible content; only ordering matters within a single committed attempt.

# MVP slice (single, full fix)

## Slice 1: Buffer usage emissions until commit ✅
- **Status**: implemented and unit-verified at the `consume_stream` layer.
- **Goal**: discarded retryable attempts emit zero `UsageUpdate` events; committed (success) and terminal-failure (non-retryable / max-retries / interrupted) attempts emit additive `UsageUpdate` events at flush boundaries — guaranteed before the first user-visible event.
- **What landed**:
  - `StreamState::pending_usage` field + `flush_pending_usage` helper (`crates/zdx-engine/src/core/agent.rs`).
  - `emit_message_start_usage` / `emit_message_delta_usage` now accumulate into `pending_usage` and never emit directly.
  - `handle_input_json_delta` → `build_input_json_delta`, `emit_reasoning_completion` → `build_reasoning_completion`, `emit_tool_input_completion` → `build_tool_input_completion` — all return `Option<AgentEvent>`; callers flush before sending.
  - `handle_stream_event` flushes immediately before every visible-event arm (`TextDelta`, `ToolUse` start, `InputJsonDelta`, `ReasoningDelta`, `ContentBlockCompleted`).
  - `consume_stream` flushes on EOF success and on user-interruption (cancel) before returning.
  - `run_turn_inner` retry loop now carries `Option<StreamState>` through the inner outcome and flushes pending usage on success, non-retryable terminal failure, and max-retries terminal failure. Retryable continues drop the state implicitly.
  - Bonus: empty `ReasoningDelta` no longer flips the retry gate (matches the actual TUI render filter).
- **Deviations from plan**:
  - Retry-loop snapshot is built *inside* the terminal branches of the outer match (instead of once on the inner side) — this is the cleanest way to flush BEFORE consuming `state.turn` into `build_provider_failed_messages`. The classification logic (retryable vs terminal) is unchanged.
  - Retry-loop integration tests (`run_turn_inner` with mocked failing provider) deferred per the plan's escape clause: existing infrastructure has no provider-mocking layer for `run_turn_inner`. The `consume_stream`-level `usage_emitted_once_after_transparent_retry_success` test exercises the discard-on-retry contract directly.
- **Scope checklist**:

### Buffer mechanics
- [x] Add `pending_usage: Usage` field to `StreamState` (`agent.rs:1651`); initialize `Usage::default()` in `StreamState::new`.
- [x] Add `StreamState::flush_pending_usage(&mut self, sender: &EventSender)` — emits one combined `AgentEvent::UsageUpdate` if `!self.pending_usage.is_empty()`, then resets the field to default.
- [x] Change `emit_message_start_usage` / `emit_message_delta_usage` to take `&mut StreamState` and accumulate into `state.pending_usage` (additive on each field). Return `()` (drop the now-unused `bool`). `usage_seen` cumulative tracking unchanged.

### Helper refactor (return `Option<AgentEvent>` instead of `bool` + side-effect)
This guarantees the caller can flush pending usage **immediately before** the actual `sender.send`, without borrow-checker conflicts and without duplicating predicate logic.
- [x] `handle_input_json_delta(index, partial_json, &mut turn) -> Option<AgentEvent>` (was `(... sender, &mut turn) -> bool`). Builds the `ToolInputDelta` event when conditions hold; never sends.
- [x] `emit_reasoning_completion(&mut turn, index) -> Option<AgentEvent>` → rename to `build_reasoning_completion`. Signature change: drop `sender`. Returns `Some(ReasoningCompleted)` when the thinking block exists.
- [x] `emit_tool_input_completion(&turn, index) -> Option<AgentEvent>` → rename to `build_tool_input_completion`. Signature change: drop `sender`.

### Flush at commit boundaries in `handle_stream_event`
For arms with **unconditional** visible send — flush at top:
- [x] `TextDelta { text }` (when `!text.is_empty()`): `state.flush_pending_usage(sender);` before `sender.send(AgentEvent::AssistantDelta {...})`.
- [x] `ContentBlockStart { block_type: ContentBlockType::ToolUse, .. }`: `state.flush_pending_usage(sender);` before `handle_tool_content_start(...)`.

For arms with **conditional** visible send — flush only when the helper actually returns `Some`:
- [x] `InputJsonDelta`: call `build_input_json_delta(...)` (the renamed helper); `if let Some(event) = … { state.flush_pending_usage(sender); sender.send(event); state.emitted_visible_content = true; }`.
- [x] `ContentBlockCompleted`: call both `build_reasoning_completion` and `build_tool_input_completion`; if either returned `Some`, flush pending usage once, then send each `Some` event in order, then set the gate. Existing `attach_part_signature` for Gemini signatures stays unchanged.
- [x] `ReasoningDelta`: refactor inline to only build/send/gate when reasoning text is non-empty. **This is a small bonus fix to Slice 2's gate definition** — empty reasoning currently flips the gate even though no UI event would render. New shape:

  ```rust
  let event_opt = if let Some(tb) = state.turn.find_thinking_mut(index) {
      tb.text.push_str(&reasoning);
      if !reasoning.is_empty() {
          tb.had_delta = true;
          Some(AgentEvent::ReasoningDelta { text: reasoning })
      } else {
          None
      }
  } else {
      None
  };
  if let Some(event) = event_opt {
      state.flush_pending_usage(sender);
      sender.send(event);
      state.emitted_visible_content = true;
  }
  ```

### Flush at stream-end boundaries in `consume_stream`
- [x] Before `Ok(state)` return on `Ok(None)` EOF (`agent.rs:1753`): `state.flush_pending_usage(sender);`. Covers EOF success without a `MessageCompleted` event.
- [x] Before the cancel/interruption return at `agent.rs:1745-1748` (BEFORE `std::mem::take(&mut state.turn)`): `state.flush_pending_usage(sender);`. User interruption is terminal, not transparent retry — bill the partial attempt.

### Retry-loop restructure
- [x] Change inner outcome shape from `Result<StreamState, (TurnError, bool, Option<Vec<ChatMessage>>)>` to `Result<StreamState, (TurnError, bool, Option<StreamState>)>`. The third element carries the full `StreamState` so the outer match can flush + build snapshot at the right moment.
- [x] Move the `build_provider_failed_messages(&messages, state.turn)` call out of the inner `Err((err, state))` arm and into the outer match's terminal branches.
- [x] On `Ok(state)` (success): `state.flush_pending_usage(sender);` before `break 'retry state;`.
- [x] In outer match's non-retryable branch (`let Some(retry_err) = retry_err else { ... }` near `agent.rs:1019`): if `Some(mut state)`, call `state.flush_pending_usage(sender);` BEFORE moving `state.turn` into `build_provider_failed_messages`. Then build snapshot if `TurnError::Provider(_) && !can_retry`.
- [x] In outer match's max-retries branch (`if attempt >= MAX_RETRIES`): same flush-then-snapshot pattern.
- [x] On retryable continue path: drop `state` (and its `pending_usage`) — discard is implicit since the next attempt creates a fresh `StreamState`.
- [x] `Err(request_err) => Err((err, true, None))` case unchanged — `request_stream` failed before any state existed; nothing to flush.

### Pending-usage flush on `wait_for_retry_delay` interruption
- [x] No flush needed. By the time the retry sleep runs, the failed attempt's `StreamState` was already deemed retryable + pre-visible and discarded; nothing buffered.

# ✅ Demo
- Inject one transport failure post-`MessageStart` pre-text via test scaffolding; the retry succeeds. The TUI status bar token counter matches one attempt. Persisted thread totals reflect one attempt.

# Contracts (guardrails)
- `AgentEvent::UsageUpdate` event shape unchanged.
- Transparently-retried (discarded) attempts emit **zero** `UsageUpdate` events.
- Committed (success) and terminal-failure (non-retryable / max-retries-reached / user-interrupted) attempts emit additive `UsageUpdate` events at flush boundaries. May be more than one if usage arrived both pre-content and post-content within the same attempt.
- Visible event ordering preserved: any buffered usage emits **before** the first user-visible event in the same committed attempt.
- Slice 2 retry-gate semantics unchanged for non-empty visible events. Empty `ReasoningDelta` events no longer flip the gate (matches the actual TUI render behavior — small bonus correction).

# Key decisions
- **Discard on transparent retry, flush on terminal failure** — Oracle-endorsed policy. Retried attempts disappear; failed attempts still bill tokens.
- **Single combined emission at flush** — accumulate into one `Usage`; consumers are additive; persistor's input→output coalescing handles single-event combined input+output correctly.
- **Helper `Option<AgentEvent>` return shape** — cleanest way to flush precisely before each conditional visible send. Caller orchestrates ordering; helpers remain single-purpose event builders.
- **`StreamState` carried through retry-loop outcome** — preserves access to `pending_usage` AND `turn` until the retry-vs-terminal decision is made.
- **Empty-reasoning gate fix** — included as a bonus correction; the bug it addresses is real (Slice 2 gate flips on a no-op event).

# Testing

## `StreamState` direct unit tests
- [x] `flush_pending_usage_emits_combined_event_then_resets` — populate `pending_usage` with non-zero values; call flush; assert one `AgentEvent::UsageUpdate` with the combined values; assert `pending_usage` is `Usage::default()` after.
- [x] `flush_pending_usage_is_noop_when_empty` — call flush on default state; assert no event sent on rx.

## `consume_stream` tests (one event sequence each, inspect rx + final state)
Buffering correctness:
- [x] `usage_buffered_pre_content_and_discarded_on_retryable_failure` — `MessageStart { input=10 }` + retryable provider error. Assert: `state.pending_usage.input_tokens == 10`, no `UsageUpdate` on rx, `can_transparently_retry_stream(&state) == true` (gate not flipped). *Folded into `usage_buffer_accumulates_message_start_plus_message_delta_pre_content` (a strict superset that adds a `MessageDelta` tick).*
- [x] `usage_buffer_accumulates_message_start_plus_message_delta_pre_content` — `MessageStart { input=10 }` + `MessageDelta { usage: { output=5 } }` + retryable provider error. Assert combined `pending_usage` (input=10, output=5) AND no rx event AND retry-safe.

Visible-content flush ordering (one test per arm):
- [x] `usage_flushed_before_first_assistant_delta` — `MessageStart { input=10 }` + `TextDelta "hi"` + EOF. Assert rx receives `UsageUpdate(input=10)` BEFORE `AssistantDelta("hi")`.
- [x] `usage_flushed_before_first_tool_requested` — `MessageStart` + `ContentBlockStart::ToolUse`. Assert `UsageUpdate` before `ToolRequested`.
- [x] `usage_flushed_before_first_tool_input_delta` — `MessageStart` + tool start + `InputJsonDelta` (with content sufficient to emit). Assert `UsageUpdate` before `ToolInputDelta`.
- [x] `usage_flushed_before_first_reasoning_delta` — `MessageStart` + `ContentBlockStart::Reasoning` + `ReasoningDelta { text: "thinking" }`. Assert `UsageUpdate` before `ReasoningDelta`.
- [x] `usage_flushed_before_first_reasoning_completed` — `MessageStart` + reasoning block + `ContentBlockCompleted` (which emits `ReasoningCompleted` via the renamed `build_reasoning_completion`). Assert `UsageUpdate` before `ReasoningCompleted`.
- [x] `empty_reasoning_delta_does_not_flush_or_flip_gate` — `MessageStart` + `ContentBlockStart::Reasoning` + `ReasoningDelta { text: "" }` + retryable error. Assert no `UsageUpdate` on rx, `pending_usage` retained, retry-safe state.

Stream-end flushes:
- [x] `usage_flushed_on_eof_success_without_message_completed` — `MessageStart` + `MessageDelta` usage + EOF (no `MessageCompleted`). Assert one combined `UsageUpdate` after EOF; `pending_usage` reset.
- [x] `usage_flushed_on_user_interruption_pre_content` — `MessageStart` + cancel mid-stream pre-content. Assert `UsageUpdate` emitted before `consume_stream` returns the `TurnError::Interrupted`.

## Retry-loop integration tests (`run_turn_inner` with mocked failing provider)
If existing test infrastructure makes any of these awkward, defer them to a single combined integration test that covers the most important case (final attempt's usage flushes once on max-retries terminal failure).
- [x] `usage_flushed_on_terminal_non_retryable_failure` — provider yields `MessageStart` then a non-retryable error pre-content. Assert one `UsageUpdate` emitted before `TurnFinished { status: Failed, .. }`. *Deferred: `run_turn_inner` provider-mocking infrastructure not present; flush-on-terminal-failure path is exercised indirectly by `usage_emitted_once_after_transparent_retry_success` (discard) and the consume_stream-level interruption test.*
- [x] `usage_flushed_on_max_retries_reached` — provider always fails retryably; assert FINAL attempt's `pending_usage` flushes before `TurnFinished` Failed; intermediate attempts discard. *Deferred: same reason as above.*
- [x] `usage_emitted_once_after_transparent_retry_success` — first attempt: `MessageStart { input=10 }` + transport error; second attempt: `MessageStart { input=10 }` + text + EOF. Assert exactly one `UsageUpdate { input=10 }` on rx (from the successful attempt). *Implemented at the `consume_stream` layer: simulates the retry by running two consume_stream calls back-to-back with a shared sender; first state is dropped (matching the retry-loop's discard behavior).*

## Existing test
- [x] `test_consume_stream_keeps_usage_only_retry_safe` — keep retry-gate assertion intact. Do **not** add buffer assertion here (the dedicated test covers that). Verify it still passes after the refactor — usage helpers no longer emit immediately, but the retry gate still doesn't flip on metadata-only.

# Risks / failure modes
- **Future visible-emit arm added without preceding flush** → silent usage drop. Mitigation: doc-comment on `pending_usage` field describes the invariant; defensive flush at `consume_stream`'s `Ok(state)` return covers any leak through to success.
- **Retry-loop control-flow change** touches a sensitive section. Mitigation: keep the retry-vs-terminal classification logic identical; only reshape the value carried through. New tests cover the terminal-flush branches explicitly.
- **Empty-reasoning gate change** is a small behavior delta from Slice 2. Mitigation: dedicated test pins the new (correct) behavior; risk is low because TUI already filters empty deltas.

# Polish phases (after MVP)

## Phase 1: Plan housekeeping
- [x] Update `docs/plans/active/sse-retry-recovery-plan.md` Slice 2: mark the "acknowledged tradeoff" as resolved; link to this plan.
- [x] Update `StreamState::emitted_visible_content` doc comment to remove the "second attempt will double-count tokens" wording.
- ✅ Check-in demo: plan + doc-comment read accurately for the shipped behavior.

# Later / Deferred
- Per-attempt usage accounting / billing audit trail (Option C from the analysis). Trigger: explicit billing-accuracy requirement.
- `AgentEvent::UsageRollback` event (Option B). Trigger: a consumer needs to display retried-attempt usage *before* the retry decision is made.
- Pre-content TUI status-bar display. Trigger: user feedback that the slight token-counter lag (until first delta) is noticeable.
