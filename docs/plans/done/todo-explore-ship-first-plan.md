# Goals
- Add a model-facing `todo_write` tool that helps the agent track multi-step work and gives the user visible progress.
- Add a built-in `explorer` subagent after `todo_write`, so code and repo research can benefit from the same task-discipline patterns.
- Keep both features usable in normal ZDX conversations without introducing plan mode.

# Non-goals
- Plan mode, approval workflows, or plan-file UX.
- Shared multi-agent task boards or team/task orchestration.
- Rich Telegram-specific task rendering in the MVP.
- Full oh-my-pi phase-heavy todo model in the first version.

# Design principles
- User journey drives order.
- Ship the behavior core first, not the full reference-project machinery.
- `todo_write` should improve both agent reliability and user visibility.
- `explorer` must stay read-mostly and focused on search/research workflows.
- Keep transcript-visible behavior as the default; add dedicated UI only after the core flow is useful.

# User journey
1. User asks for a non-trivial coding or research task.
2. The main agent creates and maintains a todo list for the work.
3. The main agent delegates search/research work to `explorer` when needed.
4. The user can follow progress from the transcript while the agent executes.

# Foundations / Already shipped (✅)
List capabilities that already exist and should not be rebuilt.

## Named subagents
- What exists: `invoke_subagent`, built-in `task` alias, standalone prompt-backed named subagents, built-in `oracle`.
- ✅ Demo: `invoke_subagent(subagent: "oracle", prompt: "...")`
- Gaps: no built-in `explorer` yet.

## Tool registry and schemas
- What exists: a stable `zdx-core/src/tools/` pattern for adding model-facing tools with structured schemas and envelopes.
- ✅ Demo: `read`, `grep`, `glob`, `invoke_subagent`.
- Gaps: no model-facing todo/task-tracking tool.

## Thread persistence and transcript surfaces
- What exists: threads persist, transcript history is already visible in TUI and bot flows, and tool outputs naturally show up in the conversation.
- ✅ Demo: existing tool calls appear in the transcript and are available in thread history.
- Gaps: no durable task-tracking state for the model to update across turns.

## TUI async task lifecycle
- What exists: internal `TaskKind`/`Tasks` runtime state for UI-side async work.
- ✅ Demo: thread loading, bash, handoff, and login tasks already expose running state.
- Gaps: this is runtime infrastructure, not a model-managed work list.

# MVP slices (ship-shaped, demoable)
Define Slice 1..N in user-journey order.

## Slice 1: Core `todo_write` tool
- **Goal**: Give the model one tool for creating and updating a visible task list during complex work.
- **Scope checklist**:
  - [ ] Add a new `todo_write` tool in `zdx-core`.
  - [ ] Start with a flat ordered task list, not phases.
  - [ ] Support a small mutation set: `replace`, `add`, `update`, `remove`.
  - [ ] Support statuses: `pending`, `in_progress`, `completed`, `abandoned`.
  - [ ] Normalize the list so exactly one task is `in_progress` when the list is non-empty.
  - [ ] Return the full current todo state in tool output so transcript history is self-contained.
- **✅ Demo**: The agent receives a 4-step task, creates a todo list, starts the first item, and updates it as work progresses.
- **Risks / failure modes**:
  - Over-designing the schema too early.
  - Weak invariants causing duplicate or missing active tasks.

## Slice 2: Todo continuity + usage guidance
- **Goal**: Make `todo_write` reliable across turns and teach the model to use it proactively.
- **Scope checklist**:
  - [ ] Recover the latest todo state from the current thread or equivalent thread-scoped state.
  - [ ] Add system-prompt guidance to use `todo_write` for 3+ step work, update immediately, and avoid batching completions.
  - [ ] Keep `todo_write` out of trivial or purely informational requests.
  - [ ] Ensure the updated todo list remains visible in normal transcript output.
