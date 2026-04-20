# Plan: Scroll Delta Accumulator

**Project/feature:** Implement scroll delta accumulation to coalesce rapid mouse/trackpad scroll events within a frame window, improving scroll smoothness especially on macOS trackpads which generate many fine-grained events.

**Existing state:**
- `ScrollState` in `src/ui/state/transcript.rs` with `ScrollMode` (FollowLatest/Anchored)
- `handle_mouse()` in `src/ui/update.rs` directly calls `scroll_up/down(MOUSE_SCROLL_LINES)` per event
- `MOUSE_SCROLL_LINES = 1` for smooth scrolling on trackpads
- Event loop batches all available terminal events before render (16ms frame)

**Constraints:**
- Must not break existing scroll behavior (keyboard PgUp/PgDn, Home/End)
- Must work with current `FollowLatest`/`Anchored` modes
- No external crates for timing (use `std::time::Instant`)

**Success looks like:** Trackpad scrolling feels smooth without jitter; discrete wheel scrolls remain responsive; no regression in keyboard scroll.

---

## Goals

- Coalesce multiple scroll events within a frame into a single scroll operation
- Improve trackpad scroll UX by summing deltas before applying
- Maintain responsive feel for discrete mouse wheel scrolls

## Non-goals

- Trackpad vs wheel detection (deferred—requires heuristics)
- Configurable scroll speed/sensitivity
- Momentum/inertial scrolling
- Touch gesture support

## Design principles

- **User journey drives order**: get basic accumulation working first, then tune
- **Ship-first**: functional accumulator > perfect scroll physics
- **Keep existing behavior for keyboard**: only affect mouse scroll events

## User journey

1. User scrolls up/down with trackpad → events accumulate within frame
2. At frame render time → accumulated delta applied as single scroll
3. User sees smooth scroll movement without per-event jitter
4. Keyboard scroll (PgUp/Down, Home/End) unchanged → immediate response

## Foundations / Already shipped (✅)

### ScrollState and TranscriptState
- **What exists**: `ScrollState` with `scroll_up/down(lines, viewport_height)` methods in `src/ui/state/transcript.rs`
- **✅ Demo**: `cargo test scroll` passes
- **Gaps**: None

### Event loop batching
- **What exists**: `collect_events()` drains all available terminal events before render
- **✅ Demo**: Multiple rapid keypresses processed together
- **Gaps**: Mouse scroll events are applied immediately in `handle_mouse()`, not accumulated

### Dirty flag rendering
- **What exists**: Only renders when `dirty = true`, throttled at 16ms
- **✅ Demo**: Streaming text doesn't cause per-character redraws
- **Gaps**: None

---

## MVP slices (ship-shaped, demoable)

### Slice 1: ScrollAccumulator struct ✅ DONE

**Goal:** Add accumulator state to track pending scroll delta.

**Scope checklist:**
- [x] Add `ScrollAccumulator` struct to `src/ui/state/transcript.rs`
  - `pending_delta: i32` (positive = down, negative = up)
- [x] Add `scroll_accumulator: ScrollAccumulator` field to `TranscriptState`
- [x] Add `accumulate(&mut self, delta: i32)` method
- [x] Add `take_delta(&mut self) -> i32` method (returns and resets)
- [x] Add `has_pending(&self) -> bool` helper
- [x] Unit tests for accumulator (6 tests)

**✅ Demo:**
```bash
cargo test scroll_accumulator  # 6 tests pass
```

**Risks / failure modes:**
- Over-engineering: keep it simple (just `i32` delta, no timing yet)

---

### Slice 2: Integrate accumulator in update.rs ✅ DONE

**Goal:** Mouse scroll events accumulate delta instead of scrolling immediately.

**Scope checklist:**
- [x] Modify `handle_mouse()` to call `scroll_accumulator.accumulate(delta)` instead of `scroll_up/down`
- [x] Add `apply_scroll_delta(state: &mut TuiState)` function
  - Takes accumulated delta, applies scroll, resets accumulator
- [x] Call `apply_scroll_delta()` in `event_loop()` before render
- [x] Keep keyboard scroll unchanged (direct `scroll_up/down` calls)
- [x] Integration tests for `apply_scroll_delta()` (2 tests)

**✅ Demo:**
1. Run `cargo run`
2. Rapid trackpad scroll → smooth movement
3. PgUp/PgDn → still works immediately
4. `cargo test` passes (329 tests)

**Risks / failure modes:**
- Delta sign confusion (up = negative, down = positive) ✅ Handled

---

### Slice 3: Add minimum delta threshold ✅ SKIPPED (implicit)

**Goal:** Apply scroll only when accumulated delta crosses a threshold.

**Analysis:** This slice is unnecessary because:
- `MOUSE_SCROLL_LINES = 1` already sets minimum scroll unit to 1 line
- Delta is accumulated as integers (no fractional lines)
- `apply_scroll_delta()` returns early when delta is 0
- The threshold of 1 line is implicit in the design

**✅ Demo:**
1. Very slow trackpad scroll → scrolls 1 line at a time (minimum)
2. Fast trackpad scroll → scrolls accumulated lines in one operation

**Decision:** Skip this slice - the MVP is complete.

---

## Contracts (guardrails)

1. **Keyboard scroll unchanged**: PgUp/PgDn/Home/End must bypass accumulator
2. **FollowLatest/Anchored modes preserved**: Accumulator only affects delta, not mode transitions
3. **No scroll on zero delta**: `apply_scroll_delta()` is a no-op when delta is zero
4. **Existing tests pass**: `cargo test scroll` must not regress

## Key decisions (decide early)

| Decision | Options | Recommendation |
|----------|---------|----------------|
| Delta sign convention | up=negative/down=positive OR reversed | up=negative, down=positive (matches screen coordinates) |
| Where to apply delta | In `handle_mouse()` vs `event_loop()` | In `event_loop()` after all events processed |
| Threshold value | 1, 2, or 3 lines | Start with 1, tune based on feel |

## Testing

### Manual smoke demos per slice
- **Slice 1**: Unit tests only
- **Slice 2**: Interactive trackpad scroll, keyboard scroll
- **Slice 3**: Slow vs fast trackpad scroll comparison

### Regression tests
- Existing `test_scroll_to_top`, `test_scroll_to_bottom`, `test_scroll_up_and_down` must pass
- New unit tests for accumulator `accumulate()` and `take_delta()`

## Polish phases (after MVP)

### Phase 1: Tuning
- [ ] Experiment with `SCROLL_THRESHOLD` values (1-3)
- [ ] Add decay: if no scroll events for N frames, reset accumulator
- **✅ Check-in demo**: Side-by-side comparison with/without accumulator

### Phase 2: Adaptive lines-per-delta (optional)
- [ ] If delta magnitude is large, scroll more lines per unit
- [ ] Adds non-linearity for fast flicks
- **✅ Check-in demo**: Fast flick scrolls farther than slow scroll

## Later / Deferred

| Item | Trigger to revisit |
|------|-------------------|
| Trackpad vs wheel detection | Users report wheel scroll feels wrong |
| Configurable scroll speed | Feature request from users |
| Momentum scrolling | Comparative UX review with other TUIs |
| Touch gesture support | Touch-capable terminal becomes common |
