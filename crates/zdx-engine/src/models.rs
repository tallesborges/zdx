//! Model registry for the TUI model picker.
//!
//! Loads models from `<base>/models.toml` when present, otherwise falls back to
//! `default_models.toml`, then applies persistent user metadata from
//! `<base>/model_overrides.toml`.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::Path;
use std::sync::{Mutex, OnceLock};

use serde::Deserialize;

const DEFAULT_MODELS_TOML: &str = zdx_assets::DEFAULT_MODELS_TOML;

pub fn default_models_toml() -> &'static str {
    DEFAULT_MODELS_TOML
}

/// Embedded `model_overrides.toml`.
pub fn default_model_overrides_toml() -> &'static str {
    zdx_assets::MODEL_OVERRIDES_TOML
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

const TOKENS_PER_MILLION: f64 = 1_000_000.0;

impl ModelPricing {
    /// USD cost for the given token counts (prices are per million tokens).
    ///
    /// Shared cost path for the TUI, bot, CLI stats, and monitor so cost is
    /// computed identically everywhere.
    pub fn cost(&self, input: u64, output: u64, cache_read: u64, cache_write: u64) -> f64 {
        (input as f64 / TOKENS_PER_MILLION) * self.input
            + (output as f64 / TOKENS_PER_MILLION) * self.output
            + (cache_read as f64 / TOKENS_PER_MILLION) * self.cache_read
            + (cache_write as f64 / TOKENS_PER_MILLION) * self.cache_write
    }

    /// USD saved by serving `cache_read` tokens from cache instead of paying
    /// the full input price.
    pub fn cache_savings(&self, cache_read: u64) -> f64 {
        (cache_read as f64 / TOKENS_PER_MILLION) * (self.input - self.cache_read)
    }
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
    /// Optional API routing hint for meta-providers (e.g., opencode-go).
    ///
    /// Expected values:
    /// - "anthropic-messages"
    /// - "openai-responses"
    /// - "google-generative-ai"
    /// - "openai-completions"
    pub api: Option<&'static str>,
}

/// Definition of an available model.
#[derive(Debug, Clone)]
pub struct ModelOption {
    /// Model ID (sent to API)
    pub id: &'static str,
    /// Provider identifier
    pub provider: &'static str,
    /// Named OAuth account for multi-account providers (`None` = default).
    pub account: Option<&'static str>,
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
            let (overrides, _) = load_user_model_overrides();
            apply_user_model_overrides(&mut models, &overrides);

            let mut seen = HashSet::new();
            let mut combined = Vec::new();

            for model in models.drain(..) {
                // Deduplicate by (provider, account, id) since id no longer
                // has the provider prefix
                if seen.insert((model.provider, model.account, model.id)) {
                    combined.push(model);
                }
            }
            combined.extend(account_variants(&combined, &mut seen));
            combined
        })
        .as_slice()
}

