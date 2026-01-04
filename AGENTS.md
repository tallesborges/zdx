# zdx development guide

`docs/SPEC.md` is the source of truth for product behavior (contracts). This file is about working in the repo.

## Where things are

- `src/main.rs`: binary entrypoint (delegates to `src/cli/`)
- `src/default_config.toml`: default configuration template
- `src/cli/`: CLI arguments + command dispatch
  - `src/cli/mod.rs`: clap structs + dispatch
  - `src/cli/commands/mod.rs`: command module exports
  - `src/cli/commands/chat.rs`: chat command handler (includes piped stdin fallback)
  - `src/cli/commands/exec.rs`: exec command handler
  - `src/cli/commands/sessions.rs`: list/show/resume sessions
  - `src/cli/commands/config.rs`: config path/init handlers
  - `src/cli/commands/auth.rs`: login/logout flows
- `src/config.rs`: config loading + paths
- `src/models.rs`: model registry for TUI model picker
- `src/models_generated.rs`: generated model data (from `cargo run --bin generate_models`)
- `src/bin/generate_models.rs`: binary to generate model data from API
- `src/core/`: UI-agnostic domain + runtime
  - `src/core/mod.rs`: core module exports
  - `src/core/events.rs`: agent event types for streaming
  - `src/core/context.rs`: project context loading (AGENTS.md files)
  - `src/core/interrupt.rs`: signal handling
  - `src/core/agent.rs`: agent loop + event channels
  - `src/core/session.rs`: session persistence
- `src/modes/`: runtime execution modes
  - `src/modes/mod.rs`: mode module exports
  - `src/modes/exec.rs`: non-interactive streaming mode (stdout/stderr rendering)
  - `src/modes/tui/`: full-screen interactive TUI (Elm-like architecture)
    - `src/modes/tui/mod.rs`: entry points (run_interactive_chat) + module declarations
    - `src/modes/tui/app.rs`: AppState + TuiState + AgentState (state composition, hierarchy)
    - `src/modes/tui/runtime/mod.rs`: TuiRuntime - owns terminal, runs event loop, effect dispatch
    - `src/modes/tui/runtime/handlers.rs`: effect handlers (session ops, agent spawn, auth)
    - `src/modes/tui/runtime/handoff.rs`: handoff generation handlers (subagent spawning)
    - `src/modes/tui/state/mod.rs`: state re-export hub (backward compatibility shim)
    - `src/modes/tui/state/auth.rs`: auth state re-exports (from auth feature slice)
    - `src/modes/tui/state/input.rs`: input state re-exports (from input feature slice)
    - `src/modes/tui/state/session.rs`: session state re-exports (from session feature slice)
    - `src/modes/tui/state/transcript.rs`: transcript state re-exports (from transcript feature slice)
    - `src/modes/tui/reducer.rs`: reducer - orchestrates state mutations, delegates to feature slices
    - `src/modes/tui/view.rs`: pure render functions (no mutations), delegates transcript rendering
    - `src/modes/tui/events.rs`: UI event types (re-exports from core feature slice)
    - `src/modes/tui/terminal.rs`: terminal setup, restore, panic hooks
    - `src/modes/tui/shared/`: shared leaf types (no feature dependencies)
      - `src/modes/tui/shared/mod.rs`: module exports
      - `src/modes/tui/shared/effects.rs`: effect types returned by reducer for runtime to execute
      - `src/modes/tui/shared/commands.rs`: command definitions for command palette
    - `src/modes/tui/auth/`: auth feature slice (authentication state, login handling)
      - `src/modes/tui/auth/mod.rs`: module exports
      - `src/modes/tui/auth/state.rs`: AuthStatus + AuthState (auth type detection, login flow state)
      - `src/modes/tui/auth/reducer.rs`: login result handling, OAuth flow state transitions
      - `src/modes/tui/auth/view.rs`: login overlay rendering
    - `src/modes/tui/core/`: core feature slice (event aggregator)
      - `src/modes/tui/core/mod.rs`: module exports
      - `src/modes/tui/core/events.rs`: UiEvent + SessionUiEvent (aggregator for all TUI events)
    - `src/modes/tui/input/`: input feature slice (keyboard handling, handoff)
      - `src/modes/tui/input/mod.rs`: module exports
      - `src/modes/tui/input/state.rs`: InputState + HandoffState
      - `src/modes/tui/input/reducer.rs`: key handling, input submission, handoff result handling
      - `src/modes/tui/input/view.rs`: input area rendering (normal + handoff modes)
    - `src/modes/tui/session/`: session feature slice (session state, session operations)
      - `src/modes/tui/session/mod.rs`: module exports
      - `src/modes/tui/session/state.rs`: SessionState, SessionOpsState, SessionUsage
      - `src/modes/tui/session/reducer.rs`: session event handlers (loading, switching, creating, renaming)
      - `src/modes/tui/session/view.rs`: session picker overlay rendering
    - `src/modes/tui/overlays/`: overlay feature slice (modal UI components)
      - `src/modes/tui/overlays/mod.rs`: `Overlay` enum, `OverlayAction`, `OverlayExt` trait for `Option<Overlay>`
      - `src/modes/tui/overlays/update.rs`: overlay key handling and update logic
      - `src/modes/tui/overlays/view.rs`: shared rendering utilities for overlays
      - `src/modes/tui/overlays/command_palette.rs`: command palette overlay
      - `src/modes/tui/overlays/model_picker.rs`: model picker overlay
      - `src/modes/tui/overlays/thinking_picker.rs`: thinking level picker overlay
      - `src/modes/tui/overlays/session_picker.rs`: session picker overlay (state + key handling; rendering delegated to session feature)
      - `src/modes/tui/overlays/file_picker.rs`: file picker overlay (triggered by `@`, async file discovery, fuzzy filtering)
      - `src/modes/tui/overlays/login.rs`: OAuth login flow overlay (state + key handling; rendering delegated to auth feature)
    - `src/modes/tui/markdown/`: markdown parsing and wrapping
      - `src/modes/tui/markdown/mod.rs`: module exports
      - `src/modes/tui/markdown/parse.rs`: markdown parsing + rendering
      - `src/modes/tui/markdown/wrap.rs`: styled span wrapping
      - `src/modes/tui/markdown/stream.rs`: streaming collector + commit logic
    - `src/modes/tui/transcript/`: transcript feature slice (transcript state, rendering, updates)
      - `src/modes/tui/transcript/mod.rs`: module exports
      - `src/modes/tui/transcript/state.rs`: TranscriptState, ScrollState, SelectionState management
      - `src/modes/tui/transcript/selection.rs`: text selection and copy (grapheme-based, OSC 52 + system clipboard)
      - `src/modes/tui/transcript/build.rs`: pure helper to build transcript cells from session events
      - `src/modes/tui/transcript/update.rs`: agent event handlers, mouse handling, delta coalescing
      - `src/modes/tui/transcript/render.rs`: transcript rendering (full and lazy), style conversion
      - `src/modes/tui/transcript/cell.rs`: HistoryCell + rendering
      - `src/modes/tui/transcript/wrap.rs`: wrapping + wrap cache
      - `src/modes/tui/transcript/style.rs`: transcript style types
