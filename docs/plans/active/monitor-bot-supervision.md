# Goals
- `zdx monitor` keeps the Telegram bot running while the monitor is open.
- If the bot exits unexpectedly, including the existing `/restart` exit path, monitor restarts it automatically.
- If the user intentionally stops the bot from monitor, monitor leaves it stopped.

# Non-goals
- Rebuilding binaries from monitor.
- Replacing system-level supervisors such as `launchd`.
- Generalizing supervision to every service before the bot flow is daily-usable.
- Changing Telegram command semantics beyond renaming `/restart` → `/exit` and gating it on active supervision.

# Design principles
- User journey drives order.
- Keep supervision bot-only first.
- Prefer existing PID-file service primitives over introducing a new process manager abstraction.

# User journey
1. User starts `zdx monitor` before traveling or while working remotely.
2. User starts the `bot` service from the monitor Services panel if it is not running.
3. User keeps using ZDX through Telegram.
4. If the bot crashes or exits after `/exit`, monitor detects it and starts it again.
5. If the user intentionally stops `bot` from monitor, it stays stopped.

# Foundations / Already shipped (✅)

## Monitor service controls
- What exists: `crates/zdx-monitor/src/app.rs` already lists `daemon` and `bot` in `load_services()`, starts services in `start_service()`, stops them with `stop_service()`, toggles them with `toggle_service()`, and restarts them with `restart_service()`.
- ✅ Demo: Run `just monitor`, select `bot` in Services, press Enter to start/stop, and press `r` to restart.
- Gaps: Monitor does not currently remember desired service state or restart stopped services automatically.

## Monitor refresh loop
- What exists: `crates/zdx-monitor/src/app.rs` refreshes app state on key events and every second in `run()`, using `refresh_app()`.
- ✅ Demo: Run `just monitor` and observe service PID/status update without restarting the monitor.
- Gaps: `refresh_app()` only reloads status; it does not apply supervision policy.

## Bot restart signal
- What exists: `crates/zdx-bot/src/commands.rs` defines `/exit`; `crates/zdx-bot/src/handlers/message.rs` checks `pidfile::is_supervised("bot")` before honoring it, sends a Telegram confirmation, then calls `context.request_exit()`; `crates/zdx-bot/src/lib.rs` exits with `EXIT_REQUESTED` (42) when the signal is observed.
- ✅ Demo: Run the bot under `just bot-loop`, send `/restart`, and confirm the wrapper restarts it.
- Gaps: Monitor currently does not inspect exit code `42`; for the MVP it only needs to observe that the bot stopped.

## PID-file status
- What exists: `crates/zdx-engine/src/pidfile.rs` provides `ensure_unique()`, `write()`, `status()`, and `terminate()`. Stale PID files are cleaned when status is checked.
- ✅ Demo: Start the bot, inspect monitor status, terminate the PID externally, and confirm monitor status changes to stopped.
- Gaps: `terminate()` sends SIGTERM but does not wait for process exit, so immediate restart can race with `ensure_unique()`.

# MVP slices (ship-shaped, demoable)

## Slice 1: Service supervision state (generalized from bot-only)
- **Goal**: Monitor can distinguish “service should be running” from “service was intentionally stopped”.
- **Scope checklist**:
  - [x] Add `supervised_services: BTreeSet<String>` to `MonitorApp`.
  - [x] Toggled via Ctrl+R on any service in the Services panel.
  - [x] No services supervised by default; user opts in per-service.
- **✅ Demo**: Start monitor, start `bot`, stop it with Enter, wait several refresh ticks, and confirm monitor does not auto-start it.
- **Risks / failure modes**:
  - Desired state only exists while monitor is running.
  - If initialized incorrectly, monitor may start a bot the user expected to remain stopped.

## Slice 2: Auto-restart supervised services
- **Goal**: If a supervised service should be running and monitor observes it stopped, monitor starts it automatically.
- **Scope checklist**:
  - [x] Add a supervision step after service refresh in `crates/zdx-monitor/src/app.rs`.
  - [x] Apply to any service in the `supervised_services` set.
  - [x] Reuse existing `start_service()` so the launch path stays consistent.
  - [x] Set a visible monitor status message when auto-restart happens.
