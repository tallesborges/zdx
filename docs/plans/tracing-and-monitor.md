# Goals
- All `eprintln!` calls replaced with structured `tracing` macros across the workspace
- Logs written to `~/.zdx/logs/` with daily file rotation via `tracing-appender`
- `zdx monitor` TUI command shows a compact dashboard: services status, recent threads, automations, bot info, model config

# Non-goals
- Message content display in monitor
- Fancy log viewer with search/filter
- Config editing from monitor
- Fancy visualizations or charts

# Design principles
- User journey drives order
- KISS and YAGNI strictly — single-line items, no decoration
- Ship logging first (foundational), then build monitor on top of it
- Replace `eprintln!` mechanically — preserve existing semantics, just change the sink

# User journey
1. User runs `zdx bot` or `zdx daemon` — logs appear in `~/.zdx/logs/zdx-YYYY-MM-DD.log` with structured fields
2. User checks `~/.zdx/logs/` and sees daily-rotated log files, old days auto-separated
3. User runs `zdx monitor` — sees a compact TUI dashboard with services, recent threads, automations, bot status, model config
4. User copies a thread ID from the monitor for debugging

# Foundations / Already shipped (✅)

## AgentEvent stream
- What exists: `zdx-core` emits structured `AgentEvent` (`TurnStarted`, `ToolStarted`, `AssistantDelta`, etc.)
- ✅ Demo: Run `ZDX_DEBUG_STREAM=1 zdx` and check JSON summary output
- Gaps: None for this feature

## TUI framework
- What exists: `zdx-tui` with ratatui, full interactive TUI for conversations
- ✅ Demo: `just run`
- Gaps: No monitor mode yet — new subcommand needed

## Config system
- What exists: `Config::load()` with providers, models, telegram settings, automations
- ✅ Demo: `zdx config show`
- Gaps: None

## Thread persistence
- What exists: Thread events persisted to disk with metadata
- ✅ Demo: `ls ~/.zdx/threads/`
- Gaps: None

## Automations/Daemon
- What exists: `zdx daemon` runs scheduled automations, `zdx automations list/run`
- ✅ Demo: `just automations list`
- Gaps: None

# MVP slices (ship-shaped, demoable)

## Slice 1: Tracing foundation + daily log rotation
- **Goal**: Set up `tracing` subscriber with daily rolling file appender; all crate entrypoints init the subscriber; logs go to `~/.zdx/logs/`
- **Scope checklist**:
  - [ ] Add `tracing`, `tracing-subscriber`, `tracing-appender` to workspace `Cargo.toml`
  - [ ] Create `zdx-core` helper: `init_tracing()` that sets up a `tracing_subscriber` with `fmt` layer writing to `tracing_appender::rolling::daily("~/.zdx/logs", "zdx.log")`
  - [ ] Also add a stderr layer (filtered to `warn+`) so critical errors still show in terminal
  - [ ] Call `init_tracing()` from `zdx-cli/src/main.rs`, `zdx-bot/src/lib.rs` entrypoints
  - [ ] Replace 5–10 `eprintln!` calls in `zdx-bot/src/lib.rs` and `zdx-cli/src/cli/commands/daemon.rs` with `tracing::info!`/`tracing::warn!`/`tracing::error!` as proof of concept
- **✅ Demo**: Run `zdx bot` (or `zdx daemon`), see log file created at `~/.zdx/logs/zdx.YYYY-MM-DD.log` with structured output; stderr still shows warnings
- **Risks / failure modes**:
  - Guard init must be held alive or logs drop silently — store `WorkerGuard` in a `let _guard` at main scope
  - TUI mode (interactive) must not double-init or conflict with ratatui stderr — gate stderr layer on non-TUI mode

## Slice 2: Replace all remaining `eprintln!` with tracing macros
- **Goal**: Mechanically replace all ~40+ `eprintln!` calls across the workspace with appropriate `tracing` macros
- **Scope checklist**:
  - [ ] `zdx-bot/src/ingest/mod.rs` (~20 calls): `warn!`/`info!`/`debug!` with structured fields (`chat_id`, `user_id`)
  - [ ] `zdx-bot/src/handlers/message.rs` (~11 calls): `info!`/`error!`/`debug!` with structured fields
  - [ ] `zdx-cli/src/cli/commands/automations.rs` (~3 calls): `warn!`/`info!`
  - [ ] `zdx-core/src/` (~4 calls in agent.rs, thread_persistence.rs, debug_metrics.rs, models.rs): `warn!`/`error!`
  - [ ] Distinguish user-facing CLI output (`println!`) from log messages — keep `println!` for CLI output
  - [ ] Test files and xtask: leave `eprintln!` as-is
