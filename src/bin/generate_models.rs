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
    /// Provider ID (redundant with key, but included for completeness).
    #[allow(dead_code)]
    id: Option<String>,
    /// Models offered by this provider.
    models: Option<BTreeMap<String, ModelEntry>>,
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
}

/// A processed model option for code generation.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct ModelOption {
    /// Model family for sorting/grouping (e.g., "claude-sonnet").
    family: String,
    /// Display name for the picker.
    display_name: String,
    /// Model ID (sent to API).
    id: String,
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
            }
        })
        .collect();

    // Sort by family for deterministic output
    models.sort();

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

    // Model struct definition
    output.push_str("/// Definition of an available model.\n");
    output.push_str("#[derive(Debug, Clone)]\n");
    output.push_str("#[allow(dead_code)] // family is reserved for future use\n");
    output.push_str("pub struct ModelOption {\n");
    output.push_str("    /// Model ID (sent to API)\n");
    output.push_str("    pub id: &'static str,\n");
    output.push_str("    /// Display name for the picker\n");
    output.push_str("    pub display_name: &'static str,\n");
    output.push_str("    /// Model family (e.g., \"claude-sonnet\")\n");
    output.push_str("    pub family: &'static str,\n");
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
        output.push_str(&format!("        family: {:?},\n", model.family));
        output.push_str("    },\n");
    }

    output.push_str("];\n");

    output
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

    #[test]
    fn test_generate_rust_code_deterministic() {
        let models = vec![
            ModelOption {
                family: "claude-sonnet".to_string(),
                id: "claude-sonnet-4-5".to_string(),
                display_name: "Claude Sonnet 4.5 (latest)".to_string(),
            },
            ModelOption {
                family: "claude-haiku".to_string(),
                id: "claude-haiku-4-5".to_string(),
                display_name: "Claude Haiku 4.5 (latest)".to_string(),
            },
        ];

        let output1 = generate_rust_code(&models);
        let output2 = generate_rust_code(&models);

        assert_eq!(output1, output2, "Output should be deterministic");
        assert!(output1.contains("claude-sonnet-4-5"));
        assert!(output1.contains("Claude Sonnet 4.5 (latest)"));
        assert!(output1.contains("claude-sonnet"));
    }

    #[test]
    fn test_model_option_sorting() {
        let mut models = [
            ModelOption {
                family: "z-family".to_string(),
                id: "z-model".to_string(),
                display_name: "Zeta Model".to_string(),
            },
            ModelOption {
                family: "a-family".to_string(),
                id: "a-model".to_string(),
                display_name: "Alpha Model".to_string(),
            },
        ];

        models.sort();

        // Should sort by family first
        assert_eq!(models[0].family, "a-family");
        assert_eq!(models[1].family, "z-family");
    }

    #[test]
    fn test_generate_includes_family_field() {
        let models = vec![ModelOption {
            family: "claude-opus".to_string(),
            id: "claude-opus-4-5".to_string(),
            display_name: "Claude Opus 4.5 (latest)".to_string(),
        }];

        let output = generate_rust_code(&models);

        assert!(output.contains("pub family: &'static str"));
        assert!(output.contains("family: \"claude-opus\""));
    }
}
