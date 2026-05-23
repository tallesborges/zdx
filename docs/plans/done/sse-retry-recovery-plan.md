# Goals
- Auto-retry transient SSE transport failures that happen before visible output.
- Reuse existing retry/backoff UX instead of adding new controls first.
- Preserve safe manual recovery for mid-stream failures without duplicating content.

# Non-goals
- True mid-stream token resume.
- Making all parse errors retryable.
- New retry settings/config in MVP.
- Solving every EOF/truncation case in Slice 1.

# Design principles
- Start with the smallest safe fix.
- Only retry when no user-visible content/tool execution has started.
- Keep provider-failure recovery separate from interruption recovery.

# User journey
1. User sends a prompt.
2. If the stream transport fails before visible output, ZDX retries automatically.
3. If the stream fails after visible output starts, ZDX stops safely and preserves recovery state.
4. User can manually continue/retry later from preserved thread state.

# Foundations / Already shipped (✅)
List capabilities that already exist and should not be rebuilt.

## Retry loop + retry UX
- What exists: bounded retry loop already exists in `crates/zdx-engine/src/core/agent.rs:734-800`.
- What exists: retry notices already render in `crates/zdx-tui/src/features/transcript/update.rs:64-77`.
- ✅ Demo: any already-classified retryable provider failure shows `⟳ Provider error, retrying...`.
- Gaps: SSE transport poll/read failures are currently wrapped as parse errors in:
  - `crates/zdx-providers/src/openai/responses_sse.rs:482-488`
  - `crates/zdx-providers/src/anthropic/sse.rs:44-49`
  - `crates/zdx-providers/src/openai/chat_completions.rs:912-918`

## Transparent retry gate
- What exists: retry is blocked once `emitted_events` becomes true in `crates/zdx-engine/src/core/agent.rs:1467-1572`.
- ✅ Demo: existing tests around transparent retry already exist near `crates/zdx-engine/src/core/agent.rs:3098-3193`.
- Gaps: usage-only metadata currently also blocks retry.

## Failed-turn persistence hook
- What exists: failed turns can preserve committed messages via `crates/zdx-engine/src/core/agent.rs:571-581`, and TUI already stores them in `crates/zdx-tui/src/features/transcript/update.rs:140-149`.
- ✅ Demo: a failed turn with non-empty `messages` still updates thread state.
- Gaps: many provider failures still emit empty `messages` via `crates/zdx-engine/src/core/agent.rs:535-565`.

# MVP slices (ship-shaped, demoable)

## Slice 1: Reclassify surfaced SSE transport failures as retryable
- **Status**: ✅ Implemented (2026-04-28)
- **Goal**: fix the smallest, highest-value failure mode first.
- **Scope checklist**:
  - [x] Reclassify only transport-level SSE poll/read failures in:
    - `crates/zdx-providers/src/openai/responses_sse.rs` (`Poll::Ready(Some(Err(e)))` arm)
    - `crates/zdx-providers/src/anthropic/sse.rs` (`Poll::Ready(Some(Err(e)))` arm)
    - `crates/zdx-providers/src/openai/chat_completions.rs` (`Poll::Ready(Some(Err(e)))` arm)
  - [x] Keep generic parse errors non-retryable in `crates/zdx-types/src/providers.rs:127-130` (unchanged)
  - [x] Add targeted parser tests for transport (retryable) AND UTF-8 (non-retryable) in all three SSE paths
  - [x] Add engine retry-loop test that pins the contract end-to-end
- **Implementation notes**:
  - Introduced shared helper `crate::shared::map_event_stream_error` in `crates/zdx-providers/src/shared.rs` that explicitly matches on `eventsource_stream::EventStreamError` variants:
    - `Transport(_)` → `ProviderErrorKind::Timeout` with `"SSE stream network error: {e}"` (retryable via `RETRYABLE_PATTERNS`)
    - `Utf8(_)` and `Parser(_)` → `ProviderErrorKind::Parse` (stay non-retryable)
  - All four SSE parsers (anthropic, openai/responses_sse, openai/chat_completions, gemini) now route `EventStreamError` through this helper.
  - Added engine test `test_consume_stream_treats_sse_transport_error_as_retryable` plus shared/parser-level tests for both retryable and non-retryable variants.
- **Deviations from original plan**:
  - Plan listed only 3 SSE paths, but `crates/zdx-providers/src/gemini/sse.rs` had the same buggy mapping (always wrapping in `Timeout` + "network error" regardless of variant). Rolled the same fix into Gemini for consistency rather than leaving a known bug behind.
  - First implementation was caught by Oracle review for over-broad classification (Utf8/Parser variants would have been wrongly retried). Fixed by explicitly matching on `EventStreamError` variants instead of stringly typing.
