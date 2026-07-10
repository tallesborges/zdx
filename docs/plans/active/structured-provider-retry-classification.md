> Stage: active. Keep this plan current while working: when a scope item is finished, check its box `[ ]`→`[x]`; when a phase's ✅ demo passes, mark the phase done (with date). The plan file is the source of truth, not memory.

# Goals
- Automatically retry pre-stream transport failures such as `error sending request for url` across OpenAI, Anthropic, Gemini, and OpenAI-compatible providers.
- Prefer structured transport kind, HTTP status, and provider error code over message text when deciding whether an error is transient.
- Keep text matching only as a fallback for unknown or unstructured provider and gateway errors.
- Prove that transient failures retry while parse, request-construction, quota, billing, authentication, and other terminal failures do not.

# Non-goals
- Separate provider-level and agent-turn retry budgets.
- Honor `Retry-After` headers in the MVP.
- Add retry settings or change the existing retry count and exponential backoff.
- Retry after visible assistant output or tool execution begins.
- Make parse, protocol, authentication, quota, or billing failures retryable.

# Design principles
- User journey drives order.
- Ship the observed transport fix before broadening structured classification.
- Classify from typed transport and provider data first; inspect prose only when structure is unavailable or unknown.
- Reuse the existing retry loop, visible-content gate, usage buffering, and retry UX.

# User journey
1. The user sends a prompt through any supported provider.
2. If the request transport fails before visible output, ZDX identifies the typed transport failure and retries automatically.
3. If the provider returns a transient HTTP status or API error code, ZDX retries through the same bounded backoff flow.
4. If the failure is terminal or occurs after visible output, ZDX stops safely and reports the error without an unsafe retry.

# Foundations / Already shipped (✅)

## Bounded provider retry loop
- What exists: `run_turn_inner` performs up to three retries with exponential backoff and emits `AgentEvent::ProviderRetry` in `crates/zdx-engine/src/core/agent.rs:870-1007`.
- ✅ Demo: an error already recognized by `ProviderError::is_retryable()` produces `⟳ Provider error, retrying...` through `crates/zdx-tui/src/features/transcript/update.rs:70-83`.
- Gaps: the loop receives only the current text-derived retry decision.

## Safe transparent-retry gate
- What exists: retries are allowed only before user-visible content via `can_transparently_retry_stream` and `StreamState::emitted_visible_content` in `crates/zdx-engine/src/core/agent.rs:911-912` and `crates/zdx-engine/src/core/agent.rs:1280-1501`.
- ✅ Demo: existing engine tests around `crates/zdx-engine/src/core/agent.rs:3255-3511` prove that visible output blocks transparent retry.
- Gaps: none for this feature.

## Retry-safe usage accounting
- What exists: usage is buffered per attempt and discarded for transparent retries, as documented in `docs/ARCHITECTURE.md:130-133` and implemented by the retry loop in `crates/zdx-engine/src/core/agent.rs:918-981`.
- ✅ Demo: existing retry tests show a discarded attempt does not duplicate committed usage.
- Gaps: none for this feature.

## Shared provider error model
- What exists: `ProviderError`, its constructors, retry classifier, and focused tests live in `crates/zdx-types/src/providers.rs:12-147` and `crates/zdx-types/src/providers.rs:313-349`.
- ✅ Demo: current tests classify overloaded, rate-limit, server, timeout, parse, and client errors.
- Gaps: `http_status` discards the numeric status, `api_error` embeds the provider code in prose, and `is_retryable` otherwise depends on text patterns.

## Provider transport adapters
- What exists: OpenAI Responses, OpenAI-compatible Chat Completions, Anthropic, and Gemini all convert `reqwest::Error` into `ProviderError` at `crates/zdx-providers/src/openai/responses.rs:91-100`, `crates/zdx-providers/src/openai/chat_completions.rs:131-140`, `crates/zdx-providers/src/anthropic/shared.rs:276-285`, and `crates/zdx-providers/src/gemini/shared.rs:207-216`.
- ✅ Demo: each adapter already sends failures into the provider-agnostic engine retry loop.
- Gaps: request-send failures can become `HttpStatus` with `Request error: ...`, which is non-retryable unless its wording happens to match a retry pattern.

