# zdx-cli development guide

Scope: CLI argument parsing/router, subcommands, and interactive/exec mode entrypoints.

## Where things are

- `src/main.rs`: binary entrypoint
- `build.rs`: compile-time CLI version/build metadata
- `src/cli/`: argument structs + command dispatch
- `src/cli/commands/ask_media.rs`: one-shot media understanding (`zdx ask-media`) over `zdx_engine::media`
- `src/cli/commands/{auth,chat,config,exec,models}.rs`: auth, interactive chat, config, exec, and models command handlers
- `src/cli/commands/jobs.rs`: the Rust side of jobs. `JOBS` (`$ZDX_JOBS` or `~/.local/bin/zdx-jobs`) and `delegate`: every `zdx jobs` subcommand but `history` execs the Go jobs program with the global `--root`, `--system-prompt`, `--no-thread`, and `--thread`. `attempt` (internal `zdx jobs attempt --result FILE [--lock-fd FD]`): one attempt's agent, from the job on stdin, with prompt composition over the engine's `jobs::resolve_run` and the previous-runs section (`jobs::history`); it writes the result file the Go runner reads. `resolve` (internal `zdx jobs resolve [--daemon] [--prepare]`): settings for the jobs on stdin. `history`: `zdx jobs history`
- `src/cli/commands/bg.rs`: background-process commands (`zdx bg list [--json]`, `zdx bg kill <bg_id>`); thin wrapper over `zdx_engine::background_activity`
- `src/cli/commands/go_program.rs`: the shared exec contract for the Go programs `zdx` runs in its place (`GoProgram`: `$ENV` absolute override or `~/.local/bin/<name>`, never PATH, never this binary; `--root ROOT`, `ZDX_HOME`, `ZDX_BIN`; fails before running anything when the program is missing, relative, or this binary)
- `src/cli/commands/bot.rs`: Telegram bot commands: `zdx bot init`, `zdx bot profile add`, and `zdx bot [-- ARGS]` (`run_gateway`: execs the Go gateway, `GATEWAY` = `$ZDX_GATEWAY` or `~/.local/bin/zdx-gateway`, as `--root ROOT bot ARGS`)
- `src/cli/commands/chat.rs`: bare `zdx`: piped stdin runs `exec`; a terminal execs the Go chat TUI (`apps/tui`), `TUI` = `$ZDX_TUI` or `~/.local/bin/zdx-tui`, as `--root ROOT [--thread ID] [--model SPEC]` (`--no-thread` is refused); `zdx threads resume [ID]` execs it with that thread (default: the latest)
- `src/cli/commands/monitor.rs`: `zdx monitor [-- ARGS]` execs the Go monitor (`apps/monitor`), `MONITOR` = `$ZDX_MONITOR` or `~/.local/bin/zdx-monitor`, as `--root ROOT ARGS`
- `src/cli/commands/imagine.rs`: image generation command handler (`zdx imagine`); `tests/integration/imagine.rs` covers the OpenAI Images API route (endpoint, saved file type, flag errors) against a mock server
- `src/cli/commands/speak.rs`: text-to-speech command handler (`zdx speak`); thin wrapper over `zdx_engine::audio::speak::synthesize_speech`
- `src/cli/commands/transcribe.rs`: speech-to-text command handler (`zdx transcribe <file>`; `--model`, `--language`, `--vocabulary`, `--diarize`, `--json`, `--list-models`); wraps `zdx_engine::audio::transcribe::transcribe_audio_detailed` + `supported_models` and `zdx_engine::audio::vocabulary::load`
- `src/cli/commands/memory.rs`: native memory indexing/search/read commands (`zdx memory index/status/search/get`)
- `src/cli/commands/mcp.rs`: MCP helper commands (`servers`, `tools`, `schema`, `call`)
- `src/cli/commands/stats.rs`: usage/cost summary command handler (`zdx stats [--days N] [--json]`; text or the shared `usage_report::UsageReport` as JSON)
- `src/cli/commands/threads.rs`: saved-thread commands; `threads inspect [--json]` is the agent/script entry point to the shared engine trajectory projection; `threads list --workers` is the programmatic worker filter (engine `list_recent_workers`, never a CLI-side rule)
- `src/cli/commands/quota.rs`: `zdx quota [--json]` execs the Go quota reader (`apps/shared/cmd/zdx-quota`), `QUOTA` = `$ZDX_QUOTA` or `~/.local/bin/zdx-quota`, as `--root ROOT --zdx-version <CARGO_PKG_VERSION> [--json]`. No quota logic in Rust here
- `src/cli/commands/orchestrator.rs`: `zdx orchestrator serve|send|status|cancel` — the orchestrator service (PID file, socket, Ctrl+C removes the socket) and its client (`send --wait` prints JSON event lines until the thread settles); thin wrappers over `zdx_engine::orchestration`
- `src/cli/commands/service.rs`: launchd service commands (`zdx service install|uninstall|start|stop|restart|status|logs [bot|daemon|orchestrator|all]`; restart supports `--force`, or `--queue` to hand the restart to the bot once no agent run is active — safe from inside an agent run; `status` reports a pending queued restart); thin wrapper over `zdx_engine::service`
- `src/cli/commands/telegram.rs`: Telegram utility commands (`create-topic`, `send-message [--context] [--json]`, `send-document [--context] [--json]`); sends print the engine's `SentMessage` receipt and record the post with `telegram::record_post` (sender thread from `ZDX_THREAD_ID`; a record failure is a stderr warning, never a command failure)
- `src/cli/commands/worktree.rs`: worktree command handler
- `src/modes/exec.rs`: non-interactive streaming mode
- `src/modes/mod.rs`: mode exports (exec)
- `tests/integration/`: CLI integration tests (`assert_cmd`, fixtures), aggregated into a single test binary via `tests/integration/main.rs`. Add new test files as `tests/integration/<name>.rs` and register them with `mod <name>;` in `main.rs` (for example `threads_export.rs` covers `zdx threads export`, `exec_subagent.rs` covers `zdx exec --subagent`, `subagent_continuation.rs` covers `invoke_subagent` `thread_id` continuation (returned ids, two-turn history, artifact dir, busy/missing/mismatched threads), `jobs_run.rs` covers `zdx jobs attempt` (prompt layers, runtime context, `root`, the result file), `zdx jobs resolve` (caller and daemon settings), and `history`, `jobs_go.rs` covers the `zdx jobs` exec delegation with fake program scripts (with `ZDX_JOBS_TEST_BIN` a real `zdx-jobs` runs a once job through a Rust attempt), `service.rs` covers `zdx service status`/target validation and the install program checks without mutating launchd, `bot_gateway.rs` covers `zdx bot` exec delegation (argv, env, PID, signals, exit status, fail-closed) with fake gateway scripts, `monitor_go.rs` covers the same for `zdx monitor` with fake monitor scripts, `quota.rs` covers it for `zdx quota` (fake reader scripts; with `ZDX_QUOTA_TEST_BIN` a real `zdx-quota` on an empty home must print the old Rust JSON byte for byte), `orchestrator.rs` covers a real `zdx orchestrator serve` child with a mock provider (`send --wait` events, the `exec --thread` refusal, `status --json`), `stream_stall.rs` covers stalled-stream timeouts, including a stalled `invoke_subagent` child, against a raw TCP mock that can hold a socket open).
  - `tests/integration/opencode_go_headers.rs` covers OpenCode session routing, identity isolation, and stability across requests.
  - `tests/integration/responses_usage.rs` checks that a Responses API turn stores the provider's usage once (guards the shared Responses mapper's terminal event order).

## Conventions

- Keep CLI glue thin; shared behavior belongs in `zdx-engine`.
- Prefer integration tests in `crates/zdx-cli/tests/integration/` for CLI behavior. Do not add top-level `tests/*.rs` files — they would compile as separate test binaries and slow down `just ci`.

## Checks

- Follow the root verification rules. Scoped tests: `cargo test -p zdx --bins --tests` (the package is named `zdx` and has no lib target).

## Maintenance

- Command behavior contract changes: update `docs/SPEC.md` as needed.
