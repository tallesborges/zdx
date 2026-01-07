# zdx Architecture

zdx is a terminal-based AI coding assistant built in Rust, featuring a non-interactive execution mode and a full-screen interactive TUI.

## TUI Architecture (Elm/MVU)

The interactive mode (`src/modes/tui/`) strictly follows The Elm Architecture (Model-View-Update).

**Core Principle:** All state lives in one place (`AppState`). All mutations happen via the reducer (`update`). All side effects are explicit descriptions (`UiEffect`) executed by the runtime.

### Data Flow

```
┌──────────────┐    ┌──────────────┐    ┌──────────────┐
│  Term Event  │    │ Agent Event  │    │ Async Event  │
└───────┬──────┘    └──────┬───────┘    └──────┬───────┘
        │                  │                   │
        ▼                  ▼                   ▼
    ┌─────────────────────────────────────────┐
    │           UiEvent (Unified)             │
    └────────────────────┬────────────────────┘
                         │
               ┌─────────▼─────────┐
               │ update(state, msg)│
               │ -> (state, effs)  │
               └────┬───────────┬──┘
                    │           │
          ┌─────────▼─┐       ┌─▼─────────────┐
          │ new state │       │ Vec<UiEffect> │
          └────┬──────┘       └─┬─────────────┘
               │                │
      ┌────────▼───────┐    ┌───▼─────────────┐
      │ render(state)  │    │ runtime executes│
      └────────────────┘    └─────────────────┘
```

### 1. Model (`AppState`)
State is a plain struct containing:
- `tui`: Core application state (input, transcript, thread, config).
- `overlay`: `Option<Overlay>` for modal UIs (command palette, file picker, etc.).

State is organized into **feature slices** (auth, input, thread, transcript), each exposing `state`, `update`, and `render` modules.

### 2. Update (The Reducer)
The `update` function is the single source of truth for state transitions. It handles `UiEvent`s and returns `Vec<UiEffect>`. It never performs I/O directly.

**StateMutations:** Feature slices return `StateMutation` enums to request changes on other slices (e.g., Input slice requesting a Transcript scroll). The reducer routes each mutation to the owning slice’s `apply()` method.

### 3. View
Pure functions render `&AppState` to a Ratatui frame.
*   **Interior Mutability:** `RefCell` is used *only* for render-time caches (markdown wrapping, selection mapping) to avoid expensive re-computations without mutating logical state.

### 4. Effects & Runtime
`UiEffect` describes I/O and task spawning only (e.g., `Quit`, `OpenBrowser`, `SaveThread`).
The `TuiRuntime`:
1.  Collects events (User input, Agent messages, Async channels).
2.  Feeds them to `update`.
3.  Executes resulting `UiEffect`s.
4.  Renders the view.

## Key Patterns

### Overlays (Modals)
Overlays (e.g., Command Palette, File Picker) are self-contained state machines in `AppState.overlay`.
- **Mutual Exclusion:** Only one overlay is active at a time.
- **Input Priority:** Active overlay intercepts keys before the main app.
- **Lifecycle:**
    - **Open:** Set directly by the reducer (often from input or overlay actions). File picker opening returns `DiscoverFiles` for I/O.
    - **Update:** Internal mutations + `StateMutation`s for global changes.
    - **Close:** Returns effects to run after dismissal (e.g., `LoadSession`).

### Async & Concurrency
- **Receivers in State:** Receivers for async workflows live in `AppState`. The runtime polls them and emits `UiEvent`s.
- **Receiver lifecycle:** `*Started` event → reducer stores receiver → runtime polls → result event → reducer clears receiver.

### Performance
- **Delta Coalescing:** High-frequency events (streaming text, scrolling) are buffered and applied once per frame (`UiEvent::Frame`).
- **Lazy Rendering:** Only visible transcript cells are rendered.
- **Wrap Cache:** Markdown layout is cached per cell ID.

### Agent State Machine
The agent progresses through `Idle` -> `Waiting` (for first byte) -> `Streaming` (accumulating deltas) -> `Idle`.

---
*For file locations, see `AGENTS.md`.*
