# Plan: TUI Reducer Architecture

Refactor `src/ui/tui.rs` (2500 lines) into a clean reducer-based architecture that eliminates borrow-checker clones, centralizes mutations, and makes the codebase easier to change without fear.

## Goals

- Keep today's behavior (input/submit/stream/scroll/tools/overlays) unchanged
- Make render "read-only": no cloning overlay/state just to satisfy the borrow checker
- Centralize mutations behind a reducer (`update(state, event) -> Vec<Effect>`)
- Model async work as explicit effects, not inline spawns
- Make `src/ui/tui.rs` easy to change without fear

## Non-goals

- Rewriting the UX, keybindings, or transcript rendering contract
- Large renames across the project (keep public entrypoints stable)
- Premature "perfect architecture" before it pays rent
- Full automated TUI/TTY integration testing

## Design Principles

- **User journey drives order** — ship what users touch first
- **Ship-first** — preserve dogfoodability every slice
- **Runtime vs State split** — terminal ownership separate from app state
- **Reducer-first UI** — one mutation path (`update`)
- **Render reads state only** — no side effects, no clones
- **Effects are explicit** — async work returned from reducer, executed by runtime
- **Terminal safety is non-negotiable** — restore on exit/panic/ctrl-c

## Architecture Overview

```
┌─────────────────────────────────────────────────────────────┐
│ TuiRuntime (src/ui/runtime.rs)                              │
│                                                             │
│  - Owns Terminal<CrosstermBackend>                          │
│  - Owns TuiState                                            │
│  - Runs the event loop                                      │
│  - Executes Effects returned by reducer                     │
│                                                             │
│  loop {                                                     │
│      let event = poll_next_event();                         │
│      let effects = update(&mut self.state, event);          │
│      self.terminal.draw(|f| view(&self.state, f));          │
│      for effect in effects { self.execute(effect); }        │
│  }                                                          │
└─────────────────────────────────────────────────────────────┘

┌─────────────────────────────────────────────────────────────┐
│ TuiState (src/ui/state.rs)                                  │
│                                                             │
│  - transcript: Vec<HistoryCell>                             │
│  - input: TextArea                                          │
│  - scroll: ScrollState                                      │
│  - overlay: OverlayState (None | Palette | ModelPicker | Login) │
│  - engine: EngineState                                      │
│  - config, session, history, auth_type, etc.                │
│                                                             │
│  NO Terminal, NO crossterm — pure data                      │
└─────────────────────────────────────────────────────────────┘

┌─────────────────────────────────────────────────────────────┐
│ UiEvent (src/ui/events.rs)                                  │
│                                                             │
│  - Tick                                                     │
│  - Terminal(crossterm::event::Event)                        │
│  - Engine(EngineEvent)                                      │
│  - TurnFinished(Result<Vec<ChatMessage>, Error>)            │
│  - LoginResult(Result<(), String>)                          │
└─────────────────────────────────────────────────────────────┘

┌─────────────────────────────────────────────────────────────┐
│ UiEffect (src/ui/effects.rs)                                │
│                                                             │
│  - Render                                                   │
│  - Quit                                                     │
│  - StartEngineTurn { prompt, options }                      │
│  - InterruptEngine                                          │
│  - SpawnTokenExchange { code, verifier }                    │
│  - OpenBrowser { url }                                      │
│  - SaveSession { messages }                                 │
│  - PersistModel { model }                                   │
└─────────────────────────────────────────────────────────────┘

┌─────────────────────────────────────────────────────────────┐
│ update() (src/ui/update.rs) — THE REDUCER                   │
│                                                             │
│  fn update(state: &mut TuiState, event: UiEvent)            │
│      -> Vec<UiEffect>                                       │
│                                                             │
│  ALL state mutations happen here.                           │
│  Returns effects for async work — never spawns directly.    │
└─────────────────────────────────────────────────────────────┘

┌─────────────────────────────────────────────────────────────┐
│ view() (src/ui/view.rs) — PURE RENDER                       │
│                                                             │
│  fn view(state: &TuiState, frame: &mut Frame)               │
│                                                             │
│  Reads state, draws to frame. No mutations, no clones.      │
└─────────────────────────────────────────────────────────────┘
```

## Why This Fixes the Clone Problem

