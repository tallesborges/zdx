//! Gemini SSE stream parser.
//!
//! Shared parser for both Gemini API (API key) and Cloud Code Assist (OAuth).

use std::collections::{HashSet, VecDeque};
use std::pin::Pin;

use eventsource_stream::{EventStream, Eventsource};
use futures_util::Stream;
use serde_json::Value;
use uuid::Uuid;

use crate::providers::{
    ContentBlockType, ProviderError, ProviderErrorKind, ProviderResult, SignatureProvider,
    StreamEvent, Usage,
};

/// Gemini SSE stream parser.
///
/// Parses Server-Sent Events from Gemini API responses and converts them
/// to normalized `StreamEvent`s.
pub struct GeminiSseParser<S> {
    inner: EventStream<S>,
    model: String,
    run_id: String,
    tool_id_prefix: String,
    pending: VecDeque<StreamEvent>,
    next_index: usize,
    text_index: Option<usize>,
    last_text: String,
    /// Current reasoning block index (when processing thought parts)
    reasoning_index: Option<usize>,
    /// Accumulated reasoning text for delta calculation
    last_reasoning: String,
    /// Accumulated thought signature to emit at block completion
    pending_signature: Option<String>,
    /// Whether the pending signature originated from a function call part
    signature_from_function_call: bool,
    saw_tool: bool,
    emitted_tool_calls: HashSet<String>,
    final_usage: Option<Usage>,
    final_finish_reason: Option<String>,
    emitted_done: bool,
}

impl<S> GeminiSseParser<S> {
    /// Creates a new parser with custom tool ID prefix.
    ///
    /// The tool ID prefix is used to distinguish tools from different providers
    /// (e.g., "gemini" for API key, "gemini-cli" for OAuth).
    pub fn new(stream: S, model: String, tool_id_prefix: &str) -> Self
    where
        S: Eventsource,
    {
        Self {
            inner: stream.eventsource(),
            model,
            run_id: Uuid::new_v4().to_string(),
            tool_id_prefix: tool_id_prefix.to_string(),
            pending: VecDeque::new(),
            next_index: 0,
            text_index: None,
            last_text: String::new(),
            reasoning_index: None,
            last_reasoning: String::new(),
            pending_signature: None,
            signature_from_function_call: false,
            saw_tool: false,
            emitted_tool_calls: HashSet::new(),
            final_usage: None,
            final_finish_reason: None,
            emitted_done: false,
        }
    }

    fn handle_event_data(&mut self, data: &str) -> ProviderResult<()> {
        let trimmed = data.trim();
        if trimmed.is_empty() || trimmed == "[DONE]" {
            return Ok(());
        }

        let value = serde_json::from_str::<Value>(trimmed).map_err(|err| {
            ProviderError::new(
                ProviderErrorKind::Parse,
                format!("Failed to parse SSE JSON: {err}"),
            )
        })?;
        self.handle_chunk(value)
    }

