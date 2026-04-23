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
- **Goal**: fix the smallest, highest-value failure mode first.
- **Scope checklist**:
  - [ ] Reclassify only transport-level SSE poll/read failures in:
    - `crates/zdx-providers/src/openai/responses_sse.rs:482-488`
    - `crates/zdx-providers/src/anthropic/sse.rs:44-49`
    - `crates/zdx-providers/src/openai/chat_completions.rs:912-918`
  - [ ] Keep generic parse errors non-retryable in `crates/zdx-types/src/providers.rs:127-130`
  - [ ] Add targeted tests proving the existing retry loop picks these up through `crates/zdx-engine/src/core/agent.rs:734-800`
- **✅ Demo**: inject an SSE transport error before any visible delta; transcript shows retry notice and the turn succeeds without manual intervention.
- **Risks / failure modes**:
  - Over-broad classification could retry real parser/protocol bugs.
  - Missing `openai/chat_completions` would leave several providers uncovered.

## Slice 2: Make metadata-only emissions retry-safe
- **Goal**: allow transparent retry after metadata-only events, while still blocking retry after visible output/tool execution.
- **Scope checklist**:
  - [ ] Split “visible emission” from “metadata emission” in `crates/zdx-engine/src/core/agent.rs:1467-1572`
  - [ ] Keep assistant text, reasoning text, tool start/input/completion as retry-blocking
  - [ ] Stop treating usage-only updates as retry-blocking
  - [ ] Extend retry-gate tests near `crates/zdx-engine/src/core/agent.rs:3098-3193`
- **✅ Demo**: provider emits usage first, then transport fails; ZDX retries automatically with no duplicated visible output.
- **Risks / failure modes**:
  - Usage is still stateful, so this must be an explicit policy.
  - Loosening the gate too much could cause duplicate transcript content.

## Slice 3: Preserve safe provider-failure recovery state
- **Goal**: preserve correct committed state after mid-stream provider failure.
- **Scope checklist**:
  - [ ] Define provider-failure recovery separately from interruption
  - [ ] Do not reuse interruption synthesis in `crates/zdx-engine/src/core/agent.rs:1877-1922`
  - [ ] Use `emit_turn_error_with_messages` only when the preserved snapshot is semantically safe
  - [ ] Keep TUI failed-turn persistence path working via `crates/zdx-tui/src/features/transcript/update.rs:140-149`
- **✅ Demo**: partial visible output + provider failure ends cleanly, no auto-retry occurs, and later manual continue/retry starts from preserved state.
- **Risks / failure modes**:
  - Incorrectly preserving unfinished tool state could poison the next turn.
  - Reusing interruption semantics would create false “Interrupted by user” state.

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