# TUI Feature-Slice Migration Plan

## Overview

Migrate `src/modes/tui/` from monolithic reducer/view to feature-slice architecture.

**Goal Structure:**
```
src/modes/tui/
├── mod.rs                    # Entry point only
├── app.rs                    # AppState, TuiState composition
├── shared/                   # Leaf types only (no feature deps)
│   ├── mod.rs
│   ├── effects.rs           # UiEffect enum
│   └── commands.rs          # Command definitions
├── core/                     # Top-level dispatch + aggregator events
│   ├── mod.rs               # update() dispatcher
│   ├── events.rs            # UiEvent (aggregates feature events)
│   └── render.rs            # render() layout + composition
├── input/                    # Input feature
│   ├── mod.rs
│   ├── state.rs
│   ├── update.rs
│   ├── render.rs
│   └── handoff.rs
├── transcript/               # Transcript feature
│   ├── mod.rs
│   ├── state.rs
│   ├── update.rs
│   ├── render.rs
│   ├── scroll.rs
│   ├── selection.rs
│   ├── cell.rs
│   ├── style.rs
│   ├── wrap.rs
│   └── build.rs
├── session/                  # Session feature
│   ├── mod.rs
│   ├── state.rs
│   ├── events.rs            # SessionUiEvent (feature-specific)
│   ├── update.rs
│   └── usage.rs
├── auth/                     # Auth feature (small)
│   ├── mod.rs
│   └── state.rs
├── overlays/                 # (unchanged)
├── runtime/                  # (unchanged)
└── markdown/                 # (unchanged)
```

**Key Architecture Decisions:**
- `shared/` contains ONLY leaf types with no feature dependencies
- `core/events.rs` contains `UiEvent` aggregator (imports from features)
- Feature-specific events live in feature modules (e.g., `session/events.rs`)
- Feature handlers take ONLY their slice of state, not `&mut TuiState`

**Naming Conventions:**
- Reducer: `update()` entry, `handle_*()` private handlers
- View: `render()` entry, `render_*()` sub-components
- Mutations: `apply_*` (deltas), `set_*` (direct), `ensure_*`/`clamp_*` (invariants)
- Predicates: `should_*`, `is_*`, `has_*`

---

## Slice 0: Preparation (Foundation) ✅

**Goal:** Create directory structure, no functional changes.

**Tasks:**
- [x] Create empty directories: `shared/`, `core/`, `input/`, `session/`, `auth/`
- [x] Create placeholder `mod.rs` files with `// TODO: migrate` comments
- [x] Run `cargo check` to ensure no breakage
- [x] Commit: `chore(tui): create feature-slice directory structure`

**Files Created:**
```
shared/mod.rs       (empty, declares future modules)
core/mod.rs         (empty)
input/mod.rs        (empty)
session/mod.rs      (empty)
auth/mod.rs         (empty)
```

**Completed:** 2025-01-04  
**Commit:** `85b4e3e`

**Risk:** None  
**Duration:** ~10 min

---

## Slice 1: Shared Types (Leaf Types Only) ✅

**Goal:** Move ONLY leaf types to `shared/`. NOT `UiEvent` (it has feature deps).

**Tasks:**
- [x] Move `effects.rs` → `shared/effects.rs`
- [x] Move `commands.rs` → `shared/commands.rs`
- [x] Create `shared/mod.rs` with re-exports
- [x] **DO NOT move `events.rs` yet** (it contains `UiEvent` which depends on features)
- [x] Update imports for effects and commands
- [x] Update `mod.rs` to declare `shared` module and re-export for backward compat
- [x] Run `cargo test`
- [x] Commit: `refactor(tui): move leaf types to shared/ module`

**Completed:** 2025-01-04

**Risk:** Low  
**Duration:** ~15 min

---

## Slice 2: Core Events Module ✅

**Goal:** Create core/ with UiEvent aggregator.

**Tasks:**
- [x] Create `core/events.rs` with `UiEvent` and `SessionUiEvent`
- [x] Create `core/mod.rs` with re-exports
- [x] Keep old `events.rs` as re-export shim for backward compat
- [x] Run `cargo test`