    #[allow(
        clippy::too_many_lines,
        clippy::needless_pass_by_value,
        clippy::unnecessary_wraps
    )]
    fn handle_chunk(&mut self, value: Value) -> ProviderResult<()> {
        let payload = value.get("response").unwrap_or(&value);

        if let Some(error) = value.get("error").or_else(|| payload.get("error")) {
            let error_type = error
                .get("status")
                .or_else(|| error.get("code"))
                .and_then(|v| v.as_str())
                .unwrap_or("error")
                .to_string();
            let message = error
                .get("message")
                .and_then(|v| v.as_str())
                .unwrap_or("Unknown error")
                .to_string();
            self.pending.push_back(StreamEvent::Error {
                error_type,
                message,
            });
            return Ok(());
        }

        if let Some(usage) = payload
            .get("usageMetadata")
            .or_else(|| payload.get("usage_metadata"))
        {
            let prompt = usage
                .get("promptTokenCount")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(0);
            let completion = usage
                .get("candidatesTokenCount")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(0);
            let cached_from_details = usage
                .get("cacheTokensDetails")
                .and_then(|v| v.as_array())
                .map_or(0, |details| {
                    details
                        .iter()
                        .filter_map(|item| {
                            item.get("tokenCount").and_then(serde_json::Value::as_u64)
                        })
                        .sum::<u64>()
                });
            let cached = usage
                .get("cachedContentTokenCount")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(cached_from_details);
            self.final_usage = Some(Usage {
                input_tokens: prompt.saturating_sub(cached),
                output_tokens: completion,
                cache_read_input_tokens: cached,
                cache_creation_input_tokens: 0,
            });
        }

        if let Some(candidates) = payload.get("candidates").and_then(|v| v.as_array())
            && let Some(candidate) = candidates.first()
        {
            if let Some(reason) = candidate.get("finishReason").and_then(|v| v.as_str()) {
                self.final_finish_reason = Some(reason.to_string());
            }

            if let Some(content) = candidate.get("content")
                && let Some(parts) = content.get("parts").and_then(|v| v.as_array())
            {
                // Capture thought signatures from any part (text, thought, or functionCall).
                // Prefer signatures attached to function calls when present.
                for part in parts {
                    let mut signature = part
                        .get("thoughtSignature")
                        .and_then(serde_json::Value::as_str)
                        .map(|sig| (sig, part.get("functionCall").is_some()));

                    if signature.is_none() {
                        signature = part
                            .get("functionCall")
                            .and_then(|call| call.get("thoughtSignature"))
                            .and_then(serde_json::Value::as_str)
                            .map(|sig| (sig, true));
                    }

                    if let Some((sig, is_function_call)) = signature
                        && (is_function_call || !self.signature_from_function_call)
                    {
                        self.pending_signature = Some(sig.to_string());
                        self.signature_from_function_call = is_function_call;
                    }
                }

                // First pass: process thought parts (reasoning)
                let mut combined_reasoning = String::new();
                for part in parts {
                    let is_thought = part
                        .get("thought")
                        .and_then(serde_json::Value::as_bool)
                        .unwrap_or(false);
                    if is_thought {
                        // Accumulate thought text
                        if let Some(text) = part.get("text").and_then(|v| v.as_str()) {
                            combined_reasoning.push_str(text);
                        }
                    }
                }

                // Emit reasoning events if we have thought content
                if !combined_reasoning.is_empty() {
                    if self.reasoning_index.is_none() {
                        let index = self.next_index;
                        self.next_index += 1;
                        self.reasoning_index = Some(index);
                        self.pending.push_back(StreamEvent::ContentBlockStart {
                            index,
                            block_type: ContentBlockType::Reasoning,
                            id: None,
                            name: None,
                        });
                    }

                    let delta = if combined_reasoning.starts_with(&self.last_reasoning) {
                        combined_reasoning[self.last_reasoning.len()..].to_string()
                    } else {
                        combined_reasoning.clone()
                    };
                    self.last_reasoning = combined_reasoning;
                    if !delta.is_empty() {
                        self.pending.push_back(StreamEvent::ReasoningDelta {
                            index: self.reasoning_index.unwrap_or(0),
                            reasoning: delta,
                        });
                    }
                }

                // Second pass: process regular text parts (non-thought)
                let mut combined_text = String::new();
                for part in parts {
                    let is_thought = part
                        .get("thought")
                        .and_then(serde_json::Value::as_bool)
                        .unwrap_or(false);
                    if !is_thought && let Some(text) = part.get("text").and_then(|v| v.as_str()) {
                        combined_text.push_str(text);
                    }
                }

                if !combined_text.is_empty() {
                    if self.text_index.is_none() {
                        let index = self.next_index;
                        self.next_index += 1;
                        self.text_index = Some(index);
                        self.pending.push_back(StreamEvent::ContentBlockStart {
                            index,
                            block_type: ContentBlockType::Text,
                            id: None,
                            name: None,
                        });
                    }

                    let delta = if combined_text.starts_with(&self.last_text) {
                        combined_text[self.last_text.len()..].to_string()
                    } else {
                        combined_text.clone()
                    };
                    self.last_text = combined_text;
                    if !delta.is_empty() {
                        self.pending.push_back(StreamEvent::TextDelta {
                            index: self.text_index.unwrap_or(0),
                            text: delta,
                        });
                    }
                }

                // Third pass: process function calls
                for part in parts {
                    if let Some(call) = part.get("functionCall") {
                        let name = call.get("name").and_then(|v| v.as_str()).unwrap_or("");
                        let args = call.get("args").unwrap_or(&Value::Null);
                        let key = format!("{name}:{args}");
                        if self.emitted_tool_calls.contains(&key) {
                            continue;
                        }
                        self.emitted_tool_calls.insert(key);

                        let tool_id = format!(
                            "{}-{}-{}",
                            self.tool_id_prefix, self.run_id, self.next_index
                        );
                        let index = self.next_index;
                        self.next_index += 1;
                        self.saw_tool = true;

                        self.pending.push_back(StreamEvent::ContentBlockStart {
                            index,
                            block_type: ContentBlockType::ToolUse,
                            id: Some(tool_id.clone()),
                            name: Some(name.to_string()),
                        });

                        let args_json = if args.is_null() {
                            "{}".to_string()
                        } else {
                            serde_json::to_string(args).unwrap_or_else(|_| "{}".to_string())
                        };
                        self.pending.push_back(StreamEvent::InputJsonDelta {
                            index,
                            partial_json: args_json,
                        });
                        self.pending
                            .push_back(StreamEvent::ContentBlockCompleted { index });
                    }
                }
            }
        }

        if let Some(reason) = self.final_finish_reason.clone()
            && !self.emitted_done
        {
            self.emitted_done = true;

            // Close reasoning block with signature if present
            if self.reasoning_index.is_none() && self.pending_signature.is_some() {
                let index = self.next_index;
                self.next_index += 1;
                self.reasoning_index = Some(index);
                self.pending.push_back(StreamEvent::ContentBlockStart {
                    index,
                    block_type: ContentBlockType::Reasoning,
                    id: None,
                    name: None,
                });
            }

            if let Some(index) = self.reasoning_index.take() {
                // Emit signature if we accumulated one
                if let Some(signature) = self.pending_signature.take() {
                    self.pending
                        .push_back(StreamEvent::ReasoningSignatureDelta {
                            index,
                            signature,
                            provider: SignatureProvider::Gemini,
                        });
                    self.signature_from_function_call = false;
                }
                self.pending
                    .push_back(StreamEvent::ContentBlockCompleted { index });
            }

            // Close text block
            if let Some(index) = self.text_index.take() {
                self.pending
                    .push_back(StreamEvent::ContentBlockCompleted { index });
            }

            let usage = self.final_usage.clone().unwrap_or_default();
            let stop_reason = if self.saw_tool {
                Some("tool_use".to_string())
            } else {
                Some(map_finish_reason(&reason))
            };

            self.pending.push_back(StreamEvent::MessageStart {
                model: self.model.clone(),
                usage: usage.clone(),
            });
            self.pending.push_back(StreamEvent::MessageDelta {
                stop_reason,
                usage: Some(usage),
            });
            self.pending.push_back(StreamEvent::MessageCompleted);
        }

        Ok(())
    }
}

