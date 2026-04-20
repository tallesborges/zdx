# Queue Prompts Implementation Plan

**Status:** WIP 🚧  
**Scope:** Allow queueing prompts while a turn is running and show a compact queue summary panel in the TUI.

---

## Goals

1. Enter during a running turn enqueues the prompt instead of sending immediately.
2. When a turn completes, the next queued prompt auto-sends (one at a time).
3. A compact queue panel appears between transcript and input, showing up to 3 items.
4. Interrupting a running turn only marks the *current* prompt as interrupted (not queued ones).
5. Queue is in-memory only and cleared when switching threads or starting a new thread.

---

## Non-Goals / Deferred

- Persisting queued prompts to thread log or disk.
- Queueing slash commands or bash commands while streaming.
- Configurable queue length or summary length.
- Queue management UI (clear, reorder, edit) beyond automatic clearing on thread changes.

---

## User Journey

```
1. User submits prompt "a"; agent starts streaming.
2. While streaming, user presses Enter with "b" in input.
   -> "b" is enqueued and input clears.
3. Queue panel appears with "b" listed (truncated to panel width with ellipsis).
4. Agent finishes "a". "b" auto-sends and starts streaming.
5. If user interrupts "a" mid-tool, only the "a" cell shows "interrupted".
```

---

## Architecture Notes (Elm/MVU)

- All state lives in `AppState` feature slices.
- Reducer (`update`) remains pure; no direct reads of global interrupt state.
- Queue lives in `InputState` and is mutated via `InputMutation` or input slice methods.

---

## Slices

### Slice 0: Input State + Queue Storage

**Goal:** Add queue storage and utilities without UI changes.

**Checklist:**
- [x] Add `queued: VecDeque<String>` to `InputState`.
- [x] Add helpers: `enqueue_prompt`, `pop_queued_prompt`, `has_queued`, `queued_summaries`.
- [x] Add `InputMutation::ClearQueue` and implement in `InputState::apply`.
- [x] Clear queue on thread switch/create/fork and `/new`.

---

### Slice 1: Enqueue on Submit While Streaming

**Goal:** Hitting Enter while streaming enqueues normal prompts.

**Checklist:**
- [x] Update input submit handler to enqueue if agent running.
- [x] Disallow queueing slash/bang commands while streaming (show system message).
- [x] Clear input after enqueue.
- [x] Keep reducer pure (no I/O or global flags).

---

### Slice 2: Auto-Dequeue on Turn Completion

**Goal:** Pop one queued prompt when a turn ends.

**Checklist:**
- [x] On `AgentEvent::TurnCompleted|Interrupted|Error`, auto-send one queued prompt.
- [x] Use a shared send helper so enqueue/dequeue path matches normal send.
- [x] Avoid double-dequeue when TurnCompleted and Interrupted arrive in the same frame.

---

### Slice 3: Queue Summary Panel (Between Transcript and Input)

**Goal:** Visible summary of queued prompts.

**Checklist:**
- [x] Add a layout segment between transcript and input.
- [x] Show a bordered panel with title `Queued (N)`.
- [x] Render up to 3 items, truncated to fit panel width with ellipsis.
- [x] Hide panel when queue empty.

---

### Slice 5: Unicode-Aware Truncation (Polish)

**Goal:** Use `truncate_with_ellipsis` for proper unicode width handling.

**Rationale:** The original implementation used `.chars().count()/.take()` which counts 
characters, not display width. Wide characters (CJK, emoji) take 2 terminal columns 
but count as 1 char, causing layout overflow. Using `common/text::truncate_with_ellipsis` 
fixes this and adds a proper ellipsis indicator.

**Checklist:**
- [x] Update `InputState::queued_summaries()` in `zdx-tui/src/features/input/state.rs`:
  - Remove `max_chars` parameter (no longer needed - truncation happens at render time)
  - Return first line of each queued prompt without truncation
- [x] Update `render_queue_panel()` in `zdx-tui/src/render.rs`:
  - Import `crate::common::text::truncate_with_ellipsis`
  - Use `truncate_with_ellipsis(line, inner_width)` instead of `.chars().take()`
  - Remove `QUEUE_MAX_CHARS` constant (width is now dynamic based on panel size)
- [x] Update call sites in `render.rs` that pass `QUEUE_MAX_CHARS` to `queued_summaries()`
- [x] Add test for truncation with wide characters (emoji, CJK)

---

### Slice 4: Correct Interrupt Marking

**Goal:** Interrupt marks the active prompt, not queued prompts.

**Checklist:**
- [x] Track `pending_user_cell_id` and `active_user_cell_id` in transcript state.
- [x] Set pending when user cell is appended; activate on `AgentSpawned`.
- [x] On interrupt, mark active user cell instead of last user cell.
- [x] Clear active/pending on reset/replace.

---

## Tests (Light)

- [x] Update tests to confirm dequeue on TurnCompleted.
- [ ] Add regression test: interrupt during tool + queued prompts → only active user cell marked interrupted.

---

## Failure Modes / Edge Cases

- TurnCompleted + Interrupted back-to-back: ensure dequeue happens once and interrupt marks the correct cell.
- Queue inserted right before interrupt: should not be marked interrupted.
- Thread switch or `/new` while queue exists: queue must clear.

---

## Files

- `zdx-tui/src/features/input/state.rs` (queue storage + helpers)
- `zdx-tui/src/features/input/update.rs` (enqueue logic)
- `zdx-tui/src/update.rs` (dequeue on turn end)
- `zdx-tui/src/render.rs` (queue panel)
- `zdx-tui/src/features/transcript/state.rs` (active user cell tracking)
- `zdx-tui/src/common/text.rs` (truncate_with_ellipsis utility)
- `docs/SPEC.md` (user-visible contract)

