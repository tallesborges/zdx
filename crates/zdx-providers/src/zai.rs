//! Z.AI provider (GLM) using OpenAI-compatible API.

use std::collections::HashMap;

use anyhow::Result;
use reqwest::header::HeaderMap;
use serde_json::{Value, json};
use zdx_types::{ThinkingLevel, ToolDefinition};

use crate::openai::chat_completions::{
    OpenAIChatCompletionsClient, OpenAIChatCompletionsConfig, ThinkingConfig,
};
use crate::shared::merge_system_prompt;
use crate::{ChatMessage, ProviderKind, ProviderStream};

/// `Z.AI` API configuration.
#[derive(Debug, Clone)]
pub struct ZaiConfig {
    pub api_key: String,
    pub base_url: String,
    pub model: String,
    pub max_tokens: Option<u32>,
    pub prompt_cache_key: Option<String>,
    pub thinking_level: ThinkingLevel,
}

impl ZaiConfig {
    /// Creates a new config from environment.
    ///
    /// Authentication resolution order:
    /// 1. `config_api_key` parameter (from config file)
    /// 2. `ZAI_API_KEY` environment variable
    ///
    /// Environment variables:
    /// - `ZAI_API_KEY` (fallback if not in config)
    /// - `ZAI_BASE_URL` (optional)
    ///
    /// # Errors
    /// Returns an error if the operation fails.
    pub fn from_env(
        model: String,
        max_tokens: Option<u32>,
        config_base_url: Option<&str>,
        config_api_key: Option<&str>,
        prompt_cache_key: Option<String>,
        thinking_level: ThinkingLevel,
    ) -> Result<Self> {
        let api_key = ProviderKind::Zai.resolve_api_key(config_api_key)?;
        let base_url = ProviderKind::Zai.resolve_base_url(config_base_url)?;

        Ok(Self {
            api_key,
            base_url,
            model,
            max_tokens,
            prompt_cache_key,
            thinking_level,
        })
    }
}

/// `Z.AI` client using OpenAI-compatible API.
pub struct ZaiClient {
    inner: OpenAIChatCompletionsClient,
}

