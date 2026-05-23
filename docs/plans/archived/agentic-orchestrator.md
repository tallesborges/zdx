# Goals
- Add an agentic orchestrator layer that can decide strategy, branch when useful, and compose multiple subagent/workflow runs.
- Keep the existing workflow engine as the deterministic execution substrate rather than replacing it.
- Support a higher-cost, higher-intelligence mode for complex tasks where orchestration itself should be part of the intelligence.
- Make orchestration moves explicit, typed, and inspectable in the transcript.
- Preserve a boring, reliable path (`/plan`, `/ralph`) for daily-driver use while introducing a separate team-style mode for harder problems.

# Non-goals
- Replacing the existing workflow engine with a fully model-driven runtime.
- Making all normal chat/task turns go through an orchestrator.
- Hiding orchestration decisions or branch outcomes from the user.
- Unbounded autonomous branching with no budget, stop conditions, or transcript visibility.
- Building a visual workflow/orchestration editor in this slice.

# Scope + relationship to current workflow plan
- This plan is a follow-up to `docs/plans/active/ralph-workflow-engine.md`.
- The workflow engine remains the source of truth for deterministic multi-step loops.
- The orchestrator is a new higher-level layer that can choose between direct subagent calls, deterministic workflows, and later branch/compare flows.
- The orchestrator should be introduced as a separate user-visible mode/command, not by changing `/plan` and `/ralph` into implicit meta-agents.

# Design principles
- Strategy is intelligence; execution is infrastructure.
- Keep deterministic primitives underneath model-driven orchestration.
- User journey drives order.
- Preserve a boring path and an agentic path side-by-side.
- Typed orchestration actions beat free-form hidden behaviors.
- Read-only/advisory first; mutating execution comes later behind explicit gates.
- Branch only when the expected value is clearly worth the extra cost.
- Transcript visibility is mandatory; users should be able to see what the orchestrator decided and why.

# User journey
1. User has a complex task where one linear pass may not be enough (for example multiple implementation strategies, conflicting tradeoffs, unclear root cause).
2. User explicitly activates an orchestrator mode (for example `/team` or `/orchestrate`) instead of the simpler deterministic workflow path.
3. The orchestrator gathers initial context and decides a strategy: direct execution, deterministic workflow, or branch/compare.
4. The orchestrator runs typed actions: delegate discovery, run a workflow, ask Oracle, branch implementations, compare results, request user input, or stop.
5. The user sees orchestration decisions, branch labels, review outcomes, and final recommendation in the same transcript.
6. For planning, the orchestrator can still hand off to the existing approval gate before execution.

# Foundations / Already shipped (✅)

## Shared workflow engine
- What exists: deterministic workflow runtime in `crates/zdx-engine/src/workflows.rs` with file-based definitions, verdict parsing, bounded iteration, artifact-path allocation, and `WorkflowEvent` streaming.
- ✅ Demo: `cargo test -p zdx-engine workflows`
- Gaps: no agentic strategy selection yet; workflows are still static `next` / `on_verdict` graphs.

## Named subagents
- What exists: `task`, `explorer`, `oracle`, and `thread-searcher` roles with isolated child runs via `invoke_subagent` / `core::subagent::run_exec_subagent_with_cancel`.
- ✅ Demo: existing delegated runs in normal agent/tool flow.
- Gaps: no dedicated orchestrator prompt/profile yet.

## Thread persistence + transcript visibility
- What exists: append-only thread transcript with message/tool/notice persistence and visible tool/subagent activity in TUI/bot transcripts.
- ✅ Demo: current turns and tool activity replay correctly from thread history.
- Gaps: no first-class persisted orchestration state or branch outcome model.

## Slash-command surfaces
- What exists: TUI command palette and Telegram command parsing/dispatch infrastructure.
- ✅ Demo: `/new`, `/status`, `/worktree`, `/model`, `/thinking` already route through both surfaces.
- Gaps: no orchestrator command yet.

