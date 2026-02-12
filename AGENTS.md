# zdx development guide

`docs/SPEC.md` is the source of truth for product behavior (contracts). This file is about working in the repo.

## Where things are

- `crates/zdx-core/`: core library (engine, providers, tools, config)
  - `crates/zdx-core/src/lib.rs`: core crate exports
  - `crates/zdx-core/src/config.rs`: config loading + paths
  - `crates/zdx-core/src/models.rs`: model registry for TUI model picker
  - `crates/zdx-core/src/prompts.rs`: prompt template helpers
  - `crates/zdx-core/src/skills.rs`: skills discovery + parsing
  - `crates/zdx-core/default_config.toml`: default configuration template
  - `crates/zdx-core/default_models.toml`: default model registry fallback
  - `crates/zdx-core/prompts/`: prompt templates
    - `crates/zdx-core/prompts/openai_codex.md`: Codex system prompt template
    - `crates/zdx-core/prompts/zdx_agentic.md`: Unified agentic system prompt (used by Gemini, StepFun, Moonshot)
  - `crates/zdx-core/src/core/`: UI-agnostic domain + runtime
    - `crates/zdx-core/src/core/mod.rs`: core module exports
    - `crates/zdx-core/src/core/events.rs`: agent event types for streaming
    - `crates/zdx-core/src/core/context.rs`: project context loading (AGENTS.md files)
    - `crates/zdx-core/src/core/interrupt.rs`: signal handling
    - `crates/zdx-core/src/core/agent.rs`: agent loop + event channels
    - `crates/zdx-core/src/core/subagent.rs`: reusable child `zdx exec` subagent runner
    - `crates/zdx-core/src/core/thread_persistence.rs`: thread persistence
    - `crates/zdx-core/src/core/worktree.rs`: git worktree management helpers
  - `crates/zdx-core/src/tools/`: tool implementations + schemas
    - `crates/zdx-core/src/tools/apply_patch/`: apply_patch tool (Codex-style file patching)
      - `crates/zdx-core/src/tools/apply_patch/mod.rs`: tool definition, execution wrapper, patch application engine
      - `crates/zdx-core/src/tools/apply_patch/parser.rs`: patch parser for file hunks
      - `crates/zdx-core/src/tools/apply_patch/types.rs`: Hunk enum, UpdateFileChunk, ParseError
    - `crates/zdx-core/src/tools/fetch_webpage.rs`: fetch webpage tool (Parallel Extract API)
    - `crates/zdx-core/src/tools/read_thread.rs`: read thread tool (subagent prompt over thread transcript)
    - `crates/zdx-core/src/tools/subagent.rs`: invoke_subagent delegation tool (isolated child `zdx exec`)
    - `crates/zdx-core/src/tools/web_search.rs`: web search tool (Parallel Search API)
  - `crates/zdx-core/src/providers/`: provider clients + OAuth helpers
    - `crates/zdx-core/src/providers/shared.rs`: provider-agnostic types + helpers (merge_system_prompt, config resolution)
    - `crates/zdx-core/src/providers/debug_metrics.rs`: stream metrics wrapper for all provider SSE streams (`ZDX_DEBUG_STREAM`)
    - `crates/zdx-core/src/providers/thinking_parser.rs`: parser for `<think>`/`</think>` reasoning blocks (handles content bleeding)
    - `crates/zdx-core/src/providers/text_tool_parser.rs`: parser for XML-like text tool calls (`<tool_call>`, `<function=>`)
    - `crates/zdx-core/src/providers/openai/`: OpenAI-compatible provider helpers (Responses + Chat Completions)
      - `crates/zdx-core/src/providers/openai/mod.rs`: OpenAI provider module exports
      - `crates/zdx-core/src/providers/openai/api.rs`: OpenAI API key provider (Responses API)
      - `crates/zdx-core/src/providers/openai/codex.rs`: OpenAI Codex OAuth provider (Responses API)
      - `crates/zdx-core/src/providers/openai/responses.rs`: Responses API helpers
      - `crates/zdx-core/src/providers/openai/responses_sse.rs`: Responses SSE parser
      - `crates/zdx-core/src/providers/openai/responses_types.rs`: Responses request/response types
      - `crates/zdx-core/src/providers/openai/chat_completions.rs`: OpenAI-compatible Chat Completions helpers
    - `crates/zdx-core/src/providers/anthropic/`: Anthropic Claude providers (API key + CLI OAuth)
      - `crates/zdx-core/src/providers/anthropic/mod.rs`: Anthropic provider module exports
      - `crates/zdx-core/src/providers/anthropic/api.rs`: Anthropic API key provider (Messages API)
      - `crates/zdx-core/src/providers/anthropic/cli.rs`: Claude CLI OAuth provider (Messages API)
      - `crates/zdx-core/src/providers/anthropic/shared.rs`: shared helpers (message builders, request helpers)
      - `crates/zdx-core/src/providers/anthropic/types.rs`: API request/response types
      - `crates/zdx-core/src/providers/anthropic/sse.rs`: Anthropic SSE parser
    - `crates/zdx-core/src/providers/gemini/`: Gemini provider helpers (API key + CLI OAuth)
      - `crates/zdx-core/src/providers/gemini/mod.rs`: Gemini provider module exports
      - `crates/zdx-core/src/providers/gemini/api.rs`: Gemini API key provider (Generative Language API)
      - `crates/zdx-core/src/providers/gemini/cli.rs`: Gemini CLI OAuth provider (Cloud Code Assist API)
      - `crates/zdx-core/src/providers/gemini/shared.rs`: shared helpers (request builders, thinking config)
      - `crates/zdx-core/src/providers/gemini/sse.rs`: Gemini SSE parser
    - `crates/zdx-core/src/providers/mistral.rs`: Mistral OpenAI-compatible chat completions provider
    - `crates/zdx-core/src/providers/moonshot.rs`: Moonshot (Kimi) OpenAI-compatible chat completions provider
    - `crates/zdx-core/src/providers/stepfun.rs`: StepFun (Step-3.5-Flash) OpenAI-compatible chat completions provider
    - `crates/zdx-core/src/providers/mimo.rs`: MiMo (Xiaomi MiMo) OpenAI-compatible chat completions provider
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
      - `crates/zdx-tui/src/runtime/handlers/skills.rs`: skill fetch/install handlers (GitHub API)
      - `crates/zdx-tui/src/runtime/handoff.rs`: handoff generation handlers
      - `crates/zdx-tui/src/runtime/thread_title.rs`: auto-title handlers
    - `crates/zdx-tui/src/common/`: shared leaf types (no feature deps)
    - `crates/zdx-tui/src/features/`: feature slices (state/update/render per slice)
      - `crates/zdx-tui/src/features/auth/`: auth feature slice
      - `crates/zdx-tui/src/features/input/`: input feature slice
        - `crates/zdx-tui/src/features/input/text_buffer.rs`: minimal text buffer + cursor editing for input
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
      - `crates/zdx-tui/src/overlays/command_palette.rs`: command palette overlay (Ctrl+O or `/` when input empty)
      - `crates/zdx-tui/src/overlays/skill_picker.rs`: skill installer overlay
      - `crates/zdx-tui/src/overlays/rename.rs`: thread rename overlay
