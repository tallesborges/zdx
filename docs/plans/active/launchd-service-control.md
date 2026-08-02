# Goals
- `zdx bot` and `zdx automations daemon` stay running without `zdx monitor` being open, and survive logout/reboot.
- One command restarts a service after a rebuild: `zdx service restart bot` (and `just deploy` for build+install+restart).
- Restart always picks up the freshly installed binary, never the stale one the supervisor happened to start from.
- Service stdout/stderr land in files so startup failures are diagnosable.

# Non-goals
- Building/installing binaries from inside `zdx service` (that's `just install`).
- Cross-platform service management. macOS/launchd only; other platforms get a clear "unsupported" error.
- Supervising `zdx monitor` itself, or arbitrary user services. Only `bot` and `daemon`.
- Replacing the background-process subsystem (`zdx bg`, `~/.zdx/run/background/`). Unrelated.
- A control socket / RPC. launchd is the control plane.

# Design principles
- launchd owns process lifetime; ZDX owns the ergonomics on top of it.
- One service module, two front-ends (CLI + monitor). Monitor must not keep its own spawn path.
- Keep the existing PID-file primitives as the *status* source of truth; launchd is the *control* mechanism.

# Current state (what exists today)

## PID-file primitives ✅
- `crates/zdx-engine/src/pidfile.rs`: `ensure_unique()`, `write()`, `status()`, `terminate()`, `remove()`, `mark_supervised()`, `is_supervised()`.
- Files: `~/.zdx/run/{bot,daemon}.pid`, `~/.zdx/run/{name}.supervised`.
- Gap: `terminate()` sends SIGTERM without waiting, so stop→start races `ensure_unique()`.

## Monitor service controls ✅ (with the two blocking gaps)
- `crates/zdx-monitor/src/app.rs`: `load_services()`, `start_service()`, `stop_service()`, `restart_service()`, plus Ctrl+R supervision (`supervised_services: BTreeSet<String>`, 5s cooldown).
- Gap 1: supervision set is memory-only — closing the monitor silently drops it.
- Gap 2: `start_service()` uses `std::env::current_exe()`, so a monitor started from `target/debug` keeps respawning that binary regardless of what you just built.
- Gap 3: children get `Stdio::null()` for stdout/stderr — startup errors vanish.

## Bot soft-restart ✅
- `/exit` (`crates/zdx-bot/src/handlers/message/commands.rs`) refuses unless `pidfile::is_supervised("bot")`; `crates/zdx-bot/src/lib.rs` exits with code 42.
- Gap: under launchd nothing writes the `.supervised` marker, so `/exit` would refuse. Slice 3 fixes this.

## Install path ✅
- `just install` already builds release and installs to `~/.local/bin/zdx`. This is the stable path the plists will point at.

# User journey
1. One-time: `zdx service install` writes and bootstraps both launchd agents.
2. Bot and daemon are running; laptop reboots; they come back automatically.
3. User adds a bot feature from Telegram or SSH.
4. User runs `just deploy` (build → install → `zdx service restart all`).
5. New binary is live in seconds; `zdx service status` confirms PIDs changed.
6. If it fails to boot, `zdx service logs bot` shows why.

# MVP slices (ship-shaped, demoable)

## Slice 1: `zdx-engine` service module (launchd backend)
- **Goal**: One place that knows how to install, bootstrap, stop, restart, and inspect the two agents.
- **Scope checklist**:
  - [x] New `crates/zdx-engine/src/service.rs`, exported from `lib.rs`.
  - [x] `enum Service { Bot, Daemon }` with `label()` (`dev.zdx.bot`, `dev.zdx.daemon`), `pid_name()` (`bot`, `daemon`), `args()`.
  - [x] `plist_path()` → `~/Library/LaunchAgents/{label}.plist`.
  - [x] `render_plist(exe: &Path, root: &Path) -> String` generating: `Label`, `ProgramArguments` (`{exe} --root {root} bot` / `... automations daemon`), `RunAtLoad=true`, `KeepAlive=true`, `ThrottleInterval=10`, `StandardOutPath`/`StandardErrorPath` → `~/.zdx/run/logs/{name}.{out,err}`, `EnvironmentVariables` with `PATH` (`~/.local/bin:/opt/homebrew/bin:/usr/bin:/bin:/usr/sbin:/sbin`), `ZDX_HOME`, and `ZDX_SERVICE_SUPERVISOR=launchd`.
  - [x] `install()` writes the plist, resolving the program to `~/.local/bin/zdx` (error out if it does not exist), then `launchctl bootstrap gui/$UID {plist}`.
  - [x] `uninstall()` → `launchctl bootout gui/$UID/{label}`, then delete plist.
  - [x] `start()` = bootstrap, `stop()` = bootout, `restart()` = `launchctl kickstart -k gui/$UID/{label}`.
  - [x] `status()` combines `pidfile::status()` with plist-installed / launchd-loaded flags.
  - [x] Non-unix / non-macOS: compile-gated `anyhow::bail!("launchd service control is macOS-only")`.
- **✅ Demo**: A unit test asserts `render_plist()` output for both services (snapshot-ish string assert on label, program args, log paths).
- **Risks**: `gui/$UID` domain vs `user/$UID` — use `gui/` since the bot may need keychain/network under an active session. `kickstart -k` on a not-loaded label errors; map it to a clear message.

## Slice 2: `zdx service` CLI
- **Goal**: The whole workflow is reachable from the terminal, from SSH, and from the agent's bash tool.
- **Scope checklist**:
  - [x] `crates/zdx-cli/src/cli/commands/service.rs` + `pub mod service;` in `commands/mod.rs`.
  - [x] `Commands::Service { command: ServiceCommands }` in `crates/zdx-cli/src/cli/mod.rs` (mirror the existing `Bg` wiring at `mod.rs:270` / dispatch at `mod.rs:1067`).
  - [x] Subcommands: `install`, `uninstall`, `start <target>`, `stop <target>`, `restart <target>`, `status [--json]`, `logs <target> [--lines N] [--err]`.
  - [x] `<target>` = `bot` | `daemon` | `all` (default `all` for `restart`/`status`).
  - [x] `status` text output: name, running/stopped, PID, uptime, launchd loaded yes/no.
  - [x] `restart` prints old PID → new PID so it's obvious the process actually cycled.
- **✅ Demo**: `zdx service install`, then `zdx service status`, kill the bot PID by hand, wait, `zdx service status` shows a new PID without any monitor open.
- **Risks**: Running `install` while an old manually-started bot holds the PID file → launchd child dies on `ensure_unique()`. `install` must detect a live PID not owned by launchd and tell the user to stop it first.

## Slice 3: `/exit` and supervision detection under launchd
- **Goal**: `/exit` from Telegram works when launchd is the supervisor.
- **Scope checklist**:
  - [x] On startup, when `ZDX_SERVICE_SUPERVISOR=launchd` is set, the bot calls `pidfile::mark_supervised("bot")` for itself (marker holds its own PID, which is alive for as long as it matters).
  - [x] Keep the monitor's `mark_supervised` path working unchanged.
  - [x] No change to the `/exit` handler's gate — it still checks `is_supervised("bot")`.
- **✅ Demo**: With no monitor open, send `/exit` in Telegram; bot confirms, exits 42, launchd restarts it within `ThrottleInterval`.
- **Risks**: `KeepAlive=true` restarts on *any* exit, including exit 42 — that's the desired behavior here, but it also means a hard config error becomes a restart loop; `ThrottleInterval=10` plus the log files are the mitigation.

## Slice 4: Monitor delegates to the service module
- **Goal**: Monitor stops being a supervisor and becomes a control panel; the stale-binary trap disappears.
- **Scope checklist**:
  - [x] `start_service()` / `stop_service()` / `restart_service()` in `crates/zdx-monitor/src/app.rs` call `zdx_engine::service::*` instead of spawning `current_exe()`.
  - [x] Delete `supervised_services`, the Ctrl+R keybinding, the auto-restart step in `refresh_app()`, and the 5s cooldown state — launchd owns this now.
  - [x] Services panel shows launchd state (`installed` / `not installed`) in `details`.
  - [x] Update the panel title hint in `crates/zdx-monitor/src/ui.rs` (drop `^R=supervise`).
  - [x] Update `crates/zdx-monitor/AGENTS.md`.
- **✅ Demo**: Run monitor from `target/debug`, press `r` on `bot`, confirm the restarted process is the `~/.local/bin/zdx` binary (`zdx service status` PID + `ps` command path), not the debug one.
- **Risks**: If agents aren't installed, Enter/`r` must fail with "run `zdx service install` first" rather than silently doing nothing.

## Slice 5: `just deploy` + docs
- **Goal**: The one-word workflow the whole plan exists for.
- **Scope checklist**:
  - [x] `justfile`: `deploy: install` → then `~/.local/bin/zdx service restart all`.
  - [x] `AGENTS.md` "Build / run": document `just deploy` and `zdx service *`.
  - [x] `docs/SPEC.md` CLI surface (around the `zdx bot` / `zdx automations daemon` entries): add the `zdx service` contract.
- **✅ Demo**: Change a bot string, run `just deploy`, see the new behavior in Telegram without touching the monitor.
- **Risks**: `just deploy` on a broken build must not restart anything — `install` failing already aborts the recipe.

# Contracts (guardrails)
- PID-file uniqueness continues to prevent duplicate `bot`/`daemon` instances.
- `zdx service restart` must observe the old process exit before the new one starts (launchd `kickstart -k` guarantees this; the current stop→start race is gone).
- Restart always launches `~/.local/bin/zdx`, never `current_exe()`.
- `zdx service stop` is durable: the service stays stopped until an explicit `start`, across reboots.
- `/exit` still requires an active supervisor and a Telegram confirmation.
- Monitor must not spawn services directly and must not rebuild binaries.
- Service stdout/stderr are always captured to `~/.zdx/run/logs/`.

# Key decisions (decide early)
- **Supervisor**: launchd, not a bespoke `zdx supervisor` process. Rationale: reboot-persistence and crash-restart for free; nothing to supervise the supervisor.
- **Program path**: `~/.local/bin/zdx` (the `just install` target), fixed at plist-write time. Debug iteration keeps using `just bot` in a terminal.
- **Domain**: `gui/$UID` so the services run inside the logged-in GUI session.
- **KeepAlive**: unconditional `true` + `ThrottleInterval=10`, rather than `SuccessfulExit=false`, so exit-42 `/exit` restarts work without special-casing.
- **Status source**: keep reading PID files (already correct and cheap) rather than parsing `launchctl print`.
- **Ctrl+R supervision**: removed, not kept as a fallback. Per workspace convention, no parallel mechanism left behind.

# Testing
- Unit: `render_plist()` output for both services (`cargo nextest run -p zdx-engine`).
- Unit: target parsing (`bot`/`daemon`/`all`, unknown → error).
- Integration (`crates/zdx-cli/tests/`): `zdx service status --json` shape on a machine with nothing installed (both stopped, not installed) — no launchd mutation in tests.
- Manual smoke per slice; the reboot check is manual and is the acceptance test for Slice 2.
- `just ci-fast` during iteration, `just test` before wrapping up.

# Later / Deferred
- `zdx service logs --follow` (tail -f equivalent). Start with `--lines`.
- Auto-restart on binary change (file watch on `~/.local/bin/zdx`). Revisit if `just deploy` still feels manual.
- Health checks / restart backoff beyond `ThrottleInterval`.
- Supervising additional services (MCP warm sessions) — nothing needs it yet.
- A zbar menu-bar action shelling out to `zdx service restart all`. Belongs in zbar, not here; this plan just makes it a one-liner.