- **✅ Demo**: Start bot from monitor, kill the bot PID externally, wait for the next monitor tick, and confirm bot starts again.
- **Risks / failure modes**:
  - A broken config or bad token could cause a restart loop.
  - Startup errors may be hard to diagnose because `start_service()` currently sends stdout/stderr to null.

## Slice 3: Restart-loop guard
- **Goal**: Prevent monitor from hammering restart attempts when bot startup fails repeatedly.
- **Scope checklist**:
  - [x] Track last bot auto-restart attempt time in `MonitorApp`.
  - [x] Add a short cooldown before another automatic start attempt.
  - [x] Keep manual `r` restart available even during cooldown.
  - [x] Show a concise status message when auto-restart is skipped due to cooldown.
- **✅ Demo**: Force bot startup failure, observe monitor attempts restart once, then waits before trying again instead of looping every second.
- **Risks / failure modes**:
  - Too long a cooldown makes travel recovery feel slow.
  - Too short a cooldown still creates noisy loops.

## Slice 4: UI clarity
- **Goal**: The Services panel makes it clear which services are monitor-supervised.
- **Scope checklist**:
  - [x] Add a details suffix in the UI for any supervised service.
  - [x] Update the Services hint in `crates/zdx-monitor/src/ui.rs` only if the current hint becomes misleading.
  - [x] Keep UI compact; no new panel.
- **✅ Demo**: Run monitor and confirm the `bot` row communicates whether monitor will restart it.
- **Risks / failure modes**:
  - Overloading service details can make the monitor harder to scan.

## Slice 5: Retire `just bot-loop` after dogfooding
- **Goal**: Remove the wrapper only after monitor supervision proves usable.
- **Scope checklist**:
  - [ ] Dogfood monitor supervision with `/exit` and external bot kill.
  - [x] Delete the `bot-loop` recipe from `justfile`.
  - [x] Update workspace guidance in `AGENTS.md` if build/run workflows change (remove `just bot-loop` references).
- **✅ Demo**: Travel workflow uses `just monitor` (with Ctrl+R on `bot`) plus Telegram `/exit`; `just bot-loop` no longer exists.
- **Risks / failure modes**:
  - Removing `bot-loop` too early removes a useful fallback.

# Contracts (guardrails)
- Intentional stop from monitor must not be undone by auto-restart.
- Service auto-restart applies only to services toggled into supervision via Ctrl+R.
- `/exit` must verify an active supervisor via `pidfile::is_supervised("bot")` and send a Telegram confirmation before the bot exits.
- Monitor must not rebuild binaries.
- Manual monitor controls must continue working: Enter toggles, `r` restarts, Ctrl+R toggles supervision, `q` quits.
- PID-file uniqueness must continue preventing duplicate bot instances.

# Key decisions (decide early)
- Desired state lifetime: keep it in monitor memory for the MVP, not persisted across monitor restarts.
- Launch command: keep using existing `current_exe()` service launch path, because rebuild is explicitly out of scope and `/exit` is a simple exit-and-let-supervisor-restart flow.
- Supervision scope: any service can be supervised via Ctrl+R; user controls scope.
- Restart-loop guard: add a minimal cooldown before broader backoff policy.

# Testing
- Manual smoke demos per slice.
- Minimal regression tests only for contracts.
- Run `cargo check -p zdx-monitor -p zdx-bot` after implementation.
- Run `cargo nextest run -p zdx-monitor` if supervision logic is extracted into testable helpers.
- Run `just ci-fast` before considering the plan complete if code changes are non-trivial.

# Polish phases (after MVP)

## Phase 1: Better restart diagnostics
- Surface the last auto-restart attempt time and the last start error in monitor status/details.
- ✅ Check-in demo: Force a startup failure and confirm monitor shows a useful status instead of silently retrying.

# Later / Deferred
- Persist desired state across monitor restarts; revisit only if monitor restarts often during travel.
- Supervise `daemon`; revisit only if automations need the same recovery behavior.
- Replace monitor supervision with `launchd`; revisit only if monitor itself needs to be resilient when not open.
- Rebuild from monitor; explicitly out of scope because the user can rebuild from chat or SSH.