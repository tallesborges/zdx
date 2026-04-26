# Goals
- Ship a shared workflow engine in `zdx-engine` that reads structured workflow files and drives multi-step agent loops.
- Ship a built-in `/plan` workflow: draft plan → Oracle review → revise until PASS, BLOCKED, or max iterations.
- Ship a built-in `/ralph` workflow: implement → review → fix → iterate until PASS or max iterations.
- Activate workflows via slash commands (`/plan`, `/ralph`) in TUI and Telegram.
- Workflow files use YAML frontmatter (family, steps, gates, max_iterations, artifact metadata) + markdown prompts — easy to edit without recompiling.

# Non-goals
- A heavy workflow DSL or graph editor.
- Orchestrator-as-agent (a dedicated agent that coordinates other agents).
- tmux-based team management or parallel worker processes.
- Automatic workflow activation (workflows are always explicitly triggered by the user).
- Turning `deep-interview` into a workflow — it remains a conversational skill.
- Replacing normal direct chat/task flow for users who don't want workflows.

# Scope + supersession
- This plan supersedes `docs/plans/active/workflow-bundles-ship-first-plan.md` for the workflow-engine direction.
- The older plan can remain as historical context, but this file is the source of truth for planning/execution workflow architecture.

# Design principles
- User journey drives order
- Workflows are composition, not intelligence — the engine drives the loop, subagents do the thinking.
- One shared engine, two workflow families: `planning` and `execution`.
- Explicit activation only — user starts a workflow intentionally; steps inside run automated.
- Simple subagents with prompt-shaped behavior (Oracle reviews via prompt, not a dedicated reviewer agent in MVP).
- Non-trivial planning must include enforced critique and revision — draft alone is not enough.
- The drafting agent should assume blind spots on architecture/contracts and explicitly seek Oracle review.
- Gate-based iteration with bounded max iterations for safety.
- Workflow files follow the same pattern as skills: bundled built-ins that users can modify.
- Planning workflows produce and revise artifacts; execution workflows consume approved artifacts.

# User journey
1. User has a vague request and optionally runs `deep-interview` to clarify requirements.
2. User activates `/plan` in TUI or Telegram.
3. Engine reads the plan workflow file, gathers context, drafts the plan, sends it to Oracle for review, and revises it as needed.
4. Engine persists the canonical plan to `docs/plans/active/<slug>.md` and presents the reviewed result.
5. User explicitly approves the plan, and ZDX records that approval in thread-scoped workflow metadata.
6. User activates `/ralph` in TUI or Telegram.
7. Engine reads the ralph workflow file, starts implementation against the approved plan, automatically reviews, fixes, and iterates as needed.
8. User sees structured results in the same transcript with clear step labels, verdicts, and iteration status.

# Foundations / Already shipped (✅)

## Named subagents
- What exists: `invoke_subagent` with built-in `task`, `oracle`, `explorer`, and `thread-searcher` roles. Isolated child runs with prompt-driven behavior.
- ✅ Demo: `invoke_subagent(subagent: "oracle", prompt: "Review this plan")`
- Gaps: None for MVP — Oracle can serve as the review agent via prompt.

## Skills system
- What exists: file-based skills in `$ZDX_HOME/skills/` and `.zdx/skills/` with YAML frontmatter + markdown. Bundled skills in `zdx-assets/bundled_skills/`. Discovery, precedence, and loading all work.
- ✅ Demo: `ship-first-plan` and `deep-interview` skills both load and execute correctly.
- Gaps: None — workflow files will follow the same discovery/precedence pattern.

## Slash commands
- What exists: `/` commands in TUI for model switching, thinking level, etc.
- ✅ Demo: Type `/` in TUI to see command palette.
- Gaps: No workflow activation commands yet.

## Thread persistence + transcript
- What exists: threads persist messages and tool activity. Both TUI and bot surfaces show multi-step work in the transcript.
- ✅ Demo: tool calls and subagent results visible in thread history.
- Gaps: No workflow phase/iteration state in threads yet.

