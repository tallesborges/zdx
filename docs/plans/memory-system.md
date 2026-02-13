# Goals
- Agent has persistent memory of personal facts across all threads/sessions
- `MEMORY.md` (index) is always loaded into system prompt — lightweight catalog of available memories
- Detailed memory files in `memories/` folder are loaded on-demand by the AI via `read` tool
- Memory is human-readable and editable (plain markdown files)
- AI organizes memories freely (people, topics, projects — no enforced naming)
- Support many memory files; only relevant files should be loaded per task

# Non-goals
- Mid-conversation memory updates (pollutes conversation flow)
- Tiered/archival memory (hot/cold like Letta)
- Vector search or semantic retrieval
- Per-thread or per-group memory isolation
- Rigid naming patterns tied to skills/projects
- Automation or hooks for memory enrichment (deferred to polish phases)

# Design principles
- User journey drives order
- **Index-based loading**: only `MEMORY.md` is injected into the system prompt; AI reads detailed files on-demand
- Use XML tags (`<memory>`) consistent with existing prompt structure (`<persona>`, `<subagents>`, etc.)
- No new dependencies or tools in MVP — use existing `read`, `write`, `edit` tools
- Use resolved absolute paths from `config::paths::zdx_home()` in tool instructions (no `~` shorthand)
- **AI organizes freely**: memory files can be about people (`alice.md`), topics (`whatsapp.md`), projects (`zdx.md`)
- AI maintains `MEMORY.md` — when memories are created/updated, AI updates the index too

# File structure

```
<ZDX_HOME>/
├── MEMORY.md              ← always loaded into system prompt (index + core facts)
├── memories/              ← detailed memory files (read on-demand)
│   ├── alice.md           ← Alice Borges: wife, contacts, preferences
│   ├── whatsapp.md        ← WhatsApp interaction patterns, tone
│   ├── zdx-project.md     ← ZDX project decisions, patterns
│   └── ...                ← AI creates/organizes freely
├── config.toml
├── skills/
└── threads/

<PROJECT_ROOT>/
└── .zdx/
    ├── MEMORY.md          ← optional project-specific memory index
    └── memories/          ← optional project-specific detailed memories
```

## `MEMORY.md` (index — always in system prompt)
- Core personal facts (name, language, key preferences)
- Catalog of available memory files with one-line descriptions
- Instructions for AI on how to use the `memories/` folder
- Kept small — this goes into every prompt

Example:
```markdown
# Memory

Core facts:
- Name: Talles Borges
- Language: PT-BR preferred, EN for technical
- Location: ...

Available memories in `$ZDX_HOME/memories/` and/or `<root>/.zdx/memories/` (use `read` tool when relevant):
- alice.md: Alice Borges — wife, WhatsApp contact, personal context
- whatsapp.md: WhatsApp interaction patterns, tone, frequent contacts
- zdx-project.md: ZDX project decisions, architecture, coding patterns

When creating or updating memories:
- Update the relevant file in the correct scope (`$ZDX_HOME/memories/` or `<root>/.zdx/memories/`)
- Update this index with any new files or changed descriptions
- Keep entries concise, one fact per line
```

## Memory vs Skills
- **Skill** = HOW to use a tool (generic, installable, shared) — e.g., "use `wacli send` to send WhatsApp messages"
- **Memory** = personal CONTEXT (your data, your patterns) — e.g., "Alice = wife, prefers informal PT-BR"
- AI combines both naturally: loads skill for instructions + loads memory for personal context

# Foundations / Already shipped (✅)

## System prompt assembly (`context.rs`)
- What exists: `build_effective_system_prompt_with_paths()` builds the prompt by combining config system prompt + AGENTS.md files + optional MEMORY.md index + skills block + subagents block
- ✅ Demo: run `just run` and observe system prompt in debug output
- Gaps: none for Slice 1/2 core prompt path

## Bot system prompt (`bot_system_prompt.md`)
- What exists: XML-tagged prompt with `<persona>`, `<context>`, `<tone>`, `<important>`, `<telegram_formatting>`, `<response_style>`, `<examples>`
- ✅ Demo: `cat crates/zdx-bot/prompts/bot_system_prompt.md`
- Gaps: no memory section

## ZDX_HOME path helper
- What exists: `config::paths::zdx_home()` resolves `<ZDX_HOME>` (env override or default home path)
- ✅ Demo: used for config, skills, threads
- Gaps: none

