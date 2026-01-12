# TUI Architecture Refactor — Implementation Plan

## Goals
- All async workflow receivers stored via reducer events (not direct runtime mutation)
- Session ops follow the same Elm-clean pattern as Agent/Login/Handoff
- Command application moved into each slice (`state.apply(cmd)`)
- Reducer owns all overlay transitions (effects reserved for I/O/spawning only)
- Clearer naming: `StateCommand` → `StateMutation`, `core::session` → `core::thread_log`, `SessionState` → `ThreadState`
- No observable behavior changes for users

## Non-goals
- Moving receivers out of state (they remain feature-owned)
- Backward compatibility with existing session files
- Helper methods for receiver access
- Mitigating one-frame timing shifts

## Design principles
- **Minimal churn**: One concern at a time; each slice independently testable
- **Preserve existing patterns**: Match `AgentSpawned` / `LoginExchangeStarted` / `HandoffGenerationStarted`
- **Slice autonomy**: Each feature slice owns its state mutations via `apply()`
- **Reducer orchestrates**: Cross-slice mutations coordinated by reducer, not compound commands

---

# Release 1: Architectural Cleanup ✅ COMPLETE

## MVP Slices 1-5: Session `*Started` Events ✅ COMPLETE

**Goal**: Replace direct runtime mutation with event dispatch for all session operations.

### Slice 1: `SessionListStarted`
- [x] Add `SessionUiEvent::ListStarted { rx }` variant in `events.rs`
- [x] Change `handlers::spawn_session_list_load` to return `UiEvent`
- [x] Update `UiEffect::OpenSessionPicker` handler to `dispatch_event(result)`
- [x] Handle `SessionUiEvent::ListStarted` in reducer: `session_ops.list_rx = Some(rx)`
- [x] Handle `SessionUiEvent::ListLoaded` in reducer: `session_ops.list_rx = None`
- [x] Handle `SessionUiEvent::ListFailed` in reducer: `session_ops.list_rx = None`
- [x] Remove `ops.list_rx = None` from `collect_session_results`
- **✅ Smoke**: Ctrl+P → session picker loads and displays sessions

### Slice 2: `SessionLoadStarted`
- [x] Add `SessionUiEvent::LoadStarted { rx }` variant
- [x] Change `handlers::spawn_session_load` to return `UiEvent`
- [x] Update `UiEffect::LoadSession` handler to `dispatch_event`
- [x] Handle in reducer: store rx, clear on `Loaded`/`LoadFailed`
- [x] Remove cleanup from `collect_session_results`
- **✅ Smoke**: Select session in picker → transcript switches

### Slice 3: `SessionPreviewStarted`
- [x] Add `SessionUiEvent::PreviewStarted { rx }` variant
- [x] Change `handlers::spawn_session_preview` to return `UiEvent`
- [x] Update `UiEffect::PreviewSession` handler to `dispatch_event`
- [x] Handle in reducer: store rx, clear on `PreviewLoaded`/`PreviewFailed`
- [x] Remove cleanup from `collect_session_results`
- **✅ Smoke**: Arrow through sessions → preview updates

### Slice 4: `SessionCreateStarted`
- [x] Add `SessionUiEvent::CreateStarted { rx }` variant
- [x] Change `handlers::spawn_session_create` to return `UiEvent`
- [x] Update `UiEffect::CreateNewSession` handler to `dispatch_event`
- [x] Handle in reducer: store rx, clear on `Created`/`CreateFailed`
- [x] Remove cleanup from `collect_session_results`
- **✅ Smoke**: `/new` → new session created, path shown

### Slice 5: `SessionRenameStarted`
- [x] Add `SessionUiEvent::RenameStarted { rx }` variant
- [x] Change `handlers::spawn_session_rename` to return `UiEvent`
- [x] Update `UiEffect::RenameSession` handler to `dispatch_event`
- [x] Handle in reducer: store rx, clear on `Renamed`/`RenameFailed`
- [x] Remove cleanup from `collect_session_results`
- **✅ Smoke**: `/rename My Session` → confirmation shown

