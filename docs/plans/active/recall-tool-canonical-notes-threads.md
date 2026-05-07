# Goals
- Ship a new read-only `recall` tool that searches canonical memory sources: `Notes`, `Calendar`, and saved ZDX threads.
- Make the main agent use `recall` automatically across all primary prompt surfaces when historical/canonical context is likely useful.
- Return structured findings/snippets plus confidence/warning signals so the main agent can decide between canonical notes and historical thread evidence.
- Improve retrieval quality without replacing or breaking the current `thread_search` / `read_thread` flow.

# Non-goals
- Reorganizing the current Second Brain / NotePlan structure.
- Adding per-project `.learning` stores.
- Automatic note updates, promotions to `AGENTS.md`, or skill generation.
- Replacing `thread_search` with `recall`.
- Shipping a dedicated recall UI/dashboard before the core chat flow is dogfooded.

# Design principles
- User journey drives order
- Read-only first: retrieval before curation
- Canonical sources remain the source of truth; the recall index is derived and rebuildable
- Prefer calling `recall` too often over missing useful saved context
- Preserve existing tools and prompts while layering `recall` on top

# User journey
1. The user asks something in any primary ZDX surface that may depend on prior notes or past threads.
2. The main agent calls `recall` automatically, starting with a cheap `quick` pass.
3. `recall` returns findings/snippets from canonical notes, calendar notes, and/or saved threads, plus confidence/warning signals.
4. If needed, the agent deepens with `recall deep`, `read`, or `read_thread`.
5. The agent answers using the retrieved evidence, distinguishing canonical note state from historical thread state.

# Foundations / Already shipped (✅)
List capabilities that already exist and should not be rebuilt.

## Saved thread discovery and extraction
- What exists: `crates/zdx-engine/src/tools/thread_search.rs` already exposes a native thread-discovery tool over persisted threads, and `crates/zdx-engine/src/tools/read_thread.rs` already extracts specific goals from a known thread transcript.
- ✅ Demo: use `thread_search` to find a candidate thread by query/date, then `read_thread` to extract a decision or prior output.
- Gaps: thread retrieval is currently thread-only, query-centric, and not unified with notes/calendar.

## Canonical memory source contract
- What exists: `crates/zdx-assets/bundled_skills/memory/SKILL.md` already defines `$ZDX_MEMORY_ROOT`, `Notes/`, `Calendar/`, and `Notes/MEMORY.md` as the memory contract, with Archive/Trash excluded unless explicitly requested.
- ✅ Demo: the current memory workflow reads/greps notes and calendar notes under that structure.
- Gaps: there is no dedicated retrieval tool over those canonical sources.

## Engine-backed tool pattern
- What exists: `crates/zdx-engine/src/tools/mod.rs` is the central registration point for engine-backed tools such as `read_thread`, `thread_search`, `invoke_subagent`, and `todo_write`.
- ✅ Demo: existing engine-backed tools are available in the default tool set without any extra surface-specific plumbing.
- Gaps: `recall` does not exist yet, and there is no index-backed retrieval substrate.

## Shared prompt/context assembly
- What exists: `crates/zdx-engine/src/core/context.rs` assembles the effective system prompt used across main surfaces, and `docs/ARCHITECTURE.md` confirms the shared base-prompt + prompt-layers architecture.
- ✅ Demo: the same system prompt structure is reused across TUI, bot, and exec surfaces.
- Gaps: there is no built-in guidance for a dedicated recall tool, and no prompt contract for when to call it.

# MVP slices (ship-shaped, demoable)
Define Slice 1..N in user-journey order.

## Slice 1: `recall` contract + tool shell
- **Goal**: Ship a new engine-backed `recall` tool with a stable read-only contract before building the full semantic stack.
- **Scope checklist**:
  - [ ] Add `crates/zdx-engine/src/tools/recall.rs` with a `ToolDefinition` and `execute` entry point.
  - [ ] Register the tool in `crates/zdx-engine/src/tools/mod.rs` and expose it in the default tool set.
  - [ ] Define a structured output contract with fields such as `source_type`, `source_id`, `title`, `snippet`, `score`, `confidence`, `warnings`, `indexed_at`, and a canonical-vs-historical signal.
  - [ ] Ensure the tool is explicitly read-only and never performs note/thread writes.
  - [ ] Return an explicit warning when the index is unavailable, stale, or partially populated rather than pretending coverage is complete.
- **✅ Demo**: The main agent can call `recall` and receive structured findings/warnings, even before semantic ranking is fully implemented.
- **Risks / failure modes**:
  - A vague or under-specified output contract will make the agent misuse results.
  - If freshness/coverage warnings are missing, stale recall will be over-trusted.

## Slice 2: derived index + lexical/metadata retrieval
- **Goal**: Make `recall` useful early by indexing canonical sources into a rebuildable local store and retrieving via lexical + metadata search first.
- **Scope checklist**:
  - [ ] Add a dedicated recall indexing module under `crates/zdx-engine/src/` (new module family) that reads canonical sources from the existing memory/thread contracts.
  - [ ] Index `Notes`, `Calendar`, and persisted thread transcripts into a local derived store.
  - [ ] Start with simple chunking: whole file for small notes, heading-based chunks for larger notes, and short turn/message chunks for threads.
  - [ ] Store text, source metadata, timestamps/hash, and lexical search data in a rebuildable local database.
  - [ ] Run indexing in background batches that refresh within a few minutes instead of inline per-turn updates.
