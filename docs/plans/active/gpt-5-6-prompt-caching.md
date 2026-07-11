> Stage: active. Keep this plan current while working: when a scope item is finished, check its box `[ ]`→`[x]`; when a phase's ✅ demo passes, mark the phase done (with date). The plan file is the source of truth, not memory.

# Goals
- Report GPT-5.6 uncached input, cache-read, and cache-write tokens accurately from OpenAI Responses API usage.
- Persist, price, aggregate, and render all four usage buckets without double counting.
- Handle official Responses API terminal events consistently over SSE and WebSocket so failed or truncated responses cannot be recorded as successful cache-chain continuations.
- Prove the behavior with deterministic provider tests and a live two-turn cache validation.

# Non-goals
- Restructuring ZDX prompts or tools solely to increase cache hits.
- Automatically choosing explicit cache-breakpoint positions.
- Adding legacy `prompt_cache_retention` behavior for pre-GPT-5.6 models.
- Reworking the shared usage, persistence, pricing, or TUI pipelines that already support cache-write tokens.

# Design principles
- User journey drives order.
- Ship accurate accounting before adding optional cache controls.
- Reuse the existing four-bucket usage pipeline instead of introducing a GPT-5.6-specific accounting path.
- Treat official OpenAI documentation as authoritative; use Codex and pi as implementation references, not contracts.
- Keep implicit prompt caching as the default until an explicit-breakpoint configuration contract exists.

# User journey
1. The user runs ZDX in a persisted thread with a GPT-5.6 model.
2. ZDX reuses the thread-derived `prompt_cache_key` across requests with the same stable prefix.
3. OpenAI reports cache writes on the first eligible request and cache reads on a later matching request.
4. ZDX displays and persists uncached input, cache reads, cache writes, output, and their costs accurately.
5. If OpenAI reports an incomplete or failed response, ZDX surfaces the correct outcome and does not retain invalid WebSocket continuation state.

# Foundations / Already shipped (✅)

## Stable per-thread cache key
- What exists: `crates/zdx-engine/src/core/agent.rs` derives `ProviderBuildContext.cache_key` from the persisted thread ID; `crates/zdx-providers/src/openai/api.rs` and `crates/zdx-providers/src/openai/codex.rs` pass it into the Responses client; `crates/zdx-providers/src/openai/responses_types.rs` serializes it as `prompt_cache_key`.
- ✅ Demo: serialize a direct OpenAI or Codex Responses request and verify that two turns in one thread contain the same non-empty `prompt_cache_key`.
- Gaps: custom thread IDs should be validated against OpenAI's cache-key length contract during implementation research.

## Four-bucket usage pipeline
- What exists: `crates/zdx-types/src/providers.rs` defines uncached input, output, cache-read, and cache-creation/write usage; `crates/zdx-engine/src/core/agent.rs`, `crates/zdx-engine/src/core/thread_persistence/`, and `crates/zdx-engine/src/core/usage_stats.rs` propagate and persist those buckets; `crates/zdx-tui/src/features/thread/state.rs` and `crates/zdx-tui/src/features/input/render.rs` aggregate and render them.
- ✅ Demo: existing Anthropic usage containing cache writes reaches persisted stats and the TUI write counter.
- Gaps: the OpenAI Responses terminal mapper currently never populates the cache-write bucket.

## GPT-5.6 pricing
- What exists: `crates/zdx-assets/default_models.toml` records GPT-5.6 Sol/Terra/Luna cache-write prices at 1.25× uncached input; `crates/zdx-engine/src/models.rs` prices cache writes independently.
- ✅ Demo: model-cost calculation charges each non-zero usage bucket at its registry rate.
- Gaps: OpenAI Responses usage currently reports zero writes to this otherwise-correct pipeline.

## Reasoning and continuation fidelity
- What exists: `crates/zdx-providers/src/openai/responses.rs` replays reasoning IDs, encrypted content, and summaries; `crates/zdx-providers/src/openai/responses_sse.rs` captures response IDs and reasoning output; `crates/zdx-providers/src/openai/responses_ws.rs` uses `previous_response_id` for incremental continuation.
- ✅ Demo: existing Responses tests preserve reasoning replay data and the latest successful response ID.
- Gaps: failed terminal events must not update successful WebSocket continuation state.

# MVP phases (ship-shaped, demoable)

