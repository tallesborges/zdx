use serde::Serialize;
use serde_json::Value;
use zdx_types::{ToolDefinition, ToolResultBlock, ToolResultContent};

use crate::shared::{ChatContentBlock, ChatMessage, MessageContent, ReasoningBlock, ReplayToken};

// === API Request Types ===

/// Thinking configuration for extended thinking feature.
#[derive(Debug, Serialize)]
#[serde(tag = "type")]
pub(crate) enum ThinkingConfig {
    #[serde(rename = "enabled")]
    Enabled { budget_tokens: u32 },
    #[serde(rename = "adaptive")]
    Adaptive,
}

impl ThinkingConfig {
    pub(crate) fn enabled(budget_tokens: u32) -> Self {
        Self::Enabled { budget_tokens }
    }

    pub(crate) fn adaptive() -> Self {
        Self::Adaptive
    }
}

/// Effort levels for `output_config.effort`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum EffortLevel {
    Low,
    Medium,
    High,
    Max,
}

#[derive(Debug, Serialize)]
pub(crate) struct OutputConfig {
    effort: EffortLevel,
}

impl OutputConfig {
    pub(crate) fn new(effort: EffortLevel) -> Self {
        Self { effort }
    }
}

#[derive(Debug, Serialize)]
pub(crate) struct StreamingMessagesRequest<'a> {
    pub(crate) model: &'a str,
    pub(crate) max_tokens: u32,
    pub(crate) messages: Vec<ApiMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) tools: Option<Vec<ApiToolDef<'a>>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) system: Option<Vec<SystemBlock>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) thinking: Option<ThinkingConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) output_config: Option<OutputConfig>,
    pub(crate) stream: bool,
}

/// System message block with optional cache control.
#[derive(Debug, Serialize)]
pub(crate) struct SystemBlock {
    #[serde(rename = "type")]
    block_type: &'static str,
    text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    cache_control: Option<CacheControl>,
}

impl SystemBlock {
    pub(crate) fn with_cache_control(text: impl Into<String>) -> Self {
        Self {
            block_type: "text",
            text: text.into(),
            cache_control: Some(CacheControl::ephemeral()),
        }
    }
}

/// Cache control settings for prompt caching.
#[derive(Debug, Serialize)]
pub(crate) struct CacheControl {
    #[serde(rename = "type")]
    cache_type: &'static str,
}

impl CacheControl {
    pub(crate) fn ephemeral() -> Self {
        Self {
            cache_type: "ephemeral",
        }
    }
}

#[derive(Debug, Serialize)]
pub(crate) struct ApiToolDef<'a> {
    pub(crate) name: &'a str,
    pub(crate) description: &'a str,
    pub(crate) input_schema: &'a Value,
}

impl<'a> From<&'a ToolDefinition> for ApiToolDef<'a> {
    fn from(def: &'a ToolDefinition) -> Self {
        Self {
            name: &def.name,
            description: &def.description,
            input_schema: &def.input_schema,
        }
    }
}

#[derive(Debug, Serialize)]
pub(crate) struct ApiMessage {
    pub(crate) role: String,
    pub(crate) content: ApiMessageContent,
}

#[derive(Debug, Serialize)]
#[serde(untagged)]
pub(crate) enum ApiMessageContent {
    Text(String),
    Blocks(Vec<ApiContentBlock>),
}

/// Content block for image data in API requests.
///
/// This is used within `tool_result` content arrays when returning images.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct ApiImageSource {
    #[serde(rename = "type")]
    source_type: &'static str,
    media_type: String,
    data: String,
}

/// Content block types that can appear in `tool_result` content arrays.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum ApiToolResultBlock {
    Text { text: String },
    Image { source: ApiImageSource },
}

/// Tool result content - either a string or array of blocks.
///
/// Anthropic API accepts:
/// - String for text-only results (backwards compatible)
/// - Array of blocks when including images
///
/// Uses `#[serde(untagged)]` so `Text` serializes as a plain string and
/// `Blocks` serializes as an array.
#[derive(Debug, Clone, Serialize)]
#[serde(untagged)]
pub(crate) enum ApiToolResultContent {
    Text(String),
    Blocks(Vec<ApiToolResultBlock>),
}

#[derive(Debug, Serialize)]
#[serde(tag = "type")]
pub(crate) enum ApiContentBlock {
    #[serde(rename = "thinking")]
    Thinking { thinking: String, signature: String },
    #[serde(rename = "text")]
    Text {
        text: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        cache_control: Option<CacheControl>,
    },
    #[serde(rename = "image")]
    Image {
        source: ApiImageSource,
        #[serde(skip_serializing_if = "Option::is_none")]
        cache_control: Option<CacheControl>,
    },
    #[serde(rename = "tool_use")]
    ToolUse {
        id: String,
        name: String,
        input: Value,
    },
    #[serde(rename = "tool_result")]
    ToolResult {
        tool_use_id: String,
        content: ApiToolResultContent,
        #[serde(skip_serializing_if = "std::ops::Not::not")]
        is_error: bool,
        #[serde(skip_serializing_if = "Option::is_none")]
        cache_control: Option<CacheControl>,
    },
}

