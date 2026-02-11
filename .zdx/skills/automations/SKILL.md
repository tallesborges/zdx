---
name: automations
description: Create, edit, validate, and test ZDX automations stored in `$ZDX_HOME/automations/*.md`. Use when users ask to add or modify automation files, recurring jobs, scheduled prompts, or YAML-frontmatter automation definitions.
---

# Automations Skill

Create and maintain automation files using the Automations CLI.

## Contract (must follow)

- Keep automations global-only in `$ZDX_HOME/automations/` (usually `~/.zdx/automations/`).
- Treat one file as one automation.
- Derive automation identity from file stem (no `id` field).
  - Example: `~/.zdx/automations/morning-report.md` -> `morning-report`.
- Require markdown with YAML frontmatter delimited by `---`.
- Keep prompt body as non-empty markdown after frontmatter.
- Never use deprecated `enabled`.

### Allowed frontmatter keys

- `schedule` (string, optional cron)
- `model` (string, optional)
- `timeout_secs` (int, optional, must be `> 0`)
- `max_retries` (int, optional, default `0`)

Do not add extra keys unless explicitly requested.

## Core principles

### Keep prompts concise

The automation prompt should include only the instructions needed to execute the task.

### Keep prompts deterministic

Use explicit expected output shape (sections/bullets/constraints) so runs are easy to review.

### Keep scope tight

Only modify automation files and only the fields needed for the request.

## Workflow

1. Infer automation file name from user intent (kebab-case).
2. Create or edit `$ZDX_HOME/automations/<name>.md`.
3. If file exists and user did not request replacement, edit in place.
4. Keep frontmatter minimal and valid.
5. Run validation:

   ```bash
   zdx automations validate
   ```

6. If user asks to test, run:

   ```bash
   zdx automations run <name>
   ```

7. Report:
   - changed file path
   - validation status
   - test-run status (if executed)

## Templates

### Scheduled automation template

```md
---
schedule: "0 8 * * *"
# model: "gemini-cli:gemini-2.5-flash"
# timeout_secs: 900
# max_retries: 1
---

<clear prompt for what should run>
```

### Manual-only automation template

```md
---
# model: "gemini-cli:gemini-2.5-flash"
# timeout_secs: 900
# max_retries: 0
---

<clear prompt for what should run>
```

## Writing guidance for prompt body

- Use direct imperative instructions.
- Specify expected output shape (bullet points, sections, constraints).
- Keep prompt concise and task-focused.
- Include scope boundaries (what to include/exclude) when relevant.
- Avoid ambiguous goals like "improve everything".

## Safety

- Do not create extra documentation files.
- Do not create automation files outside `$ZDX_HOME/automations/` unless explicitly requested.
- Prefer editing existing automation files over creating duplicates.

## Completion checklist

Before finishing, ensure:

- automation file exists at `$ZDX_HOME/automations/<name>.md`
- frontmatter is valid and minimal
- body prompt is non-empty and specific
- `zdx automations validate` was run
- final response includes file path, validation status, and run status (if tested)