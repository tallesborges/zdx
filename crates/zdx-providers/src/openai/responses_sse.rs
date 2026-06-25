//! SSE parsing for OpenAI-compatible Responses streaming.

use std::collections::VecDeque;
use std::pin::Pin;

use eventsource_stream::{EventStream, Eventsource};
use futures_util::Stream;
use serde_json::Value;

use crate::{
    ContentBlockType, ProviderError, ProviderErrorKind, ProviderResult, StreamEvent, Usage,
    error_message_from_payload, map_event_stream_error,
};

/// Extension trait for extracting strings from JSON values.
trait JsonExt {
    /// Get a string field, returning empty string if missing or not a string.
    fn get_str(&self, key: &str) -> &str;
    /// Get a string field as owned String, returning empty string if missing.
    fn get_string(&self, key: &str) -> String;
}

impl JsonExt for Value {
    fn get_str(&self, key: &str) -> &str {
        self.get(key).and_then(|v| v.as_str()).unwrap_or("")
    }

    fn get_string(&self, key: &str) -> String {
        self.get_str(key).to_string()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BlockKind {
    Text,
    Tool,
    Reasoning,
}

/// State for tracking a reasoning item being streamed.
#[derive(Debug, Clone)]
struct ReasoningState {
    index: usize,
    id: String,
    summary: String,
}

#[derive(Debug)]
struct StreamState {
    next_index: usize,
    current_index: Option<usize>,
    current_kind: Option<BlockKind>,
    current_tool_argument_bytes: usize,
    saw_tool: bool,
    /// Tracks reasoning item being streamed (for summary replay)
    current_reasoning: Option<ReasoningState>,
}

impl StreamState {
    fn new() -> Self {
        Self {
            next_index: 0,
            current_index: None,
            current_kind: None,
            current_tool_argument_bytes: 0,
            saw_tool: false,
            current_reasoning: None,
        }
    }
}

fn extract_function_call_arguments(item: &Value) -> Option<String> {
    if let Some(arguments) = item.get("arguments") {
        return if let Some(text) = arguments.as_str() {
            Some(text.to_string())
        } else if arguments.is_null() {
            None
        } else {
            Some(arguments.to_string())
        };
    }

    item.get("input").and_then(|input| {
        if let Some(text) = input.as_str() {
            Some(text.to_string())
        } else if input.is_null() {
            None
        } else {
            Some(input.to_string())
        }
    })
}

/// Maps `OpenAI` Responses JSON events to `StreamEvent`s. The payloads are
/// byte-identical over SSE and WebSocket, so both transports feed this mapper.
pub struct ResponsesEventMapper {
    model: String,
    state: StreamState,
    pending: VecDeque<StreamEvent>,
    last_response_id: Option<String>,
}

impl ResponsesEventMapper {
    pub fn new(model: String) -> Self {
        Self {
            model,
            state: StreamState::new(),
            pending: VecDeque::new(),
            last_response_id: None,
        }
    }

    /// Parses one JSON event payload and queues the resulting `StreamEvent`(s).
    ///
    /// # Errors
    /// Returns a parse error if the payload is not valid JSON.
    pub fn push_json(&mut self, data: &str) -> ProviderResult<()> {
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
        let event = self.map_event(value)?;
        self.pending.push_back(event);
        Ok(())
    }

    pub fn pop(&mut self) -> Option<StreamEvent> {
        self.pending.pop_front()
    }

    /// Most recent server `response.id` (used for WebSocket continuation).
    pub fn last_response_id(&self) -> Option<&str> {
        self.last_response_id.as_deref()
    }

    #[allow(
        clippy::too_many_lines,
        clippy::needless_pass_by_value,
        clippy::unnecessary_wraps
    )]
    fn map_event(&mut self, value: Value) -> ProviderResult<StreamEvent> {
        let event_type = value.get_str("type");

        match event_type {
            "response.output_item.added" => {
                let item = value.get("item").unwrap_or(&Value::Null);
                let item_type = item.get_str("type");
                match item_type {
                    "message" => {
                        let index = self.state.next_index;
                        self.state.next_index += 1;
                        self.state.current_index = Some(index);
                        self.state.current_kind = Some(BlockKind::Text);
                        Ok(StreamEvent::ContentBlockStart {
                            index,
                            block_type: ContentBlockType::Text,
                            id: None,
                            name: None,
                            data: None,
                            id_origin: None,
                        })
                    }
                    "function_call" => {
                        let index = self.state.next_index;
                        self.state.next_index += 1;
                        self.state.current_index = Some(index);
                        self.state.current_kind = Some(BlockKind::Tool);
                        self.state.current_tool_argument_bytes = 0;
                        self.state.saw_tool = true;

                        let call_id = item.get_str("call_id");
                        let id = item.get_str("id");
                        let name = item.get_str("name");
                        let tool_id = if !call_id.is_empty() && !id.is_empty() {
                            format!("{call_id}|{id}")
                        } else {
                            format!("{call_id}{id}")
                        };

                        Ok(StreamEvent::ContentBlockStart {
                            index,
                            block_type: ContentBlockType::ToolUse,
                            id: Some(tool_id),
                            name: Some(name.to_string()),
                            data: None,
                            id_origin: None,
                        })
                    }
                    "reasoning" => {
                        // Initialize reasoning state for streaming
                        let index = self.state.next_index;
                        self.state.next_index += 1;
                        self.state.current_index = Some(index);
                        self.state.current_kind = Some(BlockKind::Reasoning);

                        self.state.current_reasoning = Some(ReasoningState {
                            index,
                            id: item.get_string("id"),
                            summary: String::new(),
                        });

                        // Emit ContentBlockStart for reasoning so agent.rs tracks it
                        Ok(StreamEvent::ContentBlockStart {
                            index,
                            block_type: ContentBlockType::Reasoning,
                            id: None,
                            name: None,
                            data: None,
                            id_origin: None,
                        })
                    }
                    _ => Ok(StreamEvent::Ping),
                }
            }
            "response.output_text.delta" | "response.refusal.delta" => {
                if self.state.current_kind != Some(BlockKind::Text) {
                    return Ok(StreamEvent::Ping);
                }
                let index = self.state.current_index.unwrap_or(0);
                let delta = value.get_string("delta");
                Ok(StreamEvent::TextDelta { index, text: delta })
            }
            "response.function_call_arguments.delta" => {
                if self.state.current_kind != Some(BlockKind::Tool) {
                    return Ok(StreamEvent::Ping);
                }
                let index = self.state.current_index.unwrap_or(0);
                let delta = value.get_string("delta");
                self.state.current_tool_argument_bytes += delta.len();
                Ok(StreamEvent::InputJsonDelta {
                    index,
                    partial_json: delta,
                })
            }
            "response.function_call_arguments.done" => {
                if self.state.current_kind != Some(BlockKind::Tool) {
                    return Ok(StreamEvent::Ping);
                }

                let Some(arguments) = extract_function_call_arguments(&value) else {
                    if self.state.current_tool_argument_bytes == 0 {
                        // Some providers emit this event without final arguments; wait for
                        // `response.output_item.done` to avoid completing with empty input.
                        return Ok(StreamEvent::Ping);
                    }

                    let index = self.state.current_index.take().unwrap_or(0);
                    self.state.current_kind = None;
                    return Ok(StreamEvent::ContentBlockCompleted {
                        index,
                        signature: None,
                    });
                };

                let index = self.state.current_index.take().unwrap_or(0);
                self.state.current_kind = None;

                let emitted = self.state.current_tool_argument_bytes;
                let remainder = arguments.get(emitted..).unwrap_or("");
                self.state.current_tool_argument_bytes = arguments.len();

                if !remainder.is_empty() {
                    self.pending.push_back(StreamEvent::InputJsonDelta {
                        index,
                        partial_json: remainder.to_string(),
                    });
                    return Ok(StreamEvent::ContentBlockCompleted {
                        index,
                        signature: None,
                    });
                }

                Ok(StreamEvent::ContentBlockCompleted {
                    index,
                    signature: None,
                })
            }
            "response.reasoning_summary_text.delta" => {
                // Stream reasoning summary text incrementally
                if let Some(ref mut reasoning) = self.state.current_reasoning {
                    let delta = value.get_string("delta");
                    reasoning.summary.push_str(&delta);
                    Ok(StreamEvent::ReasoningDelta {
                        index: reasoning.index,
                        reasoning: delta,
                    })
                } else {
                    Ok(StreamEvent::Ping)
                }
            }
            "response.output_item.done" => {
                // Check if this is a reasoning item with encrypted_content
                let item = value.get("item").unwrap_or(&Value::Null);
                let item_type = item.get_str("type");

                if item_type == "reasoning" {
                    // Extract fields from done event for merging
                    let done_id = item.get_string("id");
                    let done_encrypted = item.get_string("encrypted_content");
                    // Extract summary from done event (array of {type, text} objects)
                    let done_summary = item
                        .get("summary")
                        .and_then(|v| v.as_array())
                        .map(|arr| {
                            arr.iter()
                                .filter_map(|item| item.get("text").and_then(|t| t.as_str()))
                                .collect::<Vec<_>>()
                                .join("")
                        })
                        .filter(|s| !s.is_empty());

                    // Merge with current_reasoning state if available
                    let (index, id, encrypted_content, summary, had_streamed_summary) =
                        if let Some(reasoning) = self.state.current_reasoning.take() {
                            // Use done event values for encrypted_content
                            let id = if reasoning.id.is_empty() {
                                done_id
                            } else {
                                reasoning.id
                            };
                            // Prefer streamed summary, fall back to done event summary
                            let had_streamed = !reasoning.summary.is_empty();
                            let summary = if had_streamed {
                                Some(reasoning.summary)
                            } else {
                                done_summary
                            };
                            (reasoning.index, id, done_encrypted, summary, had_streamed)
                        } else {
                            // No current_reasoning state - use done event values directly
                            // This shouldn't happen in normal flow, but handle it gracefully
                            let index = self.state.current_index.unwrap_or(0);
                            (index, done_id, done_encrypted, done_summary, false)
                        };

                    // Emit ReasoningCompleted for storage/replay if we have valid data
                    if !id.is_empty() && !encrypted_content.is_empty() {
                        // Use the reasoning item's stored index for ContentBlockCompleted
                        self.state.current_index = None;
                        self.state.current_kind = None;

                        // If summary wasn't streamed but is present in done event,
                        // emit ReasoningDelta first so downstream can avoid duplicating text.
                        if !had_streamed_summary && let Some(ref text) = summary {
                            let reasoning_text = text.clone();
                            self.pending.push_back(StreamEvent::ReasoningCompleted {
                                index,
                                id,
                                encrypted_content,
                                summary,
                            });
                            self.pending.push_back(StreamEvent::ContentBlockCompleted {
                                index,
                                signature: None,
                            });
                            return Ok(StreamEvent::ReasoningDelta {
                                index,
                                reasoning: reasoning_text,
                            });
                        }

                        self.pending.push_back(StreamEvent::ContentBlockCompleted {
                            index,
                            signature: None,
                        });
                        return Ok(StreamEvent::ReasoningCompleted {
                            index,
                            id,
                            encrypted_content,
                            summary,
                        });
                    }
                }

                if item_type == "function_call" {
                    if self.state.current_kind != Some(BlockKind::Tool)
                        && self.state.current_index.is_none()
                    {
                        return Ok(StreamEvent::Ping);
                    }

                    let index = self.state.current_index.take().unwrap_or(0);
                    self.state.current_kind = None;

                    if let Some(arguments) = extract_function_call_arguments(item) {
                        let emitted = self.state.current_tool_argument_bytes;
                        let remainder = arguments.get(emitted..).unwrap_or("");
                        self.state.current_tool_argument_bytes = arguments.len();

                        if !remainder.is_empty() {
                            self.pending.push_back(StreamEvent::InputJsonDelta {
                                index,
                                partial_json: remainder.to_string(),
                            });
                            return Ok(StreamEvent::ContentBlockCompleted {
                                index,
                                signature: None,
                            });
                        }
                    }

                    return Ok(StreamEvent::ContentBlockCompleted {
                        index,
                        signature: None,
                    });
                }

                if let Some(index) = self.state.current_index.take() {
                    self.state.current_kind = None;
                    self.state.current_tool_argument_bytes = 0;
                    Ok(StreamEvent::ContentBlockCompleted {
                        index,
                        signature: None,
                    })
                } else {
                    Ok(StreamEvent::Ping)
                }
            }
            "response.completed" | "response.done" => {
                let response = value.get("response").unwrap_or(&Value::Null);
                let response_id = response.get_str("id");
                if !response_id.is_empty() {
                    self.last_response_id = Some(response_id.to_string());
                }
                let usage = response.get("usage").unwrap_or(&Value::Null);
                let cached = usage
                    .get("input_tokens_details")
                    .and_then(|v| v.get("cached_tokens"))
                    .and_then(serde_json::Value::as_u64)
                    .unwrap_or(0);
                let input_tokens = usage
                    .get("input_tokens")
                    .and_then(serde_json::Value::as_u64)
                    .unwrap_or(0)
                    .saturating_sub(cached);
                let output_tokens = usage
                    .get("output_tokens")
                    .and_then(serde_json::Value::as_u64)
                    .unwrap_or(0);

                let usage = Usage {
                    input_tokens,
                    output_tokens,
                    cache_read_input_tokens: cached,
                    cache_creation_input_tokens: 0,
                };

                let stop_reason = if self.state.saw_tool {
                    "tool_use"
                } else {
                    match response.get_str("status") {
                        "incomplete" => "max_tokens",
                        "failed" | "cancelled" => "error",
                        _ => "stop",
                    }
                };

                self.pending.push_back(StreamEvent::MessageStart {
                    model: self.model.clone(),
                    usage: usage.clone(),
                });
                self.pending.push_back(StreamEvent::MessageDelta {
                    stop_reason: Some(stop_reason.to_string()),
                    usage: Some(usage.into()),
                });
                self.pending.push_back(StreamEvent::MessageCompleted);

                Ok(self
                    .pending
                    .pop_front()
                    .expect("pending should contain events"))
            }
            "error" => {
                let error_type = value
                    .get("code")
                    .and_then(|v| v.as_str())
                    .unwrap_or("error")
                    .to_string();
                let message = error_message_from_payload(&value, &["message"]);
                Ok(StreamEvent::Error {
                    error_type,
                    message,
                })
            }
            _ => Ok(StreamEvent::Ping),
        }
    }
}

