> Stage: drafts. Keep this plan current while working: when a scope item is finished, check its box `[ ]`→`[x]`; when a phase's ✅ demo passes, mark the phase done (with date). The plan file is the source of truth, not memory.

# Goals
- Replace the current NotePlan-only memory model with one Markdown-first knowledge system that can span durable document workspaces and project-scoped knowledge.
- Make explicit saves predictable: clear context saves to the relevant workspace/project; ambiguous context saves to an Inbox.
- Keep personal-value documents available offline, including relevant employment/company records, without requiring code repositories to be offline-synced.
- Preserve attachments with stable relative links and make both notes and project knowledge discoverable through qmd-backed memory search.
- Prove the architecture with one personal workspace and one project before migrating the full vault.

# Non-goals
- Moving or deleting the existing NotePlan vault, `~/Documents/notes` backup, or code repositories before the pilot passes.
- Automatically capturing `.zdx/moments/` in the MVP.
- Automatically promoting moments into curated memory or generating skills via dream/distill in the MVP.
- Silently editing curated notes, project docs, skills, or `AGENTS.md`.
- Replacing Markdown with HTML or a database as the canonical knowledge format.
- Building a custom mobile notes application or Markdown reader.

# Design principles
- User journey drives order.
- Canonical knowledge is plain Markdown; indexes are derived and rebuildable.
- Pilot before migration: prove saving, attachments, indexing, and recall on a small slice.
- Personal-value/offline and project/work are separate axes: an employment contract may be offline-critical while project architecture is not.
- Clear context routes directly; ambiguous context goes to Inbox.
- Private knowledge is private by default and may be promoted to committed project documentation later.
- No silent curation: future consolidation produces reviewable proposals before durable writes.

# User journey
1. The user shares a fact, document, learning, or project decision and asks ZDX to save it.
2. ZDX determines whether the context is clear; unclear items go to Inbox instead of being guessed into a destination.
3. The item is saved as Markdown in the appropriate durable workspace or project-memory location, with attachments linked relatively.
4. qmd indexes the configured knowledge roots and keeps the source location/type visible.
5. In a later thread or project, the user asks about the item and ZDX retrieves the canonical Markdown source through memory search.
6. After the pilot is trusted, existing personal and project notes are migrated incrementally; passive moments and consolidation remain optional later layers.

# Foundations / Already shipped (✅)

## Canonical Markdown memory + compact index
- What exists: `[memory].root` resolves a single root into `Notes/`, `Calendar/`, and `Notes/MEMORY.md` in `crates/zdx-engine/src/config.rs`; `docs/SPEC.md` defines `MEMORY.md` as the injected index while detailed notes remain on disk.
- ✅ Demo: configure `[memory].root`, start an interactive ZDX surface, and confirm the memory index is present while detailed notes are read on demand.
- Gaps: the storage contract assumes one NotePlan-shaped root and cannot describe multiple document/project roots.

## qmd-backed memory search
- What exists: `crates/zdx-engine/src/core/qmd.rs` defines fixed `zdx-threads`, `zdx-notes`, and `zdx-calendar` collections; `crates/zdx-engine/src/tools/memory_search.rs` and `memory_get.rs` expose search and doc retrieval; `crates/zdx-cli/src/cli/commands/memory.rs` indexes and searches them.
- ✅ Demo: `zdx memory index` followed by memory search returns a note or saved thread with a qmd doc ID.
- Gaps: qmd roots and source kinds are fixed; project/workspace knowledge roots cannot be configured independently.

## Explicit-save behavior and filing guidance
- What exists: `crates/zdx-assets/bundled_skills/memory/SKILL.md` requires immediate saves for explicit “remember/save this” requests and uses normal file tools to edit canonical Markdown.
- ✅ Demo: ask ZDX to remember a fact and verify the relevant canonical note changes.
- Gaps: routing is NotePlan-specific; there is no Inbox contract, project-memory contract, attachment convention, or dedicated save router.

## Project context metadata
- What exists: persisted threads carry `root_path` in `crates/zdx-engine/src/core/thread_persistence/storage.rs`; Telegram profiles map chats to cwd in `crates/zdx-bot/src/bot/context.rs`, and restored bot threads can reuse their persisted root in `crates/zdx-bot/src/handlers/message/turn.rs`.
- ✅ Demo: `/whereami` or restored thread behavior shows the project/profile working root remains stable across turns.
- Gaps: storage routing does not currently map this context to a durable document workspace or project-memory destination.

## Automation and proposal building blocks
- What exists: `crates/zdx-engine/src/automations.rs` discovers Markdown/YAML automations; thread exports, memory search, and ordinary file tools can support later curation; Telegram staging in `crates/zdx-bot/src/staging.rs` is an existing accept/discard interaction pattern.
- ✅ Demo: list/validate/run an existing automation and inspect its persisted result.
- Gaps: there is no shipped `.zdx/moments` capture, dream/distill flow, or memory proposal queue.

# MVP phases (ship-shaped, demoable)

