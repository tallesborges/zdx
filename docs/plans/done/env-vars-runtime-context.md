# Environment Variables as Runtime Context Source of Truth

## SPEC reference: §11 Environment Variables (Runtime Context)

# Goals
- Expose `ZDX_ARTIFACT_DIR` and `ZDX_THREAD_ID` as process env vars so bash commands, skill scripts, and subagents use them natively
- Simplify `<environment>` block to a lightweight index (env var names, not resolved paths)
- Update all skills and automations to reference `$ZDX_ARTIFACT_DIR` instead of `<artifact_dir>` from the `<environment>` block

# Non-goals
- Changing how `ZDX_HOME` works (already an env var)
- Adding new env vars beyond `ZDX_ARTIFACT_DIR` and `ZDX_THREAD_ID`
- Changing artifact directory layout (that's a separate plan: `../done/artifacts-refactoring.md`)

# Design principles
- User journey drives order
- Single source of truth — env vars are the authority for paths; prompt doesn't duplicate values
- Env vars just work in bash — no model interpolation needed

# User journey
1. Agent starts (TUI, exec, or bot) → env vars are set
2. Model sees `<environment>` listing available env var names
3. Model uses `$ZDX_ARTIFACT_DIR` directly in bash/skill commands (no parsing XML)
4. Subagent processes inherit env vars automatically

# Foundations / Already shipped (✅)

## ZDX_HOME env var
- What exists: `zdx_home()` in `config.rs:373` reads `ZDX_HOME` env var, falls back to `~/.zdx`
- ✅ Demo: `echo $ZDX_HOME` in bash tool shows the path
- Gaps: none

## artifact_dir_for_thread()
- What exists: `config.rs:399` computes `$ZDX_HOME/artifacts/threads/<id>` (or `/scratch`)
- ✅ Demo: value appears in system prompt `<environment>` block
- Gaps: only in prompt text, not an env var

## <environment> block in system prompt
- What exists: `system_prompt_template.md` renders `<cwd>`, `<date>`, `<artifact_dir>`, `<thread_id>` as XML values
- ✅ Demo: system prompt contains resolved paths inline
- Gaps: paths embedded as prompt text, not as env vars

## Bash tool inherits env
- What exists: `tools/bash.rs:249` uses `Command::new("sh")` with additive `.env()` calls, no `env_clear()`
- ✅ Demo: `echo $ZDX_HOME` works in bash tool
- Gaps: none — any process env var is inherited

## Subagent inherits env
- What exists: `subagent.rs:49` uses `Command::new(exe)` with no `env_clear()`
- ✅ Demo: subagent sees parent's env vars
- Gaps: none

# MVP slices (ship-shaped, demoable)

## Slice 1: Set env vars at agent startup ✅

- **Goal**: `ZDX_ARTIFACT_DIR` and `ZDX_THREAD_ID` are real env vars visible to all child processes
- **Scope checklist**:
  - [x] Add `pub fn set_runtime_env(thread_id: Option<&str>)` in `zdx-core/src/core/context.rs`
    - Sets `ZDX_ARTIFACT_DIR` via `paths::artifact_dir_for_thread(thread_id)`
    - Sets `ZDX_THREAD_ID` to `thread_id` or empty string
  - [x] Call from TUI entry: `crates/zdx-tui/src/lib.rs` before `build_effective_system_prompt` (line ~60)
  - [x] Call from exec entry: `crates/zdx-cli/src/modes/exec.rs` at top of `run_exec()` (line ~51)
    - Also fixed: exec now passes `thread_id` to `build_effective_system_prompt_with_paths` (was `None`)
  - [x] Call from bot entry: `crates/zdx-bot/src/agent/mod.rs` at top of `spawn_agent_turn()` (line ~89)
- **✅ Demo**: Run `zdx exec -p 'echo $ZDX_ARTIFACT_DIR && echo $ZDX_THREAD_ID'` → prints resolved paths
- **Risks / failure modes**:
  - `set_var` is `unsafe` in Rust 2024 (process-global mutation) — same pattern as existing `ZDX_DEBUG_TRACE` in `cli/mod.rs:414`. Acceptable since it's called once at startup before concurrent work.

## Slice 2: Simplify `<environment>` block in system prompt template ✅

- **Goal**: `<environment>` lists env var names + inline non-path metadata only
- **Scope checklist**:
  - [x] Update `crates/zdx-core/prompts/system_prompt_template.md`:
    ```xml
    <environment>
    Available env vars: $ZDX_HOME, $ZDX_ARTIFACT_DIR, $ZDX_THREAD_ID
    Current directory: {{ cwd }}
    Current date: {{ date }}
    </environment>
    ```
  - [x] Remove `artifact_dir` and `thread_id` from `PromptTemplateVars` struct (context.rs ~132)
  - [x] Remove their computation from `build_template_vars()` (context.rs ~277)
  - [x] Remove `thread_id` parameter from `build_prompt_template_vars` (internal), `build_effective_system_prompt_with_paths`, and the layered effective-prompt builder public API
  - [x] Keep `cwd` and `date` in template vars (model reasons about these without running commands)
  - [x] Update tests that assert on `<environment>` block content (context.rs)
  - [x] Update all callers: TUI (lib.rs, handoff.rs, thread.rs), exec, bot
- **✅ Demo**: System prompt shows env var names, not resolved paths. `zdx exec -p 'echo $ZDX_ARTIFACT_DIR'` still works.
- **Risks / failure modes**:
  - Models that don't understand env vars well might not use `$ZDX_ARTIFACT_DIR` — mitigated by skills explicitly telling them the command syntax.

## Slice 3: Update skills to reference `$ZDX_ARTIFACT_DIR`

- **Goal**: All skills use env var syntax instead of `<artifact_dir>` from `<environment>`
- **Scope checklist**:
  - [ ] `~/.zdx/skills/html-page/SKILL.md` (line 95): `Save to "$ZDX_ARTIFACT_DIR/<name>.html"` (fallback: cwd)
  - [ ] `~/.zdx/skills/imagine/SKILL.md` (lines 26, 29, 44, 47, 249): `--out "$ZDX_ARTIFACT_DIR/name.png"`
  - [ ] `~/.zdx/skills/screenshot/SKILL.md` (line 12): `Save to "$ZDX_ARTIFACT_DIR/"`
- **✅ Demo**: Ask agent to generate an HTML page → it uses `$ZDX_ARTIFACT_DIR` in the save command, file lands in correct thread artifact dir.
- **Risks / failure modes**:
  - Skills are user-editable files outside the repo — changes won't be version-controlled. Document the convention so future skills follow it.

## Slice 4: Update automations to reference `$ZDX_ARTIFACT_DIR`

- **Goal**: Automations use env var syntax
- **Scope checklist**:
  - [ ] `~/.zdx/automations/morning-report.md` (lines 81, 141): `"$ZDX_ARTIFACT_DIR/morning-report.html"`
  - [ ] `~/.zdx/automations/zdx-daily-interactions-summary.md` (lines 46, 118): `"$ZDX_ARTIFACT_DIR/zdx-interactions.html"`
- **✅ Demo**: Run `zdx automations run morning-report` → HTML saved to `$ZDX_ARTIFACT_DIR/`.
- **Risks / failure modes**:
  - Same as skills — user-editable files.

# Contracts (guardrails)
- `ZDX_HOME` behavior unchanged
- Bash tool and subagent processes must see all `ZDX_*` env vars (no `env_clear()`)
- `cwd` and `date` remain inline in `<environment>` (model needs these for reasoning without tool calls)
- Missing `ZDX_ARTIFACT_DIR` (e.g., old skills) must not crash — fallback to cwd is acceptable

# Key decisions (decide early)
- **Where to call `set_runtime_env`**: At each surface entry point (TUI/exec/bot), NOT inside `build_effective_system_prompt`. Prompt building should be side-effect-free.
- **Keep `cwd` and `date` inline**: These are reasoning metadata, not paths for commands. They stay in the `<environment>` block as values.

# Testing
- Manual smoke: `zdx exec -p 'echo $ZDX_ARTIFACT_DIR && echo $ZDX_THREAD_ID'`
- Manual smoke: `zdx exec --no-thread -p 'echo $ZDX_ARTIFACT_DIR'` → should show scratch dir
- `just ci` passes (existing context.rs tests updated)

# Polish phases (after MVP)

## Phase 1: Memory root as env var
- Expose `ZDX_MEMORY_ROOT` as the canonical memory env var
- Memory skill derives `Notes/`, `Calendar/`, and `Notes/MEMORY.md` under that root
- ✅ Check-in demo: memory skill uses `$ZDX_MEMORY_ROOT/...` in commands

# Later / Deferred
- **Keep a single memory env** — do not reintroduce separate notes/daily env vars unless a real use case appears.
- **`ZDX_CWD` env var** — not needed; `cwd` is standard and model can use `pwd`. Trigger: if a skill explicitly needs it.
