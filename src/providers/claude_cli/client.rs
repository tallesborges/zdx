//! Claude CLI client for Anthropic Messages API (OAuth).

use std::pin::Pin;

use anyhow::Result;
use futures_util::Stream;

use crate::providers::anthropic::types::StreamingMessagesRequest;
use crate::providers::anthropic::{
    build_api_messages_with_cache_control, build_system_blocks, build_thinking_config,
    build_tool_defs, send_streaming_request,
};
use crate::providers::claude_cli::auth::{ClaudeCliConfig, resolve_credentials};
use crate::providers::shared::{ChatMessage, StreamEvent};
use crate::tools::ToolDefinition;

const API_VERSION: &str = "2023-06-01";
const BETA_HEADER: &str = "claude-code-20250219,oauth-2025-04-20,interleaved-thinking-2025-05-14";
const CLAUDE_CODE_SYSTEM_PROMPT: &str = "You are Claude Code, Anthropic's official CLI for Claude.";

/// Claude CLI API client.
pub struct ClaudeCliClient {
    config: ClaudeCliConfig,
    http: reqwest::Client,
}

impl ClaudeCliClient {
    /// Creates a new Claude CLI client with the given configuration.
    pub fn new(config: ClaudeCliConfig) -> Self {
        Self {
            config,
            http: reqwest::Client::new(),
        }
    }

    /// Sends a thread and returns an async stream of events.
    pub async fn send_messages_stream(
        &self,
        messages: &[ChatMessage],
        tools: &[ToolDefinition],
        system: Option<&str>,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<StreamEvent>> + Send>>> {
        let creds = resolve_credentials().await?;

        // Convert messages to API format.
        // Only the last content block of the last user message gets cache_control.
        let api_messages = build_api_messages_with_cache_control(messages);

        let tool_defs = build_tool_defs(tools);

        let system_blocks = build_system_blocks(system, Some(CLAUDE_CODE_SYSTEM_PROMPT));

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

        let url = format!("{}/v1/messages?beta=true", self.config.base_url);

        send_streaming_request(&self.http, &url, &request, |builder| {
            builder
                .header("anthropic-version", API_VERSION)
                .header("Authorization", format!("Bearer {}", creds.access))
                .header("anthropic-beta", BETA_HEADER)
                .header("user-agent", "claude-cli/2.1.2 (external, cli)")
                .header("anthropic-dangerous-direct-browser-access", "true")
                .header("x-app", "cli")
        })
        .await
    }
}