- `crates/zdx-cli/`: zdx binary (CLI/router)
  - `crates/zdx-cli/src/main.rs`: binary entrypoint (delegates to `crates/zdx-cli/src/cli/`)
  - `crates/zdx-cli/src/cli/`: CLI arguments + command dispatch
  - `crates/zdx-cli/src/cli/commands/telegram.rs`: Telegram utility commands (create topics, send messages)
  - `crates/zdx-cli/src/cli/commands/worktree.rs`: worktree command handler
  - `crates/zdx-cli/src/modes/exec.rs`: non-interactive streaming mode (stdout/stderr rendering)
  - `crates/zdx-cli/src/modes/mod.rs`: mode exports (exec + TUI feature-gated)
- `crates/zdx-bot/`: Telegram bot library (long-polling)
  - `crates/zdx-bot/src/lib.rs`: Telegram bot library entrypoint (used by CLI subcommand)
  - `crates/zdx-bot/src/bot/mod.rs`: bot module exports
  - `crates/zdx-bot/src/bot/context.rs`: shared bot context
  - `crates/zdx-bot/src/bot/queue.rs`: per-chat queueing helpers
  - `crates/zdx-bot/src/handlers/mod.rs`: message handler module exports
  - `crates/zdx-bot/src/handlers/message.rs`: Telegram message flow orchestration
  - `crates/zdx-bot/src/ingest/mod.rs`: Telegram message parsing + attachment loading
  - `crates/zdx-bot/src/agent/mod.rs`: thread log + agent turn helpers
  - `crates/zdx-bot/src/telegram/mod.rs`: Telegram API client + tool wiring
  - `crates/zdx-bot/src/telegram/types.rs`: Telegram API DTOs
  - `crates/zdx-bot/src/transcribe.rs`: OpenAI audio transcription helper for Telegram audio
  - `crates/zdx-bot/src/types.rs`: Telegram bot message/media structs
- `tools/scripts/`: optional repo scripts (seed/import/dev helpers)
- `.github/workflows/`: CI workflows
- `.cargo/config.toml`: cargo alias for `cargo xtask`, shared target dir config
- `crates/xtask/`: maintainer utilities (update default models/config, codebase snapshot)
- `crates/zdx-cli/tests/`: integration tests (`assert_cmd`, fixtures)
- `justfile`: task runner recipes (run `just` to list all)

## Build / run

All common tasks are available via `just` (see `justfile`). Run `just` to list all recipes.

- `just run` (interactive TUI; pass extra args: `just run --help`)
- `just bot` (Telegram bot; requires config telegram.\* keys)
- `just bot-loop` (Telegram bot with auto-restart on exit code 42)
- `just ci` (full local CI: lint + test)
- `just lint` (format + clippy)
- `just fmt` (nightly rustfmt)
- `just clippy` (lint only)
- `just test` (fast path; skips doc tests)
- `just update-defaults` (maintainer: refresh both default_models.toml + default_config.toml)
- `just update-models` (maintainer: refresh default_models.toml)
- `just update-config` (maintainer: refresh default_config.toml)
- `just codebase` (generate codebase.txt for entire workspace)
- `just codebase crates/zdx-tui` (generate codebase.txt for specific crate)
- `just build-release` (build release binary)
- Release automation: `.github/workflows/release-please.yml` (config in `release-please-config.json`)

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

## ⚠️ IMPORTANT: Keep this file up to date

**This is mandatory, not optional.** When you:

- **Add a new `.rs` file** → Add it to "Where things are" with a one-line description
- **Move/rename a module** → Update the path in "Where things are"
- **Delete a file** → Remove it from "Where things are"
- **Change build/run/test workflows** → Update "Build / run"
- **Add new conventions** → Document here or in scoped `AGENTS.md` files
- **Change system architecture** → Update `docs/ARCHITECTURE.md` (module relationships, data flow, component boundaries, or design patterns)