---

## Phase 2: Move Command Application into Slices ✅ COMPLETE

**Goal**: Each slice owns its `apply()` method; reducer orchestrates cross-slice mutations.

- [x] Add `impl TranscriptState { pub fn apply(&mut self, cmd: TranscriptCommand) }` — move match arms from `apply_transcript_command`
- [x] Add `impl InputState { pub fn apply(&mut self, cmd: InputCommand) }` — move from `apply_input_command`
- [x] Add `impl SessionState { pub fn apply(&mut self, cmd: SessionCommand) }` — move from `apply_session_command`
- [x] Add `impl AuthState { pub fn apply(&mut self, cmd: AuthCommand) }` — move from `apply_auth_command`
- [x] Simplify `apply_state_commands()` to just route:
  ```rust
  match cmd {
      StateCommand::Transcript(c) => tui.transcript.apply(c),
      StateCommand::Input(c) => tui.input.apply(c),
      StateCommand::Session(c) => tui.thread.apply(c),
      StateCommand::Auth(c) => tui.auth.apply(c),
      StateCommand::Config(c) => apply_config_command(tui, c),
  }
  ```
- [x] Remove `apply_*_command()` helper functions from `update.rs`
- **✅ Demo**: Add a new `TranscriptCommand` variant — only edit `transcript/` module

---

## Phase 3: Reducer Owns Overlay Transitions ✅ COMPLETE

**Goal**: Remove overlay-opening effects; reducer sets `app.overlay` directly. Effects reserved for I/O and task spawning only.

- [x] Remove `UiEffect::OpenCommandPalette` — reducer sets `app.overlay = Some(Overlay::CommandPalette(...))` directly
- [x] Remove `UiEffect::OpenModelPicker` — reducer sets overlay directly
- [x] Remove `UiEffect::OpenThinkingPicker` — reducer sets overlay directly
- [x] Remove `UiEffect::OpenLogin` — reducer sets overlay directly
- [x] Refactor `UiEffect::OpenFilePicker`:
  - Reducer sets `app.overlay = Some(Overlay::FilePicker(...))`
  - Reducer returns `UiEffect::DiscoverFiles` (I/O effect remains)
- [x] Remove `TuiRuntime::set_overlay()` helper method
- [x] Update `UiEffect` doc comment: "Effects are I/O and task spawning only"
- **✅ Demo**: Ctrl+K opens command palette; `@` opens file picker; model picker works

---

## Release 1: Documentation ✅ COMPLETE

- [x] Update `AGENTS.md` "Where things are" section
- [x] Update `docs/ARCHITECTURE.md` with:
  - Receiver lifecycle (Started event → reducer stores → runtime polls → result event → reducer clears)
  - Slice `apply()` pattern
  - "Effects are I/O only" rule
  - Cross-slice mutation orchestration

---

# Release 2: Naming Cleanup

## Phase 4a: Rename `StateCommand` → `StateMutation`

**Goal**: Clear vocabulary — mutations (sync state changes) vs effects (async/I/O).

- [ ] Rename in `shared/internal.rs`:
  - `StateCommand` → `StateMutation`
  - `TranscriptCommand` → `TranscriptMutation`
  - `InputCommand` → `InputMutation`
  - `SessionCommand` → `SessionMutation`
  - `AuthCommand` → `AuthMutation`
  - `ConfigCommand` → `ConfigMutation`
- [ ] Rename `apply_state_commands()` → `apply_mutations()`
- [ ] Update all imports and references
- [ ] Update doc comments to use "mutation" terminology
- **✅ Demo**: `cargo test` passes; grep confirms no `StateCommand` references

---

## Phase 4b: Rename `core::session` → `core::thread_log`

**Goal**: Persistence types describe what they are (a log file), not the user concept.

