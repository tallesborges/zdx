> Stage: drafts. Keep this plan current while working: when a scope item is finished, check its box `[ ]`→`[x]`; when a phase's ✅ demo passes, mark the phase done (with date). The plan file is the source of truth, not memory.

# Goals
- Replace qmd with native ZDX memory infrastructure for `Memory_Search`, `Memory_Get`, and `zdx memory ...` workflows.
- Add a derived `threads.sqlite` cache for saved-thread metadata, thread text/tool search, and thread-export dirty state.
- Add a derived native `memory.sqlite` index for lexical search over saved thread exports, Notes, and Calendar.
- Add an explicit, opt-in hosted embedding layer over native memory chunks, with incremental re-embedding by input hash and embedding profile.
- Keep canonical sources unchanged: thread JSONL, thread Markdown exports, Notes Markdown, and Calendar Markdown remain the source material.
- Remove qmd from the runtime path once native lexical search is daily-usable; do not keep qmd as a fallback backend.

# Non-goals
- No qmd backend, qmd fallback window, qmd shadow comparison, qmd MCP/server lifecycle, or qmd docid compatibility layer.
- No migration of canonical thread storage from JSONL to SQLite.
- No raw JSONL embedding for memory search; memory indexing uses clean thread Markdown exports plus canonical Notes/Calendar Markdown.
- No agent-triggered corpus embedding and no hidden hosted-token spend.
- No UI/dashboard before CLI/tool status is reliable.
- No note reorganization, memory curation, or automatic note writes.
- No PDF/image/audio attachment indexing in the MVP.

# Design principles
- User journey drives order.
- Native-only target: qmd is current state to retire, not a long-term backend option.
- Derived indexes are disposable and rebuildable.
- Make freshness visible; stale, partial, incompatible, or degraded indexes must warn loudly.
- Reuse the existing SQLite cache pattern before inventing new storage machinery.
- Lexical search remains first-class for exact names, paths, URLs, commands, and errors even after embeddings ship.
- Embeddings are an explicit capability layer over a working lexical index, not a prerequisite for native memory.

# User journey
1. User runs `zdx memory status` and can see whether thread exports, native lexical index, and embeddings are ready, stale, missing, building, partial, incompatible, or errored.
2. User runs `zdx memory index` and ZDX refreshes changed thread exports plus native memory chunks without qmd.
3. User or agent calls `Memory_Search` and gets native lexical results without ZDX sweeping every saved thread on every query.
4. User calls `Memory_Get` with a native docid and gets bounded indexed document content with truncation/continuation metadata.
5. User explicitly opts into hosted embeddings with a dry-run/preflight that shows provider, endpoint host, sources, chunk count, token estimate, budget, and estimated cost.
6. User enables native vector/hybrid retrieval only after the embedding profile has complete coverage for selected sources.

# Foundations / Already shipped (✅)

## Current qmd-backed memory backend to replace
- What exists: `crates/zdx-engine/src/core/qmd.rs` registers `zdx-threads`, `zdx-notes`, and `zdx-calendar`, maps `keyword|vector|hybrid` to qmd commands, parses qmd JSON, filters active-thread hits, and exposes qmd docids.
- ✅ Demo: `zdx memory status` currently shows qmd readiness and qmd search works through `Memory_Search` / `Memory_Get`.
- Gaps: qmd is an external runtime dependency, status/freshness is qmd-specific, qmd docids are not native-owned, and current search still runs thread-export freshness logic on every query. This plan replaces that path rather than wrapping it.

## Thread Markdown export layer
- What exists: `crates/zdx-engine/src/core/thread_export.rs` exports canonical thread JSONL into `$ZDX_HOME/exports/threads/<thread_id>.md`, skips unchanged exports by mtime, removes orphans, and keeps exports disposable.
- ✅ Demo: `zdx threads export` twice; the second run skips unchanged threads.
- Gaps: export freshness is discovered by sweeping all thread summaries; exporter-format changes require `--force`; a single exported message can be one very long line.