**Completed:** 2025-01-04

**Risk:** Low  
**Duration:** ~30 min

---

## Slice 3: Input Feature Module ✅

**Goal:** Extract input state, keyboard handling, and handoff logic into feature slice.

**Tasks:**
- [x] Create `input/state.rs` with InputState and HandoffState
- [x] Create `input/reducer.rs` with key handling, submit, handoff result
- [x] Create `input/view.rs` with render_input, render_handoff_input, build_usage_display
- [x] Create `input/mod.rs` with re-exports
- [x] Update `state/input.rs` to re-export from input feature
- [x] Update main reducer to delegate input key handling to input::handle_main_key
- [x] Update main view to delegate input rendering to input::render_input
- [x] Run `cargo check` - no warnings
- [x] Run `cargo test` - all 293 tests pass
- [x] Commit: `chore(tui): extract input feature slice`

**Completed:** 2025-01-05

**Risk:** Medium  
**Duration:** ~1.5 hours

---

## Slice 4: Auth Feature Module ✅

**Goal:** Extract auth state and login overlay logic into a dedicated feature slice.

**Files Created/Modified:**
- `auth/state.rs` - Moved `AuthStatus` and `AuthState` from `state/auth.rs`
- `auth/reducer.rs` - Moved `handle_login_result` from `overlays/login.rs`
- `auth/view.rs` - Moved `render_login_overlay` and `truncate_middle` from `overlays/login.rs`
- `auth/mod.rs` - Updated with re-exports
- `state/auth.rs` - Thin re-export shim for backward compatibility
- `overlays/login.rs` - Updated to use auth feature, removed duplicated code
- `overlays/mod.rs` - Updated to re-export from auth feature

**Tasks:**
- [x] Move `state/auth.rs` → `auth/state.rs`
- [x] Create `auth/reducer.rs` with `handle_login_result`
- [x] Create `auth/view.rs` with `render_login_overlay`
- [x] Create `auth/mod.rs` with re-exports
- [x] Update `state/auth.rs` to re-export from auth feature
- [x] Update `overlays/login.rs` to use auth feature
- [x] Update `overlays/mod.rs` to re-export from auth feature
- [x] Run `cargo check` - no warnings
- [x] Run `cargo test` - all 296 tests pass
- [x] Run `cargo clippy` - no warnings
- [x] Commit: `chore(tui): extract auth feature slice`

**Completed:** 2025-01-05

**Pattern Established:**
```rust
// auth/mod.rs
mod state;
mod reducer;
mod view;

pub use state::{AuthState, AuthStatus};
pub use reducer::handle_login_result;
pub use view::render_login_overlay;
```

**Risk:** Low  
**Duration:** ~20 min

---

## Slice 5: Session Feature Module ✅

**Goal:** Extract session state, events, and update logic.

**Files Created/Modified:**
- `session/state.rs` - Moved `SessionState`, `SessionOpsState`, `SessionUsage` from `state/session.rs`
- `session/reducer.rs` - Extracted session event handlers from `reducer.rs`
- `session/view.rs` - Extracted `render_session_picker` from `overlays/session_picker.rs`
- `session/mod.rs` - Updated with re-exports
- `state/session.rs` - Thin re-export shim for backward compatibility
- `overlays/session_picker.rs` - Updated to delegate rendering to session feature
- `reducer.rs` - Removed session handlers, delegates to `session::handle_session_event`

**Tasks:**
- [x] Move `state/session.rs` → `session/state.rs`
- [x] Create `session/reducer.rs` with session event handlers
- [x] Create `session/view.rs` with `render_session_picker`
- [x] Create `session/mod.rs` with re-exports
- [x] Update `state/session.rs` to re-export from session feature
- [x] Update `overlays/session_picker.rs` to use session feature view
- [x] Update main reducer to delegate session events to `session::handle_session_event`
- [x] Run `cargo check` - no warnings
- [x] Run `cargo test` - all 296 tests pass
- [x] Run `cargo clippy` - no warnings
- [x] Update `AGENTS.md` with new structure
- [x] Commit: `chore(tui): extract session feature slice`