impl<S, E> Stream for GeminiSseParser<S>
where
    S: Stream<Item = std::result::Result<bytes::Bytes, E>> + Unpin,
    E: std::error::Error + Send + Sync + 'static,
{
    type Item = ProviderResult<StreamEvent>;

    fn poll_next(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        use std::task::Poll;

        loop {
            if let Some(event) = self.pending.pop_front() {
                return Poll::Ready(Some(Ok(event)));
            }

            let inner = Pin::new(&mut self.inner);
            match inner.poll_next(cx) {
                Poll::Ready(Some(Ok(event))) => {
                    if let Err(err) = self.handle_event_data(&event.data) {
                        return Poll::Ready(Some(Err(err)));
                    }
                }
                Poll::Ready(Some(Err(e))) => {
                    return Poll::Ready(Some(Err(ProviderError::new(
                        ProviderErrorKind::Parse,
                        format!("SSE stream error: {e}"),
                    ))));
                }
                Poll::Ready(None) => return Poll::Ready(None),
                Poll::Pending => return Poll::Pending,
            }
        }
    }
}

/// Maps Gemini finish reasons to normalized stop reasons.
pub fn map_finish_reason(reason: &str) -> String {
    match reason {
        "MAX_TOKENS" | "max_tokens" => "max_tokens".to_string(),
        "STOP" | "stop" => "stop".to_string(),
        other => other.to_lowercase(),
    }
}

#[cfg(test)]
mod tests {
    use bytes::Bytes;
    use futures_util::stream;
    use serde_json::json;

    use super::*;

    /// Creates a mock SSE parser for testing.
    fn create_test_parser() -> GeminiSseParser<impl Stream<Item = Result<Bytes, std::io::Error>>> {
        let empty_stream = stream::empty();
        GeminiSseParser::new(empty_stream, "gemini-3-flash-preview".to_string(), "test")
    }

