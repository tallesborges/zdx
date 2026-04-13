//! Chat message and content block value types shared across providers.

use std::str::FromStr;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::tools::ToolResult;

/// Provider-specific replay token for reasoning/thinking blocks.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "provider")]
pub enum ReplayToken {
    /// Anthropic extended thinking - requires signature for replay
    #[serde(rename = "anthropic")]
    Anthropic { signature: String },
    /// `OpenAI` Responses API reasoning - requires id + encrypted content for cache replay
    #[serde(rename = "openai")]
    OpenAI {
        id: String,
        encrypted_content: String,
    },
    /// Gemini thought signature - required for multi-turn function calling
    #[serde(rename = "gemini")]
    Gemini { signature: String },
}

/// Provider-agnostic reasoning/thinking content with optional replay token.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReasoningBlock {
    /// Human-readable text (thinking or summary) for display.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// Provider-specific replay data.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replay: Option<ReplayToken>,
}

/// Content block kinds emitted by streaming APIs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContentBlockType {
    Text,
    ToolUse,
    Reasoning,
}

/// Provider that produced a reasoning signature delta.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignatureProvider {
    Anthropic,
    Gemini,
}

impl FromStr for ContentBlockType {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "text" => Ok(Self::Text),
            "tool_use" => Ok(Self::ToolUse),
            "thinking" | "reasoning" => Ok(Self::Reasoning),
            _ => Err(format!("Unknown content block type: {value}")),
        }
    }
}

/// Content block in a chat message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ChatContentBlock {
    /// Model reasoning/thinking content (provider-specific)
    #[serde(rename = "reasoning")]
    Reasoning(ReasoningBlock),
    #[serde(rename = "text")]
    Text(String),
    #[serde(rename = "image")]
    Image {
        /// MIME type (e.g., "image/png", "image/jpeg")
        mime_type: String,
        /// Base64-encoded image data
        data: String,
    },
    #[serde(rename = "tool_use")]
    ToolUse {
        id: String,
        name: String,
        input: Value,
    },
    #[serde(rename = "tool_result")]
    ToolResult(ToolResult),
}

/// Message content - either simple text or structured blocks.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum MessageContent {
    Text(String),
    Blocks(Vec<ChatContentBlock>),
}

/// A chat message with owned data.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phase: Option<String>,
    pub content: MessageContent,
}

impl ChatMessage {
    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: "user".to_string(),
            phase: None,
            content: MessageContent::Text(content.into()),
        }
    }

    /// Creates a user message with text and image attachments.
    ///
    /// Each image is a tuple of `(mime_type, base64_data, optional_source_path)`.
    /// When a source path is provided, an `<attached_image>` XML tag is added
    /// to the text block so the model knows where the image came from.
    pub fn user_with_images(text: &str, images: &[(String, String, Option<String>)]) -> Self {
        let mut blocks = Vec::with_capacity(images.len() * 2 + 1);

        for (i, (mime_type, data, source_path)) in images.iter().enumerate() {
            // Add a text block describing the image source (helps the model)
            let description = if let Some(path) = source_path {
                format!(
                    "<attached_image path=\"{path}\">Image {} is from the path above.</attached_image>",
                    i + 1
                )
            } else {
                format!(
                    "<attached_image>Image {} from clipboard.</attached_image>",
                    i + 1
                )
            };
            blocks.push(ChatContentBlock::Text(description));

            // Add the actual image block
            blocks.push(ChatContentBlock::Image {
                mime_type: mime_type.clone(),
                data: data.clone(),
            });
        }

        if !text.is_empty() {
            blocks.push(ChatContentBlock::Text(text.to_string()));
        }

        Self {
            role: "user".to_string(),
            phase: None,
            content: MessageContent::Blocks(blocks),
        }
    }

    /// Creates an assistant message with content blocks (for tool use).
    pub fn assistant_blocks(blocks: Vec<ChatContentBlock>) -> Self {
        Self {
            role: "assistant".to_string(),
            phase: None,
            content: MessageContent::Blocks(blocks),
        }
    }

    /// Creates an assistant text message with an optional Responses API phase.
    pub fn assistant_text(content: impl Into<String>, phase: Option<String>) -> Self {
        Self {
            role: "assistant".to_string(),
            phase,
            content: MessageContent::Text(content.into()),
        }
    }

    /// Creates a user message with tool results.
    ///
    /// # Errors
    /// Returns an error if the operation fails.
    pub fn tool_results(results: Vec<ToolResult>) -> Self {
        let blocks: Vec<ChatContentBlock> = results
            .into_iter()
            .map(ChatContentBlock::ToolResult)
            .collect();
        Self {
            role: "user".to_string(),
            phase: None,
            content: MessageContent::Blocks(blocks),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Test: `ReplayToken::Gemini` serialization round-trips correctly.
    #[test]
    fn test_replay_token_gemini_roundtrip() {
        let token = ReplayToken::Gemini {
            signature: "base64_encoded_thought_signature".to_string(),
        };

        let json = serde_json::to_string(&token).unwrap();

        assert!(json.contains(r#""provider":"gemini""#));
        assert!(json.contains(r#""signature":"base64_encoded_thought_signature""#));

        let parsed: ReplayToken = serde_json::from_str(&json).unwrap();
        assert_eq!(token, parsed);
    }

    #[test]
    fn test_content_block_type_reasoning_parsing() {
        assert_eq!(
            ContentBlockType::from_str("thinking").unwrap(),
            ContentBlockType::Reasoning
        );
        assert_eq!(
            ContentBlockType::from_str("reasoning").unwrap(),
            ContentBlockType::Reasoning
        );
        assert_eq!(
            ContentBlockType::from_str("text").unwrap(),
            ContentBlockType::Text
        );
        assert_eq!(
            ContentBlockType::from_str("tool_use").unwrap(),
            ContentBlockType::ToolUse
        );
    }
}
