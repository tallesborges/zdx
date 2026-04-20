# Plan: Lazy Transcript Rendering

**Project/feature:** Optimize transcript rendering for long sessions by only rendering visible cells instead of the entire thread history, achieving O(viewport) instead of O(n) complexity per frame.

**Existing state:**
- `render_transcript()` in `src/ui/chat/view.rs` iterates ALL cells and renders ALL lines
- `WrapCache` in `src/ui/transcript/wrap.rs` caches wrapped lines per cell (already optimized)
- `ScrollState` in `src/ui/chat/state/transcript.rs` tracks scroll mode and offset
- `PositionMap` in `src/ui/chat/selection.rs` tracks line text for selection
- View slices rendered lines AFTER full rendering: `all_lines.skip(scroll_offset).take(viewport)`

**Constraints:**
- Must maintain correct scroll behavior (follow mode, anchored mode)
- Must maintain working selection (click, drag, copy)
- Must maintain position map for text extraction
- No visual differences from full rendering
- First frame may use full rendering (before cell_line_info populated)

**Success looks like:** User with 1000+ message session can scroll smoothly at 60fps without lag, while selection and copy still work correctly.

---

## Goals

- Render only visible cells (O(viewport) instead of O(n) per frame)
- Maintain binary search O(log n) for visibility calculation
- Keep selection highlighting working with global line indices
- Keep copy/paste working with lazy-rendered position map
- Graceful fallback to full rendering when needed

## Non-goals

- Virtualized cell recycling (cells are still created, just not rendered)
- Async/background rendering
- Progressive loading of old messages
- Memory optimization for cell storage (only rendering is optimized)

## Design principles

- **User journey drives order**: get visibility calculation working, then rendering, then selection
- **Ship-first**: correct behavior > perfect optimization
- **Graceful degradation**: fall back to full rendering if lazy fails
- **Keep existing behavior**: keyboard/mouse scroll, selection must work identically

## User journey

1. User opens zdx with long session history (1000+ messages)
2. First frame: full render (builds cell_line_info index)
3. Subsequent frames: lazy render (only visible ~5 cells rendered)
4. User scrolls → binary search finds visible cells → smooth 60fps
5. User selects text → selection highlights correctly using global indices
6. User copies → correct text extracted despite lazy position map

## Foundations / Already shipped (✅)

### WrapCache for cell content
- **What exists**: `WrapCache` in `src/ui/transcript/wrap.rs` caches wrapped lines per `(cell_id, width, content_len)`
- **✅ Demo**: `cargo test wrap_cache` passes
- **Gaps**: Cache lookup still happens for ALL cells, even off-screen

### ScrollState with modes
- **What exists**: `ScrollState` with `FollowLatest`/`Anchored` modes, `get_offset()`, `scroll_up/down()`
- **✅ Demo**: `cargo test scroll_state` passes
- **Gaps**: No cell-level visibility tracking, only line-level offset

### PositionMap for selection
- **What exists**: `PositionMap` rebuilt each frame with line text for selection coordinate translation
- **✅ Demo**: Selection works in full render mode
- **Gaps**: Indexed by global line number, breaks if only visible lines stored

---

## MVP slices (ship-shaped, demoable)

### Slice 1: Cell line info tracking ✅ DONE

**Goal:** Track how many lines each cell produces for visibility calculations.

**Scope checklist:**
- [x] Add `CellLineInfo` struct with `cell_id`, `start_line`, `line_count` fields
- [x] Add `cell_line_info: Vec<CellLineInfo>` to `ScrollState`
- [x] Add `update_cell_line_info()` method that builds cumulative offsets
- [x] Update `cached_line_count` in same call (single source of truth)
- [x] Add `reset()` to clear cell_line_info
- [x] Unit tests for cell line info (3 tests)

**✅ Demo:**
```bash
cargo test update_cell_line_info  # passes
```

**Files changed:**
- `src/ui/chat/state/transcript.rs` - Added structs and methods

---

### Slice 2: Visible range calculation ✅ DONE

**Goal:** Binary search to find which cells overlap with viewport.

**Scope checklist:**
- [x] Add `VisibleRange` struct with `cell_range`, `first_cell_line_offset`, `lines_before`
- [x] Add `visible_range(viewport_height) -> Option<VisibleRange>` method
- [x] Use `partition_point` for O(log n) binary search
- [x] Return `None` if cell_line_info empty (triggers full render fallback)
- [x] Unit tests for visible range (6 tests covering edge cases)