## Memory tool and CLI surface
- What exists: `crates/zdx-engine/src/tools/memory_search.rs`, `memory_get.rs`, and `crates/zdx-cli/src/cli/commands/memory.rs` expose `Memory_Search`, `Memory_Get`, `zdx memory index/status/search` with source filters and qmd docids.
- ✅ Demo: `zdx memory search --source thread "qmd" --json` returns `docid`, `source`, `file`, `snippet`, and `score`.
- Gaps: tool descriptions, CLI help, prompt collections, and the memory skill currently hardcode qmd/hybrid semantics and must become truthful native-memory contracts before native search is selectable.

## SQLite cache precedent
- What exists: `crates/zdx-engine/src/core/usage_stats.rs` maintains `$ZDX_HOME/cache/usage.sqlite` as a derived cache using schema versioning, `(mtime,size)` invalidation, WAL/busy-timeout setup, integrity checks, and rebuild/fallback behavior.
- ✅ Demo: `zdx stats` creates and reuses `usage.sqlite`.
- Gaps: the helper pattern is not yet shared by thread or memory indexes.

## Thread persistence and search slow paths
- What exists: `thread_persistence::list_threads()` opens thread metadata from JSONL files, `search_threads()` does text discovery over saved threads, and thread tool search parses persisted events.
- ✅ Demo: thread list/search works today.
- Gaps: list/search/export preflight scale with total thread count; a derived `threads.sqlite` cache is needed for metadata, FTS, tool rows, and export dirty state.

## Provider infrastructure
- What exists: provider config already tracks API keys/base URLs/models, but `zdx-providers` is chat/streaming oriented and has no embedding trait or vector storage contract.
- ✅ Demo: chat providers work; no embedding code exists.
- Gaps: hosted embeddings need a new provider abstraction, model config, cost/freshness reporting, privacy guardrails, and fake-provider tests.

# MVP phases (ship-shaped, demoable)

## Phase 0: Native memory contracts and operation matrix
- **Goal**: Freeze the public/native contracts before implementation starts.
- **Scope checklist**:
  - [x] Define native-only backend config and remove qmd as a target option for new work.
  - [x] Define CLI operation matrix for `zdx memory status`, `index`, `search`, `get`, `index --embed`, `index --embed --dry-run`, `index --force`, and `index --rebuild`.
  - [x] Preserve omitted strategy vs explicit strategy in `Memory_Search` and CLI parsing; omitted strategy maps to native default, explicit unsupported strategy fails clearly.
  - [x] Define native docid grammar as a disjoint, self-routing, versioned namespace; qmd `#...` handles expire with a clear unsupported-docid error after cutover.
  - [x] Define bounded `Memory_Get` output: byte/line limits, truncation flag, continuation handle or range, and source metadata.
  - [x] Define freshness states: `not_configured`, `unprobed`, `missing`, `building`, `ready`, `stale`, `partial`, `incompatible`, and `error`.
  - [x] Define generation chain: thread JSONL generation → export generation → native lexical generation → embedding profile/generation. (Decided: no JSONL-side generation; exports carry `export_format_version` in `thread_export_state`, the lexical index carries `generation`/`chunker_version` in `cache_meta`, embeddings carry `embedding_fingerprint`/`embedding_complete`.)
  - [x] Define DB/root identity: logical corpus IDs, symlink/canonical path handling, root mismatch behavior, path normalization, and traversal rejection. (Decided: root identity is the SHA-256 of the canonicalized root baked into docids — moving a root changes docids and simply reindexes; no separate corpus-ID registry.)
  - [x] Define single-writer lock/lease policy for index/export/embed commands across TUI, bot, CLI, and automations. (Decided: `zdx memory index`/`--embed` hold `$ZDX_HOME/cache/memory.lock` (create-new, 30-minute stale takeover); all other cache writers serialize through SQLite WAL + 5s busy timeout.)
  - [x] Define hosted embedding approval policy: source allowlist, endpoint host, provider/model/dimension, budget cap, pricing source, and query-time upload disclosure. (Implemented in Phase 6 as the required `[memory.embeddings]` profile plus runtime query-upload warnings.)
  - [x] Supersede the older `native-memory-index.md` and `threads-sqlite-index.md` drafts after this contract is accepted.
