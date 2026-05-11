# Goals
- Integrate qmd into ZDX as the search backend for saved thread recall.
- Add Second Brain notes/calendar as qmd-indexed memory sources after thread refs work.
- Keep `$ZDX_HOME/threads/*.jsonl` as the canonical source of truth.
- Export clean Markdown thread transcripts under `$ZDX_HOME/exports/threads/` for qmd to index.
- Make the export/index/search path manually dogfoodable before adding agent recall automation.
- Expose search/read through memory tools instead of agent-facing qmd tools.
- Map qmd results to stable memory refs so canonical ZDX data remains the read source.

# Non-goals
- No custom ZDX search index, SQLite FTS, embeddings, reranking, or chunking layer.
- No qmd-specific names in the export path or export filenames.
- No agent-facing qmd tool or qmd path contract.
- No frontmatter, turn IDs, message IDs, or custom delimiters in the first export format.
- No note/calendar export format in the MVP; notes and calendar files are already Markdown and should be indexed directly.
- No Archive/Trash indexing by default.
- No long-running qmd manager/MCP sidecar in the MVP.
- No automatic chat recall until manual qmd search is proven useful.

# Design principles
- KISS: copy OpenClaw’s proven session-transcript export shape before adding ZDX-specific polish.
- JSONL remains canonical; Markdown exports and qmd indexes are disposable and rebuildable.
- Export files are generic search documents; qmd is the first consumer, not the owner of the export layer.
- qmd is an implementation detail behind memory tools.
- Prefer explicit CLI dogfooding before hidden agent behavior.
- Let qmd handle indexing, embedding generation/storage, BM25/vector search, chunking, query expansion, and reranking.

# User journey
1. The user runs `zdx threads export` to create/update clean Markdown exports.
2. The user runs a qmd setup/index command for `$ZDX_HOME/exports/threads/`.
3. The user or agent searches through `memory_search`, not a qmd-specific tool.
4. ZDX returns memory refs such as `thread:<thread_id>`, `note:<relative_path>`, or `calendar:<relative_path>` plus snippets from qmd.
5. The user or agent calls `memory_get(ref)` or `read_thread(thread_id)` to read from the canonical source.
6. Later, the main agent uses the memory tools automatically when historical context is likely useful.

# Foundations / Already shipped (✅)

## Canonical saved threads
- What exists: ZDX stores saved threads as JSONL and implements thread persistence in `crates/zdx-engine/src/core/thread_persistence.rs`.
- ✅ Demo: existing thread commands can list/search/read saved threads.
- Gaps: raw JSONL is noisy and not ideal for qmd document indexing.

## Existing thread discovery and deep read
- What exists: `crates/zdx-engine/src/tools/thread_search.rs` finds candidate threads, and `crates/zdx-engine/src/tools/read_thread.rs` extracts focused answers from a known thread ID.
- ✅ Demo: use `thread_search` to find a thread, then `read_thread` with the returned ID.
- Gaps: current discovery is lexical over raw thread storage, not qmd-backed hybrid search over clean transcripts.

## Existing CLI thread surface
- What exists: `crates/zdx-cli/src/cli/commands/threads.rs` exposes thread commands such as search/list/show-style flows.
- ✅ Demo: run existing `zdx threads ...` commands, including `zdx threads export`.
- Gaps: there is no qmd setup/index or qmd-backed search command yet.

## Existing recall planning
- What exists: `docs/plans/active/recall-tool-canonical-notes-threads.md` already describes a broader recall direction.
- ✅ Demo: use it as context for later recall-tool work.
- Gaps: the qmd integration should replace any custom-index ambition in that plan for this path.

## Existing memory root contract
- What exists: the memory skill defines `$ZDX_MEMORY_ROOT/Notes`, `$ZDX_MEMORY_ROOT/Calendar`, and `$ZDX_MEMORY_ROOT/Notes/MEMORY.md` as canonical memory locations.
- ✅ Demo: current memory workflows can read/search these Markdown files directly.
- Gaps: qmd-backed `memory_search` only covers exported threads until notes/calendar collections are added.