- **✅ Demo**: `grep -rn "eprintln!" --include="*.rs" crates/` shows only test files and xtask; all runtime logging goes through tracing
- **Risks / failure modes**:
  - `models.rs` prints are user-facing CLI output — these should be `println!` not tracing
  - `zdx-cli/src/main.rs` error print is the top-level error handler — keep as `eprintln!`

## Slice 3: Monitor TUI — scaffold + static dashboard
- **Goal**: `zdx monitor` opens a ratatui TUI showing static/file-based info: model config, recent threads, automation definitions
- **Scope checklist**:
  - [ ] Add `monitor` subcommand to `zdx-cli` CLI router
  - [ ] Create `crates/zdx-monitor/` crate with basic ratatui app (single screen, sectioned layout)
  - [ ] Section: **Config** — show current model, thinking level, provider (read from `Config::load()`)
  - [ ] Section: **Recent Threads** — list last N threads from `~/.zdx/threads/` with ID, timestamp, surface (single-line each)
  - [ ] Section: **Automations** — list discovered automations with name, schedule, last-run time
  - [ ] Keybinding: `q` to quit, `y` to copy selected thread ID to clipboard
  - [ ] Keybinding: arrow keys / `j`/`k` to navigate lists
- **✅ Demo**: Run `zdx monitor` — see compact dashboard with config, recent threads, automations. Copy a thread ID. Press `q` to exit.
- **Risks / failure modes**:
  - Thread listing may be slow with many threads — limit to last 50, sorted by mtime
  - Clipboard copy may need platform-specific handling — use `arboard` crate or shell out to `pbcopy`

## Slice 4: Monitor TUI — live service status
- **Goal**: Monitor shows live status of running services (bot, daemon) by reading PID files
- **Scope checklist**:
  - [ ] Bot and daemon write a PID + status file to `~/.zdx/run/{service}.pid` on startup, remove on clean shutdown
  - [ ] Section: **Services** — show bot (running/stopped), daemon (running/stopped) based on PID file + process alive check
  - [ ] Auto-refresh every 2–5 seconds
  - [ ] Show uptime if running (from PID file mtime)
- **✅ Demo**: Start `zdx bot` in one terminal, run `zdx monitor` in another — see bot as "running". Stop bot — monitor updates to "stopped".
- **Risks / failure modes**:
  - Stale PID files after crash — validate PID is alive via `kill(pid, 0)` check
  - Race conditions on file write — acceptable for monitoring

# Contracts (guardrails)
- Existing `AgentEvent` stream behavior must not change
- `debug_metrics` JSON output must continue working when `ZDX_DEBUG_STREAM` is set
- Bot message handling latency must not regress (tracing is async with the appender worker thread)
- Interactive TUI (`zdx` / `just run`) must not show log output in the terminal (only file)
- `zdx monitor` is read-only — no mutations to config, threads, or automations

# Key decisions (decide early)
- **Log format**: `tracing-subscriber` `fmt` layer with `compact()` format — KISS
- **Log level default**: `info` for file, `warn` for stderr; configurable via `RUST_LOG` env var
- **Where `init_tracing()` lives**: `zdx-core` as a shared helper
- **Monitor crate**: New `zdx-monitor` crate (`crates/zdx-monitor/`), invoked via `zdx monitor` subcommand in `zdx-cli`. If shared UI code emerges later, extract to a `zdx-ui-common` crate (YAGNI until then).
- **Service status mechanism**: Simple PID files in `~/.zdx/run/` — no IPC, no sockets

# Testing
- Manual smoke demos per slice
- Minimal regression tests only for contracts
- After Slice 2: verify `just ci` passes (clippy + existing tests)
- After Slice 3: manual run of `zdx monitor` with real data

# Polish phases (after MVP)

## Phase 1: Active agents + thread detail
- Track active agent turns across all surfaces (bot, TUI, exec) via a shared state file (e.g. `~/.zdx/run/agents.json`)
- Show "Active Agents" section in monitor with surface, thread ID, model, elapsed time
- Add thread message count and token usage to thread list items
- ✅ Check-in demo: Start a bot conversation + `zdx exec`, `zdx monitor` shows both as active agents with elapsed time

# Later / Deferred
- **Fancy log viewer with search/filter in monitor** — revisit when log volume makes manual file reading painful
- **Config editing from monitor** — revisit if frequently needing to change model mid-session
- **Message content display in monitor** — explicitly deferred by user
- **Log shipping / remote monitoring** — revisit if running bot on a server
- **Log retention / max file count cleanup** — revisit when disk usage becomes a concern
