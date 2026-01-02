# zdx development guide

`docs/SPEC.md` is the source of truth for product behavior (contracts). This file is about working in the repo.

## Where things are

- `src/main.rs`: binary entrypoint (delegates to `src/app/`)
- `src/default_config.toml`: default configuration template
- `src/app/`: CLI arguments + command dispatch
  - `src/app/mod.rs`: clap structs + dispatch
  - `src/app/commands/mod.rs`: command module exports
  - `src/app/commands/chat.rs`: chat command handler (includes piped stdin fallback)
  - `src/app/commands/exec.rs`: exec command handler
  - `src/app/commands/sessions.rs`: list/show/resume sessions
  - `src/app/commands/config.rs`: config path/init handlers
  - `src/app/commands/auth.rs`: login/logout flows
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
- `src/ui/`: terminal UI
  - `src/ui/mod.rs`: UI module exports
  - `src/ui/exec.rs`: streamed stdout/stderr rendering + exec mode wrapper
  - `src/ui/chat/`: interactive TUI (Elm-like architecture)
    - `src/ui/chat/mod.rs`: entry points (run_interactive_chat) + module declarations
    - `src/ui/chat/runtime/mod.rs`: TuiRuntime - owns terminal, runs event loop, effect dispatch
    - `src/ui/chat/runtime/handlers.rs`: effect handlers (session ops, agent spawn, auth)
    - `src/ui/chat/runtime/handoff.rs`: handoff generation handlers (subagent spawning)
    - `src/ui/chat/transcript_build.rs`: pure helper to build transcript cells from session events
    - `src/ui/chat/state/mod.rs`: TuiState - all app state (no terminal)
    - `src/ui/chat/state/auth.rs`: auth status + login flow state
    - `src/ui/chat/state/input.rs`: input editor state
    - `src/ui/chat/state/session.rs`: session + message history state
    - `src/ui/chat/state/transcript.rs`: transcript view state (scroll, selection, cache)
    - `src/ui/chat/reducer.rs`: reducer - all state mutations happen here
    - `src/ui/chat/view.rs`: pure render functions (no mutations)
    - `src/ui/chat/effects.rs`: effect types returned by reducer for runtime to execute
    - `src/ui/chat/events.rs`: UI event types
    - `src/ui/chat/commands.rs`: command definitions for command palette
    - `src/ui/chat/selection.rs`: text selection and copy (grapheme-based, OSC 52 + system clipboard)
    - `src/ui/chat/terminal.rs`: terminal setup, restore, panic hooks
    - `src/ui/chat/overlays/`: self-contained overlay modules (state + update + render)
      - `src/ui/chat/overlays/palette.rs`: command palette overlay
      - `src/ui/chat/overlays/model_picker.rs`: model picker overlay
      - `src/ui/chat/overlays/thinking_picker.rs`: thinking level picker overlay
      - `src/ui/chat/overlays/session_picker.rs`: session picker overlay
      - `src/ui/chat/overlays/file_picker.rs`: file picker overlay (triggered by `@`, async file discovery, fuzzy filtering)
      - `src/ui/chat/overlays/login.rs`: OAuth login flow overlay
      - `src/ui/chat/overlays/mod.rs`: overlay exports
  - `src/ui/markdown/`: markdown parsing and wrapping (shared)
    - `src/ui/markdown/mod.rs`: module exports
    - `src/ui/markdown/parse.rs`: markdown parsing + rendering
    - `src/ui/markdown/wrap.rs`: styled span wrapping
    - `src/ui/markdown/stream.rs`: streaming collector + commit logic
  - `src/ui/transcript/`: transcript model (shared)
    - `src/ui/transcript/mod.rs`: module exports
    - `src/ui/transcript/cell.rs`: HistoryCell + rendering
    - `src/ui/transcript/wrap.rs`: wrapping + wrap cache
    - `src/ui/transcript/style.rs`: transcript style types
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
- Keep `src/core/` UI-agnostic: terminal I/O belongs in `src/ui/` only

## Tests (keep it light)

- Add tests only to protect a user-visible contract or a real regression.
- Prefer integration tests in `tests/` over unit tests for CLI/output/persistence behavior.
- Avoid mutating process-global env vars in-process; set env on spawned CLI commands instead.

## Docs

- `docs/SPEC.md`: contracts (what/behavior)
- `docs/ARCHITECTURE.md`: system architecture and design
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
