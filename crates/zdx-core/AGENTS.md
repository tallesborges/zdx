# zdx-core development guide

Scope: core runtime, providers, tools, prompt/context assembly, and shared config/model registries.

## Where things are

- `src/lib.rs`: core crate exports
- `src/automations.rs`: automation discovery + frontmatter parsing
- `src/config.rs`: config loading + paths
- `src/models.rs`: model registry for model picker
- `src/prompts.rs`: prompt template helpers
- `src/skills.rs`: skills discovery + parsing
- `default_config.toml`: default configuration template
- `default_models.toml`: default model registry fallback
- `prompts/system_prompt_template.md`: unified MiniJinja system prompt template

### Core runtime (`src/core/`)

- `core/mod.rs`: core module exports
- `core/events.rs`: agent event types for streaming
- `core/context.rs`: project context loading (`AGENTS.md`, memory)
- `core/interrupt.rs`: signal handling
- `core/agent.rs`: agent loop + event channels
- `core/subagent.rs`: child `zdx exec` subagent runner
- `core/title_generation.rs`: LLM-based title generation (shared by TUI + bot)
- `core/thread_persistence.rs`: thread persistence
- `core/worktree.rs`: git worktree management helpers

### Tools (`src/tools/`)

- `tools/apply_patch/mod.rs`: apply_patch tool definition + patch engine
- `tools/apply_patch/parser.rs`: patch parser
- `tools/apply_patch/types.rs`: patch parser shared types
- `tools/fetch_webpage.rs`: webpage extraction tool
- `tools/read_thread.rs`: read saved thread transcript tool
- `tools/subagent.rs`: invoke_subagent tool
- `tools/thread_search.rs`: thread discovery tool
- `tools/web_search.rs`: web search tool

### Providers (`src/providers/`)

- `providers/shared.rs`: provider-agnostic helpers/types
- `providers/debug_metrics.rs`: stream debug metrics wrapper
- `providers/thinking_parser.rs`: `<think>` parser
- `providers/text_tool_parser.rs`: XML-like text tool-call parser
- `providers/openai/`: OpenAI-compatible providers (Responses + Chat Completions)
- `providers/anthropic/`: Anthropic providers (API key + CLI OAuth)
- `providers/gemini/`: Gemini providers (API key + CLI OAuth)
- `providers/mistral.rs`: Mistral provider
- `providers/zen.rs`: Zen provider (OpenCode)
- `providers/apiyi.rs`: APIYI provider
- `providers/moonshot.rs`: Moonshot provider
- `providers/stepfun.rs`: StepFun provider
- `providers/mimo.rs`: MiMo provider

## Conventions

- Keep `zdx-core` UI-agnostic.
- No direct terminal UI logic here; terminal behavior belongs in `zdx-tui`.
- Prefer `anyhow::Result` + `Context` at I/O boundaries.

## Checks

- Targeted: `cargo test -p zdx-core`
- Workspace lint/test: use `just lint` / `just test` from repo root

## Maintenance

- Add/move/delete `.rs` files in this crate: update this file.
- Architecture changes: update `docs/ARCHITECTURE.md`.