    /// Test: Part with `thought: true` and text emits full reasoning event sequence.
    ///
    /// When a chunk contains a thought part with text content, the parser should emit:
    /// 1. `ContentBlockStart` { `block_type`: Reasoning }
    /// 2. `ReasoningDelta` with the thought text
    ///
    /// The signature and completion events are emitted when finishReason is present.
    #[test]
    fn test_thought_part_with_text_emits_reasoning_events() {
        let mut parser = create_test_parser();

        // Simulate a chunk with thought content
        let chunk = json!({
            "candidates": [{
                "content": {
                    "parts": [{
                        "thought": true,
                        "text": "Let me think about this..."
                    }]
                }
            }]
        });

        parser.handle_chunk(chunk).unwrap();

        // Should have emitted ContentBlockStart + ReasoningDelta
        assert_eq!(parser.pending.len(), 2);

        let event1 = parser.pending.pop_front().unwrap();
        assert!(matches!(
            event1,
            StreamEvent::ContentBlockStart {
                index: 0,
                block_type: ContentBlockType::Reasoning,
                ..
            }
        ));

        let event2 = parser.pending.pop_front().unwrap();
        assert!(matches!(
            event2,
            StreamEvent::ReasoningDelta {
                index: 0,
                ref reasoning,
            } if reasoning == "Let me think about this..."
        ));

        // Verify reasoning_index is set
        assert_eq!(parser.reasoning_index, Some(0));
    }

    /// Test: Part with `thought: true` and empty text captures signature, emits no reasoning block.
    ///
    /// When a thought part has empty text but has a signature, we should capture the signature
    /// but not emit a reasoning block (no `ContentBlockStart`, no `ReasoningDelta`).
    #[test]
    fn test_thought_part_empty_text_with_signature_captures_signature_only() {
        let mut parser = create_test_parser();

        // Simulate a chunk with thought part but empty text (signature-only)
        let chunk = json!({
            "candidates": [{
                "content": {
                    "parts": [{
                        "thought": true,
                        "text": "",
                        "thoughtSignature": "base64signature=="
                    }]
                }
            }]
        });

        parser.handle_chunk(chunk).unwrap();

        // Should not emit any events (empty thought text)
        assert!(
            parser.pending.is_empty(),
            "Should not emit events for empty thought text"
        );

        // But signature should be captured
        assert_eq!(
            parser.pending_signature,
            Some("base64signature==".to_string())
        );

        // reasoning_index should NOT be set since we didn't start a block
        assert!(parser.reasoning_index.is_none());
    }

    /// Test: Signature arriving in separate chunk after text is captured and emitted at completion.
    ///
    /// This tests the real-world scenario where:
    /// 1. First chunk has thought text
    /// 2. Second chunk has signature (possibly with empty text)
    /// 3. Third chunk has finishReason which triggers block completion with signature
    #[test]
    fn test_signature_arriving_in_separate_chunk() {
        let mut parser = create_test_parser();

        // Chunk 1: Thought text arrives
        let chunk1 = json!({
            "candidates": [{
                "content": {
                    "parts": [{
                        "thought": true,
                        "text": "I need to analyze this carefully"
                    }]
                }
            }]
        });
        parser.handle_chunk(chunk1).unwrap();

        // Clear the pending events from chunk 1
        parser.pending.clear();
        assert!(parser.pending_signature.is_none());

        // Chunk 2: Signature arrives (may have empty or same text due to rolling)
        let chunk2 = json!({
            "candidates": [{
                "content": {
                    "parts": [{
                        "thought": true,
                        "text": "I need to analyze this carefully",
                        "thoughtSignature": "late_arriving_signature_base64"
                    }]
                }
            }]
        });
        parser.handle_chunk(chunk2).unwrap();

        // No new delta since text is the same (rolling incremental)
        // But signature should be captured
        assert_eq!(
            parser.pending_signature,
            Some("late_arriving_signature_base64".to_string())
        );

        // Clear any events
        parser.pending.clear();

        // Chunk 3: Finish reason triggers completion
        let chunk3 = json!({
            "candidates": [{
                "finishReason": "STOP",
                "content": {
                    "parts": []
                }
            }],
            "usageMetadata": {
                "promptTokenCount": 100,
                "candidatesTokenCount": 50
            }
        });
        parser.handle_chunk(chunk3).unwrap();

        // Should have completion events including ReasoningSignatureDelta
        let events: Vec<_> = parser.pending.drain(..).collect();

        // Find the signature delta event
        let has_signature_delta = events.iter().any(|e| {
            matches!(
                e,
                StreamEvent::ReasoningSignatureDelta {
                    index: 0,
                    signature,
                    ..
                } if signature == "late_arriving_signature_base64"
            )
        });
        assert!(
            has_signature_delta,
            "Should emit ReasoningSignatureDelta at completion"
        );

        // Find the reasoning block completion
        let has_block_completed = events
            .iter()
            .any(|e| matches!(e, StreamEvent::ContentBlockCompleted { index: 0 }));
        assert!(
            has_block_completed,
            "Should emit ContentBlockCompleted for reasoning block"
        );
    }