## Phase 1: Lock the storage contract through a two-context pilot
- **Goal**: Make the architecture daily-usable for one personal workspace and ZDX itself without moving the existing vault or all repositories.
- **Scope checklist**:
  - [ ] Decide the durable-location conflict: either (A) synced repositories with gitignored colocated private memory, or (B) unsynced `Code/` repositories with private project memory stored in a synced document workspace. Record the choice in this plan before moving files.
  - [ ] Decide folder naming/casing and top-level workspaces; preserve the prior direction of simple workspace names rather than Johnny Decimal/PARA unless explicitly changed.
  - [ ] Decide the attachment convention: attachment beside its topic note vs a workspace-local attachments directory; require relative links either way.
  - [ ] Decide where owned-repository memory lives: committed project memory vs private document workspace.
  - [ ] Create a non-destructive pilot with one personal destination, one Inbox, and one ZDX project-memory destination; copy representative notes rather than moving originals.
  - [ ] Save one Markdown note with one attachment and verify the relative link works in an offline-capable Markdown reader.
- **✅ Demo**: with the network unavailable, open the pilot personal note and attachment; then ask ZDX to save one clearly project-scoped item and one ambiguous item and verify they land in the ZDX pilot destination and Inbox respectively, while original files remain untouched.
- **Risks / failure modes**:
  - Deferring the durability decision can place private memory inside an unsynced, disposable checkout.
  - A global attachment dump can break topic ownership; per-note folders can create excessive nesting.
  - Renaming/moving too much before recall works can strand links and invalidate the pilot.

## Phase 2: Configurable multi-root indexing and source identity
- **Goal**: Search the pilot personal workspace, project memory, calendar, and saved threads through one qmd-backed workflow without pretending they are all NotePlan notes.
- **Scope checklist**:
  - [ ] Extend the single-root memory configuration in `crates/zdx-engine/src/config.rs` to describe named knowledge roots while keeping canonical paths explicit.
  - [ ] Update default configuration assets in `crates/zdx-assets/default_config.toml` and behavior contracts in `docs/SPEC.md`.
  - [ ] Replace the fixed notes/calendar collection construction in `crates/zdx-engine/src/core/qmd.rs` with collection definitions derived from configured roots.
  - [ ] Preserve stable source metadata so results distinguish personal documents, project memory, calendar notes, and historical threads.
  - [ ] Update `crates/zdx-engine/src/tools/memory_search.rs`, `memory_get.rs`, and `crates/zdx-cli/src/cli/commands/memory.rs` for configured roots and explicit stale/missing-index warnings.
  - [ ] Add focused qmd/config tests using the existing in-module test patterns and CLI integration tests under `crates/zdx-cli/tests/integration/` when output behavior changes.
- **✅ Demo**: one query returns a result from the pilot personal root and another from the ZDX project-memory root, labels their source types correctly, and still retrieves saved threads; rebuilding the index from scratch reproduces the results.
- **Risks / failure modes**:
  - Renaming existing qmd collections can conflict with installed collection path/pattern metadata.
  - Multi-root results without source identity may cause historical threads or private project notes to be mistaken for canonical personal facts.
  - Index migration must not make stale or partial coverage look complete.

## Phase 3: Explicit-save routing + Inbox
- **Goal**: Make “save this” consistently choose the correct pilot destination without forcing the user to name a path every time.
- **Scope checklist**:
  - [ ] Update `crates/zdx-assets/bundled_skills/memory/SKILL.md` with the approved storage roots, Inbox fallback, attachment convention, and private-vs-shareable routing rules.
  - [ ] Use persisted thread `root_path` and Telegram profile cwd as context signals; do not infer ownership or durability solely from a folder name.
  - [ ] Route clear personal-value records to the appropriate document workspace, clear project knowledge to the configured project-memory destination, and ambiguous items to Inbox.
  - [ ] Keep writes on canonical files via normal file tools; do not create a parallel database or hidden copy.
  - [ ] Update prompt contracts in `crates/zdx-assets/prompts/system_prompt_template.md` and prompt assembly/tests in `crates/zdx-engine/src/core/context.rs` only where the skill alone cannot enforce the behavior.
  - [ ] Keep proactive save suggestions surface-aware and preserve the immediate-save contract for explicit requests.
- **✅ Demo**: from both TUI and a project-bound Telegram profile, save a personal-value record, a project learning, an attachment, and an ambiguous item; verify each lands once in the expected pilot destination and is retrievable after indexing.
- **Risks / failure modes**:
  - Model-only routing may drift without a small explicit contract and deterministic Inbox fallback.
  - A project cwd identifies context but does not prove whether content is private, shareable, or durable.
  - Duplicate saves can arise if both the old NotePlan rule and new routing rule remain active.

