# Plan: Timeline Overlay for Transcript Navigation

**Project/feature:** Add a timeline overlay that lists thread turns (user + assistant) with truncated content, supports arrow-key navigation to jump the transcript to a selected message, and offers fork action from any turn.

**Existing state:**
- Overlays are registered via the `Overlay` enum and handled in `src/modes/tui/overlays/mod.rs` + `src/modes/tui/overlays/update.rs`.
- Overlay rendering utilities live in `src/modes/tui/overlays/render_utils.rs`.
- Command palette and model picker provide examples of list-style overlays with navigation.
- Transcript data (messages and rendered cells) lives in `src/modes/tui/transcript/state.rs` and is built in `src/modes/tui/transcript/build.rs`.
- `ScrollState.cell_line_info` tracks `(CellId, start_line, line_count)` per cell for scroll positioning.

**Problem:** Long sessions make it hard to jump to specific turns. We need a compact timeline that surfaces turns and lets users jump or fork without scrolling the full transcript.

---

## Decisions

| Aspect | Decision |
|--------|----------|
| Turn definition | User + Assistant cells only |
| Fork behavior | Create new session truncated to that turn; if user message, populate input field |
| Opening timeline | Command palette only (`/timeline`) |
| State location | Embedded in `Overlay::Timeline(TimelineState)` |
| Jump behavior | Center the turn in the viewport |
| Preview text | First line, truncated to overlay width |

---

## Goals

- Provide a keyboard-driven timeline overlay listing User/Assistant turns in chronological order with truncated preview text.
- Use arrow keys/PageUp/PageDown/Home/End to move selection; Enter jumps transcript to the selected turn.
- Offer a fork action (`f` key) to create a new session starting from that turn's context.
- Keep the overlay lightweight and consistent with existing overlay patterns.

## Non-goals

- No search/filtering in the first iteration.
- No mouse support (keyboard-only for MVP).
- No handoff generation (fork is simple session truncation, not subagent-based).

---

## Data Model

```rust
/// A single entry in the timeline overlay.
pub struct TimelineEntry {
    /// Index into TranscriptState.cells
    pub cell_index: usize,
    /// Role for display (User or Assistant)
    pub role: TimelineRole,
    /// First line of content, truncated for display
    pub preview: String,
}

pub enum TimelineRole {
    User,
    Assistant,
}

/// Timeline overlay state (embedded in Overlay::Timeline).
pub struct TimelineState {
    /// Timeline entries derived from transcript cells
    pub entries: Vec<TimelineEntry>,
    /// Currently selected entry index
    pub selected: usize,
    /// Scroll offset for the list (when entries exceed visible height)
    pub offset: usize,
}
```

---

## MVP Slices

### Slice 1: Timeline data model + state

**Files:** `src/modes/tui/overlays/timeline.rs` (new)

- Define `TimelineEntry`, `TimelineRole`, `TimelineState` structs.
- Implement `TimelineState::from_cells(cells: &[HistoryCell]) -> Self`:
  - Filter to `HistoryCell::User` and `HistoryCell::Assistant` variants only.
  - Extract first line of content, truncate to ~60 chars with ellipsis.
  - Store `cell_index` for each entry (index into original cells vec).
- Implement `TimelineState::open(cells: &[HistoryCell]) -> (Self, Vec<UiEffect>)` following existing overlay patterns.
- Add `Timeline` variant to `Overlay` enum in `overlays/mod.rs`.
- Add `pub mod timeline;` and re-export `TimelineState`.

**Tests:**
- `from_cells` with empty input returns empty entries
- `from_cells` filters out Tool/System/Thinking cells
- Preview truncation works correctly

### Slice 2: Overlay UI and navigation

**Files:** `src/modes/tui/overlays/timeline.rs`, `src/modes/tui/shared/commands.rs`

- Implement `TimelineState::render(&self, frame, area, input_y)`:
  - Use `render_utils::{calculate_overlay_area, render_overlay_container, render_hints}`.
  - Render list with role badge (`[U]`/`[A]`) + preview text.
  - Highlight selected entry, handle scroll offset for long lists.
  - Footer hints: `↑↓ navigate • Enter jump • f fork • Esc cancel`
- Implement `TimelineState::handle_key(&mut self, tui, key) -> (Option<OverlayAction>, Vec<StateCommand>)`:
  - `Up/Down/k/j`: Move selection, adjust offset if needed.
  - `PageUp/PageDown`: Move selection by visible page size.
  - `Home/End`: Jump to first/last entry.
  - `Esc/Ctrl+C`: Close overlay.
  - `Enter`: Return jump effect (Slice 3).
  - `f`: Return fork effect (Slice 3).
- Add `"timeline"` command to `COMMANDS` in `shared/commands.rs`.
- Wire command execution in `command_palette.rs` to open the timeline overlay.

**Tests:**
- Navigation bounds (can't go below 0 or above len-1)
- Scroll offset adjusts when selection moves out of view

### Slice 3: Jump + Fork actions

**Files:** `src/modes/tui/overlays/timeline.rs`, `src/modes/tui/shared/effects.rs`, `src/modes/tui/runtime/handlers.rs`

**Jump action:**
- On Enter, return `OverlayAction::close_with(vec![UiEffect::JumpToCell { cell_index }])`.
- Add `UiEffect::JumpToCell { cell_index: usize }` variant.
- Implement handler in `runtime/handlers.rs`:
  - Look up `scroll.cell_line_info[cell_index].start_line`.
  - Calculate centered offset: `start_line.saturating_sub(viewport_height / 2)`.
  - Set `scroll.mode = ScrollMode::Anchored { offset }`.

**Fork action:**
- On `f` key, return `OverlayAction::close_with(vec![UiEffect::ForkFromTurn { cell_index }])`.
- Add `UiEffect::ForkFromTurn { cell_index: usize }` variant.
- Implement handler in `runtime/handlers.rs`:
  - Create new session with events truncated to include only up to the selected turn.
  - If the selected cell is `User`, extract its content and populate input field via `StateCommand::Input(InputCommand::SetText(...))`.
  - If the selected cell is `Assistant`, just create the truncated session (input stays empty).
  - Clear transcript, load new session, append system message "Forked from turn N".

**Tests:**
- Jump calculates correct scroll offset for centering
- Fork with user message populates input
- Fork with assistant message leaves input empty

---

## Contracts

1. Normal transcript rendering is unchanged when the overlay is closed.
2. Opening timeline does not mutate transcript state.
3. Jump centers the transcript on the selected turn (clamped to valid scroll range).
4. Fork creates a new session; if forking from a user turn, that message appears in input for editing.
5. Keyboard focus returns to input after the overlay closes.

## Testing

- Manual: open timeline in a multi-turn session; verify navigation keys, jump to various turns, fork action creates new session.
- Automated: unit tests for navigation bounds, entry filtering, preview truncation; integration test for jump scroll calculation.
- Lint + fmt gate: `cargo clippy` and `cargo +nightly fmt` stay green.

---

## File Checklist

After implementation, update `AGENTS.md` "Where things are" section:

```
- `src/modes/tui/overlays/timeline.rs`: timeline overlay (turn list, jump, fork)
```

Add to overlays module list in the existing entry.
