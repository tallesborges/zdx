//! Anthropic API key provider (Messages API).

use anyhow::Result;

use super::shared::{
    build_api_messages_with_cache_control, build_system_blocks, build_thinking_and_output_config,
    build_tool_defs, send_streaming_request, should_enable_interleaved_thinking_beta,
};
use super::types::{EffortLevel, StreamingMessagesRequest};
use crate::providers::shared::{ChatMessage, ProviderStream, resolve_api_key, resolve_base_url};
use crate::tools::ToolDefinition;

/// Default base URL for the Anthropic API.
pub const DEFAULT_BASE_URL: &str = "https://api.anthropic.com";

const API_VERSION: &str = "2023-06-01";
const INTERLEAVED_BETA_HEADER: &str = "interleaved-thinking-2025-05-14";

/// Configuration for the Anthropic client.
#[derive(Debug, Clone)]
pub struct AnthropicConfig {
    /// The authentication token (API key)
    pub api_key: String,
    pub base_url: String,
    pub model: String,
    pub max_tokens: u32,
    /// Whether extended thinking is enabled
    pub thinking_enabled: bool,
    /// Token budget for thinking (only used when `thinking_enabled` = true)
    pub thinking_budget_tokens: u32,
    /// Optional effort level for supported models
    pub thinking_effort: Option<EffortLevel>,
}

impl AnthropicConfig {
    /// Creates a new config from environment.
    ///
    /// Authentication resolution order:
    /// 1. `config_api_key` parameter (from config file)
    /// 2. `ANTHROPIC_API_KEY` environment variable
    ///
    /// Environment variables:
    /// - `ANTHROPIC_API_KEY`: API key (fallback if not in config)
    /// - `ANTHROPIC_BASE_URL`: Optional base URL override
    ///
    /// Base URL resolution order:
    /// 1. `ANTHROPIC_BASE_URL` env var (if set and non-empty)
    /// 2. `config_base_url` parameter (if Some and non-empty)
    /// 3. Default: `https://api.anthropic.com`
    ///
    /// # Errors
    /// Returns an error if the operation fails.
    pub fn from_env(
        model: String,
        max_tokens: u32,
        config_base_url: Option<&str>,
        config_api_key: Option<&str>,
        thinking_enabled: bool,
        thinking_budget_tokens: u32,
        thinking_effort: Option<EffortLevel>,
    ) -> Result<Self> {
        let api_key = resolve_api_key(config_api_key, "ANTHROPIC_API_KEY", "anthropic")?;
        let base_url = resolve_base_url(
            config_base_url,
            "ANTHROPIC_BASE_URL",
            DEFAULT_BASE_URL,
            "Anthropic",
        )?;

        Ok(Self {
            api_key,
            base_url,
            model,
            max_tokens,
            thinking_enabled,
            thinking_budget_tokens,
            thinking_effort,
        })
    }
}

/// Anthropic API client.
pub struct AnthropicClient {
    config: AnthropicConfig,
    http: reqwest::Client,
}

impl AnthropicClient {
    /// Creates a new Anthropic client with the given configuration.
    ///
    /// # Panics
    /// - In test builds (`#[cfg(test)]`), panics if `base_url` is the production API.
    /// - At runtime, panics if `ZDX_BLOCK_REAL_API=1` and `base_url` is the production API.
    ///
    /// This prevents tests from accidentally making real network requests.
    /// Use `ANTHROPIC_BASE_URL` env var or config to point to a mock server.
    pub fn new(config: AnthropicConfig) -> Self {
        // Compile-time guard for unit tests
        #[cfg(test)]
        assert!(
            (config.base_url != DEFAULT_BASE_URL),
            "Tests must not use the production Anthropic API!\n\
                 Set ANTHROPIC_BASE_URL to a mock server (e.g., wiremock).\n\
                 Found base_url: {}",
            config.base_url
        );

        // Runtime guard for integration tests (set ZDX_BLOCK_REAL_API=1 in test harness)
        #[cfg(not(test))]
        if std::env::var("ZDX_BLOCK_REAL_API").is_ok_and(|v| v == "1")
            && config.base_url == DEFAULT_BASE_URL
        {
            panic!(
                "ZDX_BLOCK_REAL_API=1 but trying to use production Anthropic API!\n\
                 Set ANTHROPIC_BASE_URL to a mock server.\n\
                 Found base_url: {}",
                config.base_url
            );
        }

        Self {
            config,
            http: reqwest::Client::new(),
        }
    }

