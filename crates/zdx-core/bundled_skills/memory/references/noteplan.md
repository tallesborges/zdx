# NotePlan reference

Load this reference when you need deeper conventions for editing or creating notes under `$ZDX_MEMORY_ROOT`.

## Layout

- Regular notes live in `$ZDX_MEMORY_ROOT/Notes`
- Calendar notes live in `$ZDX_MEMORY_ROOT/Calendar`
- The memory index lives at `$ZDX_MEMORY_ROOT/Notes/MEMORY.md`
- `$ZDX_MEMORY_ROOT` itself is a container directory; do not create note files directly under the root
- Notes are Markdown files; preserve formatting

## Regular note conventions

- Start new notes with `# Title`
- The first non-empty line is the display title unless frontmatter overrides it
- When practical, keep the `# Title` aligned with the filename (without `.md`)
  - Example: `2025-08-17 Natal.md` → `# 2025-08-17 Natal`
- Before creating a note, inspect nearby notes to match local naming patterns

Common naming patterns:

- `YYYY-MM-DD Name` — date-scoped notes
- `YYYY-MM Name` — month-scoped notes
- no date prefix — evergreen/reference notes
- `xx.00 Name` — area home/index note where that pattern already exists

## Calendar note filenames

All calendar periods live together under `$ZDX_MEMORY_ROOT/Calendar`.

| Period | Format | Example |
|---|---|---|
| Daily | `YYYYMMDD.md` | `20260304.md` |
| Weekly | `YYYY-Www.md` | `2026-W10.md` |
| Monthly | `YYYY-MM.md` | `2026-03.md` |
| Quarterly | `YYYY-Qq.md` | `2026-Q1.md` |
| Yearly | `YYYY.md` | `2026.md` |

Useful shell helpers:

```text
bash command:"date +%Y%m%d"
bash command:"date +%G-W%V"
bash command:"date +%Y-%m"
```

## Tasks

Tasks are plain text lines in Markdown files.

### States

- Open: `* [ ] Task`
- Done: `* [x] Task`
- Cancelled: `* [-] Task`
- Rescheduled: `* [>] Task`
- Some notes may use bare `* Task` or `- Task`; follow the local pattern when editing existing content

### Dates

- Schedule tag: `>YYYY-MM-DD`
- Done stamp: `@done(YYYY-MM-DD)`
- Done timestamp: `@done(YYYY-MM-DD HH:MM)`

Example:

```text
* [ ] Buy #groceries for @alex >2026-04-08
```

When editing tasks:

- Use `* [ ]` for new tasks unless the surrounding note uses another active-task style
- Use `>YYYY-MM-DD` to schedule tasks
- Use `* [x]` when completing a task and append `@done(...)` when that context is useful

## Links, tags, and mentions

- Regular note link: `[[Note Title]]`
- Daily note link: `[[YYYY-MM-DD]]`
- Heading link: `[[Note Title#Heading]]`
- Hashtag: `#tag-name`
- Mention: `@person`

Notes:

- `>YYYY-MM-DD` inside a regular note acts as a date tag/schedule link
- `<YYYY-MM-DD` may appear in daily notes as a backlink from a rescheduled task
- Standard markdown same-note anchors are not the NotePlan convention; use headings and NotePlan-style links where needed

## Attachments

- Each note can have a sibling attachments folder named `{note}_attachments/`
- For a regular note `My Trip.md`, use `My Trip_attachments/`
- For a daily note `20260120.md`, use `20260120_attachments/`
- Use Markdown relative links for attachments
  - `![image](Note%20Name_attachments/filename.png)`
  - `![file](Note%20Name_attachments/document.pdf)`
- Do not edit binary attachments directly

If you need to add an attachment:

1. Create the attachment folder if needed
2. Put the file in that folder
3. Add the relative Markdown link to the note

Example helper:

```text
bash command:"mkdir -p \"$ZDX_MEMORY_ROOT/Notes/My Note_attachments\""
```

## Editing heuristics

- Prefer updating an existing note over creating a new one
- Match the local naming pattern in the surrounding folder
- For large reorganizations or mass renames, propose a plan before making changes