# zdx-engine development guide

Scope: core runtime engine — config, agent orchestration, tools, prompt/context assembly, and shared registries.

## Where things are

- `src/lib.rs`: engine crate exports
- `src/providers.rs`: re-export of `zdx_providers::*`
- `src/audio/mod.rs`: shared audio module exports
- `src/audio/transcribe.rs`: shared audio transcription helpers (OpenAI/Mistral)
- `src/agent_activity.rs`: active-run registry (ephemeral marker files for agent turns)
- `src/automations.rs`: automation discovery + frontmatter parsing
- `src/config.rs`: config loading + paths (embeds `zdx_assets::DEFAULT_CONFIG_TOML`)
- `src/models.rs`: model registry for model picker (embeds `zdx_assets::DEFAULT_MODELS_TOML`)
- `src/mcp.rs`: MCP config loading, server discovery, helper workspace/runtime, and MCP tool-call execution helpers
- `src/prompts.rs`: prompt template helpers (re-exports of `zdx_assets` prompt constants)
- `src/skills.rs`: skills discovery + parsing (materializes bundled skills from `zdx_assets::bundled_skill_assets()`)
- `src/subagents.rs`: named subagent discovery + parsing (built-in subagents come from `zdx_assets::{EXPLORER_SUBAGENT,THREAD_SEARCHER_SUBAGENT,ORACLE_SUBAGENT}`)
- `src/images/mod.rs`: shared image utilities module exports
- `src/images/decode.rs`: generic image decode/resize/PNG encode helpers
- `src/images/path_mime.rs`: path normalization + extension MIME helpers
- `src/pidfile.rs`: PID file management
- `src/tracing_init.rs`: tracing setup

### Core runtime (`src/core/`)

- `core/mod.rs`: core module exports
- `core/events.rs`: agent event types for streaming
- `core/context.rs`: project context loading (`AGENTS.md`/`CLAUDE.md`, memory)
- `core/interrupt.rs`: signal handling
- `core/agent.rs`: agent loop + event channels
- `core/subagent.rs`: child `zdx exec` subagent runner
- `core/title_generation.rs`: LLM-based title generation (shared by TUI + bot)
- `core/thread_persistence.rs`: thread persistence
- `core/worktree.rs`: git worktree management helpers

### Tools (`src/tools/`)

- `tools/mod.rs`: ToolContext, ToolRegistry, ToolSet, handlers
- `tools/read_thread.rs`: read saved thread transcript tool
- `tools/subagent.rs`: invoke_subagent tool
- `tools/todo_write.rs`: structured todo/task tracking tool
- `tools/thread_search.rs`: thread discovery tool

## Conventions

- Keep `zdx-engine` UI-agnostic.
- No direct terminal UI logic here; terminal behavior belongs in `zdx-tui`.
- Prefer `anyhow::Result` + `Context` at I/O boundaries.

## Checks

- Default final verification after code changes: `just ci` from repo root
- Intermediate iteration for this crate: `cargo test -p zdx-engine`
- Use `just lint` or `just test` only when intentionally running one half of CI

## Adding or updating models

Two files must be updated:

1. **`src/config.rs`** — hardcoded provider defaults (e.g. `default_xiaomi_provider()`).
   Add the model ID to the provider's `models` vec. This is the source of truth for
   `default_config.toml` generation.
2. **`default_models.toml`** — model entries with pricing, capabilities, context limits.
   For models available on OpenRouter, you can skip manual editing — the update command
   fetches pricing/capabilities automatically via the OpenRouter API fallback.
   For models NOT on OpenRouter, add a full `[[model]]` block manually.
3. **`default_config.toml`** — **do not edit directly**. It is generated from `config.rs`.

### Workflow

```bash
# 1. Edit config.rs (add model to provider's models vec)
# 2. Build so the binary embeds your changes
cargo build
# 3. Regenerate both default files
just update-defaults
# 4. Verify entries in default_models.toml (OpenRouter fallback fills pricing/capabilities)
# 5. Update local user config (~/.zdx/config.toml) manually
# 6. Update local models registry
cargo run -- models update
```

### Fallback chain for model data (in `zdx models update`)

1. **models.dev** — primary upstream source
2. **embedded `default_models.toml`** — for models manually added to the file
3. **OpenRouter API** — automatic fallback with pricing, context, reasoning, images
4. **"(custom)" placeholder** — last resort with zero pricing (needs manual fix)

### Gotchas

- `just update-defaults` regenerates `default_models.toml` from external sources.
  Models not in models.dev fall back to OpenRouter, then to "(custom)" placeholders.
- Always `cargo build` before running update commands — the binary must embed the
  latest `config.rs` and `default_config.toml` changes.
- `default_config.toml` is generated output. To change provider model lists,
  edit the `default_*_provider()` functions in `config.rs`.

## Maintenance

- Add/move/delete `.rs` files in this crate: update this file.
- Architecture changes: update `docs/ARCHITECTURE.md`.
