# zdx-core development guide

Scope: core runtime, providers, tools, prompt/context assembly, and shared config/model registries.

## Where things are

- `src/lib.rs`: core crate exports
- `src/automations.rs`: automation discovery + frontmatter parsing
- `src/config.rs`: config loading + paths
- `src/models.rs`: model registry for model picker
- `src/prompts.rs`: prompt template helpers
- `prompts/identity_prompt.md`: shared minimal identity prompt for backend-safe/system prompt reuse
- `src/skills.rs`: skills discovery + parsing
- `src/images/mod.rs`: shared image utilities module exports
- `src/images/decode.rs`: generic image decode/resize/PNG encode helpers
- `src/images/path_mime.rs`: path normalization + extension MIME helpers
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
- `tools/glob.rs`: native glob tool (file discovery by name pattern)
- `tools/grep.rs`: native grep tool (structured regex search using ripgrep internals)
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
- `providers/xiomi.rs`: Xiomi provider

## Conventions

- Keep `zdx-core` UI-agnostic.
- No direct terminal UI logic here; terminal behavior belongs in `zdx-tui`.
- Prefer `anyhow::Result` + `Context` at I/O boundaries.

## Checks

- Default final verification after code changes: `just ci` from repo root
- Intermediate iteration for this crate: `cargo test -p zdx-core`
- Use `just lint` or `just test` only when intentionally running one half of CI

## Adding or updating models

Three files must be updated (in this order):

1. **`src/config.rs`** — hardcoded provider defaults (e.g. `default_xiaomi_provider()`).
   Add the model ID to the provider's `models` vec. This is the source of truth for
   `default_config.toml` generation.
2. **`default_models.toml`** — model entries with pricing, capabilities, context limits.
   Add a full `[[model]]` block. Use the OpenRouter API for accurate pricing/limits:
   ```
   curl -s https://openrouter.ai/api/v1/models | jq '.data[] | select(.id | test("model-name")) | {id, pricing, context_length, top_provider}'
   ```
   Pricing is per-token; multiply by 1,000,000 for per-M values.
3. **`default_config.toml`** — **do not edit directly**. It is generated from `config.rs`.

### Workflow

```bash
# 1. Edit config.rs + default_models.toml
# 2. Build so the binary embeds your changes
cargo build
# 3. Regenerate both default files (config from config.rs, models from upstream + defaults)
just update-defaults
# 4. Re-apply any model entries that got overwritten with "(custom)" placeholders
#    (models not found in models.dev upstream get zeroed out by update-defaults)
# 5. Update local user config (~/.zdx/config.toml) manually
# 6. Update local models registry
cargo run -- models update
```

### Gotchas

- `just update-defaults` regenerates `default_models.toml` from an external source.
  Models not in models.dev will be created as placeholder "(custom)" entries with zero
  pricing. You must re-apply correct values after the update.
- Always `cargo build` before running update commands — the binary must embed the
  latest `config.rs` and `default_config.toml` changes.
- `default_config.toml` is generated output. To change provider model lists,
  edit the `default_*_provider()` functions in `config.rs`.

## Maintenance

- Add/move/delete `.rs` files in this crate: update this file.
- Architecture changes: update `docs/ARCHITECTURE.md`.