/// Mirrors every OAuth-provider model into one variant per additional named
/// account stored in `oauth.json`, so logging in a second subscription makes
/// that provider's whole model list available without editing `models.toml`.
fn account_variants(
    models: &[ModelOption],
    seen: &mut HashSet<(&'static str, Option<&'static str>, &'static str)>,
) -> Vec<ModelOption> {
    let cache = match crate::providers::oauth::OAuthCache::load() {
        Ok(cache) => cache,
        Err(err) => {
            tracing::warn!(%err, "Failed to load OAuth cache; skipping account models");
            return Vec::new();
        }
    };

    let mut accounts_by_provider: HashMap<&'static str, Vec<&'static str>> = HashMap::new();
    for kind in crate::providers::ProviderKind::all() {
        if !kind.supports_oauth() {
            continue;
        }
        let named: Vec<&'static str> = cache
            .accounts(kind.id())
            .into_iter()
            .flatten()
            .map(leak_string)
            .collect();
        if !named.is_empty() {
            accounts_by_provider.insert(kind.id(), named);
        }
    }

    expand_accounts(models, &accounts_by_provider, seen)
}

/// Pure expansion step of [`account_variants`]: one variant per model per named
/// account of that model's provider, skipping pairs already in `seen`.
fn expand_accounts(
    models: &[ModelOption],
    accounts_by_provider: &HashMap<&'static str, Vec<&'static str>>,
    seen: &mut HashSet<(&'static str, Option<&'static str>, &'static str)>,
) -> Vec<ModelOption> {
    let mut out = Vec::new();
    for model in models {
        let Some(accounts) = accounts_by_provider.get(model.provider) else {
            continue;
        };
        for account in accounts {
            if seen.insert((model.provider, Some(account), model.id)) {
                out.push(ModelOption {
                    account: Some(account),
                    ..*model
                });
            }
        }
    }
    out
}
static CUSTOM_MODELS: OnceLock<Mutex<HashMap<String, &'static [ModelOption]>>> = OnceLock::new();

/// Synthesizes picker entries for custom providers (`[providers.custom.<name>]`)
/// so their configured models show up. Pricing/context are zeroed (not in the
/// registry).
///
/// Results are leaked to `'static` and cached keyed on the provider→models
/// pairs that shape the output, so distinct configs return distinct slices
/// (one leak per distinct config, not per call) instead of the first caller's
/// config winning for the whole process.
///
/// # Panics
/// Panics if the internal custom-models cache mutex is poisoned.
pub fn custom_provider_models(
    providers: &crate::config::ProvidersConfig,
) -> &'static [ModelOption] {
    let (overrides, overrides_key) = load_user_model_overrides();
    let mut entries: Vec<(&str, &[String])> = providers
        .custom
        .iter()
        .map(|(name, cfg)| (name.trim(), cfg.models.as_slice()))
        .filter(|(name, _)| !name.is_empty())
        .collect();
    entries.sort_by(|a, b| a.0.cmp(b.0));

    let mut key = String::new();
    for (provider, models) in &entries {
        key.push_str(provider);
        key.push('\u{1f}');
        for model in *models {
            key.push_str(model.trim());
            key.push('\u{1e}');
        }
        key.push('\u{1d}');
    }
    key.push_str(&overrides_key);

    let cache = CUSTOM_MODELS.get_or_init(|| Mutex::new(HashMap::new()));
    let mut guard = cache.lock().expect("custom models cache poisoned");
    if let Some(models) = guard.get(&key) {
        return models;
    }

    let mut out = Vec::new();
    for (provider, models) in &entries {
        for model in *models {
            let id = model.trim();
            if id.is_empty() {
                continue;
            }
            let qualified_id = format!("{provider}:{id}");
            if overrides.iter().any(|entry| entry.id == qualified_id) {
                continue;
            }
            let mut model = ModelOption {
                id: leak_string(id.to_string()),
                provider: leak_string((*provider).to_string()),
                account: None,
                display_name: leak_string(id.to_string()),
                pricing: ModelPricing {
                    input: 0.0,
                    output: 0.0,
                    cache_read: 0.0,
                    cache_write: 0.0,
                },
                context_limit: 0,
                capabilities: ModelCapabilities {
                    reasoning: true,
                    input_images: false,
                    output_limit: 0,
                    api: Some("openai-completions"),
                },
            };
            apply_overrides_to_model(&mut model, &overrides);
            out.push(model);
        }
    }

    let leaked: &'static [ModelOption] = Box::leak(out.into_boxed_slice());
    guard.insert(key, leaked);
    leaked
}

impl ModelOption {
    /// Returns the fully qualified model id used in config and model strings
    /// (`provider:id`, or `provider@account:id` for a named OAuth account).
    #[must_use]
    pub fn qualified_id(&self) -> String {
        let provider = crate::providers::oauth::account_cache_key(self.provider, self.account);
        format!("{provider}:{}", self.id)
    }

    /// Finds a model by its ID.
    pub fn find_by_id(id: &str) -> Option<&'static ModelOption> {
        // Try exact match on id first
        if let Some(model) = available_models().iter().find(|m| m.id == id) {
            return Some(model);
        }
        if let Some(model) = available_models().iter().find(|m| m.qualified_id() == id) {
            return Some(model);
        }

        // Fall back to resolving provider prefix and comparing
        let target = crate::providers::resolve_provider(id);
        available_models().iter().find(|m| {
            // Use stored provider instead of resolving from id
            let provider_kind = crate::providers::provider_kind_from_id(m.provider);
            provider_kind == Some(target.kind)
                && m.id == target.model
                && m.account.map(str::to_string) == target.account
        })
    }

    /// Finds a model by explicit provider + model ID.
    pub fn find_by_provider_and_id(provider: &str, id: &str) -> Option<&'static ModelOption> {
        available_models()
            .iter()
            .find(|m| m.provider.eq_ignore_ascii_case(provider) && m.id.eq_ignore_ascii_case(id))
    }
}