# MVP phases (ship-shaped, demoable)

## Phase 1: Retry typed request transport failures across providers
- **Status**: ✅ Implemented and verified (2026-07-10)
- **Goal**: fix the observed `error sending request for url` failure without waiting for the complete HTTP/API classification cleanup.
- **Scope checklist**:
  - [x] Add explicit transport and request-construction categories to `ProviderErrorKind` in `crates/zdx-types/src/providers.rs:12-23`, keeping transport retryable and request construction terminal without inspecting their messages.
  - [x] Mirror the new categories in `ErrorKind` and `From<ProviderErrorKind>` in `crates/zdx-types/src/events.rs:169-220` so retry events remain exhaustive and serializable.
  - [x] Add shared `ProviderError` constructors for transport and request-construction failures in `crates/zdx-types/src/providers.rs:77-124`.
  - [x] Centralize `reqwest::Error` classification in `crates/zdx-providers/src/shared.rs`, using typed predicates to reject builder/redirect failures and classify timeout/connect/send failures without relying on error prose.
  - [x] Replace the duplicated classifiers in `crates/zdx-providers/src/openai/responses.rs:91-100`, `crates/zdx-providers/src/openai/chat_completions.rs:131-140`, `crates/zdx-providers/src/anthropic/shared.rs:276-285`, and `crates/zdx-providers/src/gemini/shared.rs:207-216` with the shared classifier.
  - [x] Route OpenAI API and Codex image-generation requests through the same shared classifier.
  - [x] Classify WebSocket request, HTTP handshake, protocol, and transport failures structurally in `crates/zdx-providers/src/openai/responses_ws.rs`.
  - [x] Add focused tests for transport retryability, request-construction terminal behavior, and the same request-send failure class that produced `error sending request for url`.
- **✅ Demo**: `shared::tests::test_classify_reqwest_send_error_is_transport` reproduces reqwest's typed `Request` failure and proves it becomes retryable `Transport`; paired request-construction and WebSocket tests prove malformed requests, handshake authentication failures, and protocol failures remain terminal. All provider adapters compile against the shared classifiers, and the complete 1,308-test workspace suite passes.
- **Risks / failure modes**:
  - Treating every `reqwest::Error::is_request()` as transport could retry malformed request construction; typed builder/redirect predicates must take precedence.
  - Adding error categories requires updating every exhaustive display/event mapping in `crates/zdx-types/src/providers.rs:25-33` and `crates/zdx-types/src/events.rs:212-220`.

## Phase 2: Make HTTP and API retries structured-first
- **Status**: ✅ Implemented and verified (2026-07-10)
- **Goal**: use status and provider codes consistently across all provider families, with text matching limited to unknown/unstructured responses.
- **Scope checklist**:
  - [x] Preserve `status: Option<u16>` and `code: Option<String>` on `ProviderError` in `crates/zdx-types/src/providers.rs:35-43`, omitting absent metadata from serialized output.
  - [x] Make `ProviderError::http_status` store its numeric status and extract common structured error codes/types from provider JSON bodies in `crates/zdx-types/src/providers.rs:87-115`.
  - [x] Make `ProviderError::api_error` preserve the stream error type/code supplied by OpenAI, Anthropic, and Gemini through `StreamEvent::Error` and `crates/zdx-engine/src/core/agent.rs:1511-1640`.
  - [x] Apply retry precedence in `ProviderError::is_retryable()` at `crates/zdx-types/src/providers.rs:126-138`: parse/request-construction false; known quota/billing/authentication terminal codes false; transport/timeout true; HTTP `408`, `429`, and `500..=599` true; known transient API codes true; text fallback last for absent or unknown structure.
  - [x] Keep terminal quota/billing evidence ahead of generic `429`/`5xx` retry so account limits cannot loop.
  - [x] Replace text-shaped status tests with structured constructor tests and add coverage for `408`, `429`, nonstandard `5xx`, overload/rate-limit codes, quota/billing overrides, parse errors, and terminal `4xx` responses in `crates/zdx-types/src/providers.rs:313-349`.
  - [x] Update the retry contract in `docs/SPEC.md` and the structured provider-error flow in `docs/ARCHITECTURE.md` without changing the existing engine retry budget or UI event shape.