- `src/tools/`: tool implementations + schemas
  - `src/tools/mod.rs`: tool module exports + tool registry
  - `src/tools/bash.rs`: bash/shell command tool
  - `src/tools/edit.rs`: file edit tool
  - `src/tools/read.rs`: file read tool
  - `src/tools/write.rs`: file write tool
- `src/providers/`: provider clients
  - `src/providers/mod.rs`: provider module exports
  - `src/providers/anthropic/`: Anthropic API client
    - `src/providers/anthropic/mod.rs`: public re-exports
    - `src/providers/anthropic/auth.rs`: auth resolution + config
    - `src/providers/anthropic/client.rs`: AnthropicClient + request wiring
    - `src/providers/anthropic/sse.rs`: SSE parsing + stream events
    - `src/providers/anthropic/types.rs`: API DTOs + chat message types
    - `src/providers/anthropic/errors.rs`: provider error types
  - `src/providers/oauth.rs`: OAuth token storage + retrieval
- `tests/`: integration tests (`assert_cmd`, fixtures)

## Build / run

- `cargo run -- --help`
- `cargo run --` (interactive; needs provider key via env)
- `cargo test`
- `cargo +nightly fmt` (uses nightly for full rustfmt features; stable works but ignores some options)
- `cargo clippy`

## Conventions

- Rust edition: 2024 (see `Cargo.toml`)
- Formatting: rustfmt defaults
- Errors: prefer `anyhow::Result` + `Context` at I/O boundaries
- Keep `src/core/` UI-agnostic: terminal I/O belongs in `src/modes/` only

## Tests (keep it light)

- Add tests only to protect a user-visible contract or a real regression.
- Prefer integration tests in `tests/` over unit tests for CLI/output/persistence behavior.
- Avoid mutating process-global env vars in-process; set env on spawned CLI commands instead.

## Docs

- `docs/SPEC.md`: contracts (what/behavior)
- `docs/ARCHITECTURE.md`: system architecture and design (includes **Overlay Contract**)
- `docs/adr/`: durable decisions (why)
- `docs/plans/`: optional commit-sized plans (how)

## Delegating tasks (subagent pattern)

When a task is complex or would pollute the current context, delegate to a fresh zdx instance:

```bash
# If zdx is in PATH:
zdx --no-save exec -p "your task description"
```

This runs in an isolated process with its own context window. Use for:
- Reading large files and summarizing
- Complex multi-step analysis
- Research tasks that generate lots of intermediate output
- Any task where you only need the final result

The `--no-save` flag prevents session file creation. Output is returned directly.

**Reading previous sessions:**
```bash
zdx sessions show <session_id>
```

Use this to fetch context from a previous conversation when needed.

## ⚠️ IMPORTANT: Keep this file up to date

**This is mandatory, not optional.** When you:

- **Add a new `.rs` file** → Add it to "Where things are" with a one-line description
- **Move/rename a module** → Update the path in "Where things are"
- **Delete a file** → Remove it from "Where things are"
- **Change build/run/test workflows** → Update "Build / run"
- **Add new conventions** → Document here or in scoped `AGENTS.md` files
- **Change system architecture** → Update `docs/ARCHITECTURE.md` (module relationships, data flow, component boundaries, or design patterns)