## Planning pipeline (skills)
- What exists: `ship-first-plan` skill for structured planning with Explorer context gathering, Oracle review, and plan persistence to `docs/plans/active/`. `deep-interview` skill for Socratic requirements clarification (new, not yet battle-tested).
- ✅ Demo: Ask for a plan → Explorer gathers context → structured plan with slices generated → saved to file.
- Gaps: Planning review/revision is still prompt-driven and unreliable; no engine-enforced draft → review → revise loop yet.

# Architecture placement
- The workflow runner lives in `zdx-engine` so all surfaces can reuse one engine-owned implementation.
- The first activation surfaces are TUI and Telegram.
- MVP workflow visibility comes from explicit transcript/system status messages, not hidden engine state alone.
- `deep-interview` remains a skill outside the engine loop.
- `ship-first-plan` becomes a prompt component used by the `/plan` workflow draft step, not the final enforcement mechanism itself.

# Workflow families

## Planning workflows
- Produce or revise artifacts.
- Primary built-in workflow: `/plan`.
- Default loop: `context/explore -> draft -> review -> revise`.
- Primary review outcomes: `PASS`, `NEEDS_REVISION`, `BLOCKED`.
- Canonical artifact: `docs/plans/active/<slug>.md`.
- `PASS` means “plan review passed”, not “user approved”. User approval is a separate final gate.

## Execution workflows
- Consume approved artifacts or explicit user tasks.
- Primary built-in workflow: `/ralph`.
- Default loop: `implement -> review -> fix`.
- Primary review outcomes: `PASS`, `NEEDS_FIX`.
- Approved plan path is injected as structured context when available.

# Persistence + source of truth
- `docs/plans/active/<slug>.md` is the canonical plan artifact for a planning workflow run.
- The engine allocates and owns `artifact_path` before the draft step begins. Draft/revise steps receive that exact path and are expected to write only there.
- The workflow state stores metadata, not duplicate plan content:
  - `workflow_name`
  - `workflow_run_id`
  - `workflow_family`
  - `artifact_path`
  - `approval_status`
  - `approved_artifact_path`
  - `approved_artifact_hash`
  - `approved_workflow_run_id`
  - `approved_at`
  - `approved_by`
  - `status`
  - `current_step`
  - `iteration`
  - `last_verdict`
- During one `/plan` run, the engine stays locked to a single `artifact_path`.
- Draft step writes the first version to the engine-provided path; revise step updates the same file atomically.
- Transcript stores review reports and status events, but the active plan file remains the source of truth.
- `/ralph` only auto-consumes a plan when `approval_status=approved`, `approved_artifact_path` is present, and the current file hash still matches `approved_artifact_hash`.
- If the plan file changes after approval, approval becomes stale and the user must re-approve before `/ralph` can auto-consume it.
- Approval is therefore bound to a specific artifact version, not only to the file path.

# Step prompt/context envelope
- Every workflow step receives the same base context envelope so the engine stays generic across planning and execution families.
- Minimum envelope fields:
  - `workflow_run_id`
  - `workflow_family`
  - `workflow_name`
  - `step_id`
  - `iteration`
  - `artifact_path` (when present)
  - `previous_step_output`
  - `parsed_verdict`
  - `blocking_findings` (`MUST_FIX` for planning, `FINDINGS` for execution)
  - `approval_metadata` (when relevant)
- Workflow-specific prompts can add more context, but this base envelope is always present.

# Workflow transition contract
- Workflow definitions stay file-driven, but the engine needs one explicit transition surface so step routing is not hardcoded per workflow.
- Each step can define:
  - `next`: the next step on normal completion
  - `on_verdict`: a small mapping from parsed verdict to next state
- Example:

```yaml
steps:
  - id: review_plan
    role: oracle
    gate_schema: planning_review
    on_verdict:
      PASS: pending_approval
      NEEDS_REVISION: revise_plan
      BLOCKED: blocked

  - id: revise_plan
    role: task
    next: review_plan
```

- This is intentionally smaller than a general workflow DSL; it is only enough to express bounded loops and terminal states.

# Review result schema

## Planning review result
The `/plan` review step must produce a structured result that the engine can parse:

```text
VERDICT: PASS | NEEDS_REVISION | BLOCKED
MUST_FIX:
- ...
SHOULD_FIX:
- ...
OPEN_QUESTIONS:
- owner: user | explorer
  question: ...
ASSUMPTIONS:
- ...
```

