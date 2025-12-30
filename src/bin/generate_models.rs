//! On-demand model registry generator.
//!
//! Fetches model metadata from models.dev and generates a deterministic Rust file
//! containing the model registry for the TUI model picker.
//!
//! Usage:
//!   cargo run --bin generate_models -- --provider anthropic
//!   cargo run --bin generate_models -- --check
//!
//! See docs/plans/plan_generate_models.md for the implementation plan.

use std::collections::BTreeMap;
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use clap::Parser;
use serde::Deserialize;

/// Default URL for the models.dev API.
const DEFAULT_API_URL: &str = "https://models.dev/api.json";

/// Default output path for the generated file.
const DEFAULT_OUTPUT_PATH: &str = "src/models_generated.rs";

/// CLI arguments for the model generator.
#[derive(Parser, Debug)]
#[command(name = "generate_models")]
#[command(about = "Generate model registry from models.dev API")]
struct Args {
    /// Provider to filter models (e.g., "anthropic").
    #[arg(short, long, default_value = "anthropic")]
    provider: String,

    /// Output file path for generated Rust code.
    #[arg(short, long, default_value = DEFAULT_OUTPUT_PATH)]
    out: PathBuf,

    /// URL for the models.dev API (can also be set via MODELS_DEV_URL env var).
    #[arg(long, env = "MODELS_DEV_URL", default_value = DEFAULT_API_URL)]
    url: String,

    /// Check mode: verify that the generated file is up-to-date without writing.
    #[arg(long)]
    check: bool,
}

/// Root structure of the models.dev API response.
/// We only parse the fields we need.
#[derive(Debug, Deserialize)]
struct ApiResponse {
    /// Provider entries keyed by provider ID.
    #[serde(flatten)]
    providers: BTreeMap<String, ProviderEntry>,
}

/// A provider entry from the API.
#[derive(Debug, Deserialize)]
struct ProviderEntry {
    /// Models offered by this provider.
    models: Option<BTreeMap<String, ModelEntry>>,
}

/// Cost information from the API (prices per million tokens).
#[derive(Debug, Deserialize, Default, Clone)]
struct CostEntry {
    /// Input tokens cost per million.
    #[serde(default)]
    input: f64,
    /// Output tokens cost per million.
    #[serde(default)]
    output: f64,
    /// Cache read cost per million tokens.
    #[serde(default)]
    cache_read: f64,
    /// Cache write cost per million tokens.
    #[serde(default)]
    cache_write: f64,
}

/// Limit information from the API.
#[derive(Debug, Deserialize, Default, Clone)]
struct LimitEntry {
    /// Context window size in tokens.
    #[serde(default)]
    context: u64,
}

/// A model entry from the API.
/// We only parse the fields we need for the picker.
#[derive(Debug, Deserialize)]
struct ModelEntry {
    /// Model ID (used in API calls).
    id: String,
    /// Display name for the model.
    name: String,
    /// Model family (e.g., "claude-sonnet", "claude-opus").
    #[serde(default)]
    family: String,
    /// Whether this model supports tool calling.
    #[serde(default)]
    tool_call: bool,
    /// Release date (YYYY-MM-DD format).
    #[serde(default)]
    release_date: Option<String>,
    /// Pricing information.
    #[serde(default)]
    cost: CostEntry,
    /// Token limits.
    #[serde(default)]
    limit: LimitEntry,
}

/// Pricing information for a model (prices per million tokens).
#[derive(Debug, Clone, PartialEq, PartialOrd)]
struct ModelPricing {
    /// Input tokens cost per million.
    input: f64,
    /// Output tokens cost per million.
    output: f64,
    /// Cache read cost per million tokens.
    cache_read: f64,
    /// Cache write cost per million tokens.
    cache_write: f64,
}

impl From<CostEntry> for ModelPricing {
    fn from(cost: CostEntry) -> Self {
        Self {
            input: cost.input,
            output: cost.output,
            cache_read: cost.cache_read,
            cache_write: cost.cache_write,
        }
    }
}

/// A processed model option for code generation.
#[derive(Debug, Clone, PartialEq, PartialOrd)]
struct ModelOption {
    /// Model family for sorting/grouping (e.g., "claude-sonnet").
    family: String,
    /// Display name for the picker.
    display_name: String,
    /// Model ID (sent to API).
    id: String,
    /// Pricing information.
    pricing: ModelPricing,
    /// Context window size in tokens.
    context_limit: u64,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();

    eprintln!("Fetching models from {}...", args.url);
    let models = fetch_and_filter_models(&args.url, &args.provider).await?;

    if models.is_empty() {
        bail!(
            "No tool-capable models found for provider '{}'",
            args.provider
        );
    }

    eprintln!(
        "Found {} models ({} families) for provider '{}'",
        models.len(),
        models.len(), // After dedup, count == families
        args.provider
    );

    let generated = generate_rust_code(&models);

