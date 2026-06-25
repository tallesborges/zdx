//! `OpenCode` Go provider — meta-provider that routes to the appropriate
//! API client based on the model registry hint.

use anyhow::Result;
use reqwest::header::HeaderMap;
use zdx_types::ToolDefinition;

use crate::anthropic::api::{AnthropicClient, AnthropicConfig};
use crate::anthropic::types::EffortLevel as AnthropicEffortLevel;
use crate::gemini::api::{GeminiClient, GeminiConfig};
use crate::gemini::shared::GeminiThinkingConfig;
use crate::openai::api::{OpenAIClient, OpenAIConfig};
use crate::openai::chat_completions::{OpenAIChatCompletionsClient, OpenAIChatCompletionsConfig};
use crate::shared::merge_system_prompt;
use crate::{ProviderKind, ProviderStream};

#[derive(Debug, Clone)]
pub struct OpencodeGoConfig {
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
    /// API routing hint from the model registry (e.g. "anthropic-messages").
    pub api_hint: Option<String>,
}

impl OpencodeGoConfig {
    /// Creates a new `OpencodeGoConfig` from environment variables and provided parameters.
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
        api_hint: Option<String>,
    ) -> Result<Self> {
        let api_key = ProviderKind::OpencodeGo.resolve_api_key(config_api_key)?;
        let base_url = ProviderKind::OpencodeGo.resolve_base_url(config_base_url)?;

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
            api_hint,
        })
    }
}

enum InnerClient {
    Anthropic(AnthropicClient),
    OpenAIResponses(OpenAIClient),
    Gemini(GeminiClient),
    ChatCompletions(Box<OpenAIChatCompletionsClient>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GoRoute {
    AnthropicMessages,
    OpenAIResponses,
    GoogleGenerativeAI,
    OpenAICompletions,
}

impl GoRoute {
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

fn resolve_go_route(api_hint: Option<&str>) -> GoRoute {
    api_hint
        .and_then(GoRoute::from_registry_api_hint)
        .unwrap_or(GoRoute::OpenAICompletions)
}

/// `OpenCode` Go meta-provider that routes requests to the appropriate API
/// client based on the model registry hint.
pub struct OpencodeGoClient {
    inner: InnerClient,
}

impl OpencodeGoClient {
    /// Creates a new `OpencodeGoClient`, selecting the inner provider based on the registry hint.
    pub fn new(config: OpencodeGoConfig) -> Self {
        let route = resolve_go_route(config.api_hint.as_deref());
        let inner = match route {
            GoRoute::AnthropicMessages => {
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
            GoRoute::OpenAIResponses => {
                // OpenAI Responses API — base_url as-is (client uses {base}/v1/responses)
                InnerClient::OpenAIResponses(OpenAIClient::new(OpenAIConfig {
                    api_key: config.api_key,
                    base_url: config.base_url,
                    model: config.model,
                    max_output_tokens: config.max_tokens,
                    reasoning_effort: config.reasoning_effort,
                    text_verbosity: None,
                    prompt_cache_key: config.cache_key,
                    service_tier: None,
                    websocket: false,
                }))
            }
            GoRoute::GoogleGenerativeAI => {
                // Gemini API — append /v1 (client appends /models/{model}:stream...)
                InnerClient::Gemini(GeminiClient::new(GeminiConfig {
                    api_key: config.api_key,
                    base_url: format!("{}/v1", config.base_url),
                    model: config.model,
                    max_output_tokens: config.max_tokens,
                    thinking_config: config.gemini_thinking,
                }))
            }
            GoRoute::OpenAICompletions => {
                // Chat Completions — append /v1 (client appends /chat/completions)
                // The OpenCode proxy rejects `reasoning` and `prompt_cache_key`, so omit those.
                // Reasoning models (e.g. Kimi) need `thinking` + `include_reasoning_content`
                // so `reasoning_content` round-trips in assistant messages.
                InnerClient::ChatCompletions(Box::new(OpenAIChatCompletionsClient::new(
                    OpenAIChatCompletionsConfig {
                        api_key: config.api_key,
                        base_url: format!("{}/v1", config.base_url),
                        model: config.model,
                        max_tokens: config.max_tokens,
                        max_completion_tokens: None,
                        reasoning_effort: None,
                        prompt_cache_key: None,
                        extra_headers: HeaderMap::new(),
                        include_usage: true,
                        include_reasoning_content: config.thinking_enabled,
                        thinking: config
                            .thinking_enabled
                            .then(|| config.thinking_enabled.into()),
                    },
                )))
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
        messages: &[crate::ChatMessage],
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
            GoRoute::from_registry_api_hint("openai-responses"),
            Some(GoRoute::OpenAIResponses)
        );
        assert_eq!(
            GoRoute::from_registry_api_hint("anthropic-messages"),
            Some(GoRoute::AnthropicMessages)
        );
        assert_eq!(GoRoute::from_registry_api_hint("unknown"), None);
    }

    #[test]
    fn test_resolve_route_defaults_to_openai_completions_when_missing_hint() {
        assert_eq!(resolve_go_route(None), GoRoute::OpenAICompletions);
    }
}
