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

---

## Phase 1: Transcript foundations (UI-agnostic) ✅
Goal: define the transcript types and rendering hook that everything else builds on.

Deliverables (small chunks)
- [x] Add `HistoryCell` (user/assistant/tool/system) in a UI-agnostic module (prefer `src/core/`).
- [x] Add/choose a minimal `StyledLine` representation (or reuse an existing type) that can be produced without terminal I/O.
- [x] Implement `HistoryCell::display_lines(width) -> Vec<StyledLine>` for at least one variant (start with plain text).
- [x] Check-in: confirm `HistoryCell` variants/fields match `docs/SPEC.md` expectations before adding complexity.

**Implementation notes:**
- Added `src/core/transcript.rs` with `HistoryCell`, `StyledSpan`, `StyledLine`, and `Style` types.
- `HistoryCell` variants: `User`, `Assistant` (with `is_streaming` flag), `Tool` (with optional `result`), `System`.
- `Style` is a semantic enum (e.g., `UserPrefix`, `Assistant`, `StreamingCursor`) - renderers translate to terminal styles.
- `display_lines(width)` implemented with basic word-wrapping; handles multi-line content and streaming cursors.
- 16 unit tests covering all variants, wrapping, streaming indicators, and cell mutations.

### Phase 1b: Stable IDs + timestamps ✅
Goal: Add stable identifiers early to avoid surprise refactors later (needed for selection, scroll anchoring, tool status).

Deliverables
- [x] Add `cell_id: CellId` (u64 or UUID) to `HistoryCell`.
- [x] Add `created_at: Option<DateTime<Utc>>` for ordering/display.
- [x] For Tool cells: add explicit state enum `ToolState { Running, Done, Error }` with `started_at` timestamp.
- [x] Update constructors and tests.

**Implementation notes:**
- Added `CellId(u64)` with atomic counter for unique ID generation.
- Added `ToolState` enum with `Running`, `Done`, `Error` variants and `as_str()` method.
- All `HistoryCell` variants now have `id: CellId` and `created_at: DateTime<Utc>`.
- Tool cells have additional `started_at` timestamp and explicit `state: ToolState`.
- Added helper methods: `id()`, `created_at()`, `tool_state()`.
- 3 new tests: `test_cell_id_unique`, `test_tool_state_as_str`, `test_cell_has_id_and_timestamp`.

---

## Phase 2: Visual-line pipeline (wrap + flatten + scroll + selection)
Goal: turn a transcript into flattened visual lines that support resize reflow, scrolling, and selection.

### Unicode handling decision
Define the "unit of indexing" for selection/cursor early:
- **grapheme index** is best for UI (single user-visible character)
- Add `unicode-width` and `unicode-segmentation` crates
- TODO: Handle display width (CJK, emoji) and grapheme clusters

### Markdown subset (strict scope)
Render with a minimal parser; treat everything else as plain text:
- paragraphs + soft wrap
- inline code (backticks)
- fenced code blocks
- bold/italic if cheap
- **NOT included initially:** lists, tables, links, images

Deliverables (small chunks)
- [ ] Add `unicode-width` + `unicode-segmentation` dependencies.
- [ ] Define indexing unit: grapheme index for selection, byte index for storage.
- [ ] Add markdown-to-styled-spans helper for a single cell (strict subset above).
- [ ] Add span-wrapping helper that handles unicode width correctly.
- [ ] Add transcript flattener that concatenates per-cell wrapped lines into a single visual-line list.
- [ ] **Position mapping:** emit optional metadata mapping `(visual_line_index, column)` ↔ `(cell_id, raw_text_range)`.
- [ ] Check-in: review a small "golden" transcript (user + assistant + tool) and confirm ordering and wrapping.
- [ ] Add scroll windowing helper: (offset, height) -> visible slice.
- [ ] Add selection overlay on the visible slice (selection is defined over flattened visual lines, excluding any gutter/prefix).
- [ ] Check-in: confirm scroll math is based on flattened visual lines (not terminal scrollback) and selection doesn't change scroll math.

---

## Phase 3: Full-screen TUI shell (alt-screen + raw mode + clean restore)
Goal: run a stable full-screen TUI loop that owns terminal state and redraws from in-memory state.

### Terminal restore safety
Use a guard pattern so restore happens in `Drop`:
- Raw mode + alt-screen problems happen on: panics, ctrl-c/SIGINT, task cancellation
- Install a panic hook that restores terminal before printing panic
- This is the difference between "safe to use daily" and "occasionally wrecks my terminal"

Deliverables (small chunks)
- [ ] Add `src/ui/tui2.rs` with a minimal `Tui2App` that can render a frame.
- [ ] Use `Viewport::Fullscreen` and draw a placeholder layout (transcript pane + input pane).
- [ ] Enter alternate screen on start; leave it on exit.
- [ ] Enable raw mode on start; disable it on exit.
- [ ] **Guard pattern:** terminal restore via `Drop` impl on a guard struct.
- [ ] **Panic hook:** install hook that restores terminal before printing panic.
- [ ] **Signal handling:** ensure ctrl-c restores terminal cleanly.
- [ ] Check-in: manual smoke test start/quit and verify the terminal is not left in raw mode or alt-screen.

### Phase 3b: Input editing
Goal: Functional input editing so the TUI is usable for daily work.

