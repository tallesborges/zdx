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

### Always define output destination

Every automation prompt must state where results go.

- Default destination: persistent automation thread `automation-<name>`.
- Optional delivery destination: email, WhatsApp, Telegram, Slack, file, PR, etc.
- If destination is missing, ask one focused question before finalizing.

When updating an existing automation, preserve its destination rules unless user asks to change them.

## Workflow

1. Infer automation file name from user intent (kebab-case).
2. Create or edit `$ZDX_HOME/automations/<name>.md`.
3. If file exists and user did not request replacement, edit in place.
4. Define destination behavior in the prompt body:
   - always include default thread destination (`automation-<name>`)
   - include delivery target(s) requested by user (email/WhatsApp/etc.)
5. Keep frontmatter minimal and valid.
6. If delivery target/tooling is unclear, ask one focused clarification question.
7. For channel-specific delivery, instruct the automation to use the relevant skill/tooling when available (for example: `gog` for Google email, `wacli` for WhatsApp).
8. Run validation:
   ```bash
   zdx automations validate
   ```

9. If user asks to test, run:

   ```bash
   zdx automations run <name>
   ```

10. Report:
   - changed file path
   - validation status
   - test-run status (if executed)
   - destination summary

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

## Destination

- Primary: save full result in thread `automation-<name>`.
- Secondary: <optional channel + target>
```

### Manual-only automation template

```md
---
# model: "gemini-cli:gemini-2.5-flash"
# timeout_secs: 900
# max_retries: 0
---

<clear prompt for what should run>

## Destination

- Primary: save full result in thread `automation-<name>`.
- Secondary: <optional channel + target>
```

## Writing guidance for prompt body

- Use direct imperative instructions.
- Specify expected output shape (bullet points, sections, constraints).
- Keep prompt concise and task-focused.
- Include scope boundaries (what to include/exclude) when relevant.
- Avoid ambiguous goals like "improve everything".

### Destination block (include in prompt body)

Use an explicit destination block in automation prompts:

```md
## Destination

- Primary: save full result in thread `automation-<name>`.
- Secondary: send concise summary to <channel> (<recipient/target>).
- If channel send fails, keep result in thread and report the error clearly.
```

## Safety

- Do not create extra documentation files.
- Do not create automation files outside `$ZDX_HOME/automations/` unless explicitly requested.
- Prefer editing existing automation files over creating duplicates.

## Completion checklist

Before finishing, ensure:

- automation file exists at `$ZDX_HOME/automations/<name>.md`
- frontmatter is valid and minimal
- body prompt is non-empty and specific
- destination behavior is explicitly documented in the prompt body
- `zdx automations validate` was run
- final response includes file path, validation status, run status (if tested), and destination summary