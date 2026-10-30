---
name: jobs
description: Create, edit, validate, and test ZDX jobs stored in `$ZDX_HOME/jobs/*.md`. Use when the user asks to add or change a recurring job, a scheduled prompt, or a one-time job, or says things like "remind me on Monday at 9", "run this every morning", "schedule this once for tomorrow", or "add a job that…".
---

# Jobs Skill

Create and maintain ZDX job files.

## What is a job?

A job is a headless agent run with no human in the loop. It always produces a visible effect (a report, a message, a file, a PR) and handles errors on its own: retry, degrade gracefully, or report what failed. Every job is one markdown file with YAML frontmatter and a prompt body. It runs by hand, on a recurring cron schedule, or once at a set time.

## Contract (must follow)

- Keep jobs global-only in `$ZDX_HOME/jobs/` (usually `$HOME/.zdx/jobs/`). `jobs/archive/` holds finished once jobs; do not edit or add files there.
- One file is one job. Its name is the file stem (no `id` field): `~/.zdx/jobs/morning-report.md` → `morning-report`.
- Markdown with YAML frontmatter delimited by `---`, then a non-empty prompt body.
- Write a new or changed file to a temp file in the same directory, then rename it into place. The daemon reads `jobs/` on every tick, so a half-written file would be read as invalid.
- Edits apply on the next tick. No restart is needed.

### Allowed frontmatter keys

- `schedule` (string): 5-field cron (`minute hour day-of-month month day-of-week`) in the daemon's local time. Makes the job recurring.
- `run_at` (string): one instant in RFC 3339 **with an explicit offset**, for example `"2026-10-20T09:00:00-03:00"` or `"2026-10-20T12:00:00Z"`. Makes the job a once job. A value without an offset is rejected.
- `grace_secs` (int, 60 to 604800): how late a run may still start. Defaults: once 6 h, recurring 10 min. Not allowed on a manual job.
- `model` (string): `provider[@account]:model[@thinking][@fast]`.
- `subagent` (string): run with a named subagent profile.
- `root` (string): absolute path or `~/...` to run in another workspace.
- `timeout_secs` (int, > 0): hard limit for each tool call.
- `max_retries` (int, default 0): extra attempts after a failure, 1 min, 2 min, … apart.

Set `schedule` or `run_at`, never both. Neither means manual only. Do not add other keys; unknown keys make the file invalid.

### Once jobs

- Convert the user's time ("Monday 9am") into an absolute `run_at` with their UTC offset. State the date and offset you chose in your reply.
- After it runs (or is missed or interrupted), the file moves to `jobs/archive/<name>--<UTC instant>.md`. To run it again, write a new file or set a new `run_at`.
- If the daemon is down past the grace, the run is recorded as `missed` and is not run late.
- `zdx jobs run <name>` uses up the scheduled occurrence. Test with `zdx jobs run <name> --keep`, which leaves it scheduled.

### What runs can repeat

The scheduler starts each occurrence at most once, but a retry or a manual rerun runs the whole prompt again. For a prompt with a one-time side effect (send a message, post, open a PR), add a check first: "If today's report was already sent to <chat>, stop and say so."

## Design principles

- **Concise:** include only the instructions needed to do the task.
- **Deterministic:** give an explicit output shape (sections, bullets, limits) so runs are easy to review.
- **Tight scope:** change only job files and only the keys the request needs. Preserve delivery behavior when updating unless asked.
- **Always a visible result:** if the main output fails, produce a degraded result that says what happened.
- **Empty state:** say what to return when there is nothing to report ("If no PRs are open, return: `No open PRs today.`").
- **Reuse skills for delivery:** name the skill (`gog` for email, `wacli` for WhatsApp, the `telegram` tool for Telegram). Do not reinvent them.
- **Staged execution:** for external systems, prefer staged steps with checkpoints over one monolithic script.

### External delivery

The run output is always saved to the job's thread.

- Do not add destination boilerplate just to restate that.
- Add a `Delivery` section only when external delivery is requested or implied ("notify", "alert", "send me", "text me", "post").
- If delivery is implied but the target is unclear, ask one focused question before finalizing.
- Never add placeholders like `Secondary: none`.

For Telegram topics, shell reliability, and multi-channel fallback, see `references/delivery-patterns.md`.

## CLI commands

