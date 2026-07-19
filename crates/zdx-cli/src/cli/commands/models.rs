//! Models command handlers.

use std::collections::HashSet;
use std::fs;
use std::path::Path;
use std::sync::OnceLock;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use zdx_engine::config;
use zdx_engine::models::wildcard_match;
use zdx_engine::providers::provider_kind_from_id;

const MODELS_DEV_URL: &str = "https://models.dev/api.json";
const OPENROUTER_MODELS_URL: &str = "https://openrouter.ai/api/v1/models";

/// Default context limit for unknown models (136k tokens).
const DEFAULT_CONTEXT_LIMIT: u64 = 136_000;

#[derive(Debug, Deserialize)]
struct ApiResponse {
    #[serde(flatten)]
    providers: std::collections::BTreeMap<String, ProviderEntry>,
}

#[derive(Debug, Deserialize)]
struct ProviderEntry {
    models: Option<std::collections::BTreeMap<String, ModelEntry>>,
}

#[derive(Debug, Deserialize, Default, Clone)]
struct CostEntry {
    #[serde(default)]
    input: f64,
    #[serde(default)]
    output: f64,
    #[serde(default)]
    cache_read: f64,
    #[serde(default)]
    cache_write: f64,
}

#[derive(Debug, Deserialize, Default, Clone)]
struct LimitEntry {
    #[serde(default)]
    context: u64,
    #[serde(default)]
    output: u64,
}

#[derive(Debug, Deserialize, Default, Clone)]
struct ModalitiesEntry {
    #[serde(default)]
    input: Vec<String>,
}