- Parsing rules for MVP:
  - `VERDICT:` must be the first non-empty line.
  - Verdict tokens are exact: `PASS`, `NEEDS_REVISION`, `BLOCKED`.
  - All sections must exist even when empty (use `- none`).
  - Parse failure is terminal and visible; the engine must not guess intent from prose.

Engine behavior:
- `PASS` → stop review loop; present plan for user approval.
- `NEEDS_REVISION` → run revise step using `MUST_FIX` as blocking input.
- `BLOCKED` → stop the workflow in a terminal blocked state and present the next required action (`ask user` or `rerun Explorer`) explicitly.
- Parse failure → stop with explicit workflow failure; do not guess.

## Execution review result
The `/ralph` review step can stay minimal for MVP:

```text
VERDICT: PASS | NEEDS_FIX
FINDINGS:
- ...
```

- Parsing rules for MVP:
  - `VERDICT:` must be the first non-empty line.
  - Verdict tokens are exact: `PASS`, `NEEDS_FIX`.
  - `FINDINGS:` must always exist (use `- none` when empty).
  - Parse failure is terminal and visible.

# MVP slices (ship-shaped, demoable)

## Slice 1: Workflow file format + shared engine runner
- **Goal**: Core workflow runner in `zdx-engine` that reads a workflow file and executes steps sequentially with family-aware gate logic.
- **Scope checklist**:
  - [ ] Define `WorkflowDefinition` struct: parsed from YAML frontmatter (`name`, `family`, `steps`, `max_iterations`, optional `artifact` metadata)
  - [ ] Define `WorkflowStep` struct: `id`, `role` (subagent name), `action` label, `gate_schema`, optional `next`, optional `on_verdict` map, optional `artifact_access`
  - [ ] Define `WorkflowEvent` enum for streaming progress: `StepStarted`, `StepCompleted`, `GateVerdict`, `IterationStarted`, `WorkflowCompleted`, `WorkflowFailed`, `WorkflowBlocked`
  - [ ] Implement `run_workflow` async function that walks steps, dispatches subagent calls via `invoke_subagent`, parses gates, and loops according to verdicts
  - [ ] Support two workflow families: `planning` and `execution`
  - [ ] Add structured gate parsers for planning and execution review outputs
  - [ ] Max iterations cap: stop after N iterations with a clear status message
  - [ ] No-progress detection: planning loop stops if the same blocking issues repeat after revision
  - [ ] Workflow file discovery with explicit precedence: project-local `.zdx/workflows/` > user `$ZDX_HOME/workflows/` > bundled fallback assets/materialized cache
  - [ ] Workflow file parser: read YAML frontmatter + split markdown body by `## <step_name>` headings for per-step prompts
  - [ ] Persist thread-scoped workflow metadata (`workflow_name`, `family`, `artifact_path`, `iteration`, `last_verdict`, `status`, approval metadata)
  - [ ] Engine allocates canonical artifact path before draft steps and handles slug collision policy centrally
  - [ ] Approval metadata stores artifact hash + approving workflow run ID, and invalidates when artifact content changes after approval
- **✅ Demo**: Load a workflow file, run it programmatically, see step events stream with structured verdicts and iteration counts.
- **Risks / failure modes**:
  - Structured gate parsing may fail if the subagent does not follow the expected format. Mitigation: clear prompt instructions + explicit parse-failure stop.
  - Workflow steps accidentally re-trigger themselves. Mitigation: engine tracks current step index and loop ownership.