# Architecture stance
- The orchestrator should live in `zdx-engine` so both TUI and bot surfaces can reuse it.
- The orchestrator is not a replacement for `run_workflow`; it is a caller of deterministic primitives.
- The orchestrator should decide among typed actions, not free-form shell-like meta-behavior.
- The orchestrator's output should be advisory/strategic until it commits to a specific action primitive.

# Proposed architecture

## Layer 1: Deterministic primitives
- Direct subagent execution (`task`, `explorer`, `oracle`, `thread-searcher`)
- Deterministic workflow execution (`run_workflow`)
- Artifact files / worktree / thread transcript
- Approval helpers (`hash_artifact`, `approval_is_current`)

## Layer 2: Typed orchestration actions
The orchestrator chooses from a small typed action surface instead of inventing arbitrary next moves.

Candidate actions:
- `run_workflow { name, input, options }`
- `run_subagent { role, prompt }`
- `ask_user { question, choices? }`
- `branch { branches: [...] }`
- `compare { candidates: [...] }`
- `pick { candidate_id }`
- `merge { candidates: [...] }` (deferred; likely needs worktrees/artifact merge semantics)
- `approve_candidate { artifact_path }` (surface-gated; user confirmation still required)
- `stop { reason }`

### Strict action contract
The orchestrator must return a strict structured action output, not loose prose.

Initial MVP wire format:

```text
ACTION: RUN_WORKFLOW | RUN_SUBAGENT | ASK_USER | STOP
NAME: <workflow name>                 # required for RUN_WORKFLOW
ROLE: <subagent role>                # required for RUN_SUBAGENT
QUESTION: <user-facing question>     # required for ASK_USER
PROMPT:
- ...                                # required for RUN_SUBAGENT
INPUT:
- ...                                # required for RUN_WORKFLOW
CHOICES:
- ...                                # optional for ASK_USER
REASON:
- ...                                # required for STOP, optional otherwise
```

- `ACTION:` must be the first non-empty line.
- Parse failure is terminal and visible in MVP; do not guess.
- Unknown action names or missing required fields are terminal validation failures.
- A later slice may add one explicit retry on parse failure, but MVP should fail closed first.

### MVP action safety
- MVP orchestrator runs are advisory/read-only only.
- Allowed actions in MVP: `run_subagent` only with `explorer`, `oracle`, or `thread-searcher`; `run_workflow` only for planning/read-only workflows; `ask_user`; `stop`.
- Disallowed in MVP: mutating `task` subagent runs, execution workflows, worktree creation/removal, approval mutation, merge/pick of code branches.
- Mutating execution can be added later only after explicit approval and persisted orchestration/workflow state exist.

## Layer 3: Agentic orchestrator
- A new orchestrator prompt/profile reasons about which typed action to emit next.
- The runtime validates and executes the chosen action.
- The orchestrator then receives the structured result and decides the next move.
- This keeps strategy model-driven while preserving code-owned safety, stop rules, and transcript structure.

### Pause / resume model
- `ask_user` is not an inline action like `run_subagent`; it is a suspend point.
- When the orchestrator emits `ask_user`, the runtime stops the current loop in a `waiting_for_user` state and returns control to the surface.
- The user's answer resumes the same `orchestrator_run_id` with the preserved orchestration state and the answer injected as structured input.
- This is separate from workflow `AwaitingApproval`; the orchestrator needs a generic suspend/resume primitive.

### Minimal orchestration state
The first runtime slice should persist at least:
- `orchestrator_run_id`
- `status` (`running`, `waiting_for_user`, `completed`, `failed`, `cancelled`)
- `current_action`
- `action_index`
- `max_actions`
- `max_branches`
- `last_action_summary`
- `branch_ids` (when branching exists)

This state can start as engine-owned/caller-persisted metadata, but it must exist before `ask_user`, cancellation, or branching are real.

# MVP slices (ship-shaped, demoable)