#[derive(Debug, Deserialize, Default, Clone)]
struct ModelProviderEntry {
    #[serde(default)]
    npm: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ModelEntry {
    id: String,
    name: String,
    #[serde(default)]
    tool_call: bool,
    #[serde(default)]
    reasoning: bool,
    #[serde(default)]
    cost: CostEntry,
    #[serde(default)]
    limit: LimitEntry,
    #[serde(default)]
    modalities: ModalitiesEntry,
    #[serde(default)]
    provider: Option<ModelProviderEntry>,
}

#[derive(Debug, Clone, Default)]
struct ModelCandidate {
    full_id: String,
    display_name: String,
    pricing: ModelPricingRecord,
    context_limit: u64,
    capabilities: ModelCapabilitiesRecord,
    match_targets: Vec<String>,
}

#[derive(Debug, Serialize, Deserialize)]
struct ModelsFile {
    #[serde(rename = "model")]
    models: Vec<ModelRecord>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
struct ModelRecord {
    id: String,
    provider: String,
    display_name: String,
    context_limit: u64,
    pricing: ModelPricingRecord,
    #[serde(default)]
    capabilities: ModelCapabilitiesRecord,
}

#[derive(Debug, Serialize, Deserialize, Clone, Default)]
struct ModelPricingRecord {
    input: f64,
    output: f64,
    cache_read: f64,
    cache_write: f64,
}

#[derive(Debug, Serialize, Deserialize, Clone, Default)]
struct ModelCapabilitiesRecord {
    #[serde(default)]
    reasoning: bool,
    #[serde(default)]
    input_images: bool,
    #[serde(default)]
    output_limit: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    api: Option<String>,
}

#[derive(Debug, Deserialize)]
struct OpenRouterResponse {
    data: Vec<OpenRouterModel>,
}

#[derive(Debug, Deserialize)]
struct OpenRouterModel {
    id: String,
    name: String,
    context_length: Option<u64>,
    architecture: Option<OpenRouterArchitecture>,
    pricing: Option<OpenRouterPricing>,
    top_provider: Option<OpenRouterTopProvider>,
    #[serde(default)]
    supported_parameters: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct OpenRouterArchitecture {
    #[serde(default)]
    input_modalities: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct OpenRouterPricing {
    prompt: Option<String>,
    completion: Option<String>,
    input_cache_read: Option<String>,
}

#[derive(Debug, Deserialize)]
struct OpenRouterTopProvider {
    max_completion_tokens: Option<u64>,
}

pub async fn update(config: &config::Config) -> Result<()> {
    let url = std::env::var("MODELS_DEV_URL").unwrap_or_else(|_| MODELS_DEV_URL.to_string());
    let api = fetch_api(&url).await?;

    let openrouter_models = match fetch_openrouter_models().await {
        Ok(models) => {
            println!(
                "Info: loaded {} models from OpenRouter as fallback",
                models.len()
            );
            models
        }
        Err(e) => {
            println!("Warning: failed to fetch OpenRouter models: {e}");
            Vec::new()
        }
    };

    let mut state = UpdateState::default();
    for spec in provider_specs(config) {
        collect_provider_records(&spec, &api, &openrouter_models, &mut state)?;
    }

    if state.records.is_empty() {
        bail!("No models matched configured providers/models.");
    }

    apply_overrides(&mut state.records);

    let out_path = config.models_path();
    write_models_file(&out_path, &state.records)?;
    println!("Updated models at {}", out_path.display());
    Ok(())
}

/// Lists available models with the exact `provider:model` id to pass to `-m`.
///
/// Defaults to models from enabled providers only; `all` includes disabled ones.
pub fn list(config: &config::Config, provider: Option<&str>, all: bool, json: bool) -> Result<()> {
    use zdx_engine::models::{ModelOption, available_models, custom_provider_models};

    let mut models: Vec<&ModelOption> = available_models().iter().collect();
    models.extend(custom_provider_models(&config.providers));
    if !all {
        models.retain(|m| config.providers.is_enabled(m.provider));
    }
    if let Some(provider) = provider {
        models.retain(|m| m.provider.eq_ignore_ascii_case(provider));
    }
    models.sort_by(|a, b| (a.provider, a.id).cmp(&(b.provider, b.id)));

    if json {
        let items: Vec<serde_json::Value> = models
            .iter()
            .map(|m| {
                serde_json::json!({
                    "id": format!("{}:{}", m.provider, m.id),
                    "provider": m.provider,
                    "model": m.id,
                    "display_name": m.display_name,
                    "context_limit": m.context_limit,
                    "reasoning": m.capabilities.reasoning,
                    "input_images": m.capabilities.input_images,
                    "pricing": {
                        "input": m.pricing.input,
                        "output": m.pricing.output,
                        "cache_read": m.pricing.cache_read,
                        "cache_write": m.pricing.cache_write,
                    },
                })
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&items)?);
        return Ok(());
    }

    let width = models
        .iter()
        .map(|m| m.provider.len() + 1 + m.id.len())
        .max()
        .unwrap_or(0);

    for m in &models {
        let full_id = format!("{}:{}", m.provider, m.id);
        println!("{full_id:<width$}  {}", m.display_name);
    }

    Ok(())
}

#[derive(Clone, Copy)]
struct ProviderSpec<'a> {
    provider_id: &'static str,
    api_id: &'static str,
    prefix: Option<&'static str>,
    provider_cfg: &'a config::ProviderConfig,
}

#[derive(Default)]
struct UpdateState {
    records: Vec<ModelRecord>,
    seen_keys: HashSet<String>,
}

impl UpdateState {
    fn push_candidate(&mut self, provider_id: &str, candidate: ModelCandidate) {
        let mut pricing = candidate.pricing;
        let is_sub = provider_kind_from_id(provider_id)
            .is_some_and(zdx_engine::providers::ProviderKind::is_subscription);
        if is_sub {
            pricing = ModelPricingRecord::default();
        }
        let record = ModelRecord {
            id: candidate.full_id,
            provider: provider_id.to_string(),
            display_name: candidate.display_name,
            context_limit: candidate.context_limit,
            pricing,
            capabilities: candidate.capabilities,
        };
        let key = record_key(&record);
        if self.seen_keys.insert(key) {
            self.records.push(record);
        }
    }
}

#[allow(clippy::too_many_lines)]
fn provider_specs(config: &config::Config) -> [ProviderSpec<'_>; 19] {
    [
        ProviderSpec {
            provider_id: "anthropic",
            api_id: "anthropic",
            prefix: None,
            provider_cfg: &config.providers.anthropic,
        },
        ProviderSpec {
            provider_id: "claude-cli",
            api_id: "anthropic",
            prefix: Some("claude-cli"),
            provider_cfg: &config.providers.claude_cli,
        },
        ProviderSpec {
            provider_id: "openai",
            api_id: "openai",
            prefix: Some("openai"),
            provider_cfg: &config.providers.openai,
        },
        ProviderSpec {
            provider_id: "openai-codex",
            api_id: "openai",
            prefix: None,
            provider_cfg: &config.providers.openai_codex,
        },
        ProviderSpec {
            provider_id: "openrouter",
            api_id: "openrouter",
            prefix: Some("openrouter"),
            provider_cfg: &config.providers.openrouter,
        },
        ProviderSpec {
            provider_id: "deepseek",
            api_id: "deepseek",
            prefix: Some("deepseek"),
            provider_cfg: &config.providers.deepseek,
        },
        ProviderSpec {
            provider_id: "moonshot",
            api_id: "moonshotai",
            prefix: Some("moonshot"),
            provider_cfg: &config.providers.moonshot,
        },
        ProviderSpec {
            provider_id: "stepfun",
            api_id: "stepfun",
            prefix: Some("stepfun"),
            provider_cfg: &config.providers.stepfun,
        },
        ProviderSpec {
            provider_id: "lmstudio",
            api_id: "lmstudio",
            prefix: Some("lmstudio"),
            provider_cfg: &config.providers.lmstudio,
        },
        ProviderSpec {
            provider_id: "xiaomi",
            api_id: "xiaomi",
            prefix: Some("xiaomi"),
            provider_cfg: &config.providers.xiaomi,
        },
        ProviderSpec {
            provider_id: "xiaomi-plan",
            api_id: "xiaomi",
            prefix: Some("xiaomi-plan"),
            provider_cfg: &config.providers.xiaomi_plan,
        },
        ProviderSpec {
            provider_id: "gemini",
            api_id: "google",
            prefix: Some("gemini"),
            provider_cfg: &config.providers.gemini,
        },
        ProviderSpec {
            provider_id: "google-antigravity",
            api_id: "google-antigravity",
            prefix: Some("google-antigravity"),
            provider_cfg: &config.providers.google_antigravity,
        },
        ProviderSpec {
            provider_id: "opencode-go",
            api_id: "opencode-go",
            prefix: Some("opencode-go"),
            provider_cfg: &config.providers.opencode_go,
        },
        ProviderSpec {
            provider_id: "minimax",
            api_id: "minimax",
            prefix: Some("minimax"),
            provider_cfg: &config.providers.minimax,
        },
        ProviderSpec {
            provider_id: "zai",
            api_id: "zai",
            prefix: Some("zai"),
            provider_cfg: &config.providers.zai,
        },
        ProviderSpec {
            provider_id: "xai",
            api_id: "xai",
            prefix: Some("xai"),
            provider_cfg: &config.providers.xai,
        },
        ProviderSpec {
            provider_id: "grok-build",
            api_id: "xai",
            prefix: Some("grok-build"),
            provider_cfg: &config.providers.grok_build,
        },
        ProviderSpec {
            provider_id: "meta",
            api_id: "meta",
            prefix: Some("meta"),
            provider_cfg: &config.providers.meta,
        },
    ]
}

fn collect_provider_records(
    spec: &ProviderSpec<'_>,
    api: &ApiResponse,
    openrouter_models: &[OpenRouterModel],
    state: &mut UpdateState,
) -> Result<()> {
    // Include all providers in the registry regardless of enabled status.
    // The registry is used for model lookups (e.g., checking reasoning support)
    // which should work even if the provider isn't currently enabled.
    if spec.provider_cfg.models.is_empty() {
        println!(
            "Warning: providers.{}.models is empty; skipping.",
            spec.provider_id
        );
        return Ok(());
    }

    let all_selected = if let Some(provider_entry) = api.providers.get(spec.api_id) {
        let Some(models_map) = provider_entry.models.as_ref() else {
            bail!(
                "Provider '{}' has no models in models.dev response",
                spec.api_id
            );
        };
        selected_candidates(spec, models_map, openrouter_models)
    } else if is_meta_provider(spec.provider_id) {
        selected_meta_candidates(spec, api, openrouter_models)
    } else {
        println!(
            "Warning: provider '{}' not found in models.dev response; falling back to defaults",
            spec.api_id
        );
        fallback_candidates(spec, openrouter_models)
    };

    if all_selected.is_empty() {
        println!(
            "Warning: no models matched providers.{}.models",
            spec.provider_id
        );
        return Ok(());
    }

    for candidate in all_selected {
        state.push_candidate(spec.provider_id, candidate);
    }
    Ok(())
}

fn fallback_candidates(
    spec: &ProviderSpec<'_>,
    openrouter_models: &[OpenRouterModel],
) -> Vec<ModelCandidate> {
    let mut fallback = Vec::new();
    for pattern in &spec.provider_cfg.models {
        let pattern = pattern.trim();
        if pattern.is_empty() {
            continue;
        }
        if is_pure_wildcard(pattern) {
            println!(
                "Warning: wildcard pattern '{}' for provider '{}' requires models.dev data",
                pattern, spec.provider_id
            );
            continue;
        }
        fallback.push(create_default_candidate(
            spec.provider_id,
            spec.prefix,
            pattern,
            openrouter_models,
        ));
    }
    fallback
}

fn selected_candidates(
    spec: &ProviderSpec<'_>,
    models_map: &std::collections::BTreeMap<String, ModelEntry>,
    openrouter_models: &[OpenRouterModel],
) -> Vec<ModelCandidate> {
    let candidates = build_candidates(spec.provider_id, spec.prefix, spec.api_id, models_map);
    selected_candidates_from_candidates(spec, &candidates, openrouter_models)
}

fn selected_candidates_from_candidates(
    spec: &ProviderSpec<'_>,
    candidates: &[ModelCandidate],
    openrouter_models: &[OpenRouterModel],
) -> Vec<ModelCandidate> {
    let select_result = select_candidates(spec.provider_id, &spec.provider_cfg.models, candidates);

    let mut all_selected = select_result.matched;
    for pattern in &select_result.unmatched_patterns {
        let default_candidate =
            create_default_candidate(spec.provider_id, spec.prefix, pattern, openrouter_models);
        println!(
            "Info: creating default entry for '{}' (not found in models.dev)",
            default_candidate.full_id
        );
        all_selected.push(default_candidate);
    }
    all_selected
}

fn selected_meta_candidates(
    spec: &ProviderSpec<'_>,
    api: &ApiResponse,
    openrouter_models: &[OpenRouterModel],
) -> Vec<ModelCandidate> {
    let mut selected = Vec::new();

    for pattern in &spec.provider_cfg.models {
        let pattern = pattern.trim();
        if pattern.is_empty() {
            continue;
        }
        if is_pure_wildcard(pattern) {
            println!(
                "Warning: wildcard pattern '{}' for provider '{}' requires models.dev data",
                pattern, spec.provider_id
            );
            continue;
        }

        if let Some(candidate) = select_meta_candidate_from_official_sources(spec, api, pattern) {
            selected.push(candidate);
            continue;
        }

        let default_candidate =
            create_default_candidate(spec.provider_id, spec.prefix, pattern, openrouter_models);
        println!(
            "Info: creating default entry for '{}' (not found in official providers)",
            default_candidate.full_id
        );
        selected.push(default_candidate);
    }

    selected
}

fn select_meta_candidate_from_official_sources(
    spec: &ProviderSpec<'_>,
    api: &ApiResponse,
    pattern: &str,
) -> Option<ModelCandidate> {
    use zdx_engine::providers::resolve_provider;

    let target_model = resolve_provider(pattern).model;
    let source_providers = official_source_provider_ids(&target_model);

    for source_provider in source_providers {
        if let Some(model) = lookup_model_in_provider(api, source_provider, &target_model) {
            return Some(candidate_from_model_entry(
                spec.provider_id,
                spec.prefix,
                pattern,
                source_provider,
                model,
            ));
        }
    }

    let normalized = normalize_model_lookup_id(&target_model);
    if normalized != target_model.to_ascii_lowercase() {
        for source_provider in source_providers {
            if let Some(model) = lookup_model_in_provider(api, source_provider, &normalized) {
                return Some(candidate_from_model_entry(
                    spec.provider_id,
                    spec.prefix,
                    pattern,
                    source_provider,
                    model,
                ));
            }
        }
    }

    None
}

fn official_source_provider_ids(model_id: &str) -> &'static [&'static str] {
    let lower = model_id.to_ascii_lowercase();
    if lower.starts_with("gemini") {
        &["google", "google-vertex"]
    } else if lower.starts_with("kimi") {
        &["moonshotai", "moonshotai-cn"]
    } else if lower.starts_with("glm") {
        &["zhipuai", "zai"]
    } else if lower.starts_with("minimax") {
        &["minimax", "minimax-cn"]
    } else {
        &[]
    }
}

fn lookup_model_in_provider<'a>(
    api: &'a ApiResponse,
    provider_id: &str,
    model_id: &str,
) -> Option<&'a ModelEntry> {
    let models_map = api.providers.get(provider_id)?.models.as_ref()?;