- **✅ Demo**: focused tests prove that OpenAI, Anthropic, and Gemini payload shapes produce the same structured decisions; a transient `529` retries, while a `429` carrying an insufficient-quota code and a parse failure terminate immediately. `just ci-fast` and the complete 1,315-test workspace suite pass.
- **Risks / failure modes**:
  - A generic `429` is transient, but quota and billing errors may also use `429`; terminal structured codes and fallback terminal evidence must win.
  - Provider bodies use different code fields; extraction must remain small and cover only shapes already consumed by current adapters.
  - Unknown gateways may expose only prose; removing the fallback entirely would regress currently supported transient errors.

# Contracts (guardrails)
- Retry at most three times with the existing exponential delay in `crates/zdx-engine/src/core/agent.rs:764-765` and `crates/zdx-engine/src/core/agent.rs:983-1007`.
- Never transparently retry after visible text, reasoning, or tool activity in `crates/zdx-engine/src/core/agent.rs:911-912` and `crates/zdx-engine/src/core/agent.rs:1280-1501`.
- Never retry generic parse/protocol failures; preserve the existing SSE transport-versus-parse distinction in `crates/zdx-providers/src/shared.rs:126-151`.
- Discard usage buffered by transparently retried attempts and preserve terminal-attempt accounting as documented in `docs/ARCHITECTURE.md:130-133`.
- Use one provider-agnostic classification policy for OpenAI, Anthropic, Gemini, and OpenAI-compatible adapters.
- Keep the existing `AgentEvent::ProviderRetry` UX and avoid provider-specific retry loops.

# Key decisions (decide early)
- Represent transport and request-construction failures as distinct error kinds so their retry decisions never depend on wording.
- Preserve numeric HTTP status and provider code/type on `ProviderError`; do not recover them later from formatted messages.
- Treat `408`, `429`, and all `500..=599` statuses as transient only after terminal quota/billing/authentication evidence is excluded.
- Keep text classification as a final compatibility fallback for unknown provider/gateway errors, not the primary path.
- Keep `Retry-After`, configurable retry policy, and separate provider/agent budgets out of the MVP.

# Testing
- Manual smoke demos per phase.
- Minimal regression tests only for contracts.
- Pure classification tests in `crates/zdx-types/src/providers.rs` for structured kind/status/code precedence.
- Shared adapter tests in `crates/zdx-providers/src/shared.rs` for typed `reqwest` classification.
- Reuse existing engine retry-gate and usage-buffer tests unless a changed event mapping requires a focused assertion.
- Run `cargo nextest run -p zdx-types`, the narrow `zdx-providers` tests, and `just ci-fast` during implementation; run `just test` when behavior is complete.

# Polish rounds (after MVP)

## Polish round 1: Shrink the text fallback surface
- Audit remaining `ProviderError::new` and text-only retry paths in `crates/zdx-providers/src/` and route already-available status/code data through the structured constructors.
- Keep text fallback cases only where the upstream provider or gateway genuinely supplies no stable structure.
- ✅ Check-in demo: classification tests cover every retained fallback pattern, and known provider adapters reach structured branches for transport, HTTP, and stream API errors.

# Later / Deferred
- Honor `Retry-After` and provider-specific retry-delay headers when the existing fixed backoff causes observable throttling problems.
- Split provider-request retries from outer agent-turn retries if duplicate retry budgets become measurable in production.
- Add user-configurable retry limits or delays when a concrete tuning need appears.
- Add true mid-stream resume or reconnect; revisit only when providers expose reliable continuation semantics.
- Retry premature EOF/truncation cases not already surfaced as typed transport errors; revisit with a reproducible failure fixture.