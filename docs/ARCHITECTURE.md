# zdx Architecture

This document describes the architectural patterns used in zdx, with a focus on the TUI's Elm-like architecture.

## Overview

zdx is a terminal-based AI coding assistant built in Rust. The codebase is organized into several layers:

```
src/
├── main.rs              # Binary entrypoint
├── config.rs            # Configuration loading
├── default_config.toml  # Default configuration template
├── models.rs            # Model registry for TUI picker
├── models_generated.rs  # Generated model data
├── app/                 # CLI parsing + command dispatch
├── bin/
│   └── generate_models.rs  # Binary to generate model data from API
├── core/                # UI-agnostic domain logic
│   ├── agent.rs         # Agent loop + event channels
│   ├── context.rs       # Project context (AGENTS.md)
│   ├── events.rs        # Agent event types
│   ├── interrupt.rs     # Signal handling
│   └── session.rs       # Session persistence
├── providers/           # API clients (Anthropic, OAuth)
│   ├── mod.rs           # Provider module exports
│   ├── oauth.rs         # OAuth token storage
│   └── anthropic/       # Anthropic API client
├── tools/               # Tool implementations (bash, edit, read, write)
└── ui/                  # Terminal UI
    ├── exec.rs          # Non-interactive exec mode
    ├── markdown/        # Markdown parsing and wrapping
    ├── transcript/      # Transcript model (cells, wrapping)
    └── chat/            # Interactive TUI (Elm architecture)
```

## TUI Architecture (Elm-Like)

The interactive chat TUI (`src/ui/chat/`) follows the **Elm Architecture** (TEA), also known as Model-View-Update (MVU). This is a unidirectional data flow pattern that makes state management predictable and testable.

### Core Principle

> All state lives in one place. All state mutations happen through the reducer. All side effects are explicit.

The only exception is **render-time caches** which use interior mutability (`RefCell`) for performance. These caches (`wrap_cache`, `position_map`) don't affect logical state—they're pure memoization of expensive computations.

### Components

```
┌─────────────────────────────────────────────────────────────────┐
│                         TuiRuntime                               │
│                                                                  │
│  ┌──────────┐    ┌──────────┐    ┌──────────┐                   │
│  │ Terminal │    │  Agent   │    │  Async   │                   │
│  │  Events  │    │  Events  │    │  Tasks   │                   │
│  └────┬─────┘    └────┬─────┘    └────┬─────┘                   │
│       │               │               │                          │
│       └───────────────┼───────────────┘                          │
│                       ▼                                          │
│               ┌──────────────┐                                   │
│               │   UiEvent    │                                   │
│               └──────┬───────┘                                   │
│                      ▼                                           │
│         ┌────────────────────────────┐                           │
│         │     reducer::update()      │                           │
│         │  &mut AppState × UiEvent   │                           │
│         │      → Vec<UiEffect>       │                           │
│         └─────────────┬──────────────┘                           │
│                       │                                          │
│             ┌─────────┴─────────┐                                │
│             ▼                   ▼                                │
│        ┌─────────┐        ┌──────────┐                           │
│        │AppState │        │UiEffect[]│                           │
│        │(mutated)│        │          │                           │
│        └────┬────┘        └────┬─────┘                           │
│             │                  │                                 │
│             ▼                  ▼                                 │
│        ┌─────────┐     ┌───────────────┐                         │
│        │ view()  │     │execute_effect()│                        │
│        │ render  │     │  side effects  │                        │
│        └─────────┘     └───────────────┘                         │
│                                                                  │
└─────────────────────────────────────────────────────────────────┘
```

### 1. Model (State)

**File:** `src/ui/chat/state/mod.rs`

Application state is split: `TuiState` (non-overlay) + `OverlayState` (overlay), combined in `AppState`.
These are plain data structures with no I/O:

```rust
pub struct AppState {
    pub tui: TuiState,
    pub overlay: OverlayState,
}

pub struct TuiState {
    pub should_quit: bool,
    pub input: InputState,           // User input textarea, history
    pub transcript: TranscriptState, // Chat history, scroll, selection
    pub conversation: SessionState,  // Messages, session, token usage
    pub auth: AuthState,             // OAuth/API key status
    pub config: Config,              // Model, thinking level
    pub agent_opts: AgentOptions,    // Root path, etc.
    pub system_prompt: Option<String>, // System prompt for agent
    pub agent_state: AgentState,     // Idle | Waiting | Streaming
    pub spinner_frame: usize,        // Animation counter
    pub git_branch: Option<String>,  // Cached at startup
    pub display_path: String,        // Cached at startup
}
```

State is organized into sub-modules for maintainability:
- `state/auth.rs` - Authentication status
- `state/input.rs` - Input editor state
- `state/session.rs` - Session and message history
- `state/transcript.rs` - Transcript display, scroll, cache

### 2. Messages (Events)

**File:** `src/ui/chat/events.rs`

All inputs are converted to a unified `UiEvent` type before processing:

```rust
pub enum UiEvent {
    Tick,                            // Timer for animations
    Frame { width, height },         // Per-frame layout/delta updates
    Terminal(CrosstermEvent),        // Keyboard, mouse, paste, resize
    Agent(AgentEvent),               // Streaming text, tool calls, completion
    LoginResult(Result<(), String>), // Async OAuth result
    HandoffResult(Result<String, String>),
}
```

The `Frame` event is emitted once per frame before other events. It handles:
- Layout updates (viewport dimensions)
- Delta coalescing (streaming text, scroll events)
- Cell line info for lazy rendering

This unification simplifies the reducer—it only needs to handle one event type.

### 3. Update (Reducer)

**File:** `src/ui/chat/reducer.rs`

The reducer is the **single source of truth** for state transitions:

```rust
pub fn update(app: &mut AppState, event: UiEvent) -> Vec<UiEffect> {
    match event {
        UiEvent::Tick => {
            app.tui.spinner_frame = app.tui.spinner_frame.wrapping_add(1);
            app.tui.transcript.check_selection_timeout();
            vec![]
        }
        UiEvent::Frame { width, height } => {
            handle_frame(&mut app.tui, width, height);
            vec![]
        }
        UiEvent::Terminal(term_event) => handle_terminal_event(app, term_event),
        UiEvent::Agent(agent_event) => handle_agent_event(&mut app.tui, &agent_event),
        UiEvent::LoginResult(result) => { /* ... */ }
        UiEvent::HandoffResult(result) => { /* ... */ }
    }
}

/// Handles per-frame state updates (layout, delta coalescing, cell info).
fn handle_frame(tui: &mut TuiState, width: u16, height: u16) {
    // Update transcript layout
    let viewport_height = view::calculate_transcript_height_with_state(tui, height);
    tui.transcript.update_layout((width, height), viewport_height);

    // Apply coalesced streaming text deltas
    apply_pending_delta(tui);

    // Apply coalesced scroll events
    apply_scroll_delta(tui);

    // Update cell line info for lazy rendering
    let counts = view::calculate_cell_line_counts(tui, width as usize);
    tui.transcript.scroll.update_cell_line_info(counts);
}
```

Key properties:
- Takes mutable state reference and an event
- Mutates state directly (no cloning)
- Returns effects for the runtime to execute
- Never performs I/O directly

### 4. View

**File:** `src/ui/chat/view.rs`

Pure rendering functions that draw the UI:

```rust
pub fn view(app: &AppState, frame: &mut Frame) {
    let state = &app.tui;

    // Layout calculation
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(1),      // Transcript
            Constraint::Length(h),   // Input (dynamic height)
            Constraint::Length(1),   // Status line
        ])
        .split(area);

    // Render components
    render_transcript(state, frame, chunks[0]);
    render_input(state, frame, chunks[1]);
    render_status_line(state, frame, chunks[2]);

    // Render overlay (on top)
    app.overlay.render(frame, area, chunks[1].y);
}
```