# MVP slices (ship-shaped, demoable)

## Slice 1: Thread transcript export
- **Goal**: Generate clean, generic Markdown transcripts from saved thread JSONL files.
- **Scope checklist**:
  - [x] Add a thread export module that reads `$ZDX_HOME/threads/*.jsonl`.
  - [x] Write Markdown files to `$ZDX_HOME/exports/threads/<thread_id>.md`.
  - [x] Use this exact MVP format:
    ```md
    # Thread <thread_id>

    User: full message collapsed into one line
    Assistant: full response collapsed into one line
    ```
  - [x] Keep only user and assistant messages.
  - [x] Collapse internal newlines/tabs/repeated whitespace into single spaces.
  - [x] Skip empty messages.
  - [x] Do not split long messages.
  - [x] Do not add frontmatter, turn numbers, message IDs, or qmd-specific metadata.
- **✅ Demo**: Export one known thread and inspect `$ZDX_HOME/exports/threads/<thread_id>.md`.
- **Risks / failure modes**:
  - Very large messages become long lines; accepted for MVP.
  - Tool output is absent because MVP exports only user/assistant messages.

## Slice 2: Incremental export command
- **Goal**: Make exports cheap enough to run before qmd indexing without regenerating everything.
- **Scope checklist**:
  - [x] Add `zdx threads export`.
  - [x] Skip when target `.md` exists and is newer than the source `.jsonl`.
  - [x] Add `--force` to regenerate all exports.
  - [x] Add `--dry-run` to show what would change.
  - [x] Remove stale exported `.md` files whose source thread JSONL no longer exists.
  - [x] Print counts: exported, skipped, removed, failed.
- **✅ Demo**: Run `zdx threads export` twice; the second run reports unchanged threads as skipped.
- **Risks / failure modes**:
  - Mtime-based freshness can miss exporter logic changes; `--force` covers that for MVP.

## Slice 3: qmd setup and index command
- **Goal**: Let ZDX initialize/update qmd over the generic exports directory.
- **Scope checklist**:
  - [x] Add minimal config for qmd command path, defaulting to `qmd` on `PATH`.
  - [x] Use qmd’s default state locations, while keeping exports under `$ZDX_HOME/exports/threads/`.
  - [x] Add a manual command such as `zdx threads index` that runs export first, then qmd collection/update commands.
  - [x] Register one qmd collection over `$ZDX_HOME/exports/threads/` with pattern `**/*.md`.
  - [x] Run `qmd embed` after qmd updates so vector search modes have document embeddings.
  - [x] Do not start a persistent qmd server/MCP process in the MVP.
  - [x] Surface clear errors when qmd is missing or the qmd command fails.
- **✅ Demo**: Run the command on a fresh ZDX home and confirm qmd sees a `zdx-threads` collection with indexed Markdown files.
- **Risks / failure modes**:
  - qmd CLI flags may vary by version; keep command usage minimal and inspect help/errors during implementation.
  - First qmd semantic setup may be slow if qmd downloads local models.

## Slice 4: Memory search backed by qmd
- **Goal**: Expose qmd-backed thread discovery through a backend-neutral `memory_search` tool/command.
- **Scope checklist**:
  - [x] Add a read-only `memory_search` tool, plus a manual CLI path if useful for dogfooding.
  - [x] Run `zdx threads export` first or warn when exports are stale.
  - [x] Invoke qmd search with JSON output when available.
  - [x] Parse qmd result paths under `$ZDX_HOME/exports/threads/`.
  - [x] Derive `thread_id` from `<thread_id>.md` filename.
  - [x] Return backend-neutral refs such as `thread:<thread_id>`.
  - [x] Return enough result data for follow-up: `ref`, `source`, `thread_id`, snippet, score if available, and warnings.
  - [x] Keep qmd/export paths out of normal tool output; include them only in optional debug metadata.
  - [x] Keep existing `thread_search` behavior unchanged.
- **✅ Demo**: Call `memory_search` for a known prior discussion, get `thread:<thread_id>`, then use `read_thread` on that ID.
- **Risks / failure modes**:
  - qmd result JSON shape may vary by version; keep parser narrow and fail visibly.
  - Filename mapping requires thread IDs to remain safe as filenames, matching current JSONL storage.