- **✅ Demo**: inject an SSE transport error before any visible delta; transcript shows retry notice and the turn succeeds without manual intervention.
- **Risks / failure modes**:
  - Over-broad classification could retry real parser/protocol bugs. *Mitigated* by explicit variant matching.
  - Missing `openai/chat_completions` would leave several providers uncovered. *Done.*

## Slice 2: Make metadata-only emissions retry-safe
- **Status**: ✅ Implemented (2026-04-28)
- **Goal**: allow transparent retry after metadata-only events, while still blocking retry after visible output/tool execution.
- **Scope checklist**:
  - [x] Split "visible emission" from "metadata emission" in `crates/zdx-engine/src/core/agent.rs` `handle_stream_event`
  - [x] Keep assistant text, reasoning text, tool start/input/completion as retry-blocking
  - [x] Stop treating usage-only updates as retry-blocking
  - [x] Extend retry-gate tests around `crates/zdx-engine/src/core/agent.rs:3431-3604`
- **Implementation notes**:
  - Renamed `StreamState::emitted_events` → `emitted_visible_content` to make the retry-gating intent explicit (only "visible to the user / persisted in the transcript" content blocks retry).
  - Removed `emitted_visible_content = true` from the two metadata-only paths: `MessageStart { usage }` and `MessageDelta { usage }`. The usage emission still happens (the UI/persistence still see the `UsageUpdate` event); only the gate flag stops being set.
  - Kept the gate set for: `TextDelta`, `ContentBlockStart` (ToolUse), `InputJsonDelta` (when something was actually emitted), `ReasoningDelta`, and `ContentBlockCompleted` paths that emit reasoning completion or tool input completion.
  - Tests updated:
    - `test_can_transparently_retry_stream_requires_no_emitted_events` now toggles the renamed flag.
    - Replaced `test_consume_stream_marks_usage_update_as_retry_unsafe` with `test_consume_stream_keeps_usage_only_retry_safe` (asserts the flipped contract).
    - Added `test_consume_stream_marks_text_delta_as_retry_unsafe` to pin the complementary "visible text blocks retry" contract.
    - Existing `test_consume_stream_marks_reasoning_completion_as_retry_unsafe` still passes unchanged.
- **Known tradeoff (resolved by follow-up)**: usage emissions were originally accumulated additively in **both** the TUI and persistence:
  - TUI: `crates/zdx-tui/src/features/thread/state.rs:97-103` calls `usage.add(...)` per `UsageUpdate`.
  - Persistence: `crates/zdx-engine/src/core/thread_persistence.rs:1030-1055` writes a `ThreadEvent::Usage` row per `UsageUpdate` and the restore path (`thread_persistence.rs:2100-2124`) sums all usage rows.

  A transparent retry after a `MessageStart`/`MessageDelta` usage tick would therefore double-count tokens for that turn in both the live status bar AND the persisted thread totals. **Resolved** by `docs/plans/active/usage-buffer-on-retry-plan.md`: usage is now buffered into `StreamState::pending_usage` and only flushed at commit boundaries (immediately before the first user-visible event of an attempt, on EOF success, on user interruption, or on terminal failure in the retry loop). Transparently-retried attempts discard their buffer; double-counting is eliminated.
- **✅ Demo**: provider emits usage first, then transport fails; ZDX retries automatically with no duplicated visible output. Token counters reflect exactly one committed attempt.
- **Risks / failure modes**:
  - Usage state was the original tradeoff. *Resolved by usage-buffer-on-retry plan.*
  - Loosening the gate too much could cause duplicate transcript content. *Mitigated*: only `MessageStart`/`MessageDelta` usage paths were loosened; all assistant text / reasoning / tool paths still set the gate.

## Slice 3: Preserve safe provider-failure recovery state
- **Status**: ✅ Implemented (2026-04-28)
- **Goal**: preserve correct committed state after mid-stream provider failure.
- **Scope checklist**:
  - [x] Define provider-failure recovery separately from interruption
  - [x] Do not reuse interruption synthesis in the new helper
  - [x] Use `emit_turn_error_with_messages` only when the preserved snapshot is semantically safe
  - [x] Keep TUI failed-turn persistence path working via `crates/zdx-tui/src/features/transcript/update.rs:140-149`
