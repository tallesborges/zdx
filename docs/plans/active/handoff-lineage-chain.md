# Goals
- On a chain of handoffs (A → B → C → D), the new thread's first message exposes the **full ancestor lineage**, not just the immediate parent — so context from older threads isn't silently lost.
- The new assistant can see every ancestor thread ID (with title) and lazily `read_thread` any of them on demand.

# Non-goals
- Do NOT read/summarize ancestor transcripts during handoff generation (keep generation cheap; lazy `read_thread` fan-out only).
- Do NOT add depth caps, cycle detection, or orphan-annotation guardrails — chains are short in practice (YAGNI).
- No changes to how `handoff_from` is stored (already persisted per thread).
- No TUI thread-tree/rendering changes.

# Design principles
- User journey drives order.
- Reuse existing plumbing: `handoff_from` in meta and `ThreadSummary { id, title, handoff_from }` already exist.
- Cheap over complete: expose the chain, let the agent pull.

# User journey
1. User does repeated `/handoff`s across a work session, building a chain of threads.
2. On the Nth handoff, the new thread's first message shows the whole lineage back to the root.
3. When the new assistant needs older context, it calls `read_thread` on the right ancestor ID directly — no guessing that ancestors exist.

# Foundations / Already shipped (✅)

## Handoff generation + prefix
- What exists: `crates/zdx-tui/src/runtime/handoff.rs` — `handoff_generation()` loads the current thread transcript and runs a cheap subagent; `build_handoff_prefix(thread_id, next_message)` produces the "(Continuing from thread {id} …)" line that leads the new thread's first message.
- ✅ Demo: `just run`, `/handoff`, confirm the new thread opens with the "Continuing from thread …" prefix.
- Gaps: prefix names only the immediate source thread.

## Per-thread lineage data
- What exists: every thread's meta stores `handoff_from` (immediate parent). `ThreadSummary` carries `id`, `title`, `handoff_from`. `thread_persistence::list_all_threads()` reads meta-only for all threads in one pass (`crates/zdx-engine/src/core/thread_persistence.rs`).
- ✅ Demo: the TUI thread tree already renders parent→child nesting via `handoff_from` (`crates/zdx-tui/src/features/thread/tree.rs`).
- Gaps: nothing consumes the chain at handoff time.

## read_thread tool
- What exists: `crates/zdx-engine/src/tools/read_thread.rs` loads one thread and answers a goal.
- Gaps: it does not surface the read thread's own `handoff_from`, so old handoffs (pre-this-feature) can't be walked.

# MVP slices (ship-shaped, demoable)

## Slice 1: Full lineage in the handoff prefix
- **Goal**: The "Continuing from …" line lists the entire ancestor chain with titles, so the new assistant knows every prior thread and can `read_thread` any of them.
- **Scope checklist**:
  - [ ] In `handoff.rs`, walk `handoff_from` from the source thread up to the root. Build the lookup once from `thread_persistence::list_all_threads()` (id → `{ title, handoff_from }`); stop when a thread has no `handoff_from` or its parent isn't found.
  - [ ] Collect `(id, display_title)` per ancestor (use `ThreadSummary::display_title()`).
  - [ ] Update `build_handoff_prefix` to render the chain when there is more than the source thread, e.g.:
        `Continuing from D "title". Lineage: D "title" ← C "title" ← B "title" ← A "title". Call read_thread on any thread ID above for missing context.`
        Single-thread (no ancestors) keeps today's shorter wording.
  - [ ] User's literal next message still leads the first message, unchanged.
- **✅ Demo**: `just run` → create threads via a chain of 3 `/handoff`s → the final new thread's first message shows all ancestor IDs + titles in the lineage line; `read_thread` on the oldest ID returns its context.
- **Risks / failure modes**:
  - Titles missing → `display_title()` already falls back to a short ID.
  - Deleted ancestor mid-chain → walk stops at the missing link (no annotation needed).

## Slice 2 (fast-follow): read_thread surfaces its parent
- **Goal**: Even handoffs created before Slice 1 can be walked — reading a thread reveals its own parent so the agent can climb the chain lazily.
- **Scope checklist**:
  - [ ] In `read_thread.rs`, after loading the thread, include the thread's own `handoff_from` (if any) as a small metadata line prepended/appended outside the extractor answer, e.g. `Parent handoff thread: <id>`.
  - [ ] Pull `handoff_from` from meta (reuse `list_all_threads()` lookup or a single meta read).
- **✅ Demo**: `read_thread` on a mid-chain thread returns its answer plus a `Parent handoff thread: <id>` line; agent can call `read_thread` on that parent next.
- **Risks / failure modes**:
  - Thread with no parent → omit the line entirely.

# Contracts (guardrails)
- User's literal next message must remain the first thing in the new thread's first message (existing test `handoff_prefix_leads_with_next_message_verbatim`).
- Handoff generation stays cheap: no ancestor transcript reads during generation.
- Single-thread handoff (no ancestors) behavior is unchanged.

# Key decisions (decide early)
- **IDs + titles, not summaries** in the lineage line (titles make ancestors actionable; summaries cost model calls — skip).
- **No guardrails** (depth cap / cycle set / orphan annotations) — chains are short; add only if a real long/broken chain shows up.

# Testing
- Manual smoke demos per slice (above).
- Extend `build_handoff_prefix` unit tests in `handoff.rs`: multi-ancestor lineage renders all IDs+titles in order; zero-ancestor case keeps current wording; next-message-verbatim invariant preserved.

# Polish phases (after MVP)

## Phase 1: Wording/format tuning
- Adjust lineage line phrasing if the agent under/over-uses `read_thread`.
- ✅ Check-in demo: dogfood a few real handoff chains; confirm the agent reaches for the right ancestor.

# Later / Deferred
- Depth cap + truncation marker — revisit only if a chain grows long enough to bloat the first message.
- Cycle/orphan handling — revisit only if a broken chain actually appears.
- Pulling a little ancestor transcript context into generation — revisit only if IDs+titles prove insufficient in practice.