## Slice 2: Built-in `/plan` workflow
- **Goal**: Ship the first planning workflow — draft plan → review → revise.
- **Scope checklist**:
  - [ ] Create `plan.md` workflow file in `zdx-assets` bundled workflows
  - [ ] Steps: `context` (role: explorer), `draft_plan` (role: task using ship-first-plan-style prompt), `review_plan` (role: oracle, gate: planning schema), `revise_plan` (role: task, triggered on `NEEDS_REVISION`)
  - [ ] Engine allocates `docs/plans/active/<slug>.md`; draft step writes to that engine-provided path
  - [ ] Review step must return `PASS`, `NEEDS_REVISION`, or `BLOCKED` with `MUST_FIX`, `SHOULD_FIX`, `OPEN_QUESTIONS`, `ASSUMPTIONS`
  - [ ] Revise step updates the same plan file atomically using only blocking review input plus any explorer/user follow-ups
  - [ ] Default max_iterations: 3
  - [ ] Stop early on `PASS`, `BLOCKED`, parse failure, or no-progress detection
  - [ ] Present final plan and require explicit user approval before any execution workflow consumes it
  - [ ] Record approval in thread-scoped metadata so `/ralph` can find the approved artifact deterministically
- **✅ Demo**: Run `/plan` → context gathered → draft saved → Oracle review → revised plan saved → PASS or BLOCKED shown in transcript.
- **Risks / failure modes**:
  - Oracle may keep finding non-blocking nits and cause noisy loops. Mitigation: only `MUST_FIX` drives iteration.
  - User-owned open questions may remain unresolved. Mitigation: `BLOCKED` stops and asks the user instead of guessing.

## Slice 3: Built-in `/ralph` workflow
- **Goal**: Ship the first execution workflow — ralph (implement → review → fix → iterate).
- **Scope checklist**:
  - [ ] Create `ralph.md` workflow file in `zdx-assets` bundled workflows
  - [ ] Steps: `implement` (role: task), `review` (role: oracle, gate: `PASS/NEEDS_FIX`), `fix` (role: task, triggered on `NEEDS_FIX`)
  - [ ] `/ralph` accepts either an approved plan path or explicit current-thread task context
  - [ ] Review prompt: check for bugs, missed edge cases, regressions, and high-value convention issues. Must return clear `PASS` or `NEEDS_FIX` verdict with findings.
  - [ ] Fix prompt: receives review findings, implements fixes, then loops back to review.
  - [ ] Default max_iterations: 5
  - [ ] Bundle in `zdx-assets` and materialize built-ins into a bundled workflow cache (for example `$ZDX_HOME/bundled-workflows/`), leaving `$ZDX_HOME/workflows/` as the user override directory
  - [ ] Consume workflow files via generic discovery/materialized bundled fallback, never by blindly copying built-ins into the user override directory
- **✅ Demo**: Run `/ralph` with an approved plan → see implement step → automatic review → fix if needed → PASS verdict → done.
- **Risks / failure modes**:
  - Review may always say `NEEDS_FIX` for trivial issues. Mitigation: keep review focused on real bugs/regressions and high-value issues.
  - Fix step may introduce new issues. Mitigation: bounded iterations prevent infinite loops.

## Slice 4: Slash command activation
- **Goal**: User can type `/plan` or `/ralph` in TUI or Telegram to start workflows.
- **Scope checklist**:
  - [ ] Add `/plan` slash command that starts the planning workflow in the current thread
  - [ ] Add an approval action/command (for example `/approve-plan`) that marks the current plan artifact approved in thread-scoped state
  - [ ] Add `/ralph` slash command that starts the execution workflow in the current thread
  - [ ] Show workflow status in transcript: step labels, verdicts, iteration count, blocked state
  - [ ] Support `/cancel` or interrupt to stop a running workflow cleanly
  - [ ] Pass current thread context and artifact path as input to the first step when relevant
  - [ ] Wire slash commands in both TUI and bot surfaces
- **✅ Demo**: Type `/plan` in TUI → see planning loop run; approve the result; type `/ralph` → see implement/review/fix loop run.
- **Risks / failure modes**:
  - Workflow blocks the conversation until complete. Mitigation: steps stream output progressively; interrupt works at any point.
  - Bot surface may have different message constraints. Mitigation: keep workflow status messages compact.