    // 1) exact key lookup
    if let Some(model) = models_map.get(model_id)
        && model.tool_call
    {
        return Some(model);
    }

    // 2) case-insensitive key or model.id lookup
    models_map.iter().find_map(|(key, model)| {
        (model.tool_call
            && (key.eq_ignore_ascii_case(model_id) || model.id.eq_ignore_ascii_case(model_id)))
        .then_some(model)
    })
}

fn candidate_from_model_entry(
    provider_id: &str,
    prefix: Option<&str>,
    raw_id: &str,
    source_provider_id: &str,
    model: &ModelEntry,
) -> ModelCandidate {
    let full_id = format_model_id(prefix, raw_id);
    let input_images = model
        .modalities
        .input
        .iter()
        .any(|modality| modality == "image");

    ModelCandidate {
        full_id: full_id.clone(),
        display_name: format_display_name(provider_id, &model.name),
        pricing: ModelPricingRecord {
            input: model.cost.input,
            output: model.cost.output,
            cache_read: model.cost.cache_read,
            cache_write: model.cost.cache_write,
        },
        context_limit: model.limit.context,
        capabilities: ModelCapabilitiesRecord {
            reasoning: model.reasoning,
            input_images,
            output_limit: model.limit.output,
            api: model_api_hint(provider_id, Some(source_provider_id), model),
        },
        match_targets: build_match_targets(provider_id, raw_id, &full_id),
    }
}