```rust
// BEFORE (current): TuiApp owns both terminal AND state
impl TuiApp {
    fn render(&mut self) -> Result<()> {
        // Must clone because self.terminal.draw() borrows &mut self
        // but we also need to read self.login_state, self.command_palette
        let login_state = self.login_state.clone();      // forced clone
        let palette_state = self.command_palette.clone(); // forced clone
        
        self.terminal.draw(|f| {
            render_login(f, &login_state);
            render_palette(f, &palette_state);
        })?;
        Ok(())
    }
}

// AFTER (Plan): Runtime owns terminal, State is a separate field
impl TuiRuntime {
    fn render(&mut self) -> Result<()> {
        // self.state is a different field than self.terminal
        // No borrow conflict — just pass a reference
        self.terminal.draw(|f| {
            view(&self.state, f);  // borrows state, not self
        })?;
        Ok(())
    }
}
```

## User Journey

1. Start TUI
2. Type input
3. Submit
4. See output
5. Stream updates
6. Scroll/navigate history
7. Observe tools
8. Use overlays (palette/model/login)
9. (Later) selection/copy
10. (Later) markdown/polish

## Foundations / Already Shipped (✅)

- Full-screen alt-screen TUI + raw mode + panic hook + Drop restore
    - ✅ Demo: `cargo run --` then quit with `q`; terminal returns to normal
    - Gaps: `restore_terminal()` doesn't disable bracketed paste/mouse capture
- Input + submit + engine turn spawn + session append
    - ✅ Demo: type → Enter → see assistant response
    - Gaps: state mutations spread across many methods
- Streaming + tool events + delta coalescing
    - ✅ Demo: long answer streams; tool calls show running/done
    - Gaps: event routing isn't unified (pollers + handlers + direct mutations)
- Overlays: command palette, model picker, login overlay
    - ✅ Demo: `/` opens palette, model picker opens, login flow works
    - Gaps: multiple overlay fields force "which overlay?" cascades; render clones
- Transcript model extracted (`src/ui/transcript.rs`)
    - ✅ Demo: transcript displays user/assistant/tool/system cells correctly
    - Gaps: wrapping/viewport logic still lives in `TuiApp::render()`
- Login reducer pattern exists (`LoginEvent` + `LoginState` + `update()`)
    - ✅ Demo: login flow works end-to-end
    - Gaps: only login uses this pattern; rest of app uses ad-hoc mutations

## MVP Slices

### Slice 1: Terminal Lifecycle Extraction ✅

**Goal:** Extract terminal setup/restore into a dedicated module. Guarantee terminal restore on normal exit, ctrl-c, and panic.

**Scope checklist:**
- [x] Create `src/ui/terminal.rs` with:
    - `setup_terminal() -> Result<Terminal<CrosstermBackend<Stdout>>>`
    - `restore_terminal() -> Result<()>` (now also disables bracketed paste + mouse)
    - `install_panic_hook()`
    - `enable_input_features()` / `disable_input_features()` for bracketed paste + mouse
- [x] Move terminal lifecycle code from `tui.rs` to new module
- [x] Fix gap: `restore_terminal()` must disable bracketed paste + mouse capture
- [x] Verify ctrl-c path always exits cleanly (restore_terminal handles all cleanup)

**✅ Demo:**
- Quit via `q` → terminal restored
- Quit via Ctrl+C → terminal restored
- Force a panic → terminal restored
- All three: no stuck raw mode, no stuck mouse capture, no stuck bracketed paste

**Failure modes / guardrails:**
- Any case leaving raw mode/paste/mouse enabled is a release blocker

**Files touched:** `src/ui/tui.rs`, `src/ui/terminal.rs` (new), `src/ui/mod.rs`

**Estimated size:** ~80 lines moved, ~20 lines added

---

### Slice 2: Split TuiState from TuiRuntime ✅

**Goal:** Separate app state from terminal ownership. This is the structural fix for render cloning.

**Scope checklist:**
- [x] Create `src/ui/state.rs` with `TuiState` struct containing:
    - `transcript: Vec<HistoryCell>`
    - `textarea: TextArea`
    - `scroll_offset: usize`, `follow_output: bool`
    - `command_palette: Option<CommandPaletteState>`
    - `model_picker: Option<ModelPickerState>`
    - `login_state: LoginState`
    - `engine_state: EngineState`
    - `config`, `session`, `messages`, `command_history`, etc.
