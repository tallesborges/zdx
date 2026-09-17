//! Read-only model registry listing tool.
//!
//! Answers "which model ids can I pass here, and what are the configured
//! tiers" — the registry plus configured custom-provider models and
//! `[[model_modes]]` — for agents that have no shell (the orchestrator
//! profile), which otherwise have to guess an id.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::{ToolContext, ToolDefinition};
use crate::config::Config;
use crate::core::events::ToolOutput;
use crate::models::{ModelOption, available_models, custom_provider_models};
use crate::providers::ProviderKind;

/// Upper bound on listed models, so a registry with hundreds of entries cannot
/// flood the context; the caller narrows with `provider` instead.
const MAX_MODELS: usize = 200;

/// Returns the tool definition for the `list_models` tool.
pub fn definition() -> ToolDefinition {
    ToolDefinition {
        name: "List_Models".to_string(),
        description: "Lists the configured model modes and the model ids this machine can run, read-only. `modes` carries each `[[model_modes]]` tier: name, description, and the primary and alternative model specs backing it. `models` carries one entry per runnable model: the exact `provider:model` id to pass as a model spec, its display name, and whether the provider is subscription-backed (no per-token cost). The model list covers enabled providers plus models declared under `[providers.custom.<name>]`; pass `provider` to narrow it. Use this instead of guessing an id — `zdx models list` is the shell equivalent for the model list.".to_string(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "provider": {
                    "type": "string",
                    "description": "Only list this provider's models: matches the provider id (e.g. `claude-cli`, `parity`) or its account-qualified form (e.g. `claude-cli@parity`), case-insensitively."
                }
            },
            "additionalProperties": false
        }),
    }
}

#[derive(Debug, Deserialize)]
struct ListModelsInput {
    #[serde(default)]
    provider: Option<String>,
}

/// One `[[model_modes]]` entry, as configured.
#[derive(Debug, Serialize)]
struct ModeEntry {
    name: String,
    description: String,
    primary: String,
    alternatives: Vec<String>,
}

#[derive(Debug, Serialize)]
struct ModelEntry {
    id: String,
    display_name: String,
    subscription: bool,
}

/// Executes the list-models tool and returns the configured modes and the
/// matching registry entries.
pub fn execute(input: &Value, ctx: &ToolContext) -> ToolOutput {
    let input: ListModelsInput = match serde_json::from_value(input.clone()) {
        Ok(input) => input,
        Err(e) => {
            return ToolOutput::failure(
                "invalid_input",
                "Invalid input for list_models tool",
                Some(format!("Parse error: {e}")),
            );
        }
    };

    let config = ctx.config.clone().unwrap_or_default();
    let filter = input
        .provider
        .as_deref()
        .map(str::trim)
        .filter(|provider| !provider.is_empty())
        .map(str::to_lowercase);

    let modes = collect_modes(&config);
    let entries = collect_models(&config, filter.as_deref());
    let total = entries.len();
    let truncated = total > MAX_MODELS;
    let listed: Vec<&ModelEntry> = entries.iter().take(MAX_MODELS).collect();

    let mut data = json!({
        "modes": modes,
        "models": listed,
        "total": total,
        "truncated": truncated,
    });
    if truncated {
        data["note"] = json!(format!(
            "{total} models matched; the first {MAX_MODELS} are listed. Pass `provider` to narrow the list."
        ));
    }
    ToolOutput::success(data)
}

/// The configured `[[model_modes]]`, in config order (the order every surface
/// cycles them in).
fn collect_modes(config: &Config) -> Vec<ModeEntry> {
    config
        .model_modes
        .iter()
        .map(|mode| ModeEntry {
            name: mode.name.clone(),
            description: mode.description.clone(),
            primary: mode.primary.clone(),
            alternatives: mode.alternatives.clone(),
        })
        .collect()
}

/// Enabled-provider registry entries plus custom-provider models, sorted as
/// `zdx models list` prints them. `filter` matches the provider id or its
/// account-qualified form.
fn collect_models(config: &Config, filter: Option<&str>) -> Vec<ModelEntry> {
    let mut models: Vec<&ModelOption> = available_models().iter().collect();
    models.extend(custom_provider_models(&config.providers));
    models.retain(|model| config.providers.is_enabled(model.provider));
    if let Some(filter) = filter {
        models.retain(|model| {
            model.provider.eq_ignore_ascii_case(filter)
                || crate::providers::oauth::account_cache_key(model.provider, model.account)
                    .eq_ignore_ascii_case(filter)
        });
    }
    entries_from(models)
}

