# zdx development guide

`docs/SPEC.md` is the source of truth for product behavior (contracts). This file is about working in the repo.

## Where things are

- `zdx-core/`: core library (engine, providers, tools, config)
  - `zdx-core/src/lib.rs`: core crate exports
  - `zdx-core/src/config.rs`: config loading + paths
  - `zdx-core/src/models.rs`: model registry for TUI model picker
  - `zdx-core/src/prompts.rs`: `prompt_str!` macro for including prompts
  - `zdx-core/default_config.toml`: default configuration template
  - `zdx-core/default_models.toml`: default model registry fallback
  - `zdx-core/prompts/`: prompt templates (included via `zdx_core::prompt_str!`)
  - `zdx-core/src/core/`: UI-agnostic domain + runtime
    - `zdx-core/src/core/mod.rs`: core module exports
    - `zdx-core/src/core/events.rs`: agent event types for streaming
    - `zdx-core/src/core/context.rs`: project context loading (AGENTS.md files)
    - `zdx-core/src/core/interrupt.rs`: signal handling
    - `zdx-core/src/core/agent.rs`: agent loop + event channels
    - `zdx-core/src/core/thread_log.rs`: thread persistence
  - `zdx-core/src/tools/`: tool implementations + schemas
    - `zdx-core/src/tools/apply_patch/`: apply_patch tool (Codex-style file patching)
      - `zdx-core/src/tools/apply_patch/mod.rs`: tool definition, execution wrapper, patch application engine
      - `zdx-core/src/tools/apply_patch/parser.rs`: patch parser for file hunks
      - `zdx-core/src/tools/apply_patch/types.rs`: Hunk enum, UpdateFileChunk, ParseError
  - `zdx-core/src/providers/`: provider clients + OAuth helpers
- `zdx-tui/`: full-screen interactive TUI library
  - `zdx-tui/src/lib.rs`: TUI exports (run_interactive_chat, TuiRuntime)
  - `zdx-tui/src/terminal.rs`: terminal setup, restore, panic hooks
  - `zdx-tui/src/`: full-screen TUI (Elm-like architecture)
    - `zdx-tui/src/state.rs`: AppState + TuiState + AgentState
    - `zdx-tui/src/events.rs`: UiEvent + SessionUiEvent
    - `zdx-tui/src/update.rs`: reducer
    - `zdx-tui/src/render.rs`: pure render functions
    - `zdx-tui/src/effects.rs`: UiEffect definitions (side-effect descriptions)
    - `zdx-tui/src/mutations.rs`: StateMutation + cross-slice mutations
    - `zdx-tui/src/runtime/`: runtime + effect dispatch
      - `zdx-tui/src/runtime/mod.rs`: TuiRuntime - owns terminal, runs event loop
      - `zdx-tui/src/runtime/inbox.rs`: inbox channel types
      - `zdx-tui/src/runtime/handlers/`: effect handlers (thread ops, agent spawn, auth)
      - `zdx-tui/src/runtime/handoff.rs`: handoff generation handlers
      - `zdx-tui/src/runtime/thread_title.rs`: auto-title handlers
    - `zdx-tui/src/common/`: shared leaf types (no feature deps)
    - `zdx-tui/src/features/`: feature slices (state/update/render per slice)
      - `zdx-tui/src/features/auth/`: auth feature slice
      - `zdx-tui/src/features/input/`: input feature slice
      - `zdx-tui/src/features/thread/`: thread feature slice
      - `zdx-tui/src/features/transcript/`: transcript feature slice
        - `zdx-tui/src/features/transcript/markdown/`: markdown parsing + wrapping
    - `zdx-tui/src/overlays/`: overlay feature slice
- `src/`: zdx binary (CLI/router)
  - `src/main.rs`: binary entrypoint (delegates to `src/cli/`)
  - `src/cli/`: CLI arguments + command dispatch
  - `src/modes/exec.rs`: non-interactive streaming mode (stdout/stderr rendering)
  - `src/modes/mod.rs`: mode exports (exec + TUI feature-gated)
- `.cargo/config.toml`: cargo alias for `cargo xtask`
- `xtask/`: maintainer utilities (update default models/config, codebase snapshot)
- `tests/`: integration tests (`assert_cmd`, fixtures)

## Build / run

- `cargo run -p zdx -- --help`
- `cargo run -p zdx --` (interactive; needs provider key via env)
- `cargo xtask update-default-models` (maintainer: refresh default_models.toml)
- `cargo xtask update-default-config` (maintainer: refresh default_config.toml)
- `cargo xtask update-defaults` (maintainer: refresh both defaults)
- `cargo test`
- `cargo +nightly fmt` (uses nightly for full rustfmt features; stable works but ignores some options)
- `cargo clippy`

## Conventions

- Rust edition: 2024 (see `Cargo.toml`)
- Formatting: rustfmt defaults
- Errors: prefer `anyhow::Result` + `Context` at I/O boundaries
- Keep `zdx-core` UI-agnostic: terminal I/O lives in `zdx-tui/src/terminal.rs`

## Tests (keep it light)

- Add tests only to protect a user-visible contract or a real regression.
- Prefer integration tests in `tests/` over unit tests for CLI/output/persistence behavior.
- Avoid mutating process-global env vars in-process; set env on spawned CLI commands instead.

## Docs

- `docs/SPEC.md`: contracts (what/behavior)
- `docs/ARCHITECTURE.md`: system architecture and design (Elm/MVU patterns, key patterns)
- `docs/plans/`: optional commit-sized plans (how)

## Delegating tasks (subagent pattern)

When a task is complex or would pollute the current context, delegate to a fresh zdx instance:

```bash
# If zdx is in PATH:
zdx --no-thread exec -p "your task description"
```

This runs in an isolated process with its own context window. Use for:
- Reading large files and summarizing
- Complex multi-step analysis
- Research tasks that generate lots of intermediate output
- Any task where you only need the final result

The `--no-thread` flag prevents thread file creation. Output is returned directly.

**Reading previous threads:**
```bash
zdx threads show <thread_id>
```

Use this to fetch context from a previous thread when needed.

## ⚠️ IMPORTANT: Keep this file up to date

**This is mandatory, not optional.** When you:

- **Add a new `.rs` file** → Add it to "Where things are" with a one-line description
- **Move/rename a module** → Update the path in "Where things are"
- **Delete a file** → Remove it from "Where things are"
- **Change build/run/test workflows** → Update "Build / run"
- **Add new conventions** → Document here or in scoped `AGENTS.md` files
- **Change system architecture** → Update `docs/ARCHITECTURE.md` (module relationships, data flow, component boundaries, or design patterns)
