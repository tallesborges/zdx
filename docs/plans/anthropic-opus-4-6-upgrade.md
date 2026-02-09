# Ship-First Plan: Anthropic Opus 4.6 API Upgrade

## Goals
- Upgrade the Anthropic integration to support Claude Opus 4.6 thinking behavior safely.
- Keep older Anthropic models working without behavior regressions.
- Expose Opus 4.6 API controls needed for production migration (adaptive thinking + effort).
- Ship with clear migration guardrails for deprecated and model-specific fields.

## Non-goals
- Changes to non-Anthropic providers.
- UI redesign beyond minimal controls needed to send correct API fields.
- Enabling preview/premium features by default.
- Broad refactors unrelated to Opus 4.6 compatibility.

## Design principles
- User journey drives order
- Backward-compatible by default
- Model-aware request shaping
- Safe defaults; explicit opt-in for premium/beta behavior

## User journey
1. User sends requests with Opus 4.6 and gets valid responses with adaptive thinking behavior.
2. User switches to older Anthropic models and requests still work using legacy manual thinking.
3. User tunes cost/quality with effort levels and gets predictable behavior per model.
4. User upgrades safely without hidden regressions in API-key or OAuth paths.

## Foundations / Already shipped (✅)

### Anthropic streaming request/response path
- What exists: Streaming Messages API path for both API key and OAuth auth modes.
- ✅ Demo: Send a basic prompt through both auth modes and confirm streaming output.
- Gaps: Request payload currently centered on legacy thinking format.

### Model registry with Opus 4.6 + older Anthropic models
- What exists: Opus 4.6 and prior Anthropic model IDs are available in model selection.
- ✅ Demo: Select Opus 4.6 and older Anthropic models and run basic prompts.
- Gaps: Feature flags are not fully enforced per model at request-build time.

### Internal thinking-level abstraction
- What exists: Internal thinking levels already map to request controls.
- ✅ Demo: Toggle thinking levels and observe request differences.
- Gaps: Needs correct Opus 4.6 mapping to adaptive + `output_config.effort`.

## MVP slices (ship-shaped, demoable)

### Slice 1: Correct Opus 4.6 thinking contract
- **Goal**: Make Opus 4.6 requests use the official adaptive pattern.
- **Scope checklist**:
  - [x] For `claude-opus-4-6`, send `thinking: { "type": "adaptive" }` when thinking is enabled.
  - [x] Send effort as `output_config: { "effort": "<level>" }` (not top-level).
  - [x] Keep thinking-disabled behavior by omitting `thinking`.
  - [x] Add serialization tests for Opus 4.6 request shape.
- **✅ Demo**: Opus 4.6 request shows adaptive thinking + `output_config.effort`, returns successful streamed response.
- **Risks / failure modes**:
  - Wrong field placement (`effort` outside `output_config`) causes API errors.
  - Thinking-off accidentally still emits thinking blocks.

### Slice 2: Legacy model compatibility path
- **Goal**: Preserve older Anthropic model behavior while upgrading Opus 4.6.
- **Scope checklist**:
  - [x] For older models (e.g., Opus 4.5/Sonnet 4.5), keep `thinking: { "type": "enabled", "budget_tokens": N }`.
  - [x] Do not send adaptive thinking to unsupported models.
  - [x] Keep shared stream parsing behavior unchanged.
  - [x] Add tests proving branch behavior by model ID.
  - [x] Re-enable `interleaved-thinking-2025-05-14` **conditionally** for legacy Claude 4 models when thinking is enabled.
  - [x] Do **not** send interleaved beta header for Opus 4.6 adaptive-thinking requests.
- **✅ Demo**: Same prompt succeeds on Opus 4.6 and older model, each with correct thinking schema.
- **Risks / failure modes**:
  - Model-detection mismatch sends adaptive to legacy models and fails.
  - Legacy path regresses during new request-shape rollout.

### Slice 3: Effort behavior + validation guardrails
- **Goal**: Make effort controls predictable and safe across Anthropic model versions.
- **Scope checklist**:
  - [x] Map internal thinking levels to supported effort values (`low|medium|high|max`) in `output_config.effort`.
  - [x] Enforce `max` only for Opus 4.6 (reject early for unsupported models).
  - [x] Preserve legacy budget-token behavior where required.
  - [x] Add tests for per-model effort validation.
- **✅ Demo**: `max` works on Opus 4.6, fails fast with clear error on unsupported models; `high` default behavior is stable.
- **Risks / failure modes**:
  - Silent fallback hides invalid configs.
  - Overly strict validation blocks valid requests.

### Slice 4: Migration safety + release readiness
- **Goal**: Prevent rollout regressions and document operator-facing changes.
- **Scope checklist**:
  - [x] Add migration notes for deprecated manual thinking on Opus 4.6.
  - [x] Add smoke tests for both auth modes across Opus 4.6 + one legacy model.
  - [x] Add rollout note for prompt-cache behavior when switching thinking modes (`adaptive` vs `enabled/disabled`).
  - [x] Add concise troubleshooting messages for common invalid combinations.
  - [x] Remove obsolete `fine-grained-tool-streaming-2025-05-14` beta-header usage.
- **✅ Demo**: CI smoke set passes; intentional misconfigs return clear local errors.
- **Risks / failure modes**:
  - Incomplete docs cause downstream config mistakes.
  - Missing smoke coverage leaves auth-path divergence undetected.

## Contracts (guardrails)
- Opus 4.6 thinking-enabled requests must use adaptive thinking format.
- Older Anthropic models must not receive adaptive thinking fields.
- Effort must be serialized under `output_config.effort`.
- `effort:"max"` must only be accepted for Opus 4.6.
- Legacy Claude 4 models with thinking enabled must include `interleaved-thinking-2025-05-14`; Opus 4.6 adaptive mode must not include it.
- Do not send deprecated fine-grained-tool-streaming beta header.
- Default behavior must not silently enable premium/beta options.

## Key decisions (decide early)
- Exact model-version gating rules for adaptive thinking and advanced controls.
- Canonical mapping from internal thinking levels to effort values.
- Whether unsupported settings fail locally (preferred) or pass through to API.
- Config surface for optional features (global defaults vs per-request overrides).

## Testing
- Manual smoke demos per slice
- Minimal regression tests only for contracts

## Polish phases (after MVP)

### Phase 1: `inference_geo` (data residency)
- Add `inference_geo: "global" | "us"` (supported models only).
- Model gating + validation to avoid legacy-model 400s.
- ✅ Check-in demo: request routing works and response usage reflects inference geo.

### Phase 2: Compaction (beta)
- Add `context_management.edits[{ type: "compact_20260112" }]` support.
- Add required beta header `compact-2026-01-12` when enabled.
- ✅ Check-in demo: long thread compacts and continues with correct context behavior.

### Phase 3: Fast mode (research preview)
- Add `speed: "fast"` support.
- Add required beta header `fast-mode-2026-02-01` when enabled.
- ✅ Check-in demo: fast request succeeds and response usage reports fast speed.

## Later / Deferred
- Automatic per-prompt dynamic switching of effort/speed (revisit if manual tuning becomes operationally heavy).
- Deeper optimization of compaction policies/instructions (revisit after long-conversation usage data).
- Expansion beyond current Opus-focused rollout (revisit when additional model docs require new branching).