- **✅ Demo**: Passed 2026-08-04. Native status/search/get/index contracts are implemented in `core/native_memory.rs`, CLI parsing preserves omitted-vs-explicit strategy, and qmd is no longer a config/runtime backend.
- **Risks / failure modes**:
  - Starting implementation before these contracts are fixed will produce incompatible docids, stale status, or hidden hosted spend.

## Phase 1: Native memory status shell and qmd retirement boundary
- **Goal**: Make the user-visible memory surface native-owned before any search replacement ships.
- **Scope checklist**:
  - [x] Add native memory status/result structs in `zdx-engine`.
  - [x] Add native status command plumbing that reports thread-export, `threads.sqlite`, `memory.sqlite`, and embedding states without invoking qmd.
  - [x] Update `Memory_Search`, `Memory_Get`, CLI help, prompt collections, system prompt text, and the bundled memory skill to describe native memory accurately.
  - [x] Keep existing qmd code available only as old implementation code until replacement phases land; do not add new qmd features or qmd fallback routing. Replaced by full qmd code removal in this implementation pass.
  - [x] Add golden tests proving native status does not spawn qmd and qmd-specific docids fail with a clear unsupported-docid message when native-only mode is active.
- **✅ Demo**: Passed 2026-08-04. `zdx memory status --json` reports native components without qmd, and qmd `#...` docids fail with the native migration message.
- **Risks / failure modes**:
  - Tool descriptions can overpromise vector/hybrid behavior before embeddings exist.
  - Removing qmd routing before native search is ready can break current recall unless this phase is gated behind a native-only feature/config flag during development.

## Phase 2: `threads.sqlite` metadata cache and export dirty state
- **Goal**: Remove full JSONL reads from routine thread listing and export freshness, while keeping explicit reconciliation.
- **Scope checklist**:
  - [x] Add `$ZDX_HOME/cache/threads.sqlite` using the resilience pattern from `usage_stats.rs` (WAL, integrity check, schema-version drop/rebuild, corruption recreate, `(mtime,size)` invalidation).
  - [x] Store `thread_meta(thread_id, mtime_ns, size, title, root_path, handoff_from, origin_kind, parent_thread_id, subagent_name, activity_at, modified_at, preview)` and preserve `list_threads()` child-run filtering. (Since Phase 3, `list_threads()` itself is served from the cache with file-scan fallback.)
  - [x] Add lazy `sync_threads_cache()` that stats/enumerates for recovery but reads metadata only for new/changed `(mtime,size)` rows.
  - [ ] Add best-effort dirty marking after thread append/meta rewrite/delete so normal paths scale with changed threads, not total threads. (Deferred: `(mtime,size)` lazy sync detects changes without writer hooks; hooks remain an optimization.)
  - [ ] Inventory and hook all known writers/deleters, including bot delete paths and meta rewrites.
  - [x] Add `thread_export_state(thread_id, source_mtime_ns, source_size, export_mtime_ns, export_size, export_format_version, dirty)`.
  - [x] Change memory-index export and `thread_export_status()` to use dirty/changed rows from the index, with explicit `--force` and full-reconcile fallback (`zdx threads export` stays full-reconcile; status falls back when the cache is missing/incompatible).
  - [x] Clear dirty only after export re-stats the source and proves the source token did not change during export.
  - [x] Count and log files enumerated, JSONL files opened, exports written, and fallback/reconcile events (`thread_cache` counters in `zdx memory index --json` + tracing).
- **✅ Demo**: Passed 2026-08-04. `test_memory_index_second_run_reads_no_unchanged_thread_files` proves the second `zdx memory index` opens zero unchanged JSONL files (`metas_read=0`) and writes zero exports; deleting `threads.sqlite` rebuilds and search output is preserved.
- **Risks / failure modes**:
  - Missed dirty marking would make memory stale; keep forced/full reconcile and status warnings.
  - Lazy sync still stats files during reconciliation, so counters must distinguish stat/enumeration from expensive JSONL reads.

