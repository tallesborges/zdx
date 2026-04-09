---
name: memory
description: "Use for memory-related tasks: when the user mentions notes, memory, saved information, or asks factual questions that may already be documented in their notes."
---

# Memory

Use the user's markdown notes under `$ZDX_MEMORY_ROOT`.

This skill is about routing and tool usage, not about defining a full filing philosophy. Follow the existing note structure you find in memory.

## Paths

- Memory root: `$ZDX_MEMORY_ROOT`
- Notes: `$ZDX_MEMORY_ROOT/Notes`
- Calendar: `$ZDX_MEMORY_ROOT/Calendar`
- Memory index: `$ZDX_MEMORY_ROOT/Notes/MEMORY.md`

There is no dedicated memory tool. Use the normal file tools with these paths.

- Use `$ZDX_MEMORY_ROOT` directly in tool arguments.
- Treat `$ZDX_MEMORY_ROOT` as a container directory, not a note location.
- Do not create `.md` files directly under `$ZDX_MEMORY_ROOT`; use `Notes/` for regular notes and `Calendar/` for period notes.
- Search `Notes/` and `Calendar/` unless the user clearly scopes the request.
- Ignore `@Archive/` and `@Trash/` unless the user asks.
- Do not guess alternate memory locations; derive from `$ZDX_MEMORY_ROOT`.

## Default workflow

1. Check the embedded `<memory_index>` block to find likely notes.
2. Search the relevant memory paths with `grep` or `glob`.
3. Read only the matching files you need.
4. Answer directly, or edit the note with `apply_patch`.
5. When saving memory, write full detail to a note first, then decide whether `MEMORY.md` needs a concise pointer.

## Tool patterns

### Read the memory index

```text
read path:"$ZDX_MEMORY_ROOT/Notes/MEMORY.md"
```

### Search memory

Run note and calendar searches in parallel unless the user clearly scoped one path.

```text
grep path:"$ZDX_MEMORY_ROOT/Notes" glob:"**/*.md" pattern:"alice|cpf" case_insensitive:true
grep path:"$ZDX_MEMORY_ROOT/Calendar" glob:"*.md" pattern:"alice|cpf" case_insensitive:true
```

Tips:

- Use targeted regexes instead of broad scans.
- For terms on the same line, use `term1.*term2|term2.*term1`.
- Use `read` after `grep` to confirm context.
- If a path fails, search under the configured roots instead of inventing a different absolute path.

### List candidate notes

```text
glob path:"$ZDX_MEMORY_ROOT/Notes" pattern:"**/*.md"
glob path:"$ZDX_MEMORY_ROOT/Calendar" pattern:"*.md"
```

Narrow by folder or pattern whenever possible.

### Edit or create notes

- Read the file before editing.
- Prefer updating an existing note over creating a new one.
- Use `apply_patch` for note edits.
- If a directory must be created first, use a small `bash` helper such as `mkdir -p`.
- Use `bash` only for shell helpers like `date`, `mkdir -p`, or moving files; prefer native tools for search and reads.

Useful helpers:

```text
bash command:"date +%Y%m%d"
bash command:"date +%G-W%V"
bash command:"date +%Y-%m"
bash command:"mkdir -p \"$ZDX_MEMORY_ROOT/Notes/Some Folder\""
```

## When to consult memory

Consult memory for factual questions about the user or things they own, manage, or have already documented, such as:

- personal facts and preferences
- family details and relationships
- work and project context
- saved links, plans, and reference notes
- past decisions, discussions, and history

If the answer is more likely to live in a current external system, prefer the matching live-system skill instead:

- `gog` for Google Calendar, Gmail, Contacts, Drive, Docs, and Sheets
- `apple-reminders` for Apple Reminders
- `wacli` for WhatsApp

If memory answers the question, respond directly. Ask a follow-up only when memory is missing, ambiguous, or clearly not the right source.

## Saving memory

### Immediate saves and suggestions

- If the user explicitly says `remember X`, save it immediately.
- When proactive suggestions are enabled, suggest at most once per response and only for clearly useful items.
- Use this exact format:

```text
💡 Want me to save [specific item] to [specific note]?
```

- If the user says yes, save immediately.
- If the user says no or ignores it, move on.

### Note-first policy

1. Save the full detail in the best target note.
2. Promote to `MEMORY.md` only if the item is durable and likely to be reused.
3. In `MEMORY.md`, keep entries short and merge/update existing pointers instead of appending duplicates.

### What belongs in `MEMORY.md`

Promote durable pointers such as:

- stable preferences
- key personal facts
- long-lived project decisions
- recurring patterns
- important note locations

Keep note-only items out of `MEMORY.md`, for example:

- one-off updates
- temporary blockers
- detailed meeting notes
- ad-hoc links that are unlikely to matter later

### Thread references

When saving technical or project memory that may need later review, include the current thread ID (`$ZDX_THREAD_ID`) if future-you may want the original discussion for reasoning or tradeoffs.

Usually skip thread references for simple facts like names or stable preferences.

## Search and filing rules

- Search both `Notes/` and `Calendar/` unless the user clearly scopes the request.
- Follow the folder and naming conventions that already exist in the relevant area.
- Do not invent new Johnny Decimal codes or reorganize large areas without explicit approval.
- Prefer appending to or updating an existing note over creating a new one.
- If the right destination is unclear, inspect nearby notes first and then make the smallest reasonable choice.

## Reference docs

Load these only when you need deeper guidance beyond the default workflow above:

- `references/noteplan.md` — NotePlan-compatible filenames, note/title rules, task syntax, links, and attachments.
- `references/johnny-decimal.md` — filing guidance for the existing Johnny Decimal structure, including the current `10-19 Life Admin` layout.

## Output expectations

- Keep responses constrained to what the user asked.
- For searches, show path + line number when useful.
- For edits, summarize which note(s) changed and why.
