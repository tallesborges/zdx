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

## Slice 4: Auth Feature (Smallest - Pattern Validation)

**Goal:** Extract auth as first feature slice to validate the pattern.

**Current Files:**
- `state/auth.rs` (53 lines) → `auth/state.rs`

**Tasks:**
- [ ] Move `state/auth.rs` → `auth/state.rs`
- [ ] Create `auth/mod.rs` with re-exports
- [ ] Update `state/mod.rs` to use `crate::modes::tui::auth::AuthState`
- [ ] Add `pub(crate)` visibility where needed
- [ ] Run `cargo test`
- [ ] Commit: `refactor(tui): extract auth feature slice`

**Pattern Established:**
```rust
// auth/mod.rs
mod state;
pub use state::{AuthState, AuthStatus};

// auth/state.rs
pub struct AuthState { ... }
pub enum AuthStatus { ... }
impl AuthState { ... }
```

**Risk:** Low  
**Duration:** ~15 min

---

## Slice 3: Session Feature

**Goal:** Extract session state, events, and update logic.

**Current Files:**
- `state/session.rs` (261 lines) → `session/state.rs`
- `SessionUiEvent` from `events.rs` → `session/events.rs`
- Parts of `reducer.rs` (session handlers) → `session/update.rs`
- Usage display helpers from `view.rs` → `session/usage.rs`

**Tasks:**
- [ ] Move `state/session.rs` → `session/state.rs`
- [ ] Extract `SessionUiEvent` from `events.rs` → `session/events.rs`
- [ ] Create `session/mod.rs` with re-exports
- [ ] Extract session-related functions from `reducer.rs`:
  - `handle_session_list_loaded()`
  - `handle_session_loaded()`
  - `handle_session_preview_loaded()`
  - `handle_session_created()`
  - `handle_session_renamed()`
  → `session/update.rs`
- [ ] Extract from `view.rs`:
  - `build_usage_display()`
  - `build_token_breakdown()`
  → `session/usage.rs`
- [ ] Update old `events.rs` to import from `session/events.rs`
- [ ] Add `pub(crate)` visibility where needed
- [ ] Run `cargo test`
- [ ] Commit: `refactor(tui): extract session feature slice`

**New Structure:**
```rust
// session/mod.rs
mod state;
mod events;
mod update;
mod usage;

pub use state::{SessionState, SessionOpsState, SessionUsage};
pub use events::SessionUiEvent;
pub use update::handle_session_event;
pub use usage::{render_usage_display, render_token_breakdown};

// session/update.rs - handlers take ONLY session state
pub fn handle_session_event(
    session: &mut SessionState,
    transcript: &mut TranscriptState,  // explicit dependency
    event: SessionUiEvent,
) -> Vec<UiEffect>
```

**Risk:** Medium (first reducer split, first feature-specific event)  
**Duration:** ~1 hour

---

## Slice 4: Core Dispatcher (Moved Earlier!)

**Goal:** Create core/ with update dispatcher and UiEvent aggregator BEFORE extracting more features.

**Why now?** Prevents `reducer.rs` from becoming a "Frankenstein" dispatcher as we extract features.

**Tasks:**
- [ ] Create `core/events.rs` with `UiEvent` enum (imports from `session/events.rs`)
- [ ] Move remaining event types from old `events.rs` → `core/events.rs`
- [ ] Delete old `events.rs` (or keep as re-export shim)
- [ ] Create `core/mod.rs` with skeleton `update()` dispatcher
- [ ] Create `core/render.rs` with skeleton `render()` 
- [ ] Wire `core::update` to call `session::handle_session_event`
- [ ] Update `runtime/mod.rs` to use `core::update` and `core::render`
- [ ] Keep old `reducer.rs` and `view.rs` for remaining logic (will shrink in later slices)
- [ ] Run `cargo test`
- [ ] Commit: `refactor(tui): create core dispatcher with UiEvent aggregator`

**New Structure:**
```rust
// core/events.rs - the aggregator
use crate::modes::tui::session::SessionUiEvent;

pub enum UiEvent {
    Tick,
    Frame { width: u16, height: u16 },
    Terminal(CrosstermEvent),
    Agent(AgentEvent),
    Session(SessionUiEvent),  // imported from session/
    LoginResult(Result<(), String>),
    HandoffResult(Result<String, String>),
    FilesDiscovered(Vec<PathBuf>),
}

// core/mod.rs
mod events;
mod render;

pub use events::UiEvent;
pub use render::render;

pub fn update(app: &mut AppState, event: UiEvent) -> Vec<UiEffect> {
    match event {
        UiEvent::Session(e) => {
            session::handle_session_event(
                &mut app.tui.conversation,
                &mut app.tui.transcript,
                e,
            )
        }
        // Other events still delegate to old reducer.rs for now
        _ => crate::modes::tui::reducer::update_legacy(app, event),
    }
}
```

**Risk:** Medium  
**Duration:** ~1 hour

---

## Slice 5: Input Feature

**Goal:** Extract input state, keyboard handling, handoff logic.

