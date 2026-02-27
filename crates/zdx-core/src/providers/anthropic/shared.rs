//! Shared helpers for Anthropic API providers.
//!
//! This module contains shared logic used by both `AnthropicClient` (API key)
//! and `ClaudeCliClient` (OAuth).

use anyhow::{Result, bail};

use super::sse::SseParser;
use super::types::{
    ApiContentBlock, ApiMessage, ApiMessageContent, ApiToolDef, CacheControl, EffortLevel,
    OutputConfig, StreamingMessagesRequest, SystemBlock, ThinkingConfig,
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

///
/// # Errors
/// Returns an error if the operation fails.
pub(crate) fn build_thinking_and_output_config(
    model: &str,
    thinking_enabled: bool,
    thinking_budget_tokens: u32,
    thinking_effort: Option<EffortLevel>,
) -> Result<(Option<ThinkingConfig>, Option<OutputConfig>)> {
    let normalized_model = normalize_model_id(model);

    let thinking = if thinking_enabled {
        if supports_adaptive_thinking(normalized_model) {
            Some(ThinkingConfig::adaptive())
        } else {
            Some(ThinkingConfig::enabled(thinking_budget_tokens))
        }
    } else {
        None
    };

    let output_config = if thinking_enabled && supports_effort_control(normalized_model) {
        thinking_effort
            .map(|effort| {
                if effort == EffortLevel::Max && !supports_max_effort(normalized_model) {
                    bail!(
                        "Anthropic model '{normalized_model}' does not support output_config.effort=\"max\". \
                         thinking_level=\"xhigh\" currently maps to effort=\"max\". \
                         Use a lower thinking level or switch to claude-opus-4-6 / claude-sonnet-4-6."
                    );
                }
                Ok(OutputConfig::new(effort))
            })
            .transpose()?
    } else {
        None
    };

    Ok((thinking, output_config))
}

pub(crate) fn should_enable_interleaved_thinking_beta(model: &str, thinking_enabled: bool) -> bool {
    if !thinking_enabled {
        return false;
    }

    let model = normalize_model_id(model);
    if supports_adaptive_thinking(model) {
        return false;
    }

    model.starts_with("claude-opus-4")
        || model.starts_with("claude-sonnet-4")
        || model.starts_with("claude-haiku-4")
}

fn normalize_model_id(model: &str) -> &str {
    model.rsplit(':').next().unwrap_or(model)
}

fn supports_max_effort(model: &str) -> bool {
    model.starts_with("claude-opus-4-6") || model.starts_with("claude-sonnet-4-6")
}

fn supports_adaptive_thinking(model: &str) -> bool {
    model.starts_with("claude-opus-4-6") || model.starts_with("claude-sonnet-4-6")
}

fn supports_effort_control(model: &str) -> bool {
    model.starts_with("claude-opus-4-6")
        || model.starts_with("claude-opus-4-5")
        || model.starts_with("claude-sonnet-4-6")
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

///
/// # Errors
/// Returns an error if the operation fails.
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
            .map_err(|e| classify_reqwest_error(&e))?
    } else {
        header_fn(builder.json(request))
            .send()
            .await
            .map_err(|e| classify_reqwest_error(&e))?
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
            ApiContentBlock::Text { cache_control, .. }
            | ApiContentBlock::ToolResult { cache_control, .. } => {
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

fn classify_reqwest_error(e: &reqwest::Error) -> ProviderError {
    if e.is_timeout() {
        ProviderError::timeout(format!("Request timed out: {e}"))
    } else if e.is_connect() {
        ProviderError::timeout(format!("Connection failed: {e}"))
    } else if e.is_request() {
        ProviderError::new(ProviderErrorKind::HttpStatus, format!("Request error: {e}"))
    } else {
        ProviderError::new(ProviderErrorKind::HttpStatus, format!("Network error: {e}"))
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn thinking_and_effort_opus_46_uses_adaptive_and_allows_max() {
        let (thinking, output_config) =
            build_thinking_and_output_config("claude-opus-4-6", true, 4096, Some(EffortLevel::Max))
                .unwrap();

        assert_eq!(
            serde_json::to_value(thinking.unwrap()).unwrap(),
            json!({"type": "adaptive"})
        );
        assert_eq!(
            serde_json::to_value(output_config.unwrap()).unwrap(),
            json!({"effort": "max"})
        );
    }

    #[test]
    fn thinking_and_effort_opus_45_keeps_enabled_budget_and_high_effort() {
        let (thinking, output_config) = build_thinking_and_output_config(
            "claude-opus-4-5",
            true,
            2048,
            Some(EffortLevel::High),
        )
        .unwrap();

        assert_eq!(
            serde_json::to_value(thinking.unwrap()).unwrap(),
            json!({"type": "enabled", "budget_tokens": 2048})
        );
        assert_eq!(
            serde_json::to_value(output_config.unwrap()).unwrap(),
            json!({"effort": "high"})
        );
    }

    #[test]
    fn effort_is_not_sent_for_models_without_effort_support() {
        let (thinking, output_config) = build_thinking_and_output_config(
            "claude-sonnet-4-5",
            true,
            1024,
            Some(EffortLevel::Medium),
        )
        .unwrap();

        assert_eq!(
            serde_json::to_value(thinking.unwrap()).unwrap(),
            json!({"type": "enabled", "budget_tokens": 1024})
        );
        assert!(output_config.is_none());
    }

    #[test]
    fn max_effort_errors_on_non_opus_46() {
        let err =
            build_thinking_and_output_config("claude-opus-4-5", true, 1024, Some(EffortLevel::Max))
                .expect_err("max effort should fail on opus 4.5");

        assert!(
            err.to_string()
                .contains("does not support output_config.effort=\"max\""),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn provider_prefixed_model_ids_are_normalized() {
        let (thinking, output_config) = build_thinking_and_output_config(
            "claude-cli:claude-opus-4-6",
            true,
            4096,
            Some(EffortLevel::High),
        )
        .unwrap();

        assert_eq!(
            serde_json::to_value(thinking.unwrap()).unwrap(),
            json!({"type": "adaptive"})
        );
        assert_eq!(
            serde_json::to_value(output_config.unwrap()).unwrap(),
            json!({"effort": "high"})
        );
    }

    #[test]
    fn interleaved_beta_enabled_for_legacy_claude_4_with_thinking() {
        assert!(should_enable_interleaved_thinking_beta(
            "claude-opus-4-5",
            true
        ));
        assert!(should_enable_interleaved_thinking_beta(
            "claude-sonnet-4-5",
            true
        ));
    }

    #[test]
    fn interleaved_beta_disabled_for_opus_46_or_thinking_off() {
        assert!(!should_enable_interleaved_thinking_beta(
            "claude-opus-4-6",
            true
        ));
        assert!(!should_enable_interleaved_thinking_beta(
            "claude-opus-4-5",
            false
        ));
    }

    #[test]
    fn thinking_and_effort_sonnet_46_uses_adaptive_and_high_effort() {
        let (thinking, output_config) = build_thinking_and_output_config(
            "claude-sonnet-4-6",
            true,
            4096,
            Some(EffortLevel::High),
        )
        .unwrap();

        assert_eq!(
            serde_json::to_value(thinking.unwrap()).unwrap(),
            json!({"type": "adaptive"})
        );
        assert_eq!(
            serde_json::to_value(output_config.unwrap()).unwrap(),
            json!({"effort": "high"})
        );
    }

    #[test]
    fn max_effort_allowed_on_sonnet_46() {
        let (thinking, output_config) = build_thinking_and_output_config(
            "claude-sonnet-4-6",
            true,
            4096,
            Some(EffortLevel::Max),
        )
        .unwrap();

        assert_eq!(
            serde_json::to_value(thinking.unwrap()).unwrap(),
            json!({"type": "adaptive"})
        );
        assert_eq!(
            serde_json::to_value(output_config.unwrap()).unwrap(),
            json!({"effort": "max"})
        );
    }

    #[test]
    fn interleaved_beta_disabled_for_sonnet_46() {
        assert!(!should_enable_interleaved_thinking_beta(
            "claude-sonnet-4-6",
            true
        ));
    }

    #[test]
    fn interleaved_beta_enabled_for_sonnet_45_with_thinking() {
        assert!(should_enable_interleaved_thinking_beta(
            "claude-sonnet-4-5",
            true
        ));
    }
}
