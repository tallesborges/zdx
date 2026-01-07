# TUI Feature-Slice Migration Plan

## Overview

Migrate `src/modes/tui/` from monolithic reducer/view to feature-slice architecture.

**Goal Structure:**
```
src/modes/tui/
├── mod.rs                    # Entry point only
├── app.rs                    # AppState, TuiState composition
├── update.rs                 # Main reducer + StateCommand dispatcher
├── render.rs                 # Main view/render composition
├── events.rs                 # UiEvent aggregator (imports from features)
├── shared/                   # Leaf types only (no feature deps)
│   ├── mod.rs
│   ├── effects.rs           # UiEffect enum (runtime side-effects)
│   ├── internal.rs          # StateCommand enum (cross-slice mutations)
│   └── commands.rs          # Command definitions for palette
├── input/                    # Input feature
│   ├── mod.rs
│   ├── state.rs
│   ├── update.rs
│   └── render.rs
├── transcript/               # Transcript feature
│   ├── mod.rs
│   ├── state.rs
│   ├── update.rs
│   ├── render.rs
│   ├── layout.rs            # Feature-local layout helpers
│   ├── selection.rs
│   ├── cell.rs
│   ├── style.rs
│   ├── wrap.rs
│   └── build.rs
├── session/                  # Session feature
│   ├── mod.rs
│   ├── state.rs
│   ├── update.rs
│   └── render.rs
├── auth/                     # Auth feature (small)
│   ├── mod.rs
│   ├── state.rs
│   ├── update.rs
│   └── render.rs
├── overlays/                 # Overlay feature
│   ├── mod.rs
│   ├── update.rs
│   ├── render.rs
│   ├── session_picker.rs    # SessionPickerState stays here
│   └── ...
├── runtime/                  # Runtime (emits UiEvents, no direct mutations)
│   ├── mod.rs
│   ├── handlers.rs
│   └── handoff.rs
└── markdown/                 # (unchanged)
```

**Key Architecture Decisions:**
- `shared/` contains ONLY leaf types with no feature dependencies
- `events.rs` at root level contains `UiEvent` aggregator
- `shared/internal.rs` contains `StateCommand` for cross-slice mutations
- Feature handlers take ONLY their slice of state, return `StateCommand` for cross-slice
- Runtime emits `UiEvent`s instead of mutating state directly (Elm-like)
- Each feature owns its layout helpers (no shared layout module)

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

## Slice 9: StateCommand Infrastructure ✅

**Goal:** Create `StateCommand` enum and dispatcher for cross-slice mutations.

**Rationale:** Before enforcing strict isolation, we need infrastructure to handle
cross-slice mutations. Currently reducers directly mutate foreign state (e.g., input
reducer pushes to transcript). StateCommand provides a clean way to express these
as data that the main dispatcher applies.

**Files Created:**
- `shared/internal.rs` - StateCommand enum with feature-grouped variants

**Tasks:**
- [x] Create `shared/internal.rs` with `StateCommand` enum
- [x] Add feature-specific command enums: `TranscriptCommand`, `InputCommand`, `SessionCommand`
- [x] Update `shared/mod.rs` to export internal module
- [x] Add `apply_state_commands()` function to root `reducer.rs`
- [x] Run `cargo check` - no warnings
- [x] Run `cargo test` - all tests pass
- [x] Run `cargo clippy` - no warnings
- [x] Commit: `refactor(tui): add StateCommand infrastructure for cross-slice mutations`

**Completed:** 2025-01-05

**StateCommand Design:**
```rust
// shared/internal.rs

/// Commands for cross-slice state mutations.
/// Feature reducers return these instead of mutating foreign state directly.
#[derive(Debug)]
pub enum StateCommand {
    Transcript(TranscriptCommand),
    Input(InputCommand),
    Session(SessionCommand),
    Auth(AuthCommand),
}

#[derive(Debug)]
pub enum TranscriptCommand {
    AppendCell(HistoryCell),
    AppendSystemMessage(String),
    ClearCells,
    ReplaceCells(Vec<HistoryCell>),
}

#[derive(Debug)]
pub enum InputCommand {
    Clear,
    SetText(String),
    SetHandoffState(HandoffState),
}

#[derive(Debug)]
pub enum SessionCommand {
    ClearMessages,
    SetMessages(Vec<ChatMessage>),
    SetSession(Option<Session>),
    UpdateUsage { input: u64, output: u64 },
}

#[derive(Debug)]
pub enum AuthCommand {
    SetStatus(AuthStatus),
}
```

