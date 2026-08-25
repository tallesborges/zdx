//! `DeepSeek` provider (OpenAI-compatible Chat Completions).

use std::collections::HashMap;

use anyhow::Result;
use reqwest::header::HeaderMap;
use serde_json::json;
use zdx_types::{ThinkingLevel, ToolDefinition};

use crate::openai::chat_completions::{OpenAIChatCompletionsClient, OpenAIChatCompletionsConfig};
use crate::shared::merge_system_prompt;
use crate::{ProviderKind, ProviderStream};

/// `DeepSeek` API configuration.
#[derive(Debug, Clone)]
pub struct DeepSeekConfig {
    pub api_key: String,
    pub base_url: String,
    pub model: String,
    pub max_tokens: Option<u32>,
    pub prompt_cache_key: Option<String>,
    pub thinking_enabled: bool,
    pub reasoning_effort: Option<String>,
}

impl DeepSeekConfig {
    /// Creates a new config from environment.
    ///
    /// Authentication resolution order:
    /// 1. `config_api_key` parameter (from config file)
    /// 2. `DEEPSEEK_API_KEY` environment variable
    ///
    /// Environment variables:
    /// - `DEEPSEEK_API_KEY` (fallback if not in config)
    /// - `DEEPSEEK_BASE_URL` (optional)
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
        reasoning_effort: Option<String>,
    ) -> Result<Self> {
        let api_key = ProviderKind::DeepSeek.resolve_api_key(config_api_key)?;
        let base_url = ProviderKind::DeepSeek.resolve_base_url(config_base_url)?;

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

/// `DeepSeek` client.
pub struct DeepSeekClient {
    inner: OpenAIChatCompletionsClient,
}

fn reasoning_effort_from_thinking_level(level: ThinkingLevel) -> Option<&'static str> {
    match level {
        ThinkingLevel::Off => None,
        ThinkingLevel::Low => Some("low"),
        ThinkingLevel::Medium | ThinkingLevel::High | ThinkingLevel::XHigh => Some("high"),
        ThinkingLevel::Max => Some("max"),
    }
}

impl DeepSeekClient {
    pub fn new(config: DeepSeekConfig) -> Self {
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
                    // Don't send OpenAI-style reasoning: {effort} — DeepSeek
                    // expects a top-level `reasoning_effort` string via extra_body instead.
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

/// Constructs the `DeepSeek` client from the given context.
///
/// # Errors
/// Returns an error if the API key / base URL cannot be resolved from env or config.
pub fn build(
    ctx: &crate::ProviderBuildContext<'_>,
) -> anyhow::Result<Box<dyn crate::StreamingProvider>> {
    Ok(Box::new(DeepSeekClient::new(DeepSeekConfig::from_env(
        ctx.model.to_string(),
        ctx.config_max_tokens,
        ctx.base_url,
        ctx.api_key,
        ctx.cache_key.clone(),
        ctx.thinking_level.is_enabled(),
        reasoning_effort_from_thinking_level(ctx.thinking_level).map(str::to_owned),
    )?)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_reasoning_effort_to_deepseek_levels() {
        assert_eq!(
            reasoning_effort_from_thinking_level(ThinkingLevel::Off),
            None
        );
        assert_eq!(
            reasoning_effort_from_thinking_level(ThinkingLevel::Low),
            Some("low")
        );
        assert_eq!(
            reasoning_effort_from_thinking_level(ThinkingLevel::Medium),
            Some("high")
        );
        assert_eq!(
            reasoning_effort_from_thinking_level(ThinkingLevel::High),
            Some("high")
        );
        assert_eq!(
            reasoning_effort_from_thinking_level(ThinkingLevel::XHigh),
            Some("high")
        );
        assert_eq!(
            reasoning_effort_from_thinking_level(ThinkingLevel::Max),
            Some("max")
        );
    }
}
