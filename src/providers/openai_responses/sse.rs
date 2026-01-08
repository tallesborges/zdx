//! SSE parsing for OpenAI-compatible Responses streaming.

use std::collections::VecDeque;
use std::pin::Pin;

use anyhow::{Result, anyhow};
use futures_util::Stream;
use serde_json::Value;

use crate::providers::anthropic::{StreamEvent, Usage};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BlockKind {
    Text,
    Tool,
}

#[derive(Debug)]
struct StreamState {
    next_index: usize,
    current_index: Option<usize>,
    current_kind: Option<BlockKind>,
    saw_tool: bool,
}

impl StreamState {
    fn new() -> Self {
        Self {
            next_index: 0,
            current_index: None,
            current_kind: None,
            saw_tool: false,
        }
    }
}

/// SSE parser for OpenAI Responses API.
pub struct ResponsesSseParser<S> {
    inner: S,
    buffer: Vec<u8>,
    model: String,
    state: StreamState,
    pending: VecDeque<StreamEvent>,
}

impl<S> ResponsesSseParser<S> {
    pub fn new(stream: S, model: String) -> Self {
        Self {
            inner: stream,
            buffer: Vec::new(),
            model,
            state: StreamState::new(),
            pending: VecDeque::new(),
        }
    }

    fn try_next_event(&mut self) -> Option<Result<StreamEvent>> {
        if let Some(event) = self.pending.pop_front() {
            return Some(Ok(event));
        }

        let (pos, delim_len) = find_double_newline(&self.buffer)?;

        let chunk = self.buffer.drain(..pos).collect::<Vec<u8>>();
        self.buffer.drain(..delim_len); // remove "\n\n" or "\r\n\r\n"

        let chunk_text = String::from_utf8_lossy(&chunk);
        let data = match parse_sse_data(&chunk_text) {
            Ok(value) => value,
            Err(err) => return Some(Err(err)),
        };
        let event = match data {
            Some(value) => match self.map_event(value) {
                Ok(event) => event,
                Err(err) => return Some(Err(err)),
            },
            None => return None,
        };
        Some(Ok(event))
    }

