# Plan: Implement Full-Screen TUI (TUI2)

This plan outlines the steps to move from the current inline-viewport TUI to the full-screen alternate-screen TUI (TUI2) as specified in `docs/SPEC.md`.

## Goals
- Full-screen alternate-screen interface using `ratatui`.
- Scrollable, width-aware transcript rendering.
- Real-time streaming of assistant responses.
- Tool execution status indicators within the TUI.
- Persistent TUI state during engine execution.

## Phase 1: Transcript foundations (UI-agnostic)
Goal: define the transcript types and rendering hook that everything else builds on.

Deliverables (small chunks)
- [ ] Add `HistoryCell` (user/assistant/tool/system) in a UI-agnostic module (prefer `src/core/`).
- [ ] Add/choose a minimal `StyledLine` representation (or reuse an existing type) that can be produced without terminal I/O.
- [ ] Implement `HistoryCell::display_lines(width) -> Vec<StyledLine>` for at least one variant (start with plain text).
- [ ] Check-in: confirm `HistoryCell` variants/fields match `docs/SPEC.md` expectations before adding complexity.

## Phase 2: Visual-line pipeline (wrap + flatten + scroll + selection)
Goal: turn a transcript into flattened visual lines that support resize reflow, scrolling, and selection.

Deliverables (small chunks)
- [ ] Add markdown-to-styled-spans helper for a single cell (minimal subset first).
- [ ] Add span-wrapping helper that produces multiple lines for small widths.
- [ ] Add transcript flattener that concatenates per-cell wrapped lines into a single visual-line list.
- [ ] Check-in: review a small “golden” transcript (user + assistant + tool) and confirm ordering and wrapping.
- [ ] Add scroll windowing helper: (offset, height) -> visible slice.
- [ ] Add selection overlay on the visible slice (selection is defined over flattened visual lines, excluding any gutter/prefix).
- [ ] Check-in: confirm scroll math is based on flattened visual lines (not terminal scrollback) and selection doesn’t change scroll math.


## Phase 3: Full-screen TUI shell (alt-screen + raw mode + clean restore)
Goal: run a stable full-screen TUI loop that owns terminal state and redraws from in-memory state.

Deliverables (small chunks)
- [ ] Add `src/ui/tui2.rs` with a minimal `Tui2App` that can render a frame.
- [ ] Use `Viewport::Fullscreen` and draw a placeholder layout (transcript pane + input pane).
- [ ] Enter alternate screen on start; leave it on exit.
- [ ] Enable raw mode on start; disable it on exit.
- [ ] Ensure terminal restore runs on early exit paths (errors/interruption).
- [ ] Check-in: manual smoke test start/quit and verify the terminal is not left in raw mode or alt-screen.

## Phase 4: Engine ↔ TUI integration (event stream, no stdout transcript)
Goal: feed engine events into the TUI without printing the transcript to stdout while TUI2 is active.

Deliverables (small chunks)
- [ ] Create an `EngineEvent` channel from the engine task to the TUI task.
- [ ] Map one engine event into a transcript update (e.g., `AssistantDelta { text }` appends to an assistant cell).
- [ ] Wire `run_chat_loop_tty` to boot into `Tui2App` and run the TUI loop.
- [ ] On submit: spawn an engine turn without blocking UI input/rendering.
- [ ] Enforce the contract: no transcript output to stdout while TUI2 is active (diagnostics belong in the UI; optional file logging ok).
- [ ] Check-in: run a simple prompt and confirm you see streaming deltas in the TUI and stdout stays clean.

## Phase 5: Navigation + copy (follow-latest, anchored, selection)
Goal: make the transcript readable and navigable in long sessions with correct copy semantics.

Deliverables (small chunks)
- [ ] Add a status line with scroll position and mode indicator.
- [ ] Implement `FollowLatest` vs `Anchored` scroll modes (scrolling switches to anchored; follow-latest can be re-enabled).
- [ ] Implement selection over flattened visual lines (line/col), excluding any left gutter/prefix.
- [ ] Implement copy that reconstructs text using the same wrapping rules (including code block indentation and wide glyph widths).
- [ ] Check-in: manually verify scroll mode switching, selection fidelity, and copied text matches what you see.

How to test
- [ ] `cargo run -- zdx` (manual: long response; scroll; select; copy; paste elsewhere and compare)

## Phase 6: Streaming fidelity + performance (commit cursor, throttling)
Goal: make streaming stable (no flicker) and correct under resizes and long outputs.

Deliverables (small chunks)
- [ ] Store assistant raw markdown append-only (no lossy transformations during streaming).
- [ ] Add a commit cursor and render only the committed prefix (keep uncommitted hidden until committed).
- [ ] Add redraw throttling during streaming to reduce flicker.
- [ ] Avoid whole-transcript rewrap per frame; scope rewrap to changed/visible content where possible.
- [ ] Check-in: stream a long response and resize the terminal; confirm stability and acceptable CPU usage.

## Phase 7: Default behavior + guarded cleanup
Goal: make TUI2 the default interactive UI and keep the repository tidy without breaking shipped behavior.

Deliverables (small chunks)
- [ ] Switch `zdx` interactive path to use TUI2 by default.
- [ ] Deprecate `src/ui/app.rs` (inline TUI) only after TUI2 reaches parity for core flows.
- [ ] Add a light integration test only when a user-visible contract regresses (example: “no stdout transcript while TUI2 is active”).
- [ ] Check-in: go/no-go decision for shipping TUI2 as default.
