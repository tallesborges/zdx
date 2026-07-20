> Stage: drafts. Keep this plan current while working: when a scope item is finished, check its box `[ ]`→`[x]`; when a phase's ✅ demo passes, mark the phase done (with date). The plan file is the source of truth, not memory.

# Goals
- Cut perceived latency in the thread picker, `threads list`, thread search, and memory search when many threads exist.
- Add a **derived, disposable** `threads.sqlite` index alongside canonical JSONL (never a replacement).
- Reuse the proven `usage_stats.rs` cache pattern (schema versioning, `(mtime,size)` keying, rebuild-on-corruption, fall back to full scan).

# Non-goals
- No change to the canonical storage format. JSONL under `$ZDX_HOME/threads/` stays append-only and source-of-truth (`docs/SPEC.md:108-164`).
- No migration of thread *content*/event replay to SQLite (full context replay still reads JSONL).
- No new query-language, config surface, or third-party search engine (qmd/vector stays as-is).
- No new crate; work lands in `zdx-engine`.

# Design principles
- User journey drives order: fix the paths the user hits most often first (open picker → search → memory search).
- Index is a cache: always rebuildable from JSONL; any DB error degrades gracefully to the current file-scan path.
- Reuse before rebuild: model everything on `crates/zdx-engine/src/core/usage_stats.rs` (already ships `$ZDX_HOME/cache/usage.sqlite`).
- Keep writes cheap: update the index on thread append / meta rewrite; never block the agent loop on it.

# User journey
1. User opens the TUI thread picker (or runs `zdx threads list`) and expects an instant list, even with thousands of threads.
2. User searches threads by text/date and expects fast results without a full-directory grep.
3. User searches past tool calls (`thread_search` tool / CLI) and expects it to respect `limit` instead of parsing every thread.
4. User triggers `Memory_Search` and expects it not to stat + re-read the whole threads dir on every call.

# Foundations / Already shipped (✅)
List capabilities that already exist and should not be rebuilt.

## Canonical JSONL thread store
- What exists: append-only per-thread JSONL under `$ZDX_HOME/threads/`; atomic meta-line rewrite for title/lineage. `crates/zdx-engine/src/core/thread_persistence/storage.rs` (`append`, `ensure_meta`, `append_raw`, atomic meta update).
- ✅ Demo: create a thread in the TUI, confirm a new `<id>.jsonl` appears and grows on each turn.
- Gaps: no index over these files; every list/search re-opens them.

## Derived SQLite cache pattern (the template to copy)
- What exists: `crates/zdx-engine/src/core/usage_stats.rs` maintains `$ZDX_HOME/cache/usage.sqlite`, keyed by `(thread_id, mtime, size)`, re-scans only changed threads, rebuilds if corrupt/schema-bumped, and falls back to a full lean scan when the cache is unavailable. `rusqlite 0.40` is already a bundled dependency (`crates/zdx-engine/Cargo.toml`).
- ✅ Demo: run `zdx stats`; confirm `$ZDX_HOME/cache/usage.sqlite` exists and a second run is faster.
- Gaps: pattern is usage-specific; needs generalizing into a thread metadata + FTS index.

## Thread listing / search / memory export (current slow paths)
- What exists:
  - `list_all_threads()` / `list_threads()` — `storage.rs:718-791`: directory walk + `read_meta()` open per file, filters child runs.
  - `search_threads()` — `search.rs:~119`: `list_threads()` + raw grep pre-filter per candidate + per-match parse (has early termination).
  - `search_thread_tools()` — `search.rs`: loads + parses events for **every** top-level thread regardless of `limit`.
  - Memory-search preflight — `crates/zdx-engine/src/tools/memory_search.rs:108-125` and `crates/zdx-cli/src/cli/commands/memory.rs:131-169` call `export_threads_incremental()` (`core/thread_export.rs`), which sweeps all threads + export files on every query.
- ✅ Demo: with N threads, time the picker / `zdx threads list` / a tool search — latency scales with N.
- Gaps: all four scale with total thread count/bytes; this plan targets them in journey order.

# MVP phases (ship-shaped, demoable)
Define Phase 1..N in user-journey order.

## Phase 1: `threads.sqlite` metadata index powering list/picker
- **Goal**: make the thread picker and `zdx threads list` read metadata from SQLite instead of opening every `.jsonl` meta line.
- **Scope checklist**:
  - [ ] Add `core/thread_persistence/index.rs` (or `core/thread_index.rs`) that owns `$ZDX_HOME/cache/threads.sqlite`, reusing the open/integrity/schema-version/`(mtime,size)` helpers modeled on `usage_stats.rs:315-459`.
  - [ ] Table `thread_meta(thread_id PK, mtime_ns, size, title, root_path, handoff_from, origin_kind, parent_thread_id, subagent_name)`.
  - [ ] `sync_index()` = list files via `list_thread_files()` (cheap stat-only, already exists), re-read meta only for new/changed `(mtime,size)` rows, delete rows for vanished files.
  - [ ] New `list_threads_indexed()` that returns `ThreadSummary`s from the DB (child-run filtering preserved), with a full-scan fallback identical to today on any DB error.
  - [ ] Point the TUI picker (`crates/zdx-tui/src/runtime/handlers/thread.rs`), `zdx threads list` (`crates/zdx-cli/src/cli/commands/threads.rs`), and monitor (`crates/zdx-monitor/src/app.rs`) at the indexed path.
