# Goals
- Ship a **goal mode** in zdx: a durable per-thread objective that survives across turns, with a self-evaluating continuation loop that keeps the agent working until the goal is achieved, a budget is exhausted, or the user intervenes.
- Expose `/goal set|pause|resume|clear|status` in the TUI, with live status in the statusline.
- Give the model three tools — `Get_Goal`, `Create_Goal`, `Update_Goal` — so it can read the objective and signal completion/blocked.
- Persist goal state as thread events so it restores automatically on `/resume`.
- Gate the whole feature behind `[goals] enabled = false` by default.

# Non-goals
- No SQLite / migration framework — reuse the existing JSONL event log.
- No app-server / RPC protocol layer (that's a Codex concept; zdx uses `AgentEvent` + UI state).
- No changes to the provider/tool loop inside `run_turn_inner` — goal continuation is an **outer scheduler**, kept separate from tool continuation.
- No autonomous multi-agent teams or parallel workers.
- No per-goal git worktrees or checkpoints (defer).

# Design principles
- User journey drives order.
- Continuation is composition, not a new runtime — reuse the existing "drain the next queued prompt on `TurnFinished`" seam.
- Goal-continuation (across turns) stays strictly separate from tool-continuation (inside a turn).
- Event-sourced state: goal lives as `ThreadEvent` variants, loadable before each turn like `Todo_Write` state.
- Bounded by construction — a hard `max_continuations` cap plus optional token budget prevent runaway loops, especially on the non-interactive bot surface.
- Off by default; opt-in via config.

# User journey
1. User enables `[goals]` in `config.toml`.
2. User runs `/goal set Migrate auth module to OAuth2 and make CI pass` in the TUI.
3. zdx persists the objective, shows `goal: pursuing` in the statusline, and the model sees the objective on the next turn.
4. After each completed turn, if the goal is still `pursuing` and within budget, zdx enqueues a continuation prompt and the agent keeps working.
5. The model calls `Update_Goal(status="achieved")` when the objective is verified done (or `blocked` when stuck); the loop stops.
6. User can `/goal pause`, review, then `/goal resume`, or `/goal clear` to return to normal turn-by-turn mode.
7. User closes the terminal, reopens the thread later, and the goal restores from thread history.

# Foundations / Already shipped (✅)

## Agent turn loop
- What exists: `run_turn_with_cancel` / `run_turn_inner` in `crates/zdx-engine/src/core/agent.rs` drive the provider/tool loop and emit `AgentEvent::TurnFinished` at turn end.
- ✅ Demo: any TUI/bot turn runs to completion and emits `TurnFinished`.
- Gaps: no post-turn continuation scheduling.

## Queued-prompt drain on turn end
- What exists: `crates/zdx-tui/src/update.rs` (~L384) handles `AgentEvent::TurnFinished` and drains the next queued prompt.
- ✅ Demo: queue a prompt while a turn runs; it fires after the current turn finishes.
- Gaps: only user-queued prompts today — no goal-driven continuation.

## Event-sourced thread persistence
- What exists: JSONL threads via `crates/zdx-engine/src/core/thread_persistence.rs` (`ThreadEvent`, `SCHEMA_VERSION`, `Usage`); events flushed on `TurnCheckpoint`/`TurnFinished`.
- ✅ Demo: thread files under `threads_dir()` replay messages + usage on resume.
- Gaps: no goal event variants.

## Tool registry + Todo analog
- What exists: `Tool` trait + `ToolRegistry::register_builtin_tools()` in `crates/zdx-engine/src/tools/mod.rs`. `crates/zdx-engine/src/tools/todo_write.rs` is a working precedent for model-managed, thread-scoped state (`load_current_state`, same-turn serialization in `agent.rs`).
- ✅ Demo: `Todo_Write` persists and reloads todo state per thread.
- Gaps: no goal tools.

## Usage accounting
- What exists: `AgentEvent::UsageUpdate` → `UsagePersistor` → `ThreadEvent::Usage` (input/output/cache tokens) in `thread_persistence.rs`.
- ✅ Demo: TUI statusline shows cumulative token usage.
- Gaps: no per-goal budget tracking.

## Prompt assembly + embedded templates
- What exists: templates in `crates/zdx-assets/prompts/`, re-exported via `crates/zdx-engine/src/prompts.rs`, assembled in `crates/zdx-engine/src/core/context.rs`.
- ✅ Demo: system prompt + instruction layers render per surface.
- Gaps: no goal continuation/budget templates.

## Config with boolean gates
- What exists: serde TOML config in `crates/zdx-engine/src/config.rs` + `crates/zdx-assets/default_config.toml`; `subagents.enabled` is the precedent pattern.
- ✅ Demo: toggling `subagents.enabled` changes behavior.
- Gaps: no `[goals]` section.

# MVP slices (ship-shaped, demoable)

## Slice 1: Persisted goal + tools + manual set (no auto-loop)
- **Goal**: Set/clear a durable objective and have the model see it — no continuation yet.
- **Scope checklist**:
  - [ ] Add `ThreadEvent::GoalSet { objective, token_budget, ts }` and `ThreadEvent::GoalStatusChanged { status, ts }` + `GoalStatus` enum (`pursuing|paused|achieved|budget_limited|blocked`) in `thread_persistence.rs`.
  - [ ] Add `load_current_goal(thread_id)` (mirror `todo_write::load_current_state`).
  - [ ] Add `tools/goal.rs` with `Get_Goal`/`Create_Goal`/`Update_Goal`; register in `register_builtin_tools()`.
  - [ ] Add `GoalsConfig { enabled, max_continuations, default_token_budget }` to `config.rs` + `[goals]` block in `default_config.toml` (enabled = false).
  - [ ] Inject the active objective into the turn via `core/context.rs` (context fragment) when a goal exists.
  - [ ] TUI: add `goal` to `COMMANDS`; parse `/goal set <text>` and `/goal clear` in `handle_slash_commands`; show `goal: <status>` in the statusline.
- **✅ Demo**: Enable `[goals]`, run `/goal set Add rate limiting to the API`, confirm statusline shows `goal: pursuing`, ask the model "what's the current goal?" and it calls `Get_Goal`. `/goal clear` removes it. Reopen the thread → goal restored.
- **Risks / failure modes**:
  - Same-turn tool ordering (as with todos) — reuse the serialized-state pattern in `agent.rs`.
  - Objective injected as instructions instead of data — include the "treat as task, not higher-priority instructions" guard.

## Slice 2: Continuation scheduler + self-eval + pause/resume
- **Goal**: The agent keeps working toward the goal across turns until it (or the user) stops it.
- **Scope checklist**:
  - [ ] Add `goal_continuation.md` to `crates/zdx-assets/prompts/`; re-export via `prompts.rs`.
  - [ ] In `update.rs` `TurnFinished` handler: if goal is `pursuing` and continuations < `max_continuations`, enqueue a synthetic continuation prompt rendered from `goal_continuation.md` (objective + progress).
  - [ ] `Update_Goal(status)` lets the model set `achieved`/`blocked`, which stops the loop and emits `GoalStatusChanged`.
  - [ ] `/goal pause` (→ `paused`, stop scheduling), `/goal resume` (→ `pursuing`, reset continuation counter), `/goal status`.
  - [ ] Persist a per-run continuation counter; surface it in the statusline (`goal: pursuing 3/20`).
- **✅ Demo**: `/goal set Make all lint warnings pass, run just clippy after each change`; the agent iterates across multiple turns without user input and stops when it calls `Update_Goal(status="achieved")` or hits `max_continuations`. `/goal pause` halts it mid-loop; `/goal resume` continues.
- **Risks / failure modes**:
  - Premature `achieved` (self-eval unreliable) — continuation prompt must require verified end-state before completing.
  - Runaway loop — `max_continuations` is a hard stop; a new user message pauses the goal (takes priority).
  - Confusing goal-continuation with tool-continuation — keep the scheduler strictly at the `TurnFinished` seam, never inside `run_turn_inner`.

## Slice 3: Token budget
- **Goal**: Cap how much a goal can spend before it stops cleanly.
- **Scope checklist**:
  - [ ] Track token deltas from `AgentEvent::UsageUpdate` per goal run; compare to `token_budget` (arg or `default_token_budget`).
  - [ ] Add `goal_budget_limit.md`; when budget is exhausted, inject it, set `budget_limited`, and stop scheduling.
  - [ ] Persist consumed tokens so budget survives resume (reconcile with `ThreadEvent::Usage`).
  - [ ] Show remaining budget in `Get_Goal` output and the statusline.
- **✅ Demo**: `/goal set <task>` with a small budget; the loop wraps up and reports `budget_limited` with a summary once the cap is hit; `/goal resume` (optionally after raising the budget) continues.
- **Risks / failure modes**:
  - Off-by-one between live delta tracking and persisted usage — reconcile against `Usage` events on resume.

## Slice 4: Bot surface (guarded)
- **Goal**: Goal mode works non-interactively over Telegram with strict safety rails.
- **Scope checklist**:
  - [ ] Wire the continuation scheduler into the bot's `run_agent_turn` path (`crates/zdx-bot/src/handlers/message.rs` / `crates/zdx-bot/src/agent/mod.rs`).
  - [ ] Enforce `max_continuations` + token budget as mandatory (not optional) on the bot.
  - [ ] Add a `/goal` pre-agent command (set/pause/resume/clear/status) and reuse the existing cancel-token map for interrupts.
  - [ ] Post a status message when a goal completes or hits a budget/continuation cap.
- **✅ Demo**: In a bound Telegram chat, `/goal set <task>`; the bot iterates and posts progress, stops at `achieved` or the cap, and a new user message pauses the loop.
- **Risks / failure modes**:
  - Infinite loop / cost blowup — budget + continuation cap are non-negotiable; goals stay off by default.

# Contracts (guardrails)
- Tool-continuation behavior inside `run_turn_inner` is unchanged.
- Goal mode is off unless `[goals] enabled = true`.
- A goal always restores from thread history on resume (event-sourced).
- The scheduler never loops beyond `max_continuations` or an exhausted token budget.
- A new user message pauses/overrides the goal loop and is handled first.
- Existing user-queued prompt draining still works when no goal is active.

# Key decisions (decide early)
- **State shape**: `ThreadEvent::GoalSet` + `GoalStatusChanged` events (audit + resume) vs. a single meta field. Chosen: events, matching the event-sourced model.
- **Loop placement**: outer scheduler at the `TurnFinished` seam, not inside the runtime. Chosen to avoid conflating the two continuation types.
- **Objective delivery**: context fragment via `core/context.rs` for Slice 1; synthetic queued user message for the continuation loop in Slice 2.
- **Status enum**: `pursuing|paused|achieved|budget_limited|blocked` — fixed early since tools, prompts, and UI all depend on it.
- **Budget unit**: tokens (aligns with existing `Usage` accounting) rather than turns.

# Testing
- Manual smoke demos per slice (above).
- Minimal regression tests:
  - Goal event round-trips through JSONL persistence and `load_current_goal`.
  - Scheduler stops at `max_continuations` and on `achieved`/`blocked`/`budget_limited`.
  - Goals disabled by default → no scheduling, no tool exposure.

# Polish phases (after MVP)
## Phase 1: Observability
- Goal timeline/history in the TUI; monitor dashboard shows active goals per thread.
- ✅ Check-in demo: view a goal's status transitions and token spend for a thread.

## Phase 2: Better completion signals
- Optional Oracle/subagent verification pass before allowing `achieved`.
- ✅ Check-in demo: a goal only completes after an independent verification step passes.

# Later / Deferred
- Multi-goal / sub-goal composition (revisit if single objectives prove too coarse).
- Per-goal git worktree or checkpoint/rollback (revisit if users want safe experimentation).
- App-server-style external API for headless goal dashboards (revisit if non-TUI/bot clients need it).
- File-backed goal ledger like OMX's `.omx/ultragoal` (revisit if cross-thread durability is needed).