    /// Sends a thread and returns an async stream of events.
    ///
    /// This enables chunk-by-chunk token streaming from the API.
    ///
    /// # Errors
    /// Returns an error if the operation fails.
    pub async fn send_messages_stream(
        &self,
        messages: &[ChatMessage],
        tools: &[ToolDefinition],
        system: Option<&str>,
    ) -> Result<ProviderStream> {
        let request = self.build_streaming_request(messages, tools, system)?;
        let include_interleaved_beta = should_enable_interleaved_thinking_beta(
            &self.config.model,
            self.config.thinking_enabled,
        );

        let url = format!("{}/v1/messages", self.config.base_url);

        send_streaming_request(&self.http, &url, &request, |builder| {
            let builder = builder
                .header("anthropic-version", API_VERSION)
                .header("x-api-key", &self.config.api_key);

            if include_interleaved_beta {
                builder.header("anthropic-beta", INTERLEAVED_BETA_HEADER)
            } else {
                builder
            }
        })
        .await
    }

    fn build_streaming_request<'a>(
        &'a self,
        messages: &[ChatMessage],
        tools: &'a [ToolDefinition],
        system: Option<&str>,
    ) -> Result<StreamingMessagesRequest<'a>> {
        // Convert messages to API format.
        // Only the last content block of the last user message gets cache_control
        // to respect Anthropic's limit of 4 cache_control blocks total.
        let api_messages = build_api_messages_with_cache_control(messages);

        let tool_defs = build_tool_defs(tools);

        let system_blocks = build_system_blocks(system, None);

        let (thinking, output_config) = build_thinking_and_output_config(
            &self.config.model,
            self.config.thinking_enabled,
            self.config.thinking_budget_tokens,
            self.config.thinking_effort,
        )?;

        let request = StreamingMessagesRequest {
            model: &self.config.model,
            max_tokens: self.config.max_tokens,
            messages: api_messages,
            tools: tool_defs,
            system: system_blocks,
            thinking,
            output_config,
            stream: true,
        };

        Ok(request)
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn build_request_for_opus_46_uses_adaptive_thinking() {
        let config = AnthropicConfig {
            api_key: "test-key".to_string(),
            base_url: "http://mock-server".to_string(),
            model: "claude-opus-4-6".to_string(),
            max_tokens: 4096,
            thinking_enabled: true,
            thinking_budget_tokens: 2048,
            thinking_effort: Some(EffortLevel::High),
        };
        let client = AnthropicClient::new(config);

        let request = client
            .build_streaming_request(&[ChatMessage::user("hi")], &[], None)
            .unwrap();

        let payload = serde_json::to_value(&request).unwrap();
        assert_eq!(payload["thinking"], json!({"type": "adaptive"}));
        assert_eq!(payload["output_config"], json!({"effort": "high"}));
    }

    #[test]
    fn build_request_for_legacy_model_keeps_budget_thinking() {
        let config = AnthropicConfig {
            api_key: "test-key".to_string(),
            base_url: "http://mock-server".to_string(),
            model: "claude-opus-4-5".to_string(),
            max_tokens: 4096,
            thinking_enabled: true,
            thinking_budget_tokens: 1024,
            thinking_effort: Some(EffortLevel::Medium),
        };
        let client = AnthropicClient::new(config);

        let request = client
            .build_streaming_request(&[ChatMessage::user("hi")], &[], None)
            .unwrap();

        let payload = serde_json::to_value(&request).unwrap();
        assert_eq!(
            payload["thinking"],
            json!({"type": "enabled", "budget_tokens": 1024})
        );
        assert_eq!(payload["output_config"], json!({"effort": "medium"}));
    }
}