## Phase 3: `threads.sqlite` thread FTS and tool-row search
- **Goal**: Complete the thread-index side so thread picker/search/tool discovery also benefit from the cache.
- **Scope checklist**:
  - [x] Point thread list/picker/monitor paths at indexed thread metadata with file-scan fallback during rollout. (`list_threads()` now dispatches to `thread_index::list_threads_cached()` with the raw scan as fallback, so picker/CLI/monitor/bot all use the cache; `list_threads_scan()` keeps cache-free paths like `zdx threads export` and dry runs write-free.)
  - [x] Add a thread FTS table over title plus user/assistant message text, populated only for changed threads.
  - [x] Decide and document intentional semantic differences from current raw-JSONL substring search, which can match tool/reasoning/meta text. (Documented in `core/thread_index.rs` module docs: title + user/assistant text only; OR of token-prefix matches with LIKE substring fallback; `activity_at` always event-derived; deterministic tool-match tie ordering.)
  - [x] Rewrite `search_threads()` candidate discovery to use indexed FTS while preserving recency, date filters, active-thread exclusion, limits, and preview behavior. (Previews/activity are computed at sync time and stored, so warm queries open no JSONL files.)
  - [x] Add `thread_tool(thread_id, tool_use_id, tool_name, tool_ts, status, args_summary, error_code, error_message, error_details)` rows for tool search (plus derived `tool_date` for SQL date filters; unpaired calls keep status `pending`).
  - [x] Rewrite thread tool search to honor `limit` before materializing results (filters + limit applied in SQL).
- **✅ Demo**: Passed 2026-08-04. Existing `threads list/search/tools` integration tests pass unchanged through the indexed path, and `test_threads_search_reindexes_changed_thread_after_warmup` proves post-warmup appends are found by search and tool queries via incremental re-indexing of only the changed thread.
- **Risks / failure modes**:
  - FTS tokenization differs from substring grep; tests should verify accepted behavior, not accidental legacy quirks.
  - Tool-use pairing edge cases can drift from current parsed behavior.

## Phase 4: Native lexical `memory.sqlite` over deterministic chunks
- **Goal**: Build a native, rebuildable lexical memory index over saved thread exports, Notes, and Calendar.
- **Scope checklist**:
  - [x] Add `$ZDX_HOME/cache/memory.sqlite` separate from `threads.sqlite`; share helper code only after patterns stabilize.
  - [x] Add document and chunk tables for `thread`, `note`, and `calendar` sources, with stable native docids from corpus ID, source, and normalized relative path.
  - [x] Index thread memory from `$ZDX_HOME/exports/threads/*.md`, not raw JSONL.
  - [x] Add `export_format_version`, `chunker_version`, `content_hash`, source mtime/size, title/path metadata, source-relative path, root identity, and indexed generation fields.
  - [x] Chunk thread exports by user/assistant lines with bounded deterministic splitting for very long messages.
  - [x] Chunk notes/calendar by headings and paragraphs with bounded chunk size.
  - [x] Add FTS5 over chunk text plus title/path fields, with safe query construction for URLs, paths, punctuation, quotes, and error strings.
  - [x] Exclude `@Archive` and `@Trash` for Notes/Calendar and reject traversal/escape paths.
  - [x] Build updates in a transaction and remove stale rows for deleted/moved files.
  - [ ] Keep the last complete generation active until a replacement generation succeeds.
- **✅ Demo**: Passed 2026-08-04. `zdx memory index`, `zdx memory search --source note ... --json`, and `zdx memory get <zdxmem:v1:...> --json` work across process boundaries in a temp ZDX home.
- **Risks / failure modes**:
  - FTS5 may not be available in bundled SQLite on every target; add a startup/test assertion.
  - Public docids open indexed document snapshots; known canonical paths and known thread IDs still use `read` / `Read_Thread` for current-source reads.