Key properties:
- Takes `&AppState` (immutable borrow)
- Draws to a ratatui `Frame`
- **Does not mutate logical state** (no message history, scroll position, etc.)
- **Does update render caches** via interior mutability (`RefCell`):
  - `wrap_cache` - Cached markdown wrapping results
  - `position_map` - Line-to-cell mapping for selection

This pragmatic use of interior mutability avoids expensive recomputation while keeping the API clean.

### 5. Effects

**File:** `src/ui/chat/effects.rs`

Effects are descriptions of side effects, not the side effects themselves:

```rust
pub enum UiEffect {
    Quit,
    StartAgentTurn,
    InterruptAgent,
    SpawnTokenExchange { code: String, verifier: String },
    OpenBrowser { url: String },
    SaveSession { event: SessionEvent },
    PersistModel { model: String },
    PersistThinking { level: ThinkingLevel },
    CreateNewSession,
    OpenConfig,
    StartHandoff { goal: String },
    HandoffSubmit { prompt: String },
}
```

Effects are **pure I/O operations**—the runtime executes them without calling back into the reducer. Command execution (from the palette) happens directly in the reducer, not via effects.

### 6. Runtime

**File:** `src/ui/chat/mod.rs`

The `TuiRuntime` orchestrates the event loop. It's intentionally simple—just event collection, dispatch, and I/O:

```rust
impl TuiRuntime {
    fn event_loop(&mut self) -> Result<()> {
        let mut dirty = true;

        while !self.state.should_quit {
            // 1. Collect events from all sources
            let mut events = self.collect_events()?;

            // 2. Prepend Frame event with terminal size
            let size = self.terminal.size()?;
            events.insert(0, UiEvent::Frame {
                width: size.width,
                height: size.height,
            });

            // 3. Process ALL events through the reducer
            for event in events {
                let effects = reducer::update(&mut self.state, event);
                dirty = true;
                self.execute_effects(effects);
            }

            // 4. Render if state changed
            if dirty {
                self.terminal.draw(|frame| {
                    view::view(&self.state, frame);
                })?;
                dirty = false;
            }
        }
        Ok(())
    }

    fn execute_effect(&mut self, effect: UiEffect) {
        match effect {
            UiEffect::Quit => self.state.should_quit = true,
            UiEffect::StartAgentTurn => self.spawn_agent_turn(),
            UiEffect::OpenBrowser { url } => { let _ = open::that(&url); }
            // ... other I/O effects
        }
    }
}
```

The runtime:
- **Owns** the terminal
- **Collects** events from terminal, agent, and async tasks
- **Dispatches** all events to the reducer
- **Executes** effects (I/O only—no state logic)
- **Renders** the view

It does **no state logic**—that's entirely in the reducer.

## Key Patterns

### Overlay State (Mutual Exclusion)

Only one overlay can be active at a time:

```rust
pub enum OverlayState {
    None,
    CommandPalette(CommandPaletteState),
    ModelPicker(ModelPickerState),
    ThinkingPicker(ThinkingPickerState),
    SessionPicker(SessionPickerState),
    FilePicker(FilePickerState),
    Login(LoginState),
}
```

Overlay state is stored alongside non-overlay state in `AppState`:

```rust
pub struct AppState {
    pub tui: TuiState,
    pub overlay: OverlayState,
}
```

This eliminates cascading `if overlay_a.is_some() / if overlay_b.is_some()` checks.

## Overlay Contract

Overlays are modal UI components that temporarily take over keyboard input. Each overlay is **self-contained**: it owns its state, update handlers, and render function.

### Architecture Principle

> Overlay handlers mutate `TuiState` directly via `Overlay::handle_key()`, but they're called **FROM** the reducer (`OverlayState::handle_key`). The reducer remains the single entry point for state mutations and applies the returned `OverlayAction`.