- [x] Migrate initialization logic from `TuiApp::new()` and `with_history()` to `TuiState::new()` / `TuiState::with_history()`
    - Transcript building, command history setup, textarea styling
    - `TuiRuntime` should only handle terminal setup, then initialize state
- [x] Move `SLASH_COMMANDS` const and `SlashCommand` struct to `src/ui/commands.rs`
- [x] Rename `TuiApp` to `TuiRuntime`, keep only:
    - `terminal: Terminal<CrosstermBackend<Stdout>>`
    - `state: TuiState`
    - Event loop, effect execution
- [x] Change render to: `self.terminal.draw(|f| view(&self.state, f))`
- [x] Remove all `.clone()` calls that existed only for borrow-checker appeasement

**Note:** `TextArea` is not `Clone`, so `TuiState` cannot derive `Clone`. This is intentional — the plan eliminates render-time clones.

**✅ Demo:**
- All overlays render identically (palette/model/login) ✓
- No render-time state clones (verified: `rg "\.clone\(\)" src/ui/view.rs` returns nothing) ✓
- All existing functionality works unchanged (192 tests pass) ✓

**Failure modes / guardrails:**
- Any UX/keybinding drift is not allowed
- Render clones are not allowed (except for actual data copies)

**Files touched:**
- `src/ui/tui.rs`: Renamed `TuiApp` to `TuiRuntime`, moved state to separate module
- `src/ui/state.rs` (new): `TuiState`, `EngineState`, `LoginState`, `CommandPaletteState`, `ModelPickerState`, `ScrollMode`, `AuthType`
- `src/ui/view.rs` (new): `view()`, `render_header()`, `render_transcript()`, overlay render functions
- `src/ui/commands.rs` (new): `SlashCommand`, `SLASH_COMMANDS`
- `src/ui/mod.rs`: Updated exports

**Actual size:** ~1300 lines reorganized across new modules

---

### Slice 3: UiEvent Enum + Reducer Entrypoint ✅

**Goal:** Stop scattering mutations across `poll_*`, `handle_*`, and `execute_*`. One event enum, one `update()` function.

**Scope checklist:**
- [x] Create `src/ui/events.rs` with `UiEvent` enum:
    ```rust
    pub enum UiEvent {
        Tick,
        Terminal(crossterm::event::Event),
        Engine(EngineEvent),
        TurnFinished(TurnResult),  // Uses dedicated enum instead of Result
        LoginResult(Result<(), String>),
    }
    ```
- [x] Create `src/ui/effects.rs` with `UiEffect` enum (needed for reducer return type)
- [x] Create `src/ui/update.rs` with reducer:
    ```rust
    pub fn update(state: &mut TuiState, event: UiEvent, viewport_height: usize) -> Vec<UiEffect> {
        match event {
            UiEvent::Tick => { state.spinner_frame = ...; vec![] }
            UiEvent::Terminal(term_event) => handle_terminal_event(state, term_event, viewport_height),
            UiEvent::Engine(e) => { handle_engine_event(state, &e); vec![] }
            UiEvent::TurnFinished(r) => handle_turn_finished(state, r),
            UiEvent::LoginResult(r) => { handle_login_result(state, r); vec![] }
        }
    }
    ```
- [x] Migrate existing `handle_key`, `handle_mouse`, `handle_engine_event` logic into reducer
- [x] Handle `tui-textarea` input in reducer: `state.textarea.input(event)` for text input keys
    - Note: `tui-textarea` manages its own undo/redo history internally
- [x] Keep `LoginEvent` as internal to reducer (subsumed into `update_login()` helper)
- [x] Simplify `LoginState::Exchanging` to unit variant (code/verifier passed via effect)

**✅ Demo:**
- Submit/stream/login/model switch/new/quit all work ✓
- `rg "^pub fn update" src/ui/` shows one reducer ✓
- `rg "fn update_login" src/ui/` shows internal login sub-reducer ✓

**Failure modes / guardrails:**
- Avoid double-handling the same event ✓
- One mutation path only ✓