# Contracts (guardrails)
- Workflows must compose named subagent roles — they must not hardcode raw model choices into the engine.
- Normal non-workflow chat/task behavior must continue unchanged.
- Every workflow must have explicit max iterations and bounded stop conditions.
- Workflow steps must appear in the same visible transcript from the user's perspective.
- Planning review uses structured review schema; execution review uses structured `PASS/NEEDS_FIX` minimum.
- `PASS` in planning means “review passed”, not “user approved”.
- `BLOCKED` in planning stops the loop instead of letting the system invent answers to user-owned questions.
- The canonical plan artifact lives at `docs/plans/active/<slug>.md` and remains stable across one planning workflow run.
- The engine owns canonical artifact-path allocation before draft begins.
- `/ralph` only auto-consumes artifacts explicitly recorded as approved in thread-scoped state.
- Approval is tied to artifact content hash + approval run metadata; edits after approval invalidate approval.
- If a workflow step fails or is interrupted, the most recent artifact/result remains visible.
- Workflow state is thread-scoped — different threads can run different workflows independently.

# Key decisions (decide early)
- Workflows are engine-driven loops, not prompt-driven loops — the engine reads the file and controls iteration.
- `deep-interview` remains a skill; `/plan` and `/ralph` are workflows.
- `ship-first-plan` becomes a drafting prompt/component inside `/plan`, not the sole enforcement mechanism.
- Oracle serves as the review agent via prompt shaping for MVP — no dedicated reviewer subagent required yet.
- Workflow files use YAML frontmatter + markdown, discovered with explicit precedence: project-local `.zdx/workflows/` > user `$ZDX_HOME/workflows/` > bundled fallback assets/materialized cache.
- Planning and execution workflows use different gate schemas.
- Planning default max iterations = 3; execution default max iterations = 5.
- The revise step updates the same canonical artifact path during a planning run.
- `BLOCKED` is terminal in MVP; automatic Explorer reruns after a blocked verdict are deferred.
- Slash commands are the activation surface — no automatic post-turn triggering.

# Testing
- Manual smoke demos per slice (run real workflows, verify loop behavior and artifact updates)
- `just ci` must pass (no regressions)
- Integration test: workflow file parsing + step ordering
- Integration test: planning gate parsing (`PASS`, `NEEDS_REVISION`, `BLOCKED`, parse failure)
- Integration test: execution gate parsing (`PASS` stops, `NEEDS_FIX` iterates, max cap works)
- Integration test: plan path stays stable across revisions in one `/plan` run
- Integration test: `/ralph` receives approved plan context and consumes it correctly
- Integration test: approval metadata is required before `/ralph` auto-consumes a plan
- Integration test: approval becomes stale when the approved artifact content changes

# Polish phases (after MVP)

## Phase 1: Multi-model review passes
- Support different models per workflow step (e.g., Oracle/GPT for logic review, Gemini for conventions).
- Add optional `model` field to workflow step definition.
- ✅ Check-in demo: `/plan` reviews with one model, `/ralph` reviews with another.

## Phase 2: Workflow visibility + control improvements
- Clearer workflow status in transcript (progress bar, current step indicator).
- Workflow history: see past workflow runs for a thread.
- ✅ Check-in demo: see workflow progress and history in TUI.

## Phase 3: User-defined workflows
- Users create custom workflow files in `$ZDX_HOME/workflows/` or `.zdx/workflows/`.
- Validate workflow definitions with clear errors when referenced roles are missing.
- ✅ Check-in demo: user creates a custom workflow, activates it via slash command.

## Phase 4: Adaptive review scope
- Review step adapts based on what changed (more files → more thorough review).
- Multiple review passes with different prompts (bug hunt, conventions, security).
- ✅ Check-in demo: large plan or large change triggers multi-pass review automatically.

## Phase 5: Team-style orchestration
- Add multi-worker workflows, isolated branches/worktrees, and resolver-style outcomes such as pick/merge/compare.
- Keep this as a separate higher-cost tier on top of the simpler workflow system.
- ✅ Check-in demo: a team workflow runs two implementation branches and returns a resolved outcome.

# Later / Deferred
- Full oh-my-codex-style tmux/team operating system — revisit only if multi-worker coordination becomes a daily need.
- Cook-style composition operators (`vN` race, `vs` split, `pick`/`merge` resolvers) — revisit when parallel execution becomes a real need.
- Always-on automatic review/judge model for every turn — revisit only after explicit workflows prove the right triggers.
- Workflow graph editor or visual builder — revisit only if file-based definitions clearly fail.
- Cross-thread workflow coordination — revisit only if multi-thread workflows become a need.