## Slice 5: Memory get for canonical reads
- **Goal**: Provide a stable read API for memory refs returned by `memory_search`.
- **Scope checklist**:
  - [x] Add `memory_get` if the existing `read_thread` flow is not enough for the first integration.
  - [x] Accept refs such as `thread:<thread_id>`.
  - [x] For `thread:<thread_id>`, read the canonical JSONL thread or delegate to existing `read_thread` behavior.
  - [x] Do not read exported Markdown as the source of truth for normal answers.
  - [x] Include clear errors for unknown refs, missing canonical threads, or unsupported source types.
  - [x] Keep `thread_search` and `read_thread` available and semantically intact.
- **✅ Demo**: `memory_search` returns `thread:<thread_id>`, then `memory_get` reads the canonical thread content or focused transcript data.
- **Risks / failure modes**:
  - If `memory_get` reads exports instead of canonical JSONL, stale exports can be over-trusted.
  - The first version may not need `memory_get` if `read_thread` already covers deep reads.

## Slice 6: Notes/calendar qmd collections
- **Goal**: Add Second Brain notes and calendar notes to qmd-backed `memory_search` without creating exports.
- **Scope checklist**:
  - [ ] Add qmd collection setup for `$ZDX_MEMORY_ROOT/Notes` with pattern `**/*.md`.
  - [ ] Add qmd collection setup for `$ZDX_MEMORY_ROOT/Calendar` with pattern `**/*.md`.
  - [ ] Exclude `@Archive/` and `@Trash/` by default.
  - [ ] Keep notes/calendar files canonical; do not copy them into `$ZDX_HOME/exports/`.
  - [ ] Map qmd results under Notes to refs like `note:<relative_path>`.
  - [ ] Map qmd results under Calendar to refs like `calendar:<relative_path>`.
  - [ ] Teach `memory_get` to read `note:<relative_path>` from `$ZDX_MEMORY_ROOT/Notes`.
  - [ ] Teach `memory_get` to read `calendar:<relative_path>` from `$ZDX_MEMORY_ROOT/Calendar`.
  - [ ] Preserve `thread:<thread_id>` behavior unchanged.
- **✅ Demo**: `memory_search` for a known Second Brain fact returns a `note:<relative_path>` ref, and `memory_get` reads the canonical note.
- **Risks / failure modes**:
  - Broad notes/calendar indexing may surface too much; start with default excludes and tune only after dogfooding.
  - Relative-path mapping must prevent path traversal and keep refs rooted under Notes/Calendar.

## Slice 7: Auto memory prompt integration
- **Goal**: Make the main assistant use memory tools automatically only after the manual/tool path is stable.
- **Scope checklist**:
  - [ ] Update shared prompt/context guidance around `crates/zdx-engine/src/core/context.rs` as needed.
  - [ ] Prefer explicit `memory_search` tool calls over hidden pre-answer magic for the first integration.
  - [ ] Trigger memory search for likely historical/thread-memory questions.
  - [ ] Preserve explicit `thread_search` / `read_thread` guidance.
  - [ ] Keep qmd/export warnings visible in the tool result.
- **✅ Demo**: Ask “what did we decide about qmd?” and the assistant calls `memory_search`, finds the prior thread, then deep-reads it.
- **Risks / failure modes**:
  - Auto memory search may add latency or unnecessary tool calls.
  - Prompt guidance may cause the agent to rely on search snippets instead of deep-reading the canonical thread.