## Phase 5: Native lexical search semantics and relevance fixtures
- **Goal**: Make native lexical search daily-usable and measurable before adding hosted embeddings.
- **Scope checklist**:
  - [x] Map omitted/default native searches to lexical FTS behavior.
  - [x] Reject explicit `vector`/`hybrid` until embeddings are configured and complete.
  - [x] Preserve exact `source = thread|note|calendar`, active-thread exclusion before limiting, limits, snippets, backend-local scores, warnings, and deterministic tie-breakers.
  - [x] Deduplicate chunk hits to best document hits while overfetching enough chunks to avoid losing relevant documents.
  - [x] Add judged relevance fixtures independent of qmd (`crates/zdx-cli/tests/integration/memory_relevance.rs`: deterministic fixture corpus + judged query set, runs in CI with no qmd anywhere).
  - [x] Cover exact names, paths, URLs, error strings, commands, broad recall, long-thread, notes, and calendar queries (plus accent-insensitive matching via unicode61 diacritics removal).
  - [x] Define acceptance thresholds for exact-hit success, recall@k/success@k, p95 latency, stale-index behavior, index size, and bounded `Memory_Get` behavior:
    - success@k = 100% on the judged fixture set; exact-category queries (names/paths/URLs/errors/commands, source-scoped, long-thread) judged at top-1, broad/accent recall at top-3;
    - latency: warm per-query CLI wall time < 5s in CI (coarse regression net; library-level lexical search target is < 300ms p95 on the real corpus);
    - index size: fixture `memory.sqlite` < 5 MB for the ~100 KB corpus;
    - stale index: searching without an index fails with `run \`zdx memory index\`` guidance instead of empty results;
    - bounded `Memory_Get`: ≤ 40 KB / ≤ 1200 lines per read, `next_start_byte` on truncation, continuation via `--start-byte` / tool `start_byte` (added in this phase).
- **✅ Demo**: Passed 2026-08-04. `test_lexical_relevance_fixture_set`, `test_search_without_index_reports_stale_index_guidance`, and `test_memory_get_is_bounded_and_continues` pass in CI with no qmd installed.
- **Risks / failure modes**:
  - Agents may over-trust native lexical results if unsupported semantic strategies degrade silently.
  - Long documents need chunk-level ranking and document-level reads to avoid bad snippets.

## Phase 6: Hosted embedding provider abstraction and vector generation
- **Goal**: Add incremental hosted embeddings over native chunks without changing canonical sources or re-embedding unchanged text.
- **Scope checklist**:
  - [x] Add an embedding provider abstraction separate from chat streaming providers, with provider adapters in `zdx-providers` (`src/embeddings.rs`, OpenAI-compatible `/embeddings`) and batching/budgets/persistence in `zdx-engine`.
  - [x] Add an explicit embedding profile in `[memory.embeddings]`: provider, endpoint, model, dimension, source scope, pricing, budgets; normalization (L2), vector encoding (`f32` LE), distance metric (cosine), and chunker version are fixed fingerprint-v1 choices (input prefixes/truncation policy deferred until a provider needs them).
  - [x] Add `zdx memory index --embed --dry-run` that performs no provider calls and no cache/export/database writes (reports per-input pending/cached counts and estimated cost).
  - [x] Require explicit source allowlist, endpoint host, model, conservative token estimate, pricing source (`usd_per_million_tokens`), and hard token/USD budget before any hosted corpus upload; over-budget runs refuse before any call.
  - [x] Store vector payloads by exact embedding-input hash + profile fingerprint, with a separate chunk-to-vector mapping for dedupe across shifted chunks/documents (identical inputs embed once).
  - [x] Re-embed only inputs whose hash or embedding profile changed; delete vector mappings for stale chunks (plus orphan-vector GC).
  - [x] Stage hosted batches outside SQLite write transactions and persist completed staging batches in short transactions so interrupted runs resume without repurchasing successful work.
  - [x] Activate a vector generation only if the lexical generation is still current and selected-source coverage is complete. (Embedding reads chunks from the just-built lexical index in the same run; `embedding_complete` is set only at full allowlisted coverage and gates retrieval.)
  - [x] Report estimated tokens/cost always, and actual tokens/cost only when the provider reports usage.
  - [x] Ensure agent `Memory_Search` cannot trigger corpus embedding (vector/hybrid embeds only query/intent text; corpus upload exists only behind `zdx memory index --embed`).
- **✅ Demo**: Passed 2026-08-04 (mock provider). `test_memory_embed_flow_incremental_and_semantic_search`: dry-run makes zero calls/writes, the real run embeds and reports actual tokens, and the second run embeds zero unchanged inputs with zero hosted calls. Budget refusal covered by `test_memory_embed_refuses_over_budget_run_without_calls`.
- **Risks / failure modes**:
  - Provider APIs differ in dimensions, batching, token accounting, privacy terms, and errors.
  - Partial embedding failures must not corrupt lexical search or become active vector coverage.
  - Existing chat-provider API keys must not imply consent to upload the corpus for embeddings.

