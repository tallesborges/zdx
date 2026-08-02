# zdx-engine development guide

Scope: core runtime engine — config, agent orchestration, tools, prompt/context assembly, and shared registries.

## Where things are

- `src/lib.rs`: engine crate exports
- `src/providers.rs`: re-export of `zdx_providers::*`
- `src/audio/mod.rs`: shared audio module exports
- `src/audio/speak.rs`: shared text-to-speech (TTS) synthesis helpers (OpenAI/Mistral); default OGG/Opus output via ffmpeg transcode with MP3 fallback
- `src/audio/transcribe.rs`: shared audio transcription helpers (OpenAI/Mistral via `/audio/transcriptions`; xAI Grok STT via `/stt`; ElevenLabs Scribe via `/v1/speech-to-text` with `xi-api-key`)
- `src/agent_activity.rs`: active-run registry (ephemeral marker files for agent turns)
- `src/background_activity.rs`: background-process registry — durable marker files under `~/.zdx/run/background/` for long-lived processes started with the Bash tool's `background: true`; identity-guarded (`pid`+birth-time+`pgid`) kill defends against PID reuse; exited-tombstone + age prune
- `src/automations.rs`: automation discovery + frontmatter parsing
- `src/config.rs`: config loading + paths (embeds `zdx_assets::DEFAULT_CONFIG_TOML`)
- `src/custom_commands.rs`: custom slash command discovery + frontmatter parsing (`<ZDX_HOME>/commands` + ancestor/current `.zdx/commands`, plus bundled commands from `zdx_assets::bundled_command_assets()`)
- `src/followups.rs`: shared `<followups>` suggestion-block parsing (surfaces strip + render their own way)
- `src/models.rs`: model registry for model picker (embeds `zdx_assets::DEFAULT_MODELS_TOML`)
- `src/mcp.rs`: MCP config loading, server discovery, helper workspace/runtime, and MCP tool-call execution helpers
- `src/prompts.rs`: prompt template helpers/re-exports of `zdx_assets` prompt constants.
- `src/skills.rs`: skills discovery + parsing (materializes bundled skills from `zdx_assets::bundled_skill_assets()`)
- `src/subagents.rs`: named subagent discovery + parsing (built-in subagents come from `zdx_assets::{EXPLORER_SUBAGENT,ORACLE_SUBAGENT}`)
- `src/images/mod.rs`: shared image utilities module exports
- `src/images/decode.rs`: generic image decode/resize/PNG encode helpers
- `src/images/path_mime.rs`: path normalization + extension MIME helpers
- `src/pidfile.rs`: PID file management
- `src/service.rs`: launchd-backed control for the long-lived `bot`/`daemon` services — plist rendering, `install`/`uninstall`/`start`/`stop`/`restart`, and combined launchd+PID-file `state()`. Agents run `~/.local/bin/zdx` (never `current_exe()`) so restart picks up the installed binary, launched via `zsh -c 'exec …'` so `~/.zshenv` (provider API keys, PATH) is sourced. Plists set `ZDX_SERVICE_SUPERVISOR=launchd` and capture output to `~/.zdx/run/logs/{name}.{out,err}`. macOS-only.
- `src/tracing_init.rs`: tracing setup

### Core runtime (`src/core/`)

- `core/mod.rs`: core module exports
- `core/events.rs`: agent event types for streaming
- `core/context.rs`: project context loading (`AGENTS.md`/`CLAUDE.md`, memory)
- `core/interrupt.rs`: signal handling
- `core/agent.rs`: agent loop + event channels
- `core/handoff_generation.rs`: LLM-based handoff context generation (shared by TUI + bot)
- `core/prompt_builder_generation.rs`: LLM-based prompt-builder generation (shared by TUI + bot)
- `core/qmd.rs`: qmd binary discovery and setup helpers
- `core/subagent.rs`: child `zdx exec` subagent runner. Child runs persist their own thread JSONL tagged via `ExecSubagentOptions::thread_origin_kind`/`thread_parent_id`/`thread_subagent_name` (so their usage is captured by `usage_stats`); tagged threads are hidden from default listings.
- `core/thread_export.rs`: clean Markdown transcript exports derived from saved thread JSONL
- `core/title_generation.rs`: LLM-based title generation (shared by TUI + bot)
- `core/tldr_generation.rs`: LLM-based thread TLDR/recap generation (shared by TUI)
- `core/thread_persistence.rs`: thread persistence. `list_threads()` hides child runs (any thread with `Meta.origin_kind` set — subagents/helpers); `list_all_threads()` includes them. Usage stats scan raw files (`list_thread_files`) so they still count child runs.
- `core/usage_stats.rs`: usage/cost aggregation over saved threads (per provider/model), backed by a derived, disposable SQLite cache at `$ZDX_HOME/cache/usage.sqlite` (`rusqlite`, bundled) that re-scans only changed threads
- `core/worktree.rs`: git worktree management helpers

### Tools (`src/tools/`)

- `tools/mod.rs`: ToolContext, ToolRegistry, ToolSet, handlers
- `tools/background.rs`: `run_background` (spawn+register a background process, invoked by the Bash tool on `background: true`) + `background_output`/`background_kill` agent tools (thread-scoped)
- `tools/memory_get.rs`: stable memory-ref reads from canonical ZDX storage
- `tools/memory_search.rs`: qmd-backed memory search returning stable memory refs
- `tools/read_thread.rs`: read saved thread transcript tool
- `tools/subagent.rs`: invoke_subagent tool
- `tools/todo_write.rs`: structured todo/task tracking tool
- `tools/thread_search.rs`: thread discovery tool

## Conventions

- Keep `zdx-engine` UI-agnostic.
- No direct terminal UI logic here; terminal behavior belongs in `zdx-tui`.
- Prefer `anyhow::Result` + `Context` at I/O boundaries.

### Logging / tracing

- Spans carry identity, events stay short. Existing spans: `turn` (`run_turn_inner`: `thread`/`model`/`provider`), `provider_request` (`request_stream`), `tool_turn` (`process_tool_turn`), `tool` (`ToolRegistry::execute_tool`: `tool`/`tool_use_id`). Log lines therefore read `turn:tool_turn:tool: … Tool failed`.
- Levels: lifecycle at `info`, per-request/per-tool detail at `debug`, failures at `warn`/`error`. Interruptions/cancellations are `debug`, not `warn`.
- Use structured fields with a stable literal message: `tracing::warn!(duration_ms, code, error = %msg, "Tool failed")`. Use `%` for display, `?` for debug.
- MUST NOT log inside per-token / per-SSE-event / per-bash-chunk paths (`consume_stream`'s event loop, `handle_stream_event`, the `ToolOutputDelta` path). Log stream *ends*, not stream items.
- MUST NOT put prompts, full messages, tool inputs, error bodies, or secrets in fields; use `skip_all` on `#[instrument]` and truncate messages (`truncate_for_error`, `truncate_tool_error`).
- Tokio tasks do not inherit spans. When spawning (e.g. concurrent tool execution), attach the parent explicitly with `.instrument(tracing::Span::current())` or the context is lost.
- `tracing_init` pins noisy dependencies (`h2`, `hyper`, `reqwest`, `rustls`, `ignore`, …) to `warn` so `ZDX_LOG=debug` shows ZDX events instead of HTTP/2 frame dumps. Naming a crate in `ZDX_LOG` opts back in (`ZDX_LOG=debug,h2=debug`).

## Checks

- Default final verification after code changes: `just ci` from repo root
- Intermediate iteration for this crate: `cargo nextest run -p zdx-engine`
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