    if args.check {
        check_up_to_date(&args.out, &generated)?;
    } else {
        write_output(&args.out, &generated)?;
    }

    Ok(())
}

/// Fetches models from the API and filters by provider and capabilities.
/// Returns one model per family, preferring "(latest)" variants.
async fn fetch_and_filter_models(url: &str, provider: &str) -> Result<Vec<ModelOption>> {
    let response = reqwest::get(url)
        .await
        .context("Failed to fetch models.dev API")?;

    if !response.status().is_success() {
        bail!(
            "API request failed with status {}: {}",
            response.status(),
            response.status().canonical_reason().unwrap_or("Unknown")
        );
    }

    let api_response: ApiResponse = response
        .json()
        .await
        .context("Failed to parse API response as JSON")?;

    let provider_entry = api_response
        .providers
        .get(provider)
        .context(format!("Provider '{}' not found in API response", provider))?;

    let models_map = provider_entry
        .models
        .as_ref()
        .context(format!("Provider '{}' has no models", provider))?;

    // Filter tool-capable models with a family
    let filtered: Vec<&ModelEntry> = models_map
        .values()
        .filter(|m| m.tool_call && !m.family.is_empty())
        .collect();

    // Group by family and pick the best model for each family
    let mut by_family: BTreeMap<&str, Vec<&ModelEntry>> = BTreeMap::new();
    for model in &filtered {
        by_family.entry(&model.family).or_default().push(model);
    }

    // For each family, prefer the "(latest)" model with the most recent release_date
    let mut models: Vec<ModelOption> = by_family
        .into_iter()
        .map(|(family, family_models)| {
            // Filter to "(latest)" models and pick the one with the most recent release_date
            let latest_models: Vec<_> = family_models
                .iter()
                .filter(|m| m.name.contains("(latest)"))
                .collect();

            let best = if latest_models.is_empty() {
                // No "(latest)" model, fall back to first alphabetically
                family_models
                    .first()
                    .expect("family should have at least one model")
            } else {
                // Pick the "(latest)" model with the most recent release_date
                latest_models
                    .into_iter()
                    .max_by(|a, b| {
                        a.release_date
                            .as_deref()
                            .unwrap_or("")
                            .cmp(b.release_date.as_deref().unwrap_or(""))
                    })
                    .expect("latest_models should not be empty")
            };

            ModelOption {
                family: family.to_string(),
                display_name: best.name.clone(),
                id: best.id.clone(),
                pricing: best.cost.clone().into(),
                context_limit: best.limit.context,
            }
        })
        .collect();

    // Sort by family for deterministic output
    models.sort_by(|a, b| a.family.cmp(&b.family));

    Ok(models)
}

/// Generates deterministic Rust code for the model registry.
fn generate_rust_code(models: &[ModelOption]) -> String {
    let mut output = String::new();

    // Header comment
    output.push_str("// @generated by generate_models\n");
    output.push_str("// DO NOT EDIT - regenerate with: cargo run --bin generate_models\n");
    output.push_str("//\n");
    output.push_str("// Source: https://models.dev/api.json\n");
    output.push_str("// Filter: provider=anthropic, tool_call=true, one per family\n");
    output.push('\n');

    // Pricing struct definition
    output.push_str("/// Pricing information for a model (prices per million tokens).\n");
    output.push_str("#[derive(Debug, Clone, Copy)]\n");
    output.push_str("pub struct ModelPricing {\n");
    output.push_str("    /// Input tokens cost per million\n");
    output.push_str("    pub input: f64,\n");
    output.push_str("    /// Output tokens cost per million\n");
    output.push_str("    pub output: f64,\n");
    output.push_str("    /// Cache read cost per million tokens\n");
    output.push_str("    pub cache_read: f64,\n");
    output.push_str("    /// Cache write cost per million tokens\n");
    output.push_str("    pub cache_write: f64,\n");
    output.push_str("}\n");
    output.push('\n');

    // Model struct definition
    output.push_str("/// Definition of an available model.\n");
    output.push_str("#[derive(Debug, Clone)]\n");
    output.push_str("pub struct ModelOption {\n");
    output.push_str("    /// Model ID (sent to API)\n");
    output.push_str("    pub id: &'static str,\n");
    output.push_str("    /// Display name for the picker\n");
    output.push_str("    pub display_name: &'static str,\n");
    output.push_str("    /// Pricing information\n");
    output.push_str("    pub pricing: ModelPricing,\n");
    output.push_str("    /// Context window size in tokens\n");
    output.push_str("    pub context_limit: u64,\n");
    output.push_str("}\n");
    output.push('\n');

    // Model array
    output.push_str("/// Available models for the picker (one per family).\n");
    output.push_str("pub const AVAILABLE_MODELS: &[ModelOption] = &[\n");

    for model in models {
        output.push_str("    ModelOption {\n");
        output.push_str(&format!("        id: {:?},\n", model.id));
        output.push_str(&format!(
            "        display_name: {:?},\n",
            model.display_name
        ));
        output.push_str("        pricing: ModelPricing {\n");
        output.push_str(&format!(
            "            input: {},\n",
            format_f64(model.pricing.input)
        ));
        output.push_str(&format!(
            "            output: {},\n",
            format_f64(model.pricing.output)
        ));
        output.push_str(&format!(
            "            cache_read: {},\n",
            format_f64(model.pricing.cache_read)
        ));
        output.push_str(&format!(
            "            cache_write: {},\n",
            format_f64(model.pricing.cache_write)
        ));
        output.push_str("        },\n");
        output.push_str(&format!(
            "        context_limit: {},\n",
            model.context_limit
        ));
        output.push_str("    },\n");
    }

    output.push_str("];\n");

    output
}

