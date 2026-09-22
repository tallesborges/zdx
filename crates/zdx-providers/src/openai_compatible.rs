//! Generic OpenAI-compatible Chat Completions client for user-defined
//! (`[providers.custom.<name>]`) endpoints such as a self-hosted `LiteLLM`
//! proxy. Carries no [`crate::ProviderKind`]; the engine builds it directly
//! from a resolved base URL + API key. Mirrors the `LMStudio` passthrough.

use std::collections::HashMap;

use anyhow::Result;
use reqwest::header::HeaderMap;
use serde_json::json;
use zdx_types::{ThinkingLevel, ToolDefinition};

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
    thinking_level: ThinkingLevel,
) -> Box<dyn crate::StreamingProvider> {
    let mut extra_body = HashMap::new();
    extra_body.insert(
        "reasoning_effort".to_string(),
        json!(reasoning_effort_from_thinking_level(thinking_level)),
    );
    Box::new(OpenAICompatibleClient {
        inner: OpenAIChatCompletionsClient::with_extra_body(
            chat_completions_config(
                base_url,
                api_key,
                model,
                max_tokens,
                prompt_cache_key,
                thinking_level,
            ),
            extra_body,
        ),
    })
}

fn chat_completions_config(
    base_url: String,
    api_key: String,
    model: String,
    max_tokens: Option<u32>,
    prompt_cache_key: Option<String>,
    thinking_level: ThinkingLevel,
) -> OpenAIChatCompletionsConfig {
    OpenAIChatCompletionsConfig {
        api_key,
        base_url,
        model,
        max_tokens,
        max_completion_tokens: None,
        // Custom endpoints are proxies (e.g. LiteLLM) that expect a
        // DeepSeek-style top-level `reasoning_effort` string, not
        // OpenAI's nested `reasoning: {effort}` object.
        reasoning_effort: None,
        prompt_cache_key,
        extra_headers: HeaderMap::new(),
        include_usage: true,
        include_reasoning_content: thinking_level.is_enabled(),
        replay_historical_tool_turns: false,
        thinking: None,
    }
}

/// Maps a ZDX thinking level to the `reasoning_effort` sent to a custom
/// endpoint. The level name passes through unchanged: custom providers front
/// models with their own effort vocabulary (a vLLM-hosted `DeepSeek` V4
/// distinguishes `max` from `xhigh`), so collapsing levels here would silently
/// downgrade the request. `off` sends `none`: reasoning backends behind these
/// proxies (e.g. `DeepSeek`) think by default, so omitting the field would
/// leave thinking on, and `none` is the disable spelling a `LiteLLM`-style
/// proxy translates for its backend (`DeepSeek`'s own `thinking.type =
/// "disabled"` is dropped by such proxies; verified live 2026-09-17).
fn reasoning_effort_from_thinking_level(level: ThinkingLevel) -> &'static str {
    match level {
        ThinkingLevel::Off => "none",
        ThinkingLevel::Low => "low",
        ThinkingLevel::Medium => "medium",
        ThinkingLevel::High => "high",
        ThinkingLevel::XHigh => "xhigh",
        ThinkingLevel::Max => "max",
    }
}

#[cfg(test)]
mod tests {
    use zdx_types::ThinkingLevel;

    use super::{chat_completions_config, reasoning_effort_from_thinking_level};

    #[test]
    fn custom_reasoning_effort_passes_levels_through_and_disables_with_none() {
        for (level, expected) in [
            (ThinkingLevel::Off, "none"),
            (ThinkingLevel::Low, "low"),
            (ThinkingLevel::Medium, "medium"),
            (ThinkingLevel::High, "high"),
            (ThinkingLevel::XHigh, "xhigh"),
            (ThinkingLevel::Max, "max"),
        ] {
            assert_eq!(reasoning_effort_from_thinking_level(level), expected);
        }
    }

    #[test]
    fn custom_mimo_name_does_not_enable_historical_replay() {
        let config = chat_completions_config(
            "https://example.test/v1".to_string(),
            "test-key".to_string(),
            "xiaomi/mimo-v2.6-pro".to_string(),
            Some(4096),
            None,
            ThinkingLevel::High,
        );
        assert!(!config.replay_historical_tool_turns);
    }
}
