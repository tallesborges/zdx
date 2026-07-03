# Status
- **Slices 1 & 2 + Phase 1 implemented + verified (2026-07-03).** All child execs (subagents + the five helpers) persist tagged threads; tagged threads are hidden by default from `threads list`, TUI picker, `thread_search`, monitor, and memory export (`--all` includes them), while `zdx stats` still counts them. `zdx threads show` renders lineage + per-child cost. `cargo clippy --workspace --all-targets --all-features -D warnings` clean; full suite green (1239 tests, +3 new) with an isolated `ZDX_HOME`.
  - Slice 1 — persist all child threads + lineage meta: ✅ done.
  - Slice 2 — default-hide tagged threads across listing surfaces + `threads list --all`: ✅ done.
  - Phase 1 — parent→child lineage in `zdx threads show` (parent link + child-runs section with per-child cost): ✅ done.
  - Remaining: Phase 2 (per-agent-type usage breakdown in `zdx stats`/monitor), post-MVP.

# Goals
- Persist subagent runs as real thread JSONL instead of throwing them away with `--no-thread`, so their token usage/cost is captured by the existing thread-scanning stats (`crates/zdx-engine/src/core/usage_stats.rs`) with **no separate ledger**.
- Tag each persisted child thread with lineage in its `Meta` — parent thread id, run kind, and (for named subagents) the subagent name — so threads are filterable and relatable.
- Keep default thread listings/pickers clean: subagent and helper threads are **hidden by default** but always **counted in stats** and available for drill-down.
- Reuse the existing thread types (`title`, `handoff_from`, `root_path`, usage events) rather than inventing a parallel store.

# Non-goals
- Migrating canonical thread storage away from JSONL. JSONL stays the source of truth (consistent with `usage-stats-monitor.md`).
- Nesting/tree UI for parent→child threads in the TUI picker. Slice 2 only needs a default-hide filter + an opt-in to show them; a lineage tree is a later polish.
- Auto-generating `title`/TLDR for subagent/helper threads. Those are triggered only from the TUI main loop and stay off for child threads (avoids extra spend + recursion).
- Changing what `invoke_subagent` returns to the parent (still response text only) or how the active-agents registry works.
- Bumping `SCHEMA_VERSION`. All new `Meta` fields are additive with serde defaults.

# Design principles
- User journey drives order: capture the biggest, clearest cost first (user-visible subagents), then make listings clean, then optionally extend to the small/noisy helper runs.
- The persistence path already exists — **remove the block, don't build a new writer**. `run_exec` persists usage/messages whenever a thread is present; `--no-thread` is the only thing suppressing it.
- Lineage lives in `Meta` (persisted), distinct from the ephemeral active-agents registry (which already carries the same values at runtime).
- Every schema change is additive (serde defaults); old transcripts keep loading; no `SCHEMA_VERSION` bump.
- Filtering is a **read/display** concern (listings, pickers, search, memory export). Stats never filter — they count everything.

# User journey
1. User (or the main agent) invokes a subagent (`explorer`, `oracle`, `task`, `thread-searcher`) via `invoke_subagent`.
2. The child run now writes its own thread JSONL under `$ZDX_HOME/threads`, tagged with its parent thread id + subagent name + `kind = subagent`.
3. User runs `zdx stats` (or the monitor `Usage` tab) and sees subagent spend included in the totals — no more under-count.
4. User opens the thread picker / `zdx threads list` and does **not** see the subagent threads cluttering the list; an opt-in flag surfaces them, and each links back to its parent.

# Foundations / Already shipped (✅)

## Child exec persistence path
- What exists: `run_exec` builds a persist task (`spawn_thread_persist_task`) and logs user/assistant/usage events **whenever `thread` is `Some`** (`crates/zdx-cli/src/modes/exec.rs`, thread plumbing around the broadcaster/persist spawn). `ThreadPersistenceOptions::resolve` returns `None` only when `no_save` is set (`crates/zdx-engine/src/core/thread_persistence.rs:2374`).
- ✅ Demo: `zdx exec -p "hi"` (without `--no-thread`) already writes a thread JSONL with `usage` events carrying `model`/`provider` (post usage-stats Slice 2).
- Gaps: the subagent runner always injects `--no-thread` (`crates/zdx-engine/src/core/subagent.rs:159`), so no child ever persists.

