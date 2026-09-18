//! Chat message and content block value types shared across providers.

use std::str::FromStr;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::tools::{ToolResult, ToolResultBlock, ToolResultContent};

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
    /// Optional serialized `<runtime_context>` block attached to a user
    /// message. Persisted and replayed with the message so the provider-visible
    /// projection is identical on the live path and after restart
    /// (live == replay == what was sent). `text` stays pure for titles,
    /// search, exports, and UI; only the wire projection prepends the block.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<String>,
    /// Stable change key of the attached context block (SHA-256 over the
    /// update-eligible sections only — branch, catalogs, workspace extras).
    /// Persisted alongside `context` so later turns can decide whether a
    /// meaningful update warrants a replacement block without re-attaching on
    /// ambient tree/memory churn.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_key: Option<String>,
    pub content: MessageContent,
}

impl ChatMessage {
    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: "user".to_string(),
            phase: None,
            context: None,
            context_key: None,
            content: MessageContent::Text(content.into()),
        }
    }

    /// Attaches an optional runtime-context block to this message. Only
    /// meaningful for `role == "user"` messages; other roles ignore it.
    #[must_use]
    pub fn with_context(mut self, context: Option<String>) -> Self {
        self.context = context;
        self
    }

    /// Attaches a runtime-context block and its change key to this message.
    #[must_use]
    pub fn with_runtime_context(mut self, block: Option<String>, key: Option<String>) -> Self {
        self.context = block;
        self.context_key = key;
        self
    }

    /// Returns a copy with this message's runtime-context block prepended to
    /// the user content. This is the single projection used by both the live
    /// path and replay, so live == replay == what was sent. Messages without
    /// a context block (or non-user roles) project to an unchanged copy.
    #[must_use]
    pub fn with_runtime_context_projected(&self) -> Self {
        let Some(block) = self
            .context
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        else {
            return self.clone();
        };
        if self.role != "user" {
            return self.clone();
        }
        let mut projected = self.clone();
        match &mut projected.content {
            MessageContent::Text(text) => *text = format!("{block}\n\n{text}"),
            MessageContent::Blocks(blocks) => {
                blocks.insert(0, ChatContentBlock::text(block.to_string()));
            }
        }
        projected
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
            context: None,
            context_key: None,
            content: MessageContent::Blocks(blocks),
        }
    }

    /// Creates an assistant message with content blocks (for tool use).
    pub fn assistant_blocks(blocks: Vec<ChatContentBlock>) -> Self {
        Self {
            role: "assistant".to_string(),
            phase: None,
            context: None,
            context_key: None,
            content: MessageContent::Blocks(blocks),
        }
    }

    /// Creates an assistant text message with an optional Responses API phase.
    pub fn assistant_text(content: impl Into<String>, phase: Option<String>) -> Self {
        Self {
            role: "assistant".to_string(),
            phase,
            context: None,
            context_key: None,
            content: MessageContent::Text(content.into()),
        }
    }

    /// Whether this message carries tool results.
    ///
    /// Tool results ride the `user` role, so a `user` message is only a
    /// genuine user turn when it carries no `ToolResult` block.
    #[must_use]
    pub fn has_tool_results(&self) -> bool {
        matches!(
            &self.content,
            MessageContent::Blocks(blocks)
                if blocks
                    .iter()
                    .any(|block| matches!(block, ChatContentBlock::ToolResult(_)))
        )
    }

    /// Rough input-token estimate for this message (~4 characters per token).
    ///
    /// Approximate by design: it only sizes the `max_tokens` clamp, and the
    /// caller keeps headroom for the system prompt, tool definitions, and the
    /// error in this estimate. Image payloads are counted as a flat cost
    /// rather than by their base64 length, which bears no relation to how the
    /// model tokenizes them.
    #[must_use]
    pub fn estimated_tokens(&self) -> u64 {
        let chars = match &self.content {
            MessageContent::Text(text) => text.len(),
            MessageContent::Blocks(blocks) => blocks.iter().map(block_char_cost).sum(),
        };
        u64::try_from(chars).unwrap_or(u64::MAX).div_ceil(4)
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
            context: None,
            context_key: None,
            content: MessageContent::Blocks(blocks),
        }
    }
}

/// Index at which the in-flight turn begins: one past the last genuine user
/// turn.
///
/// Tool results also use the `user` role, so a tool-result carrier does not
/// start a turn. Request builders use this boundary to decide which assistant
/// reasoning may be replayed: only the current turn's reasoning is required
/// back on the wire, and older reasoning is dead weight that grows the request
/// without bound on providers that do not filter it server-side.
#[must_use]
pub fn current_turn_start(messages: &[ChatMessage]) -> usize {
    messages
        .iter()
        .rposition(|message| message.role == "user" && !message.has_tool_results())
        .map_or(0, |index| index + 1)
}

