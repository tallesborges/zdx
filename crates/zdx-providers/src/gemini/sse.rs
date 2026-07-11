//! Gemini SSE stream parser.
//!
//! Shared parser for both Gemini API (API key) and Cloud Code Assist (OAuth).

use std::collections::{HashSet, VecDeque};
use std::pin::Pin;

use eventsource_stream::{EventStream, Eventsource};
use futures_util::Stream;
use serde_json::Value;
use uuid::Uuid;
use zdx_types::messages::IdOrigin;

use crate::{
    ContentBlockType, ProviderError, ProviderErrorKind, ProviderResult, SignatureProvider,
    StreamEvent, Usage, error_message_from_payload, map_event_stream_error,
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
    /// The currently-open text or reasoning block. Closed (with its own
    /// signature attached on completion) when a new non-continuation part of
    /// any kind arrives, or at finalize. Function-call parts are atomic and
    /// do not flow through this slot.
    open_part: Option<OpenPart>,
    saw_tool: bool,
    emitted_tool_calls: HashSet<String>,
    final_usage: Option<Usage>,
    final_finish_reason: Option<String>,
    emitted_done: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OpenPartKind {
    Reasoning,
    Text,
}

#[derive(Debug, Clone)]
struct OpenPart {
    /// Stream block index assigned to this Gemini part.
    block_index: usize,
    kind: OpenPartKind,
    /// Latest accumulated text seen on this part (used to compute deltas
    /// across rolling-cumulative chunks where Gemini re-emits the same
    /// part with growing text).
    accumulated_text: String,
    /// Latest `thoughtSignature` seen on this part, if any.
    signature: Option<String>,
}

impl<S> GeminiSseParser<S> {
    /// Creates a new parser with custom tool ID prefix.
    ///
    /// The tool ID prefix is used to distinguish tools from different providers.
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
            open_part: None,
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

    /// Walks the chunk's parts in original order, emitting one
    /// `ContentBlock*` event sequence per Gemini part. Each part carries
    /// its own `thoughtSignature` end-to-end (no cross-part signature
    /// merging). Function-call parts are atomic; text/reasoning parts may
    /// span multiple chunks via rolling-cumulative re-emission, in which
    /// case the second chunk is treated as a continuation of the open
    /// part and only the new tail is emitted as a delta.
    fn process_parts(&mut self, parts: &[Value]) {
        // Same-chunk kind tracking: adjacent same-kind parts in one SSE
        // event must stay distinct blocks so each keeps its own signature.
        // Cross-chunk continuation lives in `handle_text_or_reasoning_part`.
        let mut prev_text_reasoning_kind: Option<OpenPartKind> = None;

        for part in parts {
            // `thoughtSignature` can sit at the part top level or under
            // `functionCall`.
            let signature = part
                .get("thoughtSignature")
                .and_then(Value::as_str)
                .or_else(|| {
                    part.get("functionCall")
                        .and_then(|c| c.get("thoughtSignature"))
                        .and_then(Value::as_str)
                })
                .map(str::to_string);

            if let Some(call) = part.get("functionCall") {
                self.handle_function_call_part(call, signature);
                // Function call closes any open text/reasoning.
                prev_text_reasoning_kind = None;
                continue;
            }

            let is_thought = part
                .get("thought")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let text = part.get("text").and_then(Value::as_str).unwrap_or("");

            // Skip pure metadata. Empty thought parts with a signature are
            // real (signature-only) and must open a block.
            if !is_thought && text.is_empty() && signature.is_none() {
                continue;
            }

            let kind = if is_thought {
                OpenPartKind::Reasoning
            } else {
                OpenPartKind::Text
            };

            // Split before a non-empty same-kind sibling so each part keeps
            // its own signature. Empty signature-only parts attach to the
            // open block instead.
            if prev_text_reasoning_kind == Some(kind) && !text.is_empty() {
                self.close_open_part();
            }

            self.handle_text_or_reasoning_part(kind, text, signature);
            // Only non-empty parts arm the split heuristic; otherwise
            // `[empty+sig, "real"]` would split into an empty signed block
            // plus an unsigned text block.
            if !text.is_empty() {
                prev_text_reasoning_kind = Some(kind);
            }
        }
    }

    fn handle_text_or_reasoning_part(
        &mut self,
        kind: OpenPartKind,
        text: &str,
        signature: Option<String>,
    ) {
        // Same-kind across chunks continues the open block. Gemini streams
        // text two ways: cumulative (each chunk = full accumulated text)
        // and incremental (each chunk = only the new tail). Both must
        // accumulate into one block — opening a fresh block per incremental
        // chunk fragments one logical response across N persisted events
        // and splits markdown spans (`**bold` … `text**`) across cells.
        let is_same_kind_open = matches!(
            self.open_part.as_ref(),
            Some(open) if open.kind == kind
        );

        if is_same_kind_open {
            let open = self
                .open_part
                .as_mut()
                .expect("open_part is Some by same-kind guard");
            let delta = if text.starts_with(&open.accumulated_text)
                && text.len() > open.accumulated_text.len()
            {
                // Cumulative: emit the new suffix, replace snapshot.
                let suffix = text[open.accumulated_text.len()..].to_string();
                open.accumulated_text = text.to_string();
                suffix
            } else if text == open.accumulated_text || text.is_empty() {
                // Pure resend or signature-only chunk; nothing to emit.
                String::new()
            } else {
                // Incremental: append verbatim.
                open.accumulated_text.push_str(text);
                text.to_string()
            };
            if signature.is_some() {
                open.signature = signature;
            }
            if !delta.is_empty() {
                let event = match kind {
                    OpenPartKind::Reasoning => StreamEvent::ReasoningDelta {
                        index: open.block_index,
                        reasoning: delta,
                    },
                    OpenPartKind::Text => StreamEvent::TextDelta {
                        index: open.block_index,
                        text: delta,
                    },
                };
                self.pending.push_back(event);
            }
            return;
        }

        // Different kind or no open part: close existing, open new.
        self.close_open_part();

        let block_index = self.next_index;
        self.next_index += 1;

        let block_type = match kind {
            OpenPartKind::Reasoning => ContentBlockType::Reasoning,
            OpenPartKind::Text => ContentBlockType::Text,
        };
        self.pending.push_back(StreamEvent::ContentBlockStart {
            index: block_index,
            block_type,
            id: None,
            name: None,
            data: None,
            id_origin: None,
        });

        if !text.is_empty() {
            let event = match kind {
                OpenPartKind::Reasoning => StreamEvent::ReasoningDelta {
                    index: block_index,
                    reasoning: text.to_string(),
                },
                OpenPartKind::Text => StreamEvent::TextDelta {
                    index: block_index,
                    text: text.to_string(),
                },
            };
            self.pending.push_back(event);
        }

        self.open_part = Some(OpenPart {
            block_index,
            kind,
            accumulated_text: text.to_string(),
            signature,
        });
    }

    fn handle_function_call_part(&mut self, call: &Value, signature: Option<String>) {
        let name = call.get("name").and_then(Value::as_str).unwrap_or("");
        // Gemini occasionally emits malformed `{ "functionCall": {} }`
        // (or with only a signature). Skip — an empty-name tool call
        // would propagate as a bogus invocation to the engine.
        if name.is_empty() {
            return;
        }
        let args = call.get("args").unwrap_or(&Value::Null);

        // Prefer Gemini 3's real `functionCall.id` so it can be replayed
        // in the subsequent `functionResponse`. Older models omit it; fall
        // back to a synthesized local id and mark `id_origin: Synthesized`
        // so the request builder knows to omit it on replay.
        let real_id = call
            .get("id")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_string);

        // Dedup only by real id: Gemini can internally retry and re-emit
        // the same functionCall in a later chunk. Without an id there is
        // no reliable cross-chunk identity; trust the protocol not to
        // duplicate.
        if let Some(id) = &real_id
            && !self.emitted_tool_calls.insert(id.clone())
        {
            return;
        }

        // A function call atomically closes any text/reasoning part that
        // came before it.
        self.close_open_part();

        let (tool_id, id_origin) = match real_id {
            Some(id) => (id, IdOrigin::Real),
            None => (
                format!(
                    "{}-{}-{}",
                    self.tool_id_prefix, self.run_id, self.next_index
                ),
                IdOrigin::Synthesized,
            ),
        };

        let block_index = self.next_index;
        self.next_index += 1;
        self.saw_tool = true;

        self.pending.push_back(StreamEvent::ContentBlockStart {
            index: block_index,
            block_type: ContentBlockType::ToolUse,
            id: Some(tool_id),
            name: Some(name.to_string()),
            data: None,
            id_origin: Some(id_origin),
        });

        let args_json = if args.is_null() {
            "{}".to_string()
        } else {
            args.to_string()
        };
        self.pending.push_back(StreamEvent::InputJsonDelta {
            index: block_index,
            partial_json: args_json,
        });

        let sig_payload = signature.map(|s| (SignatureProvider::Gemini, s));
        self.pending.push_back(StreamEvent::ContentBlockCompleted {
            index: block_index,
            signature: sig_payload,
        });
    }

    /// Closes the currently-open text or reasoning part (if any), attaching
    /// its per-part signature on completion. Reasoning parts emit the
    /// signature via `ReasoningSignatureDelta` (consumed by the existing
    /// `ReasoningBlock` path); text parts ride the generalized
    /// `ContentBlockCompleted.signature` field.
    fn close_open_part(&mut self) {
        let Some(open) = self.open_part.take() else {
            return;
        };
        match open.kind {
            OpenPartKind::Reasoning => {
                if let Some(sig) = open.signature {
                    self.pending
                        .push_back(StreamEvent::ReasoningSignatureDelta {
                            index: open.block_index,
                            signature: sig,
                            provider: SignatureProvider::Gemini,
                        });
                }
                self.pending.push_back(StreamEvent::ContentBlockCompleted {
                    index: open.block_index,
                    signature: None,
                });
            }
            OpenPartKind::Text => {
                let signature = open.signature.map(|s| (SignatureProvider::Gemini, s));
                self.pending.push_back(StreamEvent::ContentBlockCompleted {
                    index: open.block_index,
                    signature,
                });
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

        // Close any text/reasoning part still open at finishReason.
        self.close_open_part();

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
                    // Distinguish transport-level failures (retryable) from
                    // SSE framing/UTF-8 parser failures (non-retryable). See
                    // `map_event_stream_error` for the policy.
                    return Poll::Ready(Some(Err(map_event_stream_error(e))));
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

        // Verify the open part is a Reasoning block at index 0
        assert!(matches!(
            parser.open_part,
            Some(OpenPart {
                block_index: 0,
                kind: OpenPartKind::Reasoning,
                ..
            })
        ));
    }

    /// Test: Part with `thought: true`, empty text, and a signature opens an
    /// empty Reasoning block whose signature is attached on completion.
    /// Per-part fidelity requires the signature to survive end-to-end —
    /// the engine builder is responsible for keeping or dropping
    /// empty-text-with-signature blocks.
    #[test]
    fn test_thought_part_empty_text_with_signature_opens_block() {
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

        // ContentBlockStart is emitted (no ReasoningDelta — empty text).
        assert_eq!(parser.pending.len(), 1);
        let event = parser.pending.pop_front().unwrap();
        assert!(matches!(
            event,
            StreamEvent::ContentBlockStart {
                index: 0,
                block_type: ContentBlockType::Reasoning,
                ..
            }
        ));

        // The signature is recorded on the open part and will be flushed
        // via ReasoningSignatureDelta when the block closes.
        let open = parser.open_part.as_ref().expect("open_part must be set");
        assert_eq!(open.kind, OpenPartKind::Reasoning);
        assert_eq!(open.signature.as_deref(), Some("base64signature=="));
    }

    /// Test: Signature arriving in separate chunk after text is captured and emitted at completion.
    ///
    /// This tests the rolling-cumulative streaming variant where:
    /// 1. First chunk has thought text
    /// 2. Second chunk has the same thought text plus a signature
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
        let open = parser.open_part.as_ref().expect("open_part after chunk 1");
        assert_eq!(open.kind, OpenPartKind::Reasoning);
        assert!(open.signature.is_none());

        // Chunk 2: Signature arrives (rolling-cumulative — same text body).
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

        // No new delta since text is the same (continuation), but the
        // signature is now captured on the open part.
        assert!(
            parser.pending.is_empty(),
            "no events on signature-only continuation"
        );
        let open = parser.open_part.as_ref().expect("open_part still set");
        assert_eq!(
            open.signature.as_deref(),
            Some("late_arriving_signature_base64")
        );

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
        let has_block_completed = events.iter().any(|e| {
            matches!(
                e,
                StreamEvent::ContentBlockCompleted {
                    index: 0,
                    signature: None
                }
            )
        });
        assert!(
            has_block_completed,
            "Should emit ContentBlockCompleted for reasoning block"
        );
    }

    /// Test: A `functionCall` part carrying its own `thoughtSignature`
    /// emits the signature on the tool-use's `ContentBlockCompleted` (not
    /// on a separate Reasoning block). Per-part fidelity: the signature
    /// stays attached to the part that produced it.
    #[test]
    fn test_function_call_signature_emitted_on_completion() {
        let mut parser = create_test_parser();

        // Function call with thoughtSignature (no thought text)
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

        let events: Vec<_> = parser.pending.drain(..).collect();

        // ToolUse start, args delta, then completed-with-signature.
        let tool_start_index = events
            .iter()
            .position(|e| {
                matches!(
                    e,
                    StreamEvent::ContentBlockStart {
                        block_type: ContentBlockType::ToolUse,
                        ..
                    }
                )
            })
            .expect("ToolUse start must be emitted");

        let completed_with_sig = events.iter().find_map(|e| match e {
            StreamEvent::ContentBlockCompleted {
                signature: Some((SignatureProvider::Gemini, sig)),
                ..
            } => Some(sig.as_str()),
            _ => None,
        });
        assert_eq!(
            completed_with_sig,
            Some("func_call_signature_base64"),
            "function-call signature must ride the tool-use ContentBlockCompleted"
        );

        // Critical: NO Reasoning block is emitted for a function-call
        // signature. The "function-call signature wins → reasoning block"
        // hack is gone.
        let has_reasoning_start = events.iter().any(|e| {
            matches!(
                e,
                StreamEvent::ContentBlockStart {
                    block_type: ContentBlockType::Reasoning,
                    ..
                }
            )
        });
        assert!(
            !has_reasoning_start,
            "function-call signatures must NOT spawn a synthetic Reasoning block"
        );
        let _ = tool_start_index;
    }

    /// Test: Mixed thought and regular text parts are processed as
    /// separate, ordered blocks. The reasoning block must close (with its
    /// `ContentBlockCompleted`) before the text block opens, so each part
    /// keeps its own per-part signature attribution downstream.
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

        // Five events: reasoning start + delta + completed (closes the
        // reasoning part when the text part begins), then text start + delta.
        // The text block stays open until the next non-continuation part
        // or finalize.
        assert_eq!(
            events.len(),
            5,
            "expected 5 events for [thought, text]; got: {events:#?}"
        );

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
        assert!(matches!(
            &events[2],
            StreamEvent::ContentBlockCompleted {
                index: 0,
                signature: None,
            }
        ));
        assert!(matches!(
            &events[3],
            StreamEvent::ContentBlockStart {
                index: 1,
                block_type: ContentBlockType::Text,
                ..
            }
        ));
        assert!(matches!(
            &events[4],
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

    /// Incremental-delta text (each chunk = only the new tail) accumulates
    /// into one block at one index, so markdown spans whose pair straddles
    /// a chunk boundary (`**bold` then `text**`) stay intact.
    #[test]
    fn test_incremental_text_deltas_accumulate_into_one_block() {
        let mut parser = create_test_parser();

        let fragments = ["###", " Release", " Notes **fast", " mode** enabled."];
        for frag in &fragments {
            let chunk = json!({
                "candidates": [{
                    "content": {
                        "parts": [{ "text": *frag }]
                    }
                }]
            });
            parser.handle_chunk(chunk).unwrap();
        }

        let events: Vec<_> = parser.pending.drain(..).collect();

        // Exactly one ContentBlockStart for text...
        let starts: Vec<_> = events
            .iter()
            .filter(|e| {
                matches!(
                    e,
                    StreamEvent::ContentBlockStart {
                        block_type: ContentBlockType::Text,
                        ..
                    }
                )
            })
            .collect();
        assert_eq!(
            starts.len(),
            1,
            "incremental deltas must reuse one text block; got starts={starts:#?}"
        );

        // ...and every TextDelta carries the same index.
        let text_deltas: Vec<&str> = events
            .iter()
            .filter_map(|e| match e {
                StreamEvent::TextDelta { index, text } => {
                    assert_eq!(*index, 0, "all incremental deltas share block 0");
                    Some(text.as_str())
                }
                _ => None,
            })
            .collect();
        assert_eq!(text_deltas, fragments);
    }

    /// Mixed cumulative + incremental chunks in one stream stay in one
    /// block: the cumulative branch emits only the new suffix even when
    /// `accumulated_text` was last extended by an incremental chunk.
    #[test]
    fn test_mixed_cumulative_and_incremental_text_stay_in_one_block() {
        let mut parser = create_test_parser();

        // 1st chunk: incremental seed.
        parser
            .handle_chunk(json!({
                "candidates": [{ "content": { "parts": [{ "text": "Hello" }] } }]
            }))
            .unwrap();
        // 2nd chunk: incremental (does not start with accumulated).
        parser
            .handle_chunk(json!({
                "candidates": [{ "content": { "parts": [{ "text": " world" }] } }]
            }))
            .unwrap();
        // 3rd chunk: cumulative resend of the full accumulated text + new tail.
        parser
            .handle_chunk(json!({
                "candidates": [{ "content": { "parts": [{ "text": "Hello world!" }] } }]
            }))
            .unwrap();

        let events: Vec<_> = parser.pending.drain(..).collect();

        let starts = events
            .iter()
            .filter(|e| {
                matches!(
                    e,
                    StreamEvent::ContentBlockStart {
                        block_type: ContentBlockType::Text,
                        ..
                    }
                )
            })
            .count();
        assert_eq!(starts, 1, "mixed-mode chunks must stay in one block");

        let text_deltas: Vec<&str> = events
            .iter()
            .filter_map(|e| match e {
                StreamEvent::TextDelta { index: 0, text } => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(
            text_deltas,
            vec!["Hello", " world", "!"],
            "incremental chunks emit verbatim; cumulative chunk emits only its new suffix"
        );
    }

    /// Signature-only same-kind chunk (empty text + `thoughtSignature`)
    /// updates the open part's signature without emitting an empty delta
    /// or opening a new block.
    #[test]
    fn test_signature_only_same_kind_chunk_does_not_open_new_block() {
        let mut parser = create_test_parser();

        parser
            .handle_chunk(json!({
                "candidates": [{
                    "content": {
                        "parts": [{ "thought": true, "text": "Thinking..." }]
                    }
                }]
            }))
            .unwrap();
        parser.pending.clear();

        // Second chunk: same kind, empty text, with a signature.
        parser
            .handle_chunk(json!({
                "candidates": [{
                    "content": {
                        "parts": [{
                            "thought": true,
                            "text": "",
                            "thoughtSignature": "sig-xyz"
                        }]
                    }
                }]
            }))
            .unwrap();

        let events: Vec<_> = parser.pending.drain(..).collect();
        // No new ContentBlockStart, no spurious empty ReasoningDelta.
        for event in &events {
            match event {
                StreamEvent::ContentBlockStart { .. } => {
                    panic!("signature-only chunk must not open a new block; got: {event:?}")
                }
                StreamEvent::ReasoningDelta { reasoning, .. } if reasoning.is_empty() => {
                    panic!("signature-only chunk must not emit empty delta; got: {event:?}")
                }
                _ => {}
            }
        }
    }

    /// Adjacent same-kind text parts in one SSE chunk stay as distinct
    /// blocks so each keeps its own signature. Cross-chunk same-kind
    /// continuation is unaffected.
    #[test]
    fn test_adjacent_same_kind_parts_in_one_chunk_stay_distinct() {
        let mut parser = create_test_parser();

        parser
            .handle_chunk(json!({
                "candidates": [{
                    "content": {
                        "parts": [
                            { "text": "first block", "thoughtSignature": "sig-A" },
                            { "text": "second block", "thoughtSignature": "sig-B" }
                        ]
                    }
                }]
            }))
            .unwrap();

        let events: Vec<_> = parser.pending.drain(..).collect();

        // Two ContentBlockStart::Text, at consecutive indices.
        let starts: Vec<usize> = events
            .iter()
            .filter_map(|e| match e {
                StreamEvent::ContentBlockStart {
                    block_type: ContentBlockType::Text,
                    index,
                    ..
                } => Some(*index),
                _ => None,
            })
            .collect();
        assert_eq!(
            starts,
            vec![0, 1],
            "adjacent same-kind text parts in one chunk must open distinct blocks; events={events:#?}"
        );

        // First block closes with sig-A; second block stays open.
        let completed: Vec<_> = events
            .iter()
            .filter_map(|e| match e {
                StreamEvent::ContentBlockCompleted { index, signature } => {
                    Some((*index, signature.clone()))
                }
                _ => None,
            })
            .collect();
        assert_eq!(
            completed.len(),
            1,
            "only the first block is closed mid-chunk"
        );
        let (closed_index, sig) = &completed[0];
        assert_eq!(*closed_index, 0);
        assert!(
            matches!(sig, Some((SignatureProvider::Gemini, s)) if s == "sig-A"),
            "first block must close with its own sig-A; got {sig:?}"
        );

        // Flush via terminal finishReason and confirm block 1 closes with
        // sig-B (signatures don't bleed across the same-chunk split).
        parser
            .handle_chunk(json!({
                "candidates": [{
                    "content": { "parts": [] },
                    "finishReason": "STOP"
                }]
            }))
            .unwrap();
        let terminal_events: Vec<_> = parser.pending.drain(..).collect();
        let block_one_close = terminal_events.iter().find_map(|e| match e {
            StreamEvent::ContentBlockCompleted {
                index: 1,
                signature,
            } => Some(signature.clone()),
            _ => None,
        });
        assert!(
            matches!(
                block_one_close,
                Some(Some((SignatureProvider::Gemini, ref s))) if s == "sig-B"
            ),
            "second block must close with sig-B at terminal flush; got {block_one_close:?}"
        );
    }

    /// `[empty+sig, non-empty]` same-chunk same-kind parts produce one
    /// signed block, not an empty signed block plus an unsigned text
    /// block. Regression guard for the empty-skip rule in `process_parts`.
    #[test]
    fn test_empty_signature_only_part_does_not_split_following_text() {
        let mut parser = create_test_parser();

        parser
            .handle_chunk(json!({
                "candidates": [{
                    "content": {
                        "parts": [
                            { "text": "", "thoughtSignature": "sig-attached" },
                            { "text": "real text after signature" }
                        ]
                    }
                }]
            }))
            .unwrap();
        // Flush via finishReason so we can inspect the closing event.
        parser
            .handle_chunk(json!({
                "candidates": [{
                    "content": { "parts": [] },
                    "finishReason": "STOP"
                }]
            }))
            .unwrap();

        let events: Vec<_> = parser.pending.drain(..).collect();

        let starts: Vec<usize> = events
            .iter()
            .filter_map(|e| match e {
                StreamEvent::ContentBlockStart {
                    block_type: ContentBlockType::Text,
                    index,
                    ..
                } => Some(*index),
                _ => None,
            })
            .collect();
        assert_eq!(
            starts,
            vec![0],
            "signature-only + non-empty same-kind parts must share one block; events={events:#?}"
        );

        let deltas: Vec<&str> = events
            .iter()
            .filter_map(|e| match e {
                StreamEvent::TextDelta { index: 0, text } => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(
            deltas,
            vec!["real text after signature"],
            "delta must come from the non-empty part; empty part emits no delta"
        );

        let close = events.iter().find_map(|e| match e {
            StreamEvent::ContentBlockCompleted {
                index: 0,
                signature,
            } => Some(signature.clone()),
            _ => None,
        });
        assert!(
            matches!(
                close,
                Some(Some((SignatureProvider::Gemini, ref s))) if s == "sig-attached"
            ),
            "the single block must close with the attached signature; got {close:?}"
        );
    }

    /// Signatures are sticky: a later same-kind chunk with `signature:
    /// None` must not erase a signature already attached to the open part.
    #[test]
    fn test_signature_is_sticky_across_chunks_without_signature() {
        let mut parser = create_test_parser();

        // Open with signature.
        parser
            .handle_chunk(json!({
                "candidates": [{
                    "content": {
                        "parts": [{
                            "thought": true,
                            "text": "Let me think",
                            "thoughtSignature": "sig-keep-me"
                        }]
                    }
                }]
            }))
            .unwrap();
        parser.pending.clear();

        // Cumulative resend without signature — must not erase it.
        parser
            .handle_chunk(json!({
                "candidates": [{
                    "content": {
                        "parts": [{
                            "thought": true,
                            "text": "Let me think further"
                        }]
                    }
                }]
            }))
            .unwrap();
        parser.pending.clear();

        // Flush and inspect the emitted signature.
        parser
            .handle_chunk(json!({
                "candidates": [{
                    "finishReason": "STOP",
                    "content": { "parts": [] }
                }]
            }))
            .unwrap();

        let events: Vec<_> = parser.pending.drain(..).collect();
        let sig_delta = events.iter().find_map(|e| match e {
            StreamEvent::ReasoningSignatureDelta { signature, .. } => Some(signature.clone()),
            _ => None,
        });
        assert_eq!(
            sig_delta.as_deref(),
            Some("sig-keep-me"),
            "signature must persist across chunks that omit it; events={events:#?}"
        );
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

    /// Test: Invalid UTF-8 in the byte stream is a real protocol/decoding bug,
    /// not a transient transport blip, and MUST stay non-retryable so the
    /// engine surfaces it as a fatal turn failure instead of silently retrying.
    #[tokio::test]
    async fn test_utf8_error_is_not_retryable() {
        use futures_util::StreamExt;

        let byte_stream = stream::iter(vec![Ok::<bytes::Bytes, std::io::Error>(
            bytes::Bytes::from_static(&[0xF0, 0x9F]),
        )]);
        let mut parser =
            GeminiSseParser::new(byte_stream, "gemini-3-flash-preview".to_string(), "test");

        let first = parser
            .next()
            .await
            .expect("stream should yield the utf8 error");
        let err = first.expect_err("invalid utf-8 must surface as Err");

        assert_eq!(
            err.kind,
            ProviderErrorKind::Parse,
            "utf-8 framing error must stay classified as Parse",
        );
        assert!(
            !err.is_retryable(),
            "utf-8 framing errors must not be retryable, got {err:?}",
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

    // ──────────────────────────────────────────────────────────────────────
    // Gate 2.2: per-part fidelity — one ContentBlock* event sequence per
    // Gemini part, in original order, with per-part signatures and
    // `IdOrigin` attribution on tool-use blocks.
    // ──────────────────────────────────────────────────────────────────────

    /// Two consecutive text parts each carrying their own `thoughtSignature`
    /// must produce two distinct `ContentBlockCompleted` events with the
    /// matching per-part signatures, in original order. Today's parser
    /// merged everything into a single text block — this test guards the
    /// new in-order walk.
    #[test]
    fn test_per_part_signatures_emitted_in_order() {
        let mut parser = create_test_parser();

        // Two text parts arriving in separate chunks. A function-call part
        // between them forces the first text block to close (so its
        // signature is attached) and the second text block to open as a
        // fresh part.
        parser
            .handle_chunk(json!({
                "candidates": [{
                    "content": {
                        "parts": [{
                            "text": "first sentence.",
                            "thoughtSignature": "c2lnX3RleHRfMQ=="
                        }]
                    }
                }]
            }))
            .unwrap();
        parser
            .handle_chunk(json!({
                "candidates": [{
                    "content": {
                        "parts": [{
                            "functionCall": {
                                "id": "call_real_001",
                                "name": "noop",
                                "args": {}
                            },
                            "thoughtSignature": "c2lnX2NhbGw="
                        }]
                    }
                }]
            }))
            .unwrap();
        parser
            .handle_chunk(json!({
                "candidates": [{
                    "content": {
                        "parts": [{
                            "text": "second sentence.",
                            "thoughtSignature": "c2lnX3RleHRfMg=="
                        }]
                    }
                }]
            }))
            .unwrap();
        parser
            .handle_chunk(json!({
                "candidates": [{ "finishReason": "STOP" }]
            }))
            .unwrap();

        let events: Vec<_> = parser.pending.drain(..).collect();

        // Collect ContentBlockCompleted events with their (provider, sig)
        // payload, in order.
        let completions: Vec<(usize, Option<(SignatureProvider, String)>)> = events
            .iter()
            .filter_map(|e| match e {
                StreamEvent::ContentBlockCompleted { index, signature } => {
                    Some((*index, signature.clone()))
                }
                _ => None,
            })
            .collect();

        assert_eq!(
            completions.len(),
            3,
            "expected 3 ContentBlockCompleted events (text, tool_use, text), got: {completions:#?}"
        );

        let text_sigs: Vec<&str> = events
            .iter()
            .filter_map(|e| match e {
                StreamEvent::ContentBlockCompleted {
                    signature: Some((SignatureProvider::Gemini, sig)),
                    ..
                } => Some(sig.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(
            text_sigs,
            vec!["c2lnX3RleHRfMQ==", "c2lnX2NhbGw=", "c2lnX3RleHRfMg=="],
            "per-part signatures must ride their own ContentBlockCompleted events in order"
        );
    }

    /// A `functionCall` part with a real `id` must produce a
    /// `ContentBlockStart` carrying `id_origin: Some(Real)` so the request
    /// builder later replays the id verbatim.
    #[test]
    fn test_function_call_id_origin_real() {
        let mut parser = create_test_parser();

        parser
            .handle_chunk(json!({
                "candidates": [{
                    "content": {
                        "parts": [{
                            "functionCall": {
                                "id": "call_real_001",
                                "name": "read_file",
                                "args": {"path": "README.md"}
                            }
                        }]
                    }
                }]
            }))
            .unwrap();

        let start = parser
            .pending
            .iter()
            .find(|e| {
                matches!(
                    e,
                    StreamEvent::ContentBlockStart {
                        block_type: ContentBlockType::ToolUse,
                        ..
                    }
                )
            })
            .expect("ToolUse start expected");

        match start {
            StreamEvent::ContentBlockStart { id, id_origin, .. } => {
                assert_eq!(id.as_deref(), Some("call_real_001"));
                assert_eq!(*id_origin, Some(IdOrigin::Real));
            }
            _ => unreachable!(),
        }
    }

    /// A `functionCall` part WITHOUT an `id` must still produce a
    /// `ContentBlockStart`, but with `id_origin: Some(Synthesized)`. The
    /// id itself is synthesized from `(prefix, run_id, index)` for engine
    /// correlation; the origin marker is what tells the request builder
    /// to omit it on replay.
    #[test]
    fn test_function_call_id_origin_synthesized() {
        let mut parser = create_test_parser();

        parser
            .handle_chunk(json!({
                "candidates": [{
                    "content": {
                        "parts": [{
                            "functionCall": {
                                "name": "list_dir",
                                "args": {"path": "src"}
                            }
                        }]
                    }
                }]
            }))
            .unwrap();

        let start = parser
            .pending
            .iter()
            .find(|e| {
                matches!(
                    e,
                    StreamEvent::ContentBlockStart {
                        block_type: ContentBlockType::ToolUse,
                        ..
                    }
                )
            })
            .expect("ToolUse start expected");

        match start {
            StreamEvent::ContentBlockStart { id, id_origin, .. } => {
                let id = id.as_deref().expect("synthesized id must be set");
                assert!(!id.is_empty());
                assert!(
                    id.starts_with(&format!("test-{}", parser.run_id)),
                    "synthesized id should use the parser's prefix+run_id, got {id}"
                );
                assert_eq!(*id_origin, Some(IdOrigin::Synthesized));
            }
            _ => unreachable!(),
        }
    }

    /// Two consecutive thought parts must each open their own Reasoning
    /// block (closing the prior one with its own signature first).
    /// Pre-2.2 the parser merged all thought parts in a chunk into a
    /// single Reasoning block, which destroyed per-part signature
    /// attribution.
    #[test]
    fn test_thought_part_emits_own_block() {
        let mut parser = create_test_parser();

        // Two thought parts in different chunks, each with its own
        // signature. They must NOT be merged into a single Reasoning block.
        parser
            .handle_chunk(json!({
                "candidates": [{
                    "content": {
                        "parts": [{
                            "thought": true,
                            "text": "first thought.",
                            "thoughtSignature": "c2lnXzE="
                        }]
                    }
                }]
            }))
            .unwrap();

        // A function call between the two thoughts forces the first
        // reasoning block to close so its signature can be attached.
        parser
            .handle_chunk(json!({
                "candidates": [{
                    "content": {
                        "parts": [{
                            "functionCall": {
                                "id": "call_x",
                                "name": "noop",
                                "args": {}
                            }
                        }]
                    }
                }]
            }))
            .unwrap();

        parser
            .handle_chunk(json!({
                "candidates": [{
                    "content": {
                        "parts": [{
                            "thought": true,
                            "text": "second thought.",
                            "thoughtSignature": "c2lnXzI="
                        }]
                    }
                }]
            }))
            .unwrap();

        parser
            .handle_chunk(json!({
                "candidates": [{ "finishReason": "STOP" }]
            }))
            .unwrap();

        let events: Vec<_> = parser.pending.drain(..).collect();

        let reasoning_starts = events
            .iter()
            .filter(|e| {
                matches!(
                    e,
                    StreamEvent::ContentBlockStart {
                        block_type: ContentBlockType::Reasoning,
                        ..
                    }
                )
            })
            .count();
        assert_eq!(
            reasoning_starts, 2,
            "two thought parts must emit two distinct Reasoning blocks"
        );

        let signature_deltas: Vec<&str> = events
            .iter()
            .filter_map(|e| match e {
                StreamEvent::ReasoningSignatureDelta { signature, .. } => Some(signature.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(
            signature_deltas,
            vec!["c2lnXzE=", "c2lnXzI="],
            "each Reasoning block must carry its own signature in order"
        );
    }

    /// The full multipart fixture used by the engine-level golden test
    /// must produce one `ContentBlockStart` per Gemini part, in original
    /// order: `[Reasoning, Text, ToolUse, Text, ToolUse]`.
    #[tokio::test]
    async fn test_multipart_fixture_per_part_order() {
        use ContentBlockType::{Reasoning, Text, ToolUse};
        use futures_util::StreamExt;

        let fixture = std::fs::read(
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../zdx-engine/tests/fixtures/gemini/multipart_turn.sse"),
        )
        .expect("fixture must exist");

        let byte_stream = stream::iter(vec![Ok::<_, std::io::Error>(Bytes::from(fixture))]);
        let mut parser =
            GeminiSseParser::new(byte_stream, "gemini-3-pro-preview".to_string(), "gemini");
        let mut events = Vec::new();
        while let Some(item) = parser.next().await {
            events.push(item.expect("fixture is valid"));
        }

        let block_types: Vec<ContentBlockType> = events
            .iter()
            .filter_map(|e| match e {
                StreamEvent::ContentBlockStart { block_type, .. } => Some(*block_type),
                _ => None,
            })
            .collect();

        assert_eq!(
            block_types,
            vec![Reasoning, Text, ToolUse, Text, ToolUse],
            "per-part order must match the original Gemini stream"
        );
    }
}
