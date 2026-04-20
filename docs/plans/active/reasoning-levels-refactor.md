# Ship-First Plan: Reasoning Levels Refactor

## Goals
- Fix Moonshot/Kimi K2.5 to use correct `thinking` parameter (binary on/off) instead of `reasoning_effort`
- Improve reasoning UI to adapt to each model's actual capabilities (levels vs binary vs budget-based)
- Prevent user confusion when selecting reasoning options that the model doesn't support

## Non-goals
- Add new reasoning providers beyond current set
- Redesign the entire settings/config system
- Support provider-specific reasoning parameters beyond what's needed for the fix

## Design principles
- User journey drives order: start with fixing the broken Moonshot integration
- UI adapts to model capabilities, not the other way around
- Minimal breaking changes to existing config/CLI

## User journey
1. User opens thinking level picker (Ctrl+T)
2. UI shows only options that the **current model** actually supports
3. User selects appropriate level for that model
4. Provider sends correct API payload for that model type
5. User sees reasoning output streamed correctly

## Foundations / Already shipped (✅)

### Model registry with capabilities
- What exists: `default_models.toml` with `[model.capabilities] reasoning = true/false`
- ✅ Demo: `ModelOption` in model picker shows reasoning icon
- Gaps: No granularity on *type* of reasoning (levels vs binary)

### Thinking picker overlay
- What exists: `thinking_picker.rs` with 6 levels (Off → XHigh)
- ✅ Demo: Ctrl+T opens picker, selection persists
- Gaps: Shows all 6 levels regardless of model capability

### Provider-specific reasoning mapping
- What exists: `map_thinking_to_reasoning()` in agent.rs for OpenAI-style
- ✅ Demo: OpenAI o-series gets `reasoning_effort: "low/medium/high"`
- Gaps: Moonshot hardcoded to `None`, Anthropic uses budget tokens, Gemini has per-model logic

### Moonshot provider
- What exists: `MoonshotClient` wraps `OpenAIChatCompletionsClient`
- ✅ Demo: Chat works, reasoning content streams
- Gaps: Sends wrong `reasoning.effort` payload (ignored by API), should send `thinking.type`

## MVP slices (ship-shaped, demoable)

### Slice 1: Fix Moonshot reasoning parameter
- **Goal**: Make Kimi K2.5 reasoning actually work end-to-end
- **Scope checklist**:
  - [ ] Add `thinking: Option<ThinkingType>` to `OpenAIChatCompletionsConfig` (separate from `reasoning_effort`)
  - [ ] Update `ChatCompletionRequest` to serialize `thinking` as `{"type": "enabled/disabled"}` when set
  - [ ] Update `MoonshotClient` to map any enabled `ThinkingLevel` → `thinking: {type: "enabled"}` instead of `reasoning_effort`
  - [ ] Pass `thinking: {type: "disabled"}` when `ThinkingLevel::Off`
  - [ ] Update temperature logic: thinking mode = 1.0, non-thinking = 0.6 (per Moonshot docs)
- **✅ Demo**: 
  - Select Kimi K2.5 → enable thinking → send message → see reasoning content stream
  - Disable thinking → verify `thinking: {type: "disabled"}` sent (no reasoning output)
- **Risks / failure modes**:
  - Breaking other OpenAI-compatible providers if `thinking` field leaks
  - Temperature constraints may cause API errors if wrong value sent

### Slice 2: Model-aware reasoning UI (binary models)
- **Goal**: Show only relevant options for models that only support on/off reasoning
- **Scope checklist**:
  - [ ] Add `reasoning_mode` field to model capabilities in `default_models.toml`: `"levels" | "binary" | "budget" | "none"`
  - [ ] Update `ModelCapabilities` struct to parse new field
  - [ ] Modify `thinking_picker.rs` to filter levels based on current model's `reasoning_mode`
  - [ ] For `binary` mode: show only "Off" and "On" (map On → Minimal in backend)
  - [ ] Update picker title/hints to reflect constrained options
- **✅ Demo**: 
  - Select Kimi K2.5 → open thinking picker → see only "Off" and "On" options
  - Select OpenAI o3 → open picker → see all 6 levels (low/medium/high mapped from Low/Medium/High)
- **Risks / failure modes**:
  - UI state desync if model changed while picker open
  - Need backward compatibility for models without `reasoning_mode` field

### Slice 3: Unified provider reasoning dispatch
- **Goal**: Clean up the scattered reasoning mapping logic into a unified approach
- **Scope checklist**:
  - [ ] Create `ReasoningConfig` enum in providers: `Disabled | Levels(ThinkingLevel) | Budget(u32) | Binary(bool)`
  - [ ] Update each provider to convert `ReasoningConfig` to their API format:
    - OpenAI: `reasoning_effort` (levels only)
    - Anthropic: `thinking_budget_tokens` (convert level → budget)
    - Gemini: `thinking_config` (per-model logic already exists)
    - Moonshot: `thinking.type` (binary)
  - [ ] Update agent.rs to pass `ReasoningConfig` instead of `Option<String>` reasoning_effort
  - [ ] Remove hardcoded `reasoning_effort: None` in Moonshot
- **✅ Demo**: 
  - Switch between models with different reasoning types → each sends correct payload
  - Verify in debug logs: OpenAI gets `reasoning.effort`, Moonshot gets `thinking.type`, Anthropic gets `thinking.budget_tokens`
- **Risks / failure modes**:
  - Large refactor surface area across providers
  - Risk of breaking existing working providers (Anthropic, Gemini)

## Contracts (guardrails)
- `ThinkingLevel::Off` must disable reasoning for all providers
- Existing user configs with `thinking_level` continue to work (backward compatible)
- Provider payload must match API documentation (verified via debug logs)
- UI must never show more options than the model supports

## Key decisions (decide early)
1. **How to represent reasoning in config**: Keep `ThinkingLevel` enum (6 levels) internally, map to provider-specific at dispatch time, or introduce provider-agnostic `ReasoningConfig`?
2. **Where to store reasoning_mode**: In `default_models.toml` capabilities, or derive from provider type?
3. **Level mapping for binary models**: Map all non-Off levels to "enabled", or only specific ones (e.g., Minimal-Low = enabled, Medium-XHigh = error/warning)?

## Testing
- Manual smoke demos per slice
- Verify with `ZDX_DEBUG_STREAM=1` that correct payload sent
- Test switching models with different reasoning capabilities mid-session

## Polish phases (after MVP)

### Phase 1: Smart level defaults per model
- Persist per-model thinking preferences (Kimi prefers On/Off, OpenAI prefers Low/Medium/High)
- Show recommended level in picker UI
- ✅ Check-in demo: Switch models → previous preference restored; picker shows "Recommended" badge

### Phase 2: Reasoning budget preview
- Show estimated token cost for selected reasoning level
- Display reasoning token usage in transcript after completion
- ✅ Check-in demo: Select High thinking → see "~8k tokens" preview; after response see actual reasoning tokens used

## Later / Deferred
- Fine-grained reasoning control (custom token budgets) for Anthropic models
- Provider-specific reasoning parameters exposed in config file
- Reasoning mode auto-detection from model ID patterns instead of explicit config