/// Which messages' reasoning a request may replay.
///
/// Only the in-flight turn's reasoning goes back on the wire. Every provider
/// that documents the requirement scopes it the same way:
///
/// - `OpenAI`'s Responses guide: "pass back all reasoning items, function call
///   items, and function call output items, **since the last `user`
///   message**", and reasoning items from earlier turns are "ignored and
///   removed".
/// - Gemini's thought-signature rules: "strict validation is enforced for all
///   function calls within the current turn. **Only current turn is
///   required; we don't validate on previous turns**", where a turn begins at
///   "the most recent user message that is not a `functionResponse`".
/// - Anthropic's extended-thinking guide: "you can omit `thinking` blocks from
///   prior `assistant` role turns".
///
/// A turn therefore begins at the last genuine user message. Tool results also
/// use the `user` role and do not start one, so a tool loop inside a turn
/// keeps replaying its reasoning — which is what the providers validate.
///
/// Replaying earlier turns' reasoning is dead weight: it grows a request
/// without bound as a thread accumulates turns, and on a backend that does not
/// filter prior-turn thinking server-side every later request pays for it
/// again.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReasoningReplay {
    turn_start: usize,
}

impl ReasoningReplay {
    /// Computes the replay scope for a request's message list.
    #[must_use]
    pub fn for_messages(messages: &[ChatMessage]) -> Self {
        Self {
            turn_start: current_turn_start(messages),
        }
    }

    /// Whether the message at `index` may replay its reasoning.
    ///
    /// A message carrying a redacted reasoning payload is exempt on every
    /// turn: that payload is opaque server-side verification state rather than
    /// reasoning text, and the Anthropic wire requires it echoed back
    /// unmodified. The exemption is inert for the other wires, which ignore
    /// foreign replay tokens.
    #[must_use]
    pub fn allows(self, index: usize, message: &ChatMessage) -> bool {
        index >= self.turn_start || carries_redacted_reasoning(message)
    }
}

/// Whether the message carries an opaque `redacted_thinking` payload.
fn carries_redacted_reasoning(message: &ChatMessage) -> bool {
    let MessageContent::Blocks(blocks) = &message.content else {
        return false;
    };
    blocks.iter().any(|block| {
        matches!(
            block,
            ChatContentBlock::Reasoning(ReasoningBlock {
                replay: Some(ReplayToken::AnthropicRedacted { .. }),
                ..
            })
        )
    })
}

/// Rough input-token estimate for a whole message list. See
/// [`ChatMessage::estimated_tokens`].
#[must_use]
pub fn estimated_input_tokens(messages: &[ChatMessage]) -> u64 {
    messages.iter().map(ChatMessage::estimated_tokens).sum()
}

/// Character cost of one content block, for [`ChatMessage::estimated_tokens`].
fn block_char_cost(block: &ChatContentBlock) -> usize {
    /// A single image costs roughly 1600 tokens regardless of its encoded
    /// size, so count it as a fixed 4x1600 characters.
    const IMAGE_CHAR_COST: usize = 6_400;

    match block {
        ChatContentBlock::Text { text, .. } => text.len(),
        ChatContentBlock::Reasoning(reasoning) => reasoning.text.as_ref().map_or(0, String::len),
        ChatContentBlock::ToolUse { input, .. } => input.to_string().len(),
        ChatContentBlock::ToolResult(result) => match &result.content {
            ToolResultContent::Text(text) => text.len(),
            ToolResultContent::Blocks(blocks) => blocks
                .iter()
                .map(|block| match block {
                    ToolResultBlock::Text { text } => text.len(),
                    ToolResultBlock::Image { .. } => IMAGE_CHAR_COST,
                })
                .sum(),
        },
        ChatContentBlock::Image { .. } => IMAGE_CHAR_COST,
    }
}

/// Wraps dictated transcript text in a `<voice_transcript>` block.
///
/// The tag is model-facing: it signals that the message was produced by
/// speech-to-text (so minor transcription errors are expected) without
/// altering the words. Surfaces that display the message strip the tag for
/// the user; only the model sees it.
#[must_use]
pub fn wrap_voice_transcript(text: &str) -> String {
    format!("<voice_transcript>\n{}\n</voice_transcript>", text.trim())
}