    /// Test: functionCall parts can carry thoughtSignature; emit reasoning signature on completion.
    #[test]
    fn test_function_call_signature_emitted_on_completion() {
        let mut parser = create_test_parser();

        // Chunk 1: Function call with thoughtSignature (no thought text)
        let chunk1 = json!({
            "candidates": [{
                "content": {
                    "parts": [{
                        "functionCall": {
                            "name": "get_weather",
                            "args": {"city": "Paris"}
                        },
                        "thoughtSignature": "func_call_signature_base64"
                    }]
                }
            }]
        });
        parser.handle_chunk(chunk1).unwrap();

        // Tool events are emitted; clear them for focused assertions
        parser.pending.clear();

        // Signature should be captured and marked as function-call origin
        assert_eq!(
            parser.pending_signature,
            Some("func_call_signature_base64".to_string())
        );
        assert!(parser.signature_from_function_call);

        // Chunk 2: Finish reason triggers reasoning block completion with signature
        let chunk2 = json!({
            "candidates": [{
                "finishReason": "STOP",
                "content": { "parts": [] }
            }]
        });
        parser.handle_chunk(chunk2).unwrap();

        let events: Vec<_> = parser.pending.drain(..).collect();

        let has_start = events.iter().any(|e| {
            matches!(
                e,
                StreamEvent::ContentBlockStart {
                    block_type: ContentBlockType::Reasoning,
                    ..
                }
            )
        });
        assert!(
            has_start,
            "Should start a reasoning block for signature-only output"
        );

        let has_signature_delta = events.iter().any(|e| {
            matches!(
                e,
                StreamEvent::ReasoningSignatureDelta { signature, .. }
                    if signature == "func_call_signature_base64"
            )
        });
        assert!(
            has_signature_delta,
            "Should emit ReasoningSignatureDelta for functionCall signatures"
        );
    }

    /// Test: Mixed thought and regular text parts are processed separately.
    ///
    /// Gemini may return both thought parts and regular text parts in the same response.
    /// They should be processed into separate content blocks.
    #[test]
    fn test_mixed_thought_and_text_parts() {
        let mut parser = create_test_parser();

        // Chunk with both thought and regular text
        let chunk = json!({
            "candidates": [{
                "content": {
                    "parts": [
                        {
                            "thought": true,
                            "text": "Thinking about the answer..."
                        },
                        {
                            "text": "Here is my response."
                        }
                    ]
                }
            }]
        });

        parser.handle_chunk(chunk).unwrap();

        let events: Vec<_> = parser.pending.drain(..).collect();

        // Should have 4 events: reasoning start + delta, text start + delta
        assert_eq!(events.len(), 4);

        // First two should be reasoning
        assert!(matches!(
            &events[0],
            StreamEvent::ContentBlockStart {
                index: 0,
                block_type: ContentBlockType::Reasoning,
                ..
            }
        ));
        assert!(matches!(
            &events[1],
            StreamEvent::ReasoningDelta { index: 0, .. }
        ));

        // Second two should be text
        assert!(matches!(
            &events[2],
            StreamEvent::ContentBlockStart {
                index: 1,
                block_type: ContentBlockType::Text,
                ..
            }
        ));
        assert!(matches!(
            &events[3],
            StreamEvent::TextDelta { index: 1, .. }
        ));
    }

    /// Test: Incremental thought text uses delta calculation correctly.
    ///
    /// Gemini sends rolling incremental text (full accumulated text each time).
    /// The parser should only emit the delta (new portion).
    #[test]
    fn test_incremental_thought_text_delta_calculation() {
        let mut parser = create_test_parser();

        // First chunk: initial thought text
        let chunk1 = json!({
            "candidates": [{
                "content": {
                    "parts": [{
                        "thought": true,
                        "text": "First"
                    }]
                }
            }]
        });
        parser.handle_chunk(chunk1).unwrap();
        parser.pending.clear();

        // Second chunk: more text (rolling incremental)
        let chunk2 = json!({
            "candidates": [{
                "content": {
                    "parts": [{
                        "thought": true,
                        "text": "First, second"
                    }]
                }
            }]
        });
        parser.handle_chunk(chunk2).unwrap();

        // Should emit only the delta ", second"
        let events: Vec<_> = parser.pending.drain(..).collect();
        assert_eq!(events.len(), 1);
        assert!(matches!(
            &events[0],
            StreamEvent::ReasoningDelta {
                index: 0,
                reasoning,
            } if reasoning == ", second"
        ));
    }
}