## Phase 1: Accurate GPT-5.6 cache accounting
- **Status**: ✅ Completed 2026-07-10.
- **Goal**: make existing implicit caching observable and correctly priced with the smallest provider-only change.
- **Scope checklist**:
  - [x] Extract a small terminal-usage parser in `crates/zdx-providers/src/openai/responses_sse.rs` that reads `input_tokens`, `output_tokens`, `input_tokens_details.cached_tokens`, and `input_tokens_details.cache_write_tokens`.
  - [x] Map usage as `uncached = input_tokens.saturating_sub(cached_tokens).saturating_sub(cache_write_tokens)` and populate both cache buckets.
  - [x] Reuse that parser for every Responses terminal payload that contains usage rather than duplicating arithmetic across event branches.
  - [x] Add colocated regression tests for all four buckets, absent details, zero values, and inconsistent upstream totals where reads plus writes exceed total input.
  - [x] Confirm that no changes are required in `zdx-types`, engine persistence, stats, TUI rendering, or model pricing.
- **✅ Demo**: a terminal fixture with total input `20`, cache read `2`, cache write `3`, and output `7` emits usage `(input=15, cache_read=2, cache_write=3, output=7)`; `cargo nextest run -p zdx-providers` passes.
- **✅ Demo result (2026-07-10)**: the fixture emitted the expected four buckets; all 214 provider tests passed.
- **Risks / failure modes**:
  - Subtracting only reads would overprice uncached input and omit write cost.
  - Treating writes as part of reads would corrupt both pricing and cache-effectiveness metrics.
  - Non-saturating arithmetic could underflow on malformed provider telemetry.

## Phase 2: Correct terminal outcomes across SSE and WebSocket
- **Status**: ✅ Completed 2026-07-10.
- **Goal**: ensure official completed, incomplete, and failed events end a turn without silent success or invalid continuation state.
- **Scope checklist**:
  - [x] Define one internal terminal-outcome representation shared by the Responses event mapper and WebSocket ingestion; do not expand the current success boolean.
  - [x] Map `response.completed` to successful completion with final usage and response ID.
  - [x] Map `response.incomplete` from `incomplete_details.reason`, preserving final usage and using `max_tokens` only for the matching reason.
  - [x] Map `response.failed` from the response error code/message to `ProviderError`; verify whether final usage is persisted before failure and lock that behavior with a test.
  - [x] Update `crates/zdx-providers/src/openai/responses_ws.rs` so only successful completion calls `record_success()` and retains `previous_response_id` chain state.
  - [x] Update `crates/zdx-providers/src/openai/responses_sse.rs` so EOF before a recognized terminal event is a retryable transport failure; accept `[DONE]` only after a recognized terminal event if the protocol still emits it.
  - [x] Decide from captured/provider evidence whether the non-standard `response.done` compatibility branch remains necessary; remove it if unsupported and unobserved.
  - [x] Add SSE and WebSocket tests for completed, each supported incomplete reason, failed, close/EOF before terminal, and failed turns not retaining continuation state.
- **✅ Demo**: identical terminal fixtures produce equivalent SSE and WebSocket outcomes; completed succeeds, incomplete reports its actual reason, failed surfaces an error, and premature EOF never silently completes. `cargo nextest run -p zdx-providers` passes.
- **✅ Demo result (2026-07-10)**: completed responses end cleanly without waiting for EOF; incomplete responses preserve their reason and clear WebSocket continuation state; failed responses surface structured provider errors; premature SSE/WS closure is retryable and never succeeds. `response.done` remains as a compatibility alias for non-OpenAI Responses providers. Failed-response usage is not persisted because failure payloads do not provide reliable final usage. All 221 provider tests and 1,325 workspace tests passed; `just ci-fast` passed.
- **Risks / failure modes**:
  - Classifying every terminal frame as success would preserve failed WebSocket chain state.
  - Mapping every incomplete response to `max_tokens` would hide content-filter or other provider outcomes.
  - Emitting usage immediately before an error may be dropped by downstream handling unless explicitly tested.
  - Tightening EOF handling can affect retries, so visible-output retry rules in `crates/zdx-engine/src/core/agent.rs` must be verified without broad engine changes.

## Phase 3: Live cache and cost validation
- **Goal**: prove the implementation against GPT-5.6 rather than relying only on synthetic fixtures.
- **Scope checklist**:
  - [x] Use a persisted thread and a GPT-5.6 direct OpenAI model with a byte-identical stable prefix above OpenAI's 1,024-token cache threshold.
  - [x] Capture two requests with `ZDX_DEBUG_TRACE` and verify the same non-empty `prompt_cache_key`, stable prefix ordering, and final `input_tokens_details` fields.
  - [x] Verify the first eligible request reports cache writes and a later matching request reports cache reads; repeat within the documented minimum cache lifetime if routing produces an initial miss.
  - [x] Verify thread persistence and usage stats retain the same four buckets and calculate cost from GPT-5.6 registry prices without double counting.
  - [x] Delete sensitive debug traces after recording only the non-secret observed token totals and pass/fail result.
  - [ ] Add the stable four-bucket accounting and recognized-terminal invariants to `docs/SPEC.md`; update `docs/ARCHITECTURE.md` only if the terminal/usage data flow changes materially.
  - [x] Run `just ci-fast` and `just test`; use `just ci` as the optional pre-push gate.