**Dispatcher Pattern:**
```rust
// reducer.rs (or update.rs after rename)

fn apply_state_commands(tui: &mut TuiState, commands: Vec<StateCommand>) {
    for cmd in commands {
        match cmd {
            StateCommand::Transcript(tc) => apply_transcript_command(&mut tui.transcript, tc),
            StateCommand::Input(ic) => apply_input_command(&mut tui.input, ic),
            StateCommand::Session(sc) => apply_session_command(&mut tui.thread, sc),
            StateCommand::Auth(ac) => apply_auth_command(&mut tui.auth, ac),
        }
    }
}
```

**Risk:** Low (additive, no behavior changes yet)
**Duration:** ~1 hour

---

## Slice 10: Overlay StateCommand Refactor ✅

**Goal:** Refactor overlay handlers to return `StateCommand` instead of mutating `TuiState`.

**Rationale:** Overlays (especially `SessionPickerState`) currently take `&mut TuiState`
and mutate transcript/session directly. This violates slice isolation and creates
coupling. By returning `StateCommand`, overlays become pure functions.

**Files Modified:**
- `overlays/session_picker.rs` - Refactor `handle_key` to return `(OverlayAction, Vec<StateCommand>)`
- `overlays/command_palette.rs` - Refactor to return StateCommand
- `overlays/model_picker.rs` - Refactor to return StateCommand
- `overlays/file_picker.rs` - Refactor to return StateCommand
- `overlays/mod.rs` - Update `OverlayExt` trait signature
- `reducer.rs` - Update to apply returned StateCommands

**Tasks:**
- [x] Update `OverlayExt::handle_key` signature to return `(Option<Vec<UiEffect>>, Vec<StateCommand>)`
- [x] Refactor `SessionPickerState::handle_key` - return commands instead of mutating
- [x] Refactor `CommandPaletteState::handle_key` - return commands instead of mutating
- [x] Refactor `ModelPickerState::handle_key` - return commands instead of mutating
- [x] Refactor `FilePickerState::handle_key` - return commands instead of mutating
- [x] Update main reducer to apply StateCommands after overlay handling
- [x] Run `cargo check` - no warnings
- [x] Run `cargo test` - all tests pass
- [x] Run `cargo clippy` - no warnings
- [x] Commit: `refactor(tui): overlay handlers return StateCommand instead of mutating TuiState`

**Completed:** 2025-01-05

**Before:**
```rust
impl SessionPickerState {
    pub fn handle_key(&mut self, tui: &mut TuiState, key: KeyEvent) -> Option<Vec<UiEffect>> {
        // Directly mutates tui.transcript, tui.thread
        tui.transcript.cells = self.original_cells.clone();
        // ...
    }
}
```

**After:**
```rust
impl SessionPickerState {
    pub fn handle_key(&mut self, key: KeyEvent) -> (OverlayAction, Vec<StateCommand>) {
        let mut commands = vec![];
        commands.push(StateCommand::Transcript(
            TranscriptCommand::ReplaceCells(self.original_cells.clone())
        ));
        (OverlayAction::Close, commands)
    }
}
```

**Risk:** Medium (behavior must remain identical)
**Duration:** ~2 hours

---

## Slice 11: Runtime UiEvent Refactor ✅

**Goal:** Refactor runtime handlers to emit `UiEvent` instead of mutating state directly.

**Rationale:** Currently `runtime/handoff.rs` and `runtime/handlers.rs` mutate `TuiState`
directly (e.g., `tui.transcript.cells.push(...)`, `tui.agent_state = ...`). This bypasses
the reducer and violates Elm architecture. Runtime should emit events that flow through
the reducer.

**Files Modified:**
- `runtime/handlers.rs` - Return events instead of mutating
- `runtime/handoff.rs` - Return events instead of mutating
- `runtime/mod.rs` - Dispatch returned events through reducer
- `events.rs` (or `core/events.rs`) - Add new event variants if needed

