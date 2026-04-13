//! xAI provider (Grok) using the Responses API.

use anyhow::Result;
use reqwest::header::{HeaderMap, HeaderValue};
use zdx_types::ToolDefinition;

use crate::openai::responses::{ResponsesConfig, send_responses_stream};
use crate::shared::merge_system_prompt;
use crate::{ChatMessage, ProviderKind, ProviderStream};

const RESPONSES_PATH: &str = "/responses";

/// `xAI` API configuration.
#[derive(Debug, Clone)]
pub struct XaiConfig {
    pub api_key: String,
    pub base_url: String,
    pub model: String,
    pub max_tokens: Option<u32>,
    pub prompt_cache_key: Option<String>,
    pub thinking_enabled: bool,
}

impl XaiConfig {
    /// Creates a new config from environment.
    ///
    /// Authentication resolution order:
    /// 1. `config_api_key` parameter (from config file)
    /// 2. `XAI_API_KEY` environment variable
    ///
    /// Environment variables:
    /// - `XAI_API_KEY` (fallback if not in config)
    /// - `XAI_BASE_URL` (optional)
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
        let api_key = ProviderKind::Xai.resolve_api_key(config_api_key)?;
        let base_url = ProviderKind::Xai.resolve_base_url(config_base_url)?;

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

fn build_headers(api_key: &str) -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert(
        "Authorization",
        HeaderValue::from_str(&format!("Bearer {api_key}"))
            .unwrap_or_else(|_| HeaderValue::from_static("")),
    );
    headers.insert("accept", HeaderValue::from_static("text/event-stream"));
    headers.insert("content-type", HeaderValue::from_static("application/json"));
    headers.insert(
        "user-agent",
        HeaderValue::from_static(crate::shared::USER_AGENT),
    );
    headers
}

/// `xAI` client using the Responses API.
pub struct XaiClient {
    api_key: String,
    config: ResponsesConfig,
    http: reqwest::Client,
}

impl XaiClient {
    pub fn new(config: XaiConfig) -> Self {
        Self {
            api_key: config.api_key,
            config: ResponsesConfig {
                base_url: config.base_url,
                path: RESPONSES_PATH.to_string(),
                model: config.model,
                max_output_tokens: config.max_tokens,
                reasoning_effort: None,
                reasoning_summary: None,
                instructions: None,
                text_verbosity: None,
                store: Some(false),
                include: Some(vec!["reasoning.encrypted_content".to_string()]),
                stream_options: None,
                prompt_cache_key: config.prompt_cache_key,
                parallel_tool_calls: Some(true),
                tool_choice: Some("auto".to_string()),
                truncation: None,
            },
            http: reqwest::Client::new(),
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
        send_responses_stream(
            &self.http,
            &self.config,
            build_headers(&self.api_key),
            messages,
            tools,
            system.as_deref(),
        )
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::{RESPONSES_PATH, XaiClient, XaiConfig};

    fn test_config(model: &str) -> XaiConfig {
        XaiConfig {
            api_key: "test-key".to_string(),
            base_url: "https://api.x.ai/v1".to_string(),
            model: model.to_string(),
            max_tokens: Some(1024),
            prompt_cache_key: Some("thread-123".to_string()),
            thinking_enabled: true,
        }
    }

    #[test]
    fn xai_provider_uses_responses_api_for_current_models() {
        for model in [
            "grok-4.20-experimental-beta-0304-reasoning",
            "grok-4-1-fast-reasoning",
            "grok-4-1-fast-non-reasoning",
        ] {
            let client = XaiClient::new(test_config(model));
            assert_eq!(client.config.path, RESPONSES_PATH);
            assert_eq!(client.config.model, model);
            assert_eq!(
                client.config.include.as_ref(),
                Some(&vec!["reasoning.encrypted_content".to_string()])
            );
        }
    }
}
