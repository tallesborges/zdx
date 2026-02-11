# Goals
- Ship **Automations** as a first-class feature using Markdown files with frontmatter + prompt body.
- Keep v1 extremely simple: file-name identity, local-time schedules, and manual run support first.
- Ship both required pieces: **automation runtime** and **automation authoring skill**.
- Make automations safe and operable via bounded execution + run history.

# Non-goals
- Full autonomous planning/goal selection in MVP.
- Web UI/dashboard for automations.
- Multi-user permissions model.
- Complex automation-to-automation orchestration.

# Design principles
- User journey drives order
- Ship-first: manual utility first, scheduler second
- File-based and human-editable definitions
- Keep runtime state out of automation definition files
- Reuse existing execution primitives (`zdx exec`, subagent, threads, worktree)

# User journey
1. User asks to create an automation (e.g., daily morning report).
2. Skill creates `$ZDX_HOME/automations/<name>.md` with valid frontmatter + prompt body.
3. User validates and runs it manually once.
4. User runs daemon to execute scheduled automations automatically.
5. User reviews run outcomes and iterates prompt/frontmatter.

# Foundations / Already shipped (✅)
List capabilities that already exist and should not be rebuilt.

## Execution path (`zdx exec`)
- What exists: stable single-shot execution.
- ✅ Demo: `zdx exec -p "hello"`
- Gaps: no advanced run filtering/analytics yet.

## Thread persistence + transcript tooling
- What exists: append-only threads + `read_thread`.
- ✅ Demo: `zdx threads list`, `zdx threads show <id>`, `read_thread`.
- Gaps: no native `search_threads` yet (deferred).

## Isolation primitives
- What exists: `invoke_subagent`, worktree helpers.
- ✅ Demo: `zdx worktree ensure <id>`
- Gaps: no worktree-first automation mode yet.

## Telegram channel
- What exists: bot runtime.
- ✅ Demo: `zdx bot`
- Gaps: no automation delivery flow contract in MVP.

## Automations v1 (CLI + parser)
- What exists: markdown automation discovery + parsing, validation, manual run, and daemon scheduler loop.
- ✅ Demo: `zdx automations validate && zdx automations run <name> && zdx daemon`
- Gaps: first-party morning-report template flow still needs final polish.

# MVP slices (ship-shaped, demoable)
Define Slice 1..N in user-journey order.

## Slice 1: Automation contract + discovery + validate + manual run
- **Goal**: One automation file can be discovered, validated, and run manually.
- **Scope checklist**:
  - [x] Add automation discovery path (`$ZDX_HOME/automations/*.md`).
  - [x] Identity = **file stem** (no `id` field).
  - [x] Parse frontmatter + markdown body prompt.
  - [x] v1 frontmatter: `schedule?`, `model?`, `timeout_secs?`, `max_retries?`.
  - [x] Defaults: `schedule` missing => manual-only, `model` inherits global config.
  - [x] Add commands: `zdx automations list`, `validate`, `run <name>`.
- **✅ Demo**: `$ZDX_HOME/automations/morning-report.md` validates and runs with `zdx automations run morning-report`.
- **Risks / failure modes**:
  - Weak parse/validation errors.
  - Naming collisions if duplicate file stems in multiple roots.

## Slice 2: Authoring skill (scaffold + edit)
- **Goal**: Creating automations is one prompt away.
- **Scope checklist**:
  - [x] Add `automations` skill to scaffold valid file structure.
  - [x] Skill asks/fills required practical fields only.
  - [x] Skill supports updating an existing automation by file name.
  - [x] Skill avoids overwriting without explicit confirmation.
- **✅ Demo**: “Create a daily morning report automation” generates valid file; `validate` passes; `run` works.
- **Risks / failure modes**:
  - Skill emits invalid frontmatter.
  - Accidental overwrite of existing files.

## Slice 3: Scheduler daemon for scheduled automations
- **Goal**: Scheduled automations run automatically.
- **Scope checklist**:
  - [x] Add `zdx daemon` loop polling due automations.
  - [x] Support schedule types for v1 (cron expression string).
  - [x] Use **local machine timezone** only in MVP.
  - [x] Ensure restart-safe dedupe for due windows.