This is the intended pattern: overlays are self-contained modules, but the reducer orchestrates when they're called.

### Required Components

Every overlay module **must** provide:

| Component | Signature | Purpose |
|-----------|-----------|---------|
| **State** | `pub struct XxxState { ... }` | All overlay-specific state |
| **Overlay impl** | `impl Overlay for XxxState { type Config; fn open(...); fn render(...); fn handle_key(...) }` | Trait-enforced open/render/key handling |
| **From impl** | `impl From<XxxState> for OverlayState` | Type-safe conversion for `try_open` |

### State Contract

```rust
#[derive(Debug, Clone)]
pub struct XxxState {
    // Selection index for list-based overlays
    pub selected: usize,
    // Scroll offset for long lists
    pub offset: usize,
    // Overlay-specific fields...
}

impl XxxState {
    /// Creates a new state with sensible defaults.
    pub fn new(/* initialization params */) -> Self { ... }
    
    /// Helper methods for state queries (keep state encapsulated).
    pub fn selected_item(&self) -> Option<&Item> { ... }
}
```

### Overlay Trait Contract

```rust
impl Overlay for XxxState {
    type Config = /* config type, use () if none needed */;

    fn open(config: Self::Config) -> (Self, Vec<UiEffect>) {
        // Create state and return any async effects
        (Self::new(config), vec![/* effects */])
    }

    fn render(&self, frame: &mut Frame, area: Rect, input_y: u16) {
        render_xxx(frame, self, area, input_y)
    }

    fn handle_key(&mut self, tui: &mut TuiState, key: KeyEvent) -> Option<OverlayAction> {
        // Handle keys, return None to continue or Some(action) to close/transition
    }
}

impl From<XxxState> for OverlayState {
    fn from(state: XxxState) -> Self {
        OverlayState::Xxx(state)
    }
}
```

### Opening Overlays

Use `OverlayState::try_open::<T>(config)`:

```rust
// Open with simple config
overlay.try_open::<FilePickerState>(trigger_pos);
overlay.try_open::<LoginState>(());
overlay.try_open::<ModelPickerState>(current_model.clone());

// Open with struct config
overlay.try_open::<SessionPickerState>(SessionPickerConfig {
    sessions,
    original_cells,
});
```

### Key Handler Contract

```rust
impl Overlay for XxxState {
    fn handle_key(&mut self, tui: &mut TuiState, key: KeyEvent) -> Option<OverlayAction> {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);

        match key.code {
            // Standard overlay keys (Esc and Ctrl+C close)
            KeyCode::Esc | KeyCode::Char('c') if ctrl => Some(OverlayAction::close()),
            // Navigation (Up/Down for lists)
            KeyCode::Up => { /* select_prev */ None }  // Continue, no effects
            KeyCode::Down => { /* select_next */ None }
            // Selection (Enter/Tab)
            KeyCode::Enter => { /* execute */ Some(OverlayAction::close_with(effects)) }
            // Filtering (for overlays with search)
            KeyCode::Char(c) if !ctrl => { /* add to filter */ None }
            KeyCode::Backspace => { /* remove from filter */ None }
            _ => None,  // Continue with overlay open
        }
    }
}
```

The return type is `Option<OverlayAction>`:
- `None` = continue with overlay open, no effects
- `Some(Close(effects))` = close overlay and execute effects
- `Some(Transition { new_state, effects })` = transition to new overlay state
- `Some(Effects(effects))` = continue with overlay open, but execute effects (rare)

### Render Function Contract

