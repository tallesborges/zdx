# Goals
- Track `sequence_number` in OpenAI Responses SSE for dropped chunk detection
- Handle `response.created` event for queue time measurement
- Stream Gemini function call arguments incrementally via `partialArgs`/`willContinue`

# Non-goals
- `response.output_text.done` handling (conflicts with current block model — see Deferred)
- Anthropic changes (implementation is complete)

# Design principles
- User journey drives order
- Metrics/observability first (OpenAI gaps affect debugging)
- Streaming UX second (Gemini partialArgs is an enhancement)

# User journey
1. Developer runs zdx with OpenAI provider
2. If stream has issues, developer enables `ZDX_DEBUG_STREAM` to diagnose
3. Metrics show sequence gaps or timing breakdowns clearly
4. Developer runs zdx with Gemini provider
5. Tool calls with large arguments stream incrementally

# Foundations / Already shipped (✅)

## OpenAI Responses SSE Parser
- What exists: Full event parsing for text, function calls, reasoning
- ✅ Demo: `cargo test -p zdx-core` passes
- Gaps: `sequence_number` ignored, `response.created` ignored, `response.output_text.done` ignored

## Gemini SSE Parser
- What exists: Text streaming, complete function calls, reasoning/thought parts
- ✅ Demo: `cargo test -p zdx-core gemini` passes
- Gaps: Function call arguments not streamed incrementally

## Debug Metrics
- What exists: TTFB, gap detection, event counts, backpressure tracking
- ✅ Demo: `ZDX_DEBUG_STREAM=/tmp/metrics.txt cargo run -p zdx -- exec -p "hello"`
- Gaps: No sequence gap tracking (depends on Slice 1)

# MVP slices (ship-shaped, demoable)

## Slice 1: OpenAI `sequence_number` tracking
- **Goal**: Detect dropped/out-of-order SSE chunks
- **Scope checklist**:
  - [ ] Parse `sequence_number` from SSE event JSON in `responses_sse.rs`
  - [ ] Track `expected_sequence` in `StreamState`
  - [ ] On gap/out-of-order: log warning via `tracing::warn!` (parser has raw JSON access)
  - [ ] Add `SequenceGap { expected, actual }` variant to `StreamEvent` (optional, for metrics)
  - [ ] If new variant added: update `debug_metrics.rs` to count gaps
- **✅ Demo**: Unit test with mock SSE stream missing sequence 3 → warning logged
- **Risks / failure modes**:
  - Not all OpenAI events have `sequence_number` — verify via API docs/testing
  - May need to reset sequence on new response
- **Architecture note**: Sequence tracking *must* happen in parser (has raw JSON), not metrics wrapper (only sees `StreamEvent`)

## Slice 2: OpenAI `response.created` event
- **Goal**: Measure queue time (request accepted → first output)
- **Scope checklist**:
  - [ ] Handle `response.created` in `map_event()` → emit new `StreamEvent::ResponseCreated`
  - [ ] Update `debug_metrics.rs` to track `t_response_created` on this event
  - [ ] Calculate queue time = `t_first_output - t_response_created`
  - [ ] Update all `StreamEvent` match arms in consumers (TUI, exec mode, bot)
- **✅ Demo**: Debug stream output shows "Queue time: Xms" separately from TTFB
- **Risks / failure modes**:
  - New `StreamEvent` variant requires updating match arms (manageable)
  - If `response.created` is rare/optional, queue time may show as "N/A"
- **Architecture note**: New event variant required — timing-only approach won't work since `debug_metrics.rs` only sees `StreamEvent`, not raw SSE

## Slice 3: Gemini `partialArgs` streaming
- **Goal**: Stream function call arguments incrementally
- **Scope checklist**:
  - [ ] Enable `streamFunctionCallArguments` flag in Gemini request builders (both API key and OAuth paths)
  - [ ] Detect `functionCall.partialArgs` in chunk
  - [ ] Emit `ContentBlockStart` on first partial
  - [ ] Emit `InputJsonDelta` for each partial fragment
  - [ ] Track `willContinue` flag
  - [ ] Emit `ContentBlockCompleted` when `willContinue=false` or full args present
  - [ ] Keep backward compat: complete `functionCall` still works
- **✅ Demo**: Tool call with large args shows incremental streaming in TUI
- **Risks / failure modes**:
  - Gemini may not support this flag yet — verify via API docs
  - JSON fragments may need accumulation before parsing
  - If flag causes errors, need fallback to current behavior
- **Architecture note**: Requires request-side change *and* parser-side change

# Contracts (guardrails)
- Existing SSE tests must not regress
- New `StreamEvent` variants must be handled in all consumers (compile-time enforced via exhaustive match)
- Debug metrics file format remains append-friendly (no breaking changes to JSONL schema)
- Backward compatibility: streams without new events still work

# Key decisions (decide early)
- Slice 1: Add `SequenceGap` event variant or log-only?
  - Recommendation: Log-only first, add event if metrics need it
- Slice 2: `ResponseCreated` as new event variant
  - Decision: Required — metrics wrapper can't see raw SSE
- Slice 3: Enable `streamFunctionCallArguments` by default or opt-in?
  - Recommendation: Enable by default, add fallback if errors occur

# Testing
- Manual smoke demos per slice
- Unit tests for new event parsing (mock SSE fixtures)
- Integration test: `ZDX_DEBUG_STREAM` output includes new metrics

# Polish phases (after MVP)

## Phase 1: Metrics dashboard
- Add sequence gap % to debug output summary
- Add queue time breakdown chart-friendly output
- ✅ Check-in demo: Debug output clearly shows latency breakdown

## Phase 2: Error recovery
- On sequence gap, optionally request retry
- On Gemini partial timeout, emit partial result anyway
- ✅ Check-in demo: Graceful degradation on network issues

# Later / Deferred
- `response.output_text.done` handling: Current block model creates one `ContentBlockStart` per `output_item` (message), but `output_text.done` fires per text part within an item. Mapping to `ContentBlockCompleted` would double-close or prematurely close blocks. Requires content-part indexing or new event type. Revisit if multi-part text responses become common.
- Real-time sequence gap alerts (would trigger revisiting if production issues arise)
- Gemini `streamFunctionCallArguments` as user config toggle (revisit if causes issues)
- OpenAI Chat Completions API gaps (revisit if we add that provider path)
