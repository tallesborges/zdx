//! Zen provider (`OpenCode` Zen) — meta-provider that routes to the appropriate
//! API client based on the model name.

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

const DEFAULT_BASE_URL: &str = "https://opencode.ai/zen";

#[derive(Debug, Clone)]
pub struct ZenConfig {
    pub api_key: String,
    pub base_url: String,
    pub model: String,
    pub max_tokens: u32,
    pub thinking_enabled: bool,
    pub thinking_budget_tokens: u32,
    pub thinking_effort: Option<AnthropicEffortLevel>,
    pub gemini_thinking: Option<GeminiThinkingConfig>,
    pub reasoning_effort: Option<String>,
    pub cache_key: Option<String>,
}

impl ZenConfig {
    /// Creates a new `ZenConfig` from environment variables and provided parameters.
    ///
    /// # Errors
    /// Returns an error if the API key or base URL cannot be resolved.
    #[allow(clippy::too_many_arguments)]
    pub fn from_env(
        model: String,
        max_tokens: u32,
        config_base_url: Option<&str>,
        config_api_key: Option<&str>,
        thinking_enabled: bool,
        thinking_budget_tokens: u32,
        thinking_effort: Option<AnthropicEffortLevel>,
        gemini_thinking: Option<GeminiThinkingConfig>,
        reasoning_effort: Option<String>,
        cache_key: Option<String>,
    ) -> Result<Self> {
        let api_key = resolve_api_key(config_api_key, "ZEN_API_KEY", "zen")?;
        let base_url = resolve_base_url(config_base_url, "ZEN_BASE_URL", DEFAULT_BASE_URL, "Zen")?;

        Ok(Self {
            api_key,
            base_url,
            model,
            max_tokens,
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

/// `OpenCode` Zen meta-provider that routes requests to the appropriate API
/// client based on the model name.
pub struct ZenClient {
    inner: InnerClient,
}

impl ZenClient {
    /// Creates a new `ZenClient`, selecting the inner provider based on model name.
    pub fn new(config: ZenConfig) -> Self {
        let model = &config.model;
        let inner = if model.starts_with("claude") {
            // Anthropic Messages API — base_url as-is (client appends /v1/messages)
            InnerClient::Anthropic(AnthropicClient::new(AnthropicConfig {
                api_key: config.api_key,
                base_url: config.base_url,
                model: config.model,
                max_tokens: config.max_tokens,
                thinking_enabled: config.thinking_enabled,
                thinking_budget_tokens: config.thinking_budget_tokens,
                thinking_effort: config.thinking_effort,
            }))
        } else if model.starts_with("gpt")
            || model.starts_with("o1")
            || model.starts_with("o3")
            || model.starts_with("o4")
        {
            // OpenAI Responses API — base_url as-is (client uses {base}/v1/responses)
            InnerClient::OpenAIResponses(OpenAIClient::new(OpenAIConfig {
                api_key: config.api_key,
                base_url: config.base_url,
                model: config.model,
                max_output_tokens: config.max_tokens,
                prompt_cache_key: config.cache_key,
            }))
        } else if model.starts_with("gemini") {
            // Gemini API — append /v1 (client appends /models/{model}:stream...)
            InnerClient::Gemini(GeminiClient::new(GeminiConfig {
                api_key: config.api_key,
                base_url: format!("{}/v1", config.base_url),
                model: config.model,
                max_output_tokens: config.max_tokens,
                thinking_config: config.gemini_thinking,
            }))
        } else {
            // Chat Completions — append /v1 (client appends /chat/completions)
            InnerClient::ChatCompletions(OpenAIChatCompletionsClient::new(
                OpenAIChatCompletionsConfig {
                    api_key: config.api_key,
                    base_url: format!("{}/v1", config.base_url),
                    model: config.model,
                    max_tokens: Some(config.max_tokens),
                    max_completion_tokens: None,
                    reasoning_effort: config.reasoning_effort,
                    prompt_cache_key: config.cache_key,
                    extra_headers: HeaderMap::new(),
                    include_usage: true,
                    include_reasoning_content: false,
                    thinking: None,
                },
            ))
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