async fn fetch_api(url: &str) -> Result<ApiResponse> {
    let response = reqwest::get(url)
        .await
        .context("Failed to fetch models.dev API")?;

    if !response.status().is_success() {
        bail!(
            "models.dev request failed with status {}: {}",
            response.status(),
            response.status().canonical_reason().unwrap_or("Unknown")
        );
    }

    response
        .json::<ApiResponse>()
        .await
        .context("Failed to parse models.dev response as JSON")
}

async fn fetch_openrouter_models() -> Result<Vec<OpenRouterModel>> {
    let response = reqwest::get(OPENROUTER_MODELS_URL)
        .await
        .context("Failed to fetch OpenRouter models API")?;

    if !response.status().is_success() {
        bail!("OpenRouter API returned status {}", response.status());
    }

    let data: OpenRouterResponse = response
        .json()
        .await
        .context("Failed to parse OpenRouter response")?;

    Ok(data.data)
}

fn build_candidates(
    provider_id: &str,
    prefix: Option<&str>,
    source_provider_id: &str,
    models_map: &std::collections::BTreeMap<String, ModelEntry>,
) -> Vec<ModelCandidate> {
    models_map
        .values()
        .filter(|model| model.tool_call)
        .map(|model| {
            candidate_from_model_entry(provider_id, prefix, &model.id, source_provider_id, model)
        })
        .collect()
}

fn model_api_hint(
    provider_id: &str,
    source_provider_id: Option<&str>,
    model: &ModelEntry,
) -> Option<String> {
    if !is_meta_provider(provider_id) {
        return None;
    }

    // The npm field reflects which AI-SDK adapter the proxy uses internally,
    // not necessarily the wire format exposed to us.
    let npm = model
        .provider
        .as_ref()
        .and_then(|provider| provider.npm.as_deref());

    let api = match npm {
        // The opencode-go proxy exposes OpenAI-family models via its
        // openai-compatible `/v1/chat/completions` endpoint, not the OpenAI
        // Responses API — e.g. grok-4.5 (models.dev tags it `@ai-sdk/openai`)
        // is served at `/zen/go/v1/chat/completions`.
        Some("@ai-sdk/openai" | "@ai-sdk/openai-compatible") => "openai-completions",
        Some("@ai-sdk/google") => "google-generative-ai",
        // The proxy serves these models only via the Anthropic Messages API
        // (`/v1/messages`); e.g. qwen3.7-max rejects the chat-completions
        // ("oa-compat") format with a 401.
        Some("@ai-sdk/anthropic") => "anthropic-messages",
        _ => source_provider_default_api(source_provider_id.unwrap_or(provider_id)),
    };

    Some(api.to_string())
}

fn source_provider_default_api(source_provider_id: &str) -> &'static str {
    match source_provider_id {
        "anthropic" => "anthropic-messages",
        "openai" => "openai-responses",
        "google" | "google-vertex" => "google-generative-ai",
        _ => "openai-completions",
    }
}

/// Result of selecting candidates from patterns.
struct SelectResult {
    /// Candidates that matched patterns in the API response.
    matched: Vec<ModelCandidate>,
    /// Patterns that didn't match any model in the API (non-wildcard only).
    unmatched_patterns: Vec<String>,
}