## Runtime lineage already flows parent→child
- What exists: `invoke_subagent` passes `track_activity: true`, `activity_kind = "subagent"`, `activity_parent_thread_id = ctx.current_thread_id`, `activity_subagent_name` (`crates/zdx-engine/src/tools/subagent.rs:277-292`), forwarded as hidden exec flags `--activity-kind` / `--activity-parent-thread-id` / `--activity-subagent-name` (`crates/zdx-engine/src/core/subagent.rs:205-217`, parsed in `crates/zdx-cli/src/cli/mod.rs:132-143` and dispatched `:838-842`).
- ✅ Demo: `just monitor` shows a running subagent with its parent + name in the active-agents view.
- Gaps: these values feed only the **ephemeral** registry (`crates/zdx-engine/src/core/agent.rs:850-859`); nothing writes them into the persisted `Meta`.

## Thread Meta + listing
- What exists: `ThreadEvent::Meta` carries `title`, `root_path`, `handoff_from`, `model_override`, `thinking_override`, `pending_topic_title` (`crates/zdx-engine/src/core/thread_persistence.rs:120-139`); `ThreadMeta` parses them (`:906`); `ThreadSummary` exposes `handoff_from` (`:1182`); `list_threads()` returns every file (`:1757`).
- ✅ Demo: `zdx threads list` shows all threads with titles.
- Gaps: no generic `parent_thread_id`/`kind`/`subagent_name`; no way to filter a kind out of listings, pickers, `thread_search`, or memory export.

## Usage aggregation over threads
- What exists: `usage_stats::aggregate_usage` scans all thread JSONL and sums `usage` events per `(provider, model)`, backed by the incremental SQLite cache (`crates/zdx-engine/src/core/usage_stats.rs`). Child threads share the parent's `--root`, so `meta.root_path` groups them under the same project.
- ✅ Demo: `zdx stats` totals + per-provider/per-model tables.
- Gaps: subagent usage is absent today only because those threads don't exist on disk — fixed by Slice 1. No double-count risk: the parent turn never recorded the child's tokens.

# MVP slices (ship-shaped, demoable)

## Slice 1: Persist all child exec threads + lineage meta — ✅ DONE
- **Goal**: Every child `zdx exec` run (subagents **and** the five helpers) writes a tagged thread JSONL; `zdx stats` immediately reflects their spend.
- **Scope checklist**:
  - [x] Remove the hard-coded `--no-thread` from `build_exec_args` (`crates/zdx-engine/src/core/subagent.rs`); child runs now persist by default.
  - [x] Add lineage fields to `ExecSubagentOptions` (`crates/zdx-engine/src/core/subagent.rs`): `thread_origin_kind`, `thread_parent_id`, `thread_subagent_name`. `build_exec_args` emits them as new hidden global flags.
  - [x] Add additive `Meta` lineage fields to `ThreadEvent::Meta` + the parsed `ThreadMeta` + `ThreadSummary`: `origin_kind`, `parent_thread_id`, `subagent_name`, each `#[serde(default, skip_serializing_if = "Option::is_none")]`. Threaded through the `Thread` struct (`set_origin`) + `ensure_meta` write path (mirrors `handoff_from`) via a new `ThreadEvent::meta_with_lineage` constructor.
  - [x] Add hidden **global** flags (`--thread-origin-kind`, `--thread-parent-id`, `--thread-subagent-name`) on `ThreadArgs` (`crates/zdx-cli/src/cli/mod.rs`); they flow `ThreadArgs → ThreadPersistenceOptions → resolve() → Thread::set_origin → ensure_meta`. No change needed in `commands/exec.rs`/`modes/exec.rs` — the existing persistence path handles the rest.
  - [x] Set lineage at every caller:
    - `invoke_subagent` (`tools/subagent.rs`): `origin_kind = "subagent"`, `parent_thread_id = ctx.current_thread_id`, `subagent_name` = named subagent / `task`.
    - `read_thread` (`tools/read_thread.rs`): `origin_kind = "helper:read_thread"`, parent = `ctx.current_thread_id`.
    - `title_generation` / `tldr_generation` (`core/*.rs`): `origin_kind = "helper:title"` / `"helper:tldr"` (no parent id available from their signatures).
    - `handoff` / `prompt_builder` (`zdx-tui/src/runtime/*.rs`): `origin_kind = "helper:handoff"` / `"helper:prompt_builder"`.
  - [x] Update the `build_exec_args` unit tests in `subagent.rs` (no longer emit `--no-thread`; add a lineage-flags test).