/// Returns the inner transcript when `text` is a `<voice_transcript>` block,
/// otherwise `None`. Lets display surfaces show dictated messages without the
/// tag while keeping the tagged text as the source of truth.
#[must_use]
pub fn strip_voice_transcript(text: &str) -> Option<&str> {
    let inner = text
        .trim()
        .strip_prefix("<voice_transcript>")?
        .strip_suffix("</voice_transcript>")?;
    Some(inner.trim())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assistant_reasoning(text: &str) -> ChatMessage {
        ChatMessage::assistant_blocks(vec![ChatContentBlock::Reasoning(ReasoningBlock {
            text: Some(text.to_string()),
            replay: None,
        })])
    }

    fn assistant_tool_call(id: &str) -> ChatMessage {
        ChatMessage::assistant_blocks(vec![ChatContentBlock::ToolUse {
            id: id.to_string(),
            name: "read".to_string(),
            input: serde_json::json!({"file_path": "a.txt"}),
            id_origin: IdOrigin::Real,
            replay: None,
        }])
    }

    /// Reasoning is replayed for the in-flight turn and dropped for everything
    /// before it. This is the one rule every request builder applies.
    #[test]
    fn reasoning_replay_allows_only_the_current_turn() {
        let messages = vec![
            ChatMessage::user("first"),
            assistant_reasoning("old"),
            ChatMessage::user("second"),
            assistant_reasoning("new"),
        ];
        let replay = ReasoningReplay::for_messages(&messages);

        assert!(!replay.allows(1, &messages[1]), "prior-turn reasoning");
        assert!(replay.allows(3, &messages[3]), "current-turn reasoning");
    }

    /// A tool loop inside one turn is not a new turn: tool-result carriers use
    /// the `user` role, so the assistant turns they answer keep replaying.
    #[test]
    fn tool_result_carriers_do_not_start_a_turn() {
        let messages = vec![
            ChatMessage::user("go"),
            assistant_reasoning("step one"),
            ChatMessage::tool_results(vec![ToolResult {
                tool_use_id: "call_1".to_string(),
                content: ToolResultContent::Text("ok".to_string()),
                is_error: false,
            }]),
            assistant_tool_call("call_2"),
        ];
        let replay = ReasoningReplay::for_messages(&messages);

        assert!(
            replay.allows(1, &messages[1]),
            "reasoning for the in-flight turn's tool step must survive"
        );
        assert!(replay.allows(3, &messages[3]));
    }

    /// A redacted reasoning payload is opaque server-side state, not text, so
    /// it is echoed back on every turn regardless of the boundary.
    #[test]
    fn redacted_reasoning_is_replayed_on_prior_turns() {
        let redacted =
            ChatMessage::assistant_blocks(vec![ChatContentBlock::Reasoning(ReasoningBlock {
                text: None,
                replay: Some(ReplayToken::AnthropicRedacted {
                    data: "opaque".to_string(),
                }),
            })]);
        let messages = vec![
            ChatMessage::user("first"),
            redacted.clone(),
            ChatMessage::user("second"),
        ];
        let replay = ReasoningReplay::for_messages(&messages);

        assert!(replay.allows(1, &messages[1]));
        assert!(replay.allows(1, &redacted));
    }

    /// A conversation with no genuine user turn yet (a fresh thread, or a
    /// resumed one) replays everything: there is no prior turn to drop.
    #[test]
    fn replay_allows_everything_without_a_user_turn() {
        let messages = vec![assistant_reasoning("a"), assistant_reasoning("b")];
        let replay = ReasoningReplay::for_messages(&messages);

        assert!(replay.allows(0, &messages[0]));
        assert!(replay.allows(1, &messages[1]));
    }

    /// The voice-transcript wrapper round-trips: the exact string handed to
    /// the model strips back to the original words for display. Both the TUI
    /// badge rendering and the bot rely on this contract.
    #[test]
    fn voice_transcript_wrap_then_strip_roundtrips() {
        let wrapped = wrap_voice_transcript("  fix the login bug  ");
        assert_eq!(
            wrapped,
            "<voice_transcript>\nfix the login bug\n</voice_transcript>"
        );
        assert_eq!(strip_voice_transcript(&wrapped), Some("fix the login bug"));
    }

    /// Plain text (typed messages) is never mistaken for a voice block.
    #[test]
    fn strip_voice_transcript_ignores_plain_text() {
        assert_eq!(strip_voice_transcript("just a normal message"), None);
        assert_eq!(
            strip_voice_transcript("talk about <voice_transcript> tags"),
            None
        );
    }

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

    /// The runtime-context projection prepends the persisted block to the user
    /// text (`live == replay == what was sent`) and leaves pure text untouched.
    #[test]
    fn test_runtime_context_projection_prepends_block_to_user_text() {
        let block = "<runtime_context>\norientation data\n</runtime_context>";
        let msg = ChatMessage::user("original user text").with_context(Some(block.to_string()));
        let projected = msg.with_runtime_context_projected();

        assert_eq!(projected.role, "user");
        let MessageContent::Text(text) = &projected.content else {
            panic!("expected text content");
        };
        assert_eq!(
            text,
            "<runtime_context>\norientation data\n</runtime_context>\n\noriginal user text"
        );
        // The source message keeps pure text + separate context.
        let MessageContent::Text(source_text) = &msg.content else {
            panic!("expected text content");
        };
        assert_eq!(source_text, "original user text");
        assert_eq!(msg.context.as_deref(), Some(block));
    }

    /// Projection is a no-op for messages without a context block, for
    /// assistant messages, and for blank context blocks.
    #[test]
    fn test_runtime_context_projection_is_noop_without_context() {
        assert_eq!(
            ChatMessage::user("hello").with_runtime_context_projected(),
            ChatMessage::user("hello")
        );
        let assistant = ChatMessage::assistant_text("hi", None)
            .with_context(Some("<runtime_context>x</runtime_context>".to_string()));
        assert_eq!(
            assistant.with_runtime_context_projected(),
            assistant,
            "assistant messages never project context"
        );
        let blank = ChatMessage::user("hi").with_context(Some("   ".to_string()));
        let projected = blank.with_runtime_context_projected();
        let MessageContent::Text(text) = &projected.content else {
            panic!("expected text content");
        };
        assert_eq!(text, "hi", "blank context must not alter the wire text");
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
