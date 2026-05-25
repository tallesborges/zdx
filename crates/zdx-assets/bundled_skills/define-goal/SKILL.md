---
name: define-goal
description: Shape a fuzzy intention into a concrete, measurable goal with explicit success evidence before starting work. Use when the user asks to define a goal, clarify success criteria, sharpen an objective, or turn "make X better" into something verifiable. Useful as a quick pre-step before planning, debugging, performance work, research, or operations work.
metadata:
  short-description: Turn fuzzy intent into a measurable goal
---

# Define Goal

## Overview

Shape the user's intent into an objective an agent can pursue honestly. Prefer measurable outcomes, explicit evidence, and bounded scope over activity descriptions.

This skill is goal-shaping only. It does not produce plans, specs, ledgers, or implementation artifacts — pair it with `ship-first-plan` (for feature work) or proceed directly to implementation when the goal is clear.

## Workflow

Operate in read-only mode. Do not write or update files.

1. **Confirm goal definition is needed**
   - Use this skill when the user explicitly asks for it, wants help turning an intention into a clear objective, or when downstream planning/implementation work would be at risk without a sharper success criterion.
   - If the user has already stated a measurable, verifiable goal, skip this skill and proceed with the actual work.

2. **Restate the likely goal in concrete terms**
   A usable goal names:
   - the specific outcome that will be true
   - the main artifact, system, repo, environment, or user-facing behavior involved
   - how completion will be verified
   - what is in scope
   - what is out of scope when ambiguity would matter
   - the stop condition for asking the user instead of grinding

3. **Make it quantitative when the domain supports it**
   Prefer numbers that represent real success, not decorative precision:
   - **pass/fail validators**: exact tests, checks, CI jobs, evals, commands, or acceptance criteria
   - **quality thresholds**: latency, error rate, cost, accuracy, recall, precision, coverage, flake rate, bundle size, memory, uptime, completion rate
   - **artifact constraints**: file paths, affected modules, allowed commands, output formats, target environments, deadlines, or maximum blast radius
   - **evidence counts**: number of reproduced failures, successful reruns, reviewed examples, migrated records, addressed comments, or verified cases

4. **Repair weak goals before accepting them**
   - Rewrite vague goals into measurable objectives when local context makes the rewrite safe.
   - Ask one concise clarification question when the missing detail changes the intended outcome or validation.
   - Reject pure activity goals such as "make progress", "keep investigating", "improve things", or "work on X" unless they are sharpened into a verifiable outcome.

5. **Present the goal for confirmation**
   - Output a single concise objective string that includes the verification evidence inline.
   - Include scope bounds when they constrain the work.
   - Ask the user to confirm or adjust before any downstream work begins.

## Goal Quality Bar

Before accepting a goal, it should answer:

- What concrete thing will be true when this is done?
- What evidence will prove it?
- What quantitative or binary threshold defines success?
- What scope boundaries matter?
- What should cause the agent to stop and ask?

**Good:**

> Reduce checkout API p95 latency below 250 ms for the documented slow path by making the smallest safe server-side change, then verify with `npm run test:checkout` and the existing local latency benchmark showing p95 under 250 ms across 3 consecutive runs.

**Good:**

> Resolve the open review comments on PR 123 that request code changes, update only the affected auth files and tests, and verify with the targeted auth test command plus `gh pr view 123` showing no unresolved change-request threads.

**Weak:**

> Make checkout faster.

**Weak:**

> Keep investigating the PR comments.

## Quantification Heuristics

- **Bugs**: define success as reproduction first, fix second, and a failing-then-passing validator when possible.
- **Tests**: name the exact command and required pass condition.
- **Performance**: name the metric, target threshold, measurement method, and number of runs.
- **Quality work**: define an observable acceptance bar such as reviewed examples, lint/typecheck/test pass, or user-approved artifact.
- **Research**: define the decision the research must enable, the sources or systems in scope, and the evidence standard.
- **Operations**: define healthy state, monitoring window, failure threshold, and rollback or escalation trigger.
- **Features / shipping**: define the user-visible behavior that becomes true and the demo that proves it (then hand off to `ship-first-plan` for slicing).

## Clarifying Questions

Ask only when a reasonable rewrite would risk pursuing the wrong outcome. Keep the question short and oriented around the missing validator or scope boundary.

Useful question shapes:

- "What metric should define success here: latency, cost, accuracy, or user-visible behavior?"
- "Which environment should I verify against: local, staging, or production?"
- "What is the minimum evidence you want before I consider this done?"

If the user cannot provide a metric, propose the most honest binary validator available and ask for confirmation.
