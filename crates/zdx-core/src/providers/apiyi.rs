//! APIYI provider — meta-provider that routes to the appropriate
//! API client based on the model name (Anthropic native, Gemini native, or
//! `OpenAI` Chat Completions).

use anyhow::Result;
use reqwest::header::HeaderMap;

use crate::providers::ProviderStream;
use crate::providers::anthropic::api::{AnthropicClient, AnthropicConfig};
use crate::providers::anthropic::types::EffortLevel as AnthropicEffortLevel;
use crate::providers::gemini::api::{GeminiClient, GeminiConfig};
use crate::providers::gemini::shared::GeminiThinkingConfig;
use crate::providers::openai::api::{OpenAIClient, OpenAIConfig};
use crate::providers::openai::chat_completions::{
    OpenAIChatCompletionsClient, OpenAIChatCompletionsConfig,
};
use crate::providers::shared::{merge_system_prompt, resolve_api_key, resolve_base_url};
use crate::tools::ToolDefinition;

const DEFAULT_BASE_URL: &str = "https://api.apiyi.com";

#[derive(Debug, Clone)]
pub struct ApiyiConfig {
    pub api_key: String,
    pub base_url: String,
    pub model: String,
    pub max_tokens: Option<u32>,
    pub fallback_max_tokens: u32,
    pub thinking_enabled: bool,
    pub thinking_budget_tokens: u32,
    pub thinking_effort: Option<AnthropicEffortLevel>,
    pub gemini_thinking: Option<GeminiThinkingConfig>,
    pub reasoning_effort: Option<String>,
    pub cache_key: Option<String>,
}

impl ApiyiConfig {
    /// Creates a new `ApiyiConfig` from environment variables and provided parameters.
    ///
    /// # Errors
    /// Returns an error if the API key or base URL cannot be resolved.
    #[allow(clippy::too_many_arguments)]
    pub fn from_env(
        model: String,
        max_tokens: Option<u32>,
        fallback_max_tokens: u32,
        config_base_url: Option<&str>,
        config_api_key: Option<&str>,
        thinking_enabled: bool,
        thinking_budget_tokens: u32,
        thinking_effort: Option<AnthropicEffortLevel>,
        gemini_thinking: Option<GeminiThinkingConfig>,
        reasoning_effort: Option<String>,
        cache_key: Option<String>,
    ) -> Result<Self> {
        let api_key = resolve_api_key(config_api_key, "APIYI_API_KEY", "apiyi")?;
        let base_url =
            resolve_base_url(config_base_url, "APIYI_BASE_URL", DEFAULT_BASE_URL, "APIYI")?;

        Ok(Self {
            api_key,
            base_url,
            model,
            max_tokens,
            fallback_max_tokens,
            thinking_enabled,
            thinking_budget_tokens,
            thinking_effort,
            gemini_thinking,
            reasoning_effort,
            cache_key,
        })
    }
}

