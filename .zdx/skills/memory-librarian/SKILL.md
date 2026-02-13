---
name: memory-librarian
description: Maintain persistent memory files (`MEMORY.md` + `memories/*.md`) using safe, selective workflows. Use when users ask to remember/forget/update facts, curate memory files, organize memory index entries, or apply stored preferences while creating/updating skills and automations.
---

# Memory Librarian

Manage memory as a small index + detailed files.

## Memory Model

- Global index: `$ZDX_HOME/MEMORY.md`.
- Project index: `<root>/.zdx/MEMORY.md` (optional).
- Global details: `$ZDX_HOME/memories/*.md`.
- Project details: `<root>/.zdx/memories/*.md`.
- Read detailed files on demand; do not load everything.

## Path Policy

- For project-specific facts, default to `<root>/.zdx/MEMORY.md` + `<root>/.zdx/memories/`.
- For personal cross-project facts, default to `$ZDX_HOME/MEMORY.md` + `$ZDX_HOME/memories/`.
- If user says “remember globally”, use global scope.
- If user says “remember for this project”, use project scope.
- If scope is ambiguous, ask one targeted question before writing.
- Use absolute paths in tool calls.
- When editing one scope (global or project), synchronize that scope's `MEMORY.md` index.

## Bootstrap (first-time setup)

If memory is not initialized yet:

1. Create the scope-specific `MEMORY.md` using `references/memory-bootstrap-template.md`.
2. Create a minimal set of detail files under the matching `memories/` folder (only what is immediately useful).
3. Keep `MEMORY.md` concise and list each detailed file with a one-line description.
4. Prefer user profile + communication preferences first; expand incrementally.

## Trigger Conditions

Use this skill when the user asks to:

- remember, forget, or correct something
- inspect/organize memory files
- reconcile duplicates/conflicts across memory files
- apply personal preferences from memory while creating or updating skills/automations

## Core Rules

- Edit memory only on explicit user intent (e.g., “remember this”, “update my memory”, “forget X”).
- Read before write: inspect target file(s) first.
- Keep entries concise; prefer one fact per line.
- Avoid duplicates and contradictions.
- Never store secrets/credentials/tokens in memory files.
- If a memory detail file is created, renamed, or removed, update `MEMORY.md` in the same turn.

## Retrieval Workflow

1. Use the injected `<memory>` section from the system prompt.
2. Select only relevant detailed files.
3. Read those files.
4. Answer with the relevant facts only.

## Write Workflow

### When user says “remember X”

1. Resolve scope first (global vs project) using Path Policy.
2. Choose the best existing memory file in that scope; create a new one only if needed.
3. Read target file before editing.
4. Add or refine concise facts.
5. Update scope-matching `MEMORY.md` with file entry/description changes.

### When user says “forget/correct X”

1. Resolve scope first (global vs project) using Path Policy.
2. Locate the fact in scope-matching `MEMORY.md` + relevant detailed files.
3. Remove or correct outdated/conflicting facts (do not just append contradictions).
4. Update scope-matching `MEMORY.md` if file scope/description changed.

## Skill/Automation Authoring Support

When creating/updating skills or automations:

1. Use the injected `<memory>` section for preferences/conventions.
2. Read only relevant detailed memory files.
3. Apply those preferences in output structure, naming, and tone.
4. If a new durable preference appears, persist it only when user explicitly asks to remember it.

## Completion Checklist

- Files read
- Files changed
- `MEMORY.md` synchronized (yes/no)
- Any unresolved ambiguity needing user confirmation