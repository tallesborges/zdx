---
name: memory
description: Instructions for working with the user's memory notes. For memory-related tasks, read this file first, then use normal tools such as `read`, `grep`, `glob`, and `write`.
---

# Memory

A markdown-based memory system following [NotePlan conventions](https://noteplan.co). Fully compatible with NotePlan, Obsidian, or any markdown editor.

## Paths

- **Notes root:** `$ZDX_MEMORY_NOTES_DIR`
- **Daily notes:** `$ZDX_MEMORY_DAILY_DIR`
- **Memory index:** `$ZDX_HOME/MEMORY.md`

There is no dedicated `memory` tool. Use the normal file tools with these paths.

Use these env vars directly in tool arguments. Example patterns:

- `read path:"$ZDX_HOME/MEMORY.md"`
- `grep path:"$ZDX_MEMORY_NOTES_DIR" glob:"*.md" pattern:"..."`
- `grep path:"$ZDX_MEMORY_DAILY_DIR" glob:"*.md" pattern:"..."`

Default to ignoring `@Archive/` and `@Trash/` unless the user asks.

## Suggesting saves

When the system prompt enables memory suggestions (`MAY suggest saving`), follow these rules:

- Suggest only clearly noteworthy items: decisions, preferences, personal facts, useful links, learnings, recurring patterns.
- Format: one line at the end of the response — `💡 Want me to save [specific item] to [specific note]?`
- At most once per response; only when the item is genuinely useful later.
- If the user says yes, save immediately: write full detail to the target memory note first, then optionally promote to `MEMORY.md` per the memory index policy.
- If the user says no or ignores it, move on — do not repeat the same suggestion.
- If the user explicitly says "remember X", save immediately without asking first.

## Core workflow

- Use the embedded `<memory_index>` block as the routing layer.
- Load only the specific note(s) needed for the task.
- Use the normal file tools (for example `read`, `grep`, and `glob`) to inspect memory files.

### When to consult memory

- For factual questions about the user or something they own or manage — such as belongings, relationships, documents, preferences, work, trips, history, or already-documented projects — MUST consult the embedded memory index and relevant memory notes before answering from general knowledge or asking for more context.
- If the answer is more likely to live in a connected live system, SHOULD use the corresponding skill instead of memory (for example Google Calendar/Gmail/Contacts via `gog`, Apple Reminders, or WhatsApp).

### Memory index rules

- Keep the memory index concise — only core facts and pointers.
- Treat the memory index as a high-signal index, not a general knowledge dump.
- Prefer saving new information in the right note first.
- Promote to the memory index only when it should act as a frequent shortcut or durable pointer.
- Do not add occasional reference material, study notes, cheatsheets, or other one-off content unless explicitly requested.
- When notes are added, renamed, or removed, update the memory index.

## When to use proactively

- For memory-related tasks — including when the user explicitly asks about notes or memory, or asks factual questions about something they own/manage that may already exist in notes or daily/calendar files — read this file first, then use normal tools.
- Search both notes and daily/calendar paths unless the user clearly scopes the search.
- If memory answers the question, respond directly. Ask a follow-up only when memory is missing, ambiguous, or clearly not the right source.

## Memory vs live systems

- Use memory for documented notes, saved files, plans, history, and personal reference material.
- Use the corresponding live-system skill/tool when freshness or current state matters, or when the source is clearly external:
  - `gog` for Google Calendar, Gmail, Contacts, Drive, Docs, and Sheets
  - `apple-reminders` for Apple Reminders
  - `wacli` for WhatsApp
- If the user asks about a clearly current live item (for example today's Google Calendar events), prefer the live system over memory.

## Memory index policy (`MEMORY.md`)

Use this policy whenever the user asks to save memory (explicitly or by confirmation after a suggestion).

- `MEMORY.md` is an index/routing layer, not the full memory store.
- Full detail belongs in memory notes (notes or daily), not in `MEMORY.md`.

### Save flow (note-first)

1. Save the complete content in the best target note.
2. Decide whether to promote to `MEMORY.md`.
3. If promoting, add or update one concise pointer in `MEMORY.md`.

### Source thread references

- When saving technical/project memory that may need later review, prefer including a `Context thread:` or `Discussion thread:` reference.
- Add a thread reference when the future reader may want to reopen the original conversation for:
  - reasoning
  - tradeoffs
  - implementation context
  - examples or alternatives considered
- Typical good fits:
  - feature ideas
  - bug investigations
  - design decisions
  - implementation notes
- Usually skip thread references for simple durable facts where the original conversation is unlikely to matter later:
  - personal facts
  - stable preferences
  - short factual references
- Heuristic: if future-you would likely ask “why/how did we decide this?”, include the thread ID.

### Promote vs note-only

- Promote to `MEMORY.md` when information is durable/reusable:
  - stable preferences
  - key personal facts
  - long-lived project decisions
  - recurring patterns
- Keep note-only (do not index) for transient items:
  - one-off status updates
  - temporary blockers
  - ad-hoc links that are unlikely to be reused

### Compaction rules

- Upsert/merge existing pointers; avoid append-only duplicates.
- Keep pointers short (one line when possible) and high-signal.
- Avoid long narrative text in `MEMORY.md`.
- Preserve section structure and readability.

## Calendar notes

NotePlan supports period-based notes in the daily/calendar path. All live in the same configured daily path.

### Filename formats

| Period | Format | Example |
|--------|--------|---------|
| Daily | `YYYYMMDD.md` | `20260304.md` |
| Weekly | `YYYY-Www.md` | `2026-W10.md` |
| Monthly | `YYYY-MM.md` | `2026-03.md` |
| Quarterly | `YYYY-Qq.md` | `2026-Q1.md` |
| Yearly | `YYYY.md` | `2026.md` |

### Usage

- Use `date +%Y%m%d` to get today's daily filename.
- Use `date +%G-W%V` to get the current ISO week filename.
- Use `date +%Y-%m` to get the current monthly filename.
- Weekly/monthly/quarterly/yearly notes are great for summaries, plans, and reviews.
- All period notes follow the same markdown formatting as regular notes.

## File types

- Notes are `.md`. Preserve formatting.
- Attachments live in `*_attachments/` folders. Do not edit binary files.

## Attachments & images

- Each note can have an attachments folder named: `{note_filename_without_extension}_attachments/`
- For regular notes: folder sits next to the `.md` file in the same directory.
  - Example: note `My Trip.md` → attachments in `My Trip_attachments/`
- For daily notes: folder sits next to the daily note.
  - Example: `20260120.md` → `20260120_attachments/`
- To embed an image/file in a note, use markdown image syntax with URL-encoded relative path:
  - `![image](Note%20Name_attachments/filename.png)`
  - `![file](Note%20Name_attachments/document.pdf)`
- To save an image/file to a note:
  1. Create the `_attachments` folder if it doesn't exist: `mkdir -p "{note}_attachments/"`
  2. Copy/download the file into that folder
  3. Add the `![image](...)` reference in the note body
- Common attachment filenames follow the pattern: `CleanShot YYYY-MM-DD at HH.MM.SS@2x.png` (screenshots) or descriptive names.

## Organization principles (Johnny Decimal-lite)

- Top-level areas:
  - `10-19 Life Admin`
  - `20-29 Development & Tech`
  - `30-39 Work`
- Do not move or merge areas without explicit confirmation.
- Prefer **fewer notes**. Create a new note only when the content needs clear separation for later retrieval.
- Default to **append/merge** into existing notes when possible.
- Avoid deep nesting. Prefer Johnny Decimal-style flat folders (e.g., `11.01`, `11.02`) to keep a single-level view.
- Do not invent new Johnny Decimal codes or placeholder suffixes (e.g., `25.xx`). Use existing folders only; if a new folder seems needed, propose the exact name and wait for approval.
- When organizing, propose a small, minimal change set first.

## 10-19 Life Admin outline (current template)

**⚠️ IMPORTANT: All numbered folders (11.xx through 15.xx) live FLAT inside `10-19 Life Admin/`.** There are NO separate top-level folders like `15-19 Lifestyle & Travel/`. The section headings below (Personal, Home, Money, Tech, Lifestyle & travel) are just logical groupings for readability — they do NOT correspond to filesystem folders. Always create notes inside the notes root under `10-19 Life Admin/<folder>/`.

**Before creating a note, ALWAYS verify the target folder exists:**
```bash
ls "$ZDX_MEMORY_NOTES_DIR/10-19 Life Admin/" | grep "15.41"
```

Use this outline when filing in `10-19 Life Admin`. Group labels below are for readability; the bullet items are the actual folder names. Items with a leading `■` are section markers only—do not create notes inside them.

### Personal (11.xx folders)

- `11.00 ■ System management`
- `11.01 Inbox 📥`
- `11.02 System manual 📙`
- `11.09 Archive 📦`
- `11.10 ■ Personal records 🗂️`
- `11.11 Birth certificate & proof of name`
- `11.12 Passports, residency, & citizenship`
- `11.13 Identity cards`
- `11.14 Licenses`
- `11.15 Voter registration & elections`
- `11.16 Legal documents & certificates`
- `11.17 Academic records & qualifications`
- `11.20 ■ Physical health & wellbeing 🫀`
- `11.21 Health insurance & claims`
- `11.22 Health records & registrations`
- `11.23 Primary care`
- `11.24 Eyes, ears, & teeth`
- `11.25 Immunity`
- `11.26 Physical therapy`
- `11.27 Fitness, nutrition, sleep, & other pro-active wellbeing`
- `11.28 Reproductive health`
- `11.29 Surgical & specialist care`
- `11.30 ■ Mental health & wellbeing 🧠`
- `11.31 Psychologist, psychiatrist, & counselling`
- `11.32 My thoughts, journalling, diaries, & other writing`
- `11.33 Spiritual`
- `11.34 Habits, routines, & planning`
- `11.35 Brain training`
- `11.40 ■ Family 💑`
- `11.41 My partner`
- `11.42 My kids`
- `11.43 My family`
- `11.44 Dating & relationships`
- `11.45 Celebrations & gifting`
- `11.46 Letters, cards, & mementos`
- `11.50 ■ Friends, clubs, & organisations 🏑`
- `11.51 My friends`
- `11.52 Groups, clubs, & volunteering`
- `11.53 Official correspondence`
- `11.60 ■ Pets & other animals 🐓`
- `11.61 Pet health insurance & claims`
- `11.62 Pet health records & registrations`
- `11.70 ■ My brilliant career 🧑‍🍳`
- `11.71 My sales pitch`
- `11.72 My jobs past, present, & future`
- `11.73 My side-hustles`
- `11.80 ■ Personal development & who I am 📚`
- `11.81 Goals & dreams`
- `11.82 Hobbies & learning`
- `11.83 My library`

### Home (12.xx folders)

- `12.00 ■ System management`
- `12.01 Inbox 📥`
- `12.09 Archive 📦`
- `12.10 ■ Home records 📄`
- `12.11 Official documents`
- `12.12 Home insurance & claims`
- `12.13 Moving`
- `12.14 Inventory`
- `12.15 My home's user manual`
- `12.16 Appliances, tools, & gadgets`
- `12.17 Rates, taxes, & fees`
- `12.20 ■ Home services & health 🛠️`
- `12.21 Electricity, gas, & water`
- `12.22 Internet, phone, TV, & cable`
- `12.23 All other utilities & services`
- `12.24 Repairs, maintenance, & upkeep`
- `12.25 Renovation & improvements`
- `12.26 Cleaning Services & housekeeping`
- `12.30 ■ Getting around 🚲`
- `12.31 Motor vehicle purchase, leasing, & rental`
- `12.33 Mechanics, repairs, & maintenance`
- `12.34 Permits & tolls`
- `12.35 Bicycles & scooters`
- `12.36 Public transport`
- `12.40 ■ My kitchen & garden 🪴`
- `12.41 Indoor plants`
- `12.42 Outdoor plants`
- `12.43 Growing herbs, vegetables, & fruit`
- `12.50 ■ Housemates, neighbours, & the neighbourhood ☕️`
- `12.51 Housemates`
- `12.52 Neighbours`
- `12.53 The neighbourhood`

### Money (13.xx folders)

- `13.00 ■ System management`
- `13.01 Inbox 📥`
- `13.09 Archive 📦`
- `13.10 ■ Earned 🤑`
- `13.11 Payslips, invoices, & remittance`
- `13.12 Expenses & claims`
- `13.13 Government services`
- `13.14 Gifts, prizes, inheritance, & windfalls`
- `13.15 Selling my stuff`
- `13.20 ■ Saved 📈`
- `13.21 Budgets & planning`
- `13.22 Bank accounts`
- `13.23 Investments & assets`
- `13.24 Pension`
- `13.30 ■ Owed 💸`
- `13.31 Credit cards`
- `13.32 Mortgage`
- `13.33 Personal loans`
- `13.34 Tax returns & accounting`
- `13.40 ■ Spent & sent 🛍️`
- `13.41 Purchase receipts`
- `13.43 Payment services`
- `13.44 Money transfer services`
- `13.50 ■ Financial administration 📔`
- `13.51 My credit rating`

### Tech (14.xx folders)

- `14.00 ■ System management`
- `14.01 Inbox 📥`
- `14.09 Archive 📦`
- `14.10 ■ Computers & other devices 🖥️`
- `14.11 My computers & servers`
- `14.12 My mobile devices`
- `14.13 My wi-fi & network devices`
- `14.14 My data storage & backups`
- `14.20 ■ Software & accounts 🔑`
- `14.21 My emergency recovery kit 🚨`
- `14.22 Software, licenses, & downloads`
- `14.23 Email accounts`
- `14.24 Social media accounts`
- `14.25 Domains & hosting`
- `14.26 All other accounts`
- `14.30 ■ My online presence 🌏`
- `14.31 My blog`

### Lifestyle & travel (15.xx folders)

- `15.00 ■ System management`
- `15.01 Inbox 📥`
- `15.09 Archive 📦`
- `15.10 ■ Inspiration & history 💭`
- `15.11 Places I've been, or want to go`
- `15.12 Places I'd like to eat or drink`
- `15.20 ■ Administration & checklists ✅`
- `15.21 Important documents & lists`
- `15.22 Going-away checklists`
- `15.24 Loyalty programs`
- `15.30 ■ Events 🍿`
- `15.31 Eating out`
- `15.32 Music`
- `15.33 Movies`
- `15.34 The arts`
- `15.35 Sport`
- `15.36 Fairs & shows`
- `15.37 Conferences & expos`
- `15.40 ■ Short or routine trips 🚉`
- `15.41 All short trips`
- `15.50 ■ Longer trips 🛫`
- `15.51 Longer trips from the past`
- `15.52 Lucerne 2025-03`

## Task syntax (NotePlan conventions)

Tasks are plain-text lines in markdown files.

### Task states

- `* Buy groceries` or `- Buy groceries` — open/active task (configurable prefix)
- `* [x] Buy groceries` — completed/done
- `* [-] Buy groceries` — cancelled
- `* [>] Buy groceries` — rescheduled/postponed
- A bare `-` or `*` without checkbox brackets is an active task (depending on user config)

### Date tags & scheduling

- `>YYYY-MM-DD` — schedule/date tag (in regular notes, links task to that day)
- `@done(YYYY-MM-DD)` — completion date stamp
- `@done(YYYY-MM-DD HH:MM)` — completion timestamp

### Links between notes

- `[[note title]]` — link to a regular note
- `[[YYYY-MM-DD]]` — link to a daily note
- `[[Note Title#Heading]]` — link to a specific heading inside another note
- `>YYYY-MM-DD` in regular notes — schedule/link task to that day
- `<YYYY-MM-DD` in daily notes — back-link to source of rescheduled task
- **No same-note anchor links** — standard markdown `[text](#heading)` does NOT work in NotePlan. Use `##` headings for structure.

### Tags & mentions

- `#tag-name` — hashtag for categorization
- `@person` — mention
- `>YYYY-MM-DD` — date tag (in regular notes only)

### Example task with all elements

```
* [ ] Buy #groceries for @alex >2021-04-24
```

### When creating/editing tasks

- Use `* [ ]` for new open tasks (preferred default)
- Use `>YYYY-MM-DD` to schedule tasks to specific dates
- Mark done with `* [x]` and optionally append `@done(YYYY-MM-DD)`
- Cancel with `* [-]`
- Reschedule with `* [>]` and add the target `>YYYY-MM-DD`

## Note conventions (NotePlan-compatible)

- The first non-empty line of a note is used as the display title (unless a `title:` frontmatter property is set). The `# H1` header effectively becomes the note title. Always ensure the first line (`# Title`) matches the filename (without `.md`).
  - Example: file `2025-08-17 Natal.md` → first line `# 2025-08-17 Natal`
  - Ref: https://help.noteplan.co/article/237-frontmatter
- **Note naming conventions:** Many folders use date-prefixed names for automatic sort order. Before creating a note, check existing notes in the target folder for the local pattern. Common patterns:
  - `YYYY-MM-DD Name` — most common (e.g. `2025-08-17 Natal.md`). Use when notes are tied to a specific date.
  - `YYYY-MM Name` — when only month matters.
  - No date prefix — for evergreen/reference notes.
  - `xx.00 Name` — can be used as an area home/index note when a Johnny Decimal-style area has a dedicated index note.
  - **Always check existing files first** to match the folder's convention.
- For Johnny Decimal-style areas, a useful shallow pattern is:
  - `xx.00 Name/xx.00 Name.md` for the area folder + index note
  - `xx.01+` sibling folders or notes for subareas
- Treat this as a convention option, not a hard requirement; follow the existing local pattern when one is already established.
- Prefer updating an existing note over creating a new one.
- After edits, summarize changes: file path + short description.
- For large reorganizations or mass moves, propose a plan and confirm before executing.

## Core workflows

### List notes

- Use the `glob` tool with `*.md` patterns scoped to `$ZDX_MEMORY_NOTES_DIR`.
- Keep output minimal and scoped to the request (top-level folders, counts, or specific paths).

Example:

```
glob pattern="*.md" path="$ZDX_MEMORY_NOTES_DIR"
```

### Search

- Use the `grep` tool with a regex pattern, scoped to `$ZDX_MEMORY_NOTES_DIR` and/or `$ZDX_MEMORY_DAILY_DIR`.
- For terms on the same line, use a regex like `term1.*term2|term2.*term1`.
- For terms anywhere in a file, use `grep` to find candidates, then open the file to confirm context.
- **Search both notes and daily paths:** Many entries (especially links, quick notes, brainstorms) live in daily notes. Always search both directories unless the user specifies one. Run two `grep` calls in parallel (one per path).
- Treat `$ZDX_MEMORY_NOTES_DIR` and `$ZDX_MEMORY_DAILY_DIR` as the source of truth for memory paths.
- Discover files under those roots with `glob`/`grep` before `read`/`edit`; do not invent alternate absolute paths.
- If a memory path fails, inspect/search those configured roots rather than retrying with a different guessed location.

Example:

```
grep pattern="alice|cpf" path="$ZDX_MEMORY_NOTES_DIR" case_insensitive=true glob="*.md"
grep pattern="alice|cpf" path="$ZDX_MEMORY_DAILY_DIR" case_insensitive=true glob="*.md"
```

### Search by tags

- Notes and daily notes use hashtag tags for categorization: `#tag-name`
- Tags appear inline at the end of lines (e.g., `[Link](url) description #ai-reference`)
- Known tags in use (keep this list updated when adding/consolidating tags):
  - **AI cluster:** `#ai-blog`, `#ai-cli`, `#ai-prompting`, `#ai-reference`, `#ai-skills`
  - **Dev & tools:** `#apis`, `#apps`, `#cli`, `#dev-inspiration`, `#services`, `#tools`
  - **Capture & ideas:** `#ideas`, `#interesting`, `#offload`, `#todo`
  - **Life & career:** `#career`, `#childcare`, `#self-improvement`
  - **Project:** `#zdx`
  - **Social:** `#tweet`
- Retired tags (merged): `#interesting-post` → `#interesting`, `#inspiration` → `#interesting` or `#dev-inspiration`
- To search by tag:

```
grep pattern="#ai-reference" path="$ZDX_MEMORY_NOTES_DIR" glob="*.md"
grep pattern="#ai-reference" path="$ZDX_MEMORY_DAILY_DIR" glob="*.md"
```

- To list all unique tags, use `grep` with `extract_unique`:

```
grep pattern="(?:^|\s)#([a-zA-Z][a-zA-Z0-9_-]*)" path="$ZDX_MEMORY_NOTES_DIR" glob="*.md" extract_unique=true
```
- When saving links/references, apply relevant tags from the known list above.

### Read/answer

- Open the relevant file(s) with the read tool.
- Return only the requested info. Cite file path and line when helpful.

### Edit/create

- Keep file names/structure consistent. When creating a note, include a `# Title` header.
- Prefer updating an existing note over creating a new one.
- After edits, summarize changes: file path + short description.
- For large reorganizations or mass moves, propose a plan and confirm before executing.

### Save links / references

- If the target note is unspecified, default to:
  `00-Index/Links.md` (create if missing).
- Format entries like:

```
## YYYY-MM-DD
- [Title](URL) — optional description/tags
```

- Use `date +%Y-%m-%d` to get the date.

### Organize for better search

- Prefer consistent folder naming and index notes.
- Avoid moving/renaming large sets without confirmation.
- When done, update indexes or list changes clearly.

## Archive pattern

Some notes have a companion `*Archive.md` file for removed or completed items. This keeps the main note clean (better AI context) while preserving memory.

- **Example:** `ZDX Features.md` → `ZDX Features Archive.md` (in same folder)
- When removing or completing an item from a tracked note, move it to the archive with a date and brief reason.
- Archive sections: `## Removed` (with reason) and `## Completed` (with completion date).
- Don't delete items silently — always archive first if an archive file exists.

## Thread references

When saving decisions, plans, or facts that originated from a conversation, include the thread ID for traceability. The current thread ID is available via `$ZDX_THREAD_ID`.

- Format: `**Thread:** \`<thread-id>\`` (inline, at the end of the item or section)
- Multiple threads: `**Threads:** \`thread-id-1\`, \`thread-id-2\``
- This enables any future agent to trace back to the full conversation via `Read_Thread`
- Use for: decisions, feature ideas, plans, architecture choices — anything worth revisiting in context
- Skip for: trivial facts (names, preferences) that don't need conversation context

## Output expectations

- Keep responses constrained to what was asked.
- For searches, show matches with path + line number (and minimal context if needed).
- For edits, list files changed and what changed.
hanged.
