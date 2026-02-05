//! Shared helpers for Anthropic API providers.
//!
//! This module contains shared logic used by both `AnthropicClient` (API key)
//! and `ClaudeCliClient` (OAuth).

use anyhow::Result;

use super::sse::SseParser;
use super::types::{
    ApiContentBlock, ApiMessage, ApiMessageContent, ApiToolDef, CacheControl,
    StreamingMessagesRequest, SystemBlock, ThinkingConfig,
};
use crate::providers::debug_metrics::maybe_wrap_with_metrics;
use crate::providers::shared::{ChatMessage, ProviderError, ProviderErrorKind, ProviderStream};
use crate::providers::{DebugTrace, wrap_stream};
use crate::tools::ToolDefinition;

pub(crate) fn build_api_messages_with_cache_control(messages: &[ChatMessage]) -> Vec<ApiMessage> {
    let mut api_messages: Vec<ApiMessage> = messages
        .iter()
        .map(|m| ApiMessage::from_chat_message(m, false))
        .collect();

    sanitize_tool_use_ids(&mut api_messages);
    apply_cache_control_to_last_user_block(&mut api_messages);

    api_messages
}

pub(crate) fn build_tool_defs(tools: &[ToolDefinition]) -> Option<Vec<ApiToolDef<'_>>> {
    if tools.is_empty() {
        None
    } else {
        Some(tools.iter().map(ApiToolDef::from).collect::<Vec<_>>())
    }
}

pub(crate) fn build_thinking_config(enabled: bool, budget_tokens: u32) -> Option<ThinkingConfig> {
    if enabled {
        Some(ThinkingConfig::enabled(budget_tokens))
    } else {
        None
    }
}

pub(crate) fn build_system_blocks(
    prompt: Option<&str>,
    prelude: Option<&'static str>,
) -> Option<Vec<SystemBlock>> {
    let mut blocks = Vec::new();

    if let Some(prelude) = prelude {
        blocks.push(SystemBlock::with_cache_control(prelude));
    }

    if let Some(prompt) = prompt {
        blocks.push(SystemBlock::with_cache_control(prompt));
    }

    if blocks.is_empty() {
        None
    } else {
        Some(blocks)
    }
}

pub(crate) async fn send_streaming_request(
    client: &reqwest::Client,
    url: &str,
    request: &StreamingMessagesRequest<'_>,
    header_fn: impl FnOnce(reqwest::RequestBuilder) -> reqwest::RequestBuilder,
) -> Result<ProviderStream> {
    use crate::providers::shared::USER_AGENT;

    let trace = DebugTrace::from_env(request.model, None);
    let builder = client
        .post(url)
        .header("content-type", "application/json")
        .header("accept", "application/json")
        .header("user-agent", USER_AGENT);

    let response = if let Some(trace) = &trace {
        let body = serde_json::to_vec(request)?;
        trace.write_request(&body);
        header_fn(builder.body(body))
            .send()
            .await
            .map_err(classify_reqwest_error)?
    } else {
        header_fn(builder.json(request))
            .send()
            .await
            .map_err(classify_reqwest_error)?
    };

    let status = response.status();
    if !status.is_success() {
        let error_body = response.text().await.unwrap_or_default();
        return Err(ProviderError::http_status(status.as_u16(), &error_body).into());
    }

    let byte_stream = wrap_stream(trace, response.bytes_stream());
    let event_stream = SseParser::new(byte_stream);
    Ok(maybe_wrap_with_metrics(event_stream))
}

fn apply_cache_control_to_last_user_block(api_messages: &mut [ApiMessage]) {
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
}

fn sanitize_tool_use_ids(api_messages: &mut [ApiMessage]) {
    fn sanitize(id: &str) -> String {
        id.chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                    c
                } else {
                    '_'
                }
            })
            .collect::<String>()
    }

    for message in api_messages.iter_mut() {
        let ApiMessageContent::Blocks(blocks) = &mut message.content else {
            continue;
        };

        for block in blocks.iter_mut() {
            match block {
                ApiContentBlock::ToolUse { id, .. } => {
                    let sanitized = sanitize(id);
                    if sanitized != *id {
                        *id = sanitized;
                    }
                }
                ApiContentBlock::ToolResult { tool_use_id, .. } => {
                    let sanitized = sanitize(tool_use_id);
                    if sanitized != *tool_use_id {
                        *tool_use_id = sanitized;
                    }
                }
                _ => {}
            }
        }
    }
}

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
