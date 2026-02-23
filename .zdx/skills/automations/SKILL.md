---
name: automations
description: Create, edit, validate, and test ZDX automations stored in `$ZDX_HOME/automations/*.md`. Use when users ask to add or modify automation files, recurring jobs, scheduled prompts, or YAML-frontmatter automation definitions.
---

# Automations Skill

Create and maintain ZDX automation files.

## What is an Automation?

An automation is a headless agent that runs unattended — no human in the loop. It always produces a visible effect (a report, a message, a file, a PR). It must handle errors on its own: retry, degrade gracefully, or report what failed. Every automation is a single markdown file with YAML frontmatter and a prompt body.

## Contract (must follow)

- Keep automations global-only in `$ZDX_HOME/automations/` (usually `$HOME/.zdx/automations/`).
- Treat one file as one automation.
- Derive automation identity from file stem (no `id` field).
  - Example: `~/.zdx/automations/morning-report.md` → `morning-report`.
- Require markdown with YAML frontmatter delimited by `---`.
- Keep prompt body as non-empty markdown after frontmatter.

### Allowed frontmatter keys

- `schedule` (string, optional cron)
- `model` (string, optional)
- `timeout_secs` (int, optional, must be `> 0`)
- `max_retries` (int, optional, default `0`)

Do not add extra keys unless explicitly requested.

## Design Principles

### Keep prompts concise
Include only the instructions needed to execute the task.

### Keep prompts deterministic
Use explicit expected output shape (sections/bullets/constraints) so runs are easy to review.

### Keep scope tight
Only modify automation files and only the fields needed for the request.

### Always deliver a visible result
Every run must produce something the user can see — a thread entry, a message, a file. If the main output fails, produce a degraded result that explains what happened.

### Handle the empty state
Prompts must say what to do when there's nothing to report (e.g., "If no PRs are open, return: `No open PRs today.`"). Never produce a blank run.

### Chain skills for delivery
When external delivery is needed, reference specific skills/tools (e.g., `gog` for email, `wacli` for WhatsApp). Don't reinvent what a skill already does.

### Prefer staged execution over monolithic scripts
When prompt instructions involve external systems, prefer staged steps with clear checkpoints and fallback behavior.

### Define external delivery explicitly (when needed)
ZDX persists automation output to the automation run thread by default.

- Do **not** add destination boilerplate just to restate default thread persistence.
- Add a `Delivery` section only when external notification/delivery is requested or implied (email, WhatsApp, Telegram, Slack, file, PR, etc.).
- Treat verbs like "notify", "alert", "send me", "text me", "post" as implied external delivery.
- If delivery is implied but target/channel is unclear, ask one focused question before finalizing.
- Never add placeholders like `Secondary: none`.

When updating an existing automation, preserve its delivery behavior unless user asks to change it.

For detailed delivery patterns (Telegram topics, shell reliability, multi-channel fallback), see `references/delivery-patterns.md` in this skill directory.

## CLI Commands

| Command | Description |
|---------|-------------|
| `zdx automations list` | List discovered automations (name, source, schedule) |
| `zdx automations validate` | Validate all automation files (frontmatter + body) |
| `zdx automations run <name>` | Run one automation by file stem (manual trigger) |
| `zdx automations runs [name]` | Show run history (oldest first). Supports `--date`, `--date-start`, `--date-end`, `--json` |
| `zdx automations daemon` | Start the scheduled automations daemon (optional `--poll-interval-secs`, default 30) |

**Reading latest run output:** `zdx automations runs <name> | tail -1` to get the latest run, then use the thread ID to read results.

## Creation Process

### 1. Understand intent

- What effect should the automation produce? (report, action, artifact, notification)
- Is it scheduled or manual-only?
- Does it need external delivery, or is the default thread output enough?
- Which data sources or tools does it need?

### 2. Choose a pattern

| Pattern | When to use | Key trait |
|---------|-------------|-----------|
| Report | Summarize data on a schedule | Read-only, structured output |
| Action | Change something (create PR, update file) | Side effects, status reporting |
| Artifact | Generate/update a file or document | File path + validation in output |

### 3. Draft the automation

- Pick a kebab-case file name from user intent.
- Write minimal frontmatter (only set fields that differ from defaults).
- Write the prompt body following the structure below.
- Include explicit empty-state handling.
- Include failure policy.

### 4. Validate and dry-run

```bash
zdx automations validate
```

If user asks to test:

```bash
zdx automations run <name>
```

### 5. Iterate

Review output, tighten constraints, adjust model/timeout if needed.

### 6. Report

- Changed file path
- Validation status
- Test-run status (if executed)
- Delivery summary (if any)

## Writing Headless Prompts

Headless prompts differ from interactive ones: there's no human to ask for clarification. Every prompt must be self-contained.

### Prompt structure

Write prompts as executable runbooks with these sections:

1. **Goal**: one sentence describing the job.
2. **Inputs**: concrete data sources and assumptions.
3. **Execution steps**: ordered checklist; prefer staged tool calls.
4. **Output format**: exact sections/limits required in the result.
5. **Delivery** (optional): only when external send is required. See `references/delivery-patterns.md`.
6. **Empty state**: what to return when there's nothing to report.
7. **Failure policy**: what to do when things break.

### Error and fallback handling

Every headless prompt should address:

- **Source failures**: "If GitHub API is unreachable, skip that section and note `[GitHub unavailable]`."
- **Empty results**: "If no items match, return: `Nothing to report.`"
- **Delivery failures**: "If Telegram send fails, report the error; run output remains in the thread."
- **Partial data**: Decide up front — fail the whole run, or continue with what's available.

Default policy (use unless user specifies otherwise): continue with available data, clearly state what failed.

### Skill references

When automations need external tools, reference the correct skill:

- **Email**: use `gog` skill (Gmail)
- **WhatsApp**: use `wacli` skill
- **Web search**: use web search tool
- **Reminders**: use `apple-reminders` skill
- **Screenshots**: use `screenshot` skill

Don't hardcode API calls when a skill exists.

### Model selection guidance

- Default: omit `model` (uses system default).
- Long-context or complex reasoning: `model: "gemini-cli:gemini-2.5-flash"` or similar.
- Fast/cheap for simple tasks: `model: "stepfun:step-3.5-flash"` or `model: "mimo:mimo-v2-flash"`.
- Only set `model` when the default won't work well for the task.

### Style rules

- Use direct imperative instructions.
- Specify expected output shape (bullet points, sections, constraints).
- Include scope boundaries (what to include/exclude) when relevant.
- Avoid ambiguous goals like "improve everything".
- Use concrete verbs ("fetch", "summarize", "send", "save").

## Examples

See `references/examples.md` for full examples: daily thread summary (report, scheduled), PR drafter (action, manual), weekly email digest (report + delivery, scheduled).

## Templates

See `references/templates.md` for starter templates: scheduled automation and manual-only automation.

## Safety

- Do not create extra documentation files.
- Do not create automation files outside `$ZDX_HOME/automations/` unless explicitly requested.
- Prefer editing existing automation files over creating duplicates.

## Completion checklist

Before finishing, ensure:

- [ ] Automation file exists at `$ZDX_HOME/automations/<name>.md`
- [ ] Frontmatter is valid and minimal
- [ ] Body prompt is non-empty and specific
- [ ] Empty state is handled
- [ ] Failure policy is defined
- [ ] If external delivery is requested/implied: `Delivery` block includes target + policy + fallback
- [ ] If no external delivery: no destination/delivery boilerplate added
- [ ] `zdx automations validate` was run
- [ ] Final response includes file path, validation status, run status (if tested), and delivery summary (if any)
