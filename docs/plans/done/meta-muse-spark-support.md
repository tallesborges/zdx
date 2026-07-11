> Stage: done. Keep this plan current while working: when a scope item is finished, check its box `[ ]`→`[x]`; when a phase's ✅ demo passes, mark the phase done (with date). The plan file is the source of truth, not memory.

# Goals
- Users can configure Meta credentials and run `meta:muse-spark-1.1` in ZDX.
- Streaming text, reasoning levels, image input, and ZDX tool calls work through Meta's Chat Completions API.
- The model picker and usage accounting show the correct 1M context and verified pricing.
- `zdx models update` preserves the Meta model entry and capabilities.

# Non-goals
- Meta's Responses API.
- Native video or PDF attachment support beyond ZDX's existing attachment capabilities.
- Built-in web-search grounding, computer-use APIs, or Meta-specific tools.
- Additional Muse models.
- Fast mode, WebSocket transport, or OAuth.

# Design principles
- User journey drives order.
- Reuse `OpenAIChatCompletionsClient` and the existing thin-provider pattern.
- Ship direct CLI usage before polishing discovery and metadata maintenance.
- Do not pin unverified output limits.

# User journey
1. Add a Meta API key through ZDX configuration or `META_API_KEY`.
2. Select `meta:muse-spark-1.1` with the desired reasoning level.
3. Send a prompt or image and receive a streamed response.
4. Let Muse Spark call ZDX tools during an agent turn.
5. See correct context and cost metadata in model and usage surfaces.

# Foundations / Already shipped (✅)

## OpenAI-compatible streaming client
- What exists: `crates/zdx-providers/src/openai/chat_completions.rs` already handles `/chat/completions`, SSE, usage, images, and tool calls.
- ✅ Demo: Existing OpenAI-compatible providers complete streamed tool-using turns.
- Gaps: Meta requires top-level `reasoning_effort`, not the client's generic `reasoning: { effort }` shape.

## Thin provider adapters
- What exists: `crates/zdx-providers/src/deepseek.rs` wraps the shared client and injects provider-specific request fields with `extra_body`.
- ✅ Demo: DeepSeek reasoning requests serialize their provider-specific top-level field.
- Gaps: No Meta provider metadata, credentials, or builder exists.

## Registry-driven model surfaces
- What exists: `crates/zdx-engine/src/models.rs`, `crates/zdx-assets/default_models.toml`, and the TUI model picker already drive context, pricing, reasoning, and image capability displays.
- ✅ Demo: Existing registered models appear without model-specific TUI code.
- Gaps: Muse Spark has no registry record or updater source.

# MVP phases (ship-shaped, demoable)

## Phase 1: Direct Muse Spark agent turn
- **Goal**: Make Muse Spark usable from `zdx exec` with streaming, reasoning, images, and tools.
- **Scope checklist**:
  - [x] Add `crates/zdx-providers/src/meta.rs` as a thin `OpenAIChatCompletionsClient` wrapper.
  - [x] Resolve `META_API_KEY`, `META_API_BASE`, and default `https://api.meta.ai/v1` through `ProviderKind::Meta` in `crates/zdx-providers/src/lib.rs`.
  - [x] Map ZDX thinking levels to Meta's top-level `reasoning_effort`, omitting it when thinking is off.
  - [x] Register `ProviderKind::Meta`, prefix routing, streaming dispatch, and client construction in `crates/zdx-providers/src/lib.rs`.
  - [x] Add `providers.meta` and `default_meta_provider()` with `muse-spark-1.1` in `crates/zdx-engine/src/config.rs`.
  - [x] Add focused routing and request-serialization tests.
- **✅ Demo**: With `META_API_KEY` set, `zdx exec -m meta:muse-spark-1.1 -p "Use an available tool, then summarize the result"` streams a successful tool-using turn; a second run at `xhigh` sends top-level `reasoning_effort: "xhigh"`.
- **Risks / failure modes**:
  - Meta's preview account may be unavailable outside supported regions.
  - Meta may differ from OpenAI in SSE usage or reasoning-delta fields despite request compatibility.
  - Tool replay may reveal an undocumented Meta-specific message requirement.

