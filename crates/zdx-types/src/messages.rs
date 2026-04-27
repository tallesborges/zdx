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
    /// Anthropic `redacted_thinking`: opaque encrypted payload that must be
    /// replayed back to Anthropic unchanged. No plain-text summary; no
    /// signature; the `data` blob IS the block.
    #[serde(rename = "anthropic_redacted")]
    AnthropicRedacted { data: String },
    /// `OpenAI` Responses API reasoning - requires id + encrypted content for cache replay
    #[serde(rename = "openai")]
    OpenAI {
        id: String,
        encrypted_content: String,
    },
    /// Gemini thought signature - required for multi-turn function calling.
    ///
    /// `model` is the source model id that produced this signature; it is
    /// used to gate replay to the same model on the next turn (Gemini's
    /// implicit prompt cache requires byte-identical replay against the same
    /// model). Old transcripts deserialize with `model: ""`; the request
    /// builder treats empty as "unknown — replay normally" so single-model
    /// sessions are unaffected by the migration.
    #[serde(rename = "gemini")]
    Gemini {
        signature: String,
        #[serde(default)]
        model: String,
    },
}

/// Origin of a tool-use id: did the provider emit it, or did the SSE parser
/// synthesize it because the provider omitted one?
///
/// Used by the Gemini request builder to decide whether to replay the id on
/// the wire (`functionCall.id` and matching `functionResponse.id` are emitted
/// for `Real`, omitted for `Synthesized`). This keeps replay byte-identical
/// to what the provider originally produced — critical for Gemini's implicit
/// prompt cache.
///
/// **Default is `Synthesized`** so old transcripts (which were stored without
/// this field, and where Gemini may have synthesized ids) automatically opt
/// into the cache-friendly omit-on-replay behavior.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum IdOrigin {
    /// Provider did not emit an id; SSE parser synthesized one for engine
    /// correlation. Omit on replay.
    #[default]
    Synthesized,
    /// Provider emitted a real id. Replay verbatim.
    Real,
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
    /// Anthropic `redacted_thinking`: encrypted reasoning content that the
    /// server may return when a safety classifier flags the model's raw
    /// chain-of-thought. The opaque `data` blob must be replayed back
    /// unchanged on subsequent turns so the server can reconstruct the
    /// conversation; no plain-text summary is available.
    RedactedThinking,
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
            "redacted_thinking" => Ok(Self::RedactedThinking),
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
    Text {
        text: String,
        /// Provider-specific replay metadata (e.g. Gemini per-part
        /// `thoughtSignature`). `None` for messages from providers that don't
        /// produce per-text-part replay data.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        replay: Option<ReplayToken>,
    },
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
        /// Whether `id` was emitted by the provider (`Real`) or synthesized
        /// locally because the provider omitted one (`Synthesized`). Used by
        /// the Gemini request builder to decide whether to replay the id on
        /// the wire. Defaults to `Synthesized` for migration safety — see
        /// `IdOrigin` docs.
        #[serde(default)]
        id_origin: IdOrigin,
        /// Provider-specific replay metadata (e.g. Gemini per-part
        /// `thoughtSignature`).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        replay: Option<ReplayToken>,
    },
    #[serde(rename = "tool_result")]
    ToolResult(ToolResult),
}

impl ChatContentBlock {
    /// Constructs a plain text block with no replay metadata. Use this in
    /// non-Gemini code paths and tests to keep call sites compact.
    pub fn text(s: impl Into<String>) -> Self {
        Self::Text {
            text: s.into(),
            replay: None,
        }
    }

    /// Constructs a tool-use block with a synthesized id (the default for
    /// most call sites; the SSE parser explicitly sets `Real` when the
    /// provider emitted an id).
    pub fn tool_use(id: impl Into<String>, name: impl Into<String>, input: Value) -> Self {
        Self::ToolUse {
            id: id.into(),
            name: name.into(),
            input,
            id_origin: IdOrigin::Synthesized,
            replay: None,
        }
    }
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
            blocks.push(ChatContentBlock::text(description));

