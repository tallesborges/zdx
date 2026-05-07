---
description: Capture this session's repeatable workflow as a reusable skill
---
Turn what we just did in this thread into a reusable skill.

Rules:
- The conversation history above is your source material. Do not template-inject anything — just look back at the actual messages.
- Pay extra attention to where the user corrected, redirected, or refined what you were doing. Those moments are the real preferences worth encoding.
- Use Thread Searcher only if you need older context that has scrolled out of the active conversation.
- Skills earn their context tax by being auto-invoked when the model recognizes the situation. If the workflow is fundamentally user-fired at a chosen moment, suggest a custom command (`.zdx/commands/<name>.md`) instead of a skill and stop.
- Do not over-interview. A simple two-step workflow does not need four rounds of questions.
- Never invent steps that did not happen in the thread.

Phase 1 — Analyze the thread (silent):
- What repeatable process was performed?
- What were the inputs / parameters?
- What were the distinct steps in order?
- What artifact or signal proved each step was done (not "wrote code" but "PR open with CI green")?
- Where did the user steer or correct?
- What tools and subagents were used? (Explorer, Oracle, Thread Searcher, Task, `bash`, `edit`, etc.)
- What were the goals and final success criteria?

Phase 2 — Interview the user, one focused round at a time:

Round 1 — Shape:
- Propose a name (kebab-case) and one-line description.
- Propose the goal and concrete success criteria.
- Confirm or let the user rename.

Round 2 — Structure:
- Present the steps as a numbered list. Tell the user you will dig into details next.
- Propose arguments if the workflow takes parameters; otherwise say so.
- Ask where to save:
  - Project skill: `.zdx/skills/<name>/SKILL.md` — for workflows tied to this repo
  - User skill: `$ZDX_HOME/skills/<name>/SKILL.md` — follows the user across all repos
  - Default to project if the workflow references this repo's structure; otherwise user.

Round 3 — Per-step detail (only for steps that are not glaringly obvious):
- What does this step produce that later steps depend on?
- What proves this step succeeded?
- Does it need a human checkpoint before continuing (irreversible actions, destructive ops, external messages)?
- Can any steps run in parallel?
- Should a specific subagent run the step (Explorer for codebase discovery, Oracle for review/analysis, Task for scoped implementation)?
- Hard rules — things that must or must not happen, especially the user's corrections from the thread.

Round 4 — Triggers (only if not already obvious):
- Confirm trigger phrases that should auto-invoke the skill.
- Any gotchas or edge cases worth recording.

Stop interviewing as soon as you have enough. Iterate a round only if a real ambiguity remains.

Phase 3 — Draft the SKILL.md:

Use this exact frontmatter shape (zdx skills only support `name` and `description`):

```markdown
---
name: <skill-name>
description: <one-line description that includes when to use it and 2-3 trigger phrases, since this is what the model matches against>
---

# <Skill Title>

Short description of what this skill does.

## Inputs
- `<arg-name>`: what the caller must provide. Omit this section if there are no args.

## Goal
The concrete artifact or state that proves the workflow is done.

## Steps

### 1. <Step name>
What to do. Specific commands when relevant.
**Success criteria:** how you know this step is done. (Required on every step.)
**Execution:** Direct (default), Explorer, Oracle, Task, or `[human]` — only if not Direct.
**Artifacts:** data this step produces that later steps need. Omit if nothing flows forward.
**Human checkpoint:** when to pause for confirmation. Include for irreversible actions.
**Rules:** hard constraints, especially user corrections from the source thread.

### 2. <Next step>
...
```

Drafting rules:
- `description` is the auto-invocation hook. Lead with "Use when the user...". Include 2–3 concrete trigger phrases. This is the single most important field.
- Concurrent steps use sub-numbers: `3a`, `3b`.
- Steps the user must do get `[human]` in the title.
- Keep simple skills simple — a 2-step workflow doesn't need annotations on every step.
- Reference zdx subagents in prose as proper nouns: Explorer, Oracle, Thread Searcher, Task. Use the lowercase id (`oracle`, `explorer`, `thread-searcher`, `task`) only when naming the literal value passed to `invoke_subagent`.
- Artifacts emitted by the skill at runtime should default to `$ZDX_ARTIFACT_DIR` when relevant.

Phase 4 — Review and save:
- Output the full SKILL.md as a fenced markdown block in chat first.
- Ask the user to confirm with one short question. No long preamble.
- On confirmation, create the directory and write the file at the chosen path.
- Tell the user:
  - The file path
  - That the skill auto-invokes on description match (no slash needed)
  - That they can edit the SKILL.md directly to refine triggers or steps

At the end, give me:
- where the skill was saved
- the final name and description
- which steps came from explicit user corrections during the thread
- anything left as TODO inside the SKILL.md