**Completed:** 2025-01-05

**Pattern Established:**
```rust
// session/mod.rs
mod state;
mod reducer;
mod view;

pub use state::{SessionOpsState, SessionState, SessionUsage};
pub use reducer::handle_session_event;
pub use view::render_session_picker;
```

**Risk:** Medium  
**Duration:** ~45 min

---

## Slice 6: Transcript Feature (Largest) ✅

**Goal:** Extract transcript - the largest feature slice.

**Current Files:**
- `state/transcript.rs` (564 lines) → `transcript/state.rs`
- `transcript/cell.rs` (1279 lines) → stays, already in place
- `transcript/wrap.rs` (372 lines) → stays
- `transcript/style.rs` → stays
- `selection.rs` (508 lines) → `transcript/selection.rs`
- `transcript_build.rs` (253 lines) → `transcript/build.rs`
- Parts of `reducer.rs` → `transcript/update.rs`
- Parts of `view.rs` → `transcript/render.rs`

**Tasks:**

### 6a: Move State Files (~45 min) ✅
- [x] Move `state/transcript.rs` → `transcript/state.rs`
- [x] Move `selection.rs` → `transcript/selection.rs`
- [x] Move `transcript_build.rs` → `transcript/build.rs`
- [x] Update `transcript/mod.rs` with new modules
- [x] Add `pub(crate)` visibility where needed
- [x] Update imports
- [x] Run `cargo test`
- [x] Commit: `refactor(tui): move transcript state files`

### 6b: Extract Update Logic (~1 hour) ✅
- [x] Extract from `reducer.rs`:
  - `handle_agent_event()`
  - `apply_pending_delta()`
  - `apply_scroll_delta()`
  - `handle_mouse()`
  - `screen_to_transcript_pos()`
  → `transcript/update.rs`
- [x] Update main reducer to delegate agent events to `transcript::handle_agent_event`
- [x] Update imports
- [x] Run `cargo test`
- [x] Commit: `refactor(tui): extract transcript update logic`

### 6c: Extract Render Logic (~1 hour) ✅
- [x] Extract from `view.rs`:
  - `render_transcript()`
  - `render_transcript_full()`
  - `render_transcript_lazy()`
  - `convert_styled_line()`
  - `convert_styled_line_with_selection()`
  - `convert_style()`
  - `calculate_cell_line_counts()`
  → `transcript/render.rs`
- [x] Update main view to call `transcript::render_transcript`
- [x] Update imports
- [x] Run `cargo test`
- [x] Commit: `refactor(tui): extract transcript render logic`

**Completed:** 2025-01-XX

**Pattern Established:**
```rust
// transcript/mod.rs
mod state;
mod selection;
mod build;
mod update;
mod render;

pub use state::{TranscriptState, VisibleRange};
pub use selection::{LineMapping, SelectionState};
pub use build::build_transcript_from_events;
pub use update::{apply_pending_delta, apply_scroll_delta, handle_agent_event, handle_mouse};
pub use render::{calculate_cell_line_counts, render_transcript, SPINNER_SPEED_DIVISOR};

// Also re-export existing sub-modules
pub use cell::{CellId, HistoryCell, ToolState};
pub use wrap::WrapCache;
pub use style::{Style, StyledLine, StyledSpan};
```

**Risk:** High (largest file, most dependencies)  
**Duration:** ~2.5 hours (split into 3 sub-commits)

---

## Slice 7: Overlays Feature Module ✅

**Goal:** Extract overlay state and logic into a dedicated feature slice.

**Files Created/Modified:**
- `overlays/update.rs` - Created with `handle_overlay_key` and `handle_files_discovered`
- `overlays/mod.rs` - Added `OverlayExt` trait for `Option<Overlay>` convenience methods
- `reducer.rs` - Updated to use `overlays::handle_overlay_key` and `overlays::handle_files_discovered`
- `view.rs` - Updated to use `OverlayExt::render` trait method

