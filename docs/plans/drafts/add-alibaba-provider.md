> Stage: drafts | active | done | archived. Keep this plan current while working: when a scope item is finished, check its box `[ ]`→`[x]`; when a phase's ✅ demo passes, mark the phase done (with date). The plan file is the source of truth, not memory.

> **Status 2026-07-21:** Phases 1–3 implemented. The subscription provider is named **Qwen Code** (`ProviderKind::QwenCode`, id/prefix `qwen-code`, env `QWEN_CODE_API_KEY`, label "Qwen Code"); its models.dev `api_id` stays `alibaba-coding-plan` for metadata fetch. Code compiles, `just ci-fast` + full `zdx-engine`/`zdx-providers`/`zdx` tests pass (788), defaults regenerated (`alibaba:{qwen3-max,qwen-plus,qwen-flash}` with real pricing; `qwen-code:{qwen3-coder-plus,qwen3-coder-next}` pricing zeroed). Remaining: live end-to-end run requires a real `ALIBABA_API_KEY` / `QWEN_CODE_API_KEY` (user-provided).

# Goals
- Add two new first-class OpenAI-compatible providers to the zdx registry:
  - **Alibaba** (International / DashScope Model Studio, pay-as-you-go) — `qwen*` models.
  - **Alibaba Coding Plan** (International, subscription) — Qwen coder models.
- Let a user pick a Qwen model, authenticate, and run a turn end-to-end through each provider.
- Keep the two providers **DRY**: one shared client module, one shared config path, driven by `ProviderKind` — no duplicated per-plan file.

# Non-goals
- China endpoints (`alibaba-cn`, `alibaba-coding-plan-cn`) and the `alibaba-token-plan` variant — defer.
- Non-text Qwen models (Wan video, embeddings, Qwen-VL/Omni image input) — text chat only for MVP.
- Custom Qwen "thinking budget" tuning beyond the existing on/off thinking flag.
- Changing fast-mode or exact context-refine token counting (provider-specific gates stay as-is).

# Design principles
- User journey drives order: auth + pick a model + get a streamed answer first; breadth of models later.
- **Reuse before rebuild**: both providers are `@ai-sdk/openai-compatible` (verified on models.dev), so both reuse the existing `OpenAIChatCompletionsClient`. No new wire protocol.
- **DRY across plans**: model the pair like `Xiaomi` / `XiaomiPlan` (the existing precedent) but collapse the client into a **single `alibaba.rs`** module whose `build(ctx)` branches on `ctx.provider`, instead of two near-identical files.

# User journey
1. User sets `ALIBABA_API_KEY` (Alibaba Intl) and/or `ALIBABA_CODING_PLAN_API_KEY` (Coding Plan), or pastes the key in the TUI auth overlay.
2. User opens the model picker, sees `qwen3-max` (Alibaba) and `qwen3-coder-plus` (Alibaba Coding Plan).
3. User selects a model and sends a prompt.
4. zdx streams the assistant response, with tool calls and (where supported) thinking working.

# Verified facts (models.dev + Alibaba docs)
- `alibaba` → base `https://dashscope-intl.aliyuncs.com/compatible-mode/v1`, npm `@ai-sdk/openai-compatible`, name "Alibaba". **Env var: `ALIBABA_API_KEY`** (decided; the DashScope key is pasted into it — models.dev labels this key `DASHSCOPE_API_KEY`, but we standardize on `ALIBABA_API_KEY` per the AI SDK `@ai-sdk/alibaba` convention).
- `alibaba-coding-plan` → env `ALIBABA_CODING_PLAN_API_KEY`, base `https://coding-intl.dashscope.aliyuncs.com/v1`, npm `@ai-sdk/openai-compatible`, name "Alibaba Coding Plan".
- Model IDs are **bare** (e.g. `qwen3-max`, `qwen-plus`, `qwen3-coder-plus`, `qwen3-coder-next`), not `alibaba/…`-prefixed.
- Auth: `Authorization: Bearer <key>` (standard OpenAI-compatible bearer).
- Existing `models.rs` note already flags that `qwen*` needs Chat Completions (`/v1/chat/completions`), not `/v1/messages` — matches `OpenAIChatCompletionsClient`.

# Foundations / Already shipped (✅)
Capabilities that already exist and must not be rebuilt.

## OpenAI-compatible chat client
- What exists: `crates/zdx-providers/src/openai/chat_completions.rs` (`OpenAIChatCompletionsClient` + `OpenAIChatCompletionsConfig`), used by `deepseek.rs`, `moonshot.rs`, `xiaomi.rs`, `xiaomi_plan.rs`, etc.
- ✅ Demo: any existing thin provider (e.g. `deepseek`) streams a turn.
- Gaps: none — reuse directly.

