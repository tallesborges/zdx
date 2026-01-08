# zdx development guide

`docs/SPEC.md` is the source of truth for product behavior (contracts). This file is about working in the repo.

## Where things are

- `src/main.rs`: binary entrypoint (delegates to `src/cli/`)
- `default_config.toml`: default configuration template
- `default_models.toml`: default model registry fallback
- `.cargo/config.toml`: cargo alias for `cargo xtask`
- `xtask/`: maintainer utilities (update default_models.toml)
- `src/cli/`: CLI arguments + command dispatch
  - `src/cli/mod.rs`: clap structs + dispatch
  - `src/cli/commands/mod.rs`: command module exports
  - `src/cli/commands/chat.rs`: chat command handler (includes piped stdin fallback)
  - `src/cli/commands/exec.rs`: exec command handler
  - `src/cli/commands/threads.rs`: list/show/resume threads
  - `src/cli/commands/config.rs`: config path/init handlers
  - `src/cli/commands/auth.rs`: login/logout flows
  - `src/cli/commands/models.rs`: models update handler (models.dev → models.toml)
- `src/config.rs`: config loading + paths
- `src/models.rs`: model registry for TUI model picker
- `src/core/`: UI-agnostic domain + runtime
  - `src/core/mod.rs`: core module exports
  - `src/core/events.rs`: agent event types for streaming
  - `src/core/context.rs`: project context loading (AGENTS.md files)
  - `src/core/interrupt.rs`: signal handling
  - `src/core/agent.rs`: agent loop + event channels
  - `src/core/thread_log.rs`: thread persistence