fn select_candidates(
    provider_id: &str,
    patterns: &[String],
    candidates: &[ModelCandidate],
) -> SelectResult {
    let mut matched = Vec::new();
    let mut unmatched_patterns = Vec::new();

    for pattern in patterns {
        let pattern = pattern.trim();
        if pattern.is_empty() {
            continue;
        }

        let candidates_for_pattern: Vec<&ModelCandidate> = candidates
            .iter()
            .filter(|candidate| matches_pattern(pattern, candidate))
            .collect();

        if candidates_for_pattern.is_empty() {
            // Only track as unmatched if it's not a pure wildcard pattern
            // Patterns like "xiaomi/mimo-v2-flash" or "gpt-5*" are candidates for defaults
            // Pure "*" wildcards are not - they just mean "all models" which matched nothing
            if is_pure_wildcard(pattern) {
                println!(
                    "Warning: wildcard pattern '{pattern}' for provider '{provider_id}' matched no models"
                );
            } else {
                unmatched_patterns.push(pattern.to_string());
            }
            continue;
        }

        matched.extend(candidates_for_pattern.into_iter().cloned());
    }

    SelectResult {
        matched,
        unmatched_patterns,
    }
}

/// Returns true if the pattern is a pure wildcard that shouldn't create default entries.
fn is_pure_wildcard(pattern: &str) -> bool {
    pattern == "*"
}

fn is_meta_provider(provider_id: &str) -> bool {
    provider_id == "opencode-go"
}

/// Looks up a model in the embedded `default_models.toml` by ID.
/// Uses provider resolution to match models with different prefixes.
fn lookup_default_model(full_id: &str) -> Option<ModelRecord> {
    use zdx_engine::providers::{ProviderKind, provider_kind_from_id, resolve_provider};

    static DEFAULTS: OnceLock<Option<ModelsFile>> = OnceLock::new();
    let defaults = DEFAULTS.get_or_init(|| {
        match toml::from_str::<ModelsFile>(zdx_engine::models::default_models_toml()) {
            Ok(file) => Some(file),
            Err(e) => {
                eprintln!("Warning: failed to parse default_models.toml: {e}");
                None
            }
        }
    });
    let defaults = defaults.as_ref()?;

    let target = resolve_provider(full_id);
    let target_model_normalized = normalize_model_lookup_id(&target.model);
    let is_meta_target = target.kind == ProviderKind::OpencodeGo;

    defaults
        .models
        .iter()
        .find(|record| {
            // Try exact match first
            if record.id == full_id {
                return true;
            }
            // Fall back to provider-based match using record.provider field
            // This handles cases where record.id has no prefix (e.g., "step-3.5-flash")
            // but record.provider specifies the correct provider (e.g., "stepfun")
            if let Some(candidate_kind) = provider_kind_from_id(&record.provider)
                && candidate_kind == target.kind
                && record.id == target.model
            {
                return true;
            }

            // Meta-providers (opencode-go) can reuse defaults from underlying providers
            // when model IDs match (e.g., opencode-go:glm-5.1 -> zai:glm-5.1).
            if is_meta_target {
                let resolved_record = resolve_provider(&record.id);
                let resolved_match =
                    normalize_model_lookup_id(&resolved_record.model) == target_model_normalized;
                let raw_match = normalize_model_lookup_id(&record.id) == target_model_normalized;
                return resolved_match || raw_match;
            }

            false
        })
        .cloned()
}

fn normalize_model_lookup_id(model_id: &str) -> String {
    let lower = model_id.trim().to_ascii_lowercase();
    let lower = lower
        .strip_suffix("-thinking")
        .or_else(|| lower.strip_suffix("-nothinking"))
        .unwrap_or(&lower);
    lower.to_string()
}

/// Looks up a model in the `OpenRouter` API data and converts it to a `ModelCandidate`.
fn lookup_openrouter_model(
    provider_id: &str,
    model_id: &str,
    openrouter_models: &[OpenRouterModel],
) -> Option<ModelCandidate> {
    // Map our provider IDs to OpenRouter vendor prefixes
    let vendor = match provider_id {
        "xiaomi" | "xiaomi-plan" => "xiaomi",
        "minimax" => "minimax",
        "xai" | "grok-build" => "x-ai",
        "anthropic" | "claude-cli" => "anthropic",
        "openai" | "openai-codex" => "openai",
        "gemini" | "google-antigravity" => "google",
        "stepfun" => "stepfun",
        "moonshot" => "moonshotai",
        "zai" => "z-ai",
        other => other,
    };

    let openrouter_id = format!("{vendor}/{model_id}");

    let or_model = openrouter_models
        .iter()
        .find(|m| m.id.eq_ignore_ascii_case(&openrouter_id))?;

    let per_m = 1_000_000.0;
    let pricing = or_model
        .pricing
        .as_ref()
        .map(|p| ModelPricingRecord {
            input: p
                .prompt
                .as_deref()
                .and_then(|s| s.parse::<f64>().ok())
                .unwrap_or(0.0)
                * per_m,
            output: p
                .completion
                .as_deref()
                .and_then(|s| s.parse::<f64>().ok())
                .unwrap_or(0.0)
                * per_m,
            cache_read: p
                .input_cache_read
                .as_deref()
                .and_then(|s| s.parse::<f64>().ok())
                .unwrap_or(0.0)
                * per_m,
            cache_write: 0.0,
        })
        .unwrap_or_default();

    let input_images = or_model
        .architecture
        .as_ref()
        .is_some_and(|a| a.input_modalities.iter().any(|m| m == "image"));

    let output_limit = or_model
        .top_provider
        .as_ref()
        .and_then(|tp| tp.max_completion_tokens)
        .unwrap_or(0);

    let display_name = or_model.name.clone();

    let reasoning = or_model
        .supported_parameters
        .iter()
        .any(|p| p == "reasoning");

    let capabilities = ModelCapabilitiesRecord {
        reasoning,
        input_images,
        output_limit,
        api: None,
    };

    let full_id = format!("{provider_id}:{model_id}");
    let match_targets = build_match_targets(provider_id, model_id, &full_id);

    Some(ModelCandidate {
        full_id,
        display_name,
        pricing,
        context_limit: or_model.context_length.unwrap_or(DEFAULT_CONTEXT_LIMIT),
        capabilities,
        match_targets,
    })
}

