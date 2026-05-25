---
name: brainstorm
description: Explore options and pick a direction before committing to a goal or plan. Use when the user wants to think through approaches, weigh trade-offs, or decide between alternatives — not yet ready to define success criteria or slice work. Hard rule: no code, files, or implementation until the user explicitly approves a direction.
metadata:
  short-description: Explore options and pick a direction
---

# Brainstorm

## Goal

Help the user move from a fuzzy idea to a chosen direction quickly, by exploring 2 concrete approaches and converging on one. This is the "which way" step that comes before goal-shaping (`define-goal`) or planning (`ship-first-plan`).

## Hard rule

Do NOT write code, scaffold files, or take any implementation action until the user has explicitly approved a direction. This applies even when the task seems obvious. The whole point is to pause and think before building.

## When to use

Use this skill when:
- The user says "let's brainstorm", "help me think through", "what are the options for", or similar.
- The right approach is genuinely unclear and a quick exploration would prevent wasted work.
- You're about to propose a feature, component, or behavior change with non-trivial trade-offs.

Skip this skill when:
- The task is mechanical (one obvious correct approach).
- The user has already chosen a direction.
- The work is small enough that the brainstorm would take longer than just doing it.

## Workflow

Operate in read-only mode. Do not write or update files.

1. **Discover (quick)**
   - Skim relevant context: existing patterns, conventions, related code or notes. One round of reads, not deep exploration — if discovery would need many rounds, delegate to `explorer` first.
   - Ask up to 3 focused questions only if needed. Batch them in one message. Prefer multiple-choice over open-ended.
   - If the request is already clear, skip questions and go straight to Propose.

2. **Propose exactly 2 approaches**
   - Present 2 concrete approaches with trade-offs. Not 1 (no real choice), not 5 (decision paralysis).
   - Lead with your recommendation and a one-line reason.
   - Keep each option to a short paragraph. Scale detail to the weight of the task.
   - If you genuinely can only think of one viable approach, say so and explain what was eliminated.

3. **Converge**
   - Get explicit approval before proceeding. "Sounds good" or "go with option 2" counts.
   - If the user rejects both, revise and re-propose — **max 2 revise rounds**.
   - If still not aligned after 2 rounds, ask the user to state the direction directly. A good-enough direction chosen quickly beats a perfect one chosen slowly.

4. **Capture and hand off**
   - Restate the chosen direction in one or two sentences (what, why, key constraints).
   - Point to the natural next step:
     - For feature work that needs to be sliced and shipped → suggest `ship-first-plan`.
     - For work that needs a sharper success criterion → suggest `define-goal`.
     - For straightforward execution → proceed directly.
   - Do not create a separate design doc unless the user asks for one.

## Principles

- **Speed over ceremony** — the value is in the thinking, not the artifact. A short conversation that produces a good decision beats a polished document that delays one.
- **YAGNI** — design only for what's needed now. Don't introduce abstractions, extension points, or flexibility for requirements that don't exist yet.
- **Bias toward action** — when two options are close in quality, pick one and move. Movement creates clarity.
- **Batched discovery** — ask all clarifying questions in one message, not one at a time.
- **Proportional depth** — a small decision might compress steps 1–2 into a single message. A new subsystem deserves more exploration in step 2.
- **Two options, one recommendation** — always lead with what you'd do and why. Don't present neutral menus.
