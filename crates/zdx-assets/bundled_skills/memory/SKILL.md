---
name: memory
description: "Use for memory-related tasks: saved notes, factual questions that may already be documented, and saving durable information. Prefer Memory_Search and Memory_Get for discovery; use this skill for routing, note-saving, and filing conventions."
---

# Memory

Use ZDX's qmd-backed memory tools and the user's markdown notes under `$ZDX_MEMORY_ROOT`.

This skill is about routing, note-saving, and filing conventions. It is not the search implementation and does not replace `Memory_Search` / `Memory_Get`.

Follow the existing note structure you find in memory.

## Paths

- Memory root: `$ZDX_MEMORY_ROOT`
- Notes: `$ZDX_MEMORY_ROOT/Notes`
- Calendar: `$ZDX_MEMORY_ROOT/Calendar`
- Memory index: `$ZDX_MEMORY_ROOT/Notes/MEMORY.md`

Memory discovery is qmd-backed. Prefer memory tools before raw file searches.

- Use `$ZDX_MEMORY_ROOT` directly in tool arguments.
- Treat `$ZDX_MEMORY_ROOT` as a container directory, not a note location.
- Do not create `.md` files directly under `$ZDX_MEMORY_ROOT`; use `Notes/` for regular notes and `Calendar/` for period notes.
- Ignore `@Archive/` and `@Trash/` unless the user asks.
- Do not guess alternate memory locations; derive from `$ZDX_MEMORY_ROOT`.

## Default workflow

1. Check the embedded `<memory_index>` block to find likely notes.
2. Use `Memory_Search` for discovery across exported threads, notes, and calendar files.
3. Use `Memory_Get` on returned `thread:`, `note:`, or `calendar:` refs when you need canonical evidence.
4. Answer directly, or edit the relevant note with `apply_patch`.
5. When saving memory, write full detail to a note first, then decide whether `MEMORY.md` needs a concise pointer.

## Tool patterns

### Search memory

Use `Memory_Search` for open-ended memory discovery. It searches qmd-backed collections for:

- exported conversation threads
- canonical notes under `$ZDX_MEMORY_ROOT/Notes`
- canonical calendar files under `$ZDX_MEMORY_ROOT/Calendar`

Search by meaning, names, project terms, decisions, URLs, or distinctive phrases. Do not manually slug note paths or guess filesystem names; qmd may return normalized paths, and ZDX maps them back to canonical refs.

Start with a focused natural-language query and a small limit. If results are weak, run a second search with synonyms, aliases, acronyms, or the likely project/person name.

Good search patterns:

```text
Memory_Search query:"architecture decision cache invalidation" limit:10
Memory_Search query:"renewal deadline reference" limit:10
Memory_Search query:"CLI search migration notes" limit:10
```

Avoid weak searches:

- very broad terms like `work`, `notes`, or `project`
- raw regex syntax; `Memory_Search` is semantic/qmd-backed, not grep
- path-only guesses unless the user gave the path or folder name
- searching first when the user already provided an exact `thread:` / `note:` / `calendar:` ref

`Memory_Search` returns stable memory refs such as:

- `thread:<thread_id>`
- `note:<relative_path>`
- `calendar:<relative_path>`

Treat `ref` as the canonical handle for follow-up reads. Treat `relative_path`, title, score, and snippets as display/debug metadata.

Treat search snippets as leads, not source-of-truth evidence. Before answering factual questions, call `Memory_Get` for the most relevant refs and answer from canonical content.

```text
Memory_Search query:"service integration credentials reference" limit:10
```

If `Memory_Search` warns that exported threads changed or results may be stale, continue with the best returned refs when they work. If `Memory_Get` fails for a returned ref or results clearly miss recent notes, run `zdx memory index` when command execution is allowed, then retry the search.

### Read memory refs

Use `Memory_Get` after `Memory_Search` when you need canonical content.

Always pass the full `ref` returned by `Memory_Search`; do not reconstruct refs from snippets or use `relative_path` alone.

```text
Memory_Get ref:"note:20-29 Projects/21.02 References/Example Project.md"
Memory_Get ref:"calendar:2026-05.md"
Memory_Get ref:"thread:00000000-0000-0000-0000-000000000000"
```

Read multiple refs when the question depends on comparing sources or when the first result is only a weak match. Keep the number of deep reads small and targeted.

If the user already provides a thread ID and wants an answer from that thread, use `Read_Thread` directly instead of searching.

### Read the memory index

Use the embedded `<memory_index>` first. Read the full index only when you need the live file or are about to update it.

```text
read file_path:"$ZDX_MEMORY_ROOT/Notes/MEMORY.md"
```

### Raw file access

Use raw `grep`, `glob`, and `read` only when:

- the memory tools are unavailable in the current runtime
- you need to inspect nearby files before choosing where to save a note
- you are editing a known note path and need exact surrounding context
- the user explicitly asks for filesystem-level note work

When raw search is necessary, search `Notes/` and `Calendar/` unless the user clearly scopes the request.

```text
grep file_path:"$ZDX_MEMORY_ROOT/Notes" glob:"**/*.md" pattern:"deadline|reference" case_insensitive:true
grep file_path:"$ZDX_MEMORY_ROOT/Calendar" glob:"*.md" pattern:"deadline|reference" case_insensitive:true
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
- people, relationships, and household context
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

## Filing rules

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
