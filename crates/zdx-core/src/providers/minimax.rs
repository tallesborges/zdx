//! `MiniMax` provider using OpenAI-compatible API.

use anyhow::Result;
use reqwest::header::HeaderMap;

use crate::providers::openai::chat_completions::{
    OpenAIChatCompletionsClient, OpenAIChatCompletionsConfig,
};
use crate::providers::shared::{merge_system_prompt, resolve_api_key, resolve_base_url};
use crate::providers::{ChatMessage, ProviderStream};
use crate::tools::ToolDefinition;

const DEFAULT_BASE_URL: &str = "https://api.minimax.io/v1";

/// `MiniMax` API configuration.
#[derive(Debug, Clone)]
pub struct MinimaxConfig {
    pub api_key: String,
    pub base_url: String,
    pub model: String,
    pub max_tokens: Option<u32>,
    pub prompt_cache_key: Option<String>,
    pub thinking_enabled: bool,
}

impl MinimaxConfig {
    /// Creates a new config from environment.
    ///
    /// Authentication resolution order:
    /// 1. `config_api_key` parameter (from config file)
    /// 2. `MINIMAX_API_KEY` environment variable
    ///
    /// Environment variables:
    /// - `MINIMAX_API_KEY` (fallback if not in config)
    /// - `MINIMAX_BASE_URL` (optional)
    ///
    /// # Errors
    /// Returns an error if the operation fails.
    pub fn from_env(
        model: String,
        max_tokens: Option<u32>,
        config_base_url: Option<&str>,
        config_api_key: Option<&str>,
        prompt_cache_key: Option<String>,
        thinking_enabled: bool,
    ) -> Result<Self> {
        let api_key = resolve_api_key(config_api_key, "MINIMAX_API_KEY", "minimax")?;
        let base_url = resolve_base_url(
            config_base_url,
            "MINIMAX_BASE_URL",
            DEFAULT_BASE_URL,
            "MiniMax",
        )?;

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

/// `MiniMax` client using OpenAI-compatible API.
pub struct MinimaxClient {
    inner: OpenAIChatCompletionsClient,
}

impl MinimaxClient {
    pub fn new(config: MinimaxConfig) -> Self {
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
                thinking: Some(config.thinking_enabled.into()),
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
