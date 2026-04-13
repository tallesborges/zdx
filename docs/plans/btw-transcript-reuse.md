# BTW Overlay Transcript Reuse

Thread reference: `43a088c4-c3d4-4e6a-a9e6-feceb9770ee7`

## Goal

Make the `/btw` overlay reuse the main transcript rendering path instead of maintaining its own parallel implementation.

## Problem

`crates/zdx-tui/src/overlays/btw.rs` currently duplicates transcript behavior that already exists in the main transcript feature:

- custom `render_transcript_lines`
- custom `render_visible_transcript_lines`
- custom `map_style`
- custom `scroll_from_bottom: usize`

This has already drifted from the main UI:

- user-message styling differs from the main transcript
- BTW hardcodes `spinner_frame = 0`, so animated tool states do not render correctly
- scroll behavior is separate from `TranscriptState` / `ScrollState`

## Constraints

- BTW is a fixed popup (`96x28` intent), so full render is fine
- no selection or position-map support is needed in BTW
- spinner frame is still needed for streaming / animated tool cells
- keep the refactor small and local; do not redesign the whole transcript system

## Plan

1. Add a shared full-render helper in `crates/zdx-tui/src/features/transcript/render.rs`:

   ```rust
   pub fn render_transcript_cells(
       transcript: &TranscriptState,
       width: usize,
       spinner_frame: usize,
   ) -> Vec<Line<'static>>
   ```

   Behavior:
   - iterate `transcript.cells()`
   - call `display_lines_cached(width, spinner_frame / SPINNER_SPEED_DIVISOR, &transcript.wrap_cache)`
   - convert with the existing main transcript style conversion
   - append one blank line between cells
   - do not touch selection, position map, or lazy rendering

2. Export `render_transcript_cells` from `crates/zdx-tui/src/features/transcript/mod.rs`.

3. Thread `spinner_frame` through the overlay render path:
   - `crates/zdx-tui/src/render.rs`
   - `crates/zdx-tui/src/overlays/mod.rs`
   - `crates/zdx-tui/src/overlays/btw.rs`

4. Switch BTW rendering to call `render_transcript_cells(...)` and slice the visible viewport from that output.

5. Replace BTW's `scroll_from_bottom` with `TranscriptState` scroll support:
   - use `transcript.update_layout(...)`
   - compute/update `cached_line_count` before paging / mouse-scroll operations
   - use `page_up`, `page_down`, `scroll_up`, `scroll_down`, and `scroll_to_bottom`

6. Delete BTW-only duplicate helpers:
   - `render_transcript_lines`
   - `render_visible_transcript_lines`
   - `map_style`

7. Keep BTW chrome styling (border, prompt, picker accents), but unify transcript content styling with the main transcript renderer.

8. Update BTW tests to cover opening, scrolling, and rendering through the shared path.

## Order of work

1. add `render_transcript_cells`
2. export it from `mod.rs`
3. pass `spinner_frame` into overlay rendering
4. switch BTW render path first
5. migrate BTW scroll handling to `TranscriptState`
6. delete duplicate BTW helpers
7. update tests

This order keeps the app compiling throughout the refactor.

## Expected outcome

- one transcript style source of truth
- one shared cell-rendering path for main transcript + BTW popup
- correct spinner animation in BTW
- BTW scroll behavior aligned with the main transcript model
- less future drift when transcript rendering changes