/// Returns true if the model supports reasoning, defaulting to true when unknown.
pub fn model_supports_reasoning(id: &str) -> bool {
    ModelOption::find_by_id(id).is_none_or(|model| model.capabilities.reasoning)
}

/// Returns true if the model accepts image input, defaulting to true when
/// unknown.
///
/// Custom-provider models (`[providers.custom.<name>]`) are not part of
/// `available_models()`, so their synthesized entries are consulted too — they
/// default to no image input until a `model_overrides.toml` entry says
/// otherwise.
pub fn model_reads_images(
    providers: &crate::config::ProvidersConfig,
    provider: &str,
    model: &str,
) -> bool {
    ModelOption::find_by_provider_and_id(provider, model)
        .or_else(|| {
            custom_provider_models(providers)
                .iter()
                .find(|entry| entry.provider.eq_ignore_ascii_case(provider) && entry.id == model)
        })
        .is_none_or(|entry| entry.capabilities.input_images)
}

/// Model spec parsing/formatting (`provider:model[@thinking][@fast]`) lives in
/// `zdx-types` so providers and engine share one grammar.
pub use zdx_types::config::ModelSpec;

/// Formats a model spec with an explicit thinking level, preserving any other
/// modifier already present in the spec (e.g. `@fast`).
#[must_use]
pub fn format_model_thinking(model: &str, level: crate::config::ThinkingLevel) -> String {
    ModelSpec::parse(model).with_thinking(level).to_string()
}

/// Returns the `@fast` variant of a model spec when its provider supports the
/// priority service tier (`OpenAI` and `OpenAI` Codex), otherwise `None`.
#[must_use]
pub fn fast_variant(model: &str) -> Option<String> {
    let spec = ModelSpec::parse(model);
    if spec.fast
        || !crate::providers::resolve_provider(spec.base)
            .kind
            .supports_priority_tier()
    {
        return None;
    }
    Some(spec.with_fast(true).to_string())
}

/// Returns the bare model id (with any leading `provider:` prefix stripped).
///
/// `provider` should match the `ModelOption::provider` field. If the id does
/// not actually start with `<provider>:`, the id is returned unchanged.
pub fn bare_model_id<'a>(provider: &str, id: &'a str) -> &'a str {
    let prefix = format!("{provider}:");
    id.strip_prefix(prefix.as_str()).unwrap_or(id)
}

/// Returns true if a model's bare id matches any pattern in the provider's
/// `[providers.X].models` list. An empty pattern list means "no filter"
/// (matches anything) so existing configs without an explicit allow-list
/// keep showing every registered model.
///
/// Patterns may use `*` as a wildcard. Matching is case-insensitive.
pub fn model_id_matches_patterns(bare_id: &str, patterns: &[String]) -> bool {
    if patterns.is_empty() {
        return true;
    }
    patterns.iter().any(|pattern| {
        let pattern = pattern.trim();
        !pattern.is_empty() && wildcard_match(pattern, bare_id)
    })
}