- **✅ Demo**: two live turns with one persisted thread show a stable key, a cache write followed by a cache read for an eligible shared prefix, matching persisted/TUI totals, and correct calculated costs; all repository checks pass.
- **Validation result (2026-07-10)**: GPT-5.6 Luna wrote 31,516 cache tokens on turn one; turn two read 31,516 and wrote 46. Both requests used the same cache key and identical stable-prefix hash. Stats attributed two requests and 63.1k total tokens to direct OpenAI GPT-5.6 Luna at approximately $0.04. `just ci-fast` and all 1,318 workspace tests passed.
- **Risks / failure modes**:
  - A prefix below 1,024 tokens or a non-identical rendered prefix creates a false negative.
  - Cache routing may require a retry even with a stable key; captured payloads must distinguish an implementation error from a legitimate miss.
  - Debug traces contain full prompts and responses and must not remain in the repository or artifact directories.

# Contracts (guardrails)
- `Usage.input_tokens` means uncached input only; cache-read and cache-write tokens are disjoint additional buckets.
- For GPT-5.6 usage, `input_tokens = total_input - cached_tokens - cache_write_tokens` with saturating subtraction.
- Missing cache-detail fields deserialize as zero and do not fail a response.
- A stable persisted thread ID produces a stable `prompt_cache_key` across matching requests.
- Only a successful terminal response may update WebSocket continuation state.
- An incomplete response preserves its provider-reported reason; it is not universally equivalent to a token limit.
- A failed response or premature stream end cannot become a successful turn.
- Pre-GPT-5.6 requests must not receive GPT-5.6-only cache options or breakpoints.
- Existing reasoning IDs, encrypted content, summaries, function-call items, and response IDs continue to round-trip unchanged.

# Key decisions (decide early)
- Define whether usage attached to `response.failed` is persisted before the error; use one behavior for SSE and WebSocket and test the downstream engine path.
- Define the mapping from each official `incomplete_details.reason` value to existing `StreamEvent`/`ProviderError` semantics before editing terminal branches.
- Confirm from official schemas or captured traffic whether `response.done` is a supported compatibility event or dead handling.
- Validate the maximum `prompt_cache_key` length and choose rejection, hashing, or truncation for oversized custom thread IDs without changing normal generated IDs.
- Keep explicit breakpoints deferred until there is a deliberate user/config representation for placement and model gating.

# Testing
- Manual smoke demos per phase.
- Minimal regression tests only for contracts.
- Provider unit tests in `crates/zdx-providers/src/openai/responses_sse.rs` for usage arithmetic and SSE outcomes.
- Provider unit tests in `crates/zdx-providers/src/openai/responses_ws.rs` for terminal classification, chain-state retention, and parity with SSE.
- Narrow engine tests only if required to prove failed-response usage persistence or visible-output retry behavior.
- Live two-turn GPT-5.6 validation after deterministic tests pass.

# Polish rounds (after MVP)

## Polish round 1: Explicit-cache contract discovery
- Verify the current official schemas for `prompt_cache_options`, allowed `ttl` values, supported content blocks, model aliases, and breakpoint limits.
- Define an opt-in source for explicit breakpoint placement, including whether it belongs in provider config, message content, or a prompt-building policy.
- Define persistence behavior and direct OpenAI versus OpenAI Codex support before adding request fields.
- Keep implicit mode unchanged while measuring whether explicit placement would improve ZDX's real prompt shape.
- ✅ Check-in demo: an approved request/config contract identifies exactly which stable content block receives a breakpoint, how it is model-gated, and how older models omit every GPT-5.6-only field.

# Later / Deferred
- Typed `prompt_cache_options` and `prompt_cache_breakpoint` serialization: revisit after Polish round 1 defines placement and configuration; implementation must include direct HTTP/WebSocket serialization tests and strict GPT-5.6+ gating.
- Automatic breakpoint heuristics: revisit only if live measurements show implicit caching is materially insufficient and a deterministic stable boundary can be proven.
- Prompt reordering for cache efficiency: revisit only if traces show ZDX places variable content before reusable instructions/tools.
- Legacy retention controls for older models: revisit only if ZDX explicitly chooses to expose retention as a supported provider setting.