/// Formats an f64 for Rust code generation.
/// Ensures the value is formatted as a float literal (with decimal point).
fn format_f64(value: f64) -> String {
    let s = value.to_string();
    if s.contains('.') {
        s
    } else {
        format!("{}.0", s)
    }
}

/// Writes the generated code to the output file.
fn write_output(path: &PathBuf, content: &str) -> Result<()> {
    std::fs::write(path, content)
        .context(format!("Failed to write output file: {}", path.display()))?;

    eprintln!("Wrote {}", path.display());
    Ok(())
}

/// Checks if the generated file is up-to-date.
fn check_up_to_date(path: &PathBuf, expected: &str) -> Result<()> {
    let existing = std::fs::read_to_string(path)
        .context(format!("Failed to read existing file: {}", path.display()))?;

    if existing == *expected {
        eprintln!("✓ {} is up to date", path.display());
        Ok(())
    } else {
        bail!(
            "{} is out of date. Run `cargo run --bin generate_models` to update.",
            path.display()
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_pricing() -> ModelPricing {
        ModelPricing {
            input: 3.0,
            output: 15.0,
            cache_read: 0.3,
            cache_write: 3.75,
        }
    }

    #[test]
    fn test_generate_rust_code_deterministic() {
        let models = vec![
            ModelOption {
                family: "claude-sonnet".to_string(),
                id: "claude-sonnet-4-5".to_string(),
                display_name: "Claude Sonnet 4.5 (latest)".to_string(),
                pricing: test_pricing(),
                context_limit: 200000,
            },
            ModelOption {
                family: "claude-haiku".to_string(),
                id: "claude-haiku-4-5".to_string(),
                display_name: "Claude Haiku 4.5 (latest)".to_string(),
                pricing: test_pricing(),
                context_limit: 200000,
            },
        ];

        let output1 = generate_rust_code(&models);
        let output2 = generate_rust_code(&models);

        assert_eq!(output1, output2, "Output should be deterministic");
        assert!(output1.contains("claude-sonnet-4-5"));
        assert!(output1.contains("Claude Sonnet 4.5 (latest)"));
        assert!(output1.contains("claude-sonnet"));
        assert!(output1.contains("context_limit: 200000"));
    }

    #[test]
    fn test_model_option_sorting() {
        let mut models = vec![
            ModelOption {
                family: "z-family".to_string(),
                id: "z-model".to_string(),
                display_name: "Zeta Model".to_string(),
                pricing: test_pricing(),
                context_limit: 100000,
            },
            ModelOption {
                family: "a-family".to_string(),
                id: "a-model".to_string(),
                display_name: "Alpha Model".to_string(),
                pricing: test_pricing(),
                context_limit: 100000,
            },
        ];

        models.sort_by(|a, b| a.family.cmp(&b.family));

        // Should sort by family first
        assert_eq!(models[0].family, "a-family");
        assert_eq!(models[1].family, "z-family");
    }

    #[test]
    fn test_generate_includes_pricing_fields() {
        let models = vec![ModelOption {
            family: "claude-opus".to_string(),
            id: "claude-opus-4-5".to_string(),
            display_name: "Claude Opus 4.5 (latest)".to_string(),
            pricing: ModelPricing {
                input: 15.0,
                output: 75.0,
                cache_read: 1.5,
                cache_write: 18.75,
            },
            context_limit: 200000,
        }];

        let output = generate_rust_code(&models);

        assert!(output.contains("pub struct ModelPricing"));
        assert!(output.contains("input: 15.0"));
        assert!(output.contains("output: 75.0"));
        assert!(output.contains("cache_read: 1.5"));
        assert!(output.contains("cache_write: 18.75"));
    }

    #[test]
    fn test_format_f64_ensures_decimal() {
        assert_eq!(format_f64(3.0), "3.0");
        assert_eq!(format_f64(3.5), "3.5");
        assert_eq!(format_f64(0.3), "0.3");
        // Integer-like values should still have .0
        assert_eq!(format_f64(15.0), "15.0");
    }
}
