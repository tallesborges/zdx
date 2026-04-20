# Ratatui Buffer Caching

**Status:** Future consideration  
**Created:** 2025-01-29

---

# Goals
- Avoid re-rendering expensive components when content hasn't changed
- Reduce CPU usage for "dynamic but not every frame" UI elements
- Maintain smooth rendering even with complex markdown/syntax highlighting

# Non-goals
- Caching content that changes every frame (not applicable — no cursor blink/animations)
- Architectural changes to the render pipeline
- Persistent disk caching of rendered buffers

# Design principles
- User journey drives order
- Per-component caching (input, transcript, overlays each own their cache)
- Dirty-flag invalidation on user action (keystroke, scroll, selection)
- Fits existing `RefCell` render-cache pattern from ARCHITECTURE.md

# User journey
1. User opens TUI, components render once into cached buffers
2. User does nothing → caches reused, no re-render
3. User types → input cache invalidates → input re-renders
4. User scrolls transcript → transcript cache invalidates → transcript re-renders
5. Overlays same pattern: invalidate only on interaction

# Foundations / Already shipped (✅)

## Ratatui render loop
- What exists: `render.rs` renders full frame each tick
- ✅ Demo: Run TUI, observe components render correctly
- Gaps: No caching; every frame re-renders everything

## Debug status line
- What exists: `features/statusline/` with `show_debug_status` flag
- ✅ Demo: Enable debug mode, see status info at bottom
- Gaps: None — will use this for cache instrumentation

## RefCell render caches
- What exists: Markdown wrap cache uses `RefCell` for render-time caching
- ✅ Demo: See `ARCHITECTURE.md` "Interior Mutability" section
- Gaps: None — `CachedLayer` follows same pattern

# MVP slices (ship-shaped, demoable)

## Slice 1: CachedLayer struct + status line instrumentation
- **Goal**: Create reusable `CachedLayer` with debug visibility in status line
- **Scope checklist**:
  - [ ] `CachedLayer` struct: `Buffer`, `Rect`, `dirty: bool`
  - [ ] Add `render_count: u32` and `hit_count: u32` for instrumentation
  - [ ] `invalidate()` method to mark dirty
  - [ ] `get_or_render()` method that re-renders only if dirty or resized
  - [ ] Integrate with `StatusLineAccumulator` to show cache stats
  - [ ] Display format: `[cache] in:3/45 tx:2/120` (renders/hits per component)
  - [ ] Guard behind existing `show_debug_status` flag
  - [ ] Unit tests for invalidation logic
- **✅ Demo**: Enable debug status, open TUI, see cache counters. Do nothing → hits increase, renders stay flat. Type → input render count increments by 1.
- **Risks / failure modes**:
  - Forgetting to invalidate on content change → stale UI
  - Buffer size mismatch after terminal resize

## Slice 2: Integrate with command palette
- **Goal**: Command palette uses cached rendering
- **Scope checklist**:
  - [ ] Command palette holds `CachedLayer`
  - [ ] Invalidate on filter text change or selection change
  - [ ] Invalidate on terminal resize
  - [ ] Blit cached buffer to frame in render pass
- **✅ Demo**: Open command palette, watch status line. Idle → hits increase. Type filter → render count +1 per keystroke.
- **Risks / failure modes**:
  - Invalidation triggers too often (negates benefit)

## Slice 3: Integrate with transcript
- **Goal**: Transcript uses cached rendering (biggest benefit — markdown is expensive)
- **Scope checklist**:
  - [ ] Transcript holds `CachedLayer`
  - [ ] Invalidate on new message, scroll position change, or resize
  - [ ] Consider partial caching (per-message) if full transcript cache invalidates too often
- **✅ Demo**: Scroll transcript, watch status line. Stop scrolling → hits climb, renders flat.
- **Risks / failure modes**:
  - Streaming messages cause constant invalidation (may need to skip caching during streaming)

## Slice 4: Integrate with input
- **Goal**: Input uses cached rendering
- **Scope checklist**:
  - [ ] Input holds `CachedLayer`
  - [ ] Invalidate on keystroke or cursor movement
- **✅ Demo**: Type in input → render count +1 per keystroke. Idle → hits increase.
- **Risks / failure modes**:
  - Minimal — input is simple and changes only on user action

# Contracts (guardrails)
- Cached content must always match what a fresh render would produce
- Terminal resize must invalidate all caches
- Cache dropped when component/overlay is destroyed (no memory leak)
- Status line cache display only visible when `show_debug_status` is enabled

# Key decisions (decide early)
- Per-component caching (not global) ✓
- Store `CachedLayer` in component state (fits existing `RefCell` pattern) ✓
- Instrumentation via status line (only debug option for full-screen TUI) ✓

# Testing
- Manual smoke demos per slice using status line counters
- Unit tests for `CachedLayer` invalidation logic
- Verify: idle TUI = hits increase, renders flat

# Polish phases (after MVP)

## Phase 1: Performance measurement
- Compare CPU usage before/after caching (external profiler)
- Identify if any component invalidates too frequently
- ✅ Check-in demo: Side-by-side CPU comparison idle vs typing

## Phase 2: Optimize hot paths
- If transcript invalidates too often during streaming, add "streaming mode" bypass
- Consider per-message caching for transcript if beneficial
- ✅ Check-in demo: Smooth streaming without constant re-render

# Later / Deferred
- Partial buffer invalidation (only re-render changed region) → revisit if full invalidation is too coarse
- Cache size limits / eviction → revisit if memory becomes concern
- Automatic dirty detection via content hashing → revisit if manual invalidation becomes error-prone
