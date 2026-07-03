# Status
- **MVP complete (Slices 1–4), verified, uncommitted.** `just ci` / `just test` green (1230 tests) as of 2026-07-01.
  - Slice 1 — `zdx stats` + shared cost helper: ✅ done.
  - Slice 2 — model+provider on usage events: ✅ done.
  - Slice 3 — fork dedup: ✅ resolved (no dedup needed; guard test added).
  - Slice 4 — monitor `Usage` tab: ✅ done.
- **Remaining is post-MVP:** Polish Phase 1 (`--json` + time ranges, CLI), Phase 2 (per-project breakdown + cache savings, CLI **and** monitor), Phase 3 (fast incremental stats / derived SQLite cache — **recommended next** given current scan slowness), Phase 4 (latency: tok/s + TTFT, CLI **and** monitor), plus the Later / Deferred list.

# Goals
- Let the user see token usage and USD cost broken down **per provider** and **per model**, derived from saved threads.
- Attribute usage correctly when the model/provider is switched mid-thread.
- Avoid double-counting usage across forked/`/btw`/handoff threads (verified: none copy the parent's usage events, so no dedup is required — see Slice 3).
- Record the model **and provider** on each usage event so any saved thread can answer "which model/provider produced this usage" (also surfaced in `zdx threads show`).
- Surface the breakdown in two read-only places: a `zdx stats` CLI summary and a `Usage` tab in `zdx-monitor`.

# Non-goals
- Migrating canonical thread storage from JSONL to SQLite. JSONL stays the source of truth.
- Latency / tokens-per-second / TTFT metrics (zdx does not record per-request timing today; deferred).
- A web dashboard or charts (oh-my-pi style). Terminal-native only.
- Retroactive correctness for *old* threads' mid-thread switches (only data recorded after Slice 2 is per-request attributable).
- **Capturing subagent/helper cost in MVP.** Subagents and internal helpers (explorer, oracle, title-gen, TLDR, handoff, prompt builder) always run `zdx exec --no-thread` (`crates/zdx-engine/src/core/subagent.rs:156`), so their usage is **never persisted to thread JSONL**. Thread-scanning stats therefore *under-count* total spend. Capturing it needs a separate usage ledger (see Later / Deferred).
- **`zdx imagine` image-generation spend.** It calls image APIs and writes artifacts, not thread usage events (`crates/zdx-cli/src/cli/commands/imagine.rs`), and `ModelPricing` is per-token only — image spend is neither captured nor representable.
- **Exact USD for subscription/OAuth providers.** Subscription providers (`ProviderKind::is_subscription`, e.g. `claude-cli`, `openai-codex`) are flat-rate; their token-priced figure is not real spend. Stats report them as `subscription`, not as billed USD (see Key decisions).

# Dependencies & sequencing
- This plan **subsumes** the former `docs/plans/active/thread-model-metadata.md` (now removed). That plan's schema work — recording model on each `usage` event — is folded into **Slice 2** here, with its open decision #1 resolved in favor of storing **provider too** (a per-provider cost feature cannot derive provider from a bare model id: `resolve_provider("claude-opus-4-6")` defaults to Anthropic even if it ran via `claude-cli`, `crates/zdx-providers/src/lib.rs:535`).
- Ship-first ordering is preserved: Slice 1 ships an *estimated* summary over existing data; Slice 2 makes it accurate for new data; Slice 3 confirmed forks need no dedup; Slice 4 mirrors it in the monitor.

# Design principles
- User journey drives order: get a visible summary first, then make it accurate.
- JSONL is the source of truth; stats are a **derived, read-only** view computed by scanning thread files.
- One cost/aggregation code path, reused by CLI, monitor, TUI, and bot (no duplicated math).
- All schema changes are **additive** (serde defaults), so old transcripts keep loading; **no `SCHEMA_VERSION` bump**.

# User journey
1. User runs zdx normally — multiple providers/models, occasional mid-thread model switch, occasional fork (`/btw`, timeline fork) or handoff.
2. User wants to know "how much have I spent, and on which providers/models?"
3. User runs `zdx stats` (or opens the monitor `Usage` tab).
4. User sees overall totals plus a per-provider and per-model table (requests, tokens, cost) that is correctly attributed and not inflated by forks.

# Foundations / Already shipped (✅)
Capabilities that already exist and must be reused, not rebuilt.

## Per-request usage persistence
- What exists: each turn appends a `Usage` event (`input/output/cache_read/cache_write` token counts + `ts`) to the thread JSONL; cumulative totals derive by summing. `crates/zdx-engine/src/core/thread_persistence.rs:38` (struct) and `ThreadEvent::Usage` at `:203`. Persisted by `UsagePersistor` in `spawn_thread_persist_task` (`:982`), which sees only the `AgentEvent` stream.
- ✅ Demo: open any thread `.jsonl` in `threads_dir()` and see `{"type":"usage",...}` lines.
- Gaps: no `model`/`provider` on the event (fixed in Slice 2).

## Pricing + cost math
- What exists: `ModelPricing` (per-million-token prices) at `crates/zdx-engine/src/models.rs:25`; `provider_for_model`/`resolve_provider` at `crates/zdx-providers/src/lib.rs:535`; `ProviderKind::is_subscription()`/`id()`/`from_id()` at `crates/zdx-providers/src/lib.rs:190`; model lookup `ModelOption::find_by_id` / `find_by_provider_and_id` at `crates/zdx-engine/src/models.rs:96`.
- ⚠️ There are **three** cost formulas today: TUI `calculate_cost`/`cache_savings` (`crates/zdx-tui/src/features/thread/state.rs:230`), and the bot's `calculate_usage_cost`/`calculate_cache_savings` (`crates/zdx-bot/src/handlers/message.rs:1883`). Slice 1 consolidates all into one engine helper.

## Monitor TUI shell
- What exists: tabbed dashboard via `Section` enum (`crates/zdx-monitor/src/app.rs:236`), render dispatch (`crates/zdx-monitor/src/ui.rs:18`), thread scanning (`load_threads` at `:800`). Note `refresh_app` runs every 1s tick **and** after every keypress (`:786`); `r` already means "restart service".
- ✅ Demo: `just monitor` → Tab cycles Services/Threads/Config/etc.
- Gaps: no Usage tab.

## CLI subcommand pattern
- What exists: clap `Commands` enum (`crates/zdx-cli/src/cli/mod.rs:76`), routed in `dispatch_command` (`:838`), modules registered in `crates/zdx-cli/src/cli/commands/mod.rs`; read-only template `commands/threads.rs:54`.
- ✅ Demo: `zdx threads list`.

## Thread scope (already global)
- All surfaces write to the single `$ZDX_HOME/threads` dir: TUI, `zdx exec`, the Telegram bot (`telegram-{chat_id}` thread ids), and automations (`automation-<name>-<ts>`). `threads_dir()` at `crates/zdx-engine/src/config.rs:333`; `list_threads()` at `crates/zdx-engine/src/core/thread_persistence.rs:1652`. Stats are therefore **global across all surfaces** — state this in output.
- Concurrent-append safe: `read_events` skips a malformed/partial last line (`:644`), so scanning a live thread won't crash.

# MVP slices (ship-shaped, demoable)

## Slice 1: `zdx stats` summary + shared cost helper (estimated / legacy) — ✅ DONE
- **Goal**: A working summary today, scanning current JSONL with no schema changes. Numbers are **estimated** until Slice 2/3 land — ship it labeled as such.
- **Scope checklist**:
  - [x] Relocate cost math into `zdx-engine` next to `ModelPricing` as pure helpers (e.g. `models::calculate_cost(usage, pricing)` + `cache_savings`). Update **all three** callers to delegate: TUI (`state.rs:230`), bot (`message.rs:1883`), and the new aggregator. (Layering is safe: tui/monitor/cli/bot all depend on `zdx-engine`, not vice versa.)
  - [x] New shared aggregator module in `zdx-engine` (e.g. `crates/zdx-engine/src/core/usage_stats.rs`): reuse `list_threads()` + `load_thread_events()` (no hand-rolled dir scans), sum `Usage` events per thread.
  - [x] **Resilience**: return `StatsResult { totals, warnings: Vec<…> }`; skip any thread that fails to read/parse and record a warning rather than aborting the whole scan.
  - [x] **Subscription handling**: classify each row via `ProviderKind::from_id(provider).is_subscription()`. Subscription providers report tokens + `subscription` (no billed USD), excluded from the billed-USD total. Non-subscription with known pricing → USD. Missing pricing → "cost unknown" bucket (never panic).
  - [x] Best-effort attribution for old data: attribute a thread's usage to its `model_override` (existing meta helper), falling back to the config default model; resolve provider via `provider_for_model`.
  - [x] `zdx stats` subcommand: add `Stats` variant to `Commands` (`mod.rs:76`), route in `dispatch_command`, add `commands/stats.rs` modeled on `commands/threads.rs`.
  - [x] Print: overall totals (requests, tokens, billed USD, subscription tokens) + per-provider table + per-model table + a banner: "Estimated — global across all ZDX threads; old usage lacks per-request model/provider; subagent/helper + image spend excluded; subscription providers shown as flat-rate."
- **✅ Demo**: `zdx stats` prints overall totals and per-model/per-provider breakdowns that sum to the overall total, with the estimated/scope banner shown, and does not crash on an unreadable or unknown-model thread.
- **Risks / failure modes**:
  - Known inaccuracy until Slice 2 (mis-attributed model switches for *old* data) can make **dollar totals wrong** — hence the banner. New data (post-Slice-2) attributes per request.

## Slice 2: Record model + provider on usage events (folded schema work) — ✅ DONE
- **Goal**: New usage is attributed to the exact model/provider that produced it, even across mid-thread switches; the aggregator consumes it.
- **Scope checklist**:
  - [x] Add `model: String` + `provider: String` to `AgentEvent::UsageUpdate` (`crates/zdx-types/src/events.rs:136`). Provider string = `ProviderKind::id()`.
  - [x] Carry `provider` on `RunTurnSetup` (currently model-only) — it's already resolved as a local `ProviderKind` in `build_run_turn_setup` (`crates/zdx-engine/src/core/agent.rs:1051`) — and onto `StreamState`, so `flush_pending_usage` (`:1185`) emits both directly rather than re-resolving from config.
  - [x] Add `model: Option<String>` + `provider: Option<String>` to the persisted `Usage` ThreadEvent (`thread_persistence.rs:203`) with `#[serde(default, skip_serializing_if = "Option::is_none")]`. Update the `ThreadEvent::usage(...)` constructor signature to accept them.
  - [x] `UsagePersistor`: replace loose `pending: Option<Usage>` with `pending: Option<PendingUsage { usage, model, provider }>` and a `current_model`/`current_provider` carry, so **every** flush path — including `finish()` (`thread_persistence.rs:~1107`) and output-only usage — attaches the right metadata. (The persistor only sees the event stream; metadata must ride the event.)
  - [x] Surface in `zdx threads show`: `format_transcript` currently skips `Usage` events (`thread_persistence.rs:2232`); derive a "Models used: …" line (folded from the former thread-model-metadata plan).
  - [x] Aggregator resolution order: (a) event has provider → `find_by_provider_and_id(provider, model)`; (b) model string is provider-qualified → parse + resolve; (c) bare id → `find_by_id`, mark row **estimated**.
- **Blast radius** (must update or use `..`): exhaustive `UsageUpdate` destructure in TUI (`crates/zdx-tui/src/features/transcript/update.rs:178`) and engine unit tests; `ThreadEvent::usage()` call sites (3 prod in the persistor + ~5–8 tests in `thread_persistence.rs` ~3106–3190); the subagent test fixture string (`subagent.rs:460`).
- **Docs**: update `docs/SPEC.md` §8 to note `usage` carries optional `model`/`provider`; add a short usage-stats data-flow note to `docs/ARCHITECTURE.md`; update `crates/zdx-engine/AGENTS.md` (new `core/usage_stats.rs`) and `crates/zdx-cli/AGENTS.md` (new `commands/stats.rs`).
- **✅ Demo**: in one thread, switch model mid-conversation, send a turn on each; `zdx stats` shows two distinct model/provider rows with the right split; `zdx threads show <id>` lists the models used. A round-trip test (emit `UsageUpdate{model,provider}` → persist → reload) preserves both fields.
- **Risks / failure modes**:
  - Schema must stay additive — verify an old transcript (no model/provider on usage) still loads and aggregates via fallback. No `SCHEMA_VERSION` bump.
  - Usage may arrive in multiple fragments per turn — attaching model/provider to *every* usage event (not a separate "model changed" event) keeps aggregation order-insensitive.

## Slice 3: Fork de-duplication — RESOLVED (no dedup needed)
- **Finding (verified in code + test)**: the dedup this slice was designed for is unnecessary. Forks do **not** copy the parent's usage. The timeline fork (`ForkThread`/`ForkThreadAsTab` → `fork_thread_sync`) is built from `cells_to_events(&cells)` (`crates/zdx-tui/src/overlays/timeline.rs:317`), which only reconstructs message/reasoning/tool events from display cells — it emits no `Usage` (and no `Meta`) events. So a forked thread's file contains only its own usage, exactly like `/btw` (`messages_to_events`) and handoff (fresh thread, no copy). The earlier premise ("`fork_thread_sync` appends raw events including `Usage`") was wrong: the events it appends are display-derived, not raw thread JSONL.
- **What was done**: added a guard test `cells_to_events_never_emits_usage_or_meta` (`crates/zdx-tui/src/overlays/timeline.rs`) locking the invariant that fork/`/btw` context reconstruction never carries usage/meta, so a future change can't silently reintroduce double-counting.
- **Not done (intentionally)**: no `Meta.fork_from`/`inherited_event_count`, no aggregator skip logic, no `LineageBoundary` event — all would be dead code with nothing to skip. Revisit only if fork is ever changed to persist raw thread events (incl. `Usage`); the guard test would need to move to `fork_thread_sync` at that point.
- **✅ Demo**: `cargo nextest run -p zdx-tui cells_to_events` passes; aggregating a forked thread adds only its own new usage (parent's usage stays counted once in the parent file).

## Slice 4: `Usage` tab in zdx-monitor — ✅ DONE
- **Goal**: Same breakdown visible in the live monitor dashboard.
- **Scope checklist**:
  - [x] Add `Usage` to `Section`, `Section::ALL`, label, and `next()` (`crates/zdx-monitor/src/app.rs`). Placed between `Threads` and `Automations`.
  - [x] Add `Section::Usage => render_usage(...)` (`crates/zdx-monitor/src/ui.rs`) + footer hint.
  - [x] **Refresh mechanism** (monitor refreshes every 1s tick + on keypress): store `usage_stats: Option<CachedUsageStats { stats, computed_at }>`; recompute only when stale or via a dedicated refresh key, **not** on every tick. Added `R` (Shift+R) as the refresh key (distinct from `r` = restart service).
  - [x] Update `crates/zdx-monitor/AGENTS.md` if new files are added → no new files, so left unchanged.
- **Implemented as**:
  - `render_usage` builds the same overall/per-provider/per-model tables as `zdx stats` (shared `usage_stats::aggregate_usage`), rendered as a **scrollable Paragraph** (modeled on `render_config` for scroll, not `render_threads`) with `j/k`, PgUp/PgDn, and mouse-wheel scrolling.
  - Compute is **lazy on first entry** to the tab (not at startup, to avoid scanning all threads at launch); while the tab is active it auto-refreshes only when the cache is older than `USAGE_STALE_AFTER = 30s`; `R` forces an immediate recompute. A one-time scan failure keeps the previous cache and surfaces a status message.
  - Attribution uses `default_model` captured from `config.model` at monitor startup.
- **✅ Demo**: `just monitor` → Tab to `Usage` → see overall + per-provider/per-model tables matching `zdx stats`, without per-second rescans.
- **Risks / failure modes**:
  - Full rescan on every tick would be slow with many threads — the cache mechanism above is required, not optional.

# Contracts (guardrails)
- JSONL remains the canonical thread store; stats never write to or mutate thread files.
- All new event/meta fields are additive with serde defaults; existing transcripts must still load and render; **no `SCHEMA_VERSION` bump**.
- Fork/handoff/`/btw` replay, context-window math, and existing per-thread TUI cost display must not regress.
- Cost is computed only through the shared `zdx-engine` cost helper — TUI, bot, CLI, and monitor all delegate; no second formula remains.
- Subscription/OAuth providers are reported as `subscription`, never summed into billed USD.
- The aggregator never aborts on one bad thread file — it skips and warns.
- `zdx stats` and the monitor `Usage` tab produce identical numbers for the same data.

# Key decisions (decided)
- **Provider is stored** on the usage event (`model` + `provider` both), not derived from a bare id — bare ids are provider-ambiguous. Resolved against the former metadata plan's open decision #1.
- **Attribution carrier**: `model`+`provider` directly on every usage event (order-insensitive across fragmented updates), not a separate model-change event.
- **Cost helper consolidation includes the bot** — all three current formulas collapse into one engine helper.
- **Subscription display**: subscription providers shown as flat-rate `subscription` (tokens only), excluded from billed-USD total; an optional token-equivalent estimate may be shown parenthetically. Note: historical data can't distinguish Anthropic API-key vs OAuth (only provider kind is stored), so the `is_subscription` provider-kind flag is the classifier.
- **Fork dedup**: not needed (Slice 3 resolved). Forks reconstruct context from display cells (`cells_to_events`), which carry no usage events, so nothing is double-counted. A guard test locks the invariant; no lineage metadata or skip logic was added.
- **Handoff / `/btw`**: also carry no inherited usage (fresh thread / `messages_to_events`); no dedup.
- **Aggregator location**: one shared `zdx-engine` module consumed by CLI and monitor.
- **Storage**: on-demand JSONL scan for the MVP; a derived SQLite cache with incremental offset+mtime sync is now **promoted to Phase 3** (triggered by measured slowness at ~5k+ threads). JSONL stays canonical.
- **Slice 1 framing**: ship as explicitly "estimated/legacy"; drop the banner once Slice 2 makes new data attributable (Slice 3 needed no change — forks don't double-count).

# Testing
- Manual smoke demos per slice (the ✅ Demo lines above).
- Engine-level aggregator tests:
  - Old usage event without model/provider → aggregates via fallback, row marked estimated.
  - New events split by per-event provider/model.
  - Subscription provider → bucketed as subscription, excluded from billed USD.
  - Unknown model → "cost unknown" bucket, no panic.
  - Malformed JSON line skipped; unreadable thread file skipped with a warning (scan still completes).
  - Fork/`/btw` context reconstruction (`cells_to_events`) carries no usage events (guard test), so forks add only their own usage — no double-count.
  - Round-trip: `UsageUpdate{model,provider}` → persist → reload preserves both.
- CLI integration test in `crates/zdx-cli/tests/` asserting `zdx stats` output structure; assert CLI and monitor call the same aggregator (no separate math).
- Verification: `just ci-fast` during iteration; `cargo nextest run -p zdx-engine` for persistence/round-trip/dedup tests; `just test` before wrapping up.

# Polish phases (after MVP)

## Phase 1: Time ranges + scripting
- Add `--json` output and time-range filtering (e.g. `--since 7d`) bucketed by day, using each usage event's RFC3339 `ts` (already present); skip/warn on malformed timestamps.
- ✅ Check-in demo: `zdx stats --since 7d --json` returns a machine-readable per-day, per-model breakdown.

## Phase 2: Richer breakdowns (CLI **and** monitor)
- **Goal**: Add a per-project/folder breakdown and a cache-hit savings figure, surfaced identically in `zdx stats` and the monitor `Usage` tab (same shared aggregator, no second math).
- **Scope checklist**:
  - [ ] **Aggregator (shared)**: extend `UsageStats` with a `by_project: Vec<UsageRow>`-style breakdown keyed off `meta.root_path` (with an "unknown project" bucket for old/missing roots), and a cache-savings total on `UsageTotals` computed via the consolidated cost helper (`cache_savings`). All new fields live once in `crates/zdx-engine/src/core/usage_stats.rs`.
  - [ ] **CLI render**: `crates/zdx-cli/src/cli/commands/stats.rs` prints the cache-savings figure in the overall block and a new "By project:" table.
  - [ ] **Monitor render**: extend `build_usage_lines` in `crates/zdx-monitor/src/ui.rs` to add the cache-savings figure to the totals block and a "By project:" table (reuse the existing `usage_table(...)` helper). Scroll math needs no change — `usage_line_count` is derived from the same lines.
  - [ ] Keep numbers identical across both surfaces (existing contract) and keep the per-provider/per-model tables unchanged.
- **✅ Check-in demo**: `zdx stats` **and** `just monitor` → `Usage` tab both show a cache-savings figure and a per-project split, with matching numbers.
- Note: a per-agent-type (main/subagent/advisor) breakdown is **not** included here — subagent/advisor transcripts aren't persisted; it depends on the deferred usage ledger.

## Phase 3: Fast, incremental stats (derived SQLite cache) — recommended next
- **Goal**: Keep aggregation fast at thousands of threads and stop the monitor freezing. JSONL stays canonical; SQLite is a **derived, disposable cache only** (consistent with Non-goals — this is not a storage migration). Prompted by real slowness at ~5k+ threads: today `aggregate_usage` fully parses every event of every thread synchronously on the monitor's UI thread.
- **Scope checklist**:
  - [ ] **Don't block the UI**: run `aggregate_usage` off the monitor event loop (worker thread + channel); render a "Computing… (N threads)" placeholder and swap in the result when ready. The dashboard must stay live during a scan.
  - [ ] **Lean scan**: read thread JSONL line-by-line and only parse `usage`/`meta` (peek `"type"`, deserialize a minimal struct), skipping the large message/reasoning/tool payloads that the aggregator currently deserializes and discards.
  - [ ] **Incremental cache**: persist per-thread partial aggregates in a derived SQLite db (e.g. `$ZDX_HOME/cache/usage.sqlite`) keyed by `(thread_id, mtime, size)` with a stored byte offset; on refresh only re-scan threads whose mtime/size changed and resume from the last fully-parsed offset. Rebuild transparently if the cache is missing/corrupt.
  - [ ] Both `zdx stats` and the monitor read through the same cached path; first run backfills, later runs are incremental. Drop the fixed 30s auto-refresh in favor of cheap incremental refresh (plus manual `R`).
- **✅ Check-in demo**: with thousands of threads, first `zdx stats` builds the cache; a second run and the monitor `Usage` tab return near-instantly, re-scanning only changed threads; opening the tab never freezes the dashboard.
- **Risks**: cache-invalidation correctness — append-only threads mean the offset-resume must handle a skipped partial last line (as `read_events` already does); keep JSONL authoritative and treat the cache as safe to delete.

## Phase 4: Latency metrics (tokens/sec, TTFT) — CLI **and** monitor
- **Goal**: Compare provider/model speed via tokens-per-second and time-to-first-token, surfaced in `zdx stats` and the monitor `Usage` tab.
- **Scope checklist**:
  - [ ] **Schema (additive)**: record per-request timing on the usage event — e.g. `duration_ms` (request wall time) + `ttft_ms` (time to first token) — as optional fields with serde defaults (no `SCHEMA_VERSION` bump; old transcripts load as `None`). Emit where usage is flushed (`flush_pending_usage` / stream state), reusing the model/provider carry added in Slice 2.
  - [ ] **Aggregator (shared)**: derive tok/s (`output_tokens / duration`) and a TTFT summary (e.g. median) per provider/model in `usage_stats.rs`, ignoring rows that lack timing.
  - [ ] **Render (both surfaces)**: add tok/s + TTFT to the `zdx stats` tables and to `build_usage_lines` in the monitor; show `—` where timing is absent.
- **✅ Check-in demo**: after new turns, `zdx stats` and the monitor `Usage` tab show tok/s and TTFT per model/provider; pre-change usage shows `—` and never breaks aggregation.
- **Note**: like Slice 2, only data recorded after this lands is measurable; old usage has no timing.

# Later / Deferred
- **Subagent/helper usage ledger** — capture cost from `--no-thread` child execs (subagents + internal helpers) via a separate append-only usage ledger or a parent-thread usage event. Unlocks the per-agent-type breakdown. Revisit when total-spend accuracy matters more than per-thread breakdown.
- **Image-generation cost** — `zdx imagine` spend; needs a non-token pricing unit and a usage sink. Revisit if image spend matters.
- **`meta.model` mirror for fast listing** — a thread-level model badge in `threads list`/picker without scanning events (former metadata plan's Phase 3). Defer unless list-level display is needed.
- **Backfilling old multi-model threads** — only resolvable heuristically; revisit only if historical accuracy is requested.
- **Error-rate metric** — depends on persisting per-request stop reason/errors alongside usage; revisit if reliability comparison is wanted.
- **Anthropic API-key vs OAuth distinction** in historical data — not stored today; revisit only if it materially changes reported spend.