## Two-plan vendor precedent
- What exists: `Xiaomi` (`xiaomi.rs`) + `XiaomiPlan` (`xiaomi_plan.rs`) share vendor, differ by base URL / key / `is_subscription`. Full wiring already exists across `lib.rs`, `config.rs`, `models.rs`, TUI auth, and there are `provider_specs` tests (`test_provider_specs_includes_both_xiaomi_variants`).
- ✅ Demo: `zdx models update` keeps both `xiaomi:` and `xiaomi-plan:` entries.
- Gaps: Xiaomi uses two duplicated files; we improve on it with one shared module.

## Registry / config-driven TUI
- What exists: model picker, pricing display, thinking picker all read from the registry (`model-provider-mgmt` skill confirms no TUI change for new *models*; new *providers* need only an auth-title match arm for API-key providers).
- ✅ Demo: new registry entries appear in the picker automatically.
- Gaps: one auth-title match arm per new `ProviderKind`.

# MVP phases (ship-shaped, demoable)

## Phase 1: Alibaba International provider (end-to-end, one model)
- **Goal**: Run one Qwen model through the new `alibaba` provider.
- **Scope checklist**:
  - [ ] New `crates/zdx-providers/src/alibaba.rs`: shared `AlibabaConfig::from_env(kind, …)` + `AlibabaClient { inner: OpenAIChatCompletionsClient }` + `build(ctx)` that reads `ctx.provider` to pick key/base_url (mirror `deepseek.rs` shape, thinking flag like `xiaomi_plan.rs`).
  - [ ] `lib.rs`: `pub mod alibaba;`, add `ProviderKind::Alibaba`, metadata (`id: "alibaba"`, `label: "Alibaba"`, `api_key_env: "ALIBABA_API_KEY"`, `base_url: "https://dashscope-intl.aliyuncs.com/compatible-mode/v1"`, `base_url_env: "ALIBABA_BASE_URL"`, `is_subscription: false`), add to `all()`, dispatch arm → `alibaba::build(ctx)`, and `parse_provider_prefix()`.
  - [ ] `zdx-engine/src/core/agent.rs`: import + `ProviderClient` variant/dispatch + `build_alibaba_client()`.
  - [ ] `zdx-engine/src/config.rs`: `ProvidersConfig.alibaba` field + serde default + `is_enabled()/get()/get_mut()/Default`, and `default_alibaba_provider()` seeded with `["qwen3-max"]` (expand in Phase 3).
  - [ ] `zdx-cli/src/cli/commands/models.rs`: add `ProviderSpec { provider_id: "alibaba", api_id: "alibaba", prefix: Some("alibaba") }`, bump the `provider_specs()` array length, add `"alibaba" => "alibaba"` (or correct vendor) to the OpenRouter fallback vendor map.
  - [ ] `zdx-tui/src/features/auth/render.rs`: auth-title match arm for `ProviderKind::Alibaba`.
  - [ ] `just update-defaults` then verify a `qwen3-max` `[[model]]` block + `[providers.alibaba]` appear; correct pricing/context/`reasoning` against Alibaba docs.
- **✅ Demo**: with `ALIBABA_API_KEY` set, `qwen3-max` shows in the picker and streams a real answer end-to-end (`just run`, select the model, ask a question).
- **Risks / failure modes**:
  - Users copy a DashScope key into `ALIBABA_API_KEY` — document this in the auth title/help text.
  - `provider_specs()` array-size constant must be bumped or it won't compile.
  - models.dev may return promo/launch pricing → pin via `model_overrides.toml` if wrong.