```rust
/// Renders the overlay centered above the input area.
/// Takes immutable reference to overlay state (not full TuiState).
pub fn render_xxx(
    frame: &mut Frame,
    state: &XxxState,       // Immutable borrow of overlay state only
    area: Rect,             // Full terminal area
    input_top_y: u16,       // Y position of input (for vertical centering)
) {
    // 1. Calculate dimensions (width, height)
    let picker_width = /* ... */;
    let picker_height = /* ... */;
    
    // 2. Center horizontally, position above input
    let picker_x = (area.width.saturating_sub(picker_width)) / 2;
    let picker_y = (input_top_y.saturating_sub(picker_height)) / 2;
    let picker_area = Rect::new(picker_x, picker_y, picker_width, picker_height);
    
    // 3. Clear background (required for overlays)
    frame.render_widget(Clear, picker_area);
    
    // 4. Render border with title
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(/* accent color */))
        .title(" Title ");
    frame.render_widget(block, picker_area);
    
    // 5. Render content (list, form, etc.)
    // 6. Render hints at bottom
}
```

### Effect Pattern

Overlays return `UiEffect`s via the `open()` method or `OverlayAction`:

```rust
// Open returns effects for async initialization
impl Overlay for FilePickerState {
    type Config = usize;

    fn open(trigger_pos: Self::Config) -> (Self, Vec<UiEffect>) {
        (Self::new(trigger_pos), vec![UiEffect::DiscoverFiles])
    }
}

// Selection returns effects via OverlayAction
fn handle_key(&mut self, tui: &mut TuiState, key: KeyEvent) -> Option<OverlayAction> {
    match key.code {
        KeyCode::Enter => Some(OverlayAction::close_with(vec![
            UiEffect::PersistModel { model: model_id }
        ])),
        _ => None,
    }
}
```

### Reducer Integration

The reducer routes key events to the active overlay handler:

```rust
// In reducer.rs
fn handle_key(app: &mut AppState, key: KeyEvent) -> Vec<UiEffect> {
    // Try to dispatch to the active overlay
    match app.overlay.handle_key(&mut app.tui, key) {
        None => handle_main_key(app, key), // No overlay active
        Some(None) => vec![],              // Overlay handled it, continue
        Some(Some(action)) => process_overlay_action(app, action), // Overlay action
    }
}

/// Processes an OverlayAction returned by an overlay's handle_key.
fn process_overlay_action(app: &mut AppState, action: OverlayAction) -> Vec<UiEffect> {
    match action {
        OverlayAction::Close(effects) => {
            app.overlay = OverlayState::None;
            effects
        }
        OverlayAction::Transition { new_state, effects } => {
            app.overlay = new_state;
            effects
        }
        OverlayAction::Effects(effects) => effects,
    }
}
```

### View Integration

The view uses `OverlayState::render()` which delegates to each overlay's `Overlay` trait implementation:

```rust
// In view.rs
pub fn view(app: &AppState, frame: &mut Frame) {
    // ... render transcript, input, status ...
    
    // Render overlay using trait method (delegates to appropriate impl)
    app.overlay.render(frame, area, chunks[1].y);
}
```

This trait-based approach:
- Eliminates explicit match statements in the view
- Enforces that new overlays implement rendering at compile time
- Provides uniform rendering interface across all overlays


### Special Cases

#### Login Overlay (State Machine with Async Transitions)

The login overlay has multiple states and async transitions:

```rust
pub enum LoginState {
    AwaitingCode { url, verifier, input, error },
    Exchanging,
}
```

The flow is handled via `OverlayAction::Transition`:

1. **Opening**: `/login` command returns `UiEffect::OpenLogin`
2. **Runtime**: Calls `overlay.try_open::<LoginState>(())` which returns `OpenBrowser` effect
3. **Input**: User pastes auth code, `handle_key()` returns `Transition { new_state: Exchanging, effects: [SpawnTokenExchange] }`
4. **Async result**: `UiEvent::LoginResult` arrives, `handle_login_result()` closes overlay or returns to `AwaitingCode` with error

This pattern uses `OverlayAction::Transition` for state machine transitions within the overlay.

#### Overlays with Async Data Loading

For overlays that load data asynchronously (file picker, session picker):

1. **`open()`** returns effects to trigger async loading (e.g., `DiscoverFiles`)
2. **State** includes a `loading: bool` field
3. **Handler** processes result events (e.g., `UiEvent::FilesDiscovered`)
4. **Render** shows loading state until data arrives

