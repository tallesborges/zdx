---
name: automations
description: Create, update, validate, and run ZDX automations defined as markdown files with YAML frontmatter. Use when users ask to add/edit scheduled automations, morning reports, recurring jobs, or automation prompts.
---

# Automations Skill

Create and maintain automations for this repository using the existing Automations CLI.

## About

Automations are markdown files that package repeatable prompts with optional schedule/model/retry settings.

Treat this skill as a focused "automation authoring workflow": create one valid file, validate it, optionally run it, and report exactly what changed.

## Core Principles

### Keep it concise

The automation prompt should include only the instructions needed to execute the task.

### Keep it deterministic

Use explicit expected output shape (sections/bullets/constraints) so runs are easy to review.

### Keep scope disciplined

Only touch automation files and only fields required by the request.

## Contract (must follow)

- One automation = one file in `$ZDX_HOME/automations/` (usually `~/.zdx/automations/`).
- File name stem is the automation name (no `id` field).
  - Example: `~/.zdx/automations/morning-report.md` -> name `morning-report`.
- File format must be markdown with YAML frontmatter.
- Prompt body is the markdown content after frontmatter.

### Allowed frontmatter keys

- `schedule` (string, optional; if missing, automation is manual-only)
- `model` (string, optional)
- `timeout_secs` (int, optional, must be > 0)
- `max_retries` (int, optional, default `0`)

Do not add extra frontmatter fields unless explicitly requested.

## Creation/Update Process

1. Determine automation file name from user intent (kebab-case).
2. Create or edit `$ZDX_HOME/automations/<name>.md`.
3. If file already exists and user did not ask to replace it, update in place (preserve useful prompt content).
4. Ensure frontmatter includes only required/asked fields.
5. Run validation:

```bash
zdx automations validate
```

6. If the user asked to test it, run:

```bash
zdx automations run <name>
```

7. Report:
   - file path changed
   - whether validation passed
   - whether run test was executed

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

## Safety / hygiene

- Do not create extra documentation files.
- Do not create automation files outside `$ZDX_HOME/automations/` unless explicitly requested.
- Prefer editing existing automation files over creating duplicates.

## Completion checklist

Before finishing, ensure:

- automation file exists at `$ZDX_HOME/automations/<name>.md`
- frontmatter is valid and minimal
- body prompt is non-empty and specific
- `zdx automations validate` was run
- final response includes path + validation status (+ run status if tested)