use serde::Serialize;
use serde_json::Value;

use crate::providers::shared::{
    ChatContentBlock, ChatMessage, MessageContent, ReasoningBlock, ReplayToken,
};
use crate::tools::{ToolDefinition, ToolResultBlock, ToolResultContent};

// === API Request Types ===

/// Thinking configuration for extended thinking feature.
#[derive(Debug, Serialize)]
pub(crate) struct ThinkingConfig {
    #[serde(rename = "type")]
    thinking_type: &'static str,
    budget_tokens: u32,
}

impl ThinkingConfig {
    pub(crate) fn enabled(budget_tokens: u32) -> Self {
        Self {
            thinking_type: "enabled",
            budget_tokens,
        }
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
/// This is used within tool_result content arrays when returning images.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct ApiImageSource {
    #[serde(rename = "type")]
    source_type: &'static str,
    media_type: String,
    data: String,
}

/// Content block types that can appear in tool_result content arrays.
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
    Image { source: ApiImageSource },
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
    /// Converts a ChatMessage to ApiMessage with optional cache control.
    ///
    /// Handles thinking blocks with missing signatures (aborted thinking) by
    /// converting them to text blocks wrapped in `<thinking>` tags, following
    /// the pi-mono pattern for API compatibility.
    pub(crate) fn from_chat_message(msg: &ChatMessage, use_cache_control: bool) -> Self {
        match &msg.content {
            MessageContent::Text(text) => ApiMessage {
                role: msg.role.clone(),
                content: ApiMessageContent::Text(text.clone()),
            },
            MessageContent::Blocks(blocks) => {
                let api_blocks: Vec<ApiContentBlock> = blocks
                    .iter()
                    .filter_map(|b| match b {
                        ChatContentBlock::Reasoning(ReasoningBlock { text, replay }) => {
                            match replay.as_ref() {
                                Some(ReplayToken::Anthropic { signature }) => {
                                    let thinking = text.clone().unwrap_or_default();
                                    // If signature is missing or empty (aborted thinking),
                                    // convert to text block to avoid API rejection.
                                    // This follows the pi-mono pattern.
                                    if signature.is_empty() {
                                        Some(ApiContentBlock::Text {
                                            text: format!("<thinking>\n{}\n</thinking>", thinking),
                                            cache_control: None,
                                        })
                                    } else if thinking.is_empty() {
                                        None
                                    } else {
                                        Some(ApiContentBlock::Thinking {
                                            thinking,
                                            signature: signature.clone(),
                                        })
                                    }
                                }
                                // Skip OpenAI and Gemini reasoning blocks (provider-specific)
                                Some(ReplayToken::OpenAI { .. } | ReplayToken::Gemini { .. }) => {
                                    None
                                }
                                // No replay data; treat as text-only thinking for compatibility
                                None => text.as_ref().map(|thinking| ApiContentBlock::Text {
                                    text: format!("<thinking>\n{}\n</thinking>", thinking),
                                    cache_control: None,
                                }),
                            }
                        }
                        ChatContentBlock::Text(text) => Some(ApiContentBlock::Text {
                            text: text.clone(),
                            cache_control: if use_cache_control {
                                Some(CacheControl::ephemeral())
                            } else {
                                None
                            },
                        }),
                        ChatContentBlock::Image { mime_type, data } => {
                            Some(ApiContentBlock::Image {
                                source: ApiImageSource {
                                    source_type: "base64",
                                    media_type: mime_type.clone(),
                                    data: data.clone(),
                                },
                            })
                        }
                        ChatContentBlock::ToolUse { id, name, input } => {
                            Some(ApiContentBlock::ToolUse {
                                id: id.clone(),
                                name: name.clone(),
                                input: input.clone(),
                            })
                        }
                        ChatContentBlock::ToolResult(result) => {
                            let content = match &result.content {
                                ToolResultContent::Text(text) => {
                                    ApiToolResultContent::Text(text.clone())
                                }
                                ToolResultContent::Blocks(blocks) => {
                                    let api_blocks = blocks
                                        .iter()
                                        .map(|block| match block {
                                            ToolResultBlock::Text { text } => {
                                                ApiToolResultBlock::Text { text: text.clone() }
                                            }
                                            ToolResultBlock::Image { mime_type, data } => {
                                                ApiToolResultBlock::Image {
                                                    source: ApiImageSource {
                                                        source_type: "base64",
                                                        media_type: mime_type.clone(),
                                                        data: data.clone(),
                                                    },
                                                }
                                            }
                                        })
                                        .collect();
                                    ApiToolResultContent::Blocks(api_blocks)
                                }
                            };

                            Some(ApiContentBlock::ToolResult {
                                tool_use_id: result.tool_use_id.clone(),
                                content,
                                is_error: result.is_error,
                                cache_control: None,
                            })
                        }
                    })
                    .collect();
                ApiMessage {
                    role: msg.role.clone(),
                    content: ApiMessageContent::Blocks(api_blocks),
                }
            }
        }
    }
}