- **✅ Demo**: A query with relevant information in both a note and a saved thread returns useful findings from both sources with source metadata and no manual indexing step.
- **Risks / failure modes**:
  - Large-note chunking may produce weak snippets if too coarse.
  - Silent staleness will make results look correct while being incomplete.
  - Overly ambitious index infrastructure will delay dogfooding.

## Slice 3: semantic layer + `quick` / `deep`
- **Goal**: Improve retrieval quality for “I remember the idea, not the words” queries while preserving exact-name/path/date hits.
- **Scope checklist**:
  - [ ] Add provider/API-based embeddings over the same derived chunks.
  - [ ] Combine lexical, semantic, and metadata ranking into one retrieval path.
  - [ ] Add `quick` mode for cheap first-pass retrieval and `deep` mode for broader candidate retrieval + stronger rerank.
  - [ ] Keep the tool contract the same across both modes: findings/snippets only, no final synthesis.
  - [ ] Preserve exact/thread-id/path/title-like matches as strong lexical signals instead of letting semantics wash them out.
- **✅ Demo**: The agent can find relevant note/thread snippets from a vague query that would be weak under pure keyword search, while still retrieving exact matches when they exist.
- **Risks / failure modes**:
  - Embeddings/rerank can add cost and latency without enough quality gain.
  - Exact identifiers may regress if semantic scoring dominates too hard.

## Slice 4: main-agent integration across all primary surfaces
- **Goal**: Make the primary ZDX assistant use `recall` automatically wherever the shared main prompt is used.
- **Scope checklist**:
  - [ ] Update the shared prompt/context assembly (`crates/zdx-engine/src/core/context.rs` plus prompt assets as needed) so the agent knows when to call `recall`.
  - [ ] Encode the heuristic: start with `recall quick`; deepen only when confidence/coverage are weak or the topic looks important.
  - [ ] Preserve and clarify the existing role of `thread_search`, `read_thread`, and the memory skill rather than replacing them.
  - [ ] Ensure the integration applies across the main surfaces that already use the shared prompt architecture.
  - [ ] Bias toward over-calling `recall` rather than missing useful saved context.
- **✅ Demo**: In TUI and Telegram-style chat flows, the agent autonomously calls `recall` on likely historical/canonical queries and produces better answers without the user explicitly saying “search my notes.”
- **Risks / failure modes**:
  - The agent may over-call `recall` in low-value contexts and add unnecessary latency.
  - Prompt guidance may blur note-edit flows if it overstates `recall` as a replacement for direct note tools.

# Contracts (guardrails)
List non-negotiable behaviors that must not regress (derived from Inputs and existing behavior).
- `recall` is strictly read-only.
- The recall index is derived/rebuildable; canonical notes and persisted threads remain the source of truth.
- Notes and calendar notes are canonical memory sources; saved threads are historical evidence.
- When canonical and historical sources disagree, the retrieval layer must surface that tension rather than silently choosing one.
- Archive/Trash remain out of default recall scope unless explicitly requested, matching the current memory contract.
- `thread_search` and `read_thread` remain available and semantically intact during Phase 1.
- `recall` returns findings/snippets plus signals; it does not synthesize the final answer.

# Key decisions (decide early)
List only decisions that would cause rework if postponed (derived from Inputs).
- Add `recall` as a new tool instead of mutating `thread_search` directly.
- Separate indexing from querying logically: background indexer prepares data, `recall` searches it.
- Use canonical sources only for the first shipping pass: `Notes`, `Calendar`, saved threads.
- Use `quick` first and `deep` only when the first pass is weak or the question matters enough.
- Keep project metadata as an internal ranking hint only, not a user-facing filter in the MVP.
- Keep the current Second Brain structure unchanged for Phase 1.

# Testing
- Manual smoke demos per slice
- Minimal regression tests only for contracts
- Add contract tests for the `recall` output shape, read-only behavior, and warning semantics.
- Add indexing tests that prove a deleted index can be rebuilt from canonical sources.
- Add retrieval smoke tests for three cases: note-only answer, thread-only answer, and mixed note+thread answer where the agent can distinguish canonical vs historical.

# Polish phases (after MVP)
Group improvements into phases, each with a ✅ check-in demo.
Limited strictly to scope present in Inputs.

## Phase 1: retrieval quality + observability
- Improve chunk/snippet quality for large notes.
- Add better freshness/coverage diagnostics and operator-visible status/reindex commands.
- Tune lexical/semantic weighting with real dogfooding queries.
- ✅ Check-in demo: when recall underperforms, the failure is diagnosable from warnings/status instead of feeling random.

## Phase 2: curation workflow on top of recall
- Add a separate write-capable curation flow that can suggest note updates or promotions after recall proves useful.
- Reuse `recall` findings as evidence rather than mixing writes into the retrieval tool.
- ✅ Check-in demo: the system can suggest a note update from retrieved evidence without changing the `recall` contract.

## Phase 3: project learning layer
- Add optional project-scoped operational memory/learning on top of the canonical recall base.
- Keep that as a separate layer so global canonical recall stays clean.
- ✅ Check-in demo: project-specific learning improves work-in-project flows without weakening the global recall behavior.

# Later / Deferred
Explicit list of "not now" items + what would trigger revisiting them.
- Per-project `.learning` stores — revisit after canonical recall is reliable and the curation layer exists.
- Automatic promotion to `AGENTS.md` or skills — revisit after a separate write-capable curation path is proven.
- Reorganizing the Second Brain — revisit only if recall quality remains poor even with a good index.
- Replacing `thread_search` — revisit only after the new `recall` tool is dogfooded and clearly better for its intended use cases.
- Adopting an external backend such as MemPalace — revisit if the native ZDX stack becomes too complex or clearly underperforms.