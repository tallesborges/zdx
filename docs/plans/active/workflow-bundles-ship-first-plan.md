# Goals
- Add first-class workflow bundles that chain existing agent/subagent roles across a task.
- Ship a daily-usable post-turn review flow so implementation work can automatically trigger a reviewer pass.
- Add a dedicated `reviewer` subagent role, separate from `oracle`, with Gemini Pro as the default review model.
- Support explicit gates and bounded iteration so review can drive follow-up action instead of remaining a loose suggestion.
- Leave room for larger bundles such as Ralph-style persistence loops and team orchestration without forcing that complexity into MVP.

# Non-goals
- Recreating the full oh-my-codex operating system in MVP.
- Hidden always-on auto-review for every turn before explicit workflows prove useful.
- tmux-driven team management, shared agent inboxes, or a large workflow dashboard.
- A heavy workflow DSL or graph editor in the first version.
- Replacing the normal direct chat/task flow for users who do not want workflows.

# Design principles
- User journey drives order
- Roles and workflows stay separate: subagents define behavior; workflows compose them.
- Explicit activation first, adaptive routing later.
- Ship one complete daily-use loop before broader orchestration.
- Bound every loop with a clear stop condition and visible status.

# User journey
1. User activates a workflow for the current thread in the TUI (starting with a built-in review workflow).
2. ZDX runs the normal work step.
3. When that step finishes, ZDX automatically dispatches the next workflow step (for example, `reviewer`).
4. User sees a structured result in the same transcript, including gate status and concrete findings.
5. If the workflow includes bounded follow-up steps, ZDX continues until it reaches a stop condition or max iterations.
6. Later, the user can choose other built-in workflows or create custom bundles.

# Foundations / Already shipped (✅)
List capabilities that already exist and should not be rebuilt.

## Named subagents
- What exists: `invoke_subagent`, the default `task` alias, and specialized built-ins such as `oracle` and `explorer`.
- ✅ Demo: `invoke_subagent(subagent: "oracle", prompt: "...")`
- Gaps: no dedicated `reviewer` role yet.

## Isolated subagent execution
- What exists: child runs are already isolated, prompt-driven, and suitable for scoped review/debug/research work.
- ✅ Demo: a parent agent can delegate a one-shot review or investigation and continue in the same conversation.
- Gaps: no first-class workflow runner that chains steps automatically after a turn.

## Thread persistence + transcript surfaces
- What exists: threads persist messages and tool activity, and both TUI/bot flows already surface multi-step work in the transcript.
- ✅ Demo: tool calls and subagent results remain visible in the thread history.
- Gaps: no thread-scoped workflow phase/iteration state yet.

## `todo_write`
- What exists: per-thread task tracking with flat statuses and one active `in_progress` item.
- ✅ Demo: complex tasks can already expose progress while the agent works.
- Gaps: no workflow-owned phase model or gate/result normalization.

# Architecture placement
- The workflow runner lives in `zdx-engine` so all surfaces can reuse one engine-owned implementation.
- The first activation surface is TUI only; bot/exec/automation activation is deferred until the core loop proves useful.
- MVP workflow visibility comes from explicit transcript/system status messages, not from hidden engine state alone.

# Persistence + transcript model
- Persist a minimal thread-scoped workflow state: `workflow_name`, `status`, `current_step`, `iteration`, `last_verdict`.
- Emit explicit transcript-visible status messages when a workflow starts, advances steps, completes, fails, or is interrupted.
- Keep workflow state separate from child subagent state so review/debug child runs do not silently inherit or mutate the parent workflow loop.

# MVP slices (ship-shaped, demoable)
Define Slice 1..N in user-journey order.

## Slice 1: Built-in `reviewer` role + review gate contract
- **Goal**: Add a dedicated review specialist so review behavior is a reusable role instead of an ad-hoc prompt.
- **Scope checklist**:
  - [ ] Add a built-in `reviewer` subagent distinct from `oracle`.
  - [ ] Default `reviewer` to Gemini Pro and keep the role configurable through normal subagent override paths.
  - [ ] Write the prompt for code review, regressions, missed edge cases, and project conventions.
  - [ ] Define a compact structured result contract for review output (for example `PASS` / `NEEDS_FIX`, severity, findings).
  - [ ] Update capability catalogs/tool descriptions so the parent agent knows when to use `reviewer` vs `oracle`.
- **✅ Demo**: `invoke_subagent(subagent: "reviewer", prompt: "Review the just-completed work")` returns a structured review verdict.
- **Risks / failure modes**:
  - Review output becomes too verbose or too chatty to gate follow-up work reliably.
  - Gemini model/auth availability may vary across environments.

## Slice 2: Core workflow runner + built-in review workflow
- **Goal**: Ship one complete workflow that automatically does `work -> reviewer` after a task completes.
- **Scope checklist**:
  - [ ] Add an engine-owned workflow runner in `zdx-engine` with ordered steps that reference roles rather than raw models.
  - [ ] Ship one built-in workflow bundle for post-turn review.
  - [ ] Add one explicit activation path for that workflow in the TUI.
  - [ ] Persist active workflow state in thread-scoped state using the minimal workflow model.
  - [ ] Automatically dispatch the reviewer step only after the main work step completes, never after reviewer/oracle child completions.
  - [ ] Append workflow results to the same transcript/thread with explicit status messages instead of creating a detached side channel.
