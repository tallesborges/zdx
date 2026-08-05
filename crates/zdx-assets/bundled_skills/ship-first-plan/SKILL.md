---
name: ship-first-plan
description: Create a ship-first (ship-shaped) implementation plan with demoable MVP phases. Use when a user asks for an implementation plan, MVP plan, shipping plan, or wants to break down a feature into incremental, demoable phases. Emphasizes user journey order and daily-usable increments over polished completeness.
metadata:
  short-description: Create a ship-shaped MVP plan
---

# Ship-First Plan

## Goal

Create the smallest grounded plan that reaches the first usable version of the requested capability through demoable, incremental phases.

## Hard constraints

- **Stop at usable**: plan through the first usable version of the capability, ordered by real dependencies. `Later` holds rollout, hardening, polish, and follow-on capabilities as one-liners with triggers. Correctness and security for the behavior you ship belong in the phase that ships it.
- **One behavior per phase**: each phase adds one capability or changes one observable behavior. Its checklist items build that behavior; its ✅ Demo exercises it and states the observed result. Multi-step setup is fine when it proves a single new outcome; two new outcomes are two phases.
- **Bounded exit each**: each phase is work you intend now and has a bounded exit. Conditional work lives in `Later` with the trigger that would start it.
- **Gates attach to their phase**: approvals, ownership questions, and research attach as a gate note on the phase they block; the phase's ✅ Demo stays an observation of system behavior.
- **Smallest implementation**: a phase's abstractions, config, schema, flags, and interfaces are paid for by the behavior it ships. Shared code moves to a shared home once a second consumer exists.
- **Reuse before rebuild**: before proposing a new module, config, schema, or abstraction, explore the codebase for existing functionality/patterns that already do it (or most of it). Extend what exists rather than standing a parallel implementation beside it.
- **Scope from Inputs**: build the capabilities present in the Inputs, using the illustrative examples the Inputs provide.

## Workflow

Operate read-only while researching and drafting — the only file you create is the saved plan itself (step 6).

**Delegation guard:** a delegated drafter gets these Hard constraints, the template below, and a brief carrying five things — the goal, the current state, the constraints, the success signal, and the code findings that change what you would build. That brief is complete on its own.

1. **Gather Inputs**
   - Project/feature: (1–3 sentences from user)
   - Existing state: (what already exists)
   - Constraints: (platforms, requirements, no-go's)
   - Success looks like: (what "usable" means — must be measurable or binary; see Goal-quality gate below)

   **Goal-quality gate (blocking):** before moving on, "Success looks like" must answer:
   - What first usable capability or behavior will be true when this is done?
   - What test exercises it, and what observable result proves it?
   - What is explicitly out of scope?

   If the success criterion is a pure activity description ("make X better", "improve Y", "work on Z") or has no observable validator, stop and sharpen it inline with one focused clarification question before proceeding.

   Do not proceed to phasing until the goal passes this gate. Phases inherit their `✅ Demo` rigor from this criterion — a vague goal produces vague demos.

2. **Deep context gathering with Explorer**
   - Delegate this to `explorer` subagents, and fan out in parallel: when the slice touches independent areas, launch one explorer per area in a single batch rather than reading serially.
   - Each explorer reports the current behavior, the existing implementation or patterns to reuse, and the likely touchpoints for its area.
   - Read repository instructions (`README.md`, `AGENTS.md`) and only the docs relevant to the slice.
   - Ground the plan in real code — verify patterns, file locations, and APIs rather than guessing them.

3. **Ask follow-ups only if blocking**
   - At most 1–2 questions; prefer multiple-choice.
   - If unsure but not blocked, make reasonable assumptions and proceed.
   - **Question classification**: before asking the user anything, check if it is a codebase fact (file locations, patterns, APIs) or a user preference (priority, scope, constraints). For codebase facts, use Explorer first — the user answers only what the code cannot.

4. **Create the plan using the template below**
   - Output only the plan — no meta explanations.
   - Cite a specific file or code location where it helps implementation or verification.

5. **Optional: Oracle review**
   - If the plan touches architecture, security, or multiple subsystems, delegate the plan to `oracle` for a review pass against these Hard constraints before presenting to the user.
   - Correct the draft in place using Oracle's findings rather than appending them as an addendum.

6. **Save the plan**
   - Plans live under `docs/plans/` with a simple stage-based lifecycle:
     - `drafts/` — exploratory, not yet green-lit (may be revised or dropped)
     - `active/` — committed, being built
     - `done/` — completed
     - `archived/` — abandoned or superseded
   - Save an exploratory / not-yet-approved plan to `docs/plans/drafts/<slug>.md`; save a committed plan to `docs/plans/active/<slug>.md`.
   - If a plan with that slug already exists in any stage, confirm with the user before overwriting.

## Plan template

Use this shape. Add a section when omitting it would make the scope or phases misleading, and name that section for the fact it carries.

```markdown
> Stage: drafts | active | done | archived. Keep this plan current while working: when a scope item is finished, check its box `[ ]`→`[x]`; when a phase's ✅ demo passes, mark the phase done (with date). The plan file is the source of truth, not memory.

# Goals
- (tight list of user-visible outcomes)

# Non-goals
- (deferred features, and the abstractions or infrastructure you are deliberately leaving unbuilt)

# User journey
1. (numbered one-liners from the user's or operator's point of view)
Phases unlock these steps in order. When the phase names already read as the journey, the phase list is the journey — omit this section.

# Phase 1 — <one capability or behavior>
- [ ] (work that builds this phase's behavior; phase-specific tests belong here)
- [ ] ...
✅ **Demo**: (exercise the capability or behavior and state the observable result)

# Phase 2 — ...
...

# Later
- (follow-on, rollout, hardening, and polish as one-liners, each naming the trigger that would start it)

# Open questions
- (unresolved questions; they stay open. Record a decision here only when deferring it would cause rework — one line, stating the choice.)
```

## Phase guidance

A good phase adds one capability or changes one observable behavior, is runnable and testable, and states an observable ✅ Demo result. "Ugly but functional" beats "polished but incomplete". A checklist item that no demo observes is scaffolding — fold it into the behavior it serves or move it to `Later`.