**Tasks:**

### 7a: Add OverlayExt Trait (~30 min) ✅
- [x] Add `OverlayExt` trait to `overlays/mod.rs`
- [x] Implement `handle_key()` method for `Option<Overlay>`
- [x] Implement `render()` method for `Option<Overlay>`
- [x] Update `reducer.rs` to use trait
- [x] Update `view.rs` to use trait
- [x] Run `cargo check` - no warnings
- [x] Run `cargo test` - all 300 tests pass
- [x] Commit: `refactor(tui): add OverlayExt trait for cleaner overlay handling`

### 7b: Extract Update Logic (~30 min) ✅
- [x] Create `overlays/update.rs` with:
  - `handle_overlay_key()` - wrapper function for reducer
  - `handle_files_discovered()` - moved from reducer.rs
- [x] Add re-exports in `overlays/mod.rs`
- [x] Update `reducer.rs` to use `overlays::handle_overlay_key`
- [x] Update `reducer.rs` to use `overlays::handle_files_discovered`
- [x] Run `cargo check` - no warnings
- [x] Run `cargo test` - all 300 tests pass
- [x] Run `cargo clippy` - no warnings
- [x] Commit: `refactor(tui): extract overlay update logic to dedicated module`

**Completed:** 2025-01-XX

**Pattern Established:**
```rust
// overlays/mod.rs
mod update;
pub mod view;
// ... overlay sub-modules ...

pub use update::{handle_files_discovered, handle_overlay_key};

// OverlayExt trait for Option<Overlay>
pub trait OverlayExt {
    fn handle_key(&mut self, tui: &mut TuiState, key: KeyEvent) -> Option<Vec<UiEffect>>;
    fn render(&self, frame: &mut Frame, area: Rect, input_y: u16);
}
```

**Risk:** Low  
**Duration:** ~1 hour

---

## Slice 8: App State Consolidation ✅

**Goal:** Create `app.rs` with clean AppState composition.

**Tasks:**
- [x] Create `app.rs` with `AppState` and `TuiState` definitions
- [x] Move `AgentState` to `app.rs` (closely related to state composition)
- [x] Move helper functions (`get_git_branch`, `shorten_path`) to `app.rs`
- [x] Update `state/mod.rs` to only re-export from `app.rs` and feature modules
- [x] Add `#[allow(unused_imports)]` for backward compat re-exports
- [x] Run `cargo check` - no warnings
- [x] Run `cargo test` - all 300 tests pass
- [x] Run `cargo clippy` - no warnings
- [x] Update `AGENTS.md` with new `app.rs` module
- [x] Commit: `refactor(tui): consolidate app state into dedicated module`

**Files Created/Modified:**
- `app.rs` - Created with `AppState`, `TuiState`, `AgentState`, and startup helpers
- `state/mod.rs` - Reduced to pure re-export hub (tests remain for backward compat)
- `mod.rs` - Added `pub mod app;` declaration
- `AGENTS.md` - Added `app.rs` to "Where things are"

**Completed:** 2025-01-XX

**Risk:** Low  
**Duration:** ~30 min

---

## Slice 9: Cleanup & Documentation

**Goal:** Remove old files, update documentation.

**Tasks:**
- [ ] Delete old `reducer.rs` (should be empty or just re-exports)
- [ ] Delete old `view.rs` (should be empty or just re-exports)
- [ ] Delete old `events.rs` (moved to core/)
- [ ] Delete old `state/mod.rs` if migrated
- [ ] Grep for external usages: `grep -r "modes::tui::" tests/`
- [ ] Fix any broken integration test imports
- [ ] Run `cargo clippy` and fix warnings
- [ ] Update `AGENTS.md` with new structure
- [ ] Update `docs/ARCHITECTURE.md` with new module layout
- [ ] Add feature slice contract documentation
- [ ] Run full test suite
- [ ] Commit: `docs(tui): update architecture for feature slices`

