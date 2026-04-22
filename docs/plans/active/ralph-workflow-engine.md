# Goals
- Ship a workflow engine in `zdx-engine` that reads structured workflow files and drives multi-step agent loops.
- Ship a built-in `ralph` workflow: implement → review → fix → iterate until PASS or max iterations.
- Activate workflows via slash commands (`/ralph`) in TUI and Telegram.
- Workflow files use YAML frontmatter (steps, roles, gates, max_iterations) + markdown prompts — easy to edit without recompiling.

# Non-goals
- A heavy workflow DSL or graph editor.
- Orchestrator-as-agent (a dedicated agent that coordinates other agents).
- tmux-based team management or parallel worker processes.
- Automatic workflow activation (workflows are always explicitly triggered by the user).
- Plan or deep-interview as workflows (these remain skills — conversational, not engine-driven loops).
- Replacing normal direct chat/task flow for users who don't want workflows.

# Design principles
- User journey drives order
- Workflows are composition, not intelligence — the engine drives the loop, subagents do the thinking.
- Explicit activation only — user starts a workflow intentionally; steps inside run automated.
- Simple subagents with prompt-shaped behavior (Oracle reviews via prompt, not a dedicated reviewer agent).
- Gate-based iteration with bounded max iterations for safety.
- Workflow files follow the same pattern as skills: bundled built-ins that users can modify.

# User journey
1. User approves a plan (created via `ship-first-plan` skill).
2. User activates `/ralph` in TUI or Telegram.
3. Engine reads the ralph workflow file, starts the work step.
4. Work step completes — engine automatically dispatches the review step.
5. Review step returns a gate verdict: `PASS` or `NEEDS_FIX` with findings.
6. If `NEEDS_FIX`, engine dispatches a fix step with the review findings, then reviews again.
7. Loop continues until `PASS` or max iterations reached.
8. User sees structured results in the same transcript with clear step labels and gate status.

# Foundations / Already shipped (✅)

## Named subagents
- What exists: `invoke_subagent` with built-in `task`, `oracle`, `designer`, `explorer` roles. Isolated child runs with prompt-driven behavior.
- ✅ Demo: `invoke_subagent(subagent: "oracle", prompt: "Review these changes")`
- Gaps: None for MVP — oracle can serve as the review agent via prompt.

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
- What exists: `ship-first-plan` skill for structured planning with Explorer context gathering, Oracle review, and plan persistence to `docs/plans/active/`. `deep-interview` skill for Socratic requirements clarification (just created — not yet battle-tested).
- ✅ Demo: Ask for a plan → Explorer gathers context → structured plan with slices generated → saved to file.
- Gaps: No handoff from approved plan to implementation workflow yet. Deep-interview skill is new and untested.

# MVP slices (ship-shaped, demoable)

## Slice 1: Workflow file format + engine runner
- **Goal**: Core workflow runner in `zdx-engine` that reads a workflow file and executes steps sequentially with gate logic.
- **Scope checklist**:
  - [ ] Define `WorkflowDefinition` struct: parsed from YAML frontmatter (name, steps with role/action/gate/on_fail, max_iterations)
  - [ ] Define `WorkflowStep` struct: role (subagent name), action label, gate verdict pattern (optional), on_fail behavior (iterate/stop)
  - [ ] Define `WorkflowEvent` enum for streaming progress: `StepStarted`, `StepCompleted`, `GateVerdict`, `IterationStarted`, `WorkflowCompleted`, `WorkflowFailed`
  - [ ] Implement `run_workflow` async function that walks steps, dispatches subagent calls via `invoke_subagent`, parses gate verdicts, and loops on `NEEDS_FIX`
  - [ ] Gate parsing: scan subagent response for `PASS` or `NEEDS_FIX` verdict (simple keyword match)
  - [ ] Max iterations cap: stop after N iterations with a clear "max iterations reached" status
  - [ ] Workflow file discovery: `$ZDX_HOME/workflows/*.md` with same precedence as skills (project-level overrides user-level overrides bundled)
  - [ ] Workflow file parser: read YAML frontmatter + split markdown body by `## <step_name>` headings for per-step prompts
- **✅ Demo**: Load a workflow file, run it programmatically, see step events stream with gate verdicts and iteration counts.
- **Risks / failure modes**:
  - Gate verdict parsing may be fragile if the subagent doesn't follow the expected format. Mitigation: clear prompt instructions + fallback to NEEDS_FIX if verdict is ambiguous.
  - Workflow steps accidentally re-trigger themselves. Mitigation: engine tracks current step index, never re-enters.

