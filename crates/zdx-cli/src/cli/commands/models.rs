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
}

pub async fn update(config: &config::Config) -> Result<()> {
    let url = std::env::var("MODELS_DEV_URL").unwrap_or_else(|_| MODELS_DEV_URL.to_string());
    let api = fetch_api(&url).await?;

    let providers = [
        ("anthropic", "anthropic", None, &config.providers.anthropic),
        (
            "claude-cli",
            "anthropic",
            Some("claude-cli"),
            &config.providers.claude_cli,
        ),
        ("openai", "openai", Some("openai"), &config.providers.openai),
        (
            "openai-codex",
            "openai",
            None,
            &config.providers.openai_codex,
        ),
        (
            "openrouter",
            "openrouter",
            Some("openrouter"),
            &config.providers.openrouter,
        ),
        (
            "moonshot",
            "moonshotai",
            Some("moonshot"),
            &config.providers.moonshot,
        ),
        (
            "stepfun",
            "stepfun",
            Some("stepfun"),
            &config.providers.stepfun,
        ),
        ("mimo", "xiaomi", Some("mimo"), &config.providers.mimo),
        ("gemini", "google", Some("gemini"), &config.providers.gemini),
        (
            "gemini-cli",
            "google",
            Some("gemini-cli"),
            &config.providers.gemini_cli,
        ),
    ];

    let mut records = Vec::new();
    let mut seen_keys = HashSet::new();

    for (provider_id, api_id, prefix, provider_cfg) in providers {
        // Include all providers in the registry regardless of enabled status.
        // The registry is used for model lookups (e.g., checking reasoning support)
        // which should work even if the provider isn't currently enabled.
        if provider_cfg.models.is_empty() {
            eprintln!("Warning: providers.{provider_id}.models is empty; skipping.");
            continue;
        }

        let Some(provider_entry) = api.providers.get(api_id) else {
            eprintln!(
                "Warning: provider '{api_id}' not found in models.dev response; falling back to defaults"
            );
            let mut fallback = Vec::new();
            for pattern in &provider_cfg.models {
                let pattern = pattern.trim();
                if pattern.is_empty() {
                    continue;
                }
                if is_pure_wildcard(pattern) {
                    eprintln!(
                        "Warning: wildcard pattern '{pattern}' for provider '{provider_id}' requires models.dev data"
                    );
                    continue;
                }
                fallback.push(create_default_candidate(provider_id, prefix, pattern));
            }

            if fallback.is_empty() {
                continue;
            }

            for candidate in fallback {
                let record = ModelRecord {
                    id: candidate.full_id,
                    provider: provider_id.to_string(),
                    display_name: candidate.display_name,
                    context_limit: candidate.context_limit,
                    pricing: candidate.pricing,
                    capabilities: candidate.capabilities,
                };
                let key = record_key(&record);
                if !seen_keys.insert(key) {
                    continue;
                }
                records.push(record);
            }
            continue;
        };

        let Some(models_map) = provider_entry.models.as_ref() else {
            bail!("Provider '{api_id}' has no models in models.dev response");
        };

        let candidates = build_candidates(provider_id, prefix, models_map);
        let select_result = select_candidates(provider_id, &provider_cfg.models, &candidates);

        // Create default candidates for unmatched patterns
        let mut all_selected: Vec<ModelCandidate> = select_result.matched;
        for pattern in &select_result.unmatched_patterns {
            let default_candidate = create_default_candidate(provider_id, prefix, pattern);
            eprintln!(
                "Info: creating default entry for '{}' (not found in models.dev)",
                default_candidate.full_id
            );
            all_selected.push(default_candidate);
        }

        if all_selected.is_empty() {
            eprintln!("Warning: no models matched providers.{provider_id}.models");
            continue;
        }

        for candidate in all_selected {
            let record = ModelRecord {
                id: candidate.full_id,
                provider: provider_id.to_string(),
                display_name: candidate.display_name,
                context_limit: candidate.context_limit,
                pricing: candidate.pricing,
                capabilities: candidate.capabilities,
            };
            let key = record_key(&record);
            if !seen_keys.insert(key) {
                continue;
            }
            records.push(record);
        }
    }

    if records.is_empty() {
        bail!("No models matched configured providers/models.");
    }

    let out_path = config.models_path();
    write_models_file(&out_path, &records)?;
    println!("Updated models at {}", out_path.display());
    Ok(())
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

/// Looks up a model in the embedded `default_models.toml` by ID.
/// Uses provider resolution to match models with different prefixes.
fn lookup_default_model(full_id: &str) -> Option<ModelRecord> {
    use zdx_core::providers::{provider_kind_from_id, resolve_provider};

    let defaults: ModelsFile = toml::from_str(zdx_core::models::default_models_toml()).ok()?;
    let target = resolve_provider(full_id);

    defaults.models.into_iter().find(|record| {
        // Try exact match first
        if record.id == full_id {
            return true;
        }
        // Fall back to provider-based match using record.provider field
        // This handles cases where record.id has no prefix (e.g., "step-3.5-flash")
        // but record.provider specifies the correct provider (e.g., "stepfun")
        if let Some(candidate_kind) = provider_kind_from_id(&record.provider) {
            return candidate_kind == target.kind && record.id == target.model;
        }
        false
    })
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

    ModelCandidate {
        full_id,
        display_name,
        context_limit: DEFAULT_CONTEXT_LIMIT,
        match_targets,
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
}
