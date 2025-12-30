# zdx Architecture

This document describes the architectural patterns used in zdx, with a focus on the TUI's Elm-like architecture.

## Overview

zdx is a terminal-based AI coding assistant built in Rust. The codebase is organized into several layers:

```
src/
├── main.rs              # Binary entrypoint
├── app/                 # CLI parsing + command dispatch
├── config.rs            # Configuration loading
├── core/                # UI-agnostic domain logic
│   ├── agent.rs         # Agent loop + event channels
│   ├── context.rs       # Project context (AGENTS.md)
│   ├── events.rs        # Agent event types
│   ├── interrupt.rs     # Signal handling
│   └── session.rs       # Session persistence
├── providers/           # API clients (Anthropic, OAuth)
├── tools/               # Tool implementations (bash, edit, read, write)
└── ui/                  # Terminal UI
    ├── exec.rs          # Non-interactive exec mode
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
│         │  &mut TuiState × UiEvent   │                           │
│         │      → Vec<UiEffect>       │                           │
│         └─────────────┬──────────────┘                           │
│                       │                                          │
│             ┌─────────┴─────────┐                                │
│             ▼                   ▼                                │
│        ┌─────────┐        ┌──────────┐                           │
│        │TuiState │        │UiEffect[]│                           │
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

All application state lives in `TuiState`. It's a plain data structure with no I/O:

```rust
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
    pub overlay: OverlayState,       // Command palette, pickers, login
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
pub fn update(state: &mut TuiState, event: UiEvent) -> Vec<UiEffect> {
    match event {
        UiEvent::Tick => {
            state.spinner_frame = state.spinner_frame.wrapping_add(1);
            state.transcript.check_selection_timeout();
            vec![]
        }
        UiEvent::Frame { width, height } => {
            handle_frame(state, width, height);
            vec![]
        }
        UiEvent::Terminal(term_event) => handle_terminal_event(state, term_event),
        UiEvent::Agent(agent_event) => handle_agent_event(state, &agent_event),
        UiEvent::LoginResult(result) => { /* ... */ }
        UiEvent::HandoffResult(result) => { /* ... */ }
    }
}

/// Handles per-frame state updates (layout, delta coalescing, cell info).
fn handle_frame(state: &mut TuiState, width: u16, height: u16) {
    // Update transcript layout
    let viewport_height = view::calculate_transcript_height_with_state(state, height);
    state.transcript.update_layout((width, height), viewport_height);

    // Apply coalesced streaming text deltas
    apply_pending_delta(state);

    // Apply coalesced scroll events
    apply_scroll_delta(state);

    // Update cell line info for lazy rendering
    let counts = view::calculate_cell_line_counts(state, width as usize);
    state.transcript.scroll.update_cell_line_info(counts);
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
pub fn view(state: &TuiState, frame: &mut Frame) {
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
    match &state.overlay {
        OverlayState::CommandPalette(p) => render_command_palette(frame, p, area, input_y),
        OverlayState::ModelPicker(p) => render_model_picker(frame, p, area, input_y),
        OverlayState::ThinkingPicker(p) => render_thinking_picker(frame, p, area, input_y),
        OverlayState::Login(l) => render_login_overlay(frame, l, area),
        OverlayState::None => {}
    }
}
```

Key properties:
- Takes `&TuiState` (immutable borrow)
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
    Login(LoginState),
}
```

This eliminates cascading `if overlay_a.is_some() / if overlay_b.is_some()` checks.

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

The view takes `&TuiState` but still needs to update caches for performance. This is done via `RefCell`:

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
| `mod.rs` | `TuiRuntime`, event loop, effect execution |
| `state/mod.rs` | `TuiState` and sub-state types |
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
| `overlays/mod.rs` | Overlay exports and shared utilities |
| `overlays/palette.rs` | Command palette overlay |
| `overlays/model_picker.rs` | Model picker overlay |
| `overlays/thinking_picker.rs` | Thinking level picker overlay |
| `overlays/login.rs` | OAuth login flow overlay |

## Related Documentation

- `docs/SPEC.md` - Product behavior contracts
- `docs/adr/` - Architecture Decision Records
- `AGENTS.md` - Development guide and conventions
