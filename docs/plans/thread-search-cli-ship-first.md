# Goals
- Ship thread search as a native CLI-first capability (`zdx threads search`).
- Support the main workflows: date-based discovery, topic recall, and automation/report usage.
- Keep a stable machine-readable contract (`--json`) for automation pipelines.
- Reuse the same search core later as an LLM tool.

# Non-goals
- Semantic/vector retrieval in MVP.
- Automatic memory extraction.
- New persistence schema or separate search index service.

# Design principles
- User journey drives order
- CLI-first, tool-later
- Read-only over existing thread data
- Stable automation-friendly output contracts

# User journey
1. User asks for threads from a specific date/range.
2. User narrows by topic (“where did I mention X?”).
3. Automation consumes JSON output to build reports.
4. Later, an LLM tool wraps the same core for memory flows.

# Foundations / Already shipped (✅)
List capabilities that already exist and should not be rebuilt.

## Thread persistence + thread CLI
- What exists: append-only JSONL threads + list/show/resume/rename commands.
- ✅ Demo: `zdx threads list`, `zdx threads show <id>`.
- Gaps: no first-class search subcommand.

## read_thread tool
- What exists: extraction from a known thread ID.
- ✅ Demo: use `read_thread` with thread_id + goal.
- Gaps: no built-in discovery step when ID is unknown.

## Automation run logs
- What exists: JSONL run history with timestamps and thread IDs.
- ✅ Demo: `zdx automations runs`.
- Gaps: limited filtering + no JSON mode for automation pipelines.

# MVP slices (ship-shaped, demoable)
Define Slice 1..N in user-journey order.

## Slice 1: Date-based thread discovery
- **Goal**: Find threads worked on a specific day/range.
- **Scope checklist**:
  - [x] Add `zdx threads search` command.
  - [x] Add `--date`, `--date-start`, `--date-end`, `--limit`.
  - [x] Keep search read-only.
- **✅ Demo**: `zdx threads search --date 2026-02-12` returns matching thread IDs.
- **Risks / failure modes**:
  - Date semantics confusion (worked-on/activity timestamp).

## Slice 2: Topic search for recall
- **Goal**: Find where a topic was discussed.
- **Scope checklist**:
  - [x] Add optional text query (`zdx threads search "topic"`).
  - [x] Match title + thread content (messages/reasoning/tool-use input).
  - [x] Add deterministic ranking and previews.
- **✅ Demo**: `zdx threads search "thread search"` returns relevant threads with previews.
- **Risks / failure modes**:
  - Noisy matches for broad terms.

## Slice 3: Automation-grade JSON contract + automations alignment
- **Goal**: Make thread and run discovery scriptable.
- **Scope checklist**:
  - [x] Add `zdx threads search --json` with stable fields.
  - [x] Keep empty-result behavior non-error (prints `[]` in JSON mode).
  - [x] Improve `zdx automations runs` with `--date*` filters and `--json`.
- **✅ Demo**:
  - `zdx threads search "report" --date-start 2026-02-01 --json`
  - `zdx automations runs --date 2026-02-12 --json`
- **Risks / failure modes**:
  - Contract drift in JSON output can break downstream scripts.

# Contracts (guardrails)
List non-negotiable behaviors that must not regress (derived from Inputs and existing behavior).
- Search must be read-only.
- Existing thread commands behavior remains intact.
- Search tolerates malformed thread lines best-effort.
- Sorting is deterministic for same input.
- JSON outputs are stable once shipped.

# Key decisions (decide early)
List only decisions that would cause rework if postponed (derived from Inputs).
- Canonical surface: `zdx threads search`.
- Activity date source: latest event timestamp (fallback to file modified time).
- Ranking mode: relevance first when query is provided; recency first otherwise.
- JSON fields: include thread ID/title/activity/score/preview for automation use.

# Testing
- Manual smoke demos per slice
- Minimal regression tests only for contracts
- Added CLI integration coverage for query/date/JSON search flows

# Polish phases (after MVP)
Group improvements into phases, each with a ✅ check-in demo.
Limited strictly to scope present in Inputs.

## Phase 1: Tool exposure
- Add a `thread_search` tool wrapper that reuses this search core/contract.
- ✅ Check-in demo: model discovers thread IDs via tool, then deep-reads with `read_thread`.

## Phase 2: Memory-system integration
- Integrate search results into memory/report automations.
- ✅ Check-in demo: “recent work” automation retrieves candidate threads by date/topic before synthesis.

# Later / Deferred
Explicit list of "not now" items + what would trigger revisiting them.
- Vector/semantic retrieval → revisit if keyword recall is insufficient.
- Auto-memory extraction → revisit after manual/automation memory loops stabilize.