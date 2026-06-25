# Ship-First Plan: OpenAI Responses API WebSocket Mode

Opt-in WebSocket transport for the OpenAI Responses API (`/v1/responses`), driven
by `response.create` frames with `previous_response_id` chaining. Targets lower
end-to-end latency on long tool-call rollouts (OpenAI reports up to ~40% on 20+
tool-call turns). This is the Responses WebSocket *mode*, not the Realtime/audio API.

> Reviewed by oracle for KISS/simplicity/DRY. Key rulings baked in below:
> - Do **not** ship a standalone "full-input WS" mode — it is plumbing with little
>   value (reqwest already pools connections); the win is incremental input.
> - Collapse the fallback taxonomy into one rule: **prefix-match → send suffix +
>   `previous_response_id`; otherwise send full input**.
> - No new `ProviderClient` variant; put transport choice inside `OpenAIConfig`.
> - Treat chaining as an opportunistic optimization, never as correctness state.

## Status

- ✅ **Slice 1** — `ResponsesEventMapper` extracted (behavior-preserving).
- ✅ **Slice 2** — `previous_response_id` on `RequestBody` + `response.id` capture.
- ✅ **Slice 3** — WS transport in `responses_ws.rs` (`OpenAIResponsesWsClient`).
- ✅ **Slice 4 core** — persistent session + pure `plan_send` prefix chaining.
- ✅ **Slice 5** — session safety (`Drop`-poison + conservative retry); merged into Slice 4.
- ✅ **Slice 4 wiring** — `providers.openai.websocket` opt-in flag (bool, mirrors `fast_mode`) flows
  through `OpenAIConfig` into `OpenAIClient`, which holds the `OpenAIResponsesWsClient` and dispatches
  to it. The session persists across the whole multi-tool loop within a `run_turn` (the client is built
  once per turn); a new socket opens per user turn.
- ✅ **Codex (`ChatGPT` OAuth) WS** — `providers.openai_codex.websocket` opt-in. `connect()` was
  generalized to an injected async `WsHeaderFactory`; the Codex factory resolves/refreshes OAuth creds
  and sends `Authorization`, `chatgpt-account-id`, `originator`, `user-agent`, `session_id`, and
  `OpenAI-Beta: responses_websockets=2026-02-06`. Codex routes the system prompt to top-level
  `instructions` (no `developer` input item) via the client's `system_as_instructions` flag.
  Endpoint: `wss://chatgpt.com/backend-api/codex/responses`.
- ✅ **Verified live** — multi-tool `zdx exec` over WS chains correctly (suffix + `previous_response_id`):
  API-key path on `gpt-5.4-mini`; Codex/OAuth path on `gpt-5.5` and `gpt-5.4` (`gpt-5.3-codex` is
  rejected by the ChatGPT-account backend — a model-availability 400, returned cleanly over WS).

Checks green for `zdx-providers`: 202 nextest, clippy `--all-targets`, nightly fmt.

## Goals
- Reuse the existing Responses event mapper across SSE and WS (events are byte-identical).
- Add an opt-in WS transport to `OpenAIClient` that chains turns with `previous_response_id`.
- Send only new input items when the conversation is a linear continuation; otherwise full input.
- Keep the HTTP/SSE path the untouched default.

## Non-goals
- Realtime/audio API.
- Multiplexing or parallel responses on one socket (API forbids it: one in-flight response).
- Mid-turn reconnect/resume.
- Replacing the HTTP Responses path. (The Codex/OAuth Responses provider is now intentionally in
  scope — see Status; non-Responses providers remain untouched.)
- A broad retry/fallback matrix or background frame-drain machinery.

## Design principles
- Additive and opt-in: a WS session inside `OpenAIClient`, nothing removed.
- Reuse the parser; do not redesign the JSON→`StreamEvent` mapping.
- Chaining is an optimization layered on top of full input, so reconnect stays trivial.
- One local invariant instead of named special cases.

## User journey
1. User selects an OpenAI Responses model and enables WS transport (config/registry flag).
2. First turn opens one socket to `wss://.../v1/responses` and sends a `response.create` frame.
3. Each follow-up turn reuses the live socket; when it is a linear continuation it sends only
   the new user message + tool results plus `previous_response_id`.
