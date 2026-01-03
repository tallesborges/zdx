# Session Naming Implementation Plan _(MVP shipped)_

## Inputs

- **Project/feature**: Add a `title` field to sessions so users see meaningful descriptions instead of UUIDs in session lists. Inspired by sst/opencode, badlogic/pi-mono, and stakpak/agent implementations.
- **Existing state**: Sessions are JSONL files identified by UUID. `SessionSummary` has only `id` and `modified`. CLI list shows `<uuid> <timestamp>`. TUI picker shows truncated UUID + timestamp.
- **Constraints**: Ship-first (ugly but functional MVP). No AI-generated titles in MVP. No breaking changes to session format (backward compatible).
- **Success looks like**: User can name a session, see the name in `zdx sessions list`, and pick sessions by name in TUI.

---

# Goals

- Sessions display a human-readable title instead of (or alongside) UUID
- Users can name sessions when creating or rename existing sessions
- Session list/picker shows title prominently
- Backward compatible: old sessions without titles still work

# Non-goals

- AI-generated titles (deferred)
- Title search/filtering
- Title validation/uniqueness enforcement
- Changing the session filename format

# Design principles

- **User journey drives order**: name display → setting name → renaming
- **Ship-first**: get title showing in list before polishing rename UX
- **Backward compatible**: missing title = show UUID prefix (existing behavior)

# User journey

1. User runs `zdx sessions list` → sees session titles (or UUID if untitled)
2. User opens TUI session picker → sees titles instead of truncated UUIDs
3. User renames a session via `/rename <title>` in TUI → title updates
4. User renames via CLI `zdx sessions rename <id> "new title"` → title updates

---

# Foundations / Already shipped (✅)

## Session JSONL format with schema versioning
- What exists: Sessions have a `meta` event with `schema_version: 1`, events are appended as JSONL
- ✅ Demo: `cat ~/.config/zdx/sessions/<id>.jsonl | head -1` shows meta line
- Gaps: Meta event has no `title` field

## Session listing (CLI)
- What exists: `zdx sessions list` shows `<uuid> <timestamp>`
- ✅ Demo: `zdx sessions list`
- Gaps: No title column

## Session picker (TUI)
- What exists: Overlay shows truncated UUID + timestamp, preview on navigate
- ✅ Demo: Open TUI → Ctrl+O → see session list
- Gaps: Shows UUID, not title

## SessionSummary struct
- What exists: `struct SessionSummary { id, modified }`
- ✅ Demo: Used by `list_sessions()` and session picker
- Gaps: No `title` field

---

# MVP slices (ship-shaped, demoable)

## Slice 1: Add title to meta event and SessionSummary (data layer)

- **Goal**: Store and read title from session files; display in `zdx sessions list`
- **Scope checklist**:
  - [x] Add `title: Option<String>` to `SessionEvent::Meta`
  - [x] Add `title: Option<String>` to `SessionSummary`
  - [x] Update `list_sessions()` to extract title from meta event
  - [x] Update `zdx sessions list` output: `<title|id-prefix>  <uuid>  <timestamp>`
  - [x] Backward compat: sessions without title show UUID prefix
- **✅ Demo**:
  1. Manually edit a session file, add `"title":"test session"` to meta line
  2. Run `zdx sessions list` → see "test session" in output
  3. Run on old session without title → see UUID prefix (no crash)
- **Risks / failure modes**:
  - Parsing old sessions without title field → mitigated by `Option<String>` with `#[serde(default)]`

## Slice 2: Display title in TUI session picker

- **Goal**: Session picker shows title instead of truncated UUID
- **Scope checklist**:
- [x] Pass `SessionSummary.title` to picker state
- [x] Update `render_session_picker` to show `title.unwrap_or(short_id)`
- [x] Keep timestamp display
- **✅ Demo**:
  1. Have a session with title (from Slice 1 manual edit)
  2. Open TUI → Ctrl+O → see title in list
  3. Session without title → still shows UUID prefix
- **Risks / failure modes**:
  - Long titles overflow → truncate with ellipsis

## Slice 3: Rename session via TUI command

