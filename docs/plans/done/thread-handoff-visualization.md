# Thread Handoff Visualization Plan

## Inputs

- **Project/feature**: Add hierarchical visualization to the threads overview showing when a thread is a handoff from another thread. Display tree-like structure with indentation, `[handoff]` labels, and branch characters (`└──`).
- **Existing state**: Threads exist with `ThreadSummary` (id, title, root_path, modified). Handoff feature creates new threads but doesn't track parent relationships. Thread picker (`Ctrl+T`) displays flat list with title and timestamp.
- **Constraints**: Must preserve backward compatibility with existing threads (no parent_id). TUI only (ratatui). No changes to exec mode.
- **Success looks like**: User can see at a glance which threads originated from handoffs, navigate the tree in thread picker, and understand thread lineage.

---

# Goals

- Show parent-child relationships between threads in the thread picker
- Display `[handoff]` label for threads created via handoff
- Use tree indentation and branch characters (`└──`) for visual hierarchy
- Support arbitrary nesting depth (handoff of handoff)

# Non-goals

- Collapsible tree nodes (future)
- Filtering by handoff vs non-handoff
- Showing handoff lineage in transcript view
- CLI commands to query handoff relationships

# Design principles

- User journey drives order
- Ship ugly-but-functional first
- Backward compatible with existing threads (missing `handoff_from` = root thread)
- Minimal data model changes
- **Low-drift structures**: Store flat `ThreadSummary` list as source of truth; derive tree view on-demand (per ARCHITECTURE.md)
- **Elm/MVU separation**: All I/O (thread creation, file writes) happens in runtime effect handlers, never in reducers

# User journey

1. User has existing threads and performs a `/handoff`
2. New thread is created with link to source thread
3. User opens thread picker (`Ctrl+T`)
4. User sees threads displayed with tree structure showing handoff relationships
5. User navigates up/down through flattened tree (depth-first order)
6. User selects a handoff thread and resumes it

# Foundations / Already shipped (✅)

## Thread persistence
- What exists: `ThreadLog` with JSONL format, `ThreadEvent::Meta` with `schema_version`, `title`, `root_path`
- ✅ Demo: `zdx threads list` shows existing threads
- Gaps: No `handoff_from` field in meta

## Handoff feature
- What exists: `/handoff` command creates new thread, generates context via subagent
- ✅ Demo: Run `/handoff`, enter goal, new thread starts
- Gaps: Doesn't record which thread it came from

## Thread picker
- What exists: `ThreadPickerState`, flat list rendering, `ThreadSummary`
- ✅ Demo: Press `Ctrl+T`, see thread list with titles and timestamps
- Gaps: No tree structure, no handoff indicators

---

# MVP slices (ship-shaped, demoable)

## Slice 1: Persist handoff_from in meta

- Goal: New handoffs record their parent thread ID
- Scope checklist:
  - [x] Add `handoff_from: Option<String>` to `ThreadEvent::Meta` (zdx-core)
  - [x] Add `handoff_from: Option<String>` to `ThreadSummary` (zdx-core)
  - [x] Create `ThreadLog::new_with_root_and_source(root, handoff_from)` constructor (zdx-core)
  - [x] Update `read_meta_handoff_from()` helper similar to `read_meta_title` (zdx-core)
  - [x] Update `list_threads()` to populate `handoff_from` from meta (zdx-core)
  - [x] Update `execute_handoff_submit` in `runtime/handoff.rs` to accept source thread ID from current session and pass to new constructor
    - **Note**: This is a UiEffect handler (runtime I/O boundary), not reducer code - preserves Elm/MVU separation
  - [x] Update `UiEffect::HandoffSubmit` to carry `handoff_from: Option<String>`
- ✅ Demo: Perform handoff, check thread JSONL has `handoff_from` in meta line
- Risks / failure modes:
  - Existing threads won't have `handoff_from` (acceptable - treated as root)
  - Schema version bump not needed (optional field)

## Slice 2: Derive thread tree for display

- Goal: Transform flat thread list into displayable tree structure **on-demand** (not stored as parallel state)
- Scope checklist:
  - [x] Create `ThreadDisplayItem` struct in `zdx-tui/src/features/thread/`: `{ summary: &ThreadSummary, depth: usize, is_handoff: bool }`
  - [x] Create pure function `fn flatten_as_tree(threads: &[ThreadSummary]) -> Vec<ThreadDisplayItem>` 
    - Groups threads by `handoff_from`, builds depth-first flattened view
    - Returns borrowed references to summaries (no cloning)
  - [x] Handle orphans (parent deleted) as root-level threads
  - [x] **Keep `ThreadPickerState.all_threads: Vec<ThreadSummary>` as source of truth** (no `Vec<ThreadNode>`)
  - [x] Call `flatten_as_tree()` in render or cache result in `ThreadPickerState` render cache when summaries change
    - **Note**: Implemented as `visible_tree_items()` method computing on-demand (no cache needed for MVP)
  - [ ] ~~Add `tree_cache: RefCell<Option<Vec<ThreadDisplayItem>>>` to picker state for render-time caching~~ (deferred - on-demand computation is sufficient)
