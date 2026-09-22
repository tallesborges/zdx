//! `Xiaomi` provider (Xiaomi `MiMo` OpenAI-compatible Chat Completions).
//!
//! For the Token Plan subscription, see the sibling `xiaomi_plan` module.

use anyhow::Result;
use reqwest::header::HeaderMap;
use zdx_types::ToolDefinition;

use crate::openai::chat_completions::{OpenAIChatCompletionsClient, OpenAIChatCompletionsConfig};
use crate::shared::merge_system_prompt;
use crate::{ProviderKind, ProviderStream};

/// `Xiaomi` API configuration.
#[derive(Debug, Clone)]
pub struct XiaomiConfig {
    pub api_key: String,
    pub base_url: String,
    pub model: String,
    pub max_tokens: Option<u32>,
    pub prompt_cache_key: Option<String>,
    pub thinking_enabled: bool,
}

impl XiaomiConfig {
    /// Creates a new config from environment.
    ///
    /// Authentication resolution order:
    /// 1. `config_api_key` parameter (from config file)
    /// 2. `XIAOMI_API_KEY` environment variable
    ///
    /// Environment variables:
    /// - `XIAOMI_API_KEY` (fallback if not in config)
    /// - `XIAOMI_BASE_URL` (optional)
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
        let api_key = ProviderKind::Xiaomi.resolve_api_key(config_api_key)?;
        let base_url = ProviderKind::Xiaomi.resolve_base_url(config_base_url)?;

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

/// `Xiaomi` client.
pub struct XiaomiClient {
    inner: OpenAIChatCompletionsClient,
}

impl XiaomiClient {
    pub fn new(config: XiaomiConfig) -> Self {
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
                replay_historical_tool_turns: true,
                thinking: Some(config.thinking_enabled.into()),
            }),
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

/// Constructs the Xiaomi client from the given context.
///
/// # Errors
/// Returns an error if the API key / base URL cannot be resolved from env or config.
pub fn build(
    ctx: &crate::ProviderBuildContext<'_>,
) -> anyhow::Result<Box<dyn crate::StreamingProvider>> {
    Ok(Box::new(XiaomiClient::new(XiaomiConfig::from_env(
        ctx.model.to_string(),
        ctx.config_max_tokens,
        ctx.base_url,
        ctx.api_key,
        None,
        ctx.thinking_level.is_enabled(),
    )?)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xiaomi_enables_historical_tool_turn_replay() {
        let client = XiaomiClient::new(XiaomiConfig {
            api_key: "test-key".to_string(),
            base_url: "https://api.xiaomimimo.com/v1".to_string(),
            model: "mimo-v2.6-pro".to_string(),
            max_tokens: Some(4096),
            prompt_cache_key: None,
            thinking_enabled: true,
        });
        assert!(client.inner.replays_historical_tool_turns());
    }
}
