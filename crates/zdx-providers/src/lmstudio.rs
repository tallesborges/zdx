//! `LMStudio` provider (local server) using OpenAI-compatible API.

use anyhow::Result;
use reqwest::header::HeaderMap;
use zdx_types::ToolDefinition;

use crate::openai::chat_completions::{OpenAIChatCompletionsClient, OpenAIChatCompletionsConfig};
use crate::shared::merge_system_prompt;
use crate::{ChatMessage, ProviderKind, ProviderStream};

/// `LMStudio` API configuration.
#[derive(Debug, Clone)]
pub struct LMStudioConfig {
    pub api_key: String,
    pub base_url: String,
    pub model: String,
    pub max_tokens: Option<u32>,
    pub prompt_cache_key: Option<String>,
    pub thinking_enabled: bool,
}

impl LMStudioConfig {
    /// Creates a new config for a local `LMStudio` server.
    ///
    /// `LMStudio` runs locally and ignores the bearer token, so no API key is
    /// required. A `config_api_key` (when set) is forwarded as-is; otherwise a
    /// harmless placeholder is sent.
    ///
    /// Environment variables:
    /// - `LMSTUDIO_BASE_URL` (optional base URL override; defaults to `http://127.0.0.1:1234/v1`)
    ///
    /// # Errors
    /// Returns an error if the configured base URL is invalid.
    pub fn from_env(
        model: String,
        max_tokens: Option<u32>,
        config_base_url: Option<&str>,
        config_api_key: Option<&str>,
        prompt_cache_key: Option<String>,
        thinking_enabled: bool,
    ) -> Result<Self> {
        let api_key = config_api_key
            .map(str::trim)
            .filter(|key| !key.is_empty())
            .unwrap_or("lm-studio")
            .to_string();
        let base_url = ProviderKind::LMStudio.resolve_base_url(config_base_url)?;

        Ok(Self {
            api_key,
            base_url,
            model,
            max_tokens,
            prompt_cache_key,
            thinking_enabled,
        })
    }
}

/// `LMStudio` client using OpenAI-compatible API.
pub struct LMStudioClient {
    inner: OpenAIChatCompletionsClient,
}

impl LMStudioClient {
    pub fn new(config: LMStudioConfig) -> Self {
        Self {
            inner: OpenAIChatCompletionsClient::new(OpenAIChatCompletionsConfig {
                api_key: config.api_key,
                base_url: config.base_url,
                model: config.model,
                max_tokens: config.max_tokens,
                max_completion_tokens: None,
                reasoning_effort: None,
                prompt_cache_key: config.prompt_cache_key,
                extra_headers: HeaderMap::new(),
                include_usage: true,
                include_reasoning_content: config.thinking_enabled,
                thinking: None,
            }),
        }
    }

    ///
    /// # Errors
    /// Returns an error if the operation fails.
    pub async fn send_messages_stream(
        &self,
        messages: &[ChatMessage],
        tools: &[ToolDefinition],
        system: Option<&str>,
    ) -> Result<ProviderStream> {
        let system = merge_system_prompt(system);
        self.inner
            .send_messages_stream(messages, tools, system.as_deref())
            .await
    }
}

/// Constructs the `LMStudio` client from the given context.
///
/// # Errors
/// Returns an error if the base URL cannot be resolved from env or config.
pub fn build(
    ctx: &crate::ProviderBuildContext<'_>,
) -> anyhow::Result<Box<dyn crate::StreamingProvider>> {
    Ok(Box::new(LMStudioClient::new(LMStudioConfig::from_env(
        ctx.model.to_string(),
        ctx.config_max_tokens,
        ctx.base_url,
        ctx.api_key,
        ctx.cache_key.clone(),
        ctx.thinking_level.is_enabled(),
    )?)))
}
