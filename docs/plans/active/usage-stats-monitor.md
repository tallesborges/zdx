# Goals
- Let the user see token usage and USD cost broken down **per provider** and **per model**, derived from saved threads.
- Attribute usage correctly when the model/provider is switched mid-thread.
- Avoid double-counting usage from forked threads (which copy the parent's events).
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
- Ship-first ordering is preserved: Slice 1 ships an *estimated* summary over existing data; Slice 2 makes it accurate for new data; Slice 3 removes fork double-counting; Slice 4 mirrors it in the monitor.

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

## Slice 1: `zdx stats` summary + shared cost helper (estimated / legacy)
- **Goal**: A working summary today, scanning current JSONL with no schema changes. Numbers are **estimated** until Slice 2/3 land — ship it labeled as such.
- **Scope checklist**:
  - [ ] Relocate cost math into `zdx-engine` next to `ModelPricing` as pure helpers (e.g. `models::calculate_cost(usage, pricing)` + `cache_savings`). Update **all three** callers to delegate: TUI (`state.rs:230`), bot (`message.rs:1883`), and the new aggregator. (Layering is safe: tui/monitor/cli/bot all depend on `zdx-engine`, not vice versa.)
  - [ ] New shared aggregator module in `zdx-engine` (e.g. `crates/zdx-engine/src/core/usage_stats.rs`): reuse `list_threads()` + `load_thread_events()` (no hand-rolled dir scans), sum `Usage` events per thread.
  - [ ] **Resilience**: return `StatsResult { totals, warnings: Vec<…> }`; skip any thread that fails to read/parse and record a warning rather than aborting the whole scan.
  - [ ] **Subscription handling**: classify each row via `ProviderKind::from_id(provider).is_subscription()`. Subscription providers report tokens + `subscription` (no billed USD), excluded from the billed-USD total. Non-subscription with known pricing → USD. Missing pricing → "cost unknown" bucket (never panic).
  - [ ] Best-effort attribution for old data: attribute a thread's usage to its `model_override` (existing meta helper), falling back to the config default model; resolve provider via `provider_for_model`.
  - [ ] `zdx stats` subcommand: add `Stats` variant to `Commands` (`mod.rs:76`), route in `dispatch_command`, add `commands/stats.rs` modeled on `commands/threads.rs`.
  - [ ] Print: overall totals (requests, tokens, billed USD, subscription tokens) + per-provider table + per-model table + a banner: "Estimated — global across all ZDX threads; old usage lacks per-request model/provider; forked threads may be double-counted; subagent/helper + image spend excluded; subscription providers shown as flat-rate."
- **✅ Demo**: `zdx stats` prints overall totals and per-model/per-provider breakdowns that sum to the overall total, with the estimated/scope banner shown, and does not crash on an unreadable or unknown-model thread.
- **Risks / failure modes**:
  - Known inaccuracy until Slice 2/3 (mis-attributed switches, double-counted forks) can make **dollar totals wrong** — hence the banner. Consider landing Slice 1+2 together before treating output as real cost.

## Slice 2: Record model + provider on usage events (folded schema work)
- **Goal**: New usage is attributed to the exact model/provider that produced it, even across mid-thread switches; the aggregator consumes it.
- **Scope checklist**:
  - [ ] Add `model: String` + `provider: String` to `AgentEvent::UsageUpdate` (`crates/zdx-types/src/events.rs:136`). Provider string = `ProviderKind::id()`.
  - [ ] Carry `provider` on `RunTurnSetup` (currently model-only) — it's already resolved as a local `ProviderKind` in `build_run_turn_setup` (`crates/zdx-engine/src/core/agent.rs:1051`) — and onto `StreamState`, so `flush_pending_usage` (`:1185`) emits both directly rather than re-resolving from config.
  - [ ] Add `model: Option<String>` + `provider: Option<String>` to the persisted `Usage` ThreadEvent (`thread_persistence.rs:203`) with `#[serde(default, skip_serializing_if = "Option::is_none")]`. Update the `ThreadEvent::usage(...)` constructor signature to accept them.
  - [ ] `UsagePersistor`: replace loose `pending: Option<Usage>` with `pending: Option<PendingUsage { usage, model, provider }>` and a `current_model`/`current_provider` carry, so **every** flush path — including `finish()` (`thread_persistence.rs:~1107`) and output-only usage — attaches the right metadata. (The persistor only sees the event stream; metadata must ride the event.)
  - [ ] Surface in `zdx threads show`: `format_transcript` currently skips `Usage` events (`thread_persistence.rs:2232`); derive a "Models used: …" line (folded from the former thread-model-metadata plan).
  - [ ] Aggregator resolution order: (a) event has provider → `find_by_provider_and_id(provider, model)`; (b) model string is provider-qualified → parse + resolve; (c) bare id → `find_by_id`, mark row **estimated**.
- **Blast radius** (must update or use `..`): exhaustive `UsageUpdate` destructure in TUI (`crates/zdx-tui/src/features/transcript/update.rs:178`) and engine unit tests; `ThreadEvent::usage()` call sites (3 prod in the persistor + ~5–8 tests in `thread_persistence.rs` ~3106–3190); the subagent test fixture string (`subagent.rs:460`).
- **Docs**: update `docs/SPEC.md` §8 to note `usage` carries optional `model`/`provider`; add a short usage-stats data-flow note to `docs/ARCHITECTURE.md`; update `crates/zdx-engine/AGENTS.md` (new `core/usage_stats.rs`) and `crates/zdx-cli/AGENTS.md` (new `commands/stats.rs`).
- **✅ Demo**: in one thread, switch model mid-conversation, send a turn on each; `zdx stats` shows two distinct model/provider rows with the right split; `zdx threads show <id>` lists the models used. A round-trip test (emit `UsageUpdate{model,provider}` → persist → reload) preserves both fields.
- **Risks / failure modes**:
  - Schema must stay additive — verify an old transcript (no model/provider on usage) still loads and aggregates via fallback. No `SCHEMA_VERSION` bump.
  - Usage may arrive in multiple fragments per turn — attaching model/provider to *every* usage event (not a separate "model changed" event) keeps aggregation order-insensitive.

## Slice 3: Fork de-duplication (fork only; handoff/`/btw` need none)
- **Goal**: Forked threads stop inflating totals; handoff lineage is recorded for UI only.
- **Scope checklist**:
  - [ ] Add `fork_from: Option<String>` + `inherited_event_count: usize` to `Meta` (`thread_persistence.rs:125`), alongside existing `handoff_from`. Update the `meta_with_root*` constructors and direct `Meta { … }` literals (TUI `transcript/build.rs:153,297`; `thread_persistence.rs:~4042`).
  - [ ] In `fork_thread_sync` (`crates/zdx-tui/src/runtime/handlers/thread.rs:392`): after copying the parent prefix, write **fresh** lineage metadata on the new child — `fork_from = parent_id`, `inherited_event_count = events.len()`. Do **not** preserve the parent's own `fork_from`/`inherited_event_count`, or a fork-of-a-fork will skip only the original prefix and double-count the middle segment.
  - [ ] Aggregator skips the first `inherited_event_count` events of any thread with `fork_from`.
  - [ ] Handoff: leave `handoff_from` for lineage/UI but do **not** skip events — handoff creates a fresh thread with no copied parent events (`thread_create` → `Thread::new_with_root_and_source`, `crates/zdx-tui/src/runtime/handlers/thread.rs:316`).
  - [ ] `/btw` side-chats: **no dedup needed and none should be added.** `/btw` persists base context via `messages_to_events(base_messages)` (`crates/zdx-tui/src/runtime/handlers/agent.rs:151`), and `ChatMessage` carries no usage — so a btw thread's file contains **only its own** `Usage` events.
  - [ ] Docs: note `meta.fork_from`/`inherited_event_count` in SPEC §8 metadata.
- **✅ Demo**: fork a thread after some usage, run a new turn in the fork; `zdx stats` total increases only by the fork's *new* usage. Fork the fork and confirm no double-count of the middle segment. Open a `/btw` side-chat, run a turn, confirm its cost is counted exactly once and the parent's prior cost is unchanged.
- **Risks / failure modes**:
  - Must not change fork replay or context-window display (forks still need the full inherited transcript for context).
  - If count-based boundaries prove fragile, fall back to an explicit persisted `ThreadEvent::LineageBoundary { source_thread_id }` after the copied prefix; aggregation counts usage only after the last boundary.

## Slice 4: `Usage` tab in zdx-monitor
- **Goal**: Same breakdown visible in the live monitor dashboard.
- **Scope checklist**:
  - [ ] Add `Usage` to `Section`, `Section::ALL`, label, and `next()` (`crates/zdx-monitor/src/app.rs:236`).
  - [ ] Add `Section::Usage => render_usage(...)` (`crates/zdx-monitor/src/ui.rs:18`) + footer hint; model `render_usage` on `render_threads` (`ui.rs:258`).
  - [ ] **Refresh mechanism** (monitor refreshes every 1s tick + on keypress): store `usage_stats: Option<CachedUsageStats { computed_at }>`; compute lazily on first entry to the Usage tab and at startup; recompute only when stale (interval or thread-dir mtime change), **not** on every tick. Add a dedicated refresh key that doesn't collide with `r` (restart service).
  - [ ] Update `crates/zdx-monitor/AGENTS.md` if new files are added.
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
- **Fork dedup boundary**: `Meta.fork_from` + `inherited_event_count` written fresh on each fork; explicit `LineageBoundary` event is the fallback if nested forks prove fragile.
- **Handoff**: lineage-only (`handoff_from`); no usage dedup.
- **Aggregator location**: one shared `zdx-engine` module consumed by CLI and monitor.
- **Storage**: on-demand JSONL scan now; a derived SQLite cache with incremental offset+mtime sync is deferred until scans are measurably slow.
- **Slice 1 framing**: ship as explicitly "estimated/legacy"; drop the banner once Slice 2+3 land (or land 1+2 together before presenting real cost).

# Testing
- Manual smoke demos per slice (the ✅ Demo lines above).
- Engine-level aggregator tests:
  - Old usage event without model/provider → aggregates via fallback, row marked estimated.
  - New events split by per-event provider/model.
  - Subscription provider → bucketed as subscription, excluded from billed USD.
  - Unknown model → "cost unknown" bucket, no panic.
  - Malformed JSON line skipped; unreadable thread file skipped with a warning (scan still completes).
  - Forked thread with `fork_from` counts only post-fork usage; fork-of-fork doesn't double-count the middle segment.
  - Round-trip: `UsageUpdate{model,provider}` → persist → reload preserves both.
- CLI integration test in `crates/zdx-cli/tests/` asserting `zdx stats` output structure; assert CLI and monitor call the same aggregator (no separate math).
- Verification: `just ci-fast` during iteration; `cargo nextest run -p zdx-engine` for persistence/round-trip/dedup tests; `just test` before wrapping up.

# Polish phases (after MVP)

## Phase 1: Time ranges + scripting
- Add `--json` output and time-range filtering (e.g. `--since 7d`) bucketed by day, using each usage event's RFC3339 `ts` (already present); skip/warn on malformed timestamps.
- ✅ Check-in demo: `zdx stats --since 7d --json` returns a machine-readable per-day, per-model breakdown.

## Phase 2: Richer breakdowns
- Add per-project/folder breakdown via `meta.root_path` (with an "unknown project" bucket for old/missing roots); show cache-hit savings (the consolidated helper).
- ✅ Check-in demo: `zdx stats` shows a cache-savings figure and a per-project split.
- Note: a per-agent-type (main/subagent/advisor) breakdown is **not** included here — subagent/advisor transcripts aren't persisted; it depends on the deferred usage ledger.

# Later / Deferred
- **Subagent/helper usage ledger** — capture cost from `--no-thread` child execs (subagents + internal helpers) via a separate append-only usage ledger or a parent-thread usage event. Unlocks the per-agent-type breakdown. Revisit when total-spend accuracy matters more than per-thread breakdown.
- **Image-generation cost** — `zdx imagine` spend; needs a non-token pricing unit and a usage sink. Revisit if image spend matters.
- **Derived SQLite stats cache** (incremental offset+mtime sync, oh-my-pi style) — revisit when on-demand scans feel slow (many hundreds/thousands of threads).
- **Latency / tokens-per-second / TTFT** — requires recording per-request timing in the usage event; revisit if perf comparison across providers is wanted.
- **`meta.model` mirror for fast listing** — a thread-level model badge in `threads list`/picker without scanning events (former metadata plan's Phase 3). Defer unless list-level display is needed.
- **Backfilling old multi-model threads** — only resolvable heuristically; revisit only if historical accuracy is requested.
- **Error-rate metric** — depends on persisting per-request stop reason/errors alongside usage; revisit if reliability comparison is wanted.
- **Anthropic API-key vs OAuth distinction** in historical data — not stored today; revisit only if it materially changes reported spend.