**Tasks:**
- [x] Audit all direct state mutations in `runtime/handlers.rs`
- [x] Audit all direct state mutations in `runtime/handoff.rs`
- [x] Add new `UiEvent` variants for runtime operations (e.g., `UiEvent::AgentSpawned`, `UiEvent::HandoffStarted`)
- [x] Refactor `spawn_agent_turn` to return/emit event instead of mutating
- [x] Refactor `spawn_handoff_generation` to return/emit event instead of mutating
- [x] Update runtime event loop to dispatch returned events through reducer
- [x] Run `cargo check` - no warnings
- [x] Run `cargo test` - all tests pass
- [x] Run `cargo clippy` - no warnings
- [x] Commit: `refactor(tui): runtime emits UiEvent instead of direct state mutations`

**Completed:** 2025-01-05

**New Event Variants:**
```rust
pub enum UiEvent {
    // ... existing variants ...
    
    // New runtime events
    AgentSpawned { rx: mpsc::Receiver<AgentEvent> },
    HandoffGenerationStarted { goal: String },
    HandoffGenerationComplete { prompt: String },
}
```

**Risk:** Medium-High (runtime is critical path)
**Duration:** ~2-3 hours

---

## Slice 12: Strict Isolation Refactor ✅

**Goal:** Refactor all feature reducers to take only their slice and return `StateCommand`.

**Rationale:** With StateCommand infrastructure in place, we can now enforce strict
isolation. Each feature reducer takes only its own state and returns commands for
cross-slice mutations.

**Sub-slices (one commit each):**

### 12a: Auth Isolation (~30 min) ✅
- [x] Refactor `auth/reducer.rs` to take `&mut AuthState` only
- [x] Return `Vec<StateCommand>` for transcript updates
- [x] Update main reducer to pass only auth state
- [x] Run validation suite
- [x] Commit: `refactor(tui): auth reducer strict isolation`

### 12b: Transcript Isolation (~1 hour) ✅
- [x] Refactor `transcript/update.rs` handlers to take `&mut TranscriptState` only
- [x] Pass `&AgentState` as read-only dependency where needed
- [x] Return `Vec<StateCommand>` for any session/input updates
- [x] Move layout helpers to `transcript/layout.rs` (feature-local)
- [x] Update main reducer delegation
- [x] Run validation suite
- [x] Commit: `refactor(tui): transcript reducer strict isolation`

### 12c: Session Isolation (~1.5 hours) ✅
- [x] Refactor `session/reducer.rs` to take `&mut SessionState` only
- [x] Return `Vec<StateCommand>` for transcript/input updates
- [x] Remove direct `overlays` imports (use StateCommand instead)
- [x] Update main reducer delegation
- [x] Run validation suite
- [x] Commit: `refactor(tui): session reducer strict isolation`

### 12d: Input Isolation (~2 hours) - LARGEST ✅
- [x] Refactor `input/reducer.rs` to take `&mut InputState` only
- [x] Return `Vec<StateCommand>` for transcript/session/overlay updates
- [x] Handle overlay interactions via StateCommand
- [x] Update main reducer delegation
- [x] Run validation suite
- [x] Commit: `refactor(tui): input reducer strict isolation`

**Completed:** 2025-01-05

**Handler Signature After:**
```rust
// input/update.rs
pub fn handle_main_key(
    input: &mut InputState,
    agent_state: &AgentState,  // read-only dependency
    key: KeyEvent,
) -> (Vec<UiEffect>, Vec<StateCommand>)
```

**Risk:** High (largest refactor, many touch points)
**Duration:** ~5 hours total

---

## Slice 13: Delete core/, Merge events.rs ✅

**Goal:** Flatten structure by removing `core/` directory and consolidating events.

**Rationale:** The `core/` directory only contains `events.rs`. Simpler to have
`events.rs` at root level. This also resolves the conflict with the existing
`events.rs` re-export shim.

**Tasks:**
- [x] Audit all imports of `crate::modes::tui::core::events`
- [x] Move `core/events.rs` content to root `events.rs` (merge with shim)
- [x] Update all imports to use `crate::modes::tui::events`
- [x] Delete `core/mod.rs`
- [x] Delete `core/` directory
- [x] Run `cargo check` - no warnings
- [x] Run `cargo test` - all tests pass
- [x] Run `cargo clippy` - no warnings
- [x] Commit: `refactor(tui): flatten structure, delete core/ directory`

**Completed:** 2025-01-05

**Import Changes:**
```rust
// Before
use crate::modes::tui::core::events::{UiEvent, SessionUiEvent};

// After
use crate::modes::tui::events::{UiEvent, SessionUiEvent};
```

**Risk:** Low (mechanical refactor)
**Duration:** ~30 min