## Slice 1: Minimal end-to-end read-only orchestrator (TUI first)
- **Goal**: Ship one vertical slice: a read-only orchestrator that can choose a strategy, run read-only actions, suspend for user input, and stop with a structured outcome.
- **Scope checklist**:
  - [ ] Add `crates/zdx-assets/subagents/orchestrator.md`
  - [ ] Define `OrchestratorAction` enum with the initial typed surface: `run_workflow`, `run_subagent`, `ask_user`, `stop`
  - [ ] Define the strict action wire format and parser
  - [ ] Define `OrchestratorEvent` enum for transcript/status streaming
  - [ ] Implement `run_orchestrator(...)` in `zdx-engine`
  - [ ] Feed structured action results back into the orchestrator prompt loop
  - [ ] Add bounded max turns / max actions / timeout protection
  - [ ] Add `waiting_for_user` suspend/resume handling for `ask_user`
  - [ ] Restrict MVP to read-only roles/actions only
  - [ ] Add a TUI-only surface command first (do not require bot parity yet)
  - [ ] Stop on parse failure rather than guessing
- **✅ Demo**: In TUI, user runs `/team plan websocket retry policy`; transcript shows `ActionSelected(run_subagent explorer)` → `ActionSelected(run_workflow plan)` or `ActionSelected(ask_user)` → `Stop`, with no mutating execution.
- **Risks / failure modes**:
  - The orchestrator may emit vague or invalid actions. Mitigation: strict parser + explicit retry/fail policy.
  - The loop may overthink simple tasks. Mitigation: action budget and a strong instruction to prefer the simplest viable path.

## Slice 2: Persisted state + bot parity
- **Goal**: Make the orchestrator resumable and visible across surfaces.
- **Scope checklist**:
  - [ ] Persist minimal orchestration state into thread metadata/events
  - [ ] Add replay/reload support for `waiting_for_user`
  - [ ] Add the matching Telegram bot command after TUI flow is proven
  - [ ] Define exactly which `OrchestratorEvent`s are rendered in transcript vs summarized
  - [ ] Ensure cancellation and restart semantics are explicit
- **✅ Demo**: User starts `/team`, gets an `ask_user` prompt, replies later, and the same orchestrator run resumes correctly.
- **Risks / failure modes**:
  - State replay may drift from transcript state. Mitigation: keep orchestration state minimal and explicit.

## Slice 3: Sequential strategy improvements
- **Goal**: Make the read-only orchestrator genuinely useful before branching.
- **Scope checklist**:
  - [ ] Bias the orchestrator toward known deterministic workflows when they fit
  - [ ] Improve action/result summaries shown in transcript
  - [ ] Add budget fields to runtime and prompt contract (`max_actions`, `max_branches`, max child runtime)
  - [ ] Add richer stop reasons and recommendation summaries
- **✅ Demo**: The orchestrator consistently prefers the simplest viable path (`explorer` only, `plan` workflow, or `ask_user`) rather than inventing unnecessary moves.
- **Risks / failure modes**:
  - The orchestrator may still overfit to one strategy. Mitigation: add explicit strategy-selection examples and validation tests.

## Slice 4: Branch + compare (planning/read-only first)
- **Goal**: Add limited branching for high-value cases without full worktree orchestration yet.
- **Scope checklist**:
  - [ ] Add `branch` and `compare` actions
  - [ ] Support parallel or sequential fan-out of isolated read-only/planning branches only
  - [ ] Let the orchestrator ask Oracle to compare branch outputs
  - [ ] Return a picked candidate or a user-facing comparison summary
  - [ ] Persist branch labels/results in transcript events
- **✅ Demo**: Two alternative plans are drafted and Oracle recommends one with reasons.
- **Risks / failure modes**:
  - Cost can climb quickly. Mitigation: branch count cap (e.g. max 2) and explicit budget instructions.
  - Comparison may be noisy. Mitigation: use a structured compare output format.

