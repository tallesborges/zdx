# zdx Architecture

zdx is a terminal-based AI coding assistant built in Rust, featuring a non-interactive execution mode and a full-screen interactive TUI.

## Workspace Layout

- **zdx (binary):** CLI + exec mode, routes to the TUI when the `tui` feature is enabled.
- **zdx-types:** Pure shared value types (DTOs/enums) used across providers, tools, and events. No runtime deps.
- **zdx-assets:** Embedded asset content (prompts, instruction layers, default TOMLs, bundled skills, built-in subagents). No runtime deps.
- **zdx-providers:** LLM provider implementations (Anthropic, OpenAI, Gemini, etc.). Depends on zdx-types and zdx-assets.
- **zdx-tools:** Leaf tool implementations (bash, edit, read, write, glob, grep, etc.) with a minimal ToolContext. Depends on zdx-types.
- **zdx-engine:** Runtime engine: config, agent orchestration, thread persistence, skills, subagents, MCP, engine-backed tools, and all remaining runtime modules. Depends on zdx-types, zdx-assets, zdx-providers, zdx-tools.
- **zdx-tui:** Full-screen TUI (Elm/MVU), depends on zdx-engine.
- **zdx-bot:** Telegram bot surface.
- **zdx-monitor:** Service dashboard TUI.

## MCP Internal Engine

MCP support lives in `zdx-engine/src/mcp.rs` as an internal engine. The primary product surface is the helper CLI (`zdx mcp ...`), not automatic model-visible tool exposure.

- **Config source:** project-local `.mcp.json` using the standard `mcpServers` JSON shape.
- **Workspace/runtime:** `load_workspace(root)` initializes configured servers, resolves cached HTTP MCP OAuth credentials, captures per-server status/diagnostics, lists tools, exposes schemas, and supports direct MCP `tools/call` execution.
- **Helper CLI:** `zdx mcp servers|auth|logout|tools|schema|call` uses that workspace and emits structured JSON for inspection plus interactive auth/logout flows for OAuth-protected HTTP MCP servers.
- **HTTP OAuth cache:** remote MCP OAuth credentials are stored separately from model-provider OAuth tokens in `<base>/mcp_oauth.json`.
- **Naming:** discovered tools still get stable internal names like `mcp__xcode__build_app`, which the helper CLI can surface in structured output.
- **Default agent surfaces:** `zdx exec`, the TUI, and the Telegram bot keep the built-in model-visible tool list by default; MCP catalogs are not dumped into the provider tool list automatically.
- **Failure isolation:** each server is initialized independently; failed servers contribute diagnostics but do not prevent healthy MCP servers from loading.
- **Auth discovery:** OAuth-protected HTTP MCP servers are classified as `auth_required` when ZDX can discover protected-resource/auth-server metadata, instead of surfacing only generic load failures.
- **Lifecycle:** the helper CLI loads MCP state for the current invocation. Long-lived warm-session reuse for interactive surfaces is deferred until a dedicated session model is added.

This keeps provider integration unchanged for normal agent turns: providers still see the built-in `ToolDefinition` list unless an explicit MCP augmentation path is used.

## Prompt Architecture

Prompt assembly is layered in `zdx-engine` (assets come from `zdx-assets`):

- **Base system prompt:** `prompts/system_prompt_template.md` is the canonical default prompt.
- **Prompt layers:** additive prompt fragments appended after the base prompt. These are used for surface/runtime constraints (for example Telegram or exec output guidance) and behavior harnesses (for example automation/headless execution).
- **Named subagents:** optional standalone prompt profiles for delegated child runs. A subagent provides its own prompt body and can override model/tool/thinking configuration without inheriting the shared base prompt.

This keeps one source of truth for the default assistant while allowing surfaces and automation behavior to compose cleanly, and still supports specialist standalone subagents when needed.

## TUI Architecture (Elm/MVU)

The interactive mode (`crates/zdx-tui/src/`) strictly follows The Elm Architecture (Model-View-Update).

**Core Principle:** All state lives in one place (`AppState`). All mutations happen via the reducer (`update`). All side effects are explicit descriptions (`UiEffect`) executed by the runtime.

## Design Principles (Guidance)

- **Decision simplicity (prefer):** Favor designs where the answer to a UI question is obvious and derived from one clear place, reducing ambiguity and making decisions faster.
- **Low‑drift structures (prefer):** Avoid parallel state that can fall out of sync; prefer structures that minimize maintenance and drift over time.

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
- **Task state only:** All async operations are modeled as tasks; UI derives loading state from `TaskState` only (no separate loading flags).
- **Task lifecycle:** The runtime emits `UiEvent::TaskStarted` when a task is actually spawned, and `UiEvent::TaskCompleted` with the wrapped result when it finishes. The reducer is the only place that mutates `TaskState`, and uses `TaskId` for latest-only gating.
- **Cancellation pattern:** Cancelable tasks use `CancellationToken` carried in `TaskStarted`. The reducer initiates cancellation via `UiEffect::CancelTask` (with the token); the runtime only calls `token.cancel()`.
- **Lifecycle flow:**
  - User action → reducer emits effect (with/without `TaskId`)
  - Runtime `spawn_task` emits `TaskStarted` → reducer marks running
  - Runtime emits `TaskCompleted` with result → reducer clears task + applies result

### Performance
- **Delta Coalescing:** High-frequency events (streaming text, scrolling) are buffered and applied once per frame (`UiEvent::Frame`).
- **Lazy Rendering:** Only visible transcript cells are rendered.
- **Wrap Cache:** Markdown layout is cached per cell ID.

### Agent State Machine
The agent progresses through `Idle` -> `Waiting` (for first byte) -> `Streaming` (accumulating deltas) -> `Idle`.

---
*For file locations, see `AGENTS.md`.*