---

## Slice 14: Rename reducer.rs/view.rs ✅

**Goal:** Standardize naming to `update.rs`/`render.rs` across all modules.

**Rationale:** Elm convention uses update/render. Some modules already use this
(transcript/update.rs, transcript/render.rs). Standardize for consistency.

**Files Renamed:**
- `reducer.rs` → `update.rs` (root)
- `view.rs` → `render.rs` (root)
- `input/reducer.rs` → `input/update.rs`
- `input/view.rs` → `input/render.rs`
- `session/reducer.rs` → `session/update.rs`
- `session/view.rs` → `session/render.rs`
- `auth/reducer.rs` → `auth/update.rs`
- `auth/view.rs` → `auth/render.rs`

**Tasks:**
- [x] Rename root `reducer.rs` → `update.rs`
- [x] Rename root `view.rs` → `render.rs`
- [x] Rename `input/reducer.rs` → `input/update.rs`
- [x] Rename `input/view.rs` → `input/render.rs`
- [x] Rename `session/reducer.rs` → `session/update.rs`
- [x] Rename `session/view.rs` → `session/render.rs`
- [x] Rename `auth/reducer.rs` → `auth/update.rs`
- [x] Rename `auth/view.rs` → `auth/render.rs`
- [x] Update all `mod.rs` declarations
- [x] Update all internal imports
- [x] Run `cargo check` - no warnings
- [x] Run `cargo test` - all tests pass
- [x] Run `cargo clippy` - no warnings
- [x] Commit: `refactor(tui): rename reducer/view to update/render for Elm consistency`

**Completed:** 2025-01-05

**Note:** `transcript/update.rs`, `transcript/render.rs`, `overlays/update.rs` already
follow this convention - no changes needed.

**Risk:** Low (mechanical rename)
**Duration:** ~45 min

---

## Slice 15: Delete state/ Shims, Relocate Tests ✅

**Goal:** Remove backward-compatibility shims and move tests to feature modules.

**Rationale:** The `state/` directory now only contains re-export shims pointing to
feature modules. These add indirection. Tests in `state/mod.rs` need relocation.

**Tasks:**
- [x] Audit tests in `state/mod.rs` - identify what they test
- [x] Move scroll/selection tests to `transcript/state.rs` or `transcript/selection.rs`
- [x] Move any input tests to `input/state.rs`
- [x] Move any session tests to `session/state.rs`
- [x] Update all imports from `crate::modes::tui::state::X` to feature modules
- [x] Delete `state/auth.rs` shim
- [x] Delete `state/input.rs` shim
- [x] Delete `state/session.rs` shim
- [x] Delete `state/transcript.rs` shim
- [x] Delete `state/mod.rs`
- [x] Delete `state/` directory
- [x] Run `cargo check` - no warnings
- [x] Run `cargo test` - all tests pass
- [x] Run `cargo clippy` - no warnings
- [x] Commit: `refactor(tui): remove state/ shims, relocate tests to features`

**Completed:** 2025-01-05

**Import Changes:**
```rust
// Before
use crate::modes::tui::state::{InputState, TranscriptState};

// After  
use crate::modes::tui::input::InputState;
use crate::modes::tui::transcript::TranscriptState;
```

**Risk:** Medium (must ensure no external consumers of shim paths)
**Duration:** ~1 hour

---

## Slice 16: Documentation Updates ✅

**Goal:** Update all documentation to reflect new architecture.

**Tasks:**
- [x] Update `AGENTS.md` "Where things are" section with new file structure
- [x] Update `docs/ARCHITECTURE.md` with:
  - [x] New module layout diagram
  - [x] StateCommand pattern documentation
  - [x] Updated Elm architecture description
  - [x] Cross-slice communication flow
- [x] Add Feature Slice Contract section to ARCHITECTURE.md
- [x] Update code comments in key files
- [x] Run full test suite
- [x] Manual smoke test: `cargo run -- chat`
- [x] Commit: `docs(tui): update architecture documentation for feature slices`

**Completed:** 2025-01-06

**Feature Slice Contract (for ARCHITECTURE.md):**
```rust
/// Every feature module MUST:
/// 1. Export state type: `pub use state::FeatureState`
/// 2. Export update function: `pub fn handle_*(state: &mut FeatureState, ...) -> (Vec<UiEffect>, Vec<StateCommand>)`
/// 3. Export render function: `pub fn render_*(state: &FeatureState, frame: &mut Frame, ...)`
///
/// Update functions:
/// - Take ONLY their own state as mutable
/// - Take dependencies as read-only references
/// - Return UiEffect for runtime side-effects
/// - Return StateCommand for cross-slice mutations
///
/// Render functions:
/// - Take state as immutable reference
/// - Never mutate state or return effects
```