            // Add the actual image block
            blocks.push(ChatContentBlock::Image {
                mime_type: mime_type.clone(),
                data: data.clone(),
            });
        }

        if !text.is_empty() {
            blocks.push(ChatContentBlock::text(text));
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

    /// Test: `ReplayToken::Gemini` serialization round-trips correctly with
    /// the new `model` field.
    #[test]
    fn test_replay_token_gemini_with_model_roundtrip() {
        let token = ReplayToken::Gemini {
            signature: "base64_encoded_thought_signature".to_string(),
            model: "gemini-3-pro-preview".to_string(),
        };

        let json = serde_json::to_string(&token).unwrap();
        assert!(json.contains(r#""provider":"gemini""#));
        assert!(json.contains(r#""signature":"base64_encoded_thought_signature""#));
        assert!(json.contains(r#""model":"gemini-3-pro-preview""#));

        let parsed: ReplayToken = serde_json::from_str(&json).unwrap();
        assert_eq!(token, parsed);
    }

    /// Test: old `ReplayToken::Gemini` JSON without a `model` field still
    /// deserializes (via `#[serde(default)]`) with `model: ""`. This is the
    /// migration safety net: existing transcripts continue to load and the
    /// request builder treats empty model as "unknown — replay normally".
    #[test]
    fn test_replay_token_gemini_old_format_deserializes_with_empty_model() {
        let old_json = r#"{"provider":"gemini","signature":"abc"}"#;
        let parsed: ReplayToken = serde_json::from_str(old_json).unwrap();
        assert_eq!(
            parsed,
            ReplayToken::Gemini {
                signature: "abc".to_string(),
                model: String::new(),
            }
        );
    }

    /// Test: `ReplayToken::AnthropicRedacted` serialization round-trips correctly.
    #[test]
    fn test_replay_token_anthropic_redacted_roundtrip() {
        let token = ReplayToken::AnthropicRedacted {
            data: "encrypted_blob_xyz==".to_string(),
        };

        let json = serde_json::to_string(&token).unwrap();

        assert!(json.contains(r#""provider":"anthropic_redacted""#));
        assert!(json.contains(r#""data":"encrypted_blob_xyz==""#));

        let parsed: ReplayToken = serde_json::from_str(&json).unwrap();
        assert_eq!(token, parsed);
    }

    /// Test: `IdOrigin` defaults to `Synthesized` so old transcripts (which
    /// don't have the field) automatically opt into the cache-friendly
    /// omit-on-replay behavior.
    #[test]
    fn test_id_origin_default_is_synthesized() {
        assert_eq!(IdOrigin::default(), IdOrigin::Synthesized);

        // Old ToolUse JSON without id_origin field deserializes as Synthesized.
        let old_json = r#"{"type":"tool_use","id":"abc","name":"bash","input":{}}"#;
        let parsed: ChatContentBlock = serde_json::from_str(old_json).unwrap();
        let ChatContentBlock::ToolUse {
            id_origin, replay, ..
        } = parsed
        else {
            panic!("expected ToolUse");
        };
        assert_eq!(id_origin, IdOrigin::Synthesized);
        assert_eq!(replay, None);
    }

    /// Test: `ChatContentBlock::text(...)` constructor produces the
    /// `Text { text, replay: None }` shape expected by non-Gemini call sites.
    #[test]
    fn test_chat_content_block_text_constructor_helper() {
        let block = ChatContentBlock::text("hello");
        match block {
            ChatContentBlock::Text { text, replay } => {
                assert_eq!(text, "hello");
                assert_eq!(replay, None);
            }
            _ => panic!("expected Text variant"),
        }
    }

    /// Test: new `ChatContentBlock::Text` struct variant round-trips JSON
    /// with the explicit `text` field.
    #[test]
    fn test_chat_content_block_text_struct_variant_roundtrip() {
        let block = ChatContentBlock::text("hi");
        let json = serde_json::to_string(&block).unwrap();
        assert!(json.contains(r#""type":"text""#));
        assert!(json.contains(r#""text":"hi""#));
        let parsed: ChatContentBlock = serde_json::from_str(&json).unwrap();
        assert_eq!(block, parsed);
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
        assert_eq!(
            ContentBlockType::from_str("redacted_thinking").unwrap(),
            ContentBlockType::RedactedThinking
        );
    }
}