## Phase 7: Native vector and hybrid retrieval
- **Goal**: Combine lexical and semantic retrieval while preserving exact-match strength.
- **Scope checklist**:
  - [x] Use portable normalized `f32` vector BLOBs plus exact cosine as the first baseline; add ANN only if corpus-scale p95 latency misses the threshold.
  - [x] Implement vector search over the active complete embedding generation.
  - [x] Reject vector/hybrid when current-profile coverage is incomplete; lexical remains available.
  - [x] Make `keyword` lexical-only, `vector` vector-only, and `hybrid` fused retrieval.
  - [x] Combine FTS and vector candidates with reciprocal-rank fusion (k=60, deterministic tie-breakers).
  - [x] Keep `intent` as query disambiguation only for vector/hybrid and warn when ignored (keyword warns; vector/hybrid append it to the embedded query input).
  - [x] Keep `candidate_limit` meaningful for hybrid candidate gathering (per-list candidate cap, default 50).
  - [x] Document that vector/hybrid queries send the query/intent text to the configured embedding provider and can incur recurring hosted cost (SPEC, config comments, and a runtime warning on every semantic search).
- **✅ Demo**: Passed 2026-08-04 against a mock provider: a semantic query ranks the semantically-matching note first under `vector` and `hybrid` while keyword search behavior is unchanged. Real-corpus quality validation belongs to the Phase 5 relevance fixtures.
- **Risks / failure modes**:
  - Semantic scoring can wash out exact identifiers unless lexical signals remain strongly weighted.
  - Hosted embedding cost/latency may not justify quality gains on the real corpus.

## Phase 8: qmd runtime removal and draft cleanup
- **Goal**: Finish the native-only migration and remove qmd from the active memory path.
- **Scope checklist**:
  - [x] Remove qmd from default config generation, memory CLI status/search/index paths, tool descriptions, prompt memory guidance, and bundled memory skill text.
  - [x] Remove or quarantine qmd implementation/tests after native CLI/tool tests cover the replacement contracts.
  - [x] Make old qmd docids fail with a clear native-only migration message.
  - [x] Update `docs/SPEC.md`, `docs/ARCHITECTURE.md`, scoped `AGENTS.md` indexes, and README memory sections.
  - [ ] Archive or mark superseded older qmd/native/thread-index plans.
- **✅ Demo**: Passed 2026-08-04. `zdx memory status/index/search/get` works in a temp home with no qmd setup, qmd code/config were removed, and native CLI tests cover index/search.
- **Risks / failure modes**:
  - Removing qmd before native fixtures pass can regress broad recall.
  - Old qmd docids are intentionally not durable across this migration.

# Contracts (guardrails)
- Canonical sources remain unchanged: thread JSONL, thread Markdown exports, Notes Markdown, Calendar Markdown.
- `threads.sqlite`, `memory.sqlite`, and embeddings are derived, disposable, and rebuildable.
- Separate databases by ownership: `threads.sqlite` owns thread catalog/export/tool/thread-search cache; `memory.sqlite` owns cross-source memory docs/chunks/FTS/embeddings.
- `Memory_Search` result shape remains compatible: `docid`, `source`, `file`, `title`, `snippet`, `score`, `warnings`.
- `Memory_Get` dispatches from native docids and returns bounded indexed document snapshots with truncation/continuation metadata.
- Known thread IDs still go through `Read_Thread` for focused canonical extraction.
- Known note/calendar paths still use direct `read` when current canonical content matters.
- Source filters must be exact and must not rely on `intent`.
- Notes/Calendar exclude `@Archive` and `@Trash` by default.
- Active thread exclusion must apply before limiting in agent `Memory_Search` calls.
- Indexing failures must not mutate or delete canonical sources.
- Hosted embedding must never spend tokens invisibly; CLI/status must show what changed and what was embedded.
- Agent tool calls can search existing vectors but cannot trigger corpus embedding.
- Exact lexical hits for identifiers, file paths, URLs, commands, and errors must remain first-class even after vector search ships.
- A corrupt/incompatible `memory.sqlite` must not delete the last complete active generation until a replacement succeeds.

