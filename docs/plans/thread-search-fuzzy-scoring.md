# Goals
- Replace substring-only thread search scoring with nucleo fuzzy matching.
- Improve recall for generic/partial/typo queries that currently return zero results.
- Apply the same fuzzy scoring to the TUI thread picker filter.

# Non-goals
- Semantic/vector retrieval.
- Building a search index or new persistence layer.
- Changing the search API contract (fields, JSON output shape).

# Design principles
- User journey drives order
- Reuse what exists (`nucleo-matcher` already in workspace, scoring infra already in `zdx-core`)
- Minimal surface change — swap scoring internals, keep external contracts stable

# User journey
1. User searches threads with a partial or imprecise query (e.g. `"deploy"`, `"oh my pi"`, `"gtwrkflw"`).
2. Fuzzy scoring returns relevant threads ranked by match quality instead of zero results.
3. User browses threads in TUI picker with the same fuzzy matching.

# Foundations / Already shipped (✅)

## Thread search CLI + tool
- What exists: `zdx threads search` with query, date filters, `--json`, and `thread_search` LLM tool.
- ✅ Demo: `zdx threads search "thread search"` returns results with scores/previews.
- Gaps: scoring is substring `.contains()` + occurrence count — no fuzzy, no word splitting.

## nucleo-matcher in workspace
- What exists: `nucleo-matcher = "0.3"` in workspace `Cargo.toml`, used in `zdx-tui` file picker.
- ✅ Demo: file picker fuzzy matches file paths with `Pattern::score()`.
- Gaps: only wired to file picker, not to thread search or thread picker.

## TUI thread picker
- What exists: `thread_matches_filter()` in thread picker using `.contains()` on ID/title.
- ✅ Demo: type in thread picker to filter by ID or title substring.
- Gaps: no fuzzy matching, no content search, no scoring/ranking.

# MVP slices (ship-shaped, demoable)

## Slice 1: Fuzzy scoring in `score_thread_match`
- **Goal**: Replace substring scoring with nucleo for thread search queries.
- **Scope checklist**:
  - [ ] Add `nucleo-matcher` dependency to `zdx-core/Cargo.toml`.
  - [ ] Replace `score_thread_match` to use `nucleo_matcher::pattern::Pattern::score()`.
  - [ ] Score title and searchable_text separately, weight title matches higher.
  - [ ] Keep the existing `search_threads` flow (index building, date filters, sorting) unchanged.
  - [ ] Keep `score: u32` field in `ThreadSearchResult` (nucleo returns `Option<u32>`).
- **✅ Demo**: `zdx threads search "dploy"` returns threads about "deploy"; `zdx threads search "oh my pi"` returns relevant threads.
- **Risks / failure modes**:
  - Nucleo scores on individual pattern atoms — very long searchable_text may need truncation or per-segment scoring.

## Slice 2: Fuzzy filtering in TUI thread picker
- **Goal**: Replace `.contains()` in thread picker with nucleo fuzzy matching + sorting by score.
- **Scope checklist**:
  - [ ] Replace `thread_matches_filter` with nucleo `Pattern::score()` against title + ID.
  - [ ] Sort filtered results by score descending (best match first).
- **✅ Demo**: typing `"dply"` in thread picker shows threads with "deploy" in title.
- **Risks / failure modes**:
  - Thread picker currently filters a small list — performance is not a concern.

# Contracts (guardrails)
- `ThreadSearchResult` struct shape unchanged (thread_id, title, root_path, activity_at, score, preview).
- `--json` output fields unchanged.
- Empty query still returns all threads sorted by recency (no fuzzy scoring applied).
- Date filters behavior unchanged.
- Score 0 with a query still means "no match" (filtered out).

# Key decisions (decide early)
- Scoring strategy: use `Pattern::parse()` with `CaseMatching::Ignore` and `Normalization::Smart` (same as file picker).
- Title vs content weighting: score title and content independently, take `max(title_score * 2, content_score)` to preserve title preference.
- Searchable text handling: score against the concatenated searchable_text as-is (nucleo handles long strings fine).

# Testing
- Manual smoke demos per slice
- Existing CLI integration tests in `crates/zdx-cli/tests/threads_list_show.rs` must still pass
- Add one test for fuzzy match (query with typo/partial still scores > 0)

# Polish phases (after MVP)

## Phase 1: Preview alignment with fuzzy matches
- Use `Pattern::indices()` to find which part of the thread matched, improve preview snippet selection.
- ✅ Check-in demo: preview shows the text segment where the fuzzy match occurred.

# Later / Deferred
- Scoring per-message instead of concatenated text → revisit if ranking quality is poor on long threads.
- Multi-field boosting (user messages vs assistant vs tool names) → revisit if users report noisy results.
- Vector/semantic search → revisit if fuzzy recall is still insufficient.