- **✅ Demo**: from `just run`, invoke `explorer` and trigger a title/tldr; new `$ZDX_HOME/threads/*.jsonl` files appear whose `meta` lines carry `origin_kind` (+ `subagent_name`/`parent_thread_id`); `zdx stats` total rises by those runs' tokens.
- **Risks / failure modes**:
  - Child threads now appear in `zdx threads list`/picker/`thread_search`/memory export until Slice 2 lands — Slice 2 immediately follows.
  - Helpers are frequent/tiny (title runs on ~every first message) → many small files; acceptable per decision, revisit pruning in Later.
  - `run_turn` must emit `usage` with `model`/`provider` in the child (already true post usage-stats Slice 2) or stats can't attribute it.

## Slice 2: Filter tagged threads out of default listings — ✅ DONE
- **Goal**: Subagent threads stop cluttering default surfaces while staying counted in stats and reachable on demand.
- **Scope checklist**:
  - [x] `list_threads()` now filters out threads whose `origin_kind` is set (subagent/helper); added `list_all_threads()` for the full set. This single choke point cleans the TUI picker, `search_threads`, memory export, and `latest_thread_id` resume at once. `ThreadSummary` gained the lineage fields (+ `is_child_run()`).
  - [x] `zdx threads list` gained `--all` to show hidden kinds (annotated `[kind ← parent]`); default hides them. TUI picker + `thread_search` inherit the filter via `list_threads()`.
  - [x] Memory/qmd thread export skips tagged child threads (it iterates the filtered `list_threads()` in `core/thread_export.rs`).
  - [x] The monitor's own `load_threads` (`zdx-monitor/src/app.rs`, reads the dir directly) filters tagged child runs too.
  - [x] Parent link shown when a child thread is listed (`threads list --all`) and opened (`threads show`, Phase 1).
- **✅ Demo**: after Slice 1, `zdx threads list` no longer shows the `explorer` thread; `zdx threads list --all` does, marked as a subagent of its parent; `zdx stats` numbers are unchanged by the filter.
- **Risks / failure modes**:
  - Missed surface still shows child threads — enumerated every `list_threads`/`thread_search`/dir-scan consumer (bot reads threads by explicit id, so it is unaffected).

# Contracts (guardrails)
- JSONL remains canonical; this plan only starts writing more of it and never mutates existing files.
- All new `Meta` fields are additive with serde defaults; existing transcripts still load/render; **no `SCHEMA_VERSION` bump**.
- Stats count **all** threads including tagged children (no double-count: parents never recorded child usage). `zdx stats` and the monitor `Usage` tab stay identical.
- Default thread listings/pickers/search/memory-export must look **unchanged for normal threads**; only tagged child threads are hidden by default.
- Both subagents and helpers are persisted and tagged (helpers were folded into Slice 1, not deferred).