- `src/modes/`: runtime execution modes
  - `src/modes/mod.rs`: mode module exports
  - `src/modes/exec.rs`: non-interactive streaming mode (stdout/stderr rendering)
  - `src/modes/tui/`: full-screen interactive TUI (Elm-like architecture)
    - `src/modes/tui/mod.rs`: entry points (run_interactive_chat) + module declarations
    - `src/modes/tui/app.rs`: AppState + TuiState + AgentState (state composition, hierarchy)
    - `src/modes/tui/runtime/mod.rs`: TuiRuntime - owns terminal, runs event loop, effect dispatch
    - `src/modes/tui/runtime/handlers.rs`: effect handlers (thread ops, agent spawn, auth)
    - `src/modes/tui/runtime/handoff.rs`: handoff generation handlers (subagent spawning)
    - `src/modes/tui/update.rs`: reducer - orchestrates state mutations, delegates to feature slices
    - `src/modes/tui/render.rs`: pure render functions (no mutations), delegates transcript rendering
    - `src/modes/tui/events.rs`: UiEvent + SessionUiEvent (aggregator for all TUI events)
    - `src/modes/tui/terminal.rs`: terminal setup, restore, panic hooks
    - `src/modes/tui/shared/`: shared leaf types (no feature dependencies)
      - `src/modes/tui/shared/mod.rs`: module exports
      - `src/modes/tui/shared/effects.rs`: effect types returned by reducer for runtime to execute
      - `src/modes/tui/shared/commands.rs`: command definitions for command palette
      - `src/modes/tui/shared/internal.rs`: StateCommand + cross-slice mutation enums (applied via slice `apply()`)
      - `src/modes/tui/shared/clipboard.rs`: clipboard I/O (OSC 52 + arboard fallback)
      - `src/modes/tui/shared/request_id.rs`: request id + latest-only helper for async result gating
      - `src/modes/tui/shared/scrollbar.rs`: custom scrollbar widget with stable thumb size
      - `src/modes/tui/shared/text.rs`: text utilities (tab expansion, display sanitization)
    - `src/modes/tui/auth/`: auth feature slice (authentication state, login handling)
      - `src/modes/tui/auth/mod.rs`: module exports
      - `src/modes/tui/auth/state.rs`: AuthStatus + AuthState (auth type detection, login flow state)
      - `src/modes/tui/auth/update.rs`: login result handling, OAuth flow state transitions
      - `src/modes/tui/auth/render.rs`: login overlay rendering
    - `src/modes/tui/input/`: input feature slice (keyboard handling, handoff)
      - `src/modes/tui/input/mod.rs`: module exports
      - `src/modes/tui/input/state.rs`: InputState + HandoffState
      - `src/modes/tui/input/update.rs`: key handling, input submission, handoff result handling
      - `src/modes/tui/input/render.rs`: input area rendering (normal + handoff modes)
    - `src/modes/tui/thread/`: thread feature slice (thread state, thread operations)
      - `src/modes/tui/thread/mod.rs`: module exports
      - `src/modes/tui/thread/state.rs`: ThreadState, ThreadOpsState, ThreadUsage
      - `src/modes/tui/thread/update.rs`: thread event handlers (loading, switching, creating, renaming)
      - `src/modes/tui/thread/render.rs`: thread picker overlay rendering
    - `src/modes/tui/overlays/`: overlay feature slice (modal UI components)
      - `src/modes/tui/overlays/mod.rs`: `Overlay` enum, `OverlayRequest`, `OverlayTransition`, `OverlayUpdate`, `OverlayExt` render helpers
      - `src/modes/tui/overlays/update.rs`: overlay key handling and update logic
      - `src/modes/tui/overlays/render_utils.rs`: shared rendering utilities for overlays
      - `src/modes/tui/overlays/command_palette.rs`: command palette overlay
      - `src/modes/tui/overlays/model_picker.rs`: model picker overlay
      - `src/modes/tui/overlays/thinking_picker.rs`: thinking level picker overlay
      - `src/modes/tui/overlays/timeline.rs`: timeline overlay (jump/fork from turn)
      - `src/modes/tui/overlays/thread_picker.rs`: thread picker overlay (state + key handling; rendering delegated to thread feature)
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
      - `src/modes/tui/transcript/build.rs`: pure helper to build transcript cells from thread events
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
  - `src/providers/shared.rs`: provider-agnostic chat/error/stream types
  - `src/providers/anthropic/`: Anthropic API client
    - `src/providers/anthropic/mod.rs`: public re-exports
    - `src/providers/anthropic/auth.rs`: auth resolution + config
    - `src/providers/anthropic/client.rs`: AnthropicClient + request wiring
    - `src/providers/anthropic/sse.rs`: SSE parsing + stream events
    - `src/providers/anthropic/types.rs`: API DTOs + request/response shapes
  - `src/providers/openai_responses/`: shared OpenAI Responses API helpers
    - `src/providers/openai_responses/mod.rs`: shared request builder + dispatcher
    - `src/providers/openai_responses/sse.rs`: SSE parsing for Responses API
    - `src/providers/openai_responses/types.rs`: request DTOs
  - `src/providers/openai_codex/`: OpenAI Codex (ChatGPT OAuth) client
    - `src/providers/openai_codex/mod.rs`: module exports
    - `src/providers/openai_codex/auth.rs`: OAuth credential resolution + config
    - `src/providers/openai_codex/client.rs`: OpenAICodexClient + request wiring
    - `src/providers/openai_codex/prompts/mod.rs`: Codex instruction loading and model normalization
    - `src/providers/openai_codex/prompts/gpt_5_codex_prompt.md`: Codex instructions (gpt-5 codex)
    - `src/providers/openai_codex/prompts/gpt-5.2-codex_prompt.md`: Codex instructions (gpt-5.2 codex)
    - `src/providers/openai_codex/prompts/gpt-5.1-codex-max_prompt.md`: Codex instructions (gpt-5.1 codex max)
    - `src/providers/openai_codex/prompts/gpt_5_2_prompt.md`: Instructions (gpt-5.2)
    - `src/providers/openai_codex/prompts/gpt_5_1_prompt.md`: Instructions (gpt-5.1)
  - `src/providers/openai_api.rs`: OpenAI API key provider (Responses API)
  - `src/providers/openrouter.rs`: OpenRouter provider (OpenAI-compatible chat completions)
  - `src/providers/gemini.rs`: Gemini provider (Generative Language API)
  - `src/providers/oauth.rs`: OAuth token storage + retrieval
- `tests/`: integration tests (`assert_cmd`, fixtures)

## Build / run

- `cargo run -- --help`
- `cargo run --` (interactive; needs provider key via env)
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
- Keep `src/core/` UI-agnostic: terminal I/O belongs in `src/modes/` only

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
