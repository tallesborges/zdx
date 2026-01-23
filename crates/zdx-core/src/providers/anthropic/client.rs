use anyhow::Result;

use super::auth::{AnthropicConfig, DEFAULT_BASE_URL};
use super::types::StreamingMessagesRequest;
use super::{
    build_api_messages_with_cache_control, build_system_blocks, build_thinking_config,
    build_tool_defs, send_streaming_request,
};
use crate::providers::shared::{ChatMessage, ProviderStream};
use crate::tools::ToolDefinition;

const API_VERSION: &str = "2023-06-01";
/// Beta features for API key authentication
const BETA_HEADER: &str = "fine-grained-tool-streaming-2025-05-14,interleaved-thinking-2025-05-14";

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
        if config.base_url == DEFAULT_BASE_URL {
            panic!(
                "Tests must not use the production Anthropic API!\n\
                 Set ANTHROPIC_BASE_URL to a mock server (e.g., wiremock).\n\
                 Found base_url: {}",
                config.base_url
            );
        }

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
    pub async fn send_messages_stream(
        &self,
        messages: &[ChatMessage],
        tools: &[ToolDefinition],
        system: Option<&str>,
    ) -> Result<ProviderStream> {
        // Convert messages to API format.
        // Only the last content block of the last user message gets cache_control
        // to respect Anthropic's limit of 4 cache_control blocks total.
        let api_messages = build_api_messages_with_cache_control(messages);

        let tool_defs = build_tool_defs(tools);

        let system_blocks = build_system_blocks(system, None);

        // Build thinking config if enabled
        let thinking = build_thinking_config(
            self.config.thinking_enabled,
            self.config.thinking_budget_tokens,
        );

        let request = StreamingMessagesRequest {
            model: &self.config.model,
            max_tokens: self.config.max_tokens,
            messages: api_messages,
            tools: tool_defs,
            system: system_blocks,
            thinking,
            stream: true,
        };

        let url = format!("{}/v1/messages", self.config.base_url);

        send_streaming_request(&self.http, &url, &request, |builder| {
            builder
                .header("anthropic-version", API_VERSION)
                .header("x-api-key", &self.config.api_key)
                .header("anthropic-beta", BETA_HEADER)
        })
        .await
    }
}