/// SSE parser for `OpenAI` Responses API streaming.
///
/// Owns SSE framing and delegates JSON event payloads to a `ResponsesEventMapper`.
pub struct ResponsesSseParser<S> {
    inner: EventStream<S>,
    mapper: ResponsesEventMapper,
}

impl<S> ResponsesSseParser<S> {
    pub fn new(stream: S, model: String) -> Self
    where
        S: Eventsource,
    {
        Self {
            inner: stream.eventsource(),
            mapper: ResponsesEventMapper::new(model),
        }
    }
}

impl<S, E> Stream for ResponsesSseParser<S>
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
            if let Some(event) = self.mapper.pop() {
                return Poll::Ready(Some(Ok(event)));
            }

            let inner = Pin::new(&mut self.inner);
            match inner.poll_next(cx) {
                Poll::Ready(Some(Ok(event))) => {
                    if let Err(err) = self.mapper.push_json(&event.data) {
                        return Poll::Ready(Some(Err(err)));
                    }
                }
                Poll::Ready(Some(Err(e))) => {
                    return Poll::Ready(Some(Err(map_event_stream_error(e))));
                }
                Poll::Ready(None) => return Poll::Ready(None),
                Poll::Pending => return Poll::Pending,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use futures_util::stream;
    use serde_json::json;

    use super::*;

    fn mapper() -> ResponsesEventMapper {
        ResponsesEventMapper::new("gpt-5.3-codex-spark".to_string())
    }

    #[test]
    fn function_call_done_with_arguments_emits_missing_input_delta() {
        let mut mapper = mapper();

        let start = mapper
            .map_event(json!({
                "type": "response.output_item.added",
                "item": {
                    "type": "function_call",
                    "id": "fc_1",
                    "call_id": "call_1",
                    "name": "bash"
                }
            }))
            .unwrap();
        assert!(matches!(start, StreamEvent::ContentBlockStart { .. }));

        let event = mapper
            .map_event(json!({
                "type": "response.output_item.done",
                "item": {
                    "type": "function_call",
                    "arguments": "{\"command\":\"git status\"}"
                }
            }))
            .unwrap();

        assert!(matches!(
            event,
            StreamEvent::ContentBlockCompleted {
                index: 0,
                signature: None
            }
        ));
        assert!(matches!(
            mapper.pending.pop_front(),
            Some(StreamEvent::InputJsonDelta { ref partial_json, .. }) if partial_json == "{\"command\":\"git status\"}"
        ));
    }

    #[test]
    fn function_call_done_only_emits_remaining_input_after_delta() {
        let mut mapper = mapper();

        let _ = mapper
            .map_event(json!({
                "type": "response.output_item.added",
                "item": {
                    "type": "function_call",
                    "id": "fc_1",
                    "call_id": "call_1",
                    "name": "bash"
                }
            }))
            .unwrap();

        let first = mapper
            .map_event(json!({
                "type": "response.function_call_arguments.delta",
                "delta": "a"
            }))
            .unwrap();
        assert!(matches!(
            first,
            StreamEvent::InputJsonDelta { ref partial_json, .. } if partial_json == "a"
        ));

        let second = mapper
            .map_event(json!({
                "type": "response.output_item.done",
                "item": {
                    "type": "function_call",
                    "arguments": "abc"
                }
            }))
            .unwrap();

        assert!(matches!(
            second,
            StreamEvent::ContentBlockCompleted {
                index: 0,
                signature: None
            }
        ));
        assert!(matches!(
            mapper.pending.pop_front(),
            Some(StreamEvent::InputJsonDelta { ref partial_json, .. }) if partial_json == "bc"
        ));
    }

    #[test]
    fn function_call_arguments_done_emits_missing_input_and_completes() {
        let mut mapper = mapper();

        let _ = mapper
            .map_event(json!({
                "type": "response.output_item.added",
                "item": {
                    "type": "function_call",
                    "id": "fc_1",
                    "call_id": "call_1",
                    "name": "read"
                }
            }))
            .unwrap();

        let done = mapper
            .map_event(json!({
                "type": "response.function_call_arguments.done",
                "arguments": "{\"file_path\":\"Cargo.toml\"}"
            }))
            .unwrap();

        assert!(matches!(
            done,
            StreamEvent::ContentBlockCompleted {
                index: 0,
                signature: None
            }
        ));
        assert!(matches!(
            mapper.pending.pop_front(),
            Some(StreamEvent::InputJsonDelta { ref partial_json, .. }) if partial_json == "{\"file_path\":\"Cargo.toml\"}"
        ));
    }

    #[test]
    fn function_call_output_item_done_after_arguments_done_is_ignored() {
        let mut mapper = mapper();

        let _ = mapper
            .map_event(json!({
                "type": "response.output_item.added",
                "item": {
                    "type": "function_call",
                    "id": "fc_1",
                    "call_id": "call_1",
                    "name": "read"
                }
            }))
            .unwrap();

        let _ = mapper
            .map_event(json!({
                "type": "response.function_call_arguments.done",
                "arguments": "{\"file_path\":\"Cargo.toml\"}"
            }))
            .unwrap();
        let _ = mapper.pending.pop_front();

        let event = mapper
            .map_event(json!({
                "type": "response.output_item.done",
                "item": { "type": "function_call" }
            }))
            .unwrap();

        assert!(matches!(event, StreamEvent::Ping));
    }

    #[test]
    fn function_call_arguments_done_without_arguments_waits_for_output_item_done() {
        let mut mapper = mapper();

        let _ = mapper
            .map_event(json!({
                "type": "response.output_item.added",
                "item": {
                    "type": "function_call",
                    "id": "fc_1",
                    "call_id": "call_1",
                    "name": "bash"
                }
            }))
            .unwrap();

        let early_done = mapper
            .map_event(json!({
                "type": "response.function_call_arguments.done"
            }))
            .unwrap();
        assert!(matches!(early_done, StreamEvent::Ping));

        let final_done = mapper
            .map_event(json!({
                "type": "response.output_item.done",
                "item": {
                    "type": "function_call",
                    "arguments": "{\"command\":\"ls -la\"}"
                }
            }))
            .unwrap();
        assert!(matches!(
            final_done,
            StreamEvent::ContentBlockCompleted {
                index: 0,
                signature: None
            }
        ));
        assert!(matches!(
            mapper.pending.pop_front(),
            Some(StreamEvent::InputJsonDelta { ref partial_json, .. }) if partial_json == "{\"command\":\"ls -la\"}"
        ));
    }

    #[test]
    fn function_call_done_does_not_reemit_when_done_payload_is_shorter() {
        let mut mapper = mapper();

        let _ = mapper
            .map_event(json!({
                "type": "response.output_item.added",
                "item": {
                    "type": "function_call",
                    "id": "fc_1",
                    "call_id": "call_1",
                    "name": "bash"
                }
            }))
            .unwrap();

        let _ = mapper
            .map_event(json!({
                "type": "response.function_call_arguments.delta",
                "delta": "abcd"
            }))
            .unwrap();

        let done = mapper
            .map_event(json!({
                "type": "response.output_item.done",
                "item": {
                    "type": "function_call",
                    "arguments": "abc"
                }
            }))
            .unwrap();

        assert!(matches!(
            done,
            StreamEvent::ContentBlockCompleted {
                index: 0,
                signature: None
            }
        ));
        assert!(mapper.pending.is_empty());
    }

    #[test]
    fn response_completed_captures_response_id() {
        let mut mapper = mapper();

        let event = mapper
            .map_event(json!({
                "type": "response.completed",
                "response": { "id": "resp_123", "status": "completed" }
            }))
            .unwrap();

        assert!(matches!(event, StreamEvent::MessageStart { .. }));
        assert_eq!(mapper.last_response_id(), Some("resp_123"));
    }

    /// Transport-level errors mid-stream (socket reset, connection dropped,
    /// etc.) must surface as a retryable `ProviderError`. Mapping them to
    /// `ProviderErrorKind::Parse` would short-circuit `is_retryable()` to
    /// false and incorrectly treat transient socket failures as fatal.
    #[tokio::test]
    async fn transport_error_is_retryable() {
        use futures_util::StreamExt;

        let byte_stream = stream::iter(vec![Err::<bytes::Bytes, std::io::Error>(
            std::io::Error::new(
                std::io::ErrorKind::ConnectionReset,
                "socket closed mid-stream",
            ),
        )]);
        let mut parser = ResponsesSseParser::new(byte_stream, "gpt-5.3-codex-spark".to_string());

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

    /// Invalid UTF-8 in the byte stream is a real protocol/decoding bug, not a
    /// transient transport blip, and MUST stay non-retryable so the engine
    /// surfaces it as a fatal turn failure instead of silently retrying.
    #[tokio::test]
    async fn utf8_error_is_not_retryable() {
        use futures_util::StreamExt;

        let byte_stream = stream::iter(vec![Ok::<bytes::Bytes, std::io::Error>(
            bytes::Bytes::from_static(&[0xF0, 0x9F]),
        )]);
        let mut parser = ResponsesSseParser::new(byte_stream, "gpt-5.3-codex-spark".to_string());

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
}
