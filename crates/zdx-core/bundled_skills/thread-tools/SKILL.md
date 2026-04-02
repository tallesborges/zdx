---
name: thread-tools
description: Investigate tool usage across saved ZDX threads. Use when the user asks which threads used a specific tool, where a tool failed, what arguments were passed, or wants a quick audit of tool-call patterns. Prefer the `zdx threads tools` CLI before opening full thread transcripts.
---

# Thread Tools

Use the dedicated CLI first:

- `zdx threads tools <tool>`
- `zdx threads tools <tool> --failed`
- `zdx threads tools --date YYYY-MM-DD`
- `zdx threads tools <tool> --date-start YYYY-MM-DD --date-end YYYY-MM-DD --json`

## Workflow

1. Start with `zdx threads tools`.
2. Use text output for quick inspection.
3. Use `--json` when you need exact parsing, counts, or to feed results into a report.
4. Only after that, open the most relevant threads with `read_thread` or `zdx threads show <id>`.

## What the command gives you

- thread ID
- thread title
- tool name
- tool timestamp
- status (`ok`, `failed`, or `pending`)
- compact argument summary
- error code/message when available

## Good defaults

- For “where did I use X?” → `zdx threads tools X`
- For “where did X fail?” → `zdx threads tools X --failed`
- For “what happened today?” → `zdx threads tools --date YYYY-MM-DD`

## Follow-up behavior

If the user wants deeper diagnosis:

- pick the top 3–5 matching thread IDs
- inspect those threads only
- summarize repeated argument patterns, failure causes, and likely fixes