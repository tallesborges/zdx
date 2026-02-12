---
name: automations
description: Create, edit, validate, and test ZDX automations stored in `$ZDX_HOME/automations/*.md`. Use when users ask to add or modify automation files, recurring jobs, scheduled prompts, or YAML-frontmatter automation definitions.
---

# Automations Skill

Create and maintain ZDX automation files.

## What is an Automation?

An automation is a headless agent that runs unattended — no human in the loop. It always produces a visible effect (a report, a message, a file, a PR). It must handle errors on its own: retry, degrade gracefully, or report what failed. Every automation is a single markdown file with YAML frontmatter and a prompt body.

## Contract (must follow)

- Keep automations global-only in `$ZDX_HOME/automations/` (usually `~/.zdx/automations/`).
- Treat one file as one automation.
- Derive automation identity from file stem (no `id` field).
  - Example: `~/.zdx/automations/morning-report.md` → `morning-report`.
- Require markdown with YAML frontmatter delimited by `---`.
- Keep prompt body as non-empty markdown after frontmatter.
- Never use deprecated `enabled`.

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

### Daily thread summary (report, scheduled)

```md
---
schedule: "0 18 * * 1-5"
timeout_secs: 300
---

# Goal
Summarize today's ZDX conversation threads into a concise daily digest.

# Inputs
- All threads modified today (use thread search).
- Assumptions: "today" means the current calendar date in local time.

# Execution Steps
1. Search for threads modified today.
2. For each thread (max 10 most recent), extract: title, key decisions, open questions.
3. Group by topic if patterns emerge; otherwise list chronologically.

# Output Format
Return exactly:
- **Date**: YYYY-MM-DD
- **Threads Reviewed**: count
- **Summary**: one bullet per thread (title + one-line takeaway)
- **Open Questions**: collected list, or `None`

Max 30 bullets total. No tables.

# Empty State
If no threads were modified today, return:
`No threads modified today.`

# Failure Policy
If thread search fails, report the error and return `Thread search unavailable.`
```

### PR drafter (action, manual)

```md
---
timeout_secs: 600
max_retries: 1
---

# Goal
Draft a pull request for the current branch's uncommitted and committed-but-unpushed changes.

# Inputs
- Current git branch and its diff against `main`.
- Recent commit messages on the branch.
- Assumptions: the repo has a remote named `origin` and a `main` branch.

# Execution Steps
1. Run `git diff main...HEAD` and `git diff` to collect all changes.
2. Run `git log main..HEAD --oneline` for commit messages.
3. Summarize the changes: what was added, modified, removed.
4. Draft a PR title (conventional commit style, max 72 chars).
5. Draft a PR body with: Summary, Changes (bulleted), Testing Notes.

# Output Format
Return exactly:
- **PR Title**: single line
- **PR Body**: markdown with Summary, Changes, Testing Notes sections

# Empty State
If there are no changes vs main, return:
`No changes found between current branch and main.`

# Failure Policy
If git commands fail, report the exact error and stop.
```

### Weekly email digest (report + delivery, scheduled)

```md
---
schedule: "0 9 * * 1"
model: "gemini-cli:gemini-2.5-flash"
timeout_secs: 600
---

# Goal
Send a weekly summary of completed reminders and upcoming deadlines via email.

# Inputs
- Completed reminders from the past 7 days (use `apple-reminders` skill).
- Upcoming reminders due in the next 7 days.

# Execution Steps
1. Fetch completed reminders from the last 7 days.
2. Fetch upcoming reminders due within the next 7 days.
3. Compile into a digest with two sections.
4. Send via email using `gog` skill.

# Output Format
- **Week**: date range
- **Completed**: bulleted list (or `None`)
- **Upcoming**: bulleted list with due dates (or `None`)
- **Delivery Status**: sent / failed + error

# Empty State
If no completed and no upcoming reminders, send email with:
`Nothing to report this week. No completed or upcoming reminders.`

# Delivery
- Channel: email (use `gog` skill)
- Target: user's primary Gmail
- If delivery fails: report error clearly; digest remains in automation thread.

# Failure Policy
- If reminders fetch fails, report error and skip that section.
- If email send fails, report delivery error; run output remains in thread.
```

## Templates

### Scheduled automation

```md
---
schedule: "0 8 * * *"
# model: "gemini-cli:gemini-2.5-flash"
# timeout_secs: 900
# max_retries: 1
---

# Goal
<one-sentence goal>

# Inputs
- <source 1>
- <source 2>
- Assumptions: <explicit assumptions>

# Execution Steps
1. <step>
2. <step>

# Output Format
- <required sections / limits>

# Empty State
If <nothing to report condition>: `<short message>.`

# Failure Policy
- If a non-critical source fails, continue with available data and state what failed.
```

### Manual-only automation

```md
---
# model: "gemini-cli:gemini-2.5-flash"
# timeout_secs: 900
---

# Goal
<one-sentence goal>

# Inputs
- <source>

# Execution Steps
1. <step>

# Output Format
- <required sections / limits>

# Empty State
<what to return when nothing to do>

# Failure Policy
- <error handling rules>
```

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