/// Glob match with `*` wildcards (any-run). Case-insensitive ASCII compare.
pub fn wildcard_match(pattern: &str, text: &str) -> bool {
    if pattern == "*" {
        return true;
    }

    let p = pattern.to_ascii_lowercase();
    let t = text.to_ascii_lowercase();
    let p = p.as_bytes();
    let t = t.as_bytes();
    let mut p_idx = 0;
    let mut t_idx = 0;
    let mut star_idx: Option<usize> = None;
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

#[cfg(test)]
mod tests {
    use super::{
        ModelCapabilities, ModelOption, ModelPricing, UserModelOverride,
        apply_user_model_overrides, bare_model_id, custom_provider_models, fast_variant,
        format_model_thinking, model_id_matches_patterns, model_reads_images, wildcard_match,
    };
    use crate::config::{CustomProviderConfig, ProvidersConfig, ThinkingLevel};

    fn providers_with(name: &str, models: &[&str]) -> ProvidersConfig {
        let mut providers = ProvidersConfig::default();
        providers.custom.insert(
            name.to_string(),
            CustomProviderConfig {
                models: models.iter().map(|m| (*m).to_string()).collect(),
                ..Default::default()
            },
        );
        providers
    }

    #[test]
    fn custom_provider_models_honors_distinct_configs() {
        let _home = crate::test_support::temp_zdx_home();
        let a = custom_provider_models(&providers_with("prov-a", &["m1", "m2"]));
        let b = custom_provider_models(&providers_with("prov-b", &["m3"]));

        let ids_a: Vec<_> = a.iter().map(|m| (m.provider, m.id)).collect();
        let ids_b: Vec<_> = b.iter().map(|m| (m.provider, m.id)).collect();

        assert_eq!(ids_a, vec![("prov-a", "m1"), ("prov-a", "m2")]);
        assert_eq!(ids_b, vec![("prov-b", "m3")]);

        // Same config returns the identical cached slice (no re-leak per call).
        let a_again = custom_provider_models(&providers_with("prov-a", &["m1", "m2"]));
        assert_eq!(a.as_ptr(), a_again.as_ptr());
    }

    #[test]
    fn user_override_updates_existing_model_metadata() {
        let mut models = vec![ModelOption {
            id: "model-a",
            provider: "provider-a",
            account: None,
            display_name: "Model A",
            pricing: ModelPricing {
                input: 1.0,
                output: 2.0,
                cache_read: 0.0,
                cache_write: 0.0,
            },
            context_limit: 100,
            capabilities: ModelCapabilities::default(),
        }];
        let overrides = vec![override_for("provider-a:model-a", 200)];

        apply_user_model_overrides(&mut models, &overrides);

        assert_eq!(models.len(), 1);
        assert_eq!(models[0].context_limit, 200);
        assert_eq!(models[0].capabilities.output_limit, 50);
    }

    #[test]
    fn user_override_adds_custom_provider_model_metadata() {
        let mut models = Vec::new();
        let overrides = vec![override_for("parity:deepseek-flash-parity", 1_000_000)];

        apply_user_model_overrides(&mut models, &overrides);

        assert_eq!(models.len(), 1);
        assert_eq!(models[0].qualified_id(), "parity:deepseek-flash-parity");
        assert_eq!(models[0].context_limit, 1_000_000);
        assert_eq!(models[0].capabilities.output_limit, 50);
    }

    #[test]
    fn image_capability_covers_registry_and_custom_provider_models() {
        assert!(model_reads_images(
            &ProvidersConfig::default(),
            "gemini",
            "gemini-3.6-flash"
        ));
        assert!(!model_reads_images(
            &ProvidersConfig::default(),
            "deepseek",
            "deepseek-v4-flash"
        ));

        // Custom-provider models live outside `available_models()`; they must
        // still resolve, and default to no image input.
        let providers = providers_with("parity", &["openrouter/deepseek/deepseek-v4-flash"]);
        assert!(!model_reads_images(
            &providers,
            "parity",
            "openrouter/deepseek/deepseek-v4-flash"
        ));

        // Unknown models stay permissive.
        assert!(model_reads_images(
            &ProvidersConfig::default(),
            "parity",
            "not-configured"
        ));
    }

    fn override_for(id: &str, context_limit: u64) -> UserModelOverride {
        UserModelOverride {
            id: id.to_string(),
            display_name: None,
            input: None,
            output: None,
            cache_read: None,
            cache_write: None,
            context_limit: Some(context_limit),
            output_limit: Some(50),
            reasoning: None,
            input_images: None,
            api: None,
        }
    }

    #[test]
    fn fast_variant_offered_only_for_priority_tier_providers() {
        assert_eq!(
            fast_variant("openai:gpt-5.2").as_deref(),
            Some("openai:gpt-5.2@fast")
        );
        assert_eq!(
            fast_variant("openai-codex:gpt-5.2").as_deref(),
            Some("openai-codex:gpt-5.2@fast")
        );
        assert_eq!(fast_variant("openai:gpt-5.2@fast"), None);
        assert_eq!(fast_variant("claude-haiku-4-5"), None);
    }

    #[test]
    fn format_model_thinking_keeps_fast_and_replaces_thinking() {
        assert_eq!(
            format_model_thinking("openai:gpt-5.2@fast", ThinkingLevel::High),
            "openai:gpt-5.2@high@fast"
        );
        assert_eq!(
            format_model_thinking("openai:gpt-5.2@low", ThinkingLevel::High),
            "openai:gpt-5.2@high"
        );
    }

    #[test]
    fn wildcard_match_exact_and_star() {
        assert!(wildcard_match("mimo-v2.5", "mimo-v2.5"));
        assert!(wildcard_match("*", "anything"));
        assert!(wildcard_match("gpt-5*", "gpt-5.5"));
        assert!(wildcard_match("*:exacto", "claude-sonnet-4-5:exacto"));
        assert!(!wildcard_match("mimo-v2.5", "mimo-v2.5-pro"));
        assert!(!wildcard_match("gpt-5*", "claude-sonnet"));
    }

    #[test]
    fn wildcard_match_is_case_insensitive() {
        assert!(wildcard_match("MiMo-V2.5", "mimo-v2.5"));
    }

    #[test]
    fn bare_model_id_strips_provider_prefix() {
        assert_eq!(
            bare_model_id("xiaomi-plan", "xiaomi-plan:mimo-v2.5"),
            "mimo-v2.5"
        );
        assert_eq!(
            bare_model_id("openrouter", "openrouter:xiaomi/mimo-v2-flash:free"),
            "xiaomi/mimo-v2-flash:free"
        );
        // Non-matching prefix returns the original id unchanged.
        assert_eq!(bare_model_id("xiaomi", "openai:gpt-5"), "openai:gpt-5");
    }

    #[test]
    fn model_id_matches_patterns_empty_list_matches_everything() {
        assert!(model_id_matches_patterns("mimo-v2.5-pro", &[]));
    }

    #[test]
    fn model_id_matches_patterns_literal_and_wildcard() {
        let patterns = vec!["mimo-v2.5-pro".to_string(), "mimo-v2.5".to_string()];
        assert!(model_id_matches_patterns("mimo-v2.5-pro", &patterns));
        assert!(model_id_matches_patterns("mimo-v2.5", &patterns));
        assert!(!model_id_matches_patterns("mimo-v2-flash", &patterns));

        let wildcard = vec!["*:exacto".to_string()];
        assert!(model_id_matches_patterns("anything:exacto", &wildcard));
        assert!(!model_id_matches_patterns("anything:free", &wildcard));
    }

    #[test]
    fn model_id_matches_patterns_ignores_blank_entries() {
        let patterns = vec![String::new(), "   ".to_string()];
        // Treated as "no usable patterns": only blank entries should not silently match
        // because the empty-list rule already covers the "no filter" case. Blank-only
        // lists are treated like an empty list.
        assert!(!model_id_matches_patterns("mimo-v2.5", &patterns));
    }
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
    #[serde(default)]
    api: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
struct UserModelOverridesFile {
    #[serde(rename = "override", default)]
    overrides: Vec<UserModelOverride>,
}

#[derive(Debug, Deserialize)]
struct UserModelOverride {
    id: String,
    #[serde(default)]
    display_name: Option<String>,
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
    #[serde(default)]
    api: Option<String>,
}

fn load_user_model_overrides() -> (Vec<UserModelOverride>, String) {
    let path = crate::config::paths::zdx_home().join("model_overrides.toml");
    let Ok(contents) = fs::read_to_string(&path) else {
        return (Vec::new(), String::new());
    };
    let file = match toml::from_str::<UserModelOverridesFile>(&contents) {
        Ok(file) => file,
        Err(err) => {
            tracing::warn!(%err, path = %path.display(), "Failed to parse user model overrides");
            return (Vec::new(), contents);
        }
    };
    (file.overrides, contents)
}

fn apply_user_model_overrides(models: &mut Vec<ModelOption>, overrides: &[UserModelOverride]) {
    for entry in overrides {
        let mut matched = false;
        for model in &mut *models {
            if override_matches(model, &entry.id) {
                apply_override(model, entry);
                matched = true;
            }
        }
        if !matched && let Some(mut model) = model_from_override(entry) {
            apply_override(&mut model, entry);
            models.push(model);
        }
    }
}

fn apply_overrides_to_model(model: &mut ModelOption, overrides: &[UserModelOverride]) {
    for entry in overrides {
        if override_matches(model, &entry.id) {
            apply_override(model, entry);
        }
    }
}

fn override_matches(model: &ModelOption, id: &str) -> bool {
    let id = id.trim();
    model.qualified_id().eq_ignore_ascii_case(id)
        || (!id.contains(':') && model.id.eq_ignore_ascii_case(id))
}

fn model_from_override(entry: &UserModelOverride) -> Option<ModelOption> {
    let (provider, id) = entry.id.trim().split_once(':')?;
    let provider = provider.trim().to_lowercase();
    let id = id.trim();
    if provider.is_empty() || id.is_empty() {
        return None;
    }
    Some(ModelOption {
        id: leak_string(id.to_string()),
        provider: leak_string(provider),
        account: None,
        display_name: leak_string(id.to_string()),
        pricing: ModelPricing {
            input: 0.0,
            output: 0.0,
            cache_read: 0.0,
            cache_write: 0.0,
        },
        context_limit: 0,
        capabilities: ModelCapabilities {
            reasoning: true,
            input_images: false,
            output_limit: 0,
            api: Some("openai-completions"),
        },
    })
}

fn apply_override(model: &mut ModelOption, entry: &UserModelOverride) {
    if let Some(value) = entry.display_name.as_deref() {
        model.display_name = leak_string(value.trim().to_string());
    }
    if let Some(value) = entry.input {
        model.pricing.input = value;
    }
    if let Some(value) = entry.output {
        model.pricing.output = value;
    }
    if let Some(value) = entry.cache_read {
        model.pricing.cache_read = value;
    }
    if let Some(value) = entry.cache_write {
        model.pricing.cache_write = value;
    }
    if let Some(value) = entry.context_limit {
        model.context_limit = value;
    }
    if let Some(value) = entry.output_limit {
        model.capabilities.output_limit = value;
    }
    if let Some(value) = entry.reasoning {
        model.capabilities.reasoning = value;
    }
    if let Some(value) = entry.input_images {
        model.capabilities.input_images = value;
    }
    if let Some(value) = entry.api.as_deref() {
        model.capabilities.api = Some(leak_string(value.trim().to_string()));
    }
}

fn load_models_from_path(path: &Path) -> Option<Vec<ModelOption>> {
    let contents = fs::read_to_string(path).ok()?;
    load_models_from_str(&contents)
}

fn load_models_from_str(contents: &str) -> Option<Vec<ModelOption>> {
    let file: ModelsFile = match toml::from_str(contents) {
        Ok(file) => file,
        Err(err) => {
            tracing::warn!(%err, "Failed to parse models file");
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
    let raw_id = record.id.trim();
    if raw_id.is_empty() {
        return None;
    }

    // Strip provider prefix from id if present (e.g., "claude-cli:claude-opus-4-6" -> "claude-opus-4-6")
    let resolved = crate::providers::resolve_provider(raw_id);
    let id = resolved.model;
    let account = resolved.account.map(leak_string);

    let provider = record
        .provider
        .unwrap_or_else(|| resolved.kind.id().to_string())
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
        account,
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
            api: capabilities.api.map(leak_string),
        },
    })
}

fn leak_string(value: String) -> &'static str {
    Box::leak(value.into_boxed_str())
}