- **Goal**: `/rename <new title>` in TUI updates session title
- **Scope checklist**:
- [x] Add `/rename` slash command
- [x] Implement `Session::set_title()` in `src/core/session.rs` (rewrite meta line atomically)
- [x] TUI triggers rename via `UiEffect::RenameSession` → runtime handler calls core
- [x] Show confirmation in transcript
- [ ] Update header or status to reflect new title (still pending)
- **✅ Demo**:
  1. Open TUI with existing session
  2. Type `/rename my new title` → see confirmation
  3. `zdx sessions list` → see updated title
- **Risks / failure modes**:
  - Rewriting JSONL safely → read all, update meta, write to temp, rename (in core)
  - No active session (new chat) → show error message

## Slice 4: Rename session via CLI

- **Goal**: `zdx sessions rename <id> "new title"` for scripting
- **Scope checklist**:
- [x] Add `Rename { id, title }` variant to `SessionCommands`
- [x] Implement `commands::sessions::rename(id, title)` — calls `Session::set_title()` from core
- [x] Reuses title update logic from `src/core/session.rs`
- **✅ Demo**:
  1. `zdx sessions list` → note a session ID
  2. `zdx sessions rename <id> "renamed session"`
  3. `zdx sessions list` → see "renamed session"
- **Risks / failure modes**:
  - Session not found → error message

---

# Contracts (guardrails)

1. **Backward compatibility**: Sessions without `title` in meta must parse without error
2. **No data loss**: Title update must not corrupt session events
3. **Display fallback**: Missing title → show UUID prefix (first 8 chars)
4. **Session file format**: Title stored only in meta event (first line), not in filename

---

# Key decisions (decide early)

1. **Where to store title**: In meta event (chosen) vs. separate index file vs. filename
   - Meta event: simple, self-contained, already versioned
   - Tradeoff: requires reading first line to list sessions (already done)

2. **Title update strategy**: Rewrite meta line vs. append update event
   - Rewrite: cleaner, title always in meta, spec updated to allow this (§8 Metadata Updates)
   - Append: safer but complicates reading (need to scan for latest title)
   - **Decision**: Rewrite with temp-file-then-rename for atomicity (spec-compliant)

3. **Default title for new sessions**: Empty/None (show UUID) vs. timestamp-based
   - **Decision**: None (keep existing UX, user explicitly sets title)

4. **Architecture boundary**: Rename logic in core vs. UI
   - **Decision**: `Session::set_title()` lives in `src/core/session.rs`; TUI/CLI invoke via effects/commands

---

# Testing

- **Slice 1**: Integration test: `zdx sessions list` output includes title; manual test with edited session file
- **Slice 2**: Manual TUI test; no automated UI tests
- **Slice 3**: Manual TUI test with `/rename`
- **Slice 4**: Integration test: `zdx sessions rename <id> "title"` then `zdx sessions list`

---

# Polish phases (after MVP)

## Phase 1: UX refinements
- [ ] Show title in TUI header/status bar when session is loaded
- [ ] Truncate long titles gracefully in picker (with tooltip or scroll)
- [ ] `/rename` with no args → prompt for title interactively
- **✅ Check-in demo**: Title visible in header; long title truncated in picker

## Phase 2: CLI title flag
- [ ] Add `--title <TITLE>` arg to CLI (applies to default chat and exec)
- [ ] Pass title through `SessionPersistenceOptions`
- [ ] Write title in meta event on session creation
- **✅ Check-in demo**: `zdx --title "auth refactor"` → send message → `zdx sessions list` shows title

## Phase 3: Auto-title suggestions
- [ ] If title is empty after first user message, suggest title from first message (truncated)
- [ ] User can accept or dismiss suggestion
- **✅ Check-in demo**: New session → type message → see suggested title → accept/dismiss

---

# Later / Deferred

| Item | Trigger to revisit |
|------|-------------------|
| AI-generated titles | User feedback requests it; daily use shows manual naming is friction |
| Title search/filter | Session count grows large (>50 sessions common) |
| Title in filename | Need for external tools to see titles without parsing JSONL |
| Title uniqueness | Users report confusion from duplicate titles |
| Bulk rename | Users request it for cleanup workflows |