Approach decision:
- [ ] **Option A:** Use existing widget/crate (e.g., `tui-textarea`)
- [ ] **Option B:** Custom minimal editor

Minimum requirements:
- [ ] Insert/delete characters
- [ ] Cursor movement: left/right, home/end
- [ ] Multiline input support
- [ ] Submit vs newline (Enter submits, Shift+Enter or Ctrl+Enter for newline)
- [ ] History navigation (↑/↓ for previous commands)
- [ ] Paste handling (Ctrl+V or terminal paste)
- [ ] Check-in: input feels responsive and supports basic editing workflows.

---

## Phase 4: Engine ↔ TUI integration (event stream, no stdout transcript)
Goal: feed engine events into the TUI without printing the transcript to stdout while TUI2 is active.

### EngineEvent schema (reducer-friendly)
Events should be:
- **Append-only** and ordered
- **ID-addressable** (e.g., `cell_id`, `span_id`)
- Avoid "replace the whole transcript" updates

Event types needed:
- `AssistantStart { cell_id }`, `AssistantDelta { cell_id, text }`, `AssistantEnd { cell_id }`
- `ToolStart { cell_id, name, args_preview }`, `ToolOutput { cell_id, chunk }`, `ToolEnd { cell_id, success }`
- `Status { message }` (spinner, queued, waiting)
- `Error { cell_id?, message, details }` (display in transcript, not stdout)

### Backpressure/throttling
Streaming can produce events faster than UI can draw:
- Use bounded channel with "latest wins" coalescing for `AssistantDelta`
- Or coalesce deltas in engine before sending
- Prevent: input lag, unbounded queue, CPU burn

Deliverables (small chunks)
- [ ] Define `EngineEvent` enum with cell_id addressing.
- [ ] Create a bounded channel from engine task to TUI task.
- [ ] Implement delta coalescing (batch rapid deltas before send or in receiver).
- [ ] Map engine events into transcript updates via reducer pattern.
- [ ] Wire `run_chat_loop_tty` to boot into `Tui2App` and run the TUI loop.
- [ ] On submit: spawn an engine turn without blocking UI input/rendering.
- [ ] Enforce the contract: no transcript output to stdout while TUI2 is active.
- [ ] Check-in: run a simple prompt and confirm you see streaming deltas in the TUI and stdout stays clean.

---

## Phase 5: Navigation + copy (follow-latest, anchored, selection)
Goal: make the transcript readable and navigable in long sessions with correct copy semantics.

### Copy mechanism decision
Choose transport early (don't block on this late):
- [ ] **OSC 52** (terminal clipboard): works in many terminals, not all
- [ ] **System clipboard crate:** works in GUI environments, not SSH/headless
- [ ] **Fallback:** "copied to internal buffer" with save/paste command

Deliverables (small chunks)
- [ ] Add a status line with scroll position and mode indicator.
- [ ] Implement `FollowLatest` vs `Anchored` scroll modes (scrolling switches to anchored; follow-latest can be re-enabled).
- [ ] Implement selection over flattened visual lines (line/col), excluding any left gutter/prefix.
- [ ] **Copy transport:** implement OSC 52 with system clipboard fallback.
- [ ] Implement copy that reconstructs text using the same wrapping rules (including code block indentation and wide glyph widths).
- [ ] Check-in: manually verify scroll mode switching, selection fidelity, and copied text matches what you see.

How to test
- [ ] `cargo run -- zdx` (manual: long response; scroll; select; copy; paste elsewhere and compare)

---

## Phase 6: Streaming fidelity + performance (commit cursor, throttling)
Goal: make streaming stable (no flicker) and correct under resizes and long outputs.

### Cache rewrap by (cell_id, width)
Prevent O(n) scrolling over entire history:
- Each `HistoryCell` caches its wrapped visual lines keyed by `width`
- On resize, invalidate caches (or keep small LRU per cell)
- Flattener works from cached wrapped lines

Deliverables (small chunks)
- [ ] Store assistant raw markdown append-only (no lossy transformations during streaming).
- [ ] Add a commit cursor and render only the committed prefix (keep uncommitted hidden until committed).
- [ ] Add redraw throttling during streaming to reduce flicker.
- [ ] **Wrap cache:** cache wrapped lines per `(cell_id, width)`, invalidate on resize.
- [ ] Avoid whole-transcript rewrap per frame; scope rewrap to changed/visible content.
- [ ] Check-in: stream a long response and resize the terminal; confirm stability and acceptable CPU usage.

---

## Phase 7: Default behavior + guarded cleanup
Goal: make TUI2 the default interactive UI and keep the repository tidy without breaking shipped behavior.

Deliverables (small chunks)
- [ ] Switch `zdx` interactive path to use TUI2 by default.
- [ ] Deprecate `src/ui/app.rs` (inline TUI) only after TUI2 reaches parity for core flows.
- [ ] Add a light integration test only when a user-visible contract regresses (example: "no stdout transcript while TUI2 is active").
- [ ] Check-in: go/no-go decision for shipping TUI2 as default.

---

## MVP path (ship-first ordering)

If daily usability matters more than perfect transcript modeling early:

1. **Phase 3** minimal alt-screen shell + input editor (even crude)
2. **Phase 4** event stream with assistant streaming into a single assistant cell
3. **Phase 2** scroll + wrap (enough to read long answers)
4. Then selection/copy and tool indicators

Current plan is sensible; this is just an alternative "ship-first" ordering.