**Risk:** None
**Duration:** ~1 hour

---

## Migration Order Summary (Revised)

```
Slice 0-16: All work completed              ✅ DONE
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

**Per-Slice Rollback Notes:**
- Slice 9-10: Safe to revert independently (additive)
- Slice 11: Revert requires also reverting any dependent slices
- Slice 12a-d: Can revert individual sub-slices
- Slice 13-15: Mechanical, easy to revert
- Slice 16: Docs only, safe to revert

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
Slices 0-8 (Previous) ✅
    │
    ▼
Slice 9 (StateCommand Infrastructure)
    │
    ├─────────────────┬─────────────────┐
    ▼                 ▼                 ▼
Slice 10          Slice 11          (can start 12a)
(Overlays)        (Runtime)
    │                 │
    └────────┬────────┘
             ▼
    Slice 12 (Strict Isolation)
    ├── 12a (Auth)      ← smallest, do first
    ├── 12b (Transcript)
    ├── 12c (Session)
    └── 12d (Input)     ← largest, do last
             │
             ▼
    Slice 13 (Delete core/)
             │
             ▼
    Slice 14 (Rename files)
             │
             ▼
    Slice 15 (Delete state/ shims)
             │
             ▼
    Slice 16 (Documentation)
```

---

## Critical Risk Mitigations

### 1. Circular Dependency Prevention
```
✅ DO: Feature events in root events.rs
   events.rs → UiEvent, SessionUiEvent
   
✅ DO: StateCommand in shared/internal.rs
   shared/internal.rs → StateCommand (leaf type, no deps)
   
✅ DO: Overlay state stays in overlays/
   overlays/session_picker.rs → SessionPickerState
   
❌ DON'T: Feature imports from other features
   input/update.rs → session::SessionState  ← Creates cycles!
```

### 2. StateCommand Pattern
```rust
// ✅ DO: Return commands for cross-slice mutations
fn handle_submit(input: &mut InputState) -> (Vec<UiEffect>, Vec<StateCommand>) {
    let text = input.get_text();
    input.clear();
    (
        vec![UiEffect::SpawnAgent],
        vec![StateCommand::Transcript(TranscriptCommand::AppendCell(
            HistoryCell::user(&text)
        ))]
    )
}

// ❌ DON'T: Mutate foreign state directly
fn handle_submit(input: &mut InputState, transcript: &mut TranscriptState) {
    transcript.cells.push(...);  // Violates isolation!
}
```

### 3. Runtime Event Pattern
```rust
// ✅ DO: Runtime emits events
fn spawn_agent_turn(tui: &TuiState) -> UiEvent {
    let rx = /* spawn agent */;
    UiEvent::AgentSpawned { rx }
}

// ❌ DON'T: Runtime mutates state
fn spawn_agent_turn(tui: &mut TuiState) {
    tui.agent_state = AgentState::Running(rx);  // Bypasses reducer!
}
```

### 4. Visibility Cascade
When moving structs to sub-modules:
- Private fields may need `pub(crate)`
- Add getters/setters if encapsulation is important
- Run `cargo check` frequently during moves

### 5. Integration Test Imports
Before Slice 15, check:
```bash
grep -r "modes::tui::state::" tests/
grep -r "modes::tui::state::" src/
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
pub use state::FeatureState;

// Update function signature
pub fn handle_feature_event(
    state: &mut FeatureState,           // Own state (mutable)
    deps: &Dependencies,                 // Read-only dependencies
    event: FeatureEvent,
) -> (Vec<UiEffect>, Vec<StateCommand>)

// Render function signature  
pub fn render_feature(
    state: &FeatureState,               // Read-only
    frame: &mut Frame,
    area: Rect,
)
```

**StateCommand Usage:**
- Return `StateCommand` for ANY mutation outside your slice
- Main dispatcher applies commands after your handler returns
- Commands are applied in order, atomically

**Read-Only Dependencies:**
- Pass other feature state as `&State` (immutable) when needed
- Never mutate dependencies - use StateCommand instead
- Keep dependency list minimal