**Files touched:**
- `src/ui/events.rs` (new): `UiEvent`, `TurnResult`
- `src/ui/effects.rs` (new): `UiEffect` enum
- `src/ui/update.rs` (new): Main reducer + all event handlers
- `src/ui/tui.rs`: Refactored to collect events, call reducer, execute effects
- `src/ui/state.rs`: Simplified `LoginState::Exchanging`
- `src/ui/view.rs`: Updated for simplified `LoginState::Exchanging`
- `src/ui/mod.rs`: Added exports for new modules

**Actual size:** ~700 lines in update.rs (handlers moved from tui.rs), tui.rs reduced to ~400 lines

---

### Slice 4: Effects System ✅

**Goal:** Make engine orchestration and async work explicit effects, not mixed into UI state mutations.

**Scope checklist:**
- [x] Create `src/ui/effects.rs` with `UiEffect` enum:
    ```rust
    pub enum UiEffect {
        Quit,
        StartEngineTurn,  // Reads prompt/options from state
        InterruptEngine,
        SpawnTokenExchange { code: String, verifier: String },
        OpenBrowser { url: String },
        SaveSession { event: SessionEvent },
        PersistModel { model: String },
        CreateNewSession,  // Added for /new command
    }
    ```
- [x] Update reducer to return `Vec<UiEffect>` instead of spawning tasks directly
- [x] Add effect executor in `TuiRuntime`:
    ```rust
    fn execute_effect(&mut self, effect: UiEffect) {
        match effect {
            UiEffect::Quit => self.state.should_quit = true,
            UiEffect::StartEngineTurn => self.spawn_engine_turn(),
            UiEffect::InterruptEngine => self.interrupt_engine(),
            UiEffect::SpawnTokenExchange { code, verifier } => { ... }
            UiEffect::OpenBrowser { url } => { let _ = open::that(&url); }
            UiEffect::SaveSession { event } => { ... }
            UiEffect::PersistModel { model } => { ... }
            UiEffect::CreateNewSession => { ... }
        }
    }
    ```
- [x] Keep existing coalescing/backpressure behavior (bounded channels)

**✅ Demo:**
- Streaming remains smooth ✓
- Input stays responsive during streaming ✓
- Tool events still appear ✓
- Login flow works end-to-end ✓

**Failure modes / guardrails:**
- Blocking awaits on the UI thread is a no-go ✓
- Effects must not mutate state directly ✓

**Implementation notes:**
- `StartEngineTurn` has no parameters (cleaner than plan); reads from `state.messages` and `state.config`
- Added `CreateNewSession` effect for the `/new` command (improvement over plan)
- Effects executed synchronously in main loop; async work spawns tasks that send results back via channels

**Files touched:**
- `src/ui/effects.rs`: `UiEffect` enum with 8 variants
- `src/ui/update.rs`: All handlers return `Vec<UiEffect>`
- `src/ui/tui.rs`: `execute_effect()` and `execute_effects()` methods

**Actual size:** ~50 lines in effects.rs, effect execution integrated during Slice 3

---

### Slice 5: Scroll State Extraction

**Goal:** Keep scroll stable under streaming and reduce render complexity.

**Scope checklist:**
- [ ] Create `ScrollState` helper struct:
    ```rust
    pub struct ScrollState {
        pub offset: usize,
        pub follow_output: bool,
    }
    
    impl ScrollState {
        pub fn scroll_up(&mut self, lines: usize, max_offset: usize);
        pub fn scroll_down(&mut self, lines: usize);
        pub fn scroll_to_top(&mut self, max_offset: usize);
        pub fn scroll_to_bottom(&mut self);
        pub fn maybe_follow(&mut self, content_changed: bool);
    }
    ```
- [ ] Move viewport/offset math into `ScrollState`
- [ ] Move "visible lines" calculation into view layer (or a `TranscriptViewModel`)

**✅ Demo:**
- Scrolling during streaming doesn't jump
- Home/End/PageUp/PageDown behave as before
- Resize doesn't break scroll position

**Failure modes / guardrails:**
- Off-by-one scroll bugs on resize
- Scroll jumps when new content arrives while scrolled up

**Files touched:** `src/ui/state.rs`, `src/ui/update.rs`, `src/ui/view.rs`

**Estimated size:** ~100 lines reorganized

---

### Slice 6: Overlay Focus Model

**Goal:** Eliminate cascades like `if login/palette/picker...` with a single overlay enum.