## Phase 4: Incremental migration and cutover
- **Goal**: Retire the duplicated/legacy structure only after the pilot flow is reliable.
- **Scope checklist**:
  - [ ] Inventory the live NotePlan vault into personal-value records, learning archives, project memory, calendar notes, and attachments; exclude `@Archive/` and `@Trash/` unless deliberately reviewed.
  - [ ] Keep employment contracts, compensation, legal/company records, family, health, home, financial, and travel documents in the offline-capable document set even when currently filed under Work.
  - [ ] Migrate ZDX and active work-project knowledge incrementally to the approved project-memory destinations; split oversized notes along topic boundaries rather than preserving giant files.
  - [ ] Preserve relative attachment links and add redirect/index notes only where needed during transition.
  - [ ] Rebuild qmd after each small batch and compare representative searches before and after.
  - [ ] Archive the dead `~/Documents/notes` backup and old NotePlan structure only after checksums/backups and retrieval demos pass; do not delete them as part of this plan.
- **✅ Demo**: representative personal records, a personal-value work record, a ZDX note, and an active work-project note each open from their new canonical path and are returned by memory search; the old vault is no longer required for those samples.
- **Risks / failure modes**:
  - Bulk moves can break Markdown links, attachment references, and `MEMORY.md` pointers.
  - “Work” is not one category: job records and project memory require different destinations.
  - An offline promise is invalid until the selected iCloud locations are actually downloaded and tested offline.

# Contracts (guardrails)
- Canonical knowledge MUST remain human-readable Markdown; HTML reports and qmd indexes are derived artifacts, not sources of truth.
- Explicit “remember/save this” requests MUST save immediately; ambiguous location MUST route to Inbox rather than be guessed.
- Personal-value documents selected for offline use MUST be readable with network access disabled, including their attachments.
- Project context MUST NOT automatically imply that content is safe to commit or share.
- Gitignored private memory MUST NOT be treated as durable when it lives in an unsynced/disposable checkout.
- Search results MUST identify source/root and warn when indexing is stale or incomplete.
- Migration MUST be copy-and-verify before cutover; this plan does not delete original notes, backups, or repositories.
- Automatic moments/consolidation MUST NOT silently modify curated memory, skills, or `AGENTS.md`.

# Key decisions (decide early)
- **Durable code/memory topology**: synced repositories with colocated private memory vs unsynced `Code/` with private memory in document workspaces. This choice changes storage paths and migration steps.
- **Top-level workspace layout and casing**: confirm the simple `Documents/Personal`, `Documents/Parity`, and `Documents/Borges Consultoria` direction from the earlier brainstorm, or replace it before Phase 1.
- **Attachment ownership**: beside-topic attachments vs workspace-local attachment directory. Relative links are required either way.
- **Owned-repository memory**: committed in the repository vs kept in the private document workspace.
- **iCloud scope**: choose which document roots are synced/downloaded for offline use; do not sync code by accident if the unsynced-Code topology is chosen.

# Testing
- Manual smoke demos per phase.
- Minimal regression tests only for config/path contracts, qmd collection/source behavior, prompt routing rules, and CLI-visible indexing output.
- Verify offline behavior with network disabled and files marked downloaded locally; “present in iCloud” alone is not sufficient.
- Before any migration batch, record source paths and checksums; after copying, verify file counts, hashes, Markdown links, and representative searches.

# Polish rounds (after MVP)

## Polish round 1: Migration ergonomics
- Add a dry-run inventory/report for proposed moves, duplicate detection, oversized-note candidates, and broken relative links.
- ✅ Check-in demo: the report explains every proposed move and flags conflicts without modifying canonical files.

## Polish round 2: Reviewable memory consolidation
- Add a proposal queue with list/diff/apply/reject behavior, preserving source thread, scope, and creation time.
- Use the existing automation framework and staging interaction patterns rather than allowing direct background edits.
- ✅ Check-in demo: an automation proposes one update to curated memory; rejecting changes nothing, applying performs only the displayed diff.

# Later / Deferred
- `.zdx/moments/` passive episodic capture. Revisit only after explicit-save usage reveals valuable things the user repeatedly forgets to save.
- MiMo-style dream consolidation from moments into curated memory. Revisit after moments produce useful signal and the proposal queue is trusted.
- Distill repeated workflows into skill/subagent/command drafts. Revisit after duplicate detection and review-first promotion exist.
- Promotion between notes, project memory, skills, and `AGENTS.md`. Revisit after storage and recall are stable; never promote automatically in the first version.
- A custom mobile reader or HTML-first knowledge system. Revisit only if ordinary offline Markdown readers cannot satisfy the mobile workflow.

# Supersedes / related context
- Supersedes the storage direction in `docs/plans/archived/memory-system.md`, which explicitly removed project-scoped memory and made NotePlan the only detailed backend.
- Extends the deferred project-learning/promotion scope in `docs/plans/archived/recall-tool-canonical-notes-threads.md`.
- Reuses the shipped proactive-save behavior documented in `docs/plans/done/proactive-memory-suggestions.md`.
- Related brainstorms are recorded in the ZDX Features note under Knowledge Curation, Recall Tool, Memory Steward Moments, and Memory System Evolution References.
- Source discussion: Telegram thread `telegram--1003804637932-topic-5100` and earlier threads `topic-10005`, `topic-10197`, `topic-10436`, `topic-10559`, `topic-11113`, and `topic-9171`.