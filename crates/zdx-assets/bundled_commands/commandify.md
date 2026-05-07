---
description: Capture a prompt from this thread as a reusable slash command
---
Turn a prompt from this thread into a reusable zdx slash command at `$ZDX_HOME/commands/<name>.md`.

Rules:
- The conversation history above is your source material. Pay extra attention to outputs from `/prompt-builder` and to messages where the user refined or corrected the prompt — those are usually the thing worth saving.
- If the body to save is ambiguous (multiple candidate prompts in the thread), ask once which one before drafting.
- Commands are user-fired at a chosen moment. If the workflow is fundamentally pattern-recognized and should auto-invoke, suggest a skill via `/skillify` instead and stop.
- Built-ins always win. Reject any name that collides with a built-in command or alias and tell the user why.
- Never overwrite an existing command file silently.
- Do not invent a description that isn't supported by the prompt body.

Phase 1 — Analyze the thread (silent):
- Which message contains the prompt body to save?
- What is the user's real intent for this command — when would they want to invoke it?
- Is there a natural one-line description hiding in the body's opening sentence?
- Did the user already suggest a name?

Phase 2 — Interview the user, one focused round at a time:

Round 1 — Shape:
- Propose a kebab-case name and a one-line description.
- Show the prompt body you plan to save (trimmed of preamble, no fenced wrapper).
- Confirm or let the user rename / edit the description.

Round 2 — Body refinements (only if needed):
- If the body still references "above"/"earlier"/specific scrolled-back content, propose rewrites that work as a slash command invoked inside an active thread (`this bug`, `the code I just changed` is fine; `as I said earlier` is not).
- If the body lacks an explicit `At the end, give me:` block and the workflow asks the assistant to do something, propose adding one.
- Skip this round entirely if the body is already in good shape.

Stop interviewing as soon as you have enough. Iterate a round only if a real ambiguity remains.

Phase 3 — Draft the command file:

Use this exact shape:

```markdown
---
description: <one-line description, lower case start, no trailing period>
---
<prompt body, second person, no leading blank line, single trailing newline>
```

Drafting rules:
- `description` is what shows in the command palette next to `/<name>`. Lead with a verb. Keep it under ~80 characters.
- The body must read as standalone instructions to the future assistant invoked inside a thread (second person, no "I will...").
- Reference subagents as proper nouns when applicable: Oracle (review/diagnosis), Explorer (read-only local discovery), Thread Searcher (saved-thread retrieval), Task (scoped implementation).
- For actionable workflows, the body should normally include: opening 1-line imperative, a `Rules:` block, an explicit loop arrow when iterative (`inspect → ... → repeat`), role separation when multi-agent, and a closing `At the end, give me:` block. Drop these only when the prompt is plainly a pure transformation/data prompt.
- Do not strip frontmatter the user already wrote into the body. If the body already starts with `---`, use it as-is and only adjust the description if needed.

Phase 4 — Validate, save, confirm:
- Validate the name: must match `^[a-z0-9][a-z0-9-]*$` and must not collide with any built-in slash command or alias defined in `crates/zdx-tui/src/common/commands.rs` (read that file to check). If invalid, stop and explain.
- Check `$ZDX_HOME/commands/<name>.md` with `glob`. If it exists, ask the user whether to overwrite or pick a new name. Do not overwrite without explicit confirmation.
- Write the file with the `write` tool.
- Tell the user to run `/commands-refresh` (or restart) to pick up the new command in the palette.

At the end, give me:
- the file path that was written
- the final name and description
- the prompt body that was saved (so I can verify what got persisted)
- which thread message the body was sourced from
- anything left as TODO inside the command (for example a `[describe X]` placeholder)