# Key decisions (decided)
- **Slice-1 scope**: persist **all** `--no-thread` child execs — subagents **and** the five helpers — in one shot, each tagged with `origin_kind`. The filter (Slice 2) keeps listings clean; stats count everything.
- **Lineage carrier**: added generic `parent_thread_id`/`origin_kind`/`subagent_name` to `Meta` rather than overloading `handoff_from` (which has handoff-replay semantics).
- **Meta population path**: dedicated hidden **global** flags (`--thread-origin-kind`/`--thread-parent-id`/`--thread-subagent-name`), **not** the `--activity-*` values. Reason: helpers run with `track_activity: false` and therefore emit no activity flags, so reusing them wouldn't tag helper threads. The dedicated flags ride `ThreadArgs → ThreadPersistenceOptions → resolve()` and only apply when creating a new thread.
- **Child thread id**: the child mints a fresh id via `Thread::new_with_root` (no explicit `--thread`); parent linkage lives in meta.
- **Filter location**: filter inside `list_threads()` (single choke point) + `list_all_threads()` escape hatch. Safe because stats scan raw files (`list_thread_files`), not `list_threads()`, so they still count children.

# Testing (as shipped)
- Engine round-trip + filtering (`thread_persistence.rs::test_thread_lineage_roundtrip_and_list_filtering`): `Meta` with `origin_kind`/`parent_thread_id`/`subagent_name` → persist → reload preserves them; a normal thread has `origin_kind: None`; `list_threads()` hides the child while `list_all_threads()` includes it.
- `build_exec_args` (`subagent.rs`): updated existing asserts to drop `--no-thread`; added `build_exec_args_emits_thread_lineage_flags` (lineage flags emitted before the `exec` subcommand).
- Per-thread cost (`usage_stats.rs::thread_usage_stats_scopes_to_one_thread`): `thread_usage_stats` sums only the target thread's usage (a sibling thread does not contribute).
- Manual smoke: crafted parent + `subagent`/`helper:title` children in a temp `ZDX_HOME` → `threads list` hides children, `threads list --all` shows them annotated, `threads show <parent>` renders the child-runs cost table, `threads show <child>` renders the parent-link header.
- Not added: a `crates/zdx-cli/tests/` integration test for `threads list --all` (covered by the engine filter test + manual smoke; add if regressions appear).
- Verification: `cargo clippy --workspace --all-targets --all-features -D warnings` + full `cargo nextest run --workspace` (1239 passed) with an isolated `ZDX_HOME`.
  - Note: `config::tests::test_subagent_available_models_filters_disabled_providers` is non-hermetic — it reads the real `~/.zdx/models.toml` via `available_models()` and fails only when the shell's `ZDX_HOME` points at a populated registry. Unrelated to this plan; worth hardening separately.

# Polish phases (after MVP)

## Phase 1: Parent→child lineage display — ✅ DONE
- `zdx threads show <id>` now displays lineage: a `↳ Child run [kind/name] of <parent>` header when the shown thread is itself a child, and a `── Child runs (N) ──` section listing each spawned subagent/helper with its short id, tokens, and cost (subagents before helpers). Cost reuses the shared `usage_stats` path via a new `thread_usage_stats(thread_id, default_model)` helper (no second cost math).
- **Picker decision:** the live thread picker is intentionally flat (`visible_tree_items` maps to depth-0; `flatten_as_tree` is unused in production), so re-introducing tree nesting was deliberately skipped to avoid fighting that design. `threads show` is the lineage/cost surface; children remain in `threads list --all`.
- ✅ Check-in demo: `zdx threads show <parent>` reveals its subagent children and their individual costs; `zdx threads show <child>` shows its parent link.

## Phase 2: Per-agent-type usage breakdown
- With `origin_kind` persisted, extend `usage_stats` to break spend down by main vs subagent vs helper (the breakdown deferred in `usage-stats-monitor.md` Phase 2), surfaced identically in `zdx stats` and the monitor.
- ✅ Check-in demo: `zdx stats` shows a main/subagent/helper split with matching monitor numbers.

# Later / Deferred
- **Retention/pruning** of tagged child threads (they multiply faster than main threads, especially helpers) — a `zdx threads prune` policy. Revisit if disk/volume becomes a concern.
- **Image-generation cost** (`zdx imagine`) — unchanged non-goal from `usage-stats-monitor.md`; needs non-token pricing.