**Risk:** Low  
**Duration:** ~45 min

---

## Migration Order Summary (Revised)

```
Slice 0: Preparation          [~10 min]  ████ ✅ DONE
Slice 1: Shared (leaf only)   [~15 min]  ██████ ✅ DONE
Slice 2: Core Events          [~30 min]  ████████████ ✅ DONE
Slice 3: Input Feature        [~90 min]  ████████████████████████████████████ ✅ DONE
Slice 4: Auth Feature         [~20 min]  ████████ ✅ DONE
Slice 5: Session Feature      [~45 min]  ██████████████████ ✅ DONE
Slice 6: Transcript Feature   [~180 min] ████████████████████████████████████████████████████████████████████████ ✅ DONE
Slice 7: Overlays Feature     [~60 min]  ████████████████████████ ✅ DONE
Slice 8: App State            [~30 min]  ████████████ ✅ DONE
Slice 9: Cleanup & Docs       [~45 min]  ██████████████████
                              ─────────
                              ~9 hours total (with 1.5x buffer)
```

---

## Rollback Strategy

Each slice is a separate commit. To rollback:
```bash
git revert <commit-hash>
```

If mid-slice and tests fail:
```bash
git checkout -- src/modes/tui/
```

---

## Validation Checklist (Run After Each Slice)

```bash
# Must pass after every slice
cargo check
cargo test
cargo clippy -- -D warnings

# Optional: manual smoke test
cargo run -- chat
```

---

## Dependencies Between Slices (Revised)

```
Slice 0 (Prep)
    │
    ▼
Slice 1 (Shared - leaf types only)
    │
    ▼
Slice 2 (Auth)
    │
    ▼
Slice 3 (Session + SessionUiEvent)
    │
    ▼
Slice 4 (Core - UiEvent aggregator) ← MOVED UP to prevent Frankenstein reducer
    │
    ├──────────────┐
    ▼              ▼
Slice 5 (Input)    Slice 6 (Transcript)  [can be parallel after Core]
    │              │
    └──────┬───────┘
           ▼
    Slice 7 (Overlays)
           │
           ▼
    Slice 8 (App State)
           │
           ▼
    Slice 9 (Cleanup)
```

---

## Critical Risk Mitigations

### 1. Circular Dependency Prevention
```
✅ DO: Feature events in feature modules
   session/events.rs → SessionUiEvent
   
✅ DO: Aggregator in core/
   core/events.rs → UiEvent (imports from features)
   
❌ DON'T: Put UiEvent in shared/
   shared/events.rs → UiEvent  ← Creates cycles!
```

### 2. Borrow Checker Friendly Handlers
```rust
// ✅ DO: Take only what you need
fn handle_session_event(
    session: &mut SessionState,
    transcript: &mut TranscriptState,
    event: SessionUiEvent,
) -> Vec<UiEffect>

// ❌ DON'T: Take entire parent state
fn handle_session_event(
    tui: &mut TuiState,  // Causes borrow conflicts!
    event: SessionUiEvent,
) -> Vec<UiEffect>
```

### 3. Visibility Cascade
When moving structs to sub-modules:
- Private fields may need `pub(crate)`
- Add getters/setters if encapsulation is important
- Run `cargo check` frequently during moves

### 4. Integration Test Imports
Before Slice 8, check:
```bash
grep -r "modes::tui::" tests/
grep -r "use crate::modes::tui" src/
```

---

## Feature Slice Contract

Every feature module MUST export:

```rust
// feature/mod.rs
mod state;
mod update;
mod render;

// Required exports
pub use state::State;                    // The feature's state struct
pub fn update(...) -> Vec<UiEffect>;     // State transition
pub fn render(...);                       // Rendering

// Optional exports
pub use events::FeatureEvent;            // If feature has its own events
```

Handler signatures MUST take only their slice:
```rust
pub fn update(
    state: &mut FeatureState,           // Own state (mutable)
    other: &OtherState,                  // Dependencies (prefer read-only)
    event: FeatureEvent,
) -> Vec<UiEffect>
```