- **✅ Demo**: User enables the review workflow, asks for an implementation task, and sees the reviewer fire automatically after the first completion.
- **Risks / failure modes**:
  - Workflow steps accidentally re-trigger themselves.
  - Added latency/cost feels surprising if activation is not explicit enough.

## Slice 3: Interactive workflow visibility + control
- **Goal**: Make workflows understandable and manageable during normal usage.
- **Scope checklist**:
  - [ ] Show active workflow name, current step, and final gate status in transcript-visible output.
  - [ ] Make interrupted or failed workflow runs stop cleanly with a readable status.
  - [ ] Keep the original work result visible even if the reviewer fails or is interrupted.
  - [ ] Preserve enough workflow state that resuming the thread does not lose the active phase unexpectedly.
  - [ ] Support turning the workflow off for the current thread/run without affecting normal chat behavior.
- **✅ Demo**: Start a workflow-backed task, interrupt once, resume the thread, and see a clear workflow status instead of silent state loss.
- **Risks / failure modes**:
  - Workflow state drifts from transcript history.
  - Users cannot tell whether the assistant response is the work step or the review step.

# Contracts (guardrails)
List non-negotiable behaviors that must not regress (derived from Inputs and existing behavior).
- Workflows must compose named roles/subagents; they must not hardcode raw model choices into every call site.
- `oracle` remains focused on planning/debugging/architecture; `reviewer` becomes the default review role.
- Normal non-workflow chat/task behavior must continue to work unchanged.
- Workflow steps must run in the same visible thread/transcript context from the user's perspective.
- Every iterative workflow must have explicit stop conditions and bounded max iterations.
- Review verdicts must be structured enough to drive follow-up workflow logic.
- The first review workflow is status-only: `PASS` / `NEEDS_FIX` reports findings and stops; it does not auto-fix in MVP.
- `reviewer` stays read-only in MVP.

# Key decisions (decide early)
List only decisions that would cause rework if postponed (derived from Inputs).
- The canonical abstraction is `role + workflow`, not a single mega-mode.
- The first shipped role split is `task` / `oracle` / `reviewer`.
- The first workflow is explicit post-turn review, not hidden auto-routing.
- The first activation surface is TUI only, and activation is per-thread.
- Workflows reference role names, not direct model IDs.
- The first workflow triggers only after main work completion, not after `reviewer` or `oracle` child completions.
- Workflow state is thread-scoped so the user can understand and resume it.
- Child workflow steps do not inherit the active workflow state; this prevents review-on-review recursion.
- If a reviewer step fails or is interrupted, the original work result remains visible and the workflow ends with a failed/interrupted status.
- If user-defined workflows ship later, discovery should follow existing file-based precedence: `.zdx/workflows/*.md` overrides `$ZDX_HOME/workflows/*.md` overrides built-ins.

# Testing
- Manual smoke demos per slice
- Minimal regression tests only for contracts
- Runner tests for step ordering, bounded iteration, and interruption/recovery behavior
- Transcript/state tests for thread resume with active workflow metadata

# Polish phases (after MVP)
Group improvements into phases, each with a ✅ check-in demo.
Limited strictly to scope present in Inputs.

## Phase 1: User-defined workflow bundles
- Add file-backed workflow definitions for project-level and user-level custom bundles.
- Keep the format simple: ordered steps, optional max iterations, and role references.
- Validate workflow definitions with clear errors when referenced roles are missing.
- Expose discovered workflow names/descriptions in the user-facing activation path.
- ✅ Check-in demo: user creates a custom workflow bundle, activates it, and sees the same runner execute it.

## Phase 2: Better workflow UX
- Add a clearer workflow picker/toggle and more compact transcript summaries.
- Keep reviewer/oracle configurability flowing through existing subagent override paths rather than adding a separate model-slot system first.
- ✅ Check-in demo: user can switch workflow and reviewer preference without rewriting workflow definitions.

## Phase 3: Ralph-lite persistence loop
- Add a built-in bounded workflow that does `work -> reviewer -> fix -> reviewer` until pass or max iterations.
- Reuse the gate contract instead of inventing a separate loop protocol.
- ✅ Check-in demo: a task runs through multiple bounded review/fix passes and ends with a clear final verdict.

## Phase 4: Plan-first and adaptive gates
- Add optional gates that can route vague requests to `oracle` planning first or skip review when explicitly unnecessary.
- Keep routing visible and deterministic rather than hidden “judge model” magic.
- ✅ Check-in demo: an underspecified execution request is redirected into a plan-first workflow before implementation starts.

## Phase 5: Team-style orchestration
- Add multi-worker workflows, isolated branches/worktrees, and resolver-style outcomes such as pick/merge/compare.
- Keep this as a separate higher-cost tier on top of the simpler workflow system.
- ✅ Check-in demo: a team workflow runs two implementation branches and returns a resolved outcome.

# Later / Deferred
Explicit list of “not now” items + what would trigger revisiting them.
- Full oh-my-codex-style tmux/team operating system — revisit only if multi-worker coordination becomes a daily need.
- Always-on automatic review/judge model for every turn — revisit only after explicit workflows prove the right triggers.
- Unbounded recursive workflow graphs — revisit only if simple ordered bundles clearly fail in practice.
- Shared cross-agent task boards — revisit only if team workflows become first-class.