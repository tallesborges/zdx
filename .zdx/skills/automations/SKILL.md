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

### Prefer staged execution over monolithic scripts

When prompt instructions involve external systems, prefer staged steps with clear checkpoints and fallback behavior.

### Define external delivery explicitly (when needed)

ZDX already persists automation output to the automation run thread by default (runtime-managed, typically `automation-<name>-<YYYYMMDD-HHMM>`).

- Do **not** add destination boilerplate just to restate default thread persistence.
- Add a `Delivery` section only when external notification/delivery is requested or implied (email, WhatsApp, Telegram, Slack, file, PR, etc.).
- Treat verbs like "notify", "alert", "send me", "text me", "post" as implied external delivery.
- If delivery is implied but target/channel is unclear, ask one focused question before finalizing.
- Never add placeholders like `Secondary: none`.

When updating an existing automation, preserve its delivery behavior unless user asks to change it.

## Workflow

1. Infer automation file name from user intent (kebab-case).
2. Create or edit `$ZDX_HOME/automations/<name>.md`.
3. If file exists and user did not request replacement, edit in place.
4. Decide whether external delivery is required:
   - if not requested/implied: omit destination/delivery boilerplate
   - if requested/implied: add explicit `Delivery` block with channel + target + routing policy + fallback behavior
5. Keep frontmatter minimal and valid.
6. If delivery target/tooling is unclear (or implied but ambiguous), ask one focused clarification question.
   - channel/provider
   - recipient/target ID
   - topic/thread policy (reuse existing vs create new per run)
   - failure policy (fail run vs continue with partial result)
   - example: `You said "notify me" — should I send via Telegram, WhatsApp, or email, and to which target?`
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
   - delivery summary (if any)

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

### LLM-run automation prompt style (recommended default)

Write prompts as executable runbooks with explicit sections in this order:

1. **Goal**: one sentence describing the job.
2. **Inputs**: concrete data sources and assumptions.
3. **Execution steps**: ordered checklist; prefer staged tool calls.
4. **Output contract**: exact format/sections required in the result.
5. **Delivery (optional)**: only when external send is required; specify channel/target/policy/fallback.
6. **Failure policy**: fail-fast vs partial result + error reporting rules.

Use imperative language and concrete verbs ("fetch", "summarize", "send", "save").
Avoid vague verbs ("improve", "analyze deeply") unless constraints are explicit.

### Output contract style

Always define an output shape with strict headings/bullets, but tailor it to the automation type.

Recommended patterns:

- **Report automation**: sections + concise bullets.
- **Action automation**: what was changed, where, status, follow-up actions.
- **Delivery automation**: delivery status per channel + failure details.
- **Artifact automation**: file paths generated/updated + summary + validation result.

Generic template:

```md
## Output Format

Return exactly:
1. Summary
2. Results (type-specific details)
3. Errors/Warnings (or `None`)
4. Next Step (or `None`)
```

If output must be short, include explicit limits (`max bullets`, `max chars`, `no tables`).

### Delivery reliability patterns

- Prefer first-party tooling/skills over raw API calls when available.
- For multi-step delivery flows, explicitly capture and reuse IDs from prior steps.
- If one data source fails, continue with available sources unless user requested strict failure.

### Telegram-specific guidance (when requested)

- Prefer dedicated CLI/tool wrappers over raw `curl` when available.
- If topic routing is required, state policy explicitly:
  - reuse existing thread/topic ID, or
  - create a new topic per run.
- If creating topics per run, include deterministic naming guidance (for example: `Morning Report - YYYY-MM-DD HH:MM`).
- Require clear fallback: if topic creation fails, report the error clearly (full run output remains in the automation thread by default).

### Shell reliability guidance

- For multiline content, prefer heredocs over fragile quoting.
- Do not assume shell variables persist across separate tool invocations unless explicitly preserved.

### Delivery block (only when external delivery is required)

Use an explicit `Delivery` block only when external notification/delivery is requested or implied.

```md
## Delivery

- Channel/provider: <telegram | whatsapp | email | slack | ...>
- Target: <chat id / phone / email / webhook / file path>
- Topic/thread policy: <reuse-existing | create-per-run> (if applicable)
- If delivery fails: report the delivery error clearly; run output remains in the automation thread.
```

### Prompt skeleton (copy/paste)

```md
# Goal
<one-sentence goal>

# Inputs
- <source 1>
- <source 2>
- Assumptions: <explicit assumptions>

# Execution Steps
1. <step 1>
2. <step 2>
3. <step 3>

# Output Format
- <required sections / limits>

## Delivery (optional; only if requested/known)
- Channel/provider: <channel>
- Target: <recipient/target>
- Topic/thread policy: <reuse-existing | create-per-run> (if applicable)
- If delivery fails: report error clearly.

# Failure Policy
- If a non-critical source fails, continue with available data and state what failed.
- If delivery fails, report delivery error and continue returning the run output.
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
- if external delivery is requested/implied: `Delivery` block includes target + policy + fallback behavior
- if no external delivery: no destination/delivery boilerplate is added
- `zdx automations validate` was run
- final response includes file path, validation status, run status (if tested), and delivery summary (if any)