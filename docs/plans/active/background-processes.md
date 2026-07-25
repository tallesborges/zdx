> Stage: **active** — building. Keep this plan current while working: when a scope item is finished, check its box `[ ]`→`[x]`; when a phase's ✅ demo passes, mark the phase done (with date). The plan file is the source of truth, not memory.

> **Status: ACTIVE — building (promoted from draft 2026-07-25).** Direction from a chat with Talles: when the agent in a thread starts a long-running process (e.g. a dev server), zdx should track it as a **background process** so it can be listed and killed from the Monitor (a new Background tab, grouped by thread across all threads), from the chat TUI (an overlay for the current thread, with kill), and surfaced as a subtle bottom-right status-line indicator. Spawning stays a flag on the existing bash tool; the agent also gets dedicated `background_output` + `background_kill` tools (mirrors Claude Code's `run_in_background` + `BashOutput`/`KillShell` split). **Naming decision:** term is "background process" (NOT "task" — "task" is the agent/subagent delegation tool). **KISS pass (oracle, 2026-07-22):** merged to 3 vertical slices; cut per-session output cursor, output regex filter, `bg clean`, and monitor kill-confirm; simplified CLI + logs; kept the safety-critical identity guard / waiter / commit-then-report; added exited-tombstone semantics. **Foreground-leak pass (oracle, 2026-07-22):** instead of parsing for `&`/`nohup`/`setsid` or adopting leaked survivors (unsound on macOS), make `background: true` the *only* persistent path and kill leftover foreground process-group members on completion — so plain `cmd &` no longer silently survives.

# Goals
- The agent can launch a long-running command in the **background** so it does not block the turn (e.g. start a dev server, keep working).
- Every background process is tracked in a disk registry with enough identity to **list** and **kill** it later, scoped to the thread that started it.
- **Agent tools**: the agent can check a background process's output (`background_output`) and stop it (`background_kill`) mid-turn — not just the human.
- **Monitor**: a new **Background** tab lists running background processes grouped by originating thread, across all threads, with a key to kill a selected one.
- **Chat TUI**: a new overlay lists the background processes started by the **current** thread, with a key to kill one.
- **Chat TUI**: a subtle bottom-right status-line indicator shows the count of live background processes for the current thread (e.g. `▸ 2 bg`).
- Background processes survive the agent turn AND the zdx/TUI process that started them (a started dev server keeps running); dead/exited ones are reaped from the registry.

# Non-goals
- Interactive processes / PTY allocation (no TTY, no stdin injection into a running process). Detached, non-interactive commands only.
- Restart of a background process (kill + re-run is out of scope for the MVP; deferred).
- Cross-machine / remote execution (that's the separate Orbs / Remote Orbs idea).
- Streaming a process's live output token-by-token into the model turn. Output goes to log files that `background_output` reads on demand (pull-based, bounded tail), not a live push stream.
- Persisting background processes across a full machine reboot (a rebooted machine kills detached children; the registry only needs to reap them, not resurrect them).
- Live log rotation / hard size caps on a running process's log file (deferred — MVP accepts an unbounded running-log as a documented alpha risk; only the *read* is bounded).
- Incremental "only new output since last check" cursor state; regex output filtering; bulk/`--json` CLI ergonomics; `bg clean` (all cut for MVP — see KISS pass).
- Sandboxing / resource limits on background processes (separate "Sandbox Safe Execution" idea).

# Design principles
- User journey drives order: unlock "agent starts a bg process → can check its output + kill it" first (registry + bash flag + agent tools + minimal CLI), then the TUI overlay + indicator, then the Monitor tab.
- Follow the proven split (Claude Code): **spawn = a flag on bash** (`background: true`, like `run_in_background`), **monitor + kill = dedicated tools** (`background_output` ≈ `BashOutput`, `background_kill` ≈ `KillShell`). Do NOT build a separate spawn tool — it would duplicate bash's execution semantics (sh -c, cwd, env, session/group, null stdin) for no gain.
- One rule, not a parser: **foreground = ephemeral, `background: true` = persistent.** Enforce it by killing leftover foreground process-group members on completion (behavioral), never by parsing the command for `&`/`nohup`/`setsid` (fragile) or adopting leaked survivors (unsound on macOS). `background: true` is the single sanctioned way to leave something running.
- KISS for an alpha personal tool: keep only the cheap-correctness pieces that prevent a real foot-gun (identity-guarded kill, child reaping, atomic registration, foreground-leak cleanup); defer ceremony (cursors, rotation, cleanup commands, confirmations) until real use demands it.
- Reuse before rebuild:
  - Mirror the **active-agent marker pattern** (`crates/zdx-engine/src/agent_activity.rs`): `RunRecord` + `RunGuard` + `list_active()` writing marker JSON under `paths::zdx_home()/run/agents/`. The background registry is the same shape under `.../run/background/`, but **without** RAII-on-drop cleanup (processes outlive the turn) and **with** liveness reaping + a short exited-tombstone window.
  - Reuse the process-group spawn already in bash (`ProcessGroupGuard`, Unix `pre_exec` in `crates/zdx-tools/src/bash.rs:182-225,317-413`) — but detach (new session) instead of kill-on-drop, keeping the pgid for later kill.
  - Monitor: clone the **Active Agents** tab machinery (`Section` enum, `Section::ALL/next/label/item_count`, `load_active_agents()`, `render_active_agents()` in `crates/zdx-monitor/src/app.rs` + `ui.rs`); group by `thread_id` instead of tree-by-parent.
  - TUI: clone the **overlay** pattern (`OverlayRequest`/`Overlay`/`handle_key`/`render` in `crates/zdx-tui/src/overlays/`, `timeline.rs` as the compact template) and read the current thread id from `app.tui.thread.thread_handle.id`.
- Ugly-but-functional first: whole-file marker rereads on the timed tick are fine (mirrors `list_active()` / `refresh_app()`); no inotify/watchers.
- Kill safety over convenience: never kill a PID we can't verify still belongs to our process (guard against PID reuse).

# User journey
1. In a thread, the user asks the agent to start something long-running (e.g. "start the dev server").
2. The agent runs a bash command with `background: true`; it detaches (new session) and keeps running, and the turn continues without blocking. The tool returns a `bg_id`.
3. zdx records the process (bg_id, pid, pgid, birth_id, command, cwd, thread id, log paths) in the registry.
4. The agent uses `background_output(bg_id)` to confirm it booted (reads a bounded tail, e.g. the "listening on :3000" line) + status, keeps working, and can `background_kill(bg_id)` when done — all within the thread.
5. Later, the user opens the **Monitor Background** tab and sees the process listed under its thread (alongside those from other threads), and can kill it.
6. In the chat **TUI**, the user opens the background overlay for the current thread, sees the running process, and can kill it; a bottom-right `▸ N bg` shows the live count.
7. When a process exits (or is killed), it becomes an exited tombstone briefly (so exit code / startup-failure logs are still readable), then is pruned; active lists/counts drop it immediately.

# Foundations / Already shipped (✅)

## Active-agent marker registry (pattern to mirror)
- What exists: `agent_activity::RunRecord { pid, started_at, thread_id, surface, model, provider, thinking, kind, parent_thread_id, subagent_name }`; `start()` atomically writes `<pid>-<uuid>.json` under `paths::zdx_home()/run/agents/` and returns a `RunGuard` whose `Drop` removes the marker; `list_active()` reads markers, reaps corrupt/stale ones, checks liveness with `kill(pid, 0)`, sorts oldest-first (`crates/zdx-engine/src/agent_activity.rs:20-149`).
- ✅ Demo: start a run; the marker appears under `~/.zdx/run/agents/` and disappears when the run ends.
- Gaps: `RunGuard` cleans up on drop — **wrong** for background processes, which must outlive the turn. The registry keeps the marker until the process dies or is killed, reaps on liveness check, and holds a short exited tombstone.

## Bash process-group spawning (detach primitive)
- What exists: `run_command` spawns `sh -c <cmd>` via `tokio::process::Command`, sets a new process group in Unix `pre_exec`, and `ProcessGroupGuard` kills the group on drop/cancel; timeout SIGTERM/SIGKILLs the group (`crates/zdx-tools/src/bash.rs:182-225,317-413`). `BashInput` has only `command` + `timeout_secs` (`:50-91`).
- ✅ Demo: a normal bash call runs to completion and its process group is cleaned up on cancel/timeout.
- **Gap / bug the feature must fix**: on **normal completion** the guard calls `pg_guard.disarm()` (`bash.rs:404-406`) and does NOT kill the group — so a foreground `cmd &` / `nohup … &` / `disown` leaves live orphans (the reader code even comments that a descendant can inherit pipes and "escape the process group"). Background mode spawns into a new **session** (`setsid`, which also makes a new group), redirects stdout/stderr to log files, registers the process, and returns **without** waiting or killing; foreground mode must instead kill any leftover group members before returning (Phase A).

## Tool → thread-id plumbing
- What exists: engine `ToolContext` carries `current_thread_id: Option<String>` via `.with_current_thread_id(thread_id)` in `build_run_turn_setup` (`crates/zdx-engine/src/core/agent.rs:1035-1151`); TUI supplies the id from `thread_handle.id` (`crates/zdx-tui/src/runtime/handlers/agent.rs:34-62`).
- ✅ Demo: an in-thread turn has the thread id at tool-call time.
- Gaps: `as_leaf()` drops everything except `root` + `timeout` (`crates/zdx-engine/src/tools/mod.rs:131-133`). Registration + the agent tools live in the **engine** layer (`tools/mod.rs:348-376`) where the id is available; the leaf stays engine-free.

## Monitor tab framework + TUI overlay/status line
- Monitor: `Section` enum + exhaustive `Section::ALL`/`next()`/`label()`/`item_count()`; `MonitorApp.active_agents`; `load_active_agents()`; `refresh_app()` on key + 1s tick; per-section `render()` (`crates/zdx-monitor/src/app.rs:231-424,720-765,1178-1249,1482-1637`, `ui.rs:18-63,123-186`).
- TUI: `OverlayRequest`/`Overlay`, `AppState.overlay`, `handle_key`/`render`, `open_overlay_request` (`crates/zdx-tui/src/overlays/mod.rs:63-219`); `timeline.rs:42-180` compact template. Status row `render_status_line` is **left-aligned only**, no right-side region today (`crates/zdx-tui/src/render.rs:340-461`); tick at `update.rs:28-40`.
- ✅ Demo: Active Agents lists live runs; an overlay captures keys; the status line shows `Running bash…`.
- Gaps: adding a tab touches every exhaustive `Section` site; the status line needs a two-column layout for a bottom-right region.

# MVP phases (ship-shaped, demoable) — 3 vertical slices

## Phase A: Background execution + registry + agent tools + minimal CLI + foreground-leak cleanup — ✅ done 2026-07-25
- **Goal**: The agent starts a background command that survives the turn, checks its output, and kills it — all in-thread — and the human has a CLI escape hatch. Establishes the registry every surface reuses. **Also closes the escape hatch**: `background: true` becomes the *only* way to leave a process running; a plain foreground `cmd &` / `nohup … &` no longer silently survives.
- **Scope checklist**:
  - [x] `background_activity` module (mirror `agent_activity.rs`): `BackgroundProcess { bg_id (uuid), pid, pgid, birth_id (OS start-time/identity captured right after spawn), thread_id: Option<String>, command, cwd, started_at, status (running | exited{code, exited_at}), stdout_log, stderr_log }`, marker files `<bg_id>.json` under `paths::zdx_home()/run/background/` (dir `0700`, files `0600`).
  - [x] `list_background()`: read markers, reap; **prune exited tombstones older than a fixed age** (e.g. a few minutes) here — no separate cleanup command. Sorted oldest-first.
  - [x] `kill_background(bg_id)`: **identity guard** — before signalling require `pid alive` AND `birth_id matches` AND `getpgid(pid) == pgid`. Only `ESRCH` = dead; `EPERM` = alive-but-unverifiable → fail closed. Signal the group TERM → poll → KILL; on confirmed exit, flip the marker to `exited` (tombstone), don't delete immediately.
  - [x] **MVP command constraint**: only non-daemonizing commands whose spawned leader stays alive (dev servers, watchers run in the foreground). Self-daemonizing / re-`setsid` / shell-backgrounded workloads are out of scope.
  - [x] Durable per-process log files under `.../run/background/logs/<bg_id>.{out,err}` (`0600`), paths derived from the validated `bg_id` — never unlink arbitrary paths from marker JSON. Running-log growth is unbounded for MVP (documented alpha risk); only the *read* is bounded. Exited-tombstone logs are pruned with the tombstone.
  - [x] Add `background: bool` (default false) to `BashInput` — spawn/log-redirection stays a reusable `zdx-tools::bash` primitive. When true: `sh -c <cmd>` with null stdin into a **new session** (`setsid()`, which also creates a new process group — use it *instead of* the foreground `setpgid`), stdout/stderr → log files, `kill_on_drop(false)`. **Reject** `background: true` with a non-default `timeout_secs`.
  - [x] **Registration atomicity**: preallocate `bg_id` + log files, spawn, capture `birth_id`, atomically commit the running marker, then report success. On identity-capture or commit failure → terminate the group and `wait()` the child before returning an error. (Narrow crash-window leak = accepted alpha limitation, documented.)
  - [x] **Detached waiter**: after registration, `tokio::spawn` a waiter that owns the `Child`, `wait()`s it, and flips the marker to `exited{code}` on exit (Tokio orphan reaping is best-effort — don't just drop the `Child`). On zdx shutdown the waiter is dropped without killing the child (macOS reparents it → it survives).
  - [x] **Register in the engine bash adapter** (`tools/mod.rs:348-376`); the leaf `ToolContext` is unchanged. cwd is already `leaf.root`.
  - [x] Agent tool `background_output`: input `{ bg_id }`; returns a **bounded tail** of stdout/stderr + `status` (running | exited{code}). No cursor, no regex filter. Readable while `running` and during the exited-tombstone window (so startup failures are visible).
  - [x] Agent tool `background_kill`: input `{ bg_id }`; calls `kill_background`; returns `killed` | `already-exited`. Idempotent.
  - [x] **Thread scoping** for both tools: only address `bg_id`s whose record `thread_id` matches the caller's `current_thread_id`; reject others.
  - [x] Bash tool description: how/when to use `background: true` (returns a `bg_id`; output via `background_output`; "no new output" ≠ done — check status; "started" = spawned + registered, not "service ready").
  - [x] **Foreground-leak cleanup (closes the escape hatch)**: today `run_command` calls `pg_guard.disarm()` on normal completion (`crates/zdx-tools/src/bash.rs:404-406`), so a foreground `cmd &` / `nohup … &` / `disown` leaves orphans alive and untracked. Change: after `child.wait()`, check `kill(-pgid, 0)` — if the group still has members, TERM → (async grace) → KILL them **before** draining the pipe readers, then return (optionally with a warning: "foreground command left descendants; they were stopped — use `background: true` for persistent processes"). `EPERM` = members exist but unsignalable → report cleanup failure, don't claim success. This makes foreground truly ephemeral; `background: true` is the only persistent path.
  - [x] **Async kill helper**: `kill_process_group` currently does a synchronous `std::thread::sleep(100ms)` between TERM and KILL (`bash.rs`), which blocks a tokio worker. Make the TERM→grace→KILL poll async (`tokio::time::sleep`); reuse it for both foreground cleanup and `kill_background`.
  - [x] Minimal `zdx bg` CLI: `list` and `kill <bg_id>` only. (No `--json`, `--all`, `--thread`, or `clean` for MVP.)
- **✅ Demo**: agent runs a dev server with `background: true` → turn returns immediately with a `bg_id`; `background_output` shows the "listening on :3000" line + `running`; the agent keeps working; `background_kill` stops it and a follow-up `background_output` reports `exited`. `zdx bg list` shows it while alive; `zdx bg kill <bg_id>` works as a human escape hatch. **Foreground cleanup**: running `sleep 300 &` or `nohup sleep 300 &` as a normal (non-background) bash command → the command returns, and the `sleep` is **gone** (no orphan), because foreground cleanup killed the leftover group.
- **Risks / failure modes**:
  - PID reuse → the `pid + birth_id + pgid` guard; `EPERM` fails closed.
  - Group-leader exits before descendants → constrained away by the non-daemonizing rule + fail-closed on missing leader.
  - Spawn↔marker crash window → accepted alpha leak, documented; caught errors kill+wait.
  - Zombie/orphan → the waiter owns and `wait()`s the `Child`.
  - **Deliberate `setsid`-escape from a foreground command still survives** — it leaves the shell's process group so `kill(-pgid, 0)` can't see it. Accepted alpha limitation (reliable containment needs a sandbox/supervisor — out of scope); the non-daemonizing constraint already documents this class.
  - `thread_id` genuinely `Option` → stored `None`; such processes show only in the "all" view.

## Phase B: TUI background overlay (current thread) + kill + bottom-right indicator — ✅ done 2026-07-25
- **Goal**: In the chat TUI, an overlay lists the current thread's running background processes with kill, and a subtle bottom-right indicator shows the live count. (Overlay + indicator share one cached refresh.)
- **Scope checklist**:
  - [x] `OverlayRequest::Background` + `Overlay::Background(BackgroundState)` (mirror `timeline.rs`): `open()` reads `list_background()` filtered to `app.tui.thread.thread_handle.id` and `status == running`; holds entries + selection + scroll.
  - [x] `handle_key`: navigate, `Esc`/`Ctrl+C` close, a kill key issues a `UiEffect` → `kill_background(bg_id)` → refresh. (No confirm step — selection + explicit kill key is enough.)
  - [x] `render`: rows (command truncated, pid, uptime); empty state "No background processes for this thread."
  - [x] Cache the current-thread running count in `TuiState`, refreshed on the tick (`update.rs:28-40`) at a modest cadence — the overlay and the indicator both read this cache (no per-frame disk reads).
  - [x] Two-column status row in `render_status_line` (`render.rs`): keep left spans, add a right-aligned `▸ N bg` (dim) when N > 0; hide when N == 0 or width is tight.
- **✅ Demo**: start a bg process from a TUI thread → `▸ 1 bg` appears bottom-right; open the overlay, see it, press kill → it stops, indicator clears; the overlay from a different thread is empty.
- **Risks / failure modes**:
  - Overlay reads live selection vs captured list → refresh by thread id, re-clamp selection.
  - Kill effect runs off the render path (runtime effect), not inside `render`.
  - Right-aligned region overlapping long left content on narrow terminals → truncate/hide.

## Phase C: Monitor "Background" tab — ✅ done 2026-07-25
- **Goal**: The Monitor gets a Background tab listing all running background processes grouped by originating thread, with kill.
- **Scope checklist**:
  - [x] Add `Section::Background` and wire every exhaustive site: `Section::ALL`, `next()`, `label()` ("Background"), `item_count()`, `build_app()`, `refresh_app()`, `render()` match, `render_tabs()`, footer hints.
  - [x] `MonitorApp.background: Vec<BackgroundInfo>` + `load_background()` mapping `list_background()` (running only); group rows by `thread_id` with a header row per thread + short id (reuse the `arrange_agent_tree()` grouping idea, flat by thread). Threads that were deleted/archived still group their live processes under an "(unavailable thread)" header.
  - [x] `render_background()`: per-thread grouping; each row shows command (truncated), pid, uptime, cwd; selected row highlighted.
  - [x] Kill key (e.g. `k`) → `kill_background(bg_id)`; refresh. No confirm step.
  - [x] Refreshes on the 1s tick via `refresh_app()`.
- **✅ Demo**: with bg processes under two threads, `zdx monitor` → Background tab shows them grouped by thread; kill key stops the selected one within a tick.
- **Risks / failure modes**:
  - Missing an exhaustive `Section` site → compile error (good) or dead tab; check every match.
  - Kill races the tick reaper → idempotent kill.

# Contracts (guardrails)
- A background process **outlives** the agent turn and the zdx/TUI process that started it (a started dev server keeps running). This is stated in the bash tool + `zdx bg`/overlay UI text.
- Killing requires a passing **identity guard** (`pid alive + birth_id + pgid`) and signals the group TERM→(grace)→KILL; on confirmed exit the marker becomes an `exited` tombstone (not immediately deleted). `EPERM` fails closed — never a blind kill. Kills are idempotent.
- The registry is self-healing: dead/exited processes are reaped/tombstoned by the waiter or the next `list_background()`, and exited tombstones are pruned after a fixed age. A stale marker never blocks listing.
- Active lists, the Monitor tab, and the bottom-right count show only `status == running`. `background_output` may read an exited process during its tombstone window (so startup failures / exit codes are recoverable).
- Registration is commit-then-report: on failure the child is terminated + waited before returning an error (narrow crash-window leak = accepted alpha limitation).
- "Started" means spawned + registered, NOT "service ready".
- **`background: true` is the only way to leave a process running.** Foreground bash is ephemeral: on completion it kills any leftover process-group members (`kill(-pgid, 0)` → TERM→grace→KILL) before returning. This intentionally **changes** the old "foreground behavior unchanged" behavior — that behavior was the bug (it let `cmd &` orphans survive untracked). A deliberate `setsid`-escape is the one accepted leak.
- Agent tools are **thread-scoped**: `background_output`/`background_kill` only address `bg_id`s owned by the caller's current thread.
- Deleting/archiving a thread does **not** kill its background processes; the Monitor groups them under an unavailable-thread header until they exit or are killed.
- Registry dir/logs are user-only (`0700`/`0600`); log paths are derived from validated `bg_id`s; arbitrary paths from marker JSON are never unlinked. Background commands run under the same permission gate as foreground bash.
- Monitor/TUI reads are at most once per timed tick, never per keypress or per frame; the current-thread count is cached.
- Missing/empty/mid-write markers or log files never crash or hang any surface — degrade to an empty list.

# Key decisions (decide early)
- **Naming**: "background process" (entity `BackgroundProcess`, id `bg_id`, CLI `zdx bg`, tab "Background", tools `background_output`/`background_kill`). Not "task".
- **Spawn = bash flag, monitor/kill = dedicated tools** (Claude Code split). No separate spawn tool.
- **Exited tombstone** (fixed short retention) instead of immediate marker deletion — required so `background_output`/exit-code survive kill/exit; active views filter to `running`.
- **`thread_id` is `Option<String>`**; thread-scoped views filter it out, the "all" view includes it.
- **No RAII cleanup** (unlike `RunGuard`): processes outlive the turn; cleanup is the waiter + liveness-reaping + tombstone prune + explicit kill.
- **Kill = identity-guarded pgid signalling** (`pid + birth_id + pgid`, TERM→grace→KILL). pgid-only is insufficient.
- **Detached waiter owns the `Child`** with `kill_on_drop(false)`.
- **`setsid()` for background mode, replacing (not in addition to) the foreground `setpgid`** — one syscall gives a new session + group and survives terminal/TUI exit. Foreground keeps `setpgid`.
- **Foreground = ephemeral, background = persistent (single rule).** Rather than parse the command for `&`/`nohup`/`setsid` (fragile shell syntax) or adopt leaked survivors (unsound on macOS — no kill-session syscall, can't retro-attach logs, dead `sh` leader breaks the identity guard), just kill leftover foreground group members on completion. One clean primitive (`kill(-pgid, 0)`), no string parsing, no adoption.
- **Async TERM→grace→KILL helper** shared by foreground cleanup and `kill_background` (replaces the current synchronous `sleep(100ms)` that blocks a tokio worker).
- **Log reads are bounded; running-log files are unbounded for MVP** (documented risk); no cursor, no regex filter, no `bg clean`.
- **Registration in the engine bash adapter**, not by expanding the leaf `ToolContext`.
- **Registry location**: `paths::zdx_home()/run/background/`; markers keyed by opaque `bg_id`.
- **`background: true` + non-default `timeout_secs` is rejected**.

# Testing
- Manual smoke demo per phase (see each ✅ Demo).
- `cargo nextest run -p zdx-tools -p zdx-engine -p zdx-monitor -p zdx-tui` after the relevant phase; `just ci-fast` for lint.
- Minimal regression tests for pure logic only: `BackgroundProcess` (de)serialization round-trip; `list_background()` reaping a dead pid + pruning an aged tombstone (fixture dir); kill-path identity guard rejects a mismatched birth_id; `background_output` returns a bounded tail + correct status for running vs exited. Foreground-cleanup smoke: `sleep 300 &` and `nohup sleep 300 &` run as a foreground bash command are **gone** after the call returns; a `background: true` process **survives** the call and is killable. Avoid TUI-render tests.

# Polish rounds (after MVP)

## Polish round 1: Inspect output in the UIs
- View a process's stdout/stderr logs from the Monitor Background tab and the TUI overlay (reuse the transcript/logs overlay pattern to tail the durable log file).
- ✅ Check-in demo: select a running process, open its output, see the latest log lines refresh on the tick.

## Polish round 2: Richer metadata + counts
- Per-thread counts in the Monitor tab headers and the thread list; briefly show a just-exited process's exit code before the tombstone prunes.
- ✅ Check-in demo: the Background tab shows `thread X — 2 bg`, and an exiting process shows its exit code for a moment.

# Later / Deferred
- **Restart a background process** (kill + re-run the recorded command in the same cwd/thread). Trigger: killing + re-asking the agent feels repetitive.
- **Incremental output** (per-`bg_id` read cursor / "only new since last check") and **regex output filtering**. Trigger: agents repeatedly poll long logs and the bounded tail is too coarse.
- **Live log rotation / hard size cap** on running processes; **`zdx bg clean`** + bulk/`--json` CLI. Trigger: a chatty server fills disk, or manual cleanup / scripting becomes painful.
- **Persistence across zdx restarts already works** (detached children survive); **surviving a machine reboot** (respawn on boot) is out — revisit only on real need.
- **Interactive/PTY processes** (attach a TTY, send stdin). Trigger: a workflow needs an interactive REPL/server.
- **Resource limits / sandboxing** — folds into the separate "Sandbox Safe Execution (macOS)" idea.