- **Implementation notes**:
  - Added `build_provider_failed_messages(prior_messages, turn) -> Vec<ChatMessage>` next to `build_interrupted_messages` in `crates/zdx-engine/src/core/agent.rs`. It finalizes the in-flight assistant turn but **drops** all `ChatContentBlock::ToolUse` blocks (so the assistant↔tool_result pairing stays balanced) and never synthesizes "Interrupted by user" tool results. Only `Text` and `Reasoning` blocks survive.
  - Rewired the retry loop's stream-error arm to thread `(err, can_retry, snapshot: Option<Vec<ChatMessage>>)` instead of `(err, can_retry)`. The snapshot is built **only** for `TurnError::Provider` failures where `can_transparently_retry_stream` returned false (i.e. visible content was emitted). Interruption errors and request-build errors carry `None` (existing behavior preserved).
  - When the retry loop exits with a non-retryable / exhausted-retry provider error, the snapshot replaces the bare `messages.clone()` so `run_turn_with_cancel` routes through `emit_turn_error_with_messages` with the preserved partial turn. If the snapshot is empty (no visible content emitted), the existing empty-messages path keeps using `emit_turn_error`.
  - The interrupted path inside `consume_stream` is unchanged: it still returns a `TurnError::Interrupted` whose messages are built by `build_interrupted_messages`. Slice 3 deliberately leaves that synthesis intact for true user interruptions.
  - Tests added:
    - `provider_failed_snapshot_preserves_text_and_reasoning_only` — pins the contract that pending `ToolUse` blocks are dropped and no synthetic `tool_result` message is appended.
    - `provider_failed_snapshot_drops_completed_tool_use_without_result` — pins that even a fully-formed (parseable) `ToolUse` is dropped if the provider died before tool execution, so the next provider request stays balanced.
    - `provider_failed_snapshot_is_empty_when_no_visible_blocks` — pins that pre-output failures still produce an empty snapshot (so the empty-messages branch in `run_turn_with_cancel` keeps firing).
- **✅ Demo**: partial visible output + provider failure ends cleanly, no auto-retry occurs, and later manual continue/retry starts from preserved state (text + reasoning, no orphaned tool calls).
- **Risks / failure modes**:
  - Incorrectly preserving unfinished tool state could poison the next turn. *Mitigated*: pending `ToolUse` blocks are explicitly filtered out.
  - Reusing interruption semantics would create false "Interrupted by user" state. *Mitigated*: `build_provider_failed_messages` is a separate helper that never synthesizes tool results; the retry loop only invokes it for `TurnError::Provider` failures, never for `TurnError::Interrupted`.

# Contracts (guardrails)
- Do not make generic parse errors retryable: `crates/zdx-types/src/providers.rs:127-130`
- Do not auto-retry after visible content/tool execution starts: `crates/zdx-engine/src/core/agent.rs:1467-1572`
- Reuse existing retry UX first: `crates/zdx-tui/src/features/transcript/update.rs:64-77`
- Do not reuse interruption recovery for provider failures: `crates/zdx-engine/src/core/agent.rs:1877-1922`

# Key decisions (decide early)
- Slice 1 uses an existing retryable error kind at the parser boundary, not a new error taxonomy.
- Slice 1 explicitly covers only surfaced transport errors, not all EOF/truncation cases.
- Slice 2 is framed as “metadata-only emissions are retry-safe,” not “usage is invisible.”
- Slice 3 must model provider-failure recovery as its own case.

# Testing
- Manual smoke demos per slice
- Minimal regression tests only for contracts
- Parser tests for all 3 parser paths in Slice 1
- Engine tests for retry gating in Slice 2
- Failed-turn state preservation tests in Slice 3

# Polish phases (after MVP)

## Phase 1: EOF / truncation detection
- Improve early-EOF/truncated-stream handling, since `consume_stream` currently treats `None` as success in `crates/zdx-engine/src/core/agent.rs:1454-1458`
- Normalize parser behavior for premature stream end
- ✅ Check-in demo: truncated stream before visible output becomes a recoverable transport-style failure

## Phase 2: Explicit retry/resume affordance
- Add a user-facing retry/resume action on failed turns
- Build on preserved failed-turn state already handled in `crates/zdx-tui/src/features/transcript/update.rs:140-149`
- ✅ Check-in demo: after failure, user triggers explicit retry/resume without retyping

# Later / Deferred
- True token-level stream resume
- User-configurable retry knobs
- New `ProviderErrorKind::Transport`
- Broad retry of all parse/decode failures