- **✅ Demo**: Resume an existing thread and continue updating the same todo list instead of recreating it from scratch.
- **Risks / failure modes**:
  - State recovery drift between turns.
  - Prompt guidance too weak, so the model underuses the tool.

## Slice 3: Built-in `explorer` subagent
- **Goal**: Add a unified search/research subagent after `todo_write` is available, so the main agent can track both local and external investigation work explicitly.
- **Scope checklist**:
  - [ ] Add built-in `crates/zdx-core/subagents/explorer.md`.
  - [ ] Support current-workspace search, thread-history search, and external repo/library research in one prompt.
  - [ ] Allow controlled external workflows (`deepwiki`, shallow clone into `$ZDX_ARTIFACT_DIR`) without allowing general project mutation.
  - [ ] Bias the prompt toward parallel search, alternate strategies on empty results, and concise handoff output.
  - [ ] Add `explorer` to the curated capability catalog and `invoke_subagent` descriptions.
  - [ ] Decide whether `explorer` gets `todo_write` in MVP or whether the parent agent owns todo tracking around investigation.
- **✅ Demo**: The main agent creates a todo list, delegates investigation to `explorer`, and then continues the tracked work using the findings.
- **Risks / failure modes**:
  - If `explorer` gets `todo_write`, the child may create internal task state that is not visible to the parent.
  - If `explorer` tries to cover too many cases without a clear scope distinction, the model may use it inconsistently.

# Contracts (guardrails)
List non-negotiable behaviors that must not regress (derived from Inputs and existing behavior).
- `todo_write` is the single task-tracking tool in MVP.
- `todo_write` must keep task state coherent enough for same-thread reuse.
- `explorer` must not mutate the user's project.
- `explorer` must prefer search/read tools and use `bash` only for tightly-scoped research flows.
- Normal `task` and `oracle` subagent behavior must not regress.
- Transcript output must remain readable when todo updates occur frequently.

# Key decisions (decide early)
List only decisions that would cause rework if postponed (derived from Inputs).
- Start with flat todos, not phases.
- Ship `todo_write` before `explorer`.
- Keep `todo_read` deferred unless `todo_write` alone proves insufficient.
- Decide whether subagents, especially `explorer`, should be allowed to call `todo_write` in MVP or whether todo ownership stays with the parent agent.
- Keep the first UX transcript-first rather than adding a dedicated TUI task panel immediately.

# Testing
- Manual smoke demos per slice
- Minimal regression tests only for contracts
- `todo_write` tests for schema validation, status normalization, and same-thread recovery
- `explorer` tests for subagent discovery, tool allowlist, and prompt rendering

# Polish phases (after MVP)
Group improvements into phases, each with a ✅ check-in demo.
Limited strictly to scope present in Inputs.

## Phase 1: Better task visibility
- Add a more compact transcript rendering for todo updates.
- Add a dedicated TUI surface for the current todo state if transcript-only visibility feels noisy.
- ✅ Check-in demo: the active task is visible at a glance without reading every todo tool result.

## Phase 2: Richer todo structure
- Add optional phases or grouped work only if flat tasks prove too limiting.
- Improve notes/details handling for active tasks.
- ✅ Check-in demo: large multi-step work stays organized without losing the simple default path.

## Phase 3: Explorer and todo integration refinements
- If useful in practice, let `explorer` use `todo_write` in a constrained way or surface investigation subtasks back to the parent more explicitly.
- ✅ Check-in demo: exploration-heavy tasks stay organized without duplicating or hiding task state.

# Later / Deferred
Explicit list of “not now” items + what would trigger revisiting them.
- Plan mode — revisit only if approval-driven planning becomes a repeated workflow.
- Shared task lists across multiple subagents — revisit if orchestration becomes multi-agent by default.
- Telegram-specific live task UI — revisit if transcript output is not enough for bot use.
- oh-my-pi-style phase-first todo model — revisit only if flat tasks clearly fail in practice.