- ✅ Demo: Unit test showing `flatten_as_tree` produces correct depth-first order with depths
- Risks / failure modes:
  - Circular references (defensive: detect and break cycle, log warning)
    - **Note**: Cycles result in empty output (threads excluded) - acceptable for malformed data
  - Cache invalidation (mitigate: clear cache when `all_threads` changes)
    - **Note**: No cache implemented - computed on-demand each render

## Slice 3: Render tree structure in thread picker

- Goal: Visual tree display with indentation and handoff labels
- Scope checklist:
  - [x] Update `render_thread_picker` to get flattened tree view (from cache or computed)
  - [x] Iterate `ThreadDisplayItem` for rendering, using `depth` for indentation
  - [x] Add indentation based on `depth` (e.g., 2 spaces per level, capped at 4 levels = 8 chars max)
  - [x] Add branch character `└──` before child threads (threads where `depth > 0`)
  - [x] Add `[handoff]` label for threads with `is_handoff == true`
  - [x] Adjust text truncation to account for indentation width
  - [x] **Selection mapping**: Selected index maps directly to flattened tree order; no separate mapping needed since we iterate the same derived list
- ✅ Demo: Perform handoff, open thread picker, see child thread indented under parent
- Risks / failure modes:
  - Deep nesting truncates title too much (mitigate: cap visual indent at 4 levels)
  - Alignment issues with Unicode box-drawing chars (mitigate: test in multiple terminals)

## Slice 4: Current thread indicator

- Goal: Show which thread is currently active in the picker
- Scope checklist:
  - [x] Pass current thread ID to `ThreadPickerState`
  - [x] Add `(current)` suffix for thread matching current session
  - [x] Style `(current)` with distinct color (e.g., cyan)
- ✅ Demo: Open thread picker while in a thread, see `(current)` marker
- Risks / failure modes:
  - None significant

---

# Contracts (guardrails)

- Existing threads without `handoff_from` must still load and display correctly
- Thread picker must remain navigable with up/down keys (depth-first order)
- Handoff feature must not regress (new thread creation, context generation)
- Thread resume must work for both parent and child threads

# Key decisions (decide early)

1. **Field name**: `handoff_from` vs `parent_id` vs `source_thread_id`
   - Decision: `handoff_from` (specific to handoff origin; allows future `fork_from` for different lineage types)
   
2. **Tree storage in state**: Store tree structure vs derive on-demand
   - Decision: **Derive on-demand** from flat `ThreadSummary` list (source of truth). Use render-time cache (`RefCell<Option<...>>`) to avoid recomputing every frame. This follows the "low-drift structures" principle from ARCHITECTURE.md.

3. **Branch character style**: `└──` vs `├──` + `│`
   - Decision: `└──` only for MVP (simpler, matches mockup). Polish phase adds `├──` for middle children.

4. **I/O boundary**: Where does thread creation happen?
   - Decision: All thread file I/O happens in `runtime/handoff.rs` effect handler, never in reducers. Reducer only emits `UiEffect::HandoffSubmit` with data.

# Testing

- Manual smoke demos per slice:
  - Slice 1: Create handoff, `cat` the jsonl file, verify `handoff_from`
  - Slice 2: Add unit test for `flatten_as_tree` (correct order, depths, orphan handling)
  - Slice 3: Visual inspection in thread picker
  - Slice 4: Visual inspection of current marker
- Minimal regression tests:
  - Test that threads without `handoff_from` deserialize correctly
  - Test tree flattening with orphan threads (parent deleted)
  - Test cache invalidation when thread list changes

# Polish phases (after MVP)

## Phase 1: Visual polish
- [ ] Use `├──` for middle children, `└──` only for last child
- [ ] Add subtle dimming for deeply nested items
- [ ] Show count of children in collapsed form (future prep)
- ✅ Check-in: Thread picker looks polished with multiple nested handoffs

## Phase 2: Navigation improvements
- [ ] Jump to parent thread with keybind (e.g., `p`)
- [ ] Highlight thread lineage when selecting a handoff
- ✅ Check-in: Can navigate up the handoff chain easily

# Later / Deferred

- **Collapsible tree nodes**: Would need expand/collapse state per node. Revisit when users report deep trees are unwieldy.
- **Visited indicator**: `(visited)` marker from mockup. Requires tracking which threads user has opened. Revisit when requested.
- **CLI commands for lineage**: `zdx threads lineage <id>`. Revisit when scripting use cases emerge.
- **Delete cascading**: Whether deleting parent should warn about children. Revisit when delete feature is added.