impl ApiMessage {
    /// Converts a `ChatMessage` to `ApiMessage` with optional cache control.
    ///
    /// Handles thinking blocks with missing signatures (aborted thinking) by
    /// converting them to plain text blocks, matching pi-mono's Anthropic
    /// replay fallback behavior.
    pub(crate) fn from_chat_message(msg: &ChatMessage, use_cache_control: bool) -> Self {
        match &msg.content {
            MessageContent::Text(text) => ApiMessage {
                role: msg.role.clone(),
                content: ApiMessageContent::Text(text.clone()),
            },
            MessageContent::Blocks(blocks) => {
                let api_blocks = blocks
                    .iter()
                    .filter_map(|block| api_content_block(block, use_cache_control))
                    .collect();
                ApiMessage {
                    role: msg.role.clone(),
                    content: ApiMessageContent::Blocks(api_blocks),
                }
            }
        }
    }
}

fn api_content_block(block: &ChatContentBlock, use_cache_control: bool) -> Option<ApiContentBlock> {
    match block {
        ChatContentBlock::Reasoning(reasoning) => api_reasoning_block(reasoning),
        ChatContentBlock::Text(text) => Some(ApiContentBlock::Text {
            text: text.clone(),
            cache_control: use_cache_control.then(CacheControl::ephemeral),
        }),
        ChatContentBlock::Image { mime_type, data } => Some(ApiContentBlock::Image {
            source: api_image_source(mime_type, data),
            cache_control: use_cache_control.then(CacheControl::ephemeral),
        }),
        ChatContentBlock::ToolUse { id, name, input } => Some(ApiContentBlock::ToolUse {
            id: id.clone(),
            name: name.clone(),
            input: input.clone(),
        }),
        ChatContentBlock::ToolResult(result) => Some(ApiContentBlock::ToolResult {
            tool_use_id: result.tool_use_id.clone(),
            content: api_tool_result_content(&result.content),
            is_error: result.is_error,
            cache_control: None,
        }),
    }
}

fn api_reasoning_block(reasoning: &ReasoningBlock) -> Option<ApiContentBlock> {
    match reasoning.replay.as_ref() {
        Some(ReplayToken::Anthropic { signature }) => {
            let thinking = reasoning.text.as_deref().unwrap_or_default();
            if signature.is_empty() {
                plain_reasoning_text(thinking)
            } else {
                Some(ApiContentBlock::Thinking {
                    thinking: thinking.to_string(),
                    signature: signature.clone(),
                })
            }
        }
        Some(ReplayToken::OpenAI { .. } | ReplayToken::Gemini { .. }) => None,
        None => reasoning
            .text
            .as_ref()
            .and_then(|thinking| plain_reasoning_text(thinking)),
    }
}

fn plain_reasoning_text(thinking: &str) -> Option<ApiContentBlock> {
    (!thinking.is_empty()).then(|| ApiContentBlock::Text {
        text: thinking.to_string(),
        cache_control: None,
    })
}

fn api_image_source(mime_type: &str, data: &str) -> ApiImageSource {
    ApiImageSource {
        source_type: "base64",
        media_type: mime_type.to_string(),
        data: data.to_string(),
    }
}

fn api_tool_result_content(content: &ToolResultContent) -> ApiToolResultContent {
    match content {
        ToolResultContent::Text(text) => ApiToolResultContent::Text(text.clone()),
        ToolResultContent::Blocks(blocks) => {
            ApiToolResultContent::Blocks(blocks.iter().map(api_tool_result_block).collect())
        }
    }
}

fn api_tool_result_block(block: &ToolResultBlock) -> ApiToolResultBlock {
    match block {
        ToolResultBlock::Text { text } => ApiToolResultBlock::Text { text: text.clone() },
        ToolResultBlock::Image { mime_type, data } => ApiToolResultBlock::Image {
            source: api_image_source(mime_type, data),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_anthropic_signature_falls_back_to_plain_text() {
        let block = api_reasoning_block(&ReasoningBlock {
            text: Some("I was thinking".to_string()),
            replay: Some(ReplayToken::Anthropic {
                signature: String::new(),
            }),
        });

        assert!(matches!(
            block,
            Some(ApiContentBlock::Text { ref text, .. }) if text == "I was thinking"
        ));
    }

    #[test]
    fn reasoning_without_replay_falls_back_to_plain_text() {
        let block = api_reasoning_block(&ReasoningBlock {
            text: Some("temporary reasoning".to_string()),
            replay: None,
        });

        assert!(matches!(
            block,
            Some(ApiContentBlock::Text { ref text, .. }) if text == "temporary reasoning"
        ));
    }

    #[test]
    fn empty_reasoning_without_signature_is_dropped() {
        let block = api_reasoning_block(&ReasoningBlock {
            text: Some(String::new()),
            replay: Some(ReplayToken::Anthropic {
                signature: String::new(),
            }),
        });

        assert!(block.is_none());
    }

    #[test]
    fn signature_only_reasoning_block_is_preserved() {
        let block = api_reasoning_block(&ReasoningBlock {
            text: Some(String::new()),
            replay: Some(ReplayToken::Anthropic {
                signature: "sig_123".to_string(),
            }),
        });

        assert!(matches!(
            block,
            Some(ApiContentBlock::Thinking {
                ref thinking,
                ref signature,
            }) if thinking.is_empty() && signature == "sig_123"
        ));
    }
}