**Scope checklist:**
- [ ] Replace separate overlay fields with unified enum:
    ```rust
    pub enum OverlayState {
        None,
        CommandPalette(CommandPaletteState),
        ModelPicker(ModelPickerState),
        Login(LoginState),
    }
    ```
- [ ] Route keys by focus: overlay first, then input
    ```rust
    fn handle_key(state: &mut TuiState, key: KeyEvent) -> Vec<UiEffect> {
        match &mut state.overlay {
            OverlayState::CommandPalette(p) => handle_palette_key(p, key),
            OverlayState::ModelPicker(m) => handle_picker_key(m, key),
            OverlayState::Login(l) => handle_login_key(l, key),
            OverlayState::None => handle_main_key(state, key),
        }
    }
    ```
- [ ] Ensure Esc consistently closes the active overlay

**✅ Demo:**
- Esc closes any active overlay
- Arrow keys do the right thing per overlay
- No "which overlay?" cascade in code

**Failure modes / guardrails:**
- Arrow-key conflicts with input/history must be resolved consistently

**Files touched:** `src/ui/state.rs`, `src/ui/update.rs`, `src/ui/view.rs`

**Estimated size:** ~150 lines reorganized

---

## Final File Structure

```
src/ui/
├── mod.rs              # Public exports
├── terminal.rs         # Terminal lifecycle (setup/restore/panic hook)
├── runtime.rs          # TuiRuntime (event loop, effect execution)
├── state.rs            # TuiState (all app state, no terminal)
├── events.rs           # UiEvent enum
├── effects.rs          # UiEffect enum
├── update.rs           # The reducer: update(state, event) -> effects
├── view.rs             # Pure render: view(&state, frame)
├── commands.rs         # SLASH_COMMANDS const + SlashCommand struct
├── transcript.rs       # (existing) Transcript model + styling
└── overlays/           # (optional) Per-overlay modules
    ├── mod.rs
    ├── palette.rs
    ├── model_picker.rs
    └── login.rs
```

## Contracts (Guardrails)

- Terminal restore always runs on exit/panic/ctrl-c (including paste/mouse toggles)
- No stdout/stderr spam corrupts the TUI while running
- Input remains responsive while streaming and tools run
- High-volume deltas never block the UI thread (coalescing preserved)
- All state mutations go through `update()` — one grep to find all changes
- `view()` never mutates state or returns effects

## Known Risks

| Risk | Mitigation |
|------|------------|
| **`JoinHandle` drop semantics** — `EngineState` owns the `JoinHandle`. When moved to `TuiState`, dropping the state aborts the running task. | Preserve current drop behavior. Be careful in reducer not to accidentally drop `EngineState` (and thus the running task) when transitioning states unless intended. |
| **`TextArea` is not `Clone`** — Cannot derive `Clone` for `TuiState`. | This is fine and intended. Plan eliminates render-time clones. No part of the design relies on cloning `TuiState`. |
| **Effect ordering** — If effect A must happen before effect B, need explicit sequencing. | For now, effects execute in returned order. Add explicit deps if needed later. |

## Key Decisions

| Decision | Choice | Rationale |
|----------|--------|-----------|
| Naming | `TuiRuntime` + `TuiState` + `UiEvent` + `UiEffect` | Minimal rename, clear roles |
| Overlay model | Single `OverlayState` enum | Eliminates cascading ifs |
| Effect execution | Synchronous in runtime loop | Keep it simple; async spawns inside executor |
| Login reducer | Subsume into main reducer or keep separate | Either works; main reducer is cleaner |
| Scroll state | Separate `ScrollState` struct inside `TuiState` | Clear ownership, testable |

## Testing

- **Manual smoke demos per slice** (the ✅ demos above)
- **Minimal regression tests** only for contracts:
    - Integration test: CLI exits cleanly and doesn't leak raw mode
    - Unit tests for pure logic: scroll clamping, command filtering, overlay transitions

## Polish Phases (After MVP)

### Phase A: Selection/Copy
- ✅ Demo: copy a transcript cell to clipboard without breaking streaming

### Phase B: Markdown/Polish
- ✅ Demo: code blocks/lists render legibly and wrap correctly

## Later / Deferred

- Mouse selection + richer interactions
- Full markdown renderer
- Automated TUI/TTY integration testing
