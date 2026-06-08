---
name: model-provider-mgmt
description: Add, update, or remove models and providers in the zdx LLM registry. Use when the user asks to add a new model, add a new provider, update model pricing or context limits, regenerate default models/config, or says things like "add support for X model", "add the new Y provider", "update pricing for Z", "regenerate models", or "run models update".
---

# Model & Provider Management

Covers three workflows: adding a model to an existing provider, adding a new provider from scratch, and regenerating/updating the default registry. All paths converge at `just update-defaults` which regenerates both `default_config.toml` and `default_models.toml`.

## Quick reference

| Task | Rust file to edit | Then run |
|------|-------------------|----------|
| Add model (existing provider) | `crates/zdx-engine/src/config.rs` → `default_<provider>_provider()` | `just update-defaults` |
| Update pricing/context only | `crates/zdx-assets/default_models.toml` (manual edit after generation) | `just ci-fast` |
| Add new provider | Multiple (see below) | `just update-defaults && just ci-fast` |

## Workflow 1: Add a model to an existing provider

### 1. Update the Rust model allow-list

Edit `crates/zdx-engine/src/config.rs`. Find `fn default_<provider>_provider()` and add the model ID to the `models` vec. Model IDs must match what the provider's API expects.

```rust
fn default_<provider>_provider() -> ProviderConfig {
    ProviderConfig {
        enabled: Some(true),
        models: vec![
            "<new-model-id>".to_string(),  // ← added
            "<existing-model-id>".to_string(),
            // ...
        ],
        ..Default::default()
    }
}
```

### 2. Regenerate defaults

```sh
just update-defaults
```

This runs `update-config` (regenerates `crates/zdx-assets/default_config.toml` from Rust defaults) then `update-models` (fetches pricing/capabilities from models.dev + OpenRouter fallback, writes `crates/zdx-assets/default_models.toml`).

### 3. Verify and fix the generated entry

**Always inspect the generated entry.** The updater pulls from models.dev and OpenRouter, which may have:
- Stale or promotional pricing (vs official provider docs)
- Wrong context limits (e.g. OpenRouter may report a pricing tier boundary instead of actual context window)
- Missing capabilities (e.g. `input_images` defaults to false)

Check `crates/zdx-assets/default_models.toml` for the new `[[model]]` block. Correct any fields that don't match the provider's official docs. Key fields:

| Field | What to verify |
|-------|----------------|
| `context_limit` | Actual context window, not pricing tier |
| `input` / `output` | Per-million-token pricing (standard, not promotional) |
| `cache_read` / `cache_write` | Cache pricing; `0.0` if not supported |
| `reasoning` | `true` if model has thinking/chain-of-thought |
| `input_images` | `true` if model accepts image/video input |
| `output_limit` | Max output tokens |

### 4. Build check

```sh
just ci-fast
```

### 5. (Optional) Add Zen proxy entry

If the model should also be accessible through the Zen meta-provider, update:
- `default_zen_provider()` in `config.rs`
- `[providers.zen].models` in the generated `default_config.toml`
- Re-run `just update-defaults` and verify a `zen:<model>` entry appears in `default_models.toml`

## Workflow 2: Add a new provider

Requires plumbing through multiple files. Order matters.

### 1. Provider module

Create `crates/zdx-providers/src/<provider>.rs`:
- Define `<Provider>Config` with `from_env()` (uses `ProviderKind::<Provider>.resolve_api_key()` and `.resolve_base_url()`)
- Define `<Provider>Client { inner: OpenAIChatCompletionsClient }` for OpenAI-compatible providers
- Implement `send_messages_stream()` delegating to inner client

### 2. Register provider kind

Edit `crates/zdx-providers/src/lib.rs`:
- Add `pub mod <provider>;`
- Add `ProviderKind` variant
- Add to `all()`, `id()`, `from_id()`, `label()`, `api_key_env_var()`, `default_base_url()`, `base_url_env_var()`, `auth_mode()`, `is_subscription()`
- Add prefix parsing in `parse_provider_prefix()`

### 3. Wire engine

Edit `crates/zdx-engine/src/core/agent.rs`:
- Import client/config
- Add `ProviderClient` variant and dispatch arm
- Add branch in provider selection/build function
- Add `build_<provider>_client()`

### 4. Wire config

Edit `crates/zdx-engine/src/config.rs`:
- Add field to `ProvidersConfig` struct
- Add serde default function
- Update `is_enabled()`, `get()`, `get_mut()`, `Default for ProvidersConfig`
- Add `default_<provider>_provider()` with initial model list

### 5. Register model updater

Edit `crates/zdx-cli/src/cli/commands/models.rs`:
- Add `ProviderSpec` to `provider_specs()` array
- Set `api_id` (models.dev provider key), `prefix`, `provider_cfg`

### 6. TUI auth label

Edit `crates/zdx-tui/src/features/auth/render.rs`:
- Add auth title match arm for the new `ProviderKind` variant

### 7. Regenerate and verify

```sh
just update-defaults
just ci-fast
```

## Workflow 3: Update pricing or metadata only

When a provider changes pricing but no new models are added:

1. If the change is systematic: just re-run `just update-models` — it fetches fresh data from models.dev/OpenRouter.
2. If the fetched data is **wrong** (promotional/launch pricing, stale rates, or a pricing-tier threshold reported as the context window): add a **pinned override** in `crates/zdx-assets/model_overrides.toml`, then re-run `just update-models`. This is the durable fix — manual edits to `default_models.toml`/`models.toml` get overwritten on the next update, overrides do not.
3. Don't forget cache pricing (`cache_read`, `cache_write`) — set to `0.0` if not supported.

### Pinned overrides (`model_overrides.toml`)

`zdx models update` applies `crates/zdx-assets/model_overrides.toml` **after** fetching from models.dev/OpenRouter, so pinned fields always win. Use it for providers where upstream is known-wrong (e.g. Xiaomi MiMo served old tiered pricing; MiniMax-M3 served the launch promo + 512K tier threshold as context).

- Match is by exact model `id` (the same `id` written to `models.toml`).
- Only the fields you set override the fetched value; omit a field to let upstream pass through.
- Overridable fields: `input`, `output`, `cache_read`, `cache_write`, `context_limit`, `output_limit`, `reasoning`, `input_images`.
- Always include a source URL + verification date comment. Keep the list small — each pin can hide a real upstream price change.
- On update, `zdx models update` prints `Info: applied override for '<id>'` per pin, and `Warning: override for '<id>' did not match any fetched model` if an `id` is stale.

```toml
[[override]]
id = "minimax:MiniMax-M3"
# Source: https://platform.minimax.io/docs/guides/pricing-paygo (standard, non-promo). Verified: 2026-06-08
input = 0.6
output = 2.4
cache_read = 0.12
context_limit = 1000000
```

The override layer lives in `update()` (`crates/zdx-cli/src/cli/commands/models.rs`, `apply_overrides()`), so it covers both the runtime `$ZDX_HOME/models.toml` and the regenerated repo `default_models.toml`.

## TUI / UI impact

**New model on existing provider — no TUI changes needed.** The model picker, pricing display, and thinking picker are all registry/config-driven:

- Model picker (`crates/zdx-tui/src/overlays/model_picker.rs`) loads from the registry, filters by enabled provider + `models` allow-list. New entries appear automatically.
- Usage/pricing display (`crates/zdx-tui/src/features/input/render.rs:611-650`) looks up `ModelOption` from the registry — context limit, pricing, and cost calculation are all dynamic.
- Thinking picker uses `model_supports_reasoning()` — driven by the `reasoning` capability in the registry.

**New provider — TUI changes required:**
- Auth title: add a match arm in `login_overlay_title()` at `crates/zdx-tui/src/features/auth/render.rs:40-64`.
- If it's an OAuth/CLI provider: add to the CLI provider selection list at `crates/zdx-tui/src/features/auth/render.rs:181-187` and the login flow in `crates/zdx-tui/src/overlays/login.rs:39-157`.
- API-key providers use the generic fallback automatically — just the title match is needed.

**Feature gates to be aware of:**
- Fast mode is hardcoded to specific providers only (`crates/zdx-tui/src/common/commands.rs:249-260`). New provider needing fast mode requires a code change.
- Context refine (exact token counting) is provider-specific (`crates/zdx-tui/src/runtime/context_analyze.rs:62-67`).

## Gotchas

- **Ordering**: always run `update-config` before `update-models` (or just `just update-defaults` which enforces this).
- **Subscription providers**: pricing is zeroed automatically — don't add pricing to their model entries. Check `ProviderKind::is_subscription()` for the current list.
- **Meta providers** (zen, apiyi): proxy models need separate entries with `capabilities.api` set (usually `"openai-completions"`).
- **`model_supports_reasoning` defaults to `true`** when the field is unknown (`crates/zdx-engine/src/models.rs:120-122`). Explicitly set `reasoning = false` for non-reasoning models.
- **`default_models.toml` is generated** — manual edits get overwritten by `just update-models`. For corrections the updater fetches wrong, add a pin in `model_overrides.toml` instead (it survives regeneration); only fix the updater logic for structural problems an override can't express.
- **`default_config.toml` is generated** from Rust defaults — edit `config.rs`, not the TOML file directly.

## Key files

| File | Purpose |
|------|---------|
| `crates/zdx-engine/src/config.rs` | `ProvidersConfig`, `default_<provider>_provider()` functions |
| `crates/zdx-providers/src/lib.rs` | `ProviderKind` enum, provider metadata |
| `crates/zdx-providers/src/<provider>.rs` | Provider client implementation |
| `crates/zdx-engine/src/core/agent.rs` | `ProviderClient` dispatch, client builders |
| `crates/zdx-engine/src/models.rs` | Model registry load/parse |
| `crates/zdx-assets/default_models.toml` | Generated model registry (don't edit directly for new models) |
| `crates/zdx-assets/model_overrides.toml` | Pinned post-fetch overrides applied by `zdx models update` (durable price/metadata corrections) |
| `crates/zdx-assets/default_config.toml` | Generated config defaults (don't edit directly) |
| `crates/zdx-cli/src/cli/commands/models.rs` | `zdx models update` implementation, `ProviderSpec` |
| `crates/zdx-tui/src/features/auth/render.rs` | TUI auth labels per provider, CLI provider list |
| `crates/zdx-tui/src/overlays/model_picker.rs` | Model picker (dynamic from registry, no changes for new models) |
| `crates/zdx-tui/src/overlays/login.rs` | Login flow (hardcoded OAuth providers, API-key fallback) |