# MVP slices (ship-shaped, demoable)

## Slice 1: Load `MEMORY.md` into system prompt
- **Goal**: global (`<ZDX_HOME>/MEMORY.md`) and optional project (`<root>/.zdx/MEMORY.md`) indexes are loaded and injected; AI reads detailed files on-demand
- **Scope checklist**:
  - [x] Add memory loading in `context.rs` for `ZDX_HOME/MEMORY.md` and `<root>/.zdx/MEMORY.md`
  - [x] Wrap content in `<memory>` XML tags when injecting
  - [x] Append memory block in `build_effective_system_prompt_with_paths()` after AGENTS.md, before skills
  - [x] If `MEMORY.md` doesn't exist, skip silently (no error, no block)
  - [x] Cap `MEMORY.md` size (e.g., `16 * 1024`) with truncation warning
  - [x] Reuse lossy UTF-8 decoding behavior from AGENTS.md loading for consistency
  - [x] If `MEMORY.md` exists but read fails, emit warning and continue (no startup failure)
  - [x] Ensure `memories/` path is reachable by tools (note: `read`/`edit` require existing file; `write` can create files)
  - [x] Update `docs/SPEC.md` prompt assembly contract to include optional memory block and insertion order
- **✅ Demo**:
  - Create `<ZDX_HOME>/MEMORY.md` with core facts + index of available memories
  - Create memory details under the intended scope, e.g. `$ZDX_HOME/memories/alice.md` or `<root>/.zdx/memories/project-notes.md`
  - Start ZDX bot or TUI
  - Ask "send a WhatsApp message to my wife" — AI reads MEMORY.md → sees alice.md → reads it → knows Alice
  - Ask something unrelated — AI reads MEMORY.md but doesn't load any detail files (saves tokens)
  - Delete MEMORY.md — agent works normally without memory block
- **Risks / failure modes**:
  - File encoding issues (use same lossy UTF-8 as AGENTS.md loading)
  - AI might not read detailed files when it should (needs good instructions in MEMORY.md)
  - AI might read ALL files every time (MEMORY.md must emphasize selective loading)

## Slice 2: Add memory instructions to system prompts
- **Goal**: System prompts tell the agent how to use and maintain the memory system
- **Scope checklist**:
  - [ ] Add `<memory_instructions>` section to `bot_system_prompt.md` explaining:
    - `MEMORY.md` is loaded with your core facts and an index of detailed memories
    - Use `read` tool to load relevant memory files from `<ZDX_HOME>/memories/` and/or `<root>/.zdx/memories/` when needed
    - Be selective — only load what's relevant to the current conversation
    - When user explicitly says "remember X": update appropriate file AND update `MEMORY.md` index
    - NEVER update memory during normal conversation (only on explicit "remember" requests)
    - Create new files in `memories/` for new topics/people if needed
    - Keep entries concise: one fact per line
    - Don't duplicate — read existing file before adding
  - [x] Add same instructions to the unified `system_prompt_template.md` (all providers)
  - [x] Show `<memory_instructions>` only when memory index is present
- **✅ Demo**:
  - Tell bot "remember that my daughter's name is Sofia, she's 3"
  - Agent creates/updates a scoped detail file (global or project) AND updates the matching `MEMORY.md` index
  - Start a new thread — ask "what's my daughter's name?" — agent reads MEMORY.md → loads family.md → knows
  - Have a normal conversation — verify agent does NOT touch memory files
- **Risks / failure modes**:
  - Agent might update memory without being asked (prompt tuning needed)
  - Agent might not maintain `MEMORY.md` index consistently (prompt must emphasize)

# Contracts (guardrails)
- Memory is optional: ZDX must work identically when `MEMORY.md` doesn't exist
- Memory loading must not crash or block startup if file is missing/corrupted
- Only `MEMORY.md` content is injected into system prompt — detailed files read on-demand
- Prompts should instruct the agent not to update memory files during normal conversation (only on explicit "remember X")
- Existing system prompt structure must remain stable: config + AGENTS.md + memory + skills + subagents
- AI maintains `MEMORY.md` index — any memory file creation/update must also update the index

