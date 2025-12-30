use std::pin::Pin;

use anyhow::Result;
use futures_util::Stream;

use super::auth::{AnthropicConfig, AuthMethod, DEFAULT_BASE_URL};
use super::errors::{ProviderError, ProviderErrorKind};
use super::sse::{SseParser, StreamEvent};
use super::types::{
    ApiContentBlock, ApiMessage, ApiMessageContent, ApiToolDef, CacheControl, ChatMessage,
    StreamingMessagesRequest, SystemBlock, ThinkingConfig,
};
use crate::tools::ToolDefinition;

const API_VERSION: &str = "2023-06-01";
/// Beta features for API key authentication
const BETA_HEADER: &str = "fine-grained-tool-streaming-2025-05-14,interleaved-thinking-2025-05-14";
/// Beta features for OAuth authentication
const OAUTH_BETA_HEADER: &str =
    "oauth-2025-04-20,fine-grained-tool-streaming-2025-05-14,interleaved-thinking-2025-05-14";
/// Required system prompt prefix for OAuth tokens (Claude Code identification)
const CLAUDE_CODE_SYSTEM_PROMPT: &str = "You are Claude Code, Anthropic's official CLI for Claude.";

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

    /// Sends a conversation and returns an async stream of events.
    ///
    /// This enables chunk-by-chunk token streaming from the API.
    pub async fn send_messages_stream(
        &self,
        messages: &[ChatMessage],
        tools: &[ToolDefinition],
        system: Option<&str>,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<StreamEvent>> + Send>>> {
        // Convert messages to API format.
        // Only the last content block of the last user message gets cache_control
        // to respect Anthropic's limit of 4 cache_control blocks total.
        let mut api_messages: Vec<ApiMessage> = messages
            .iter()
            .map(|m| ApiMessage::from_chat_message(m, false))
            .collect();

        // Add cache_control to the last content block of the last user message
        if let Some(last_user_msg) = api_messages.iter_mut().rev().find(|m| m.role == "user")
            && let ApiMessageContent::Blocks(blocks) = &mut last_user_msg.content
            && let Some(last_block) = blocks.last_mut()
        {
            match last_block {
                ApiContentBlock::Text { cache_control, .. } => {
                    *cache_control = Some(CacheControl::ephemeral());
                }
                ApiContentBlock::ToolResult { cache_control, .. } => {
                    *cache_control = Some(CacheControl::ephemeral());
                }
                _ => {}
            }
        }

        let tool_defs = if tools.is_empty() {
            None
        } else {
            Some(tools.iter().map(ApiToolDef::from).collect::<Vec<_>>())
        };

        // Build system blocks based on auth type
        // OAuth requires the Claude Code system prompt prefix with cache_control
        let system_blocks = self.build_system_blocks(system);

        // Build thinking config if enabled
        let thinking = if self.config.thinking_enabled {
            Some(ThinkingConfig::enabled(self.config.thinking_budget_tokens))
        } else {
            None
        };

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

        let mut request_builder = self
            .http
            .post(&url)
            .header("anthropic-version", API_VERSION)
            .header("content-type", "application/json")
            .header("accept", "application/json");

        // Set authentication and beta headers based on auth method
        request_builder = match self.config.auth_method {
            AuthMethod::ApiKey => request_builder
                .header("x-api-key", &self.config.auth_token)
                .header("anthropic-beta", BETA_HEADER),
            AuthMethod::OAuth => request_builder
                .header(
                    "Authorization",
                    format!("Bearer {}", self.config.auth_token),
                )
                .header("anthropic-beta", OAUTH_BETA_HEADER),
        };

        let response = request_builder
            .json(&request)
            .send()
            .await
            .map_err(Self::classify_reqwest_error)?;

        let status = response.status();
        if !status.is_success() {
            let error_body = response.text().await.unwrap_or_default();
            return Err(ProviderError::http_status(status.as_u16(), &error_body).into());
        }

        let byte_stream = response.bytes_stream();
        let event_stream = SseParser::new(byte_stream);
        Ok(Box::pin(event_stream))
    }

    /// Builds system message blocks with cache control on the last block only.
    ///
    /// Always includes the Claude Code identification prompt.
    /// Cache control placement:
    /// - Last system block: caches system prompt (often large with AGENTS.md)
    /// - Last user message: caches conversation history (set in send_messages_stream)
    ///
    /// This uses 2 of 4 allowed breakpoints. The minimum cache threshold is
    /// 1,024 tokens, so caching the system prompt separately ensures it gets
    /// cached even for short conversations.
    fn build_system_blocks(&self, system: Option<&str>) -> Option<Vec<SystemBlock>> {
        match system {
            Some(prompt) => Some(vec![
                SystemBlock::new(CLAUDE_CODE_SYSTEM_PROMPT),
                SystemBlock::with_cache_control(prompt),
            ]),
            None => Some(vec![SystemBlock::with_cache_control(
                CLAUDE_CODE_SYSTEM_PROMPT,
            )]),
        }
    }

    /// Classifies a reqwest error into a ProviderError.
    fn classify_reqwest_error(e: reqwest::Error) -> ProviderError {
        if e.is_timeout() {
            ProviderError::timeout(format!("Request timed out: {}", e))
        } else if e.is_connect() {
            ProviderError::timeout(format!("Connection failed: {}", e))
        } else if e.is_request() {
            ProviderError::new(
                ProviderErrorKind::HttpStatus,
                format!("Request error: {}", e),
            )
        } else {
            ProviderError::new(
                ProviderErrorKind::HttpStatus,
                format!("Network error: {}", e),
            )
        }
    }
}