## Slice 5: Worktree-backed execution branches
- **Goal**: Support true multi-implementation branching for execution tasks as a separate large slice after read-only orchestration is proven.
- **Scope checklist**:
  - [ ] Reuse existing worktree helpers in `zdx-engine/src/core/worktree.rs`
  - [ ] Run implementation branches in isolated worktrees
  - [ ] Add compare/pick flow for code changes
  - [ ] Decide whether merge is in-scope or whether MVP is pick-only
  - [ ] Surface branch roots and selected outcome clearly in transcript
- **✅ Demo**: Two implementation branches run in separate worktrees; reviewer/orchestrator picks one.
- **Risks / failure modes**:
  - Merge semantics are hard. Mitigation: start with pick-only, not merge.
  - Cleanup and interruption become trickier. Mitigation: explicit lifecycle ownership and bounded branch count.

# Contracts (guardrails)
- The orchestrator must use typed actions; it must not rely on hidden, unstructured control flow.
- Deterministic workflows remain available and unchanged for users who want the boring path.
- Every orchestrator run must have explicit action/turn budgets and terminal stop conditions.
- Transcript visibility is mandatory: strategy decisions, branch labels, and final picks must be user-visible.
- MVP orchestrator is read-only/advisory only; it must not launch mutating `task` runs or execution workflows.
- User approval remains required before execution workflows auto-consume plans.
- Branching is opt-in or strategy-justified; it must not become the default for simple tasks.
- Model-driven strategy must not bypass code-owned validation for workflow definitions, verdict parsing, or approval checks.
- `ask_user` must suspend/resume a specific `orchestrator_run_id`; it cannot be handled as a generic inline step.

# Key decisions (decide early)
- Orchestrator is a separate mode, not a replacement for `/plan` and `/ralph`.
- Orchestrator chooses typed actions; runtime executes them.
- Start with TUI-only, read-only sequential strategy selection and only then add branching/bot parity.
- Branch planning before branch execution.
- Worktree-backed implementation branches start as pick-only, not merge.
- Keep the first orchestrator profile narrow and strategy-focused.
- Canonical first command name: `/team` (keep `/orchestrate` as a possible later alias if needed).

# Testing
- Manual smoke demos per slice
- Unit tests for orchestrator action parsing and validation
- Integration test: orchestrator chooses `run_workflow(plan)` when the request is a planning request
- Integration test: orchestrator asks the user instead of guessing when it emits `ask_user`
- Integration test: `ask_user` persists `waiting_for_user` state and resumes the same `orchestrator_run_id`
- Integration test: invalid orchestrator action output fails visibly
- Integration test: disallowed MVP actions/roles (`task`, execution workflows, merge) are rejected visibly
- Integration test: branch count and action budget caps work
- Later: worktree-backed branch lifecycle tests

# Polish phases (after MVP)

## Phase 1: Better comparison semantics
- Structured compare outputs with criteria, winner, caveats
- Optional multi-pass judge prompts (bugs, conventions, architecture)
- ✅ Check-in demo: compare two candidate plans with a structured winner summary

## Phase 2: Persistent orchestration history
- Persist orchestration state and branch summaries into thread metadata/events
- Let users revisit prior orchestrator runs in a thread
- ✅ Check-in demo: reopen a thread and inspect prior orchestration decisions

## Phase 3: User-defined orchestrator strategies
- Users can define custom orchestrator prompts and action preferences
- Add validation for referenced workflows/subagents
- ✅ Check-in demo: a project-specific orchestrator profile prefers local workflows first

# Later / Deferred
- Free-form graph editor for orchestration flows — revisit only if typed actions prove too rigid
- Automatic orchestrator selection for normal turns — revisit only after explicit orchestrator mode proves useful and trustworthy
- Unbounded multi-worker team operating system — revisit only when branch/compare is a daily need
- Full merge resolvers for conflicting implementation branches — revisit only after pick-only worktree branching proves useful