enum InnerClient {
    Anthropic(AnthropicClient),
    OpenAIResponses(OpenAIClient),
    Gemini(GeminiClient),
    ChatCompletions(OpenAIChatCompletionsClient),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ApiyiRoute {
    AnthropicMessages,
    OpenAIResponses,
    GoogleGenerativeAI,
    OpenAICompletions,
}

impl ApiyiRoute {
    fn from_registry_api_hint(api: &str) -> Option<Self> {
        match api {
            "anthropic-messages" => Some(Self::AnthropicMessages),
            "openai-responses" => Some(Self::OpenAIResponses),
            "google-generative-ai" => Some(Self::GoogleGenerativeAI),
            "openai-completions" => Some(Self::OpenAICompletions),
            _ => None,
        }
    }
}

fn resolve_apiyi_route(model: &str) -> ApiyiRoute {
    crate::models::ModelOption::find_by_provider_and_id("apiyi", model)
        .and_then(|m| m.capabilities.api)
        .and_then(ApiyiRoute::from_registry_api_hint)
        .unwrap_or(ApiyiRoute::OpenAICompletions)
}

/// APIYI meta-provider that routes requests to the appropriate API
/// client based on the model name.
pub struct ApiyiClient {
    inner: InnerClient,
}

impl ApiyiClient {
    /// Creates a new `ApiyiClient`, selecting the inner provider based on model name.
    pub fn new(config: ApiyiConfig) -> Self {
        let route = resolve_apiyi_route(&config.model);
        let inner = match route {
            ApiyiRoute::AnthropicMessages => {
                // Anthropic Messages API — base_url as-is (client appends /v1/messages)
                InnerClient::Anthropic(AnthropicClient::new(AnthropicConfig {
                    api_key: config.api_key,
                    base_url: config.base_url,
                    model: config.model,
                    max_tokens: config.max_tokens.unwrap_or(config.fallback_max_tokens),
                    thinking_enabled: config.thinking_enabled,
                    thinking_budget_tokens: config.thinking_budget_tokens,
                    thinking_effort: config.thinking_effort,
                }))
            }
            ApiyiRoute::OpenAIResponses => {
                // OpenAI Responses API — base_url as-is (client uses {base}/v1/responses)
                InnerClient::OpenAIResponses(OpenAIClient::new(OpenAIConfig {
                    api_key: config.api_key,
                    base_url: config.base_url,
                    model: config.model,
                    max_output_tokens: config.max_tokens,
                    prompt_cache_key: config.cache_key,
                }))
            }
            ApiyiRoute::GoogleGenerativeAI => {
                // Gemini API — append /v1 (client appends /models/{model}:stream...)
                InnerClient::Gemini(GeminiClient::new(GeminiConfig {
                    api_key: config.api_key,
                    base_url: format!("{}/v1", config.base_url),
                    model: config.model,
                    max_output_tokens: config.max_tokens,
                    thinking_config: config.gemini_thinking,
                }))
            }
            ApiyiRoute::OpenAICompletions => {
                // Chat Completions fallback — append /v1 (client appends /chat/completions)
                InnerClient::ChatCompletions(OpenAIChatCompletionsClient::new(
                    OpenAIChatCompletionsConfig {
                        api_key: config.api_key,
                        base_url: format!("{}/v1", config.base_url),
                        model: config.model,
                        max_tokens: config.max_tokens,
                        max_completion_tokens: None,
                        reasoning_effort: config.reasoning_effort,
                        prompt_cache_key: config.cache_key,
                        extra_headers: HeaderMap::new(),
                        include_usage: true,
                        include_reasoning_content: false,
                        thinking: None,
                    },
                ))
            }
        };

        Self { inner }
    }

    /// Sends messages to the appropriate inner provider and returns a stream.
    ///
    /// # Errors
    /// Returns an error if the inner provider request fails.
    pub async fn send_messages_stream(
        &self,
        messages: &[crate::providers::ChatMessage],
        tools: &[ToolDefinition],
        system: Option<&str>,
    ) -> Result<ProviderStream> {
        let system = merge_system_prompt(system);
        match &self.inner {
            InnerClient::Anthropic(c) => {
                c.send_messages_stream(messages, tools, system.as_deref())
                    .await
            }
            InnerClient::OpenAIResponses(c) => {
                c.send_messages_stream(messages, tools, system.as_deref())
                    .await
            }
            InnerClient::Gemini(c) => {
                c.send_messages_stream(messages, tools, system.as_deref())
                    .await
            }
            InnerClient::ChatCompletions(c) => {
                c.send_messages_stream(messages, tools, system.as_deref())
                    .await
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_route_from_registry_api_hint() {
        assert_eq!(
            ApiyiRoute::from_registry_api_hint("google-generative-ai"),
            Some(ApiyiRoute::GoogleGenerativeAI)
        );
        assert_eq!(
            ApiyiRoute::from_registry_api_hint("openai-completions"),
            Some(ApiyiRoute::OpenAICompletions)
        );
        assert_eq!(ApiyiRoute::from_registry_api_hint("unknown"), None);
    }

    #[test]
    fn test_resolve_route_defaults_to_openai_completions_when_missing_hint() {
        assert_eq!(
            resolve_apiyi_route("non-existent-model"),
            ApiyiRoute::OpenAICompletions
        );
    }
}
