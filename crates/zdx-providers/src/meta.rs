//! Meta Model API provider (OpenAI-compatible Chat Completions).
//!
//! Serves Meta's Muse Spark family. The Chat Completions API expects a
//! top-level `reasoning_effort` string (`minimal`..`xhigh`) rather than the
//! OpenAI-style `reasoning: { effort }` object, so it is injected via
//! `extra_body` (mirroring the `DeepSeek` provider).

use std::collections::HashMap;

use anyhow::Result;
use reqwest::header::HeaderMap;
use serde_json::json;
use zdx_types::ToolDefinition;

use crate::openai::chat_completions::{OpenAIChatCompletionsClient, OpenAIChatCompletionsConfig};
use crate::shared::merge_system_prompt;
use crate::{ProviderKind, ProviderStream};

/// Meta Model API configuration.
#[derive(Debug, Clone)]
pub struct MetaConfig {
    pub api_key: String,
    pub base_url: String,
    pub model: String,
    pub max_tokens: Option<u32>,
    pub prompt_cache_key: Option<String>,
    pub thinking_enabled: bool,
    pub reasoning_effort: Option<String>,
}

impl MetaConfig {
    /// Creates a new config from environment/config.
    ///
    /// Authentication resolution order:
    /// 1. `config_api_key` parameter (from config file)
    /// 2. `META_API_KEY` environment variable
    ///
    /// Environment variables:
    /// - `META_API_KEY` (fallback if not in config)
    /// - `META_API_BASE` (optional base URL override)
    ///
    /// # Errors
    /// Returns an error if the API key / base URL cannot be resolved.
    pub fn from_env(
        model: String,
        max_tokens: Option<u32>,
        config_base_url: Option<&str>,
        config_api_key: Option<&str>,
        prompt_cache_key: Option<String>,
        thinking_enabled: bool,
        reasoning_effort: Option<String>,
    ) -> Result<Self> {
        let api_key = ProviderKind::Meta.resolve_api_key(config_api_key)?;
        let base_url = ProviderKind::Meta.resolve_base_url(config_base_url)?;

        Ok(Self {
            api_key,
            base_url,
            model,
            max_tokens,
            prompt_cache_key,
            thinking_enabled,
            reasoning_effort,
        })
    }
}

/// Meta Model API client.
pub struct MetaClient {
    inner: OpenAIChatCompletionsClient,
}

impl MetaClient {
    pub fn new(config: MetaConfig) -> Self {
        let mut extra_body = HashMap::new();
        if let Some(effort) = &config.reasoning_effort {
            extra_body.insert("reasoning_effort".to_string(), json!(effort));
        }

        Self {
            inner: OpenAIChatCompletionsClient::with_extra_body(
                OpenAIChatCompletionsConfig {
                    api_key: config.api_key,
                    base_url: config.base_url,
                    model: config.model,
                    max_tokens: config.max_tokens,
                    max_completion_tokens: None,
                    // Meta expects a top-level `reasoning_effort` string via
                    // extra_body, not the OpenAI-style `reasoning: {effort}` object.
                    reasoning_effort: None,
                    prompt_cache_key: config.prompt_cache_key,
                    extra_headers: HeaderMap::new(),
                    include_usage: true,
                    include_reasoning_content: config.thinking_enabled,
                    thinking: Some(config.thinking_enabled.into()),
                },
                extra_body,
            ),
        }
    }

    ///
    /// # Errors
    /// Returns an error if the operation fails.
    pub async fn send_messages_stream(
        &self,
        messages: &[crate::ChatMessage],
        tools: &[ToolDefinition],
        system: Option<&str>,
    ) -> Result<ProviderStream> {
        let system = merge_system_prompt(system);
        self.inner
            .send_messages_stream(messages, tools, system.as_deref())
            .await
    }
}

/// Constructs the Meta client from the given context.
///
/// # Errors
/// Returns an error if the API key / base URL cannot be resolved from env or config.
pub fn build(
    ctx: &crate::ProviderBuildContext<'_>,
) -> anyhow::Result<Box<dyn crate::StreamingProvider>> {
    Ok(Box::new(MetaClient::new(MetaConfig::from_env(
        ctx.model.to_string(),
        ctx.config_max_tokens,
        ctx.base_url,
        ctx.api_key,
        ctx.cache_key.clone(),
        ctx.thinking_level.is_enabled(),
        crate::openai::reasoning_effort_from_thinking_level(ctx.thinking_level).map(str::to_owned),
    )?)))
}

#[cfg(test)]
mod tests {
    use crate::{ProviderKind, resolve_provider};

    #[test]
    fn meta_prefix_routes_to_meta_with_bare_model() {
        let selection = resolve_provider("meta:muse-spark-1.1");
        assert_eq!(selection.kind, ProviderKind::Meta);
        assert_eq!(selection.model, "muse-spark-1.1");
    }

    #[test]
    fn meta_id_resolves_kind() {
        assert_eq!(ProviderKind::from_id("meta"), Some(ProviderKind::Meta));
    }

    #[test]
    fn meta_metadata_matches_model_api() {
        assert_eq!(ProviderKind::Meta.id(), "meta");
        assert_eq!(
            ProviderKind::Meta.default_base_url(),
            "https://api.meta.ai/v1"
        );
        assert_eq!(ProviderKind::Meta.api_key_env_var(), Some("META_API_KEY"));
        assert_eq!(ProviderKind::Meta.base_url_env_var(), Some("META_API_BASE"));
        assert!(!ProviderKind::Meta.is_subscription());
        assert!(!ProviderKind::Meta.supports_oauth());
    }
}