/// Creates a default `ModelCandidate` for a model ID not found in the API.
/// Looks up pricing and capabilities from embedded `default_models.toml` if available.
fn create_default_candidate(
    provider_id: &str,
    prefix: Option<&str>,
    model_id: &str,
    openrouter_models: &[OpenRouterModel],
) -> ModelCandidate {
    // Use the pattern as-is for the model ID - don't try to parse it
    // OpenRouter models have IDs like "xiaomi/mimo-v2-flash:free" which should stay intact
    let full_id = format_model_id(prefix, model_id);
    let match_targets = build_match_targets(provider_id, model_id, &full_id);

    // Try to find this model in the embedded default_models.toml
    if let Some(default_model) = lookup_default_model(&full_id) {
        return ModelCandidate {
            full_id,
            display_name: default_model.display_name,
            pricing: default_model.pricing,
            context_limit: default_model.context_limit,
            capabilities: default_model.capabilities,
            match_targets,
        };
    }

    // Try OpenRouter as secondary fallback
    if let Some(or_candidate) = lookup_openrouter_model(provider_id, model_id, openrouter_models) {
        println!("Info: using OpenRouter data for '{}'", or_candidate.full_id);
        return or_candidate;
    }

    // Fall back to generic defaults
    let display_name = format!("{model_id} (custom)");
    let capabilities = if is_meta_provider(provider_id) {
        ModelCapabilitiesRecord {
            api: Some("openai-completions".to_string()),
            ..Default::default()
        }
    } else {
        ModelCapabilitiesRecord::default()
    };

    ModelCandidate {
        full_id,
        display_name,
        context_limit: DEFAULT_CONTEXT_LIMIT,
        match_targets,
        capabilities,
        ..Default::default()
    }
}

fn build_match_targets(provider_id: &str, raw_id: &str, full_id: &str) -> Vec<String> {
    let mut targets = vec![full_id.to_string(), raw_id.to_string()];
    targets.push(format!("{provider_id}:{raw_id}"));
    targets.push(format!("{provider_id}/{raw_id}"));

    if provider_id == "anthropic" || provider_id == "claude-cli" {
        targets.push(format!("claude:{raw_id}"));
        targets.push(format!("claude/{raw_id}"));
    }

    if provider_id == "gemini" {
        targets.push(format!("google:{raw_id}"));
        targets.push(format!("google/{raw_id}"));
    }

    targets
}

fn matches_pattern(pattern: &str, candidate: &ModelCandidate) -> bool {
    candidate
        .match_targets
        .iter()
        .any(|target| wildcard_match(pattern, target))
}

fn format_display_name(provider_id: &str, name: &str) -> String {
    if provider_id == "anthropic" || provider_id == "claude-cli" {
        name.replace(" (latest)", "")
    } else {
        name.to_string()
    }
}

fn format_model_id(prefix: Option<&str>, raw_id: &str) -> String {
    match prefix {
        Some(prefix) => format!("{prefix}:{raw_id}"),
        None => raw_id.to_string(),
    }
}

fn record_key(record: &ModelRecord) -> String {
    format!("{}::{}", record.provider, record.id)
}

#[derive(Debug, Deserialize)]
struct OverridesFile {
    #[serde(rename = "override", default)]
    overrides: Vec<ModelOverride>,
}

/// One `model_overrides.toml` entry. Set fields override fetched values; the rest pass through.
#[derive(Debug, Deserialize)]
struct ModelOverride {
    id: String,
    #[serde(default)]
    input: Option<f64>,
    #[serde(default)]
    output: Option<f64>,
    #[serde(default)]
    cache_read: Option<f64>,
    #[serde(default)]
    cache_write: Option<f64>,
    #[serde(default)]
    context_limit: Option<u64>,
    #[serde(default)]
    output_limit: Option<u64>,
    #[serde(default)]
    reasoning: Option<bool>,
    #[serde(default)]
    input_images: Option<bool>,
}

/// Pins known-correct values over stale/promotional upstream data, keyed by exact `id`.
fn apply_overrides(records: &mut [ModelRecord]) {
    let overrides_toml = zdx_engine::models::default_model_overrides_toml();
    let parsed = match toml::from_str::<OverridesFile>(overrides_toml) {
        Ok(file) => file,
        Err(e) => {
            eprintln!("Warning: failed to parse model_overrides.toml: {e}");
            return;
        }
    };

    for ov in &parsed.overrides {
        let Some(record) = records.iter_mut().find(|r| r.id == ov.id) else {
            continue;
        };

        if let Some(v) = ov.input {
            record.pricing.input = v;
        }
        if let Some(v) = ov.output {
            record.pricing.output = v;
        }
        if let Some(v) = ov.cache_read {
            record.pricing.cache_read = v;
        }
        if let Some(v) = ov.cache_write {
            record.pricing.cache_write = v;
        }
        if let Some(v) = ov.context_limit {
            record.context_limit = v;
        }
        if let Some(v) = ov.output_limit {
            record.capabilities.output_limit = v;
        }
        if let Some(v) = ov.reasoning {
            record.capabilities.reasoning = v;
        }
        if let Some(v) = ov.input_images {
            record.capabilities.input_images = v;
        }

        println!("Info: applied override for '{}'", ov.id);
    }
}

