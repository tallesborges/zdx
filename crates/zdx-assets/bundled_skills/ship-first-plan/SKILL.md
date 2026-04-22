---
name: ship-first-plan
description: Create a ship-first (ship-shaped) implementation plan with demoable MVP slices. Use when a user asks for an implementation plan, MVP plan, shipping plan, or wants to break down a feature into incremental, demoable slices. Emphasizes user journey order and daily-usable increments over polished completeness.
metadata:
  short-description: Create a ship-shaped MVP plan
---

# Ship-First Plan

## Goal

Create a **ship-first (ship-shaped) implementation plan** that unlocks the user journey as early as possible with demoable, incremental slices.

## Hard constraints

- **User journey drives order**: derive the primary user journey from the Inputs and order work to unlock that journey as early as possible.
- **Ship-first**: prioritize a "daily-usable" MVP early; iterate and refine after.
- **Demoable slices**: every slice must produce a runnable, testable increment and include ✅ Demo criteria.
- **Scope discipline**: keep slices small; avoid big rewrites before there's something to dogfood.

## Important

- Do NOT introduce feature ideas not explicitly present in the Inputs.
- Do NOT add illustrative examples unless the Inputs include them.
- Keep wording generic (avoid project-specific technical assumptions).

## Workflow

Operate in read-only mode throughout. Do not write or update files.

1. **Gather Inputs**
   - Project/feature: (1–3 sentences from user)
   - Existing state: (what already exists)
   - Constraints: (platforms, requirements, no-go's)
   - Success looks like: (what "usable" means)

2. **Deep context gathering with Explorer**
   - Delegate to `explorer` to gather codebase facts relevant to the feature: directory structure, existing patterns, related modules, test conventions, and likely touchpoints.
   - Read `README.md`, `AGENTS.md`, and obvious docs (`docs/`, `ARCHITECTURE.md`).
   - Use Explorer findings to ground the plan in real code — do not guess about patterns, file locations, or APIs that can be looked up.

3. **Ask follow-ups only if blocking**
   - At most 1–2 questions; prefer multiple-choice.
   - If unsure but not blocked, make reasonable assumptions and proceed.
   - **Question classification**: before asking the user anything, check if it is a codebase fact (file locations, patterns, APIs) or a user preference (priority, scope, constraints). For codebase facts, use Explorer first — never ask the user about things you can look up.

4. **Create the plan using the template below**
   - Output only the plan—no meta explanations.
   - 80%+ claims should cite specific files or code locations found during context gathering.

5. **Optional: Oracle review**
   - If the plan touches architecture, security, or multiple subsystems, delegate the plan to `oracle` for a review pass before presenting to the user.
   - Include Oracle's findings (risks, blind spots, suggestions) as a brief addendum after the plan.

6. **Save the plan**
   - Write the plan to `docs/plans/active/<slug>.md` in the project root.
   - If a plan with that slug already exists, confirm with the user before overwriting.

## Plan template (follow exactly)

```markdown
# Goals
- (tight list of user-visible outcomes)

# Non-goals
- (explicitly deferred scope)

# Design principles
- User journey drives order
- (add only principles that clearly follow from Inputs)

# User journey
1. (core journey as numbered steps from the user's point of view)
2. (only include steps implied by the Inputs)

# Foundations / Already shipped (✅)
List capabilities that already exist and should not be rebuilt.

## <Capability name>
- What exists: ...
- ✅ Demo: (how to verify quickly)
- Gaps: (only if any)

# MVP slices (ship-shaped, demoable)
Define Slice 1..N in user-journey order.

## Slice 1: <name>
- **Goal**: ...
- **Scope checklist**:
  - [ ] ...
  - [ ] ...
- **✅ Demo**: ...
- **Risks / failure modes**:
  - ...

## Slice 2: <name>
...

# Contracts (guardrails)
List non-negotiable behaviors that must not regress (derived from Inputs and existing behavior).
- ...

# Key decisions (decide early)
List only decisions that would cause rework if postponed (derived from Inputs).
- ...

# Testing
- Manual smoke demos per slice
- Minimal regression tests only for contracts

# Polish phases (after MVP)
Group improvements into phases, each with a ✅ check-in demo.
Limited strictly to scope present in Inputs.

## Phase 1: <name>
- ...
- ✅ Check-in demo: ...

# Later / Deferred
Explicit list of "not now" items + what would trigger revisiting them.
- ...
```

## Slice guidance

**Good slices:**
- Unlock a step in the user journey
- Are runnable and testable
- Have clear ✅ Demo criteria
- "Ugly but functional" over "polished but incomplete"

**Avoid:**
- Slices that don't produce user-visible value
- Big rewrites before something to dogfood
- Adding complexity without paying rent in user value
- Slices too large to finish and demo quickly


