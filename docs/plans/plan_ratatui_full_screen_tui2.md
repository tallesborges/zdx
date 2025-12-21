# Plan: Implement Full-Screen TUI (TUI2)

This plan outlines the steps to move from the current inline-viewport TUI to the full-screen alternate-screen TUI (TUI2) as specified in `docs/SPEC.md`.

## Goals
- Full-screen alternate-screen interface using `ratatui`.
- Scrollable, width-aware transcript rendering.
- Real-time streaming of assistant responses.
- Tool execution status indicators within the TUI.
- Persistent TUI state during engine execution.

## Design Principles

### Reducer pattern for TUI state
Treat TUI state updates like a reducer:
- `EngineEvent` (and user input events) go into a single `update(state, event)` function
- Rendering reads state only
- Tests become: "given events A, B, C → state matches snapshot"
- Aligns with "resumable conversations" goal (event stream is serializable)

### User journey drives order
Build in order of user journey: start TUI → type → send → see reply → stream → scroll → tools → selection/copy → markdown polish

---

# MVP Slices (each must be demoable)

These slices get to "daily-usable" as fast as possible. Each has a clear demo criterion.

## Slice 0: Terminal safety + blank screen ✅
Goal: Alt-screen + raw mode + restore guard. Never wreck the terminal.

- [x] Add `src/ui/tui2.rs` with `Tui2App` struct.
- [x] Enter alternate screen on start; leave on exit.
- [x] Enable raw mode on start; disable on exit.
- [x] **Guard pattern:** terminal restore via `Drop` impl.
- [x] **Panic hook:** restore terminal before printing panic.
- [x] **Signal handling:** ctrl-c restores terminal cleanly.
- [x] Quit key (`q` or `Ctrl+C`) exits cleanly.
- [x] ✅ **Demo:** `cargo run -- dev tui2` — start/quit never wrecks terminal.

**Implementation notes:**
- `Tui2App` struct in `src/ui/tui2.rs`.
- Hidden `dev tui2` CLI command for testing.
- Uses existing `core::interrupt` module for Ctrl+C handling (global handler already registered in main).
- Panic hook installed before entering alternate screen.
- `Drop` impl ensures terminal restored even on error paths.

## Slice 1: Input works (even ugly) ✅
Goal: Functional input editing. Use `tui-textarea` (already in deps).

- [x] Wire `tui-textarea` into input pane.
- [x] Insert/delete characters.
- [x] Cursor movement: left/right, home/end, up/down (multiline).
- [x] Multiline input support.
- [x] Submit vs newline: Enter submits, Shift+Enter for newline.
- [x] Paste handling (terminal paste works via EnableBracketedPaste).
- [x] On submit: create a `User` cell in transcript (in-memory only, no engine yet).
- [x] ✅ **Demo:** `cargo run -- dev tui2` — type/edit/paste, submit shows "You: ..." in transcript pane.

**Implementation notes:**
- Input area at bottom with `tui-textarea`.
- Transcript renders using `HistoryCell::display_lines()` from Phase 1.
- Style conversion from transcript `Style` enum to ratatui styles.
- Escape clears input, q quits (only when input empty).

## Slice 2: Send loop (no streaming yet)
Goal: Actually call the engine and get a response.

- [ ] On submit: spawn engine turn (non-blocking).
- [ ] Show "thinking..." or spinner while waiting.
- [ ] When response arrives: append `Assistant` cell with full text.
- [ ] No streaming, no markdown, plain text only.
- [ ] ✅ **Demo:** ask a question, get an answer displayed in TUI.

## Slice 3: Streaming (throttled)
Goal: Stream responses smoothly without input lag.