## Slice 2: Built-in ralph workflow file
- **Goal**: Ship the first real workflow — ralph (implement → review → fix → iterate).
- **Scope checklist**:
  - [ ] Create `ralph.md` workflow file in `zdx-assets` bundled workflows
  - [ ] Steps: `implement` (role: task), `review` (role: oracle, gate: PASS/NEEDS_FIX), `fix` (role: task, triggered on NEEDS_FIX)
  - [ ] Review prompt: check for bugs, code conventions, missed edge cases, regressions. Must return clear `PASS` or `NEEDS_FIX` verdict with findings.
  - [ ] Fix prompt: receives review findings, implements fixes, then loops back to review.
  - [ ] Default max_iterations: 5
  - [ ] Bundle in `zdx-assets`, copy to `$ZDX_HOME/workflows/` on first run (same pattern as bundled skills)
- **✅ Demo**: Run ralph workflow → see implement step → automatic review → fix if needed → PASS verdict → done.
- **Risks / failure modes**:
  - Review may always say NEEDS_FIX for trivial issues, causing unnecessary iterations. Mitigation: review prompt should focus on real bugs/regressions, not style nits.
  - Fix step may introduce new issues. Mitigation: bounded iterations prevent infinite loops.

## Slice 3: Slash command activation
- **Goal**: User can type `/ralph` in TUI or Telegram to start the workflow.
- **Scope checklist**:
  - [ ] Add `/ralph` slash command that starts the ralph workflow in the current thread
  - [ ] Show workflow status in transcript: step labels, gate verdicts, iteration count
  - [ ] Support `/cancel` or interrupt to stop a running workflow cleanly
  - [ ] Pass current thread context (approved plan, recent messages) as input to the first step
  - [ ] Wire slash command in both TUI and bot surfaces
- **✅ Demo**: Type `/ralph` in TUI after approving a plan → see the implement/review/fix loop run with labeled output in the transcript.
- **Risks / failure modes**:
  - Workflow blocks the conversation until complete. Mitigation: steps stream output progressively; interrupt works at any point.
  - Bot surface may have different message constraints. Mitigation: keep workflow status messages compact.

# Contracts (guardrails)
- Workflows must compose named subagent roles — they must not hardcode raw model choices.
- Normal non-workflow chat/task behavior must continue unchanged.
- Every workflow must have explicit max iterations and bounded stop conditions.
- Workflow steps must appear in the same visible transcript from the user's perspective.
- Gate verdicts must be structured enough to drive follow-up logic (PASS/NEEDS_FIX minimum).
- If a workflow step fails or is interrupted, the original work result remains visible.
- Workflow state is thread-scoped — different threads can run different workflows independently.

# Key decisions (decide early)
- Workflows are engine-driven loops, not prompt-driven loops — the engine reads the file and controls iteration.
- Plan and deep-interview remain skills, not workflows — they are conversational, not iterative engine loops.
- Oracle serves as the review agent via prompt shaping — no dedicated reviewer subagent needed for MVP.
- Workflow files use YAML frontmatter + markdown, discovered from `$ZDX_HOME/workflows/` with bundled fallbacks.
- Gate parsing uses simple keyword matching (PASS/NEEDS_FIX) — no structured JSON output required from subagents.
- Slash commands are the activation surface — no automatic post-turn triggering.
- The fix step reuses the `task` subagent role with review findings injected into the prompt.

# Testing
- Manual smoke demos per slice (run real workflows, verify gate logic and iteration)
- `just ci` must pass (no regressions)
- Integration test: workflow file parsing + step ordering
- Integration test: gate verdict parsing (PASS stops, NEEDS_FIX iterates, max cap works)

# Polish phases (after MVP)

## Phase 1: Multi-model review passes
- Support different models per workflow step (e.g., Oracle/GPT for logic review, Gemini for conventions).
- Add optional `model` field to workflow step definition.
- ✅ Check-in demo: ralph workflow runs review with Gemini, fix with default model.

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
- ✅ Check-in demo: large change triggers multi-pass review automatically.

## Phase 5: Plan-to-ralph handoff
- After `ship-first-plan` saves a plan and user approves, offer direct `/ralph` activation.
- Ralph receives the approved plan as structured context for its first step.
- ✅ Check-in demo: approve plan → start ralph → first step references the plan directly.

# Later / Deferred
- Team-style orchestration with parallel workers — revisit when single-worker ralph proves daily value.
- Cook-style composition operators (vN race, vs split, pick/merge resolvers) — revisit when parallel execution becomes a real need.
- Always-on automatic review for every turn — revisit only after explicit workflows prove the right triggers.
- Workflow graph editor or visual builder — revisit only if file-based definitions clearly fail.
- Cross-thread workflow coordination — revisit only if multi-thread workflows become a need.
