//! Claude CLI (Anthropic OAuth) provider.

use anyhow::{Context, Result};

use super::api::DEFAULT_BASE_URL;
use super::shared::{
    build_api_messages_with_cache_control, build_system_blocks, build_thinking_and_output_config,
    build_tool_defs, send_streaming_request, should_enable_interleaved_thinking_beta,
};
use super::types::{EffortLevel, StreamingMessagesRequest};
use crate::providers::oauth::claude_cli as oauth_claude_cli;
use crate::providers::shared::{ChatMessage, ProviderStream};
use crate::tools::ToolDefinition;

const API_VERSION: &str = "2023-06-01";
const BETA_HEADER: &str = "claude-code-20250219,oauth-2025-04-20";
const CLAUDE_CODE_SYSTEM_PROMPT: &str = "You are Claude Code, Anthropic's official CLI for Claude.";

/// Runtime config for Claude CLI requests.
#[derive(Debug, Clone)]
pub struct ClaudeCliConfig {
    pub model: String,
    pub max_tokens: u32,
    pub base_url: String,
    /// Whether extended thinking is enabled
    pub thinking_enabled: bool,
    /// Token budget for thinking (only used when `thinking_enabled` = true)
    pub thinking_budget_tokens: u32,
    /// Optional effort level for supported models
    pub thinking_effort: Option<EffortLevel>,
}

impl ClaudeCliConfig {
    pub fn new(
        model: String,
        max_tokens: u32,
        base_url: Option<&str>,
        thinking_enabled: bool,
        thinking_budget_tokens: u32,
        thinking_effort: Option<EffortLevel>,
    ) -> Self {
        let base_url = base_url.unwrap_or(DEFAULT_BASE_URL).to_string();
        Self {
            model,
            max_tokens,
            base_url,
            thinking_enabled,
            thinking_budget_tokens,
            thinking_effort,
        }
    }
}

/// Resolves OAuth credentials, refreshing if expired.
///
/// # Errors
/// Returns an error if the operation fails.
pub async fn resolve_credentials() -> Result<oauth_claude_cli::ClaudeCliCredentials> {
    let mut creds = oauth_claude_cli::load_credentials()?.ok_or_else(|| {
        anyhow::anyhow!(
            "No Claude CLI OAuth credentials found. Run 'zdx login --claude-cli' to authenticate."
        )
    })?;

    if creds.is_expired() {
        let refreshed = oauth_claude_cli::refresh_token(&creds.refresh)
            .await
            .context("Failed to refresh Claude CLI OAuth token")?;
        oauth_claude_cli::save_credentials(&refreshed)?;
        creds = refreshed;
    }

    Ok(oauth_claude_cli::ClaudeCliCredentials {
        access: creds.access,
        refresh: creds.refresh,
        expires: creds.expires,
    })
}

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
    ///
    /// # Errors
    /// Returns an error if the operation fails.
    pub async fn send_messages_stream(
        &self,
        messages: &[ChatMessage],
        tools: &[ToolDefinition],
        system: Option<&str>,
    ) -> Result<ProviderStream> {
        let creds = resolve_credentials().await?;

        let request = self.build_streaming_request(messages, tools, system)?;
        let include_interleaved_beta = should_enable_interleaved_thinking_beta(
            &self.config.model,
            self.config.thinking_enabled,
        );
        let beta_header = if include_interleaved_beta {
            format!("{BETA_HEADER},interleaved-thinking-2025-05-14")
        } else {
            BETA_HEADER.to_string()
        };

        let url = format!("{}/v1/messages?beta=true", self.config.base_url);

        send_streaming_request(&self.http, &url, &request, |builder| {
            builder
                .header("anthropic-version", API_VERSION)
                .header("Authorization", format!("Bearer {}", creds.access))
                .header("anthropic-beta", beta_header)
                .header("user-agent", "claude-cli/2.1.2 (external, cli)")
                .header("anthropic-dangerous-direct-browser-access", "true")
                .header("x-app", "cli")
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
        // Only the last content block of the last user message gets cache_control.
        let api_messages = build_api_messages_with_cache_control(messages);

        let tool_defs = build_tool_defs(tools);

        let system_blocks = build_system_blocks(system, Some(CLAUDE_CODE_SYSTEM_PROMPT));

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
        let config = ClaudeCliConfig::new(
            "claude-opus-4-6".to_string(),
            4096,
            Some("http://mock-server"),
            true,
            2048,
            Some(EffortLevel::High),
        );
        let client = ClaudeCliClient::new(config);

        let request = client
            .build_streaming_request(&[ChatMessage::user("hi")], &[], None)
            .unwrap();

        let payload = serde_json::to_value(&request).unwrap();
        assert_eq!(payload["thinking"], json!({"type": "adaptive"}));
        assert_eq!(payload["output_config"], json!({"effort": "high"}));
    }

    #[test]
    fn build_request_for_legacy_model_keeps_budget_thinking() {
        let config = ClaudeCliConfig::new(
            "claude-opus-4-5".to_string(),
            4096,
            Some("http://mock-server"),
            true,
            1024,
            Some(EffortLevel::Low),
        );
        let client = ClaudeCliClient::new(config);

        let request = client
            .build_streaming_request(&[ChatMessage::user("hi")], &[], None)
            .unwrap();

        let payload = serde_json::to_value(&request).unwrap();
        assert_eq!(
            payload["thinking"],
            json!({"type": "enabled", "budget_tokens": 1024})
        );
        assert_eq!(payload["output_config"], json!({"effort": "low"}));
    }
}