## Phase 2: Alibaba Coding Plan provider (reuse Phase 1 module)
- **Goal**: Run a Qwen coder model through the subscription `alibaba-coding-plan` provider, reusing the same client.
- **Scope checklist**:
  - [ ] `lib.rs`: add `ProviderKind::AlibabaCodingPlan`, metadata (`id: "alibaba-coding-plan"`, `label: "Alibaba Coding Plan"`, `api_key_env: "ALIBABA_CODING_PLAN_API_KEY"`, `base_url: "https://coding-intl.dashscope.aliyuncs.com/v1"`, `base_url_env: "ALIBABA_CODING_PLAN_BASE_URL"`, `is_subscription: true`), add to `all()`, dispatch arm → `alibaba::build(ctx)` (same module), and `parse_provider_prefix()` (`alibaba-coding-plan`).
  - [ ] `agent.rs`: dispatch `AlibabaCodingPlan` through the same `build_alibaba_client()` path.
  - [ ] `config.rs`: `ProvidersConfig.alibaba_coding_plan` + `default_alibaba_coding_plan_provider()` seeded with `["qwen3-coder-plus"]`.
  - [ ] `models.rs`: `ProviderSpec { provider_id: "alibaba-coding-plan", api_id: "alibaba-coding-plan", prefix: Some("alibaba-coding-plan") }`; bump array length; add a `provider_specs` test asserting both Alibaba variants (mirror `test_provider_specs_includes_both_xiaomi_variants`).
  - [ ] `auth/render.rs`: auth-title arm for `ProviderKind::AlibabaCodingPlan`.
  - [ ] `just update-defaults` and verify `alibaba-coding-plan:qwen3-coder-plus` entry; subscription pricing is zeroed automatically.
- **✅ Demo**: with `ALIBABA_CODING_PLAN_API_KEY` set, `qwen3-coder-plus` streams a real answer; both providers coexist in the picker.
- **Risks / failure modes**:
  - Coding-plan base URL has no `/compatible-mode` segment (`…/coding-intl.dashscope.aliyuncs.com/v1`) — must not copy Intl's path.
  - Subscription providers must NOT carry pricing in model entries (`is_subscription()` handles zeroing).

## Phase 3: Seed a useful default model set
- **Goal**: Ship a sensible default model allow-list per provider.
- **Scope checklist**:
  - [ ] Alibaba Intl: add `qwen3-max`, `qwen-plus`, `qwen-flash` (+ optionally `qwen3-coder-plus`) to `default_alibaba_provider()`.
  - [ ] Coding Plan: add `qwen3-coder-plus`, `qwen3-coder-next` to `default_alibaba_coding_plan_provider()`.
  - [ ] `just update-defaults` and verify/correct each generated `[[model]]` (context, pricing, `reasoning`, `input_images`).
- **✅ Demo**: picker lists the seeded Qwen models for both providers with correct context/pricing.
- **Risks / failure modes**: exact model IDs must match Alibaba's API (bare IDs, verify against models.dev / Model Studio model list).

# Contracts (guardrails)
- Existing providers (deepseek/xiaomi/etc.) keep working unchanged.
- `default_config.toml` / `default_models.toml` stay generated — edit `config.rs`, not the TOMLs; durable corrections go in `model_overrides.toml`.
- `zdx models update` must retain both `alibaba:` and `alibaba-coding-plan:` entries (enforced by a `provider_specs` test).
- `provider_specs()` array-length constant stays in sync with the number of specs.

# Key decisions (decide early)
- **API-key env var for Alibaba Intl**: ✅ **Decided: `ALIBABA_API_KEY`** (AI SDK `@ai-sdk/alibaba` convention). The value is the DashScope key generated in Model Studio.
- **DRY shape**: ✅ **Decided: single `alibaba.rs`** with `build(ctx)` branching on `ctx.provider` for both `Alibaba` and `AlibabaCodingPlan`, instead of the Xiaomi two-file pattern.
- **Coding Plan key sharing**: Coding Plan uses a distinct `ALIBABA_CODING_PLAN_API_KEY` (per models.dev), not the DashScope key — keep them separate.

# Testing
- Manual smoke demos per phase (picker + streamed turn for each provider).
- One regression test in `models.rs`: `provider_specs` includes both Alibaba variants (mirror the Xiaomi test).
- `just ci-fast` after each phase; `just test -p zdx-cli` for the spec test.

# Polish rounds (after MVP)
## Polish round 1: thinking + capabilities fidelity
- Verify Qwen thinking (`enable_thinking`) maps correctly through the OpenAI-compatible path; set `reasoning` accurately per model.
- Correct any `input_images`/context/pricing the updater gets wrong via `model_overrides.toml`.
- ✅ Check-in demo: a thinking-capable Qwen model streams reasoning; picker shows correct pricing/context.

# Later / Deferred
- China endpoints (`alibaba-cn`, `alibaba-coding-plan-cn`) — add if the user needs mainland access.
- `alibaba-token-plan` subscription variant — add if the user subscribes to the token plan.
- Qwen-VL / Omni image input, Wan video, embeddings — revisit if multimodal Qwen is needed.
t — add if the user subscribes to the token plan.
- Qwen-VL / Omni image input, Wan video, embeddings — revisit if multimodal Qwen is needed.