/// Sorts, de-duplicates, and maps models to their listing entries. Identity is
/// `(provider, account, id)`: an account-qualified variant (`claude-cli@work:…`)
/// is a distinct dispatch target from the default account's entry and must not
/// be collapsed into it.
fn entries_from(mut models: Vec<&ModelOption>) -> Vec<ModelEntry> {
    models.sort_by(|a, b| (a.provider, a.account, a.id).cmp(&(b.provider, b.account, b.id)));
    models.dedup_by(|a, b| a.provider == b.provider && a.account == b.account && a.id == b.id);

    models
        .into_iter()
        .map(|model| ModelEntry {
            id: model.qualified_id(),
            display_name: model.display_name.to_string(),
            subscription: ProviderKind::from_id(model.provider)
                .is_some_and(ProviderKind::is_subscription),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::config::CustomProviderConfig;

    fn config_with_custom_models(models: &[&str]) -> Config {
        let mut config = Config::default();
        config.providers.custom.insert(
            "mock".to_string(),
            CustomProviderConfig {
                base_url: "https://example.test/v1".to_string(),
                api_key: Some("test".to_string()),
                models: models.iter().map(|model| (*model).to_string()).collect(),
                ..Default::default()
            },
        );
        config
    }

    fn context(config: &Config) -> ToolContext {
        ToolContext::new(PathBuf::from("."), None).with_config(config)
    }

    fn ids(output: &ToolOutput) -> Vec<String> {
        assert!(output.is_ok(), "expected success: {output:?}");
        output.data().unwrap()["models"]
            .as_array()
            .unwrap()
            .iter()
            .map(|model| model["id"].as_str().unwrap().to_string())
            .collect()
    }

    #[test]
    fn definition_names_the_registry_tool() {
        let def = definition();
        assert_eq!(def.name, "List_Models");
        assert!(def.description.contains("zdx models list"));
        assert!(def.description.contains("modes"));
    }

    #[test]
    fn modes_are_listed_with_primaries_and_alternatives_in_config_order() {
        let _home = crate::test_support::temp_zdx_home();
        let mut config = Config::default();
        config.model_modes = vec![
            crate::config::ModelMode {
                name: "smart".to_string(),
                description: "deep reasoning".to_string(),
                primary: "claude-cli:claude-opus-5@high".to_string(),
                alternatives: vec!["openai-codex:gpt-5.6-sol@xhigh".to_string()],
                thinking: crate::config::ThinkingLevel::High,
            },
            crate::config::ModelMode {
                name: "fast".to_string(),
                description: String::new(),
                primary: "gemini:flash@low".to_string(),
                alternatives: Vec::new(),
                thinking: crate::config::ThinkingLevel::Low,
            },
        ];

        let output = execute(&json!({}), &context(&config));
        let modes = output.data().unwrap()["modes"].as_array().unwrap().clone();

        assert_eq!(modes.len(), 2);
        assert_eq!(modes[0]["name"], json!("smart"));
        assert_eq!(modes[0]["description"], json!("deep reasoning"));
        assert_eq!(modes[0]["primary"], json!("claude-cli:claude-opus-5@high"));
        assert_eq!(
            modes[0]["alternatives"],
            json!(["openai-codex:gpt-5.6-sol@xhigh"])
        );
        assert_eq!(modes[1]["name"], json!("fast"));
        assert_eq!(modes[1]["alternatives"], json!([]));
    }

    #[test]
    fn no_configured_modes_list_an_empty_array() {
        let _home = crate::test_support::temp_zdx_home();

        let output = execute(&json!({}), &context(&Config::default()));

        assert_eq!(output.data().unwrap()["modes"], json!([]));
    }

    fn model_option(
        provider: &'static str,
        id: &'static str,
        account: Option<&'static str>,
    ) -> ModelOption {
        ModelOption {
            id,
            provider,
            account,
            display_name: id,
            pricing: crate::models::ModelPricing {
                input: 0.0,
                output: 0.0,
                cache_read: 0.0,
                cache_write: 0.0,
            },
            context_limit: 0,
            capabilities: crate::models::ModelCapabilities::default(),
        }
    }

    /// An account-qualified entry (`provider@account:id`) is a distinct
    /// dispatch target from the default account's entry; collapsing them made
    /// the tool drop every mirrored account model.
    #[test]
    fn account_variants_survive_dedupe_and_exact_duplicates_do_not() {
        let entries = entries_from(vec![
            &model_option("claude-cli", "claude-opus-5", None),
            &model_option("claude-cli", "claude-opus-5", Some("parity")),
            &model_option("claude-cli", "claude-opus-5", None),
        ]);

        let ids: Vec<&str> = entries.iter().map(|entry| entry.id.as_str()).collect();
        assert_eq!(
            ids,
            vec![
                "claude-cli:claude-opus-5",
                "claude-cli@parity:claude-opus-5"
            ]
        );
    }

    #[test]
    fn provider_filter_narrows_models_but_keeps_modes() {
        let _home = crate::test_support::temp_zdx_home();
        let mut config = config_with_custom_models(&["alpha"]);
        config.model_modes = vec![crate::config::ModelMode {
            name: "smart".to_string(),
            description: String::new(),
            primary: "claude-cli:claude-opus-5@high".to_string(),
            alternatives: Vec::new(),
            thinking: crate::config::ThinkingLevel::High,
        }];

        let output = execute(&json!({"provider": "mock"}), &context(&config));
        let data = output.data().unwrap();

        assert_eq!(data["modes"][0]["name"], json!("smart"));
        assert_eq!(data["models"].as_array().unwrap().len(), 1);
        assert_eq!(data["models"][0]["id"], json!("mock:alpha"));
    }

    #[test]
    fn custom_provider_models_are_listed_with_their_qualified_id() {
        let _home = crate::test_support::temp_zdx_home();
        let config = config_with_custom_models(&["alpha", "beta"]);

        let ids = ids(&execute(&json!({}), &context(&config)));

        assert!(ids.contains(&"mock:alpha".to_string()));
        assert!(ids.contains(&"mock:beta".to_string()));
    }

    #[test]
    fn custom_provider_models_are_not_subscription_backed() {
        let _home = crate::test_support::temp_zdx_home();
        let config = config_with_custom_models(&["alpha"]);

        let output = execute(&json!({}), &context(&config));
        let models = output.data().unwrap()["models"].as_array().unwrap().clone();
        let entry = models
            .iter()
            .find(|model| model["id"] == "mock:alpha")
            .expect("custom model listed");
        assert_eq!(entry["subscription"], json!(false));
        assert_eq!(entry["display_name"], json!("alpha"));
    }

    #[test]
    fn provider_filter_is_case_insensitive_and_exact() {
        let _home = crate::test_support::temp_zdx_home();
        let config = config_with_custom_models(&["alpha", "beta"]);

        let filtered = ids(&execute(&json!({"provider": "MOCK"}), &context(&config)));
        assert_eq!(filtered, vec!["mock:alpha", "mock:beta"]);

        let other = ids(&execute(&json!({"provider": "parity"}), &context(&config)));
        assert!(
            other
                .iter()
                .all(|id| id.starts_with("parity:") && !id.starts_with("mock:")),
            "filter must exclude other providers: {other:?}"
        );
    }

    #[test]
    fn disabled_providers_are_excluded() {
        let _home = crate::test_support::temp_zdx_home();
        let mut config = Config::default();
        config.providers.claude_cli.enabled = Some(false);

        let ids = ids(&execute(&json!({}), &context(&config)));

        assert!(
            !ids.iter().any(|id| id.starts_with("claude-cli")),
            "disabled providers must not be listed: {ids:?}"
        );
    }

    #[test]
    fn oversized_lists_are_truncated_with_a_narrowing_note() {
        let _home = crate::test_support::temp_zdx_home();
        let models: Vec<String> = (0..MAX_MODELS + 5).map(|i| format!("m{i:04}")).collect();
        let models: Vec<&str> = models.iter().map(String::as_str).collect();
        let config = config_with_custom_models(&models);

        let output = execute(&json!({"provider": "mock"}), &context(&config));
        let data = output.data().unwrap().clone();

        assert_eq!(data["truncated"], json!(true));
        assert_eq!(data["total"], json!(MAX_MODELS + 5));
        assert_eq!(data["models"].as_array().unwrap().len(), MAX_MODELS);
        assert!(data["note"].as_str().unwrap().contains("Pass `provider`"));
    }
}