**✅ Demo:**
```bash
cargo test visible_range  # 6 tests pass
```

**Files changed:**
- `src/ui/chat/state/transcript.rs` - Added VisibleRange and visible_range()
- `src/ui/chat/state/mod.rs` - Re-exported VisibleRange

---

### Slice 3: Lazy render path ✅ DONE

**Goal:** Split render_transcript into full/lazy paths.

**Scope checklist:**
- [x] Add `render_transcript_full()` - original behavior, all cells
- [x] Add `render_transcript_lazy()` - only visible cells from range
- [x] Modify `render_transcript()` to return `(Vec<Line>, bool)` where bool = is_lazy
- [x] Update `view()` to skip scroll slicing when is_lazy=true
- [x] Handle first_cell_line_offset (skip lines in first visible cell)
- [x] Add blank line after each cell (consistent with full render)

**✅ Demo:**
```bash
cargo run  # Long session scrolls smoothly
```

**Files changed:**
- `src/ui/chat/view.rs` - Split render paths, updated view()

**Risks / failure modes:**
- Double-scroll bug if view() slices already-scrolled lines ✅ Fixed with is_lazy flag
- Blank line mismatch between full/lazy ✅ Fixed, both add blank after every cell

---

### Slice 4: Selection mapping for lazy mode ✅ DONE

**Goal:** Make selection work when position_map only has visible lines.

**Scope checklist:**
- [x] Add `scroll_offset: RefCell<usize>` to `PositionMap`
- [x] Add `set_scroll_offset()` method called by lazy render
- [x] Update `get_text_range()` to translate global→local indices
- [x] Update `screen_to_transcript_pos()` in reducer to detect lazy mode
- [x] Use local indexing for position_map.get() in lazy mode
- [x] Clear scroll_offset in `clear()` method

**✅ Demo:**
```bash
cargo run  # Select text while scrolled, Ctrl+C copies correct text
cargo test position_map  # passes
```

**Files changed:**
- `src/ui/chat/selection.rs` - Added scroll_offset, updated get_text_range()
- `src/ui/chat/reducer.rs` - Updated screen_to_transcript_pos()
- `src/ui/chat/view.rs` - Call set_scroll_offset() in lazy render

**Risks / failure modes:**
- Selection indices off-by-one ✅ Fixed with proper offset subtraction
- Copy getting wrong text ✅ Fixed with global→local translation

---

### Slice 5: Runtime integration ✅ DONE

**Goal:** Wire up cell_line_info updates in event loop.

**Scope checklist:**
- [x] Add `calculate_cell_line_counts()` function in view.rs
- [x] Call it in event loop before render (in mod.rs)
- [x] Pass result to `scroll.update_cell_line_info()`
- [x] Remove old `calculate_line_count()` call (replaced)

**✅ Demo:**
```bash
cargo run  # First frame: full render, subsequent: lazy render
cargo test  # 235 tests pass
```

**Files changed:**
- `src/ui/chat/view.rs` - Added calculate_cell_line_counts()
- `src/ui/chat/mod.rs` - Updated event loop

---

## Performance analysis

### Complexity comparison

| Operation | Before | After |
|-----------|--------|-------|
| Find visible cells | N/A | O(log n) binary search |
| Render lines | O(total_lines) | O(viewport_lines) |
| Build position_map | O(total_lines) | O(viewport_lines) |
| Memory per frame | O(total_lines) Line objects | O(viewport_lines) Line objects |

### Real-world impact

| Scenario | Before | After | Improvement |
|----------|--------|-------|-------------|
| 10 cells | ~50 lines rendered | ~25 visible | 2x |
| 100 cells | ~500 lines rendered | ~25 visible | 20x |
| 1000 cells | ~5000 lines rendered | ~25 visible | 200x |

---

## Validation

- [x] All 235 unit tests pass
- [x] Codex code review: All 5 identified bugs fixed
- [x] Gemini code review: Implementation correct and robust
- [x] Manual testing: scroll, selection, copy all work correctly

---

## Future improvements (not in scope)

- Skip grapheme counting when no selection active (minor perf gain)
- Cache cell_line_info across frames when cells unchanged
- Virtualized cell recycling for memory optimization
- Background pre-rendering of adjacent cells
