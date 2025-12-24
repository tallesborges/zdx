# zdx-cli development guide

`docs/SPEC.md` is the source of truth for product behavior (contracts). This file is about working in the repo.

## Where things are

- `src/main.rs`: CLI entrypoint + `clap` args
- `src/config.rs`: config loading + paths
- `src/models.rs`: model registry for TUI model picker
- `src/models_generated.rs`: generated model data (from `cargo run --bin generate_models`)
- `src/bin/generate_models.rs`: binary to generate model data from API
- `src/core/`: UI-agnostic domain + runtime
  - `src/core/events.rs`: engine event types for streaming
  - `src/core/context.rs`: project context loading (AGENTS.md files)
  - `src/core/interrupt.rs`: signal handling
  - `src/core/engine.rs`: engine loop + event channels
  - `src/core/session.rs`: session persistence
- `src/ui/`: terminal UI (Elm-like architecture)
  - `src/ui/tui.rs`: TuiRuntime - owns terminal, runs event loop, executes effects
  - `src/ui/state.rs`: TuiState - all app state (no terminal)
  - `src/ui/update.rs`: reducer - all state mutations happen here
  - `src/ui/view.rs`: pure render functions (no mutations)
  - `src/ui/effects.rs`: effect types returned by reducer for runtime to execute
  - `src/ui/events.rs`: UI event types
  - `src/ui/markdown.rs`: markdown parsing and styled text wrapping for assistant responses
  - `src/ui/overlays/`: self-contained overlay modules (state + update + render)
    - `src/ui/overlays/palette.rs`: command palette overlay
    - `src/ui/overlays/model_picker.rs`: model picker overlay
    - `src/ui/overlays/thinking_picker.rs`: thinking level picker overlay
    - `src/ui/overlays/login.rs`: OAuth login flow overlay
  - `src/ui/commands.rs`: slash command definitions for command palette
  - `src/ui/transcript.rs`: transcript view model (styles, wrapping, rendering)
  - `src/ui/stream.rs`: streamed stdout/stderr rendering + exec mode wrapper
  - `src/ui/terminal.rs`: terminal setup, restore, panic hooks
- `src/tools/`: tool implementations + schemas (bash, edit, read, write)
- `src/providers/`: provider clients
  - `src/providers/anthropic.rs`: Anthropic API client
  - `src/providers/oauth.rs`: OAuth token storage + retrieval
- `tests/`: integration tests (`assert_cmd`, fixtures)

## Build / run

- `cargo run -- --help`
- `cargo run --` (interactive; needs provider key via env)
- `cargo test`
- `cargo fmt`
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
- `docs/adr/`: durable decisions (why)
- `docs/plans/`: optional commit-sized plans (how)

## ⚠️ IMPORTANT: Keep this file up to date

**This is mandatory, not optional.** When you:

- **Add a new `.rs` file** → Add it to "Where things are" with a one-line description
- **Move/rename a module** → Update the path in "Where things are"
- **Delete a file** → Remove it from "Where things are"
- **Change build/run/test workflows** → Update "Build / run"
- **Add new conventions** → Document here or in scoped `AGENTS.md` files
