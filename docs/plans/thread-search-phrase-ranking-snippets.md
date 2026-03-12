# Goals
- Make `thread_search` return more relevant threads for natural investigative queries.
- Support quoted phrase queries so searches like exact task names or commands work as expected.
- Show previews that explain why a thread matched instead of unrelated first-assistant text.
- Preserve current date filters and keep the search fast enough for daily use.

# Non-goals
- Full boolean query language (`AND`/`OR`/`NOT`).
- Fuzzy matching, stemming, or typo tolerance.
- Building a persistent full-text index.
- Changing `read_thread` behavior.

# Design principles
- User journey drives order
- Keep the current fast grep prefilter
- Improve relevance before adding query-language complexity
- Ship useful search behavior before advanced syntax

# User journey
1. User searches for a past thread with a natural query or quoted phrase.
2. Search returns the most relevant threads first, not just the newest ones.
3. User can see from the preview why each result matched.
4. User opens the right thread or uses `read_thread` on it.

# Foundations / Already shipped (✅)

## Basic thread search tool
- What exists: `thread_search` accepts `query`, date filters, and `limit`; it scans saved thread JSONL files and returns structured results.
- ✅ Demo: run `thread_search` with a simple word query and get matching thread IDs back.
- Gaps: query handling is whitespace-token-based, not phrase-aware; results are recency-first.

## Fast prefilter
- What exists: `search_threads()` builds a reusable grep matcher and skips threads whose title/raw JSONL cannot match before loading events.
- ✅ Demo: simple keyword searches return quickly even with many threads.
- Gaps: prefilter only understands literal words split by whitespace.

## Date filtering
- What exists: exact date and date range filters are supported and validated.
- ✅ Demo: `thread_search` with `date_start`/`date_end` narrows results correctly.
- Gaps: none for this scope.

## Structured results
- What exists: results already include `thread_id`, `title`, `root_path`, `activity_at`, and `preview`.
- ✅ Demo: tool returns JSON-serializable result objects.
- Gaps: preview is based on first assistant message, not the matched text.

# MVP slices (ship-shaped, demoable)

## Slice 1: Parse search queries into phrases + tokens
- **Goal**: Let users search exact phrases without breaking current free-text behavior.
- **Scope checklist**:
  - [ ] Add a small query parser that extracts quoted phrases and remaining unquoted words.
  - [ ] Keep whitespace-token behavior for unquoted text.
  - [ ] Update grep prefilter builder to include both phrases and tokens as literals.
  - [ ] Add unit tests for quoted phrases, mixed phrase+word queries, and empty/whitespace queries.
- **✅ Demo**: searching for a quoted phrase like a PR title or command returns only threads containing that exact phrase.
- **Risks / failure modes**:
  - Quoted parsing edge cases (unclosed quote, repeated quotes).
  - Over-constraining search if phrases are treated too strictly in prefilter.

## Slice 2: Add relevance scoring and sort by score before recency
- **Goal**: Return the best matches first instead of simply the newest matches.
- **Scope checklist**:
  - [ ] Add lightweight scoring for title phrase matches, title token matches, content phrase matches, and content token matches.
  - [ ] Sort by score descending, then recency descending.
  - [ ] Stop applying the current early-termination-before-ranking logic.
  - [ ] Keep final result limiting after ranking.
  - [ ] Add tests that prove an older but stronger match beats a newer weak match.
- **✅ Demo**: searching for a phrase that appears exactly in an older thread title ranks that thread above newer weak token matches.
- **Risks / failure modes**:
  - Loading too many candidate threads could slow searches.
  - Bad scoring weights could make ranking feel inconsistent.

## Slice 3: Replace generic preview with matched snippet preview
- **Goal**: Help users understand why a thread matched.
- **Scope checklist**:
  - [ ] Build preview from the first matched line/snippet in title or transcript content.
  - [ ] Center the snippet around the first phrase/token hit in plain text form.
  - [ ] Keep current assistant-message preview only as fallback.
  - [ ] Add tests for phrase-based snippet selection and fallback behavior.
- **✅ Demo**: result previews show the actual matching text such as a command, PR phrase, or worktree-related sentence.
- **Risks / failure modes**:
  - Snippets may come from noisy JSONL lines if extraction is too naive.
  - Preview generation could become expensive if done for too many threads.

# Contracts (guardrails)
- Date filters must keep working exactly as they do today.
- Empty query must still work with date-only searches.
- `thread_search` output shape must remain backward compatible.
- Search should stay fast enough for normal interactive use.
- Unquoted free-text searches must still return sensible results.

# Key decisions (decide early)
- Whether quoted phrases are exact-match literals only.
- Whether scoring is done after loading all prefiltered candidates or with a capped candidate pool.
- Whether preview snippets come from raw JSONL lines or reconstructed transcript text.

# Testing
- Manual smoke demos per slice.
- Minimal regression tests only for contracts.

# Polish phases (after MVP)

## Phase 1: Better ranking heuristics
- Tune scoring weights for title vs content and phrase vs token matches.
- Add a small candidate cap if full ranking becomes slow.
- ✅ Check-in demo: common investigative searches consistently put the obvious right thread in the top 1-3 results.

## Phase 2: Query UX clarity
- Improve tool description/input wording so users know quotes are supported.
- Consider returning lightweight match metadata in results if needed.
- ✅ Check-in demo: users can discover and use phrase search without trial and error.

# Later / Deferred
- Full boolean query parsing (`AND`/`OR`/`NOT`) — revisit if phrase+ranking still feels insufficient.
- Fuzzy search / typo tolerance — revisit if exact wording remains a common blocker.
- Persistent index — revisit only if thread volume makes ranked scanning too slow.
- Field-aware search (`title:`, `path:`, `content:`) — revisit if users need more precise power-user queries.