### Checklist for New Overlays

When adding a new overlay:

- [ ] Create `src/ui/chat/overlays/xxx.rs`
- [ ] Define `XxxState` struct with `new()` constructor
- [ ] **Implement `Overlay` trait** for `XxxState`:
  - `type Config` - configuration parameters (use `()` if none)
  - `fn open(config) -> (Self, Vec<UiEffect>)` - create state and return effects
  - `fn render(&self, frame, area, input_y)` - render the overlay
  - `fn handle_key(&mut self, tui, key) -> Option<OverlayAction>` - key handling
- [ ] **Implement `From<XxxState> for OverlayState`** - for `try_open` conversion
- [ ] Add `XxxState` to `OverlayState` enum in `overlays/mod.rs`
- [ ] **Add variant to `OverlayState::render()` match** in `overlays/mod.rs`
- [ ] **Add variant to `OverlayState::handle_key()` match** in `overlays/mod.rs`
- [ ] Export state type from `overlays/mod.rs`
- [ ] Add any new effects to `effects.rs`
- [ ] Add effect handler in `runtime/mod.rs` if needed
- [ ] Update this documentation

### Agent State Machine

The agent goes through distinct states:

```rust
pub enum AgentState {
    Idle,                              // Ready for input
    Waiting { rx: Receiver<...> },     // Waiting for first response
    Streaming { rx, cell_id, pending_delta },  // Receiving content
}
```

### Delta Coalescing

Multiple events can arrive per frame. Streaming deltas are buffered in `AgentState::Streaming`:

```rust
AgentState::Streaming {
    pending_delta: String,  // Accumulates until Frame event
    // ...
}
```

Applied during Frame event handling in the reducer:

```rust
fn handle_frame(state: &mut TuiState, width: u16, height: u16) {
    // ... layout updates ...
    apply_pending_delta(state);  // Coalesces streaming text
    apply_scroll_delta(state);   // Coalesces scroll events
    // ... cell line info ...
}
```

### Scroll Accumulator

Mouse scroll events are coalesced similarly. Events accumulate during collection:

```rust
// During mouse event handling in reducer
state.transcript.scroll_accumulator.accumulate(-3);  // Scroll up
state.transcript.scroll_accumulator.accumulate(-3);  // Another scroll up
```

Applied once per frame during Frame event handling:

```rust
// In handle_frame()
apply_scroll_delta(state);
// Internally: delta = -6, applied as single scroll operation
```

### Lazy Rendering

For long transcripts, only visible cells are rendered:

```rust
fn render_transcript_lazy(state: &TuiState, width: usize, visible: VisibleRange) 
    -> Vec<Line<'static>> 
{
    // Only iterate cells in visible.cell_range
    for cell in &state.transcript.cells[visible.cell_range.clone()] {
        // ...
    }
}
```

**How it works:**

1. `ScrollState::cell_line_info` stores `(CellId, line_count)` for each cell
2. `visible_range()` calculates which cells are on screen based on scroll position
3. `render_transcript_lazy()` only renders those cells
4. `position_map` is built with scroll offset for correct selection coordinates

This enables smooth scrolling even with thousands of messages.

### Wrap Cache

Markdown rendering and line wrapping results are cached:

```rust
// In HistoryCell::display_lines_cached
if let Some(cached) = wrap_cache.get(&self.id()) {
    return cached.clone();
}
// ... expensive computation ...
wrap_cache.insert(self.id(), result.clone());
```

Cache is invalidated on:
- Terminal resize
- `/new` command (clear conversation)

### Render-Time Caches (Interior Mutability)

The view takes `&AppState`, but render helpers still read from `&TuiState` and update caches for performance. This is done via `RefCell`:

```rust
// In TranscriptState
pub wrap_cache: WrapCache,      // RefCell<HashMap<CellId, Vec<StyledLine>>>
pub position_map: PositionMap,  // RefCell<Vec<LineMapping>>
```

