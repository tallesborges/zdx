//! Models command handlers.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::Path;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::config;

const MODELS_DEV_URL: &str = "https://models.dev/api.json";

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
}

#[derive(Debug, Deserialize)]
struct ModelEntry {
    id: String,
    name: String,
    #[serde(default)]
    tool_call: bool,
    #[serde(default)]
    cost: CostEntry,
    #[serde(default)]
    limit: LimitEntry,
}

#[derive(Debug, Clone)]
struct ModelCandidate {
    full_id: String,
    display_name: String,
    pricing: ModelPricingRecord,
    context_limit: u64,
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
}

#[derive(Debug, Serialize, Deserialize, Clone)]
struct ModelPricingRecord {
    input: f64,
    output: f64,
    cache_read: f64,
    cache_write: f64,
}

pub async fn update(config: &config::Config) -> Result<()> {
    let url = std::env::var("MODELS_DEV_URL").unwrap_or_else(|_| MODELS_DEV_URL.to_string());
    let api = fetch_api(&url).await?;

    let providers = [
        ("anthropic", "anthropic", None, &config.providers.anthropic),
        ("openai", "openai", Some("openai"), &config.providers.openai),
        (
            "openrouter",
            "openrouter",
            Some("openrouter"),
            &config.providers.openrouter,
        ),
        ("gemini", "google", Some("gemini"), &config.providers.gemini),
    ];

    let mut records = Vec::new();
    let mut seen_keys = HashSet::new();

    for (provider_id, api_id, prefix, provider_cfg) in providers {
        if !provider_cfg.enabled.unwrap_or(true) {
            continue;
        }

        if provider_cfg.models.is_empty() {
            eprintln!(
                "Warning: providers.{}.models is empty; skipping.",
                provider_id
            );
            continue;
        }

        let Some(provider_entry) = api.providers.get(api_id) else {
            bail!("Provider '{}' not found in models.dev response", api_id);
        };

        let Some(models_map) = provider_entry.models.as_ref() else {
            bail!("Provider '{}' has no models in models.dev response", api_id);
        };

        let candidates = build_candidates(provider_id, prefix, models_map);
        let selected = select_candidates(provider_id, &provider_cfg.models, &candidates);

        if selected.is_empty() {
            eprintln!(
                "Warning: no models matched providers.{}.models",
                provider_id
            );
            continue;
        }

        for candidate in selected {
            let record = ModelRecord {
                id: candidate.full_id,
                provider: provider_id.to_string(),
                display_name: candidate.display_name,
                context_limit: candidate.context_limit,
                pricing: candidate.pricing,
            };
            let key = record_key(&record);
            if !seen_keys.insert(key) {
                continue;
            }
            records.push(record);
        }
    }

    append_codex_records(config, &mut records, &mut seen_keys)?;

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

            let match_targets = build_match_targets(provider_id, &model.id, &full_id);

            ModelCandidate {
                full_id,
                display_name,
                pricing,
                context_limit: model.limit.context,
                match_targets,
            }
        })
        .collect()
}

fn select_candidates(
    provider_id: &str,
    patterns: &[String],
    candidates: &[ModelCandidate],
) -> Vec<ModelCandidate> {
    let mut selected = Vec::new();

    for pattern in patterns {
        let pattern = pattern.trim();
        if pattern.is_empty() {
            continue;
        }

        let matches: Vec<&ModelCandidate> = candidates
            .iter()
            .filter(|candidate| matches_pattern(pattern, candidate))
            .collect();

        if matches.is_empty() {
            eprintln!(
                "Warning: pattern '{}' for provider '{}' matched no models",
                pattern, provider_id
            );
            continue;
        }

        selected.extend(matches.into_iter().cloned());
    }

    selected
}

fn build_match_targets(provider_id: &str, raw_id: &str, full_id: &str) -> Vec<String> {
    let mut targets = vec![full_id.to_string(), raw_id.to_string()];
    targets.push(format!("{}:{}", provider_id, raw_id));
    targets.push(format!("{}/{}", provider_id, raw_id));

    if provider_id == "anthropic" {
        targets.push(format!("claude:{}", raw_id));
        targets.push(format!("claude/{}", raw_id));
    }

    if provider_id == "gemini" {
        targets.push(format!("google:{}", raw_id));
        targets.push(format!("google/{}", raw_id));
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
    if provider_id == "anthropic" {
        name.replace(" (latest)", "")
    } else {
        name.to_string()
    }
}

fn format_model_id(prefix: Option<&str>, raw_id: &str) -> String {
    match prefix {
        Some(prefix) => format!("{}:{}", prefix, raw_id),
        None => raw_id.to_string(),
    }
}

fn append_codex_records(
    config: &config::Config,
    records: &mut Vec<ModelRecord>,
    seen_keys: &mut HashSet<String>,
) -> Result<()> {
    let cfg = &config.providers.openai_codex;
    if !cfg.enabled.unwrap_or(true) {
        return Ok(());
    }

    if cfg.models.is_empty() {
        eprintln!("Warning: providers.openai_codex.models is empty; skipping.");
        return Ok(());
    }

    let catalog = load_codex_catalog(&config.models_path())?;
    let selected = select_manual_records("openai-codex", &cfg.models, &catalog);

    if selected.is_empty() {
        eprintln!("Warning: no models matched providers.openai_codex.models");
        return Ok(());
    }

    for record in selected {
        let key = record_key(&record);
        if !seen_keys.insert(key) {
            continue;
        }
        records.push(record);
    }

    Ok(())
}

fn load_codex_catalog(path: &Path) -> Result<Vec<ModelRecord>> {
    let mut by_id = HashMap::new();

    let defaults: ModelsFile = toml::from_str(crate::models::default_models_toml())
        .context("Failed to parse default models registry")?;
    for record in defaults.models {
        if record.provider == "openai-codex" {
            by_id.insert(record.id.clone(), record);
        }
    }

    if let Ok(contents) = fs::read_to_string(path) {
        let existing: ModelsFile = toml::from_str(&contents)
            .with_context(|| format!("Failed to parse models file at {}", path.display()))?;
        for record in existing.models {
            if record.provider == "openai-codex" {
                by_id.insert(record.id.clone(), record);
            }
        }
    }

    Ok(by_id.into_values().collect())
}

fn select_manual_records(
    provider_id: &str,
    patterns: &[String],
    catalog: &[ModelRecord],
) -> Vec<ModelRecord> {
    let mut selected = Vec::new();

    for pattern in patterns {
        let pattern = pattern.trim();
        if pattern.is_empty() {
            continue;
        }

        let matches: Vec<&ModelRecord> = catalog
            .iter()
            .filter(|record| matches_manual_pattern(provider_id, pattern, record))
            .collect();

        if matches.is_empty() {
            eprintln!(
                "Warning: pattern '{}' for provider '{}' matched no models",
                pattern, provider_id
            );
            continue;
        }

        selected.extend(matches.into_iter().cloned());
    }

    selected
}

fn matches_manual_pattern(provider_id: &str, pattern: &str, record: &ModelRecord) -> bool {
    build_manual_targets(provider_id, record)
        .iter()
        .any(|target| wildcard_match(pattern, target))
}

fn build_manual_targets(provider_id: &str, record: &ModelRecord) -> Vec<String> {
    let mut targets = vec![record.id.clone()];
    targets.push(format!("{}:{}", provider_id, record.id));
    targets.push(format!("{}/{}", provider_id, record.id));

    if provider_id == "openai-codex" {
        targets.push(format!("codex:{}", record.id));
        targets.push(format!("codex/{}", record.id));
    }

    targets
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

    let mut content = format!("{}{}", header, body);
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
