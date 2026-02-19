//! Models command handlers.

use std::collections::HashSet;
use std::fs;
use std::path::Path;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use zdx_core::config;

const MODELS_DEV_URL: &str = "https://models.dev/api.json";

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

pub async fn update(config: &config::Config) -> Result<()> {
    let url = std::env::var("MODELS_DEV_URL").unwrap_or_else(|_| MODELS_DEV_URL.to_string());
    let api = fetch_api(&url).await?;

    let mut state = UpdateState::default();
    for spec in provider_specs(config) {
        collect_provider_records(&spec, &api, &mut state)?;
    }

    if state.records.is_empty() {
        bail!("No models matched configured providers/models.");
    }

    let out_path = config.models_path();
    write_models_file(&out_path, &state.records)?;
    println!("Updated models at {}", out_path.display());
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
        let record = ModelRecord {
            id: candidate.full_id,
            provider: provider_id.to_string(),
            display_name: candidate.display_name,
            context_limit: candidate.context_limit,
            pricing: candidate.pricing,
            capabilities: candidate.capabilities,
        };
        let key = record_key(&record);
        if self.seen_keys.insert(key) {
            self.records.push(record);
        }
    }
}

fn provider_specs(config: &config::Config) -> [ProviderSpec<'_>; 12] {
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
            provider_id: "mimo",
            api_id: "xiaomi",
            prefix: Some("mimo"),
            provider_cfg: &config.providers.mimo,
        },
        ProviderSpec {
            provider_id: "gemini",
            api_id: "google",
            prefix: Some("gemini"),
            provider_cfg: &config.providers.gemini,
        },
        ProviderSpec {
            provider_id: "gemini-cli",
            api_id: "google",
            prefix: Some("gemini-cli"),
            provider_cfg: &config.providers.gemini_cli,
        },
        ProviderSpec {
            provider_id: "zen",
            api_id: "opencode",
            prefix: Some("zen"),
            provider_cfg: &config.providers.zen,
        },
        ProviderSpec {
            provider_id: "apiyi",
            api_id: "apiyi",
            prefix: Some("apiyi"),
            provider_cfg: &config.providers.apiyi,
        },
    ]
}