- **wrap_cache**: Stores pre-wrapped markdown lines per cell
- **position_map**: Maps screen coordinates to transcript positions for selection

This pattern allows the view to cache expensive computations without requiring `&mut TuiState`.

### Input Handling (tui-textarea)

The input area uses the [`tui-textarea`](https://github.com/rhysd/tui-textarea) crate, which provides:
- Multi-line editing with cursor movement
- Undo/redo support
- Unicode-aware text handling

```rust
pub struct InputState {
    pub textarea: TextArea<'static>,  // From tui-textarea crate
    pub history: Vec<String>,         // Command history
    pub history_index: Option<usize>, // Navigation position
    // ...
}
```

## Why This Architecture?

### 1. Borrow-Checker Friendly

Separating `TuiState` from `TuiRuntime` avoids borrow conflicts:

```rust
// This works because state is a separate field
self.terminal.draw(|frame| {
    view::view(&self.state, frame);  // Borrows self.state
})?;
// self.terminal is borrowed by draw(), but self.state is independent
```

### 2. Testability

The reducer is pure logic—easy to unit test:

```rust
#[test]
fn test_execute_new_clears_state() {
    let mut state = TuiState::new(config, path, None, None);
    state.transcript.cells.push(HistoryCell::user("test"));
    
    execute_new(&mut state);
    
    assert!(state.transcript.cells.is_empty());
    assert!(state.conversation.messages.is_empty());
}
```

### 3. Predictability

All state changes go through `update()`. Debugging is straightforward:
1. Log the event
2. Log the resulting effects
3. Trace state changes

### 4. Separation of Concerns

| Layer | Responsibility | I/O Allowed |
|-------|---------------|-------------|
| State | Data structures | No |
| Events | Input types | No |
| Reducer | State transitions | No |
| View | Rendering | No (read-only) |
| Effects | Side effect descriptions | No |
| Runtime | Execution | Yes |

## File Reference

| File | Purpose |
|------|---------|
| `mod.rs` | Entry points, module declarations |
| `runtime/mod.rs` | `TuiRuntime`, event loop, effect dispatch |
| `runtime/handlers.rs` | Effect handlers (session ops, agent spawn, auth) |
| `runtime/handoff.rs` | Handoff generation handlers (subagent spawning) |
| `transcript_build.rs` | Pure helper to build transcript cells from session events |
| `state/mod.rs` | `AppState`, `TuiState`, and sub-state types |
| `state/auth.rs` | `AuthState`, `AuthStatus` |
| `state/input.rs` | `InputState`, textarea, history navigation |
| `state/session.rs` | `SessionState`, messages, usage tracking |
| `state/transcript.rs` | `TranscriptState`, scroll, selection, cache |
| `reducer.rs` | `update()` function, all state mutations |
| `view.rs` | `view()` function, all rendering |
| `effects.rs` | `UiEffect` enum |
| `events.rs` | `UiEvent` enum |
| `commands.rs` | Slash command definitions |
| `selection.rs` | Text selection, clipboard, position mapping |
| `terminal.rs` | Terminal setup, restore, panic hooks |
| `overlays/mod.rs` | Overlay exports, `OverlayState` enum, dispatch methods |
| `overlays/traits.rs` | `Overlay` trait and `OverlayAction` types |
| `overlays/palette.rs` | Command palette overlay |
| `overlays/model_picker.rs` | Model picker overlay |
| `overlays/thinking_picker.rs` | Thinking level picker overlay |
| `overlays/session_picker.rs` | Session picker overlay |
| `overlays/file_picker.rs` | File picker overlay (triggered by `@`) |
| `overlays/login.rs` | OAuth login flow overlay |

## Related Documentation

- `docs/SPEC.md` - Product behavior contracts
- `docs/adr/` - Architecture Decision Records
- `AGENTS.md` - Development guide and conventions