### Module & Types
- [ ] Rename `src/core/session.rs` → `src/core/thread_log.rs`
- [ ] Update `src/core/mod.rs` exports
- [ ] Rename types:
  - `Session` → `ThreadLog`
  - `SessionEvent` → `ThreadEvent`
  - `SessionSummary` → `ThreadSummary`

### Functions
- [ ] `list_sessions` → `list_threads`
- [ ] `load_session` → `load_thread_events`
- [ ] `latest_session_id` → `latest_thread_id`
- [ ] `set_session_title` → `set_thread_title`
- [ ] `short_session_id` → `short_thread_id`
- [ ] `events_to_messages` → `thread_events_to_messages`
- [ ] `extract_usage_from_events` → `extract_usage_from_thread_events`
- [ ] `spawn_persist_task` → `spawn_thread_persist_task`

### CLI
- [ ] Rename `SessionPersistenceOptions` → `ThreadPersistenceOptions`
- [ ] Rename field `session_id` → `thread_id`
- [ ] Rename CLI flags: `--session` → `--thread`, `--session-id` → `--thread-id`, `--no-session` → `--no-thread`
- [ ] Update CLI help text

- **✅ Demo**: `cargo test` passes; `grep -r "core::session" src/` returns nothing

---

## Phase 4c: Rename TUI `SessionState` → `ThreadState`

**Goal**: TUI state describes the thread, persistence handle uses "thread" terminology.

- [ ] Rename `SessionState` → `ThreadState` in `src/modes/tui/session/state.rs`
- [ ] Rename field `session: Option<Session>` → `thread_log: Option<ThreadLog>`
- [ ] Update `TuiState.thread` type annotation
- [ ] Rename `SessionMutation` → `ThreadMutation` (from Phase 4a)
- [ ] Rename `SessionUiEvent` → `ThreadUiEvent`
- [ ] Remove thread ops state in favor of task lifecycle tracking
- [ ] Update all imports and references across codebase
- **✅ Demo**: No `session.session` pattern; `cargo test` passes

---

## Release 2: Documentation

- [ ] Update `AGENTS.md` with all new paths and type names
- [ ] Update `docs/ARCHITECTURE.md` with new terminology
- [ ] Add release notes documenting:
  - Breaking change: existing session files incompatible
  - Breaking change: CLI flags renamed (`--session` → `--thread`)

---

# Contracts (Guardrails)

1. **Fast poll when ops pending**: `tasks.is_any_running()` is source of truth for runtime poll duration
2. **Receiver cleanup in reducer**: All `*_rx = None` happens in reducer when handling result events (success and failure)
3. **Guard against double-spawn**: `if *_rx.is_none()` checks remain in effect handlers before spawning
4. **No behavior change**: All operations work identically (except one-frame timing shift, accepted as imperceptible)
5. **Effects are I/O only**: After Phase 3, `UiEffect` is reserved for I/O operations and task spawning; overlay transitions happen in reducer
6. **Slice autonomy**: Each slice owns its `apply()` method; cross-slice coordination happens in reducer

---

# Testing

### Per-Slice Smoke Tests (Minimal) ✅ COMPLETE
- [x] Open session picker (Ctrl+P) → sessions load and display
- [x] Switch to a session → transcript updates
- [x] Create new session (/new) → new session created, path shown
- [x] Rename session (/rename X) → confirmation shown

### Regression ✅ COMPLETE
- [x] `cargo test` passes after each slice/phase
- [x] `cargo clippy` passes after each slice/phase

---

# Summary

| Release | Phases | Scope | Status |
|---------|--------|-------|--------|
| **Release 1** | MVP 1-5, Phase 2, Phase 3 | Architectural cleanup (event-driven receivers, slice apply, reducer owns overlays) | ✅ **COMPLETE** |
| **Release 2** | Phase 4a, 4b, 4c | Naming cleanup (StateMutation, thread_log, ThreadState) | ❌ Not started |