# Contracts (guardrails)
- `$ZDX_HOME/threads/*.jsonl` remains canonical.
- `$ZDX_HOME/exports/threads/*.md` is derived, disposable, and rebuildable.
- `$ZDX_MEMORY_ROOT/Notes/**/*.md` and `$ZDX_MEMORY_ROOT/Calendar/**/*.md` are canonical Markdown sources and are indexed directly.
- Exported filenames are the mapping contract: `<thread_id>.md` maps to `thread_id`.
- The export format stays intentionally simple for MVP: one user/assistant message per line.
- No qmd-specific path or filename in the export layer.
- qmd is the search backend, not the primary database.
- Agent-facing search/read APIs use memory refs, not qmd paths.
- `memory_get(thread:<thread_id>)` reads canonical ZDX thread data, not exported Markdown, unless debug/export inspection is explicitly requested.
- `memory_get(note:<relative_path>)` reads under `$ZDX_MEMORY_ROOT/Notes` only.
- `memory_get(calendar:<relative_path>)` reads under `$ZDX_MEMORY_ROOT/Calendar` only.
- Archive/Trash are excluded from default qmd memory search unless explicitly supported later.
- Existing `thread_search` and `read_thread` stay available during integration.

# Key decisions
- Use the term “thread transcript export”, not “projection”.
- Use `$ZDX_HOME/exports/threads/` for exported Markdown.
- Skip frontmatter for MVP.
- Use mtime-based incremental export first; avoid a state DB until needed.
- Shell out to qmd in the MVP; defer MCP/server lifecycle.
- Add `memory_search` as the qmd-backed public discovery tool instead of `qmd_search`.
- Add `memory_get` only when the memory-ref read path is needed; `read_thread` can remain the first deep-read path.
- Add notes/calendar by indexing canonical Markdown directly; only threads need Markdown exports.
- Start with manual export/index/search before auto memory integration.
- Treat `qmd vsearch` and `qmd query` as high-level retrieval modes over qmd-managed embeddings, not as a raw embedding API; ZDX should not persist or expose embeddings itself.
- Run `qmd embed` as part of the manual qmd index path because `vsearch`, `vec`, and `hyde` search only work well after embeddings exist.

# Testing
- Unit test transcript formatting: user/assistant only, whitespace collapsed, empty messages skipped.
- Unit test incremental behavior: unchanged files are skipped; `--force` rewrites.
- Unit test stale cleanup: orphaned exported `.md` files are removed.
- CLI smoke test for export counts: exported/skipped/removed/failed.
- CLI smoke test for qmd missing: command fails with a clear error.
- Tool contract test: `memory_search` returns `thread:<thread_id>` refs and no required qmd paths.
- Tool contract test: `memory_get(thread:<thread_id>)` reads canonical thread data.
- Tool contract test: `memory_search` returns `note:<relative_path>` and `calendar:<relative_path>` refs for notes/calendar hits.
- Tool contract test: `memory_get` rejects `note:` / `calendar:` refs that escape their canonical roots.
- qmd search smoke test can be best-effort or ignored when qmd is not installed.

# Polish phases (after MVP)

## Phase 1: qmd lifecycle polish
- Add better status output for qmd binary, collection state, export freshness, and last successful update.
- Add configurable qmd command path only if `PATH` lookup is insufficient.
- ✅ Check-in demo: `zdx threads index/status` tells whether qmd search is ready.

## Phase 2: Search quality tuning
- Compare qmd `search`, `vsearch`, and `query` modes using real ZDX threads.
- Add qmd collection context for `zdx-threads` so qmd knows the collection contains exported ZDX chat thread transcripts and that filenames map to thread IDs.
- Keep mode selection explicit and simple.
- ✅ Check-in demo: the same `memory_search` query can be run in different qmd modes to compare results.

## Phase 3: Notes/calendar tuning
- Tune collection names, excludes, and qmd collection context for Notes and Calendar after dogfooding real queries.
- Keep canonical notes separate from historical thread evidence in results.
- ✅ Check-in demo: one recall query returns clearly labeled note, calendar, and thread results.

# Later / Deferred
- Frontmatter — revisit only if filename mapping becomes insufficient.
- Hash/state DB — revisit only if mtime-based skipping is unreliable.
- Message IDs/turn numbers — revisit only if search results need exact message navigation.
- Long-running qmd MCP/server mode — revisit only if shelling out is too slow.
- Custom indexing/ranking — revisit only if qmd fails the use case.
- Replacing `thread_search` — revisit only after qmd-backed recall is dogfooded and clearly better.