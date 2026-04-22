---
name: deep-interview
description: Socratic requirements clarification for vague or broad requests. Use when the user says "deep interview", "interview me", "clarify this", "don't assume", or when a request is too vague for direct planning. Reduces ambiguity through targeted one-question-at-a-time loops before handing off to ship-first-plan.
metadata:
  short-description: Clarify requirements before planning
---

# Deep Interview

## Goal

Turn a vague idea into an execution-ready spec through Socratic questioning. Reduce ambiguity systematically before planning or implementation begins.

## Use when

- The request is broad, ambiguous, or missing concrete acceptance criteria.
- The user says "deep interview", "interview me", "ask me everything", "don't assume", or "clarify this".
- You need clear requirements before handing off to `ship-first-plan`.

## Do NOT use when

- The request already has concrete file/symbol targets and clear acceptance criteria — go straight to planning or implementation.
- The user explicitly asks to skip interview and execute immediately.
- A complete plan already exists and execution should start.

## Hard constraints

- Ask **ONE question per round** — never batch multiple questions.
- Ask about **intent and boundaries first**, implementation detail later.
- **Never implement** inside this skill — this is requirements only.
- Reduce user effort: never ask about codebase facts that Explorer can look up.

## Workflow

1. **Preflight context**
   - Delegate to `explorer` to gather codebase context relevant to the request (brownfield detection, existing patterns, likely touchpoints).
   - Classify: **brownfield** (existing codebase target) or **greenfield** (new from scratch).
   - For brownfield, use Explorer findings to ask evidence-backed confirmation questions ("I found X in Y. Should this change follow that pattern?").

2. **Initialize tracking**
   - Note the initial idea and classify the depth:
     - **Quick**: target ≤ 5 rounds (for mildly unclear requests)
     - **Standard** (default): target ≤ 10 rounds
   - Track clarity across these dimensions:
     - **Intent** — why the user wants this
     - **Outcome** — what end state they want
     - **Scope** — how far the change should go
     - **Non-goals** — what is explicitly out of scope
     - **Constraints** — technical or business limits
     - **Success criteria** — how completion will be judged

3. **Socratic interview loop**

   Repeat until clarity is sufficient or max rounds reached:

   a. **Pick the weakest dimension** — ask about the least clear area, but respect this priority order:
      - Stage 1 (rounds 1–4): Intent, Outcome, Scope, Non-goals
      - Stage 2 (rounds 5+): Constraints, Success Criteria

   b. **Ask ONE question** — present it with the current round and target dimension:
      ```
      Round N | Focus: <dimension>

      <question>
      ```

   c. **Pressure-test each answer** — before moving to a new dimension:
      - Ask for a concrete example or evidence
      - Probe the hidden assumption behind the answer
      - Force a boundary or tradeoff: "what would you explicitly NOT do?"
      - If the answer describes symptoms, reframe toward root cause

   d. **Report progress** — after each answer, briefly note which dimensions are clear and which still need work.

   e. **Early exit** — from round 4+, if the user signals readiness ("that's enough", "let's plan", "I'm clear"), proceed to crystallize with a note about any remaining gaps.

4. **Crystallize spec**

   When clarity is sufficient or the user exits:

   Write a compact spec covering:
   - **Intent**: why the user wants this
   - **Desired outcome**: what success looks like
   - **In-scope**: what to build
   - **Out-of-scope / Non-goals**: what to explicitly exclude
   - **Constraints**: technical or business limits
   - **Acceptance criteria**: testable conditions for "done"
   - **Assumptions exposed**: what was unclear and how it was resolved
   - **Brownfield context**: relevant existing code/patterns found by Explorer

5. **Handoff**

   Present the spec and offer:
   - **Start planning** → hand off to `ship-first-plan` with the spec as input
   - **Refine further** → continue the interview loop
   - **Skip planning, just do it** → proceed directly to implementation (only if scope is small and clear)

## Question classification

Before asking any question, classify it:

| Type | Example | Action |
|------|---------|--------|
| Codebase fact | "What patterns exist?", "Where is X?" | Use Explorer — do not ask the user |
| User preference | "Priority?", "Timeline?" | Ask the user |
| Scope decision | "Include feature Y?" | Ask the user |
| Tradeoff | "Speed vs correctness?" | Ask the user |

## Anti-patterns

- ❌ Batching multiple questions in one round
- ❌ Asking the user about code structure Explorer can discover
- ❌ Rotating to a new dimension just for coverage when the current answer is still vague
- ❌ Implementing anything inside this skill
- ❌ Over-interviewing when the request is already clear enough to plan
