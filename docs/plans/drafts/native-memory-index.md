> **SUPERSEDED** by `integrated-memory-index-and-embeddings.md`. Kept for historical context only; do not work from this plan.

> Stage: drafts. Keep this plan current while working: when a scope item is finished, check its box `[ ]`→`[x]`; when a phase's ✅ demo passes, mark the phase done (with date). The plan file is the source of truth, not memory.

# Goals
- Replace ZDX's qmd CLI dependency with a native, rebuildable memory index inside ZDX.
- Preserve the user-facing `Memory_Search`, `Memory_Get`, and `zdx memory index/status/search` workflows.
- Keep thread JSONL, Notes, and Calendar Markdown as canonical sources; the native index is derived and disposable.
- Ship a daily-usable lexical memory search first, then measure whether chunking or semantic retrieval is needed.

# Non-goals
- No native clone of qmd's full hybrid stack in the MVP: no embeddings, local model downloads, query expansion, HyDE, vector fusion, or reranking.
- No changes to canonical thread storage in `crates/zdx-engine/src/core/thread_persistence.rs`.
- No changes to thread transcript export format in `crates/zdx-engine/src/core/thread_export.rs` unless a later chunking phase proves it necessary.
- No removal of `thread_search` or `read_thread`; they remain separate thread-specific tools.
- No automatic migration of old qmd `#...` docids; native docids start a new stable ZDX namespace.

# Design principles
- User journey drives order.
- Keep the memory tool API stable while swapping the backend.
- Prefer SQLite FTS5 first because `zdx-engine` already depends on bundled `rusqlite` for usage stats.
- Be honest about retrieval quality: lexical MVP must not claim vector/hybrid parity.
- Keep all indexed data rebuildable from canonical sources.

# User journey
1. User runs `zdx memory index`; ZDX exports changed threads and builds/refreshes its native memory index.
2. User runs `zdx memory status`; ZDX reports whether the native memory index is ready, stale, missing, or needs rebuild.
3. User or agent calls `Memory_Search`; ZDX returns results from threads, Notes, and Calendar with `docid`, `source`, `file`, `title`, `snippet`, `score`, and warnings.
4. User or agent calls `Memory_Get` with a returned `docid`; ZDX returns indexed document content.
5. If a known thread ID needs focused extraction, the agent still uses `Read_Thread` directly.

# Foundations / Already shipped (✅)

## Thread transcript export
- What exists: `crates/zdx-engine/src/core/thread_export.rs` exports canonical thread JSONL into clean Markdown under `$ZDX_HOME/exports/threads/`.
- ✅ Demo: `zdx threads export` produces one Markdown file per thread and skips unchanged files.
- Gaps: export freshness is mtime-based; force rebuild remains the escape hatch.

## qmd-backed memory tool surface
- What exists: `crates/zdx-engine/src/tools/memory_search.rs` and `crates/zdx-engine/src/tools/memory_get.rs` define the agent-facing schemas.
- ✅ Demo: `Memory_Search` returns qmd docids; `Memory_Get` reads a returned qmd docid.
- Gaps: descriptions currently promise qmd-backed hybrid/vector behavior, so they must be updated when the backend becomes lexical-first.

## qmd backend module
- What exists: `crates/zdx-engine/src/core/qmd.rs` centralizes qmd collection setup, status, search, get, and qmd CLI process calls.
- ✅ Demo: `zdx memory index/status/search` goes through this module.
- Gaps: backend logic is coupled to qmd commands, qmd config YAML, qmd URI parsing, and qmd docids.

## Memory CLI
- What exists: `crates/zdx-cli/src/cli/commands/memory.rs` exposes `zdx memory index`, `status`, and `search`.
- ✅ Demo: CLI prints thread export counts, qmd collection readiness, search results, and warnings.
- Gaps: wording and readiness logic are qmd-specific.

## Existing SQLite dependency
- What exists: `crates/zdx-engine/Cargo.toml` already depends on bundled `rusqlite`; `crates/zdx-engine/src/core/usage_stats.rs` already uses a derived SQLite cache pattern.
- ✅ Demo: existing usage cache builds without requiring a new database dependency.
- Gaps: no current memory-index schema or FTS5 query path exists.

# MVP phases (ship-shaped, demoable)

