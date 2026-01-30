# Goals
- Add support for the `mimo-v2-flash` model via Xiaomi MiMo's OpenAI-compatible API.
- Allow users to select `mimo-v2-flash` in config/model picker and run prompts in `zdx` and `zdx exec`.
- Preserve streaming and basic tool loop behavior for the MiMo provider.

# Non-goals
- Supporting additional MiMo models or non-text endpoints beyond `mimo-v2-flash`.
- Implementing an Anthropic-compatible API path unless MiMo docs show a clear advantage.
- Refactoring provider architecture beyond what’s needed for MiMo.

# Design principles
- User journey drives order
- OpenAI-compatible implementation first (per request)
- Reuse the new `providers/openai` helpers (chat completions) where possible

# User journey
1. User gets a MiMo API key and sets env/config.
2. User selects `mimo-v2-flash` in the model picker or config.
3. User submits a prompt in `zdx` or `zdx exec`.
4. Response streams; tool calls work (or are explicitly disabled if unsupported).
5. Usage/pricing is shown when available.

# Foundations / Already shipped (✅)
List capabilities that already exist and should not be rebuilt.

## OpenAI-compatible chat completions client + SSE parser
- What exists: `providers/openai/chat_completions.rs` (`OpenAIChatCompletionsClient`) with streaming, tool-call parsing, reasoning deltas, usage parsing, and extra headers.
- ✅ Demo: Use any OpenAI-compatible chat completions provider (e.g., OpenRouter/Moonshot) and observe streaming output.
- Gaps: MiMo auth header/base URL may differ; confirm compatibility and whether Authorization can be reused.

## OpenAI Responses helpers (for OpenAI/Codex)
- What exists: `providers/openai/responses.rs` + `responses_sse.rs`.
- ✅ Demo: OpenAI API/Codex work via Responses API.
- Gaps: MiMo likely uses chat completions, not Responses.

## Provider config + model registry
- What exists: Provider config wiring (`config.rs`), default config template, model registry (`default_models.toml`).
- ✅ Demo: Pick a model from the command palette and persist selection.
- Gaps: No MiMo provider or model entry yet.

## Provider selection and instantiation
- What exists: `ProviderKind` enum, prefix parsing, and agent client wiring.
- ✅ Demo: `openrouter:` or `moonshot:` prefixed models route to their providers.
- Gaps: No MiMo provider kind or prefix/heuristic routing.

# MVP slices (ship-shaped, demoable)
Define Slice 1..N in user-journey order.

## Slice 1: MiMo provider skeleton + basic prompt streaming
- **Goal**: Make `mimo-v2-flash` usable for basic prompts via MiMo’s OpenAI-compatible chat completions endpoint.
- **Scope checklist**:
  - [ ] Confirm MiMo OpenAI API details from official docs: base URL, auth header, endpoint path, request fields, streaming format.
  - [ ] Add new provider kind (e.g., `mimo`) with config/env resolution and base URL validation.
  - [ ] Implement client using `OpenAIChatCompletionsClient` from `providers/openai/chat_completions.rs`.
  - [ ] Decide auth header strategy: keep `Authorization: Bearer` or add/override with MiMo’s required header (using `extra_headers`).
  - [ ] Wire provider selection for `mimo:` prefix and (if safe) `mimo-` heuristic.
  - [ ] Add `mimo-v2-flash` to the model registry (minimal metadata if docs don’t specify limits yet).
- **✅ Demo**: Set `MIMO_API_KEY` (or the doc-specified env var), run `zdx exec -p "hello" -m mimo:mimo-v2-flash`, observe a streamed response.
- **Risks / failure modes**:
  - MiMo docs are SPA-only and details are missing; wrong header/path breaks auth.
  - MiMo expects `max_completion_tokens` or other non-standard fields that the current chat-completions payload doesn’t send.

## Slice 2: Tool calling parity (or explicit disable)
- **Goal**: Ensure the agent tool loop works with MiMo, or cleanly disable tools if unsupported.
- **Scope checklist**:
  - [ ] Verify tool-call support in MiMo docs (request field names, response deltas, `tool_calls` shape).
  - [ ] If supported, ensure tool definitions and tool-call parsing round-trip cleanly through `OpenAIChatCompletionsClient`.
  - [ ] If unsupported, set provider tool list to a safe subset or none, and surface a clear limitation.
- **✅ Demo**: Prompt that triggers a `read` tool call and observe tool execution + follow-up response (or documented tool disable behavior).
- **Risks / failure modes**:
  - MiMo uses non-standard reasoning fields (e.g., `thinking`) that don’t map to `reasoning_content`.

## Slice 3: UX + metadata polish
- **Goal**: Make MiMo feel first-class in configuration and model picker.
- **Scope checklist**:
  - [ ] Populate pricing/context/output limit in `default_models.toml` from official docs.
  - [ ] Add MiMo provider defaults to `default_config.toml` and `Config` provider list.
  - [ ] Update `docs/SPEC.md` provider list to include MiMo.
- **✅ Demo**: Model picker shows `mimo-v2-flash` with pricing/capabilities; config template includes MiMo provider block.
- **Risks / failure modes**:
  - Missing or ambiguous model metadata in official docs leads to incorrect defaults.

# Contracts (guardrails)
List non-negotiable behaviors that must not regress (derived from Inputs and existing behavior).
- Existing providers (OpenAI, OpenRouter, Moonshot, Anthropic, Gemini) continue to work unchanged.
- Streaming responses still emit normalized `StreamEvent` sequences in TUI and exec modes.
- Model registry remains loadable even if MiMo metadata is incomplete.

# Key decisions (decide early)
List only decisions that would cause rework if postponed (derived from Inputs).
- Exact MiMo API contract (base URL, auth header, endpoint path, request/response fields, streaming format).
- Provider identifier and prefix (`mimo:` vs `xiaomi:`) and heuristic matching for `mimo-v2-*`.
- Whether MiMo supports tool calls and/or reasoning content; choose to map or omit.
- Whether to override/remove the default `Authorization` header if MiMo requires `api-key` only.
- Whether any Anthropic-compatible API is officially supported and worth implementing later.

# Testing
- Manual smoke demos per slice
- Minimal regression tests only for contracts

# Polish phases (after MVP)
Group improvements into phases, each with a ✅ check-in demo.
Limited strictly to scope present in Inputs.

## Phase 1: Docs + config ergonomics
- Add provider-specific troubleshooting notes (auth/base URL) to repo docs.
- ✅ Check-in demo: README/SPEC mention MiMo provider and config keys.

# Later / Deferred
Explicit list of "not now" items + what would trigger revisiting them.
- Additional MiMo models or endpoints (revisit when user requests broader MiMo coverage).
- Anthropic-compatible API implementation (revisit if MiMo docs publish it and it offers clear capability benefits).