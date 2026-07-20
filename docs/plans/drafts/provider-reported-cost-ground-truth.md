> Stage: drafts | active | done | archived. Keep this plan current while working: when a scope item is finished, check its box `[ ]`→`[x]`; when a phase's ✅ demo passes, mark the phase done (with date). The plan file is the source of truth, not memory.

# Goals
- Capture the provider-reported cost that xAI returns in every Responses payload (`usage.cost_in_usd_ticks`, where `1 tick = 1e-10 USD`) and treat it as ground-truth billed cost.
- Make displayed cost (TUI footer, bot `/status`, `zdx stats`, monitor Usage tab) match the provider's actual bill for xAI/grok-build, including turns that cross the >200K higher-context surcharge that registry pricing does not model.
- Fall back to computed registry pricing (`ModelPricing::cost`) unchanged for every provider/record that does not report a cost.

# Non-goals
- Modeling the xAI >200K higher-context tier inside `ModelPricing` (provider-reported cost supersedes the need).
- Populating provider cost for Anthropic / Gemini / OpenAI (they don't send such a field today; the plumbing is generic but stays dormant for them).
- Changing the token→cost formula used for the computed fallback, or the token/cache accounting semantics.
- Backfilling cost onto historical transcripts recorded before this change.

# Design principles
- User journey drives order: capture the number first, then surface it everywhere cost is shown.
- Ground truth beats estimate: when the provider reports cost, prefer it; otherwise compute from the registry exactly as today.
- Exactness + type safety: store cost as `Option<u64>` ticks (not `f64`) so `zdx_types::providers::Usage` keeps its `Eq` derive (`crates/zdx-types/src/providers.rs:280`) and no rounding is introduced until display.
- Backward compatible persistence: the new JSONL field is optional and skipped when absent; old transcripts load and aggregate unchanged.

# User journey
1. User runs a turn on `xai:grok-4.5` (or `grok-build:grok-4.5`).
2. The turn's real billed cost is captured from the provider response and persisted with the usage event.
3. Cumulative/stat surfaces (`zdx stats`, monitor Usage tab) report billed USD from the provider's own number.
4. Live surfaces (TUI footer, bot `/status`) show the exact per-turn cost, including >200K-context turns, instead of an estimate that drifts.

# Foundations / Already shipped (✅)
List capabilities that already exist and should not be rebuilt.

## Usage parsing on the Responses API
- What exists: `usage_from_response()` parses `input_tokens`, `output_tokens`, and `input_tokens_details.cached_tokens` into `zdx_types::providers::Usage` (`crates/zdx-providers/src/openai/responses_sse.rs:100-126`). Shared by the `xai`, `grok-build`, and OpenAI Responses paths.
- ✅ Demo: `ZDX_DEBUG_TRACE=<dir> zdx exec -m xai:grok-4.5 --tools bash -p "run echo hi"` → response `.sse` contains `"usage":{...,"cost_in_usd_ticks":<n>,"input_tokens_details":{"cached_tokens":<n>}}`. Verified: cached field is parsed correctly and `cost_in_usd_ticks / 1e10` equals the computed price to 10 decimals for sub-200K turns.
- Gaps: `cost_in_usd_ticks` is currently discarded.

## Usage transport → persistence → display pipeline
- What exists: provider `Usage`/`UsageDelta` (`crates/zdx-types/src/providers.rs:281-311`) → `StreamState` aggregation (`crates/zdx-engine/src/core/agent.rs:1325-1339`, `1767-1800`) → `AgentEvent::UsageUpdate` (`crates/zdx-types/src/events.rs:137-163`) → persisted `ThreadEvent::Usage` (`crates/zdx-engine/src/core/thread_persistence/event.rs:188-216`) → live TUI footer + JSONL-scanning stats.
- ✅ Demo: run any turn, then `zdx stats` and the TUI footer show token counts and an estimated cost.
- Gaps: no field carries a provider-reported cost end to end; billed USD is always computed (`crates/zdx-engine/src/core/usage_stats.rs:660-692`).

## Computed cost + subscription/unknown handling
- What exists: `ModelPricing::cost()` / `cache_savings()` (`crates/zdx-engine/src/models.rs:40-54`); aggregation marks subscription providers as `$0`, tracks `unknown_pricing_rows` and `cost_known` (`crates/zdx-engine/src/core/usage_stats.rs:663-692`).
- ✅ Demo: `zdx stats` shows billed USD, subscription tokens, and unknown-pricing rows separately.
- Gaps: no provider-cost override path.

# MVP phases (ship-shaped, demoable)

## Phase 1: Capture provider cost end to end (parse → persist)
- **Goal**: The provider's reported cost survives from the raw payload into the persisted usage event; no display changes yet.
- **Scope checklist**:
  - [ ] Add `provider_cost_ticks: Option<u64>` to `zdx_types::providers::Usage` (`crates/zdx-types/src/providers.rs:281-290`). Confirm `Eq` still derives (u64 is fine). Update `is_empty()` to ignore cost, and `From<Usage> for UsageDelta` / `UsageDelta` handling as needed.
  - [ ] Parse `usage.cost_in_usd_ticks` (u64) in `usage_from_response()` (`crates/zdx-providers/src/openai/responses_sse.rs:100-126`) into `provider_cost_ticks`; absent field → `None`. Leave other providers' `Usage` construction returning `None`.
  - [ ] Carry the value through engine aggregation without double counting between `MessageStart` (full `Usage`) and `MessageDelta` (`crates/zdx-engine/src/core/agent.rs:1636-1659`, `1767-1800`) — see Key decisions. Sum across multiple requests in a turn; count each request once.
  - [ ] Add optional `cost_ticks: Option<u64>` to `ThreadEvent::Usage` (`crates/zdx-engine/src/core/thread_persistence/event.rs:188-216`) with `#[serde(default, skip_serializing_if = "Option::is_none")]`; thread it through `ThreadEvent::usage(...)` (`event.rs:358-376`), `UsagePersistor` (`persist.rs:75-112`, `176-189`), and the persistence-layer `Usage` (`event.rs:15-34`) or an added parameter.
  - [ ] Rehydrate `cost_ticks` in `extract_usage_from_thread_events()` (`crates/zdx-engine/src/core/thread_persistence/replay.rs:319-348`) — sum across records, tolerate `None`.
- **✅ Demo**: `zdx exec -m xai:grok-4.5 --tools bash -p "run echo hi then report output"`; the thread JSONL usage event(s) include `"cost_ticks":<n>` whose sum equals the sum of raw `cost_in_usd_ticks` across the captured request traces. Running the same on `openai:*` produces usage events with no `cost_ticks` key.
- **Risks / failure modes**:
  - Double counting cost when both `MessageStart` and terminal `MessageDelta` carry it (see Key decisions).
  - Adding a non-token field to `Usage` breaking `is_empty()` semantics used to gate persistence (`persist.rs:89-90`).

## Phase 2: Ground-truth aggregation in stats + monitor
- **Goal**: `zdx stats` and the monitor Usage tab bill from provider cost when present, else computed pricing.
- **Scope checklist**:
  - [ ] Extend the JSONL parse (`UsageLine` / `LeanUsage`, `crates/zdx-engine/src/core/usage_stats.rs:535-573`) to read `cost_ticks`.
  - [ ] In the billing loop (`usage_stats.rs:660-692`): per record/bucket, if provider cost is present use `ticks as f64 / 1e10`; else keep the current `ModelPricing::cost(...)` path. Treat a provider-costed row as `cost_known = true` even if the registry lacks pricing. Never mix provider + computed cost within one record.
  - [ ] Keep subscription providers at `$0` and preserve `unknown_pricing_rows` semantics for records with neither provider cost nor registry pricing.
- **✅ Demo**: after a real `xai:grok-4.5` session that crosses 200K context, `zdx stats` billed USD for the xai row equals `sum(cost_ticks)/1e10` and matches the xAI console; the monitor Usage tab shows the same. An OpenAI row is unchanged vs. before.
- **Risks / failure modes**:
  - Mixed buckets (some records with cost, some without) producing a blended number that's hard to reason about — resolve per-record, then sum.
  - `1e10` tick scale wrong for a future model/region — assert against a known trace in tests.

## Phase 3: Live per-turn cost on TUI footer + bot /status
- **Goal**: The most-seen surfaces show exact provider cost when available.
- **Scope checklist**:
  - [ ] Carry `provider_cost_ticks` into the TUI thread usage state (`crates/zdx-tui/src/features/thread/state.rs:230-236`) and prefer it in the footer cost (`crates/zdx-tui/src/features/input/render.rs:636-645`), falling back to `ThreadUsage::calculate_cost`.
  - [ ] Prefer provider cost in the bot `/status` cost line (`crates/zdx-bot/src/handlers/message/status.rs:302-329`), else `calculate_usage_cost`.
  - [ ] Decide a subtle marker (e.g. no marker when exact, keep existing "estimated" affordance only for computed) — minimal, no new noise.
- **✅ Demo**: run a `xai:grok-4.5` turn in the TUI; footer cost equals `cost_in_usd_ticks/1e10` for that turn. `/status` in the bot shows the same exact figure. OpenAI/Anthropic turns still show the computed estimate unchanged.
- **Risks / failure modes**:
  - Live footer updates mid-stream before the terminal usage arrives — show computed until the provider cost lands, then reconcile.

# Contracts (guardrails)
- Subscription providers (`ProviderKind::is_subscription`) continue to display `$0` / "subscription", never a provider-cost number.
- Providers/records without `cost_ticks` compute cost from the registry exactly as before (bit-for-bit same output).
- Token counts, cache-read/cache-write accounting, and `cache_savings` are unchanged.
- Old transcripts (no `cost_ticks`) load, replay, and aggregate without error and produce the same numbers as today.
- `zdx_types::providers::Usage` keeps `#[derive(... PartialEq, Eq)]`.

# Key decisions (decide early)
- **Cost dedupe across `MessageStart` + `MessageDelta`**: `cost_in_usd_ticks` is a per-request cumulative total that appears on the terminal event carried by both the `MessageStart` (full `Usage`) and `MessageDelta` emissions (`responses_sse.rs:496-503`). Mirror the existing `usage_seen`/incremental pattern (`agent.rs:1767-1800`) or capture cost only once per request (e.g. on the terminal delta). Postponing this causes systematic double-billing.
- **Storage unit**: store raw `u64` ticks (`1 tick = 1e-10 USD`) end to end; convert to `f64` USD only at display/aggregation. Keeps `Eq` and avoids float drift in persistence.
- **Field naming**: `provider_cost_ticks` on the in-memory `Usage`; `cost_ticks` in the JSONL usage event. Document the `1e10` scale next to both.
- **Fallback boundary**: provider cost vs. computed pricing is chosen per-record, never blended within a record.

# Testing
- Manual smoke demos per phase (commands above), using `ZDX_DEBUG_TRACE` to capture raw payloads for cross-checking.
- Minimal regression tests only for contracts:
  - `usage_from_response` parses `cost_in_usd_ticks` and defaults to `None` when absent (extend tests in `responses_sse.rs:646-691`).
  - Persistence round-trip: a `ThreadEvent::Usage` with and without `cost_ticks` serializes/deserializes and replays correctly (extend `thread_persistence/tests.rs`).
  - `usage_stats` billing: a record with `cost_ticks` bills `ticks/1e10`; a record without falls back to `ModelPricing::cost`; subscription still `$0`.

# Polish rounds (after MVP)
Group improvements into rounds, each with a ✅ check-in demo.

## Polish round 1: Estimate vs. exact affordance
- Show a subtle indicator distinguishing provider-reported (exact) cost from computed (estimated) cost in stats/monitor/footer.
- ✅ Check-in demo: a mixed session lists xai rows as exact and openai rows as estimated.

# Later / Deferred
Explicit list of "not now" items + what would trigger revisiting them.
- Model the xAI >200K higher-context tier in `ModelPricing` — revisit only if a surface must estimate large-context cost for a provider that does NOT report cost.
- Extend provider-cost capture to other providers — revisit if/when Anthropic/Gemini/OpenAI expose a per-response billed-cost field.
- Backfill `cost_ticks` onto historical transcripts — revisit only if retroactive accuracy is explicitly requested.
