# zdx-cli development guide

Scope: CLI argument parsing/router, subcommands, and interactive/exec mode entrypoints.

## Where things are

- `src/main.rs`: binary entrypoint
- `build.rs`: compile-time CLI version/build metadata
- `src/cli/`: argument structs + command dispatch
- `src/cli/commands/ask_media.rs`: one-shot media understanding (`zdx ask-media`) over `zdx_engine::media`
- `src/cli/commands/{auth,chat,config,exec,models}.rs`: auth, interactive chat, config, exec, and models command handlers
- `src/cli/commands/jobs.rs`: jobs commands (`list [--json] [--daemon|--archived]`, `validate`, `runs`, `history <name> [--limit] [--json]`, `set-model`, `run [--keep]`, `migrate [--dry-run]`). `run` takes the job lock, claims the occurrence, runs attempts with delayed retries, then records the outcome and archives a consumed once job; prompt composition over the engine's `jobs::resolve_run`, plus the previous-runs section (`jobs::history`) added to each attempt's runtime context
- `src/cli/commands/bg.rs`: background-process commands (`zdx bg list [--json]`, `zdx bg kill <bg_id>`); thin wrapper over `zdx_engine::background_activity`
- `src/cli/commands/bot.rs`: Telegram bot setup/init command handler (`zdx bot init`)
- `src/cli/commands/daemon.rs`: jobs scheduler loop (`zdx jobs daemon`): plans each tick with `jobs::plan::plan_tick`, starts due occurrences as child `zdx jobs run --occurrence` processes (max 3, one per job), records missed/interrupted occurrences under the job lock (a missed summary that cannot be recorded holds the tick back), repairs stale `last.json` from claims on its rescan, writes the daemon context record
- `src/cli/commands/imagine.rs`: image generation command handler (`zdx imagine`)
- `src/cli/commands/speak.rs`: text-to-speech command handler (`zdx speak`); thin wrapper over `zdx_engine::audio::speak::synthesize_speech`
- `src/cli/commands/transcribe.rs`: speech-to-text command handler (`zdx transcribe <file>`; `--model`, `--language`, `--diarize`, `--json`, `--list-models`); wraps `zdx_engine::audio::transcribe::transcribe_audio_detailed` + `supported_models`
- `src/cli/commands/memory.rs`: native memory indexing/search/read commands (`zdx memory index/status/search/get`)
- `src/cli/commands/mcp.rs`: MCP helper commands (`servers`, `tools`, `schema`, `call`)
- `src/cli/commands/stats.rs`: usage/cost summary command handler (`zdx stats`)
- `src/cli/commands/threads.rs`: saved-thread commands; `threads inspect [--json]` is the agent/script entry point to the shared engine trajectory projection; `threads list --workers` is the programmatic worker filter (engine `list_recent_workers`, never a CLI-side rule)
- `src/cli/commands/quota.rs`: live subscription-quota command handler (`zdx quota`, `--json`); thin renderer over the shared `subscription_quota::fetch_snapshot()` API
- `src/cli/commands/service.rs`: launchd service commands (`zdx service install|uninstall|start|stop|restart|status|logs [bot|daemon|all]`; restart supports `--force`, or `--queue` to hand the restart to the bot once no agent run is active — safe from inside an agent run; `status` reports a pending queued restart); thin wrapper over `zdx_engine::service`
- `src/cli/commands/telegram.rs`: Telegram utility commands (`create-topic`, `send-message [--context] [--json]`, `send-document [--context] [--json]`); sends print the engine's `SentMessage` receipt and record the post with `telegram::record_post` (sender thread from `ZDX_THREAD_ID`; a record failure is a stderr warning, never a command failure)
- `src/cli/commands/worktree.rs`: worktree command handler
- `src/modes/exec.rs`: non-interactive streaming mode
- `src/modes/mod.rs`: mode exports (exec + feature-gated TUI)
- `tests/integration/`: CLI integration tests (`assert_cmd`, fixtures), aggregated into a single test binary via `tests/integration/main.rs`. Add new test files as `tests/integration/<name>.rs` and register them with `mod <name>;` in `main.rs` (for example `threads_export.rs` covers `zdx threads export`, `exec_subagent.rs` covers `zdx exec --subagent`, `subagent_continuation.rs` covers `invoke_subagent` `thread_id` continuation (returned ids, two-turn history, artifact dir, busy/missing/mismatched threads), `jobs_run.rs` covers job prompt layers, runtime context, `root`, `list --json`, `list --daemon`, and `set-model`, `jobs_schedule.rs` covers once jobs (`--keep`, consume, archive), the run lock, invalid files, the daemon end to end (run, missed, interrupted), and `jobs migrate`, `service.rs` covers `zdx service status`/target validation without mutating launchd, `stream_stall.rs` covers stalled-stream timeouts, including a stalled `invoke_subagent` child, against a raw TCP mock that can hold a socket open).
  - `tests/integration/opencode_go_headers.rs` covers OpenCode session routing, identity isolation, and stability across requests.
  - `tests/integration/responses_usage.rs` checks that a Responses API turn stores the provider's usage once (guards the shared Responses mapper's terminal event order).

## Conventions

- Keep CLI glue thin; shared behavior belongs in `zdx-engine`.
- Prefer integration tests in `crates/zdx-cli/tests/integration/` for CLI behavior. Do not add top-level `tests/*.rs` files — they would compile as separate test binaries and slow down `just ci`.

## Checks

- Follow the root verification rules. Scoped tests: `cargo test -p zdx-cli --lib --bins --tests`.

## Maintenance

- Command behavior contract changes: update `docs/SPEC.md` as needed.