# Key decisions
- Index location: `<ZDX_HOME>/MEMORY.md` (top-level, alongside config.toml)
- Detail files: `$ZDX_HOME/memories/` (global) and optional `<root>/.zdx/memories/` (project)
- XML tag name: `<memory>` (consistent with `<persona>`, `<subagents>`)
- Prompt position: after AGENTS.md content, before skills block
- Loading: MEMORY.md only in prompt; detail files via `read` tool on-demand
- Naming: free-form, AI-organized
- Update: explicit "remember X" only during sessions; enrichment deferred to polish phases

# Testing
- Manual smoke demos per slice
- Unit test: `build_effective_system_prompt_with_paths` includes memory block when `MEMORY.md` exists
- Unit test: no memory block when `MEMORY.md` is missing
- Unit test: memory block contains only `MEMORY.md` content, not files from `memories/`
- Unit test: prompt ordering remains `config -> AGENTS -> memory -> skills -> subagents`
- Unit test: memory truncation emits warning and caps injected content
- Integration: verify memory appears in prompt across TUI, bot, exec modes
- Integration: verify AI can read files from `memories/` on-demand

# Polish phases (after MVP)

## Phase 1: Seed memory
- Create starter `MEMORY.md` template during first run or via `just` recipe
- Create example `memories/` files (contacts, preferences)
- ✅ Check-in demo: new user gets helpful starter templates

## Phase 2: Memory in status display
- Show memory status in TUI status line (file count, total size)
- ✅ Check-in demo: status shows "Memory: 4 files (3.2KB)"

## Phase 3: Memory enrichment automation
- Scheduled automation mines recent threads and updates memory files + index
- Daily run with cheap model
- ✅ Check-in demo: facts from conversations appear in memory next day

### Phase 3 details (proposed)
- Add a built-in/user automation (e.g. `memory-enrichment.md`) using the existing automations system
- Recommended cadence:
  - Daily full pass (last 24h)
  - Optional lightweight pass every few hours (recent N threads)
- Expected automation behavior:
  - Read recent thread summaries/transcripts
  - Extract durable facts/preferences only (avoid transient chatter)
  - Update or create files in the matching memories scope (`$ZDX_HOME/memories/` or `<root>/.zdx/memories/`)
  - Always sync `MEMORY.md` index entries (file + one-line description)
  - Deduplicate and normalize wording (one fact per line where practical)
- Safety guardrails:
  - Never overwrite large sections blindly; prefer minimal edits
  - Skip uncertain facts or mark them as low-confidence notes
  - Do not store sensitive credentials/secrets in memory files
  - Keep user-intent rule: interactive session writes require explicit "remember"; automation handles background maintenance
- Validation/demo:
  - Run automation manually (`zdx automations run ...`) and verify changed files
  - Confirm `MEMORY.md` stays concise while detailed files absorb growth

## Phase 4: Session-close hooks
- Hook triggered on thread switch or app close
- Lightweight enrichment run after each session
- ✅ Check-in demo: close session → memory updated within seconds

## Phase 5: Memory enrichment tuning
- Refine automation/hook prompts based on real usage
- Dedup/conflict resolution logic
- ✅ Check-in demo: clean, non-duplicated memory updates

## Phase 6: Optional memory metadata/frontmatter
- Add optional YAML frontmatter support in `memories/*.md` files (not required)
- Candidate fields: `summary`, `tags`, `when_to_load`, `priority`, `last_reviewed`
- Keep plain markdown fully supported (no hard migration)
- Expose metadata hints in `MEMORY.md` index entries to improve selective loading
- ✅ Check-in demo: model chooses memory files more accurately using metadata hints

## Phase 7: Memory-maintenance skill
- Add a dedicated skill that teaches memory hygiene and update workflow
- Skill covers: read-before-write, dedup, concise facts, index maintenance, explicit-user-consent updates
- Use skill + memory together: skill defines HOW, memory files provide WHAT
- ✅ Check-in demo: memory updates become more consistent across sessions/providers

# Later / Deferred
- Mid-conversation memory updates → explicitly rejected; pollutes conversation
- Dedicated `update_memory` tool → revisit if write/edit proves unreliable
- Tiered memory (hot/cold) → revisit when memory grows beyond useful size
- Per-chat/per-group memory isolation → revisit when group support lands
- NotePlan as memory backend → revisit after evaluating file-based approach
- Rigid naming patterns → rejected; AI organizes freely
- Mandatory frontmatter for memories → deferred; prefer optional metadata first
- Automatic memory updates without explicit user intent → keep deferred (safety/consent)