fn collect_provider_records(
    spec: &ProviderSpec<'_>,
    api: &ApiResponse,
    state: &mut UpdateState,
) -> Result<()> {
    // Include all providers in the registry regardless of enabled status.
    // The registry is used for model lookups (e.g., checking reasoning support)
    // which should work even if the provider isn't currently enabled.
    if spec.provider_cfg.models.is_empty() {
        eprintln!(
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
        selected_candidates(spec, models_map)
    } else if is_meta_provider(spec.provider_id) {
        selected_meta_candidates(spec, api)
    } else {
        eprintln!(
            "Warning: provider '{}' not found in models.dev response; falling back to defaults",
            spec.api_id
        );
        fallback_candidates(spec)
    };

    if all_selected.is_empty() {
        eprintln!(
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

fn fallback_candidates(spec: &ProviderSpec<'_>) -> Vec<ModelCandidate> {
    let mut fallback = Vec::new();
    for pattern in &spec.provider_cfg.models {
        let pattern = pattern.trim();
        if pattern.is_empty() {
            continue;
        }
        if is_pure_wildcard(pattern) {
            eprintln!(
                "Warning: wildcard pattern '{}' for provider '{}' requires models.dev data",
                pattern, spec.provider_id
            );
            continue;
        }
        fallback.push(create_default_candidate(
            spec.provider_id,
            spec.prefix,
            pattern,
        ));
    }
    fallback
}

fn selected_candidates(
    spec: &ProviderSpec<'_>,
    models_map: &std::collections::BTreeMap<String, ModelEntry>,
) -> Vec<ModelCandidate> {
    let candidates = build_candidates(spec.provider_id, spec.prefix, spec.api_id, models_map);
    selected_candidates_from_candidates(spec, &candidates)
}

fn selected_candidates_from_candidates(
    spec: &ProviderSpec<'_>,
    candidates: &[ModelCandidate],
) -> Vec<ModelCandidate> {
    let select_result = select_candidates(spec.provider_id, &spec.provider_cfg.models, candidates);

    let mut all_selected = select_result.matched;
    for pattern in &select_result.unmatched_patterns {
        let default_candidate = create_default_candidate(spec.provider_id, spec.prefix, pattern);
        eprintln!(
            "Info: creating default entry for '{}' (not found in models.dev)",
            default_candidate.full_id
        );
        all_selected.push(default_candidate);
    }
    all_selected
}

fn selected_meta_candidates(spec: &ProviderSpec<'_>, api: &ApiResponse) -> Vec<ModelCandidate> {
    let mut selected = Vec::new();

    for pattern in &spec.provider_cfg.models {
        let pattern = pattern.trim();
        if pattern.is_empty() {
            continue;
        }
        if is_pure_wildcard(pattern) {
            eprintln!(
                "Warning: wildcard pattern '{}' for provider '{}' requires models.dev data",
                pattern, spec.provider_id
            );
            continue;
        }

        if let Some(candidate) = select_meta_candidate_from_official_sources(spec, api, pattern) {
            selected.push(candidate);
            continue;
        }

        let default_candidate = create_default_candidate(spec.provider_id, spec.prefix, pattern);
        eprintln!(
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
    use zdx_core::providers::resolve_provider;

    let target_model = resolve_provider(pattern).model;
    let source_providers = official_source_provider_ids(&target_model);

    for source_provider in source_providers {
        if let Some(model) = lookup_model_in_provider(api, source_provider, &target_model) {
            return Some(candidate_from_model_entry(
                spec,
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
                    spec,
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
    spec: &ProviderSpec<'_>,
    pattern: &str,
    source_provider_id: &str,
    model: &ModelEntry,
) -> ModelCandidate {
    let full_id = format_model_id(spec.prefix, pattern);
    let input_images = model
        .modalities
        .input
        .iter()
        .any(|modality| modality == "image");

    ModelCandidate {
        full_id: full_id.clone(),
        display_name: format_display_name(spec.provider_id, &model.name),
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
            api: model_api_hint(spec.provider_id, Some(source_provider_id), model),
        },
        match_targets: build_match_targets(spec.provider_id, pattern, &full_id),
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
            let display_name = format_display_name(provider_id, &model.name);
            let full_id = format_model_id(prefix, &model.id);
            let pricing = ModelPricingRecord {
                input: model.cost.input,
                output: model.cost.output,
                cache_read: model.cost.cache_read,
                cache_write: model.cost.cache_write,
            };
            let input_images = model
                .modalities
                .input
                .iter()
                .any(|modality| modality == "image");
            let capabilities = ModelCapabilitiesRecord {
                reasoning: model.reasoning,
                input_images,
                output_limit: model.limit.output,
                api: model_api_hint(provider_id, Some(source_provider_id), model),
            };

            let match_targets = build_match_targets(provider_id, &model.id, &full_id);

            ModelCandidate {
                full_id,
                display_name,
                pricing,
                context_limit: model.limit.context,
                capabilities,
                match_targets,
            }
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

    let npm = model
        .provider
        .as_ref()
        .and_then(|provider| provider.npm.as_deref());

    let api = match npm {
        Some("@ai-sdk/openai") => "openai-responses",
        Some("@ai-sdk/anthropic") => "anthropic-messages",
        Some("@ai-sdk/google") => "google-generative-ai",
        Some("@ai-sdk/openai-compatible") => "openai-completions",
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
                eprintln!(
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
    matches!(provider_id, "zen" | "apiyi")
}

/// Looks up a model in the embedded `default_models.toml` by ID.
/// Uses provider resolution to match models with different prefixes.
fn lookup_default_model(full_id: &str) -> Option<ModelRecord> {
    use zdx_core::providers::{ProviderKind, provider_kind_from_id, resolve_provider};

    let defaults: ModelsFile = toml::from_str(zdx_core::models::default_models_toml()).ok()?;
    let target = resolve_provider(full_id);
    let target_model_normalized = normalize_model_lookup_id(&target.model);
    let is_meta_target = matches!(target.kind, ProviderKind::Zen | ProviderKind::Apiyi);

    defaults.models.into_iter().find(|record| {
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

        // Meta-providers (zen/apiyi) can reuse defaults from underlying providers
        // when model IDs match (e.g., apiyi:gemini-2.5-flash -> gemini:gemini-2.5-flash).
        if is_meta_target {
            let resolved_record = resolve_provider(&record.id);
            let resolved_match =
                normalize_model_lookup_id(&resolved_record.model) == target_model_normalized;
            let raw_match = normalize_model_lookup_id(&record.id) == target_model_normalized;
            return resolved_match || raw_match;
        }

        false
    })
}

fn normalize_model_lookup_id(model_id: &str) -> String {
    let lower = model_id.trim().to_ascii_lowercase();
    let lower = lower
        .strip_suffix("-thinking")
        .or_else(|| lower.strip_suffix("-nothinking"))
        .unwrap_or(&lower);
    lower.to_string()
}

/// Creates a default `ModelCandidate` for a model ID not found in the API.
/// Looks up pricing and capabilities from embedded `default_models.toml` if available.
fn create_default_candidate(
    provider_id: &str,
    prefix: Option<&str>,
    model_id: &str,
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

fn wildcard_match(pattern: &str, text: &str) -> bool {
    if pattern == "*" {
        return true;
    }

    let p = pattern.as_bytes();
    let t = text.as_bytes();
    let mut p_idx = 0;
    let mut t_idx = 0;
    let mut star_idx = None;
    let mut match_idx = 0;

    while t_idx < t.len() {
        if p_idx < p.len() && p[p_idx] == t[t_idx] {
            p_idx += 1;
            t_idx += 1;
            continue;
        }

        if p_idx < p.len() && p[p_idx] == b'*' {
            star_idx = Some(p_idx);
            p_idx += 1;
            match_idx = t_idx;
            continue;
        }

        if let Some(star) = star_idx {
            p_idx = star + 1;
            match_idx += 1;
            t_idx = match_idx;
            continue;
        }

        return false;
    }

    while p_idx < p.len() && p[p_idx] == b'*' {
        p_idx += 1;
    }

    p_idx == p.len()
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
    fn test_lookup_default_model_mimo() {
        // Also test mimo which has similar structure
        let result = lookup_default_model("mimo:mimo-v2-flash");
        assert!(result.is_some(), "Should find mimo model in defaults");

        let model = result.unwrap();
        assert_eq!(model.provider, "mimo");
        assert!(!model.display_name.contains("custom"));
    }

    #[test]
    fn test_lookup_default_model_meta_provider_uses_underlying_defaults() {
        let result = lookup_default_model("apiyi:gemini-2.5-flash");
        assert!(result.is_some(), "Should find gemini model for apiyi");
    }

    #[test]
    fn test_lookup_default_model_meta_provider_normalizes_thinking_suffix() {
        let result = lookup_default_model("apiyi:gemini-3-pro-preview-thinking");
        assert!(
            result.is_some(),
            "Should map -thinking variant to base model default"
        );
    }

    #[test]
    fn test_provider_specs_includes_zen_and_apiyi() {
        let config = config::Config::default();
        let specs = provider_specs(&config);

        assert!(specs.iter().any(|s| {
            s.provider_id == "zen" && s.api_id == "opencode" && s.prefix == Some("zen")
        }));
        assert!(specs.iter().any(|s| {
            s.provider_id == "apiyi" && s.api_id == "apiyi" && s.prefix == Some("apiyi")
        }));
    }

    #[test]
    fn test_official_source_provider_ids_for_apiyi_models() {
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
    fn test_model_api_hint_for_meta_provider_uses_models_dev_npm() {
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

        assert_eq!(
            model_api_hint("zen", Some("opencode"), &model).as_deref(),
            Some("openai-responses")
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
            model_api_hint("apiyi", Some("google"), &model).as_deref(),
            Some("google-generative-ai")
        );
        assert_eq!(
            model_api_hint("apiyi", Some("moonshotai"), &model).as_deref(),
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
