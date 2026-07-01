//! Generic OpenAI-compatible Chat Completions client for user-defined
//! (`[providers.custom.<name>]`) endpoints such as a self-hosted `LiteLLM`
//! proxy. Carries no [`crate::ProviderKind`]; the engine builds it directly
//! from a resolved base URL + API key. Mirrors the `LMStudio` passthrough.

use anyhow::Result;
use reqwest::header::HeaderMap;
use zdx_types::ToolDefinition;

use crate::openai::chat_completions::{OpenAIChatCompletionsClient, OpenAIChatCompletionsConfig};
use crate::shared::merge_system_prompt;
use crate::{ChatMessage, ProviderStream};

/// Generic OpenAI-compatible chat-completions client.
pub struct OpenAICompatibleClient {
    inner: OpenAIChatCompletionsClient,
}

impl OpenAICompatibleClient {
    /// # Errors
    /// Returns an error if the request fails.
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

/// Builds a generic OpenAI-compatible client for a user-defined custom provider.
#[must_use]
pub fn build_custom(
    base_url: String,
    api_key: String,
    model: String,
    max_tokens: Option<u32>,
    prompt_cache_key: Option<String>,
    thinking_enabled: bool,
) -> Box<dyn crate::StreamingProvider> {
    Box::new(OpenAICompatibleClient {
        inner: OpenAIChatCompletionsClient::new(OpenAIChatCompletionsConfig {
            api_key,
            base_url,
            model,
            max_tokens,
            max_completion_tokens: None,
            reasoning_effort: None,
            prompt_cache_key,
            extra_headers: HeaderMap::new(),
            include_usage: true,
            include_reasoning_content: thinking_enabled,
            thinking: None,
        }),
    })
}