impl ZaiClient {
    pub fn new(config: ZaiConfig) -> Self {
        let (chat_config, extra_body) = zai_chat_config(config);
        Self {
            inner: OpenAIChatCompletionsClient::with_extra_body(chat_config, extra_body),
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

/// Request body extras for one model + level.
///
/// The GLM-5.3 family always reasons: it rejects `thinking: {"type":
/// "disabled"}` with HTTP 400 (code 1210) and accepts only the `low`, `high`,
/// and `max` effort values (verified against api.z.ai on 2026-09-15). Those
/// models therefore get no thinking toggle and carry the level as a top-level
/// `reasoning_effort` instead; every other model keeps the toggle, where
/// `disabled` is the documented way to turn reasoning off.
fn zai_chat_config(config: ZaiConfig) -> (OpenAIChatCompletionsConfig, HashMap<String, Value>) {
    let thinking_enabled = config.thinking_level.is_enabled();
    let forced = forces_thinking(&config.model);
    let mut extra_body = HashMap::new();
    let thinking: Option<ThinkingConfig> = if forced {
        if let Some(effort) = forced_model_effort(config.thinking_level) {
            extra_body.insert("reasoning_effort".to_string(), json!(effort));
        }
        None
    } else {
        Some(thinking_enabled.into())
    };

    (
        OpenAIChatCompletionsConfig {
            api_key: config.api_key,
            base_url: config.base_url,
            model: config.model,
            max_tokens: config.max_tokens,
            max_completion_tokens: None,
            reasoning_effort: None,
            prompt_cache_key: config.prompt_cache_key,
            extra_headers: HeaderMap::new(),
            include_usage: true,
            // A forced-thinking model is always in thinking mode, so its
            // reasoning round-trips through history the same way an
            // enabled toggle does elsewhere.
            include_reasoning_content: thinking_enabled || forced,
            replay_historical_tool_turns: false,
            thinking,
        },
        extra_body,
    )
}

/// True for the models that cannot stop reasoning (GLM-5.3 and GLM-5.3-Flash).
fn forces_thinking(model: &str) -> bool {
    matches!(bare_model_id(model), "glm-5.3" | "glm-5.3-flash")
}

/// Effort vocabulary the GLM-5.3 family accepts: `low | high | max`.
fn forced_model_effort(level: ThinkingLevel) -> Option<&'static str> {
    match level {
        ThinkingLevel::Off => None,
        ThinkingLevel::Low => Some("low"),
        ThinkingLevel::Medium | ThinkingLevel::High | ThinkingLevel::XHigh => Some("high"),
        ThinkingLevel::Max => Some("max"),
    }
}

/// Model id with any `provider:` prefix stripped.
fn bare_model_id(model: &str) -> &str {
    model.rsplit(':').next().unwrap_or(model)
}

/// Constructs the Z.AI client from the given context.
///
/// # Errors
/// Returns an error if the API key / base URL cannot be resolved from env or config.
pub fn build(
    ctx: &crate::ProviderBuildContext<'_>,
) -> anyhow::Result<Box<dyn crate::StreamingProvider>> {
    Ok(Box::new(ZaiClient::new(ZaiConfig::from_env(
        ctx.model.to_string(),
        ctx.config_max_tokens,
        ctx.base_url,
        ctx.api_key,
        ctx.cache_key.clone(),
        ctx.thinking_level,
    )?)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(model: &str, level: ThinkingLevel) -> ZaiConfig {
        ZaiConfig {
            api_key: "test-key".to_string(),
            base_url: "https://api.z.ai/api/paas/v4".to_string(),
            model: model.to_string(),
            max_tokens: Some(4096),
            prompt_cache_key: None,
            thinking_level: level,
        }
    }

    #[test]
    fn glm_5_3_carries_effort_instead_of_a_thinking_toggle() {
        for model in ["glm-5.3", "glm-5.3-flash", "zai:glm-5.3"] {
            let (chat, extra) = zai_chat_config(config(model, ThinkingLevel::Max));
            assert!(
                chat.thinking.is_none(),
                "{model} must not get a thinking field"
            );
            assert_eq!(extra.get("reasoning_effort"), Some(&json!("max")));
            assert!(chat.include_reasoning_content, "{model} always reasons");
        }

        let (chat, extra) = zai_chat_config(config("glm-5.3", ThinkingLevel::Off));
        assert!(chat.thinking.is_none());
        assert!(extra.is_empty(), "off sends no effort and no toggle");
        assert!(
            chat.include_reasoning_content,
            "a forced-thinking model still round-trips reasoning"
        );
    }

    #[test]
    fn glm_5_3_effort_mapping_uses_the_documented_vocabulary() {
        assert_eq!(forced_model_effort(ThinkingLevel::Off), None);
        assert_eq!(forced_model_effort(ThinkingLevel::Low), Some("low"));
        assert_eq!(forced_model_effort(ThinkingLevel::Medium), Some("high"));
        assert_eq!(forced_model_effort(ThinkingLevel::High), Some("high"));
        assert_eq!(forced_model_effort(ThinkingLevel::XHigh), Some("high"));
        assert_eq!(forced_model_effort(ThinkingLevel::Max), Some("max"));
    }

    #[test]
    fn other_models_keep_the_thinking_toggle() {
        let (chat, extra) = zai_chat_config(config("glm-5.2", ThinkingLevel::Off));
        assert_eq!(
            chat.thinking.as_ref().map(|t| t.kind.as_str()),
            Some("disabled")
        );
        assert!(extra.is_empty());
        assert!(!chat.include_reasoning_content);

        let (chat, _) = zai_chat_config(config("glm-5.2", ThinkingLevel::High));
        assert_eq!(
            chat.thinking.as_ref().map(|t| t.kind.as_str()),
            Some("enabled")
        );
        assert!(chat.include_reasoning_content);
    }
}