    fn map_event(&mut self, value: Value) -> Result<StreamEvent> {
        let event_type = value.get("type").and_then(|v| v.as_str()).unwrap_or("");

        match event_type {
            "response.output_item.added" => {
                let item = value.get("item").unwrap_or(&Value::Null);
                let item_type = item.get("type").and_then(|v| v.as_str()).unwrap_or("");
                match item_type {
                    "message" => {
                        let index = self.state.next_index;
                        self.state.next_index += 1;
                        self.state.current_index = Some(index);
                        self.state.current_kind = Some(BlockKind::Text);
                        Ok(StreamEvent::ContentBlockStart {
                            index,
                            block_type: "text".to_string(),
                            id: None,
                            name: None,
                        })
                    }
                    "function_call" => {
                        let index = self.state.next_index;
                        self.state.next_index += 1;
                        self.state.current_index = Some(index);
                        self.state.current_kind = Some(BlockKind::Tool);
                        self.state.saw_tool = true;

                        let call_id = item.get("call_id").and_then(|v| v.as_str()).unwrap_or("");
                        let id = item.get("id").and_then(|v| v.as_str()).unwrap_or("");
                        let name = item.get("name").and_then(|v| v.as_str()).unwrap_or("");
                        let tool_id = if !call_id.is_empty() && !id.is_empty() {
                            format!("{}|{}", call_id, id)
                        } else {
                            format!("{}{}", call_id, id)
                        };

                        Ok(StreamEvent::ContentBlockStart {
                            index,
                            block_type: "tool_use".to_string(),
                            id: Some(tool_id),
                            name: Some(name.to_string()),
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
                let delta = value
                    .get("delta")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                Ok(StreamEvent::TextDelta { index, text: delta })
            }
            "response.function_call_arguments.delta" => {
                if self.state.current_kind != Some(BlockKind::Tool) {
                    return Ok(StreamEvent::Ping);
                }
                let index = self.state.current_index.unwrap_or(0);
                let delta = value
                    .get("delta")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                Ok(StreamEvent::InputJsonDelta {
                    index,
                    partial_json: delta,
                })
            }
            "response.output_item.done" => {
                if let Some(index) = self.state.current_index.take() {
                    self.state.current_kind = None;
                    Ok(StreamEvent::ContentBlockStop { index })
                } else {
                    Ok(StreamEvent::Ping)
                }
            }
            "response.completed" | "response.done" => {
                let response = value.get("response").unwrap_or(&Value::Null);
                let usage = response.get("usage").unwrap_or(&Value::Null);
                let cached = usage
                    .get("input_tokens_details")
                    .and_then(|v| v.get("cached_tokens"))
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0);
                let input_tokens = usage
                    .get("input_tokens")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0)
                    .saturating_sub(cached);
                let output_tokens = usage
                    .get("output_tokens")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0);

                let usage = Usage {
                    input_tokens,
                    output_tokens,
                    cache_read_input_tokens: cached,
                    cache_creation_input_tokens: 0,
                };

                let status = response
                    .get("status")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let stop_reason = match status {
                    "incomplete" => "max_tokens",
                    "failed" | "cancelled" => "error",
                    _ => "stop",
                };

                let stop_reason = if self.state.saw_tool {
                    Some("tool_use".to_string())
                } else {
                    Some(stop_reason.to_string())
                };

                self.pending.push_back(StreamEvent::MessageStart {
                    model: self.model.clone(),
                    usage: usage.clone(),
                });
                self.pending.push_back(StreamEvent::MessageDelta {
                    stop_reason,
                    usage: Some(usage),
                });
                self.pending.push_back(StreamEvent::MessageStop);

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
                let message = value
                    .get("message")
                    .and_then(|v| v.as_str())
                    .unwrap_or("Unknown error")
                    .to_string();
                Ok(StreamEvent::Error {
                    error_type,
                    message,
                })
            }
            _ => Ok(StreamEvent::Ping),
        }
    }
}

impl<S, E> Stream for ResponsesSseParser<S>
where
    S: Stream<Item = std::result::Result<bytes::Bytes, E>> + Unpin,
    E: std::error::Error + Send + Sync + 'static,
{
    type Item = Result<StreamEvent>;

    fn poll_next(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        use std::task::Poll;

        loop {
            if let Some(event) = self.try_next_event() {
                return Poll::Ready(Some(event));
            }

            let inner = Pin::new(&mut self.inner);
            match inner.poll_next(cx) {
                Poll::Ready(Some(Ok(bytes))) => {
                    self.buffer.extend_from_slice(&bytes);
                }
                Poll::Ready(Some(Err(e))) => {
                    return Poll::Ready(Some(Err(anyhow!("Stream error: {}", e))));
                }
                Poll::Ready(None) => {
                    let is_empty = self.buffer.iter().all(|b| b.is_ascii_whitespace());
                    if is_empty {
                        return Poll::Ready(None);
                    }
                    if let Some(event) = self.try_next_event() {
                        return Poll::Ready(Some(event));
                    }
                    return Poll::Ready(None);
                }
                Poll::Pending => return Poll::Pending,
            }
        }
    }
}

/// Finds the position of a double newline in the buffer.
/// Handles both LF (\n\n) and CRLF (\r\n\r\n) line endings.
/// Returns the position and the length of the delimiter (2 or 4 bytes).
fn find_double_newline(buffer: &[u8]) -> Option<(usize, usize)> {
    let crlf_pos = buffer.windows(4).position(|w| w == b"\r\n\r\n");
    let lf_pos = buffer.windows(2).position(|w| w == b"\n\n");

    match (crlf_pos, lf_pos) {
        (Some(c), Some(l)) => {
            if l <= c {
                Some((l, 2))
            } else {
                Some((c, 4))
            }
        }
        (Some(c), None) => Some((c, 4)),
        (None, Some(l)) => Some((l, 2)),
        (None, None) => None,
    }
}

fn parse_sse_data(chunk: &str) -> Result<Option<Value>> {
    let mut data_lines = Vec::new();
    for line in chunk.lines() {
        if let Some(rest) = line.strip_prefix("data:") {
            data_lines.push(rest.trim());
        }
    }
    if data_lines.is_empty() {
        return Ok(None);
    }
    let data = data_lines.join("\n");
    let trimmed = data.trim();
    if trimmed.is_empty() || trimmed == "[DONE]" {
        return Ok(None);
    }
    let value = serde_json::from_str::<Value>(trimmed)
        .map_err(|err| anyhow!("Failed to parse SSE JSON: {}", err))?;
    Ok(Some(value))
}