## Phase 2: Daily-usable configuration and discovery
- **Goal**: Make Muse Spark selectable and understandable in normal ZDX surfaces.
- **Scope checklist**:
  - [x] Regenerate `crates/zdx-assets/default_config.toml` from `crates/zdx-engine/src/config.rs`.
  - [x] Add the verified `meta:muse-spark-1.1` record to `crates/zdx-assets/default_models.toml` with 1M context, reasoning, image input, and pricing.
  - [x] Add the Meta API-key configuration title in `crates/zdx-tui/src/features/auth/render.rs`; reuse ZDX's generic API-key setup flow without OAuth or browser login.
  - [x] Verify the model picker, thinking picker, image attachment flow, and usage-cost display.
  - [x] Add Meta to the provider list in `README.md`.
- **✅ Demo**: A fresh generated config exposes Meta, ZDX accepts the key from provider config or `META_API_KEY`, the picker shows Muse Spark with reasoning and image support, and a saved turn reports cost using $1.25/M input and $4.25/M output.
- **Risks / failure modes**:
  - Cached-input pricing must be confirmed from Meta's portal before being pinned.
  - The exact maximum output-token limit may remain unavailable; leave it unpinned rather than guessing.

## Phase 3: Registry-update durability
- **Goal**: Ensure normal model-registry refreshes do not erase or degrade Muse Spark metadata.
- **Scope checklist**:
  - [x] Add Meta to `provider_specs()` in `crates/zdx-cli/src/cli/commands/models.rs`.
  - [x] Use the embedded default record when models.dev has no Meta provider entry. (models.dev now serves `meta/muse-spark-1.1` directly, so the updater pulls authoritative data; embedded record remains as fallback.)
  - [x] Add updater coverage proving `meta:muse-spark-1.1` survives with pricing and capabilities intact.
  - [x] Skip `model_overrides.toml` — models.dev data is verified correct (context 1M, output 32000, input 1.25, output 4.25, cache_read 0.15), so no pin is needed.
  - [x] Run `just update-defaults`, `just ci-fast`, and the relevant provider/config/updater tests.
- **✅ Demo**: `zdx models update` retains `meta:muse-spark-1.1` with the verified 1M context, pricing, reasoning, and image capability; rerunning it produces no metadata regression.
- **Risks / failure modes**:
  - The models.dev provider key may differ from `meta` or may appear after implementation. (Resolved: models.dev uses `meta` and serves the model.)
  - Generated defaults can regress if the binary is not rebuilt before regeneration.

# Contracts (guardrails)
- `meta:muse-spark-1.1` routes to Meta and sends the bare model ID `muse-spark-1.1`.
- Credentials resolve from provider config first, then `META_API_KEY`; `META_API_BASE` can override the endpoint.
- Thinking off omits `reasoning_effort`; enabled levels use Meta's accepted vocabulary through `xhigh`.
- Requests use `/chat/completions` and preserve ZDX's existing streaming, image, tool-call, usage, and retry behavior.
- Model metadata must not claim unsupported or unverified output limits.
- Generated `default_config.toml` is never edited as its source of truth.

# Key decisions (decide early)
- Use Chat Completions for the MVP because it reuses ZDX's mature OpenAI-compatible stream and tool path; defer Responses API.
- Implement Meta as a first-class provider rather than a custom provider so credentials, model discovery, metadata, and usage attribution work consistently.
- Represent only capabilities supported by ZDX's current registry: reasoning and image input; do not expand the schema for video, PDF, structured output, or parallel tools in this feature.
- Treat Meta's developer portal as authoritative for output limits and cache pricing before pinning either value.

# Testing
- Manual smoke demos per phase.
- Minimal regression tests only for contracts.
- Provider unit tests for prefix routing, metadata, and top-level `reasoning_effort` serialization.
- Config tests for Meta defaults and enablement.
- Updater tests for embedded fallback and metadata preservation.
- Final `just ci-fast`; run targeted provider/engine/CLI tests during implementation.

# Polish rounds (after MVP)

## Polish round 1: Provider diagnostics
- Confirm Meta authentication and API errors use ZDX's existing structured provider-error classification.
- Verify usage parsing includes reasoning and cached tokens when Meta emits them.
- ✅ Check-in demo: Invalid credentials produce a clear terminal auth error, while a successful cached/reasoning request records all usage fields Meta returns.

# Later / Deferred
- Meta Responses API: revisit only if it unlocks behavior unavailable through Chat Completions.
- Video and PDF attachments: revisit when ZDX gains provider-neutral support for those input types.
- Native web-search grounding and computer use: revisit as separate tool/API integrations.
- Additional Muse models: add when Meta publishes stable model IDs and metadata.
- Fast mode or WebSocket transport: revisit only if Meta documents compatible support.