## Phase 1: Backend boundary and native index skeleton
- **Goal**: Make the memory backend swappable without changing tools or CLI UX.
- **Scope checklist**:
  - [ ] Introduce a backend-neutral memory index module in `zdx-engine`, reusing public shapes from `crates/zdx-engine/src/core/qmd.rs` where possible.
  - [ ] Add an internal backend choice with qmd as the current default and native SQLite as opt-in.
  - [ ] Move qmd-specific process calls behind the backend boundary.
  - [ ] Keep `Memory_Search`, `Memory_Get`, and `zdx memory ...` call sites stable.
  - [ ] Add native-index status shape with database path, schema version, root paths, document counts, and last successful index timestamp.
- **✅ Demo**: With qmd backend selected, existing `zdx memory index/status/search` behavior still works; with native backend selected, `zdx memory status` reports an empty/missing native index instead of invoking qmd.
- **Risks / failure modes**:
  - Leaking qmd-specific docid or collection assumptions into the new backend boundary.
  - Accidentally changing the agent-facing JSON contract before the native backend is ready.

## Phase 2: Native SQLite FTS5 index and indexing command
- **Goal**: Build a complete native lexical index over thread exports, Notes, and Calendar.
- **Scope checklist**:
  - [ ] Create a derived SQLite DB under `$ZDX_HOME/cache/` or another ZDX-owned cache path.
  - [ ] Add schema tables for source documents, FTS5 content, index metadata, schema version, roots, and generation timestamps.
  - [ ] Generate stable native docids from `source + normalized relative path` using existing SHA-256 support.
  - [ ] Index thread exports from `$ZDX_HOME/exports/threads/`.
  - [ ] Index canonical Notes from `$ZDX_MEMORY_ROOT/Notes` and Calendar from `$ZDX_MEMORY_ROOT/Calendar`.
  - [ ] Exclude `@Archive` and `@Trash` path components for Notes/Calendar.
  - [ ] Remove stale DB rows for deleted/moved files.
  - [ ] Build in a transaction so searches see either the old complete index or the new complete index.
  - [ ] Keep `zdx memory index` exporting threads before indexing.
- **✅ Demo**: Select native backend, run `zdx memory index`, and see document counts for thread, note, and calendar sources with no qmd binary needed.
- **Risks / failure modes**:
  - SQLite FTS5 may not be available in the bundled build on every target; add a startup/test assertion.
  - Absolute-path-based IDs would break stability across root moves; IDs must use logical source-relative paths.

## Phase 3: Native `Memory_Search` lexical path
- **Goal**: Make `Memory_Search` daily-usable on the native backend with honest lexical semantics.
- **Scope checklist**:
  - [ ] Implement safe FTS5 query construction for arbitrary user text, including quotes, punctuation, `-`, `:`, paths, URLs, and error strings.
  - [ ] Support `source = thread|note|calendar` exactly.
  - [ ] Support active-thread exclusion using the current thread ID and thread-export filename mapping.
  - [ ] Return result fields compatible with the current tool: `docid`, `source`, `file`, optional `title`, `snippet`, optional `score`, and `warnings`.
  - [ ] Return bounded snippets using FTS5 `snippet()` or a deterministic local snippet builder.
  - [ ] Map `keyword` to FTS5 search.
  - [ ] Map `vector` and `hybrid` to FTS5 temporarily, with warnings that semantic retrieval is not yet implemented.
  - [ ] Ignore `intent` and `candidate_limit` on native lexical search with precise warnings.
  - [ ] Update `Memory_Search` descriptions so agents do not assume qmd-grade hybrid behavior on the native backend.
- **✅ Demo**: With native backend selected, `zdx memory search --source note "qmd" --json` returns note hits and warnings are correct for unsupported vector/hybrid options.
- **Risks / failure modes**:
  - Passing raw user strings directly to FTS5 `MATCH` can create syntax errors or unintended operators.
  - SQLite `bm25()` score direction/magnitude differs from qmd; ordering matters more than numeric parity.

## Phase 4: Native `Memory_Get` and status readiness
- **Goal**: Make the native backend usable end-to-end from search result to content read.
- **Scope checklist**:
  - [ ] Teach `Memory_Get` to resolve native `#...` docids to indexed document content.
  - [ ] Preserve qmd `Memory_Get` behavior while qmd backend remains selectable.
  - [ ] Make native docids stable across process restart, full rebuild, unrelated file changes, and content edits.
  - [ ] Make `zdx memory status` report native DB missing, schema mismatch, stale roots, stale exports, failed index, document counts, and last successful index.
  - [ ] Ensure corrupt/incompatible DBs produce a clear rebuild path without touching canonical sources.
- **✅ Demo**: Search with native backend, pass the returned docid to `zdx memory get` or `Memory_Get`, and receive the indexed document content after a process restart and full reindex.
- **Risks / failure modes**:
  - Old qmd docids cannot be guaranteed to resolve after native cutover.
  - Returning stale indexed content after source changes unless status and indexing metadata are strict enough.

## Phase 5: Shadow comparison and default switch decision
- **Goal**: Decide from real ZDX memory searches whether native lexical search is good enough to become default.
- **Scope checklist**:
  - [ ] Add a developer-only comparison command or test harness that runs fixed queries against qmd and native FTS when qmd is installed.
  - [ ] Build a small real-corpus query set covering exact names, paths, URLs, errors, commands, factual recall, long-thread hits, and source filters.
  - [ ] Compare top-5/top-10 recall and inspect misses.
  - [ ] Decide whether document-level FTS is good enough or whether chunking must happen before default switch.
  - [ ] If accepted, switch native backend to default and keep qmd selectable for one rollback window.
- **✅ Demo**: A comparison report shows native lexical search meets agreed thresholds, then `zdx memory search` works on a machine without qmd installed.
- **Risks / failure modes**:
  - qmd's semantic/hybrid behavior may still win for paraphrase-heavy recall.
  - Long thread exports may rank poorly as whole documents.

# Contracts (guardrails)
- Canonical sources stay canonical: threads in JSONL, Notes/Calendar in Markdown.
- Native index data is derived, disposable, and rebuildable.
- `Memory_Search` result shape remains compatible: `results`, `warnings`, and per-result `docid`, `source`, `file`, `title`, `snippet`, `score`.
- `Memory_Get` continues to require opaque `#...` docids.
- Source filters must be exact.
- Archive/Trash exclusions must apply to Notes/Calendar.
- Active thread exclusion must apply in agent `Memory_Search` calls.
- Search must not expose partial index writes.
- Backend warnings must be visible when vector/hybrid/intent/candidate-limit behavior is degraded.
- Existing `thread_search` and `read_thread` semantics stay intact.

# Key decisions (decide early)
- Native docid format: stable hash of source plus normalized relative path, not rowid, content hash, absolute path, or mtime.
- Backend config shape and migration path from `[qmd] command` to native default.
- Native DB location and schema-version/rebuild policy.
- Whether the first default switch can ship with document-level FTS or requires chunk-level indexing first.
- What happens to old qmd docids after cutover: likely expire with a clear error.

# Testing
- Manual smoke demos per phase.
- Unit test FTS query escaping with punctuation, quoted text, paths, URLs, and error strings.
- Unit test stable docid generation across rebuild and content edit.
- Unit test source filtering and Archive/Trash exclusion.
- Unit test active-thread exclusion.
- Integration test `zdx memory index/status/search` on native backend without qmd installed.
- Integration test `Memory_Search` → `Memory_Get` end-to-end.
- Best-effort shadow comparison test skipped when qmd is absent.

# Polish rounds (after MVP)

## Polish round 1: Chunked retrieval
- Add chunk-level FTS rows while preserving document-level docids for `Memory_Get`.
- Split thread exports by user/assistant message lines from `thread_export.rs`.
- Split notes/calendar by headings and paragraphs with bounded chunk size.
- Deduplicate chunk hits to best document hit for the public result list.
- ✅ Check-in demo: a long-thread query returns a relevant snippet from the matching part of the thread and still opens the whole document via `Memory_Get`.

## Polish round 2: Better ranking and query tuning
- Tune FTS fields and weights for title/path/body.
- Add deterministic tie-breakers for stable ordering.
- Add source-aware defaults for exact-note searches vs broad memory recall.
- ✅ Check-in demo: fixed benchmark queries produce stable top-k results across runs.

## Polish round 3: Semantic retrieval decision
- Classify remaining real misses after lexical + chunking.
- If paraphrase/vocabulary mismatch is still a real problem, evaluate embeddings/vector search as a separate plan.
- ✅ Check-in demo: decision record says either lexical is enough or lists the specific semantic failures that justify new dependencies.

# Later / Deferred
- Embeddings/vector search — revisit only if measured recall gaps remain after chunking and lexical tuning.
- Query expansion, HyDE, fusion, and reranking — revisit only as a separate semantic retrieval plan.
- qmd MCP/server parity — not needed for ZDX's memory tools.
- Importing old qmd docids — only revisit if users need durable qmd handles across cutover.
- Replacing `thread_search` — revisit only after native memory search is dogfooded as clearly better for thread discovery.