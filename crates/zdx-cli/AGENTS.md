# zdx-cli development guide

Scope: CLI argument parsing/router, subcommands, and interactive/exec mode entrypoints.

## Where things are

- `src/main.rs`: binary entrypoint
- `build.rs`: compile-time CLI version/build metadata
- `src/cli/`: argument structs + command dispatch
- `src/cli/commands/automations.rs`: automations commands (`list`, `validate`, `run`)
- `src/cli/commands/bot.rs`: Telegram bot setup/init command handler (`zdx bot init`)
- `src/cli/commands/daemon.rs`: scheduled automations daemon loop
- `src/cli/commands/imagine.rs`: image generation command handler (`zdx imagine`)
- `src/cli/commands/speak.rs`: text-to-speech command handler (`zdx speak`); thin wrapper over `zdx_engine::audio::speak::synthesize_speech`
- `src/cli/commands/transcribe.rs`: speech-to-text command handler (`zdx transcribe <file>`; `--model`, `--language`, `--diarize`, `--json`, `--list-models`); wraps `zdx_engine::audio::transcribe::transcribe_audio_detailed` + `supported_models`
- `src/cli/commands/memory.rs`: memory indexing/search commands (`zdx memory index`, `zdx memory search`)
- `src/cli/commands/mcp.rs`: MCP helper commands (`servers`, `tools`, `schema`, `call`)
- `src/cli/commands/stats.rs`: usage/cost summary command handler (`zdx stats`)
- `src/cli/commands/quota.rs`: live subscription-quota command handler (`zdx quota`, `--json`); async, fetches `zdx_engine::providers::subscription_quota::FETCHERS`
- `src/cli/commands/telegram.rs`: Telegram utility commands
- `src/cli/commands/worktree.rs`: worktree command handler
- `src/modes/exec.rs`: non-interactive streaming mode
- `src/modes/mod.rs`: mode exports (exec + feature-gated TUI)
- `tests/integration/`: CLI integration tests (`assert_cmd`, fixtures), aggregated into a single test binary via `tests/integration/main.rs`. Add new test files as `tests/integration/<name>.rs` and register them with `mod <name>;` in `main.rs` (for example `threads_export.rs` covers `zdx threads export`).

## Conventions

- Keep CLI glue thin; shared behavior belongs in `zdx-engine`.
- Prefer integration tests in `crates/zdx-cli/tests/integration/` for CLI behavior. Do not add top-level `tests/*.rs` files — they would compile as separate test binaries and slow down `just ci`.

## Checks

- Default final verification after code changes: `just ci` from repo root
- Intermediate iteration for this crate: `cargo nextest run -p zdx-cli`
- Use `just lint` or `just test` only when intentionally running one half of CI

## Maintenance

- Add/move/delete `.rs` files in this crate: update this file.
- Add/move/delete prompt layer files in this crate: update this file.
- Command behavior contract changes: update `docs/SPEC.md` as needed.