fn write_models_file(path: &Path, models: &[ModelRecord]) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("Failed to create directory {}", parent.display()))?;
    }

    let body = toml::to_string_pretty(&ModelsFile {
        models: models.to_vec(),
    })
    .context("Failed to serialize models file")?;

    let header = concat!(
        "# Generated by zdx models update\n",
        "# Edit this file to customize the model picker.\n\n",
    );

    let mut content = format!("{header}{body}");
    if !content.ends_with('\n') {
        content.push('\n');
    }

    let tmp_path = path.with_extension("toml.tmp");
    fs::write(&tmp_path, content)
        .with_context(|| format!("Failed to write models file at {}", tmp_path.display()))?;
    fs::rename(&tmp_path, path).with_context(|| {
        format!(
            "Failed to rename {} to {}",
            tmp_path.display(),
            path.display()
        )
    })?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_create_default_candidate_preserves_full_id() {
        let candidate = create_default_candidate(
            "openrouter",
            Some("openrouter"),
            "xiaomi/mimo-v2-flash:free",
            &[],
        );

        assert_eq!(candidate.full_id, "openrouter:xiaomi/mimo-v2-flash:free");
        assert_eq!(candidate.display_name, "xiaomi/mimo-v2-flash:free (custom)");
        assert_eq!(candidate.context_limit, DEFAULT_CONTEXT_LIMIT);
    }

    #[test]
    fn test_select_candidates_tracks_unmatched_patterns() {
        let candidates = vec![ModelCandidate {
            full_id: "openrouter:gpt-4".to_string(),
            match_targets: vec!["openrouter:gpt-4".to_string(), "gpt-4".to_string()],
            ..Default::default()
        }];

        let result = select_candidates(
            "openrouter",
            &[
                "gpt-4".to_string(),
                "xiaomi/mimo-v2-flash".to_string(), // unmatched - should create default
                "*".to_string(),                    // pure wildcard - should NOT create default
            ],
            &candidates,
        );

        assert_eq!(result.matched.len(), 2); // gpt-4 matched twice (pattern + wildcard)
        assert_eq!(result.unmatched_patterns, vec!["xiaomi/mimo-v2-flash"]);
    }

    #[test]
    fn test_lookup_default_model_uses_provider_field() {
        // This tests that lookup_default_model correctly matches models where
        // the record.id has no prefix but record.provider specifies the provider.
        // e.g., default_models.toml has: id = "step-3.5-flash", provider = "stepfun"
        // and we look up "stepfun:step-3.5-flash"

        let result = lookup_default_model("stepfun:step-3.5-flash");
        assert!(result.is_some(), "Should find stepfun model in defaults");

        let model = result.unwrap();
        assert_eq!(model.provider, "stepfun");
        assert_eq!(model.display_name, "Step 3.5 Flash");
        // Should NOT be "(custom)" since it's in default_models.toml
        assert!(
            !model.display_name.contains("custom"),
            "Should use display_name from defaults, not '(custom)'"
        );
    }

    #[test]
    fn test_lookup_default_model_xiaomi() {
        // Also test xiaomi which has similar structure
        let result = lookup_default_model("xiaomi:mimo-v2.5");
        assert!(result.is_some(), "Should find xiaomi model in defaults");

        let model = result.unwrap();
        assert_eq!(model.provider, "xiaomi");
        assert!(!model.display_name.contains("custom"));
    }

    #[test]
    fn test_lookup_default_model_meta_provider_uses_underlying_defaults() {
        let result = lookup_default_model("opencode-go:glm-5.2");
        assert!(result.is_some(), "Should find glm model for opencode-go");
    }

    #[test]
    fn test_lookup_default_model_meta_provider_normalizes_thinking_suffix() {
        let result = lookup_default_model("opencode-go:glm-5.2-thinking");
        assert!(
            result.is_some(),
            "Should map -thinking variant to base model default"
        );
    }

    #[test]
    fn test_provider_specs_includes_opencode_go() {
        let config = config::Config::default();
        let specs = provider_specs(&config);

        assert!(specs.iter().any(|s| {
            s.provider_id == "opencode-go"
                && s.api_id == "opencode-go"
                && s.prefix == Some("opencode-go")
        }));
    }

    #[test]
    fn test_provider_specs_includes_meta() {
        let config = config::Config::default();
        let specs = provider_specs(&config);

        assert!(
            specs.iter().any(|s| {
                s.provider_id == "meta" && s.api_id == "meta" && s.prefix == Some("meta")
            }),
            "provider_specs must include meta so `zdx models update` keeps Muse Spark"
        );
    }

    #[test]
    fn test_lookup_default_model_meta_preserves_muse_spark_metadata() {
        // Meta is not on models.dev, so `zdx models update` falls back to the
        // embedded default record. Verify it carries the pinned Muse Spark
        // pricing/context/capabilities instead of a "(custom)" placeholder.
        let result = lookup_default_model("meta:muse-spark-1.1");
        assert!(result.is_some(), "Should find meta model in defaults");

        let model = result.unwrap();
        assert_eq!(model.provider, "meta");
        assert_eq!(model.display_name, "Muse Spark 1.1");
        assert!(!model.display_name.contains("custom"));
        assert_eq!(model.context_limit, 1_000_000);
        assert!((model.pricing.input - 1.25).abs() < f64::EPSILON);
        assert!((model.pricing.output - 4.25).abs() < f64::EPSILON);
        assert!(model.capabilities.reasoning);
        assert!(model.capabilities.input_images);
    }

    #[test]
    fn test_provider_specs_includes_both_xiaomi_variants() {
        let config = config::Config::default();
        let specs = provider_specs(&config);

        assert!(
            specs.iter().any(|s| {
                s.provider_id == "xiaomi" && s.api_id == "xiaomi" && s.prefix == Some("xiaomi")
            }),
            "provider_specs must include xiaomi"
        );
        assert!(
            specs.iter().any(|s| {
                s.provider_id == "xiaomi-plan"
                    && s.api_id == "xiaomi"
                    && s.prefix == Some("xiaomi-plan")
            }),
            "provider_specs must include xiaomi-plan so `zdx models update` keeps Plan models"
        );
    }

    #[test]
    fn test_official_source_provider_ids_for_meta_models() {
        assert_eq!(official_source_provider_ids("glm-5"), &["zhipuai", "zai"]);
        assert_eq!(
            official_source_provider_ids("gemini-2.5-flash"),
            &["google", "google-vertex"]
        );
        assert_eq!(
            official_source_provider_ids("kimi-k2.5"),
            &["moonshotai", "moonshotai-cn"]
        );
        assert_eq!(
            official_source_provider_ids("MiniMax-M2.5"),
            &["minimax", "minimax-cn"]
        );
    }

    #[test]
    fn test_normalize_model_lookup_id_handles_thinking_suffixes() {
        assert_eq!(
            normalize_model_lookup_id("gemini-3-pro-preview-thinking"),
            "gemini-3-pro-preview"
        );
        assert_eq!(
            normalize_model_lookup_id("gemini-3-flash-preview-nothinking"),
            "gemini-3-flash-preview"
        );
    }

    #[test]
    fn test_model_api_hint_for_meta_provider_prefers_source_provider_over_npm() {
        // The npm field reflects the AI-SDK adapter used internally by the
        // proxy, which may differ from the actual wire format. The opencode-go
        // proxy only exposes openai-compatible chat-completions and anthropic
        // messages endpoints, so @ai-sdk/openai maps to openai-completions
        // (not the OpenAI Responses API) and @ai-sdk/anthropic to
        // anthropic-messages for known third-party models like minimax.
        let model = ModelEntry {
            id: "gpt-5".to_string(),
            name: "GPT-5".to_string(),
            tool_call: true,
            reasoning: true,
            cost: CostEntry::default(),
            limit: LimitEntry::default(),
            modalities: ModalitiesEntry::default(),
            provider: Some(ModelProviderEntry {
                npm: Some("@ai-sdk/openai".to_string()),
            }),
        };

        // npm @ai-sdk/openai → openai-completions (proxy has no Responses API)
        assert_eq!(
            model_api_hint("opencode-go", Some("opencode-go"), &model).as_deref(),
            Some("openai-completions")
        );

        // npm @ai-sdk/anthropic for minimax → anthropic-messages
        // (the proxy serves these via /v1/messages, not oa-compat)
        let minimax_model = ModelEntry {
            id: "minimax-m2.5-free".to_string(),
            name: "MiniMax M2.5 Free".to_string(),
            tool_call: true,
            reasoning: true,
            cost: CostEntry::default(),
            limit: LimitEntry::default(),
            modalities: ModalitiesEntry::default(),
            provider: Some(ModelProviderEntry {
                npm: Some("@ai-sdk/anthropic".to_string()),
            }),
        };

        assert_eq!(
            model_api_hint("opencode-go", Some("opencode-go"), &minimax_model).as_deref(),
            Some("anthropic-messages")
        );

        // npm @ai-sdk/anthropic for claude → anthropic-messages (trusted)
        let claude_model = ModelEntry {
            id: "claude-sonnet-4-5".to_string(),
            name: "Claude Sonnet 4.5".to_string(),
            tool_call: true,
            reasoning: true,
            cost: CostEntry::default(),
            limit: LimitEntry::default(),
            modalities: ModalitiesEntry::default(),
            provider: Some(ModelProviderEntry {
                npm: Some("@ai-sdk/anthropic".to_string()),
            }),
        };

        assert_eq!(
            model_api_hint("opencode-go", Some("opencode-go"), &claude_model).as_deref(),
            Some("anthropic-messages")
        );
    }

    #[test]
    fn test_model_api_hint_for_meta_provider_defaults_from_source_provider() {
        let model = ModelEntry {
            id: "gemini-2.5-flash".to_string(),
            name: "Gemini 2.5 Flash".to_string(),
            tool_call: true,
            reasoning: true,
            cost: CostEntry::default(),
            limit: LimitEntry::default(),
            modalities: ModalitiesEntry::default(),
            provider: None,
        };

        assert_eq!(
            model_api_hint("opencode-go", Some("google"), &model).as_deref(),
            Some("google-generative-ai")
        );
        assert_eq!(
            model_api_hint("opencode-go", Some("moonshotai"), &model).as_deref(),
            Some("openai-completions")
        );
    }

    #[test]
    fn test_model_api_hint_non_meta_provider_is_none() {
        let model = ModelEntry {
            id: "gpt-5".to_string(),
            name: "GPT-5".to_string(),
            tool_call: true,
            reasoning: true,
            cost: CostEntry::default(),
            limit: LimitEntry::default(),
            modalities: ModalitiesEntry::default(),
            provider: Some(ModelProviderEntry {
                npm: Some("@ai-sdk/openai".to_string()),
            }),
        };

        assert_eq!(model_api_hint("openai", Some("openai"), &model), None);
    }
}
