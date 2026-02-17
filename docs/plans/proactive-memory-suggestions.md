# Goals
- In TUI and Telegram sessions, the agent proactively suggests saving noteworthy info to the user's second brain (NotePlan)
- Existing "remember X" → immediate save behavior stays unchanged
- Exec mode, automations, and subagent runs remain suggestion-free
- Keep `MEMORY.md` compact as an index/routing layer so prompt size stays stable over time

# Non-goals
- New tools or commands (this is prompt/plumbing only)
- Vector DB, semantic search, or any storage infrastructure
- Replacing NotePlan with a different memory backend
- Fine-tuning suggestion quality (iterate later via prompt edits)

# Design principles
- User journey drives order
- Minimal plumbing change — the intelligence lives in the prompt template
- Don't break what works: exec/automation/subagent behavior must not change
- `MEMORY.md` is an index, not a database (detail lives in NotePlan notes)

# User journey
1. User chats in TUI or Telegram
2. User shares a decision, preference, fact, or useful link during conversation
3. Agent responds normally, then appends a one-line suggestion: "💡 Want me to save [X] to [note]?"
4. User says yes → agent saves detail to NotePlan; updates `MEMORY.md` only when the item is durable/reusable
5. User says no or ignores → agent moves on, no repeat nagging

# Foundations / Already shipped (✅)

## Memory system (MEMORY.md + NotePlan)
- What exists: `MEMORY.md` loaded from `$ZDX_HOME`, rendered into system prompt via `memory_index` template var. Second-brain skill for read/write.
- ✅ Demo: Check system prompt in TUI — `<memory>` block appears with index content
- Gaps: None

## System prompt template rendering
- What exists: MiniJinja template at `crates/zdx-core/prompts/system_prompt_template.md`. `PromptTemplateVars` struct with conditional blocks. `build_effective_system_prompt_with_paths_and_surface_rules()` is the main entry point.
- ✅ Demo: Tests in `context.rs` verify template rendering with/without memory, surface rules, skills
- Gaps: No explicit "index vs detail" policy yet (what goes to NotePlan only vs `MEMORY.md` index)

## Surface-aware callers
- What exists: TUI calls `build_effective_system_prompt_with_paths()` (no surface rules). Telegram bot calls with surface rules. CLI exec and subagents also call the same functions.
- ✅ Demo: Grep callers in `zdx-tui`, `zdx-bot`, `zdx-cli` to see distinct call sites
- Gaps: None for proactive-flag routing; memory-growth controls still missing

# MVP slices (ship-shaped, demoable)

## Slice 1: Add `memory_suggestions` plumbing
- **Goal**: Thread a `memory_suggestions: bool` through `PromptTemplateSections` → `PromptTemplateVars` → template rendering, defaulting to `false`
- **Scope checklist**:
  - [x] Add `memory_suggestions: bool` field to `PromptTemplateSections`
  - [x] Add `memory_suggestions: bool` field to `PromptTemplateVars`
  - [x] Wire it through `build_prompt_template_vars()`
  - [x] Update all existing callers/tests to pass `memory_suggestions: false` (no behavior change)
  - [x] Add a unit test: when `memory_suggestions` is `true`, a placeholder marker appears in rendered output
- **✅ Demo**: `cargo test` passes; new test confirms the flag flows through
- **Risks / failure modes**:
  - Forgetting a call site → compile error (struct literal), so low risk

## Slice 2: Proactive suggestion prompt content in template
- **Goal**: Add conditional prompt instructions in `system_prompt_template.md` that guide the agent to suggest saving noteworthy info
- **Scope checklist**:
  - [x] Add `{% if memory_suggestions %}` block inside the Memory section of `system_prompt_template.md`
  - [x] Content covers: capture triggers (decisions, preferences, facts, links, learnings, patterns), suggestion format (one-line 💡 at end of response), "be specific: say what to save and where"
  - [x] Replace the "Only update memory when the user explicitly says 'remember X'" line with conditional: when `memory_suggestions` is false keep current text, when true add proactive instructions while keeping "remember X" as immediate-save
  - [x] Add test verifying rendered prompt includes suggestion instructions when flag is true
  - [x] Add test verifying rendered prompt excludes suggestion instructions when flag is false
- **✅ Demo**: Render template with `memory_suggestions: true`, inspect output contains 💡 format guidance and capture triggers
- **Risks / failure modes**:
  - Prompt wording too aggressive → agent suggests every turn. Mitigate with "suggest sparingly, at most once per conversation turn, only for clearly noteworthy items"

## Slice 3: Enable in TUI and Telegram callers
- **Goal**: Pass `memory_suggestions: true` from TUI and Telegram bot callers so interactive sessions get proactive suggestions
- **Scope checklist**:
  - [x] In TUI caller(s), set `memory_suggestions: true` in `PromptTemplateSections`
  - [x] In Telegram bot caller, set `memory_suggestions: true` in `PromptTemplateSections`
  - [x] Verify exec mode caller passes `false`
  - [x] Verify subagent caller passes `false`
  - [ ] Smoke test: start TUI, have a conversation mentioning a preference, confirm suggestion appears
- **✅ Demo**: Launch TUI → chat about a preference → agent appends 💡 suggestion. Launch in exec mode → no suggestion.
- **Risks / failure modes**:
  - Telegram bot surface rules interaction — verify suggestion block doesn't conflict with existing surface rules content