- **✅ Demo**: Start daemon; scheduled `morning-report` runs at expected local time exactly once.
- **Risks / failure modes**:
  - Duplicate execution after restart.
  - Cron parsing edge cases.

## Slice 4: Run history + guardrails
- **Goal**: Make automations reliable and debuggable.
- **Scope checklist**:
  - [x] Persist run logs: start/end/duration/status/error/result summary.
  - [x] Enforce per-run timeout.
  - [x] Enforce bounded retries (`max_retries` + backoff).
  - [x] Keep run metadata in runtime store (not in `.md` files).
- **✅ Demo**: Forced-failure automation shows retry behavior and clear final error in run history.
- **Risks / failure modes**:
  - Runaway retries if limits fail.
  - Poor observability slows debugging.

## Slice 5: Morning report as first “daily value” automation
- **Goal**: Deliver concrete autonomous value quickly.
- **Scope checklist**:
  - [ ] Ship first-party morning-report template through skill.
  - [ ] Prompt pattern: priorities, blockers, next actions from recent thread context.
  - [ ] Ensure output is saved/retrievable (and delivered via configured channel where applicable).
- **✅ Demo**: Morning report appears daily and is actionable.
- **Risks / failure modes**:
  - Low-quality context selection leads to weak report usefulness.

## Implementation notes (current)
- `zdx-core` now has `automations.rs` for discovery/parsing + cron matching.
- Project skill added at `.zdx/skills/automations/SKILL.md` for create/update/validate/run workflows.
- `zdx` CLI now supports:
  - `zdx automations list`
  - `zdx automations validate`
  - `zdx automations runs [name]`
  - `zdx automations run <name>`
  - `zdx daemon [--poll-interval-secs N]`
- Manual and daemon automation runs reuse `exec` path and support per-automation `model`, `timeout_secs`, and `max_retries`.
- Default automation runs are `--no-thread` unless a thread is explicitly provided.
- Daemon restart dedupe state is persisted in `<ZDX_HOME>/automations_daemon_state.json`.
- Run history is appended to `<ZDX_HOME>/automations_runs.jsonl` (JSONL, no DB).
- Verified user-created automation at `<ZDX_HOME>/automations/morning-report.md`; validation now passes with current frontmatter schema.

# Contracts (guardrails)
List non-negotiable behaviors that must not regress (derived from Inputs and existing behavior).
- Automation identity is file-name stem (no mandatory `id` in frontmatter).
- Automation prompt source of truth is markdown body.
- Missing `schedule` means manual-only; must not be auto-run.
- Scheduler must run only automations with a valid matching `schedule`.
- Runtime state/logs must not mutate automation definition files.
- Each run is bounded (timeout/retries) and auditable.

# Key decisions (decide early)
List only decisions that would cause rework if postponed (derived from Inputs).
- User-facing name is **Automations**.
- Definition format is Markdown + frontmatter + body.
- No `id`; identity comes from filename.
- No timezone config in v1; use local machine timezone.
- Skill is the authoring UX; core feature is execution/scheduling.
- Manual run ships before scheduler automation.

# Testing
- Manual smoke demos per slice
- Minimal regression tests only for contracts
- Parser tests for defaults + validation failures
- Scheduler tests for due-time computation + restart dedupe
- Guardrail tests for timeout/retry behavior

# Polish phases (after MVP)
Group improvements into phases, each with a ✅ check-in demo.
Limited strictly to scope present in Inputs.

## Phase 1: Authoring UX polish
- Better validation messages and autofix hints.
- Template variants for common automation types.
- ✅ Check-in demo: new automation created + validated in one interaction.

## Phase 2: Context quality improvements
- Improve retrieval quality for report-style automations.
- ✅ Check-in demo: morning report consistently references relevant recent work.

## Phase 3: Optional schedule/time sophistication
- Add explicit timezone only if local-time-only causes real pain.
- ✅ Check-in demo: same automation runs at expected wall-clock time across host timezone changes.

# Later / Deferred
Explicit list of “not now” items + what would trigger revisiting them.
- Automatic autonomous task picking from all notes/threads by default (revisit after scheduler trust is established).
- Web dashboard for automations (revisit when CLI/bot UX becomes limiting).
- Multi-user permissions/roles (revisit if scope moves beyond personal use).