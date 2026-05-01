---
description: Review changed code in parallel (reuse + quality + efficiency), then fix
---
Review the code I just changed for reuse, quality, and efficiency. Fix the real issues you find.

Rules:
- Start from the actual diff, not from memory.
- Run `git diff` first; if there are staged changes too, also check `git diff HEAD`.
- If there is no git change, fall back to the files most recently edited in this conversation.
- Pass the full diff to every reviewer so each one has complete context.
- Launch all three reviewers in parallel in a single response — do not serialize them.
- Wait for every reviewer to finish before fixing anything.
- Fix the real issues directly. If a finding is a false positive or not worth addressing, skip it without arguing.
- Do not invent issues to look thorough. If the diff is already clean, say so.

Reviewers (run in parallel):

1. Reuse review — Explorer
   - Search the codebase for existing utilities, helpers, constants, or patterns that the diff duplicates.
   - Flag new functions that re-implement something that already exists.
   - Flag inline logic (string handling, path handling, env checks, type guards, etc.) that should use an existing helper.
   - For each finding, name the existing symbol or file and propose the swap.

2. Quality review — Oracle
   - Redundant state (duplicates existing state, derivable values cached, observers that could be direct calls).
   - Parameter sprawl (new params bolted on instead of restructuring).
   - Copy-paste with slight variation that should be unified.
   - Leaky abstractions or broken encapsulation boundaries.
   - Stringly-typed code where constants, enums, or branded types already exist.
   - Unnecessary wrapper components / nesting that adds no value.
   - Comments that narrate WHAT the code does, narrate the change, or reference the task — keep only non-obvious WHY.

3. Efficiency review — Oracle
   - Redundant work: repeated reads, duplicate calls, N+1 patterns.
   - Missed concurrency: independent operations run sequentially.
   - Hot-path bloat added to startup or per-request/per-render paths.
   - No-op updates inside loops/intervals/handlers without change-detection guards; verify wrapper updaters honor same-reference returns.
   - Pre-existence checks before operating (TOCTOU) — operate directly and handle the error.
   - Memory: unbounded structures, missing cleanup, listener leaks.
   - Overly broad operations: full file reads when a slice would do, loading all items to filter for one.

Execution loop:
collect diff → launch 3 reviewers in parallel → aggregate findings → fix the real ones → verify

Reviewer output contract (each reviewer must return):
- a list of findings, each with: file + line range, the issue, and the concrete suggested change
- false-positive risk callouts so you can filter

Your role:
- decide which findings are real
- apply fixes directly, batched by file when possible
- re-read changed regions after fixing to confirm the fix is coherent
- run a quick build/test only if the change clearly warrants it (do not over-verify)

At the end, give me:
- what each reviewer flagged (one-line per finding)
- what you fixed and where
- what you skipped and why
- anything still worth a human look