4. Streamed events render identically to the HTTP path (same `StreamEvent`s).
5. On any break in the chain (reconnect, dropped turn, edited history), the next turn silently
   sends full input — no user-visible change beyond latency.

## Foundations / Already shipped (✅)

### Transport-agnostic Responses event mapping
- What exists: `ResponsesSseParser` in `crates/zdx-providers/src/openai/responses_sse.rs` with
  `map_event(Value) -> ProviderResult<StreamEvent>`, a `StreamState`, and a `pending` queue;
  `response.completed` emits several normalized events (`responses_sse.rs:415+`).
- ✅ Demo: `cargo nextest run -p zdx-providers` (existing `map_event` unit tests pass).
- Gaps: mapping is fused with SSE framing (`EventStream<S>`), so WS can't reuse it yet; no
  `response.id` capture.

### HTTP Responses request path
- What exists: `send_responses_stream` (`responses.rs`) builds `RequestBody` via
  `build_input(messages, system)` (full conversation every turn), POSTs with `stream: true`,
  `store: false`, `include: ["reasoning.encrypted_content"]`.
- ✅ Demo: `just run` against an OpenAI Responses model.
- Gaps: `RequestBody` has no `previous_response_id` (`responses_types.rs:7`); always full input.

### Stateless client + engine dispatch
- What exists: `OpenAIClient` holds only `reqwest::Client` + `OpenAIConfig` (`api.rs`); engine
  dispatches `send_messages_stream(messages, tools, system)` per turn via the 19-variant
  `ProviderClient` enum (`crates/zdx-engine/src/core/agent.rs:161`, `:184`).
- ✅ Demo: works today over HTTP.
- Gaps: no persistent connection; no place for per-conversation `previous_response_id`.

## MVP slices (ship-shaped, demoable)

### Slice 1: Extract `ResponsesEventMapper` (behavior-preserving DRY prep) — ✅ done
- **Goal**: Share the JSON→`StreamEvent` logic between SSE and WS without changing behavior.
- **Scope checklist**:
  - [x] `ResponsesEventMapper { model, state, pending, last_response_id }` in `responses_sse.rs`.
  - [x] `new(model)`, `push_json(&str) -> ProviderResult<()>`, `pop() -> Option<StreamEvent>`.
  - [x] `ResponsesSseParser` keeps only SSE framing and delegates to `mapper.push_json`.
  - [x] Existing `map_event` unit tests retargeted at the mapper.
- **✅ Demo**: `cargo nextest run -p zdx-providers` green, no behavior change.

### Slice 2: `previous_response_id` plumbing (additive, HTTP unaffected) — ✅ done
- **Goal**: Be able to chain and to read the server response id.
- **Scope checklist**:
  - [x] `previous_response_id: Option<String>` on `RequestBody` (`skip_serializing_if`).
  - [x] Mapper captures `response.id` from `response.completed`/`response.done`; `last_response_id()` getter.
- **✅ Demo**: unit tests — mapper exposes the id; `RequestBody` omits the field when `None`.