**Current Files:**
- `state/input.rs` (212 lines) → `input/state.rs`
- Handoff types from `state/input.rs` → `input/handoff.rs`
- Parts of `reducer.rs` → `input/update.rs`
- Parts of `view.rs` → `input/render.rs`

**Tasks:**
- [ ] Move `state/input.rs` → `input/state.rs`
- [ ] Extract `HandoffState` and related → `input/handoff.rs`
- [ ] Create `input/mod.rs`
- [ ] Extract from `reducer.rs`:
  - `handle_main_key()`
  - `handle_paste()`
  - `submit_input()`
  - History navigation functions
  - Handoff result handler
  → `input/update.rs`
- [ ] Extract from `view.rs`:
  - `render_input()`
  - `render_handoff_input()`
  - `wrap_textarea()`
  - `calculate_input_height()`
  → `input/render.rs`
- [ ] Update `core/mod.rs` to dispatch keyboard events to `input::handle_key`
- [ ] Add `pub(crate)` visibility where needed
- [ ] Run `cargo test`
- [ ] Commit: `refactor(tui): extract input feature slice`

**Handler Signature (Avoid Borrow Conflicts):**
```rust
// input/update.rs - take only what you need
pub fn handle_key(
    input: &mut InputState,
    transcript: &mut TranscriptState,  // for adding cells
    session: &SessionState,            // read-only check
    key: KeyEvent,
) -> Vec<UiEffect>

// NOT this (causes borrow conflicts):
// pub fn handle_key(tui: &mut TuiState, key: KeyEvent)
```

**Risk:** Medium  
**Duration:** ~1.5 hours

---

## Slice 6: Transcript Feature (Largest)

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

### 6a: Move State Files (~45 min)
- [ ] Move `state/transcript.rs` → `transcript/state.rs`
- [ ] Move `selection.rs` → `transcript/selection.rs`
- [ ] Move `transcript_build.rs` → `transcript/build.rs`
- [ ] Update `transcript/mod.rs` with new modules
- [ ] Add `pub(crate)` visibility where needed
- [ ] Update imports
- [ ] Run `cargo test`
- [ ] Commit: `refactor(tui): move transcript state files`

### 6b: Extract Update Logic (~1 hour)
- [ ] Extract from `reducer.rs`:
  - `handle_agent_event()`
  - `apply_pending_delta()`
  - `apply_scroll_delta()`
  - `handle_mouse()`
  - `screen_to_transcript_pos()`
  - `handle_handoff_result()`
  - `handle_files_discovered()`
  → `transcript/update.rs`
- [ ] Update `core/mod.rs` to dispatch agent events to `transcript::handle_agent_event`
- [ ] Update imports
- [ ] Run `cargo test`
- [ ] Commit: `refactor(tui): extract transcript update logic`

### 6c: Extract Render Logic (~1 hour)
- [ ] Extract from `view.rs`:
  - `render_transcript()`
  - `render_transcript_full()`
  - `render_transcript_lazy()`
  - `convert_styled_line()`
  - `convert_styled_line_with_selection()`
  - `convert_style()`
  - `calculate_cell_line_counts()`
  → `transcript/render.rs`
- [ ] Update `core/render.rs` to call `transcript::render`
- [ ] Update imports
- [ ] Run `cargo test`
- [ ] Commit: `refactor(tui): extract transcript render logic`

**Handler Signatures:**
```rust
// transcript/update.rs
pub fn handle_agent_event(
    transcript: &mut TranscriptState,
    agent_state: &mut AgentState,
    event: &AgentEvent,
) -> Vec<UiEffect>

pub fn handle_mouse(
    transcript: &mut TranscriptState,
    mouse: MouseEvent,
)
```

**Risk:** High (largest file, most dependencies)  
**Duration:** ~3 hours (split into 3 sub-commits)

---

## Slice 7: App State Consolidation

**Goal:** Create `app.rs` with clean AppState composition.

**Tasks:**
- [ ] Create `app.rs` with `AppState` and `TuiState` definitions
- [ ] Move remaining state composition from `state/mod.rs`
- [ ] Update `state/mod.rs` to only re-export from feature modules
- [ ] Consider removing `state/` directory if empty
- [ ] Run `cargo test`
- [ ] Commit: `refactor(tui): consolidate app state`

**Risk:** Low  
**Duration:** ~30 min

---

## Slice 8: Cleanup & Documentation

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
Slice 1: Shared (leaf only)   [~15 min]  ██████
Slice 2: Auth Feature         [~15 min]  ██████
Slice 3: Session Feature      [~60 min]  ████████████████████████
Slice 4: Core Dispatcher      [~60 min]  ████████████████████████  ← MOVED UP
Slice 5: Input Feature        [~90 min]  ████████████████████████████████████
Slice 6: Transcript Feature   [~180 min] ████████████████████████████████████████████████████████████████████████
Slice 7: App State            [~30 min]  ████████████
Slice 8: Cleanup & Docs       [~45 min]  ██████████████████
                              ─────────
                              ~8.5 hours total (with 1.5x buffer)
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
    Slice 7 (App State)
           │
           ▼
    Slice 8 (Cleanup)
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