| Command | Description |
|---------|-------------|
| `zdx jobs list [--json]` | Jobs with schedule, status, next run, and last outcome. Invalid files are listed with their error |
| `zdx jobs list --daemon` | Same, with model and root resolved the way the scheduled daemon resolves them |
| `zdx jobs list --archived [--json]` | Finished once jobs in `jobs/archive/`, newest first, with their final outcome |
| `zdx jobs validate` | Check every job file; reports all problems |
| `zdx jobs run <name> [--keep]` | Run one job now. `--keep` tests a once job without using it up |
| `zdx jobs runs [name]` | Run history (`--date`, `--date-start`, `--date-end`, `--json`). Outcomes: `succeeded`, `failed`, `retrying`, `missed`, `interrupted` |
| `zdx jobs history <name> [--limit N] [--json]` | The job's recent runs (retries grouped), each with its thread and what it posted. Every run gets this list (last 10) in its runtime context, so a prompt can say "check what earlier runs sent" without extra setup |
| `zdx jobs set-model <name> (<spec>\|--clear)` | Change only the `model:` line |
| `zdx jobs daemon` | The scheduler (normally run by the `daemon` service) |

**Latest run output:** `zdx jobs runs <name> | tail -2`, then read the thread ID it shows.

**Status words** (`zdx jobs list`): `manual`, `scheduled`, `pending` (once, in the future), `due`, `expired` (past grace; will be recorded missed), `running`, `interrupted`, `finished`, `invalid`.

## Creation process

1. **Understand intent:** what effect (report, action, artifact, notification)? Manual, recurring, or once? External delivery or thread only? Which data sources or tools?
2. **Choose a pattern:**

   | Pattern | When to use | Key trait |
   |---------|-------------|-----------|
   | Report | Summarize data on a schedule | Read-only, structured output |
   | Action | Change something (PR, file) | Side effects, status reporting |
   | Artifact | Generate or update a file | File path + validation in output |
   | Reminder | One message at a set time | `run_at`, short prompt, delivery |

3. **Draft:** kebab-case file name; minimal frontmatter (only keys that differ from defaults); the prompt structure below; empty state; failure policy.
4. **Validate:** `zdx jobs validate`. If the user asks to test: `zdx jobs run <name>` (add `--keep` for a once job).
5. **Iterate:** tighten constraints; adjust model or timeout if needed.
6. **Report:** file path, schedule in plain words (for once jobs: the absolute date, time, and offset), validation status, test-run status, delivery summary.

## Writing headless prompts

There is no human to ask, so every prompt must be self-contained. Write it as a runbook:

1. **Goal**: one sentence.
2. **Inputs**: concrete data sources and assumptions.
3. **Execution steps**: ordered checklist; prefer staged tool calls.
4. **Output format**: exact sections and limits.
5. **Delivery** (optional): only for external send. See `references/delivery-patterns.md`.
6. **Empty state**: what to return when there is nothing to report.
7. **Failure policy**: what to do when things break.

Failure handling to cover: source failures ("If GitHub is unreachable, skip that section and note `[GitHub unavailable]`"), empty results, delivery failures ("report the error; the output stays in the thread"), and partial data. Default: continue with available data and state what failed.

### Model selection

- Default: omit `model` (uses the configured default).
- Long context or complex reasoning: a large model such as `gemini:gemini-3.1-pro-preview`.
- Fast and cheap for simple tasks: `stepfun:step-3.7-flash` or `xiaomi:mimo-v2.6-flash`.

### Style

Direct imperative instructions, concrete verbs ("fetch", "summarize", "send", "save"), explicit output shape, clear scope boundaries. Avoid goals like "improve everything".

## Examples and templates

- `references/examples.md`: daily thread summary (recurring report), PR drafter (manual action), weekly email digest (recurring report + delivery).
- `references/templates.md`: recurring, once, and manual-only starters.

## Safety

- Do not create extra documentation files.
- Do not create job files outside `$ZDX_HOME/jobs/` unless explicitly requested.
- Prefer editing an existing job over creating a duplicate.
- Do not edit `$ZDX_HOME/state/jobs/` or `jobs_runs.jsonl` by hand.

## Completion checklist

- [ ] Job file exists at `$ZDX_HOME/jobs/<name>.md`, written by temp file + rename
- [ ] Frontmatter is valid and minimal; `schedule` or `run_at` (with offset), not both
- [ ] Body prompt is non-empty and specific, with empty state and failure policy
- [ ] One-time side effects are guarded against a rerun
- [ ] External delivery, if requested or implied, has target + policy + fallback; otherwise no delivery boilerplate
- [ ] `zdx jobs validate` was run
- [ ] Final response gives the file path, the schedule in plain words, validation status, run status (if tested), and delivery summary (if any)
