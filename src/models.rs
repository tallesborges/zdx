//! Model registry for the TUI model picker.
//!
//! Loads models from `<base>/models.toml` when present, otherwise falls back to
//! `default_models.toml`.

use std::collections::HashSet;
use std::fs;
use std::path::Path;
use std::sync::OnceLock;

use serde::Deserialize;

const DEFAULT_MODELS_TOML: &str = include_str!("../default_models.toml");

pub fn default_models_toml() -> &'static str {
    DEFAULT_MODELS_TOML
}

/// Pricing information for a model (prices per million tokens).
#[derive(Debug, Clone, Copy)]
pub struct ModelPricing {
    /// Input tokens cost per million
    pub input: f64,
    /// Output tokens cost per million
    pub output: f64,
    /// Cache read cost per million tokens
    pub cache_read: f64,
    /// Cache write cost per million tokens
    pub cache_write: f64,
}

/// Capability metadata for a model.
#[derive(Debug, Clone, Copy, Default)]
pub struct ModelCapabilities {
    /// Whether the model supports reasoning mode.
    pub reasoning: bool,
    /// Whether the model supports image inputs.
    pub input_images: bool,
    /// Maximum output tokens supported by the model.
    pub output_limit: u64,
}

/// Definition of an available model.
#[derive(Debug, Clone)]
pub struct ModelOption {
    /// Model ID (sent to API)
    pub id: &'static str,
    /// Provider identifier
    pub provider: &'static str,
    /// Display name for the picker
    pub display_name: &'static str,
    /// Pricing information
    pub pricing: ModelPricing,
    /// Context window size in tokens
    pub context_limit: u64,
    /// Capability metadata from the registry
    pub capabilities: ModelCapabilities,
}

static ALL_MODELS: OnceLock<Vec<ModelOption>> = OnceLock::new();

/// Returns all available models (config + fallback).
pub fn available_models() -> &'static [ModelOption] {
    ALL_MODELS
        .get_or_init(|| {
            let mut models =
                load_models_from_path(&crate::config::paths::zdx_home().join("models.toml"))
                    .or_else(|| load_models_from_str(DEFAULT_MODELS_TOML))
                    .unwrap_or_default();

            let mut seen = HashSet::new();
            let mut combined = Vec::new();

            for model in models.drain(..) {
                if seen.insert(model.id) {
                    combined.push(model);
                }
            }
            combined
        })
        .as_slice()
}

impl ModelOption {
    /// Finds a model by its ID.
    pub fn find_by_id(id: &str) -> Option<&'static ModelOption> {
        if let Some(model) = available_models().iter().find(|m| m.id == id) {
            return Some(model);
        }

        let target = crate::providers::resolve_provider(id);
        available_models().iter().find(|m| {
            let candidate = crate::providers::resolve_provider(m.id);
            candidate.kind == target.kind && candidate.model == target.model
        })
    }
}

/// Returns true if the model supports reasoning, defaulting to true when unknown.
pub fn model_supports_reasoning(id: &str) -> bool {
    ModelOption::find_by_id(id)
        .map(|model| model.capabilities.reasoning)
        .unwrap_or(true)
}

#[derive(Debug, Deserialize)]
struct ModelsFile {
    #[serde(rename = "model")]
    models: Vec<ModelRecord>,
}

#[derive(Debug, Deserialize)]
struct ModelRecord {
    id: String,
    #[serde(default)]
    provider: Option<String>,
    #[serde(default)]
    display_name: Option<String>,
    #[serde(default)]
    context_limit: Option<u64>,
    #[serde(default)]
    pricing: Option<ModelPricingRecord>,
    #[serde(default)]
    capabilities: Option<ModelCapabilitiesRecord>,
}

#[derive(Debug, Deserialize, Default)]
struct ModelPricingRecord {
    #[serde(default)]
    input: f64,
    #[serde(default)]
    output: f64,
    #[serde(default)]
    cache_read: f64,
    #[serde(default)]
    cache_write: f64,
}

#[derive(Debug, Deserialize, Default)]
struct ModelCapabilitiesRecord {
    #[serde(default)]
    reasoning: bool,
    #[serde(default)]
    input_images: bool,
    #[serde(default)]
    output_limit: u64,
}

fn load_models_from_path(path: &Path) -> Option<Vec<ModelOption>> {
    let contents = fs::read_to_string(path).ok()?;
    load_models_from_str(&contents)
}

fn load_models_from_str(contents: &str) -> Option<Vec<ModelOption>> {
    let file: ModelsFile = match toml::from_str(contents) {
        Ok(file) => file,
        Err(err) => {
            eprintln!("Warning: failed to parse models file: {}", err);
            return None;
        }
    };

    let models = file
        .models
        .into_iter()
        .filter_map(model_record_to_option)
        .collect();

    Some(models)
}

fn model_record_to_option(record: ModelRecord) -> Option<ModelOption> {
    let id = record.id.trim().to_string();
    if id.is_empty() {
        return None;
    }

    let provider = record
        .provider
        .unwrap_or_else(|| {
            provider_id_for_kind(crate::providers::resolve_provider(&id).kind).to_string()
        })
        .trim()
        .to_lowercase();

    let display_name = record
        .display_name
        .unwrap_or_else(|| id.clone())
        .trim()
        .to_string();

    let context_limit = record.context_limit.unwrap_or(0);
    let pricing = record.pricing.unwrap_or_default();
    let capabilities = record.capabilities.unwrap_or_default();

    Some(ModelOption {
        id: leak_string(id),
        provider: leak_string(provider),
        display_name: leak_string(display_name),
        pricing: ModelPricing {
            input: pricing.input,
            output: pricing.output,
            cache_read: pricing.cache_read,
            cache_write: pricing.cache_write,
        },
        context_limit,
        capabilities: ModelCapabilities {
            reasoning: capabilities.reasoning,
            input_images: capabilities.input_images,
            output_limit: capabilities.output_limit,
        },
    })
}

fn leak_string(value: String) -> &'static str {
    Box::leak(value.into_boxed_str())
}

fn provider_id_for_kind(kind: crate::providers::ProviderKind) -> &'static str {
    match kind {
        crate::providers::ProviderKind::Anthropic => "anthropic",
        crate::providers::ProviderKind::ClaudeCli => "claude-cli",
        crate::providers::ProviderKind::OpenAICodex => "openai-codex",
        crate::providers::ProviderKind::OpenAI => "openai",
        crate::providers::ProviderKind::OpenRouter => "openrouter",
        crate::providers::ProviderKind::Gemini => "gemini",
        crate::providers::ProviderKind::GeminiCli => "gemini-cli",
    }
}
