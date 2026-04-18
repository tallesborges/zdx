//! Gemini SSE stream parser.
//!
//! Shared parser for both Gemini API (API key) and Cloud Code Assist (OAuth).

use std::collections::{HashSet, VecDeque};
use std::pin::Pin;

use eventsource_stream::{EventStream, Eventsource};
use futures_util::Stream;
use serde_json::Value;
use uuid::Uuid;

use crate::{
    ContentBlockType, ProviderError, ProviderErrorKind, ProviderResult, SignatureProvider,
    StreamEvent, Usage, error_message_from_payload,
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

    #[allow(clippy::needless_pass_by_value, clippy::unnecessary_wraps)]
    fn handle_chunk(&mut self, value: Value) -> ProviderResult<()> {
        let payload = value.get("response").unwrap_or(&value);

        if self.emit_error_payload(&value, payload) {
            return Ok(());
        }

        self.capture_usage(payload);

        if self.emit_prompt_feedback(payload) {
            return Ok(());
        }

        // Once a prior chunk has already emitted the completion block, a late
        // candidate payload (e.g. a follow-up `functionCall` after `STOP`) must
        // not emit any more content/tool events.
        if !self.emitted_done
            && let Some(candidate) = payload
                .get("candidates")
                .and_then(|v| v.as_array())
                .and_then(|c| c.first())
        {
            if let Some(reason) = candidate.get("finishReason").and_then(|v| v.as_str()) {
                self.final_finish_reason = Some(reason.to_string());
            }
            if let Some(parts) = candidate
                .get("content")
                .and_then(|c| c.get("parts"))
                .and_then(|p| p.as_array())
            {
                self.process_parts(parts);
            }
        }

        self.maybe_finalize();
        Ok(())
    }

    fn emit_error_payload(&mut self, value: &Value, payload: &Value) -> bool {
        let Some(error) = value.get("error").or_else(|| payload.get("error")) else {
            return false;
        };
        let error_type = error
            .get("status")
            .or_else(|| error.get("code"))
            .and_then(|v| v.as_str())
            .unwrap_or("error")
            .to_string();
        let message = error_message_from_payload(error, &["message"]);
        self.pending.push_back(StreamEvent::Error {
            error_type,
            message,
        });
        // Error is terminal: prevent a subsequent EOF from synthesizing a
        // spurious second `truncated` error.
        self.emitted_done = true;
        true
    }

    fn capture_usage(&mut self, payload: &Value) {
        let Some(usage) = payload
            .get("usageMetadata")
            .or_else(|| payload.get("usage_metadata"))
        else {
            return;
        };
        let prompt = usage
            .get("promptTokenCount")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0);
        let completion = usage
            .get("candidatesTokenCount")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0);
        // Per Vertex docs, `totalTokenCount = promptTokenCount + candidatesTokenCount
        // + toolUsePromptTokenCount + thoughtsTokenCount`, so the tool-use and
        // thoughts counts are additive to prompt/candidates, not already included.
        let thoughts = usage
            .get("thoughtsTokenCount")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0);
        let tool_use = usage
            .get("toolUsePromptTokenCount")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0);
        let cached_from_details = usage
            .get("cacheTokensDetails")
            .and_then(|v| v.as_array())
            .map_or(0, |details| {
                details
                    .iter()
                    .filter_map(|item| item.get("tokenCount").and_then(serde_json::Value::as_u64))
                    .sum::<u64>()
            });
        let cached = usage
            .get("cachedContentTokenCount")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(cached_from_details);
        // Cached tokens are already included in `promptTokenCount`; subtract them
        // to avoid double-counting (they're reported separately in
        // `cache_read_input_tokens`). `toolUsePromptTokenCount` is input-side but
        // separate from the prompt, so add it after the subtraction.
        let input_tokens = prompt.saturating_sub(cached).saturating_add(tool_use);
        self.final_usage = Some(Usage {
            input_tokens,
            output_tokens: completion.saturating_add(thoughts),
            cache_read_input_tokens: cached,
            cache_creation_input_tokens: 0,
        });
    }

    fn emit_prompt_feedback(&mut self, payload: &Value) -> bool {
        // Gemini may block a prompt entirely; in that case the response contains
        // `promptFeedback` with a `blockReason`. The same payload may also include
        // `usageMetadata` (processed above), which we surface via a `MessageDelta`
        // so consumers can see prompt tokens for the blocked request. The `Error`
        // event is the terminal signal for blocked streams — downstream consumers
        // (e.g. the engine, debug metrics) abort on it, so we intentionally do
        // not emit `MessageCompleted` afterward.
        if self.emitted_done {
            return false;
        }
        let Some(feedback) = payload.get("promptFeedback") else {
            return false;
        };
        let Some(reason) = feedback
            .get("blockReason")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
        else {
            return false;
        };
        let message = if let Some(detail) = feedback
            .get("blockReasonMessage")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
        {
            format!("Prompt blocked: {reason}: {detail}")
        } else {
            format!("Prompt blocked: {reason}")
        };
        if let Some(usage) = self.final_usage.clone() {
            self.pending.push_back(StreamEvent::MessageDelta {
                stop_reason: Some("blocked".to_string()),
                usage: Some(usage.into()),
            });
        }
        self.pending.push_back(StreamEvent::Error {
            error_type: "blocked".to_string(),
            message,
        });
        self.emitted_done = true;
        true
    }

    #[allow(clippy::too_many_lines)]
    fn process_parts(&mut self, parts: &[Value]) {
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

        // First pass: thought parts (reasoning).
        let mut combined_reasoning = String::new();
        for part in parts {
            let is_thought = part
                .get("thought")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false);
            if is_thought && let Some(text) = part.get("text").and_then(|v| v.as_str()) {
                combined_reasoning.push_str(text);
            }
        }

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
                    data: None,
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

        // Second pass: regular text parts (non-thought).
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
                    data: None,
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

        // Third pass: function calls.
        for part in parts {
            if let Some(call) = part.get("functionCall") {
                let name = call.get("name").and_then(|v| v.as_str()).unwrap_or("");
                // Gemini occasionally emits malformed `{ "functionCall": {} }`
                // (or with only a signature). Skip — an empty-name tool call
                // would propagate as a bogus invocation to the engine.
                if name.is_empty() {
                    continue;
                }
                let args = call.get("args").unwrap_or(&Value::Null);

                // Prefer Gemini 3's real `functionCall.id` so it can be replayed
                // in the subsequent `functionResponse`. Older models omit it;
                // fall back to a synthesized local id in that case.
                let real_id = call
                    .get("id")
                    .and_then(|v| v.as_str())
                    .filter(|s| !s.is_empty())
                    .map(str::to_string);

                // Dedup only by real id: Gemini can internally retry and re-emit
                // the same functionCall in a later chunk. Within a single chunk
                // each part is iterated once, so no content-keyed dedup is
                // needed for legacy models without an id.
                if let Some(id) = &real_id
                    && !self.emitted_tool_calls.insert(id.clone())
                {
                    continue;
                }

                let tool_id = real_id.unwrap_or_else(|| {
                    format!(
                        "{}-{}-{}",
                        self.tool_id_prefix, self.run_id, self.next_index
                    )
                });
                let index = self.next_index;
                self.next_index += 1;
                self.saw_tool = true;

                self.pending.push_back(StreamEvent::ContentBlockStart {
                    index,
                    block_type: ContentBlockType::ToolUse,
                    id: Some(tool_id.clone()),
                    name: Some(name.to_string()),
                    data: None,
                });

                let args_json = if args.is_null() {
                    "{}".to_string()
                } else {
                    args.to_string()
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

    fn maybe_finalize(&mut self) {
        let Some(reason) = self.final_finish_reason.clone() else {
            return;
        };
        if self.emitted_done {
            return;
        }
        self.emitted_done = true;

        // Close reasoning block with signature if present.
        if self.reasoning_index.is_none() && self.pending_signature.is_some() {
            let index = self.next_index;
            self.next_index += 1;
            self.reasoning_index = Some(index);
            self.pending.push_back(StreamEvent::ContentBlockStart {
                index,
                block_type: ContentBlockType::Reasoning,
                id: None,
                name: None,
                data: None,
            });
        }

        if let Some(index) = self.reasoning_index.take() {
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
            usage: Some(usage.into()),
        });
        self.pending.push_back(StreamEvent::MessageCompleted);
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
                    // Transport-level failure during streaming (socket reset,
                    // connection dropped, etc.). Classify as `Timeout` (not `Parse`)
                    // so `ProviderError::is_retryable()` can see it as transient, and
                    // include an explicit "network error" token in the message so the
                    // pattern match in `RETRYABLE_PATTERNS` catches arbitrary
                    // underlying transport errors.
                    return Poll::Ready(Some(Err(ProviderError::new(
                        ProviderErrorKind::Timeout,
                        format!("SSE stream network error: {e}"),
                    ))));
                }
                Poll::Ready(None) => {
                    // The upstream stream ended. If we never emitted the completion
                    // block, the response was truncated (server closed the connection
                    // mid-stream). Surface a terminal `Error` event so consumers can
                    // distinguish this from a clean end; subsequent polls return None
                    // normally.
                    if !self.emitted_done {
                        self.emitted_done = true;
                        self.pending.push_back(StreamEvent::Error {
                            error_type: "truncated".to_string(),
                            message: "Gemini stream ended without finishReason".to_string(),
                        });
                        continue;
                    }
                    return Poll::Ready(None);
                }
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

    /// Test: A real Gemini 3 `functionCall.id` is preserved end-to-end instead of
    /// being replaced by a synthesized local id. The id must flow to
    /// `ContentBlockStart` so it can be replayed on the subsequent
    /// `functionResponse`.
    #[test]
    fn test_function_call_id_preserved_from_gemini() {
        let mut parser = create_test_parser();

        let chunk = json!({
            "candidates": [{
                "content": {
                    "parts": [{
                        "functionCall": {
                            "id": "call_abc123_from_gemini",
                            "name": "get_weather",
                            "args": {"city": "Paris"}
                        }
                    }]
                }
            }]
        });
        parser.handle_chunk(chunk).unwrap();

        let start = parser
            .pending
            .iter()
            .find(|e| matches!(e, StreamEvent::ContentBlockStart { .. }))
            .cloned()
            .expect("should emit ContentBlockStart for tool use");

        match start {
            StreamEvent::ContentBlockStart {
                block_type: ContentBlockType::ToolUse,
                id,
                name,
                ..
            } => {
                assert_eq!(
                    id.as_deref(),
                    Some("call_abc123_from_gemini"),
                    "real functionCall.id must be preserved"
                );
                assert_eq!(name.as_deref(), Some("get_weather"));
            }
            other => panic!("expected ToolUse ContentBlockStart, got {other:?}"),
        }

        // The synthesized prefix must not appear in the emitted id.
        let synthesized_prefix = format!("test-{}", parser.run_id);
        let uses_synthesized = parser.pending.iter().any(|e| match e {
            StreamEvent::ContentBlockStart {
                id: Some(id),
                block_type: ContentBlockType::ToolUse,
                ..
            } => id.starts_with(&synthesized_prefix),
            _ => false,
        });
        assert!(
            !uses_synthesized,
            "should not use synthesized id when Gemini supplied one"
        );

        // Re-emitting the same functionCall with the same real id in a later chunk
        // must be deduped (guards against internal retries).
        parser.pending.clear();
        let retry = json!({
            "candidates": [{
                "content": {
                    "parts": [{
                        "functionCall": {
                            "id": "call_abc123_from_gemini",
                            "name": "get_weather",
                            "args": {"city": "Paris"}
                        }
                    }]
                }
            }]
        });
        parser.handle_chunk(retry).unwrap();
        let retry_tool_starts = parser
            .pending
            .iter()
            .filter(|e| {
                matches!(
                    e,
                    StreamEvent::ContentBlockStart {
                        block_type: ContentBlockType::ToolUse,
                        ..
                    }
                )
            })
            .count();
        assert_eq!(
            retry_tool_starts, 0,
            "duplicate functionCall.id must be deduped across chunks"
        );
    }

    /// Test: `promptFeedback.blockReason` combined with `usageMetadata` emits a
    /// `MessageDelta` carrying the prompt tokens followed by a terminal `Error`.
    /// The engine aborts on `Error`, so `MessageCompleted` must NOT be emitted
    /// afterward — otherwise debug metrics misclassify blocked streams as
    /// `Completed`. The `MessageDelta` must come first so consumers see the
    /// usage before the `Error` aborts the stream.
    #[test]
    fn test_prompt_feedback_blocked_emits_error_with_usage() {
        use crate::UsageDelta;

        let mut parser = create_test_parser();

        // Real Gemini payload: both promptFeedback and usageMetadata in the same
        // chunk. Without reordering handle_chunk, the early return from the
        // promptFeedback branch would drop usageMetadata entirely.
        let chunk = json!({
            "promptFeedback": {
                "blockReason": "SAFETY",
                "safetyRatings": [
                    {"category": "HARM_CATEGORY_DANGEROUS_CONTENT", "probability": "HIGH"}
                ]
            },
            "usageMetadata": {
                "promptTokenCount": 42,
                "totalTokenCount": 42
            }
        });
        parser.handle_chunk(chunk).unwrap();

        let events: Vec<_> = parser.pending.drain(..).collect();

        let delta_idx = events
            .iter()
            .position(|e| {
                matches!(
                    e,
                    StreamEvent::MessageDelta {
                        stop_reason: Some(sr),
                        usage: Some(UsageDelta {
                            input_tokens: Some(tokens),
                            ..
                        }),
                    } if sr == "blocked" && *tokens > 0
                )
            })
            .expect("should emit MessageDelta carrying prompt tokens for blocked prompt");

        let error_idx = events
            .iter()
            .position(|e| {
                matches!(
                    e,
                    StreamEvent::Error { error_type, message }
                        if error_type == "blocked" && message.contains("SAFETY")
                )
            })
            .expect("should emit a blocked Error event with the reason");

        assert!(
            delta_idx < error_idx,
            "MessageDelta must come before Error so consumers see usage before the stream aborts",
        );

        assert!(
            !events
                .iter()
                .any(|e| matches!(e, StreamEvent::MessageCompleted)),
            "MessageCompleted must NOT be emitted — Error is the terminal signal for blocked streams",
        );

        assert!(parser.emitted_done, "emitted_done must be set");
    }

    /// Test: A blocked-prompt payload carrying `usageMetadata` reports the token
    /// counts via the emitted `MessageDelta`, and they are also retained in
    /// `parser.final_usage`. Regression test for the bug where the early return
    /// from the `promptFeedback` branch dropped `usageMetadata`, causing blocked
    /// requests to account as zero tokens.
    #[test]
    fn test_prompt_feedback_blocked_with_usage_reports_tokens() {
        use crate::UsageDelta;

        let mut parser = create_test_parser();

        let chunk = json!({
            "promptFeedback": {
                "blockReason": "SAFETY"
            },
            "usageMetadata": {
                "promptTokenCount": 7,
                "totalTokenCount": 7
            }
        });
        parser.handle_chunk(chunk).unwrap();

        // final_usage must be captured despite the blocked branch returning early.
        let final_usage = parser
            .final_usage
            .clone()
            .expect("final_usage must be captured from usageMetadata on blocked payloads");
        assert_eq!(final_usage.input_tokens, 7);

        let events: Vec<_> = parser.pending.drain(..).collect();

        // Event sequence: MessageDelta (with usage), then Error. No MessageCompleted.
        let mut iter = events.iter();
        let first = iter.next().expect("expected at least one event");
        match first {
            StreamEvent::MessageDelta {
                stop_reason,
                usage:
                    Some(UsageDelta {
                        input_tokens: Some(tokens),
                        ..
                    }),
            } => {
                assert_eq!(stop_reason.as_deref(), Some("blocked"));
                assert_eq!(*tokens, 7, "prompt tokens must be surfaced to consumers");
            }
            other => panic!("expected MessageDelta with usage first, got {other:?}"),
        }

        let second = iter.next().expect("expected Error after MessageDelta");
        assert!(
            matches!(
                second,
                StreamEvent::Error { error_type, .. } if error_type == "blocked"
            ),
            "expected blocked Error after MessageDelta, got {second:?}",
        );

        assert!(
            iter.next().is_none(),
            "no further events should be emitted; MessageCompleted must not follow Error",
        );
    }

    /// Test: `thoughtsTokenCount` is added to `output_tokens` and
    /// `toolUsePromptTokenCount` is added to `input_tokens`. Per Vertex docs,
    /// `totalTokenCount` equals `promptTokenCount` + `candidatesTokenCount` +
    /// `toolUsePromptTokenCount` + `thoughtsTokenCount`, so those fields are
    /// additive, not already included.
    #[test]
    fn test_usage_includes_thoughts_tokens() {
        let mut parser = create_test_parser();

        let chunk = json!({
            "candidates": [{
                "finishReason": "STOP",
                "content": { "parts": [{ "text": "done" }] }
            }],
            "usageMetadata": {
                "promptTokenCount": 100,
                "candidatesTokenCount": 50,
                "thoughtsTokenCount": 30,
                "toolUsePromptTokenCount": 10,
                "cachedContentTokenCount": 20
            }
        });
        parser.handle_chunk(chunk).unwrap();

        let usage = parser
            .final_usage
            .clone()
            .expect("final_usage should be set from usageMetadata");

        // input = (prompt - cached) + tool_use = (100 - 20) + 10 = 90
        assert_eq!(
            usage.input_tokens, 90,
            "input_tokens should include tool_use and exclude cached"
        );
        // output = completion + thoughts = 50 + 30 = 80
        assert_eq!(
            usage.output_tokens, 80,
            "output_tokens should include thoughtsTokenCount"
        );
        assert_eq!(usage.cache_read_input_tokens, 20);
        assert_eq!(usage.cache_creation_input_tokens, 0);
    }

    /// Test: After a chunk with `finishReason` has already emitted the completion
    /// block, a late chunk carrying a `functionCall` must NOT emit any further
    /// tool events. Previously the parts pass was unconditional, so Gemini could
    /// leak a post-STOP tool call through to the engine.
    #[test]
    fn test_late_tool_call_after_stop_is_ignored() {
        let mut parser = create_test_parser();

        // Chunk 1: finishReason=STOP with empty parts → emits completion block.
        let chunk1 = json!({
            "candidates": [{
                "finishReason": "STOP",
                "content": { "parts": [] }
            }]
        });
        parser.handle_chunk(chunk1).unwrap();
        assert!(parser.emitted_done, "chunk 1 should mark done");
        let drained: Vec<_> = parser.pending.drain(..).collect();
        assert!(
            drained
                .iter()
                .any(|e| matches!(e, StreamEvent::MessageStart { .. })),
            "chunk 1 should drain MessageStart",
        );
        assert!(
            drained
                .iter()
                .any(|e| matches!(e, StreamEvent::MessageCompleted)),
            "chunk 1 should drain MessageCompleted",
        );

        // Chunk 2: late functionCall after STOP — must be ignored.
        let chunk2 = json!({
            "candidates": [{
                "content": {
                    "parts": [{
                        "functionCall": {
                            "id": "x",
                            "name": "bash",
                            "args": {}
                        }
                    }]
                }
            }]
        });
        parser.handle_chunk(chunk2).unwrap();

        let post_stop_tool_start = parser.pending.iter().any(|e| {
            matches!(
                e,
                StreamEvent::ContentBlockStart {
                    block_type: ContentBlockType::ToolUse,
                    ..
                }
            )
        });
        assert!(
            !post_stop_tool_start,
            "late functionCall after STOP must not emit a ToolUse ContentBlockStart",
        );
        assert!(
            !parser.saw_tool,
            "saw_tool must remain false for post-STOP tool payloads",
        );
    }

    /// Test: The inner byte stream ending without a `finishReason` is a truncated
    /// response. `poll_next` must emit a terminal `Error { error_type: "truncated" }`
    /// (so consumers can distinguish truncation from a clean end) and only then
    /// return `None`.
    #[tokio::test]
    async fn test_eof_without_finish_reason_emits_truncated_error() {
        use futures_util::StreamExt;

        // One SSE event with partial text (no finishReason), then the stream
        // ends. The eventsource layer parses `data: ...\n\n` blocks.
        let sse_body =
            b"data: {\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"hi\"}]}}]}\n\n";
        let byte_stream = stream::iter(vec![Ok::<_, std::io::Error>(Bytes::from_static(sse_body))]);
        let mut parser =
            GeminiSseParser::new(byte_stream, "gemini-3-flash-preview".to_string(), "test");

        // Drain the partial text events.
        let mut saw_text_delta = false;
        let mut saw_truncated_error = false;
        while let Some(event) = parser.next().await {
            let event = event.expect("partial events should be Ok");
            match event {
                StreamEvent::TextDelta { ref text, .. } if text == "hi" => {
                    saw_text_delta = true;
                }
                StreamEvent::Error {
                    ref error_type,
                    ref message,
                } if error_type == "truncated" => {
                    assert!(
                        message.contains("finishReason"),
                        "truncated error message should mention missing finishReason, got: {message}",
                    );
                    saw_truncated_error = true;
                }
                _ => {}
            }
        }

        assert!(saw_text_delta, "should emit the partial text delta first");
        assert!(
            saw_truncated_error,
            "stream ending without finishReason must emit a truncated Error event",
        );
    }

    /// Test: A transport-level error mid-stream must surface as a retryable
    /// `ProviderError`. Previously the parser mapped it to `ProviderErrorKind::Parse`,
    /// which `is_retryable()` hard-codes as non-retryable, so transient socket
    /// failures were incorrectly treated as fatal.
    #[tokio::test]
    async fn test_transport_error_is_retryable() {
        use futures_util::StreamExt;

        let byte_stream = stream::iter(vec![Err::<Bytes, std::io::Error>(std::io::Error::new(
            std::io::ErrorKind::ConnectionReset,
            "socket closed mid-stream",
        ))]);
        let mut parser =
            GeminiSseParser::new(byte_stream, "gemini-3-flash-preview".to_string(), "test");

        let first = parser
            .next()
            .await
            .expect("stream should yield the transport error");
        let err = first.expect_err("transport failure must surface as Err");

        assert_ne!(
            err.kind,
            ProviderErrorKind::Parse,
            "transport error must not be classified as Parse (non-retryable)",
        );
        assert!(
            err.is_retryable(),
            "transient transport errors must be retryable, got {err:?}",
        );
    }

    /// Test: A malformed `functionCall` with no `name` (or an empty one) must be
    /// skipped silently. Previously it propagated as an empty-name tool
    /// invocation to the engine.
    #[test]
    fn test_malformed_empty_function_call_is_skipped() {
        // Case A: functionCall is an empty object.
        let mut parser = create_test_parser();
        let chunk_a = json!({
            "candidates": [{
                "content": {
                    "parts": [{ "functionCall": {} }]
                }
            }]
        });
        parser.handle_chunk(chunk_a).unwrap();
        let any_tool_start = parser.pending.iter().any(|e| {
            matches!(
                e,
                StreamEvent::ContentBlockStart {
                    block_type: ContentBlockType::ToolUse,
                    ..
                }
            )
        });
        assert!(
            !any_tool_start,
            "empty `functionCall: {{}}` must not emit a ToolUse block",
        );
        assert!(
            !parser.saw_tool,
            "empty `functionCall: {{}}` must not flip saw_tool",
        );

        // Case B: functionCall with explicit empty name.
        let mut parser = create_test_parser();
        let chunk_b = json!({
            "candidates": [{
                "content": {
                    "parts": [{ "functionCall": { "name": "" } }]
                }
            }]
        });
        parser.handle_chunk(chunk_b).unwrap();
        let any_tool_start = parser.pending.iter().any(|e| {
            matches!(
                e,
                StreamEvent::ContentBlockStart {
                    block_type: ContentBlockType::ToolUse,
                    ..
                }
            )
        });
        assert!(
            !any_tool_start,
            "`functionCall` with empty name must not emit a ToolUse block",
        );
        assert!(
            !parser.saw_tool,
            "`functionCall` with empty name must not flip saw_tool",
        );
    }

    /// Dedup is keyed on `functionCall.id`, not `(name, args)`. Two calls with
    /// identical name+args but distinct ids must both be emitted.
    #[test]
    fn test_function_call_dedup_is_id_based_not_content_based() {
        let mut parser = create_test_parser();

        let chunk = json!({
            "candidates": [{
                "content": {
                    "parts": [
                        { "functionCall": { "id": "call_a", "name": "bash", "args": {"cmd": "ls"} } },
                        { "functionCall": { "id": "call_b", "name": "bash", "args": {"cmd": "ls"} } }
                    ]
                }
            }]
        });
        parser.handle_chunk(chunk).unwrap();

        let tool_ids: Vec<String> = parser
            .pending
            .iter()
            .filter_map(|e| match e {
                StreamEvent::ContentBlockStart {
                    block_type: ContentBlockType::ToolUse,
                    id: Some(id),
                    ..
                } => Some(id.clone()),
                _ => None,
            })
            .collect();

        assert_eq!(
            tool_ids,
            vec!["call_a".to_string(), "call_b".to_string()],
            "both distinct-id calls must be emitted even with identical name+args",
        );
    }

    /// An API error payload is terminal. A subsequent EOF must NOT synthesize
    /// a second `truncated` error on top of the original API error.
    #[test]
    fn test_error_payload_marks_stream_terminal() {
        let mut parser = create_test_parser();

        let chunk = json!({
            "error": {
                "status": "INVALID_ARGUMENT",
                "message": "bad request"
            }
        });
        parser.handle_chunk(chunk).unwrap();

        assert!(parser.emitted_done, "error payload must set emitted_done");

        let error_count = parser
            .pending
            .iter()
            .filter(|e| matches!(e, StreamEvent::Error { .. }))
            .count();
        assert_eq!(error_count, 1, "exactly one Error event from the payload");

        // Simulate a subsequent EOF poll: with emitted_done=true, poll_next
        // must NOT push an additional `truncated` error. Verify by calling
        // the same path the Stream impl's None branch takes: if emitted_done
        // is already true, no new error should be enqueued.
        let pending_before = parser.pending.len();
        // This mirrors the `Poll::Ready(None)` branch logic:
        if !parser.emitted_done {
            parser.emitted_done = true;
            parser.pending.push_back(StreamEvent::Error {
                error_type: "truncated".to_string(),
                message: "Gemini stream ended without finishReason".to_string(),
            });
        }
        assert_eq!(
            parser.pending.len(),
            pending_before,
            "EOF after API error must not enqueue a second truncated error",
        );
    }

    /// `emit_error_payload` must handle errors nested under `response.error`
    /// (Cloud Code Assist wrapper) identically to top-level `error`.
    #[test]
    fn test_error_under_response_wrapper_is_handled() {
        let mut parser = create_test_parser();

        let chunk = json!({
            "response": {
                "error": {
                    "status": "PERMISSION_DENIED",
                    "message": "auth failed"
                }
            }
        });
        parser.handle_chunk(chunk).unwrap();

        let error = parser
            .pending
            .iter()
            .find_map(|e| match e {
                StreamEvent::Error {
                    error_type,
                    message,
                } => Some((error_type.clone(), message.clone())),
                _ => None,
            })
            .expect("Error event must be emitted for response.error");
        assert_eq!(error.0, "PERMISSION_DENIED");
        assert!(error.1.contains("auth failed"));
        assert!(parser.emitted_done);
    }
}