## Slice 4: Add memory growth guardrails in prompt template
- **Goal**: Prevent unbounded memory index growth by encoding "note detail first, index selectively" policy in the prompt
- **Scope checklist**:
  - [x] In memory instructions, explicitly state: full detail goes to NotePlan note; `MEMORY.md` keeps short pointers only
  - [x] Add "promote vs save" guidance:
    - [x] Promote to `MEMORY.md` when durable/reusable (stable preferences, key personal facts, long-lived project decisions, recurring patterns)
    - [x] Save note-only for transient items (one-off status updates, temporary blockers, most ad-hoc links)
  - [x] Add "upsert, don’t append" guidance for `MEMORY.md` entries to reduce duplication
  - [x] Add soft cap guidance for index sections (keep concise)
  - [x] Add test asserting guardrail wording appears when `memory_suggestions` is true
- **✅ Demo**: Rendered prompt clearly differentiates NotePlan detail vs `MEMORY.md` index pointers
- **Risks / failure modes**:
  - Overly strict guidance can under-capture useful durable info; tune during dogfooding

## Slice 5: Align second-brain skill instructions with index policy
- **Goal**: Make second-brain skill enforce the same compaction policy used by the system prompt
- **Scope checklist**:
  - [x] Update `second-brain` skill instructions to explicitly treat `MEMORY.md` as an index
  - [x] Document note-first flow: write full detail to note, then selectively update index pointer
  - [x] Document dedupe/upsert behavior for existing index pointers
  - [x] Document concise-entry rule (short pointers, no long narrative in `MEMORY.md`)
- **✅ Demo**: In a memory-save flow, assistant updates note detail and avoids over-appending to `MEMORY.md`
- **Risks / failure modes**:
  - Prompt + skill drift over time; keep both instructions in sync

## Slice 6: Add automation-based memory indexer (recommended)
- **Goal**: Periodically compact and curate `MEMORY.md` so it stays useful and small
- **Scope checklist**:
  - [ ] Create automation to review/compact `MEMORY.md` on a cadence (e.g., weekly)
  - [ ] Merge duplicates and stale pointers
  - [ ] Keep only durable/reusable entries; demote transient items to note-only
  - [ ] Preserve section structure and readability
- **✅ Demo**: Run automation once and observe smaller, cleaner `MEMORY.md`
- **Risks / failure modes**:
  - Over-pruning can remove useful routing info; keep conservative first pass

# Contracts (guardrails)
- Exec mode, automations, and subagent runs MUST NOT include proactive suggestion instructions in their system prompt
- "remember X" → immediate save behavior MUST remain unchanged regardless of `memory_suggestions` flag
- All existing `context.rs` tests MUST continue to pass
- Template rendering with `memory_suggestions: false` MUST produce identical output to current behavior
- Detailed memory content MUST be saved in NotePlan notes; `MEMORY.md` MUST remain a compact index
- `MEMORY.md` updates SHOULD be upsert-style (update/merge existing pointer) rather than append-only
- Memory suggestions MUST NOT imply that every saved item should also be added to `MEMORY.md`

# Key decisions (decide early)
- **Where in the template**: The suggestion instructions go inside the existing `{% if memory_index %}` Memory section (they're only useful when memory is loaded). This avoids a new top-level section.
- **Conditional strategy**: Single `{% if memory_suggestions %}` block replaces/augments the "Only update memory when..." line rather than adding a separate section
- **Index policy**: `MEMORY.md` is a routing index (durable pointers), not a full memory dump
- **Daily-note policy**: Daily-note batching suggestions removed from this feature (handled by automation)

# Testing
- Manual smoke demos per slice
- Unit tests in `context.rs` for template rendering with `memory_suggestions` true/false
- Existing tests must pass unchanged (with `memory_suggestions: false` added to struct literals)
- Manual save-flow checks:
  - one-off/transient item → saved to note, not promoted to `MEMORY.md`
  - durable preference/decision → note + concise index pointer
  - repeated durable item → index pointer updated/merged, not duplicated

# Polish phases (after MVP)

## Phase 1: Prompt tuning
- Adjust suggestion frequency/tone based on dogfooding
- Add examples of good vs. bad suggestions in the prompt
- Tune suggestion trigger threshold
- Tune promotion threshold (what deserves index pointer vs note-only)
- ✅ Check-in demo: Run 5 TUI sessions, review suggestion quality and frequency

## Phase 2: Surface-specific suggestion style
- Telegram: shorter suggestions (mobile-friendly)
- TUI: can be slightly more verbose
- ✅ Check-in demo: Compare suggestion format in TUI vs Telegram

# Later / Deferred
- **Config toggle** (`config.toml` flag to disable suggestions per-user) → revisit if suggestions feel too noisy after dogfooding
- **Suggestion tracking** (avoid re-suggesting already-saved items) → revisit if duplicates become a problem
- **Auto-save mode** (save without asking) → revisit only if explicit confirmation becomes tedious
- **Category-specific note routing** (e.g., career facts → career note) → the prompt already says "be specific about where"; revisit if routing quality is poor
- **Smarter indexing/curation** (heuristics/LLM-assisted MEMORY compaction) → revisit after basic automation proves value