- [ ] Create bounded channel from engine to TUI.
- [ ] Map `AssistantDelta` events into transcript updates.
- [ ] Coalesce rapid deltas (don't redraw per-character).
- [ ] Tick-based redraw (e.g., 30fps max during streaming).
- [ ] Show streaming cursor (▌) during response.
- [ ] ✅ **Demo:** response streams smoothly, typing stays responsive during stream.

## Slice 4: Scroll (read long answers)
Goal: Navigate long transcripts.

- [ ] Wrap lines at current terminal width (plain text, no markdown yet).
- [ ] Flatten wrapped lines into visual-line list.
- [ ] Track scroll offset over flattened lines.
- [ ] **FollowLatest:** auto-scroll to bottom on new content (default).
- [ ] **Anchored:** user scroll (PageUp/Down, arrows) switches to anchored mode.
- [ ] Press `End` or `G` to re-enable follow-latest.
- [ ] ✅ **Demo:** long reply, PageUp/PageDown works, auto-scroll resumes.

---

At this point, TUI2 is **daily-usable enough to dogfood**.

---

# Foundations (completed)

## Phase 1: Transcript foundations (UI-agnostic) ✅
Goal: define the transcript types and rendering hook that everything else builds on.

Deliverables
- [x] Add `HistoryCell` (user/assistant/tool/system) in `src/core/transcript.rs`.
- [x] Add `StyledLine` representation (UI-agnostic).
- [x] Implement `HistoryCell::display_lines(width) -> Vec<StyledLine>` (plain text).

**Implementation notes:**
- `HistoryCell` variants: `User`, `Assistant` (with `is_streaming`), `Tool`, `System`.
- `Style` is semantic enum; renderers translate to terminal styles.
- `display_lines(width)` with word-wrapping, streaming cursors.
- 16 unit tests.

## Phase 1b: Stable IDs + timestamps ✅
Goal: Stable identifiers for selection, scroll anchoring, tool status.

Deliverables
- [x] `CellId(u64)` with atomic counter.
- [x] `created_at: DateTime<Utc>` on all cells.
- [x] `ToolState { Running, Done, Error }` with `started_at`.

**Implementation notes:**
- All cells have `id` and `created_at`.
- Tool cells have explicit state enum.
- 3 new tests.

---

# Polish Phases (after MVP)

## Phase 2a: Plain text wrap + scroll
Goal: Proper line wrapping (already partially done in Slice 4).

- [ ] Add `unicode-width` for display width (CJK, emoji).
- [ ] Wrap by display width, not byte length.
- [ ] Cache wrapped lines per `(cell_id, width)`.
- [ ] ✅ Check-in: emoji and CJK characters wrap correctly.

## Phase 2b: Markdown rendering (strict subset)
Goal: Styled markdown output. Keep scope tight.

**Subset (everything else = plain text):**
- paragraphs + soft wrap
- inline code (backticks)
- fenced code blocks
- bold/italic

Deliverables
- [ ] Add minimal markdown parser (or use `pulldown-cmark`).
- [ ] Convert markdown to styled spans.
- [ ] Store raw markdown in cells; render styled at display time.
- [ ] ✅ Check-in: code blocks render with background, bold/italic work.

## Phase 2c: Selection + copy
Goal: Select and copy transcript text.

### Unicode/grapheme decision
- **grapheme index** for selection (user-visible character)
- Add `unicode-segmentation` crate

### Copy transport decision
- [ ] **OSC 52** (terminal clipboard): works in many terminals
- [ ] **System clipboard** fallback (`arboard` crate)
- [ ] **Internal buffer** fallback if both fail

Deliverables
- [ ] Position mapping: `(visual_line, column)` ↔ `(cell_id, raw_text_range)`.
- [ ] Selection overlay on visible lines.
- [ ] Copy reconstructs text with correct wrapping.
- [ ] ✅ Check-in: select text, copy, paste elsewhere matches.

## Phase 3: Tool UI
Goal: Show tool execution in transcript.

- [ ] "Tool running..." indicator when tool starts.
- [ ] Show tool name and status (running/done/failed).
- [ ] Optionally show tool output preview.
- [ ] ✅ Check-in: tool use shows status in TUI, not stdout.

## Phase 4: Streaming fidelity + performance
Goal: Stable streaming under resizes and long outputs.

- [ ] Commit cursor: render only committed prefix during stream.
- [ ] Redraw throttling to reduce flicker.
- [ ] Wrap cache invalidation on resize.
- [ ] Avoid whole-transcript rewrap per frame.
- [ ] ✅ Check-in: stream long response, resize terminal, no flicker or lag.

## Phase 5: Input polish
Goal: Quality-of-life input features.

- [ ] History navigation (↑/↓ for previous commands).
- [ ] Ctrl+C to cancel current input or interrupt engine.
- [ ] Status line with mode indicator.
- [ ] ✅ Check-in: input feels polished for daily use.

## Phase 6: Default + cleanup
Goal: TUI2 becomes the default.

- [ ] Switch `zdx` interactive path to TUI2 by default.
- [ ] Deprecate `src/ui/app.rs` (inline TUI).
- [ ] Integration test: "no stdout transcript while TUI2 is active".
- [ ] ✅ Check-in: go/no-go for shipping as default.

---

# Later / Explicitly Deferred

These items are **not** in MVP or Polish phases. Don't build until needed.

- **Grapheme-perfect selection** — byte index is fine for MVP
- **Full markdown spec** — lists, tables, links, images
- **Tool output streaming** — show simple status line first
- **Bidirectional position mapping** — only if needed for cursor-follow
- **Session resume in TUI2** — works via existing session system
- **Mouse selection** — keyboard-only is fine for MVP

---

# Minimal transcript model for MVP

You don't need the full visual-line pipeline up front. MVP uses:

```rust
Vec<HistoryCell { id, role, raw_text, is_streaming }>
```

- Wrapping at render time (basic word-wrap)
- Scroll offset over flattened wrapped lines
- Cache per `(cell_id, width)` added in polish phase

This supports long-term architecture without blocking sending messages.
