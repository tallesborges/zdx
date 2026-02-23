# Automation Examples

## Daily thread summary (report, scheduled)

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

## PR drafter (action, manual)

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

## Weekly email digest (report + delivery, scheduled)

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