### Slice 3: WS session + `response.create` — ✅ done (superseded by Slice 4)
- **Goal**: Open a socket, run a turn over WS, parity with HTTP.
- **Scope checklist**:
  - [x] Added `tokio-tungstenite` 0.29 (`connect` + `native-tls`, matching reqwest's TLS stack).
  - [x] `responses_ws.rs` with `OpenAIResponsesWsClient`; `https→wss` URL + `Authorization` handshake header.
  - [x] `response.create` frame from the shared `RequestBody` (serialized with `stream` stripped, `type` added).
  - [x] Event stream feeds inbound frames into a `ResponsesEventMapper`, ends at `response.completed`.
- **Shipped as**: started per-call (no persistence); Slice 4 replaced it with a persistent session.
- **✅ Demo**: `ingest_frame` is unit-tested with canned frames (socket-free) for StreamEvent parity —
  simpler than a mock WS server, and the mapping is already covered by the mapper tests.

### Slice 4: Persistent session + prefix-based chaining — ✅ done
- **Goal**: Send only new items on linear continuations; expose the WS transport to users.
- **Scope checklist**:
  - [x] Persistent `SessionInner { socket, last_response_id, last_input }` behind `Arc<Mutex>`; turns serialized via `lock_owned()`.
  - [x] After a clean `response.completed`, record `previous_response_id` + the full-input snapshot.
  - [x] Pure `plan_send`: continue with suffix + `previous_response_id` only when the new input exactly extends the prior snapshot; otherwise full input.
  - [x] **Wire the `websocket` flag through `OpenAIConfig`/provider config** so `OpenAIClient` selects WS and users can opt in (`providers.openai.websocket = true`).
- **✅ Demo (target)**: multi-tool `just run` on a codex model — 2nd+ turns send suffix-only
  (verify via `ZDX_DEBUG_STREAM`/trace); editing/compacting history sends full input.
- **Shipped logic tested by**: `plan_send` unit tests (fresh / extends / diverges / no id / no-new-items).

### Slice 5: Session safety (poison + conservative retry) — ✅ done (merged into Slice 4)
- **Scope checklist**:
  - [x] `TurnState` holds the session guard for the turn; its `Drop` poisons the socket and clears chain state on any early end (error, close, dropped stream).
  - [x] Send failure also poisons; connect/send failures surface as retryable transport errors.
  - [x] No background drain, no mid-turn reconnect/resume.
- **✅ Demo**: covered by the `ingest_frame` early-close test and the `Drop` poison path (manual abandon verifies reconnect).

## Contracts (guardrails)
- HTTP/SSE path behavior is unchanged; `previous_response_id` is never sent over HTTP.
- WS and HTTP produce identical `StreamEvent` sequences for the same server events.
- Existing `responses_sse.rs` tests must not regress after the mapper extraction.
- Chaining is opportunistic: any uncertainty falls back to full input, which is always correct.
- One in-flight response per socket; turns are serialized by the session mutex.

## Key decisions (decide early)
1. **Transport selection surface**: `OpenAIConfig` field vs model-registry hint vs both.
   - Recommendation: `OpenAIConfig.transport` (`OpenAITransport` enum), set from config/registry.
2. **Response-id capture mechanism**: mapper getter vs new terminal `StreamEvent` field.
   - Recommendation: mapper getter — avoids touching every `StreamEvent` consumer.
3. **Cache-miss error from the API** (continuing from an id the server dropped): return the error
   (next turn goes full) vs one immediate full-input retry.
   - Recommendation: start with return-the-error; add the single retry only if it shows up in
     normal use.

## Testing
- ✅ Slice 1: mapper unit tests retargeted, stay green.
- ✅ Slice 2: mapper exposes `response.id`; `RequestBody` omits `previous_response_id` when `None`.
- ✅ Slice 3/5: `ingest_frame` unit-tested with canned frames (StreamEvent parity + early-close error).
- ✅ Slice 4: `plan_send` unit tests (fresh / extends-prefix / diverges / no id / no new items).
- ⬜ Slice 4 wiring: request-trace assertion that linear turns send suffix-only with
  `previous_response_id` and non-prefix turns send full input (after engine wiring).
- ✅ Manual: multi-tool `zdx exec` over WS verified live — API-key (`gpt-5.4-mini`) and Codex/OAuth
  (`gpt-5.5`, `gpt-5.4`); confirmed one socket per turn plus suffix + `previous_response_id` chaining
  across tool rounds. (Used temporary stderr markers; the permanent `ZDX_DEBUG_STREAM` readout is Phase 2.)

## Polish phases (after MVP)
### Phase 1: Reconnect on the 60-minute cap
- Detect the connection-duration limit and transparently reopen before the next turn
  (full input on the new socket). ✅ Demo: a long session crosses 60 min without a user-visible error.

### Phase 2: Observability
- Surface WS vs HTTP transport and chain hit/miss in `ZDX_DEBUG_STREAM` metrics.
- ✅ Demo: debug output shows "WS continuation: suffix N items" vs "full input".

## Later / Deferred
- Reconnect/resume *within* an active response (explicitly out of scope; reconnect between turns only).
- Extending WS mode to OpenAI-compatible third parties (revisit if any expose the same protocol).
- ✅ Codex OAuth provider over WS — done: `providers.openai_codex.websocket`, OAuth headers +
  `OpenAI-Beta: responses_websockets=2026-02-06`, endpoint `wss://chatgpt.com/backend-api/codex/responses`.