# Key decisions (decide early)
- Native docid namespace and root/corpus identity scheme.
- CLI command matrix and JSON status schema.
- Export format version and chunker version scheme.
- Native DB locations: default to separate `$ZDX_HOME/cache/threads.sqlite` and `$ZDX_HOME/cache/memory.sqlite`.
- Single-writer lock/lease strategy for index/export/embed commands.
- First embedding provider/model/dimension baseline. → Decided 2026-08-04: `openai:text-embedding-3-small` (native 1536d, ~$0.02/M tokens) as the documented baseline; the profile is fully config-driven so any OpenAI-compatible endpoint works.
- Vector storage baseline: normalized `f32` BLOBs with exact cosine first.
- Partial vector coverage policy: reject vector/hybrid until selected-source coverage is complete.
- Acceptance thresholds for lexical and hybrid relevance fixtures.

# Testing
- Manual smoke demos per phase.
- Integration tests in `crates/zdx-cli/tests/integration/` for CLI memory status/index/search/get behavior with no qmd installed.
- Unit tests for stable docid generation, path normalization, source filters, Archive/Trash exclusion, active-thread exclusion, FTS query escaping, strategy behavior, and bounded `Memory_Get`.
- Regression tests proving indexed thread list/search/tool results match accepted behavior for a fixed fixture.
- End-to-end `Memory_Search` → `Memory_Get` tests for native lexical and native hybrid where enabled.
- Embedding tests with fake provider responses proving zero-call dry-run, budgets, malformed vectors, 429/timeouts, resumability, profile/source changes, concurrent runs, unchanged second runs, and partial generations not becoming active.
- Concurrency/corruption tests for locked DBs, newer schema, disk-full/interrupted builds, root mismatch, and concurrent readers retaining the old generation.

# Polish rounds (after MVP)

## Polish round 1: Shared cache scaffolding
- Extract common SQLite open/schema/integrity/rebuild helpers from `usage_stats.rs`, `threads.sqlite`, and `memory.sqlite` if duplication becomes noisy.
- ✅ Check-in demo: usage, thread, and memory caches all build through shared helpers without behavior changes.

## Polish round 2: Better chunking and ranking
- Tune heading/message chunk sizes, title/path/body weights, deterministic tie-breakers, and snippet quality from real misses.
- ✅ Check-in demo: fixed benchmark queries produce stable top-k results across runs.

## Polish round 3: Embedding provider expansion
- Add additional embedding providers only after the first provider is stable and the trait is proven.
- ✅ Check-in demo: switching provider/model re-embeds only inputs whose embedding profile changed and reports expected cost.

# Later / Deferred
- Reranking, HyDE, or LLM query expansion in native search; revisit only if native hybrid still misses valuable recall cases.
- ANN/vector extension; revisit only if exact cosine misses corpus-scale p95 latency targets.
- File-watch based live indexing; revisit if dirty marking plus lazy/full reconciliation is too coarse.
- PDF/image/audio attachment embeddings; revisit after Markdown/thread recall is stable.
- Memory curation or automatic note updates; keep separate from retrieval infrastructure.

# Oracle review notes incorporated
- Added Phase 0 to freeze backend/config/CLI, docid, freshness, DB/root identity, hosted approval, and vector-storage contracts before implementation.
- Native-only target removes qmd fallback, qmd shadow comparison, and qmd rollback language.
- Phase 2 distinguishes cheap stat/enumeration from expensive JSONL reads and adds post-write dirty marking.
- `threads.sqlite` and `memory.sqlite` are separate by ownership and lifecycle.
- Native memory indexes thread exports, not raw JSONL, and adds `export_format_version`, `chunker_version`, and `content_hash` contracts.
- Explicit `vector`/`hybrid` on native lexical fails until embeddings are configured and complete.
- Hosted embeddings require explicit configuration and dry-run/preflight because they upload private notes, calendar, and thread text.
- Embedding API calls are staged outside SQLite transactions and vector generations become active only when complete.and dry-run/preflight because they upload private notes, calendar, and thread text.
- Embedding API calls are staged outside SQLite transactions and vector generations become active only when complete.