---
name: skill-creator
description: Create or update a skill (a SKILL.md-format module that extends an agent's capabilities). Use when the user wants to make a new skill, refactor an existing one, capture a repeatable workflow as auto-invoked guidance, or says "create a skill for X", "turn this into a skill", or "update the X skill". Works for any SKILL.md target (zdx, Anthropic Claude apps, etc.) and covers skill-vs-alternatives choice, frontmatter rules, save locations, per-step structure, and the draft-then-confirm-then-write flow.
license: Complete terms in LICENSE.txt
---

# Skill Creator

Build skills: SKILL.md modules that auto-invoke when their description matches the situation. The format is the same across runtimes (Anthropic Claude apps, zdx, etc.); save location and packaging differ. This skill covers deciding whether a skill is the right shape, drafting it, and writing it to the right place.

## Step 0: Is a skill the right shape?

Skills earn their context tax (the description sits in every prompt) by being **auto-invoked when the model recognizes the situation**. Before drafting, check the alternatives:

- **Custom command / slash command** (e.g. zdx `.zdx/commands/<name>.md`, Claude Code custom commands) — the workflow is user-fired at a chosen moment (`/skillify`, `/release-notes`). Suggest a command and stop.
- **Standalone script** — deterministic transform with no decision-making. Drop it in the repo or a bin dir.
- **One-off ask** — the user just wants the thing done now, not a reusable artifact. Do the work; don't make a skill.
- **Skill** — the agent needs to recognize a class of situations and apply procedural knowledge it doesn't already have, without the user remembering to invoke anything.

If "skill" wins, continue.

## Anatomy

```
<skill-name>/
├── SKILL.md          # required
├── references/       # optional; loaded into context only when needed
├── scripts/          # optional; deterministic helpers, may run without being read
└── assets/           # optional; files used in skill output, not loaded into context
```

Most skills are a single `SKILL.md`. Add `references/`, `scripts/`, or `assets/` only when the content is too big or too deterministic to live inline.

## Frontmatter rules

```yaml
---
name: <kebab-case>
description: <see formula below>
---
```

- `name`: lowercase letters, digits, hyphens; ≤64 chars; no leading/trailing/double hyphens; matches the directory name.
- `description`: ≤1024 chars; no `<` or `>`; this is the **single most important field** because it's the only thing the agent matches against to decide whether to load the skill.
- Allowed extra keys (rarely needed): `license`, `metadata`, `compatibility`, `allowed-tools`. Anything else is invalid.

`scripts/quick_validate.py <skill-dir>` enforces all of the above; run it when in doubt.

### Description formula

Lead with **"Use when the user…"** and include **2–3 concrete trigger phrases** the user is likely to say.

- Bad: `"Helps with PDFs."`
- Good: `"Read, create, and edit PDFs. Use when the user asks to extract text from a PDF, fill a form, rotate pages, or says 'merge these PDFs' or 'convert this PDF to images.'"`

## Where to save

Pick the location based on the runtime the skill targets. Ask the user if it's not obvious.

- **zdx project skill** — `.zdx/skills/<name>/SKILL.md` — workflow references this repo.
- **zdx user skill** — `$ZDX_HOME/skills/<name>/SKILL.md` — follows the user across all repos.
- **Anthropic / external skill** — author anywhere convenient (often `skills/public/<name>/` or `skills/private/<name>/`), then package into a `.skill` zip with `scripts/package_skill.py` for distribution.
- Default to project when the workflow names this repo's files; otherwise user/external.

## Core principles

### Concise is key

The context window is shared. Default assumption: the agent is already smart. Only add what it doesn't already know. Challenge each line: does it justify its tokens?

### Set appropriate degrees of freedom

- **High** (prose) — multiple valid approaches; decisions depend on context.
- **Medium** (pseudocode, parameterized scripts) — preferred pattern with variation.
- **Low** (specific scripts, fixed sequences) — fragile, error-prone, or consistency-critical.

A narrow bridge with cliffs needs guardrails. An open field allows many routes.

### Artifact location discipline

When a skill produces files (reports, screenshots, audio, HTML, generated docs), default outputs to the runtime's artifact dir. In zdx that's `$ZDX_ARTIFACT_DIR` (use `$ZDX_ARTIFACT_DIR/tmp/` for scratch). For other runtimes, follow their convention or write under a clearly-named subdir the user can find. Avoid casual `tmp/`, `output/`, `/tmp` defaults in examples.

### Don't fabricate

When building a skill from observed work (a thread, a PR, a workflow you watched), only encode steps that actually happened. Don't invent steps to make the workflow feel more complete.

### User corrections are gold

Wherever the user pushed back, redirected, or refined the work — those are the rules to encode. They're more valuable than the happy path.

## Progressive disclosure

Keep `SKILL.md` under ~500 lines. When it grows, split into reference files and link from `SKILL.md`:

- **Multi-step workflows, sequential logic, conditional branching** → see `references/workflows.md`
- **Output formats, templates, quality standards** → see `references/output-patterns.md`

Reference files load only when the agent follows the link. Keep `SKILL.md` focused on the core workflow and navigation; push variant-specific details, schemas, and examples into references. Avoid deeply nested references — all reference files should link directly from `SKILL.md`. For files >100 lines, include a table of contents at the top.

## Workflow

### 1. Understand the skill

Get concrete examples — either from the user, or by inferring from observed work. Ask:

- "What should this skill do?"
- "Which runtime is it for? (zdx, Anthropic Claude apps, other)"
- "Give me 2–3 example user requests that should trigger it."

Stop asking once you have enough. **Don't over-interview.** A 2-step workflow doesn't need four rounds of questions.

If the skill is being built from observed work in this thread or a past one (zdx-specific: lean on Thread Searcher for context that's scrolled out), the conversation history is the source material — don't re-derive from scratch.

### 2. Decide what's reusable

For each example, identify what would be helpful to bundle:

- Same code rewritten each time → `scripts/<name>.py`
- Domain knowledge, schemas, API docs → `references/<topic>.md`
- Templates, boilerplate, brand assets used in output → `assets/<file>`

Most skills don't need any of these. Don't add directories for hypothetical needs.

### 3. Initialize the directory

Two ways, pick whichever fits:

- **Direct write** (best for simple single-file skills, especially zdx user/project skills): `mkdir -p <path>/<name>` and write `SKILL.md` directly.
- **Scaffold via `scripts/init_skill.py <name> --path <dir>`**: creates the directory plus example `scripts/`, `references/`, `assets/` files. Useful when starting a complex skill or when targeting Anthropic distribution. Delete the example files you don't need.

### 4. Draft `SKILL.md` in chat

Write the full `SKILL.md` as a fenced markdown block in the response. Include the chosen save path. Ask one short confirmation question. Don't write the file yet.

### 5. Confirm and write

On confirmation, write the file. Do not write before confirmation.

### 6. Validate (and package, if distributing)

- Run `scripts/quick_validate.py <skill-dir>` to check frontmatter shape.
- If the skill is being distributed as a `.skill` archive (Anthropic apps), run `scripts/package_skill.py <skill-dir> [output-dir]` — this re-validates and produces `<name>.skill` (a zip).
- For zdx skills, no packaging step — the runtime loads the directory directly.

### 7. Final summary

Tell the user:

- The file path
- The final name and description
- Which steps came from explicit user corrections (if any)
- Anything left as `TODO` inside the `SKILL.md`
- Whether a `.skill` package was produced and where

The skill auto-invokes on description match — no slash command needed. The user can edit `SKILL.md` directly to refine triggers.

## Per-step schema (when needed)

For multi-step workflows, annotate each step. Required: `Success criteria`. Optional: the rest.

```markdown
### 1. <Step name>
What to do. Specific commands when relevant.
**Success criteria:** how you know this step is done.
**Execution:** Direct (default), `[human]`, or a named subagent the runtime supports.
**Artifacts:** data this step produces that later steps need.
**Human checkpoint:** when to pause (irreversible actions, destructive ops, external messages).
**Rules:** hard constraints, especially user corrections from the source material.
```

- For zdx, the named subagents are Explorer, Oracle, Thread Searcher, and Task. Reference them as proper nouns in prose; use the lowercase id (`explorer`, `oracle`, `thread-searcher`, `task`) only when it's the literal value passed to `invoke_subagent`. Other runtimes have their own subagent names — use those instead.
- For parallel steps, use sub-numbers: `3a`, `3b`.
- Steps the user must do themselves get `[human]` in the title.
- Simple skills don't need annotations on every step. A 2-step workflow can stay plain prose.

## What not to include

A skill should only contain files that directly support its function. Do **not** create:

- `README.md`, `INSTALLATION.md`, `CHANGELOG.md`, `QUICK_REFERENCE.md`
- Setup/testing notes
- Meta-commentary about how the skill was made

The skill is for an AI agent, not a human reader. Extra files are clutter.

## Iterate

After the skill runs on real tasks, watch for:

- Skill didn't trigger when it should have → tighten the description, add trigger phrases.
- Skill triggered when it shouldn't have → narrow the description.
- A step kept failing → add success criteria or a `Rules` block with the correction.
- Same content rewritten across runs → promote to `scripts/` or `references/`.

Edit `SKILL.md` directly. Re-run `scripts/quick_validate.py` after frontmatter changes. No build step for zdx; re-run `scripts/package_skill.py` if you're shipping a `.skill` archive.
