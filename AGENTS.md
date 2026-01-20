# zdx development guide

`docs/SPEC.md` is the source of truth for product behavior (contracts). This file is about working in the repo.

## Where things are

- `crates/zdx-core/`: core library (engine, providers, tools, config)
  - `crates/zdx-core/src/lib.rs`: core crate exports
  - `crates/zdx-core/src/config.rs`: config loading + paths
  - `crates/zdx-core/src/models.rs`: model registry for TUI model picker
  - `crates/zdx-core/src/prompts.rs`: prompt template helpers
  - `crates/zdx-core/default_config.toml`: default configuration template
  - `crates/zdx-core/default_models.toml`: default model registry fallback
  - `crates/zdx-core/prompts/`: prompt templates
    - `crates/zdx-core/prompts/openai_codex.md`: Codex system prompt template
  - `crates/zdx-core/src/core/`: UI-agnostic domain + runtime
    - `crates/zdx-core/src/core/mod.rs`: core module exports
    - `crates/zdx-core/src/core/events.rs`: agent event types for streaming
    - `crates/zdx-core/src/core/context.rs`: project context loading (AGENTS.md files)
    - `crates/zdx-core/src/core/interrupt.rs`: signal handling
    - `crates/zdx-core/src/core/agent.rs`: agent loop + event channels
    - `crates/zdx-core/src/core/thread_log.rs`: thread persistence
  - `crates/zdx-core/src/tools/`: tool implementations + schemas
    - `crates/zdx-core/src/tools/apply_patch/`: apply_patch tool (Codex-style file patching)
      - `crates/zdx-core/src/tools/apply_patch/mod.rs`: tool definition, execution wrapper, patch application engine
      - `crates/zdx-core/src/tools/apply_patch/parser.rs`: patch parser for file hunks
      - `crates/zdx-core/src/tools/apply_patch/types.rs`: Hunk enum, UpdateFileChunk, ParseError
  - `crates/zdx-core/src/providers/`: provider clients + OAuth helpers
    - `crates/zdx-core/src/providers/debug_metrics.rs`: stream metrics wrapper for all provider SSE streams (`ZDX_DEBUG_STREAM`)
- `crates/zdx-tui/`: full-screen interactive TUI library
  - `crates/zdx-tui/src/lib.rs`: TUI exports (run_interactive_chat, TuiRuntime)
  - `crates/zdx-tui/src/terminal.rs`: terminal setup, restore, panic hooks
  - `crates/zdx-tui/src/`: full-screen TUI (Elm-like architecture)
    - `crates/zdx-tui/src/state.rs`: AppState + TuiState + AgentState
    - `crates/zdx-tui/src/events.rs`: UiEvent + SessionUiEvent
    - `crates/zdx-tui/src/update.rs`: reducer
    - `crates/zdx-tui/src/render.rs`: pure render functions
    - `crates/zdx-tui/src/effects.rs`: UiEffect definitions (side-effect descriptions)
    - `crates/zdx-tui/src/mutations.rs`: StateMutation + cross-slice mutations
    - `crates/zdx-tui/src/runtime/`: runtime + effect dispatch
      - `crates/zdx-tui/src/runtime/mod.rs`: TuiRuntime - owns terminal, runs event loop
      - `crates/zdx-tui/src/runtime/inbox.rs`: inbox channel types
      - `crates/zdx-tui/src/runtime/handlers/`: effect handlers (thread ops, agent spawn, auth)
      - `crates/zdx-tui/src/runtime/handoff.rs`: handoff generation handlers
      - `crates/zdx-tui/src/runtime/thread_title.rs`: auto-title handlers
    - `crates/zdx-tui/src/common/`: shared leaf types (no feature deps)
    - `crates/zdx-tui/src/features/`: feature slices (state/update/render per slice)
      - `crates/zdx-tui/src/features/auth/`: auth feature slice
      - `crates/zdx-tui/src/features/input/`: input feature slice
      - `crates/zdx-tui/src/features/statusline/`: debug status line feature slice
        - `crates/zdx-tui/src/features/statusline/mod.rs`: module exports
        - `crates/zdx-tui/src/features/statusline/state.rs`: StatusLineAccumulator (mutable), StatusLine (snapshot)
        - `crates/zdx-tui/src/features/statusline/render.rs`: render_debug_status_line function
      - `crates/zdx-tui/src/features/thread/`: thread feature slice
        - `crates/zdx-tui/src/features/thread/mod.rs`: module exports
        - `crates/zdx-tui/src/features/thread/state.rs`: ThreadState, ThreadUsage
        - `crates/zdx-tui/src/features/thread/update.rs`: thread event handlers
        - `crates/zdx-tui/src/features/thread/render.rs`: thread picker rendering
        - `crates/zdx-tui/src/features/thread/tree.rs`: tree derivation for hierarchical display (ThreadDisplayItem, flatten_as_tree)
      - `crates/zdx-tui/src/features/transcript/`: transcript feature slice
        - `crates/zdx-tui/src/features/transcript/markdown/`: markdown parsing + wrapping
    - `crates/zdx-tui/src/overlays/`: overlay feature slice
- `crates/zdx-cli/`: zdx binary (CLI/router)
  - `crates/zdx-cli/src/main.rs`: binary entrypoint (delegates to `crates/zdx-cli/src/cli/`)
  - `crates/zdx-cli/src/cli/`: CLI arguments + command dispatch
  - `crates/zdx-cli/src/modes/exec.rs`: non-interactive streaming mode (stdout/stderr rendering)
  - `crates/zdx-cli/src/modes/mod.rs`: mode exports (exec + TUI feature-gated)
- `tools/scripts/`: optional repo scripts (seed/import/dev helpers)
- `.github/workflows/`: CI workflows
- `.cargo/config.toml`: cargo alias for `cargo xtask`
- `crates/xtask/`: maintainer utilities (update default models/config, codebase snapshot)
- `crates/zdx-cli/tests/`: integration tests (`assert_cmd`, fixtures)

## Build / run

- `cargo run -p zdx -- --help`
- `cargo run -p zdx --` (interactive; needs provider key via env)
- `cargo xtask update-default-models` (maintainer: refresh default_models.toml)
- `cargo xtask update-default-config` (maintainer: refresh default_config.toml)
- `cargo xtask update-defaults` (maintainer: refresh both defaults)
- `cargo test --workspace --lib --tests --bins` (fast path; skips doc tests)
- `cargo +nightly fmt` (uses nightly for full rustfmt features; stable works but ignores some options)
- `cargo clippy`

## Conventions

- Rust edition: 2024 (see `Cargo.toml`)
- Formatting: rustfmt defaults
- Errors: prefer `anyhow::Result` + `Context` at I/O boundaries
- Keep `zdx-core` UI-agnostic: terminal I/O lives in `crates/zdx-tui/src/terminal.rs`

## Tests (keep it light)

- Add tests only to protect a user-visible contract or a real regression.
- Prefer integration tests in `crates/zdx-cli/tests/` over unit tests for CLI/output/persistence behavior.
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
