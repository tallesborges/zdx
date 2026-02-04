//! MiMo provider (Xiaomi MiMo OpenAI-compatible Chat Completions).

use anyhow::Result;
use reqwest::header::HeaderMap;

use crate::providers::ProviderStream;
use crate::providers::openai::chat_completions::{
    OpenAIChatCompletionsClient, OpenAIChatCompletionsConfig,
};
use crate::providers::shared::{resolve_api_key, resolve_base_url};
use crate::tools::ToolDefinition;

const DEFAULT_BASE_URL: &str = "https://api.xiaomimimo.com/v1";

/// MiMo API configuration.
#[derive(Debug, Clone)]
pub struct MimoConfig {
    pub api_key: String,
    pub base_url: String,
    pub model: String,
    pub max_tokens: Option<u32>,
    pub prompt_cache_key: Option<String>,
    pub thinking_enabled: bool,
}

impl MimoConfig {
    /// Creates a new config from environment.
    ///
    /// Authentication resolution order:
    /// 1. `config_api_key` parameter (from config file)
    /// 2. `MIMO_API_KEY` environment variable
    ///
    /// Environment variables:
    /// - `MIMO_API_KEY` (fallback if not in config)
    /// - `MIMO_BASE_URL` (optional)
    pub fn from_env(
        model: String,
        max_tokens: Option<u32>,
        config_base_url: Option<&str>,
        config_api_key: Option<&str>,
        prompt_cache_key: Option<String>,
        thinking_enabled: bool,
    ) -> Result<Self> {
        let api_key = resolve_api_key(config_api_key, "MIMO_API_KEY", "mimo")?;
        let base_url =
            resolve_base_url(config_base_url, "MIMO_BASE_URL", DEFAULT_BASE_URL, "MiMo")?;

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

/// MiMo client.
pub struct MimoClient {
    inner: OpenAIChatCompletionsClient,
}

impl MimoClient {
    pub fn new(config: MimoConfig) -> Self {
        Self {
            inner: OpenAIChatCompletionsClient::new(OpenAIChatCompletionsConfig {
                api_key: config.api_key,
                base_url: config.base_url,
                model: config.model,
                max_tokens: None,
                max_completion_tokens: config.max_tokens,
                reasoning_effort: None,
                prompt_cache_key: config.prompt_cache_key,
                extra_headers: HeaderMap::new(),
                include_usage: true,
                include_reasoning_content: config.thinking_enabled,
                thinking: Some(config.thinking_enabled.into()),
            }),
        }
    }

    pub async fn send_messages_stream(
        &self,
        messages: &[crate::providers::ChatMessage],
        tools: &[ToolDefinition],
        system: Option<&str>,
    ) -> Result<ProviderStream> {
        self.inner
            .send_messages_stream(messages, tools, system)
            .await
    }
}