#[cfg(test)]
mod account_tests {
    use std::collections::{HashMap, HashSet};

    use super::{ModelCapabilities, ModelOption, ModelPricing, expand_accounts};

    fn model(provider: &'static str, id: &'static str) -> ModelOption {
        ModelOption {
            id,
            provider,
            account: None,
            display_name: id,
            pricing: ModelPricing {
                input: 0.0,
                output: 0.0,
                cache_read: 0.0,
                cache_write: 0.0,
            },
            context_limit: 0,
            capabilities: ModelCapabilities::default(),
        }
    }

    #[test]
    fn expands_every_model_of_a_provider_with_named_accounts() {
        let models = vec![
            model("claude-cli", "opus-5"),
            model("claude-cli", "sonnet-5"),
            model("openai", "gpt-5.2"),
        ];
        let accounts = HashMap::from([("claude-cli", vec!["parity"])]);
        let mut seen: HashSet<_> = models
            .iter()
            .map(|m| (m.provider, m.account, m.id))
            .collect();

        let variants = expand_accounts(&models, &accounts, &mut seen);

        assert_eq!(variants.len(), 2, "only claude-cli models are mirrored");
        assert!(
            variants
                .iter()
                .all(|m| m.account == Some("parity") && m.provider == "claude-cli")
        );
        assert_eq!(variants[0].qualified_id(), "claude-cli@parity:opus-5");
    }

    #[test]
    fn does_not_duplicate_an_explicit_models_toml_account_row() {
        let mut explicit = model("claude-cli", "opus-5");
        explicit.account = Some("parity");
        let models = vec![model("claude-cli", "opus-5"), explicit];
        let accounts = HashMap::from([("claude-cli", vec!["parity"])]);
        let mut seen: HashSet<_> = models
            .iter()
            .map(|m| (m.provider, m.account, m.id))
            .collect();

        assert!(expand_accounts(&models, &accounts, &mut seen).is_empty());
    }
}