- **✅ Demo**: with a few hundred+ threads, open the picker and run `zdx threads list`; results match the file-scan output exactly and the second open is visibly faster. Deleting `threads.sqlite` transparently rebuilds it.
- **Risks / failure modes**:
  - Stale index if a file changes without mtime bump → mitigated by `(mtime,size)` key + rebuild fallback.
  - Child-run filtering drift → assert indexed list equals `list_threads()` output in a test.

## Phase 2: FTS5 text search powering `search_threads()`
- **Goal**: replace the per-file raw grep with an FTS5 query for the text path.
- **Scope checklist**:
  - [ ] Add FTS5 virtual table over searchable text (title + user/assistant message text), populated during `sync_index()` for changed threads only.
  - [ ] Rewrite the query branch of `search_threads()` (`search.rs:~119`) to select candidate thread ids from FTS, preserving recency ordering, date filters, `exclude_thread_id`, and preview building.
  - [ ] Keep the current grep path as the fallback when the DB/FTS is unavailable.
- **✅ Demo**: search a term known to appear in old threads; results and previews match the current implementation, returned faster, with `limit` respected.
- **Risks / failure modes**:
  - FTS tokenization mismatch vs. substring grep semantics → document the change; reference MiMo-Code FTS5/BM25 notes (saved in *ZDX Reference Projects*).
  - Preview divergence → snapshot-compare previews for a fixed query in a test.

## Phase 3: tool-call index powering `search_thread_tools()`
- **Goal**: stop parsing every thread's events; serve tool search from indexed rows.
- **Scope checklist**:
  - [ ] Table `thread_tool(thread_id, tool_use_id, tool_name, tool_ts, status, args_summary, error_code, error_message)` with indexes on `tool_name`, `tool_ts`, `status`.
  - [ ] Populate during `sync_index()` (pairing tool_use/tool_result as `search.rs` does today).
  - [ ] Rewrite `search_thread_tools()` to query indexed rows honoring `limit` before materializing results.
- **✅ Demo**: `thread_search` tool for a tool name returns the same matches as today, honors `limit`, and no longer scales with total thread count.
- **Risks / failure modes**:
  - Tool pairing edge cases (unpaired results) → cover with the existing search tests as a baseline.

## Phase 4: dirty-flag to kill the memory-search full sweep
- **Goal**: avoid stat-ing + re-reading the whole threads dir on every `Memory_Search`.
- **Scope checklist**:
  - [ ] Track per-thread export state (source `(mtime,size)` + exported flag) in the index; mark the active thread dirty on append.
  - [ ] Change `export_threads_incremental()` (`core/thread_export.rs`) to export only dirty/changed rows using the index instead of a directory-wide sweep.
  - [ ] Keep qmd as the semantic/vector backend unchanged.
- **✅ Demo**: run two consecutive `Memory_Search` calls with no thread changes; the second does zero full-directory work (verify via timing/log).
- **Risks / failure modes**:
  - Missed dirty marking → keep a periodic/full reconcile as a safety net (same rebuild fallback philosophy).

# Contracts (guardrails)
- JSONL under `$ZDX_HOME/threads/` remains canonical and append-only; index writes never modify thread files (`docs/SPEC.md:108-164`).
- Any index read/write failure degrades to the current file-scan behavior — no user-visible errors, no data loss.
- `threads.sqlite` is disposable: deleting it fully rebuilds from JSONL.
- Child-run filtering, recency ordering, date filters, `exclude_thread_id`, and preview content must match current outputs.
- Index maintenance must not block or slow the agent turn loop.

# Key decisions (decide early)
- Index location + name: `$ZDX_HOME/cache/threads.sqlite` (mirrors `usage.sqlite`). Decide whether to extract a shared `cache-open` helper from `usage_stats.rs` or copy it.
- Sync trigger: lazy `sync_index()` on read (like usage stats) vs. also hooking `ThreadWriter::append`/meta-rewrite for incremental updates. Lazy-on-read is the simpler ship-first choice; append-hook is a later optimization.
- What text goes into FTS (message roles included) — fixes tokenization/size tradeoffs before Phase 2.
- Schema version constant + rebuild-on-mismatch, decided up front so later phases can add columns/tables cleanly.

# Testing
- Manual smoke demos per phase (above).
- Minimal regression tests only for contracts, preferably integration tests in `crates/zdx-cli/tests/`:
  - Indexed thread list equals `list_threads()` (order + child-run filtering).
  - `search_threads` results/previews match for a fixed corpus/query.
  - `search_thread_tools` respects `limit` and matches baseline.
  - Deleting the DB triggers a correct rebuild.

# Polish rounds (after MVP)
Group improvements into rounds, each with a ✅ check-in demo.

## Polish round 1: incremental append-time sync
- Hook `ThreadWriter::append` / meta rewrite to update the index incrementally instead of lazy-on-read reconcile.
- ✅ Check-in demo: appending a turn updates `threads.sqlite` without a full `sync_index()` pass.

## Polish round 2: shared cache scaffolding
- Extract the open/integrity/schema-version helpers shared by `usage.sqlite` and `threads.sqlite` into one module.
- ✅ Check-in demo: both caches build on the shared helper; `just ci-fast` clean.

# Later / Deferred
- Migrating canonical event storage into SQLite — revisit only if JSONL replay itself becomes a bottleneck.
- Vector/semantic search inside `threads.sqlite` — deferred; qmd already owns semantic memory.
- File-watcher-based invalidation for the TUI/bot long-lived processes — revisit if lazy reconcile proves too coarse.
