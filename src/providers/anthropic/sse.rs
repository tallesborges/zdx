use std::pin::Pin;

use anyhow::{Context, Result, bail};
use futures_util::Stream;
use serde::Deserialize;

use crate::providers::shared::{ContentBlockType, StreamEvent, Usage};

/// SSE parser that converts a byte stream into StreamEvents.
pub struct SseParser<S> {
    inner: S,
    buffer: Vec<u8>,
}

impl<S> SseParser<S> {
    pub fn new(stream: S) -> Self {
        Self {
            inner: stream,
            buffer: Vec::new(),
        }
    }
}

impl<S, E> Stream for SseParser<S>
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
            // Check if we have a complete event in the buffer
            if let Some(event) = self.try_parse_event() {
                return Poll::Ready(Some(event));
            }

            // Try to get more data from the underlying stream
            let inner = Pin::new(&mut self.inner);
            match inner.poll_next(cx) {
                Poll::Ready(Some(Ok(bytes))) => {
                    self.buffer.extend_from_slice(&bytes);
                    // Continue looping to parse
                }
                Poll::Ready(Some(Err(e))) => {
                    return Poll::Ready(Some(Err(anyhow::anyhow!("Stream error: {}", e))));
                }
                Poll::Ready(None) => {
                    // Stream ended - check for any remaining buffered event
                    let is_empty = self.buffer.iter().all(|b| b.is_ascii_whitespace());
                    if is_empty {
                        return Poll::Ready(None);
                    }
                    // Try to parse remaining buffer
                    if let Some(event) = self.try_parse_event() {
                        return Poll::Ready(Some(event));
                    }
                    return Poll::Ready(None);
                }
                Poll::Pending => return Poll::Pending,
            }
        }
    }
}

impl<S> SseParser<S> {
    /// Tries to parse a complete SSE event from the buffer.
    /// Returns None if no complete event is available yet.
    fn try_parse_event(&mut self) -> Option<Result<StreamEvent>> {
        // SSE events are separated by double newlines
        // Handle both LF (\n\n) and CRLF (\r\n\r\n) line endings
        let (event_end, delim_len) = find_double_newline(&self.buffer)?;

        // Extract the event bytes and remove from buffer
        let event_bytes: Vec<u8> = self.buffer.drain(..event_end).collect();
        self.buffer.drain(..delim_len); // remove the delimiter

        // Decode UTF-8 only after we have the complete event
        let event_text = match std::str::from_utf8(&event_bytes) {
            Ok(s) => s,
            Err(e) => return Some(Err(anyhow::anyhow!("Invalid UTF-8 in SSE event: {}", e))),
        };

        Some(parse_sse_event(event_text))
    }
}

/// Parses a single SSE event block into a StreamEvent.
pub fn parse_sse_event(event_text: &str) -> Result<StreamEvent> {
    let mut event_type = None;
    let mut data = None;

    for line in event_text.lines() {
        if let Some(value) = line.strip_prefix("event: ") {
            event_type = Some(value.trim());
        } else if let Some(value) = line.strip_prefix("data: ") {
            data = Some(value);
        }
    }

    let event_type = event_type.unwrap_or("message");

    match event_type {
        "ping" => Ok(StreamEvent::Ping),
        "message_start" => {
            let data = data.context("Missing data for message_start event")?;
            let parsed: SseMessageStart =
                serde_json::from_str(data).context("Failed to parse message_start")?;
            Ok(StreamEvent::MessageStart {
                model: parsed.message.model,
                usage: parsed.message.usage.into(),
            })
        }
        "content_block_start" => {
            let data = data.context("Missing data for content_block_start event")?;
            let parsed: SseContentBlockStart =
                serde_json::from_str(data).context("Failed to parse content_block_start")?;
            let block_type = parsed
                .content_block
                .block_type
                .parse::<ContentBlockType>()
                .map_err(|e| anyhow::anyhow!(e))?;
            Ok(StreamEvent::ContentBlockStart {
                index: parsed.index,
                block_type,
                id: parsed.content_block.id,
                name: parsed.content_block.name,
            })
        }
        "content_block_delta" => {
            let data = data.context("Missing data for content_block_delta event")?;
            let parsed: SseContentBlockDelta =
                serde_json::from_str(data).context("Failed to parse content_block_delta")?;
            match parsed.delta.delta_type.as_str() {
                "text_delta" => Ok(StreamEvent::TextDelta {
                    index: parsed.index,
                    text: parsed.delta.text.unwrap_or_default(),
                }),
                "input_json_delta" => Ok(StreamEvent::InputJsonDelta {
                    index: parsed.index,
                    partial_json: parsed.delta.partial_json.unwrap_or_default(),
                }),
                "thinking_delta" => Ok(StreamEvent::ReasoningDelta {
                    index: parsed.index,
                    reasoning: parsed.delta.thinking.unwrap_or_default(),
                }),
                "signature_delta" => Ok(StreamEvent::ReasoningSignatureDelta {
                    index: parsed.index,
                    signature: parsed.delta.signature.unwrap_or_default(),
                }),
                other => bail!("Unknown delta type: {}", other),
            }
        }
        "content_block_stop" => {
            let data = data.context("Missing data for content_block_stop event")?;
            let parsed: SseContentBlockCompleted =
                serde_json::from_str(data).context("Failed to parse content_block_stop")?;
            Ok(StreamEvent::ContentBlockCompleted {
                index: parsed.index,
            })
        }
        "message_delta" => {
            let data = data.context("Missing data for message_delta event")?;
            let parsed: SseMessageDelta =
                serde_json::from_str(data).context("Failed to parse message_delta")?;
            Ok(StreamEvent::MessageDelta {
                stop_reason: parsed.delta.stop_reason.clone(),
                usage: parsed.usage.map(|u| u.into()),
            })
        }
        "message_stop" => Ok(StreamEvent::MessageCompleted),
        "error" => {
            let data = data.context("Missing data for error event")?;
            let parsed: SseError = serde_json::from_str(data).context("Failed to parse error")?;
            Ok(StreamEvent::Error {
                error_type: parsed.error.error_type,
                message: parsed.error.message,
            })
        }
        other => bail!("Unknown SSE event type: {}", other),
    }
}

// === SSE Response Structures ===

#[derive(Debug, Deserialize)]
struct SseMessageStart {
    message: SseMessageInfo,
}

#[derive(Debug, Deserialize)]
struct SseMessageInfo {
    model: String,
    #[serde(default)]
    usage: SseUsage,
}

/// Usage data from Anthropic SSE events.
#[derive(Debug, Default, Deserialize)]
struct SseUsage {
    #[serde(default)]
    input_tokens: u64,
    #[serde(default)]
    output_tokens: u64,
    #[serde(default)]
    cache_read_input_tokens: u64,
    #[serde(default)]
    cache_creation_input_tokens: u64,
}

impl From<SseUsage> for Usage {
    fn from(u: SseUsage) -> Self {
        Usage {
            input_tokens: u.input_tokens,
            output_tokens: u.output_tokens,
            cache_read_input_tokens: u.cache_read_input_tokens,
            cache_creation_input_tokens: u.cache_creation_input_tokens,
        }
    }
}

#[derive(Debug, Deserialize)]
struct SseContentBlockStart {
    index: usize,
    content_block: SseContentBlock,
}

#[derive(Debug, Deserialize)]
struct SseContentBlock {
    #[serde(rename = "type")]
    block_type: String,
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    name: Option<String>,
}

#[derive(Debug, Deserialize)]
struct SseContentBlockDelta {
    index: usize,
    delta: SseDelta,
}

#[derive(Debug, Deserialize)]
struct SseDelta {
    #[serde(rename = "type")]
    delta_type: String,
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    partial_json: Option<String>,
    #[serde(default)]
    thinking: Option<String>,
    #[serde(default)]
    signature: Option<String>,
}

#[derive(Debug, Deserialize)]
struct SseContentBlockCompleted {
    index: usize,
}

#[derive(Debug, Deserialize)]
struct SseMessageDelta {
    delta: SseMessageDeltaInner,
    #[serde(default)]
    usage: Option<SseUsage>,
}

#[derive(Debug, Deserialize)]
struct SseMessageDeltaInner {
    #[serde(default)]
    stop_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
struct SseError {
    error: SseErrorInfo,
}

#[derive(Debug, Deserialize)]
struct SseErrorInfo {
    #[serde(rename = "type")]
    error_type: String,
    message: String,
}

/// Finds the position of a double newline in the buffer.
/// Handles both LF (\n\n) and CRLF (\r\n\r\n) line endings.
/// Returns the position and the length of the delimiter (2 or 4 bytes).
fn find_double_newline(buffer: &[u8]) -> Option<(usize, usize)> {
    let crlf_pos = buffer.windows(4).position(|w| w == b"\r\n\r\n");
    let lf_pos = buffer.windows(2).position(|w| w == b"\n\n");

    match (crlf_pos, lf_pos) {
        (Some(c), Some(l)) => {
            // Return whichever comes first
            if l <= c { Some((l, 2)) } else { Some((c, 4)) }
        }
        (Some(c), None) => Some((c, 4)),
        (None, Some(l)) => Some((l, 2)),
        (None, None) => None,
    }
}

#[cfg(test)]
mod tests {
    use futures_util::StreamExt;

    use super::*;

    /// SSE fixture simulating a typical Anthropic streaming response
    const SSE_TEXT_RESPONSE: &str = r#"event: message_start
data: {"type":"message_start","message":{"id":"msg_123","type":"message","role":"assistant","content":[],"model":"claude-sonnet-4-20250514","stop_reason":null,"stop_sequence":null,"usage":{"input_tokens":10,"output_tokens":1}}}

event: content_block_start
data: {"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}

event: ping
data: {"type":"ping"}

event: content_block_delta
data: {"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hello"}}

event: content_block_delta
data: {"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":" world"}}

event: content_block_delta
data: {"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"!"}}

event: content_block_stop
data: {"type":"content_block_stop","index":0}

event: message_delta
data: {"type":"message_delta","delta":{"stop_reason":"end_turn","stop_sequence":null},"usage":{"output_tokens":5}}

event: message_stop
data: {"type":"message_stop"}

"#;

    /// SSE fixture simulating a tool use streaming response
    const SSE_TOOL_USE_RESPONSE: &str = r#"event: message_start
data: {"type":"message_start","message":{"id":"msg_456","type":"message","role":"assistant","content":[],"model":"claude-sonnet-4-20250514","stop_reason":null,"stop_sequence":null,"usage":{"input_tokens":20,"output_tokens":1}}}

event: content_block_start
data: {"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"toolu_abc123","name":"get_weather"}}

event: content_block_delta
data: {"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{\""}}

event: content_block_delta
data: {"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"location\": \"San"}}

event: content_block_delta
data: {"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":" Francisco\"}"}}

event: content_block_stop
data: {"type":"content_block_stop","index":0}

event: message_delta
data: {"type":"message_delta","delta":{"stop_reason":"tool_use","stop_sequence":null},"usage":{"output_tokens":25}}

event: message_stop
data: {"type":"message_stop"}

"#;

    /// SSE fixture simulating an error mid-stream
    const SSE_ERROR_RESPONSE: &str = r#"event: message_start
data: {"type":"message_start","message":{"id":"msg_789","type":"message","role":"assistant","content":[],"model":"claude-sonnet-4-20250514","stop_reason":null,"stop_sequence":null,"usage":{"input_tokens":10,"output_tokens":1}}}

event: error
data: {"type":"error","error":{"type":"overloaded_error","message":"API is temporarily overloaded"}}

"#;

    /// Helper to create a mock byte stream from a string
    fn mock_byte_stream(
        data: &str,
    ) -> impl Stream<Item = std::result::Result<bytes::Bytes, std::io::Error>> {
        let chunks: Vec<_> = data
            .as_bytes()
            .chunks(50) // Simulate chunked delivery
            .map(|c| Ok(bytes::Bytes::copy_from_slice(c)))
            .collect();
        futures_util::stream::iter(chunks)
    }

    #[tokio::test]
    async fn test_sse_parser_text_response() {
        let stream = mock_byte_stream(SSE_TEXT_RESPONSE);
        let mut parser = SseParser::new(stream);

        let mut events = Vec::new();
        while let Some(result) = parser.next().await {
            events.push(result.expect("Expected valid event"));
        }

        // Verify we got all expected events
        assert_eq!(events.len(), 9);

        // Check specific events
        assert!(
            matches!(&events[0], StreamEvent::MessageStart { model, .. } if model == "claude-sonnet-4-20250514")
        );
        assert!(matches!(
            &events[1],
            StreamEvent::ContentBlockStart {
                index: 0,
                block_type,
                ..
            } if *block_type == ContentBlockType::Text
        ));
        assert_eq!(events[2], StreamEvent::Ping);
        assert_eq!(
            events[3],
            StreamEvent::TextDelta {
                index: 0,
                text: "Hello".to_string()
            }
        );
        assert_eq!(
            events[4],
            StreamEvent::TextDelta {
                index: 0,
                text: " world".to_string()
            }
        );
        assert_eq!(
            events[5],
            StreamEvent::TextDelta {
                index: 0,
                text: "!".to_string()
            }
        );
        assert_eq!(events[6], StreamEvent::ContentBlockCompleted { index: 0 });
        assert!(matches!(
            &events[7],
            StreamEvent::MessageDelta {
                stop_reason: Some(reason),
                ..
            } if reason == "end_turn"
        ));
        assert_eq!(events[8], StreamEvent::MessageCompleted);
    }

    #[tokio::test]
    async fn test_sse_parser_tool_use_response() {
        let stream = mock_byte_stream(SSE_TOOL_USE_RESPONSE);
        let mut parser = SseParser::new(stream);

        let mut events = Vec::new();
        while let Some(result) = parser.next().await {
            events.push(result.expect("Expected valid event"));
        }

        // Verify we got all expected events
        assert_eq!(events.len(), 8);

        // Check tool_use specific events
        assert!(matches!(
            &events[1],
            StreamEvent::ContentBlockStart {
                index: 0,
                block_type,
                id: Some(id),
                name: Some(name),
            } if *block_type == ContentBlockType::ToolUse
                && id == "toolu_abc123"
                && name == "get_weather"
        ));

        // Check input_json_delta events
        assert_eq!(
            events[2],
            StreamEvent::InputJsonDelta {
                index: 0,
                partial_json: "{\"".to_string()
            }
        );
        assert_eq!(
            events[3],
            StreamEvent::InputJsonDelta {
                index: 0,
                partial_json: "location\": \"San".to_string()
            }
        );
        assert_eq!(
            events[4],
            StreamEvent::InputJsonDelta {
                index: 0,
                partial_json: " Francisco\"}".to_string()
            }
        );

        // Check stop_reason is tool_use
        assert!(matches!(
            &events[6],
            StreamEvent::MessageDelta {
                stop_reason: Some(reason),
                ..
            } if reason == "tool_use"
        ));
    }

    #[tokio::test]
    async fn test_sse_parser_error_response() {
        let stream = mock_byte_stream(SSE_ERROR_RESPONSE);
        let mut parser = SseParser::new(stream);

        let mut events = Vec::new();
        while let Some(result) = parser.next().await {
            events.push(result.expect("Expected valid event"));
        }

        // Verify we got the error event
        assert_eq!(events.len(), 2);
        assert_eq!(
            events[1],
            StreamEvent::Error {
                error_type: "overloaded_error".to_string(),
                message: "API is temporarily overloaded".to_string()
            }
        );
    }

    #[tokio::test]
    async fn test_sse_parser_handles_incomplete_chunks() {
        // Simulate receiving data in very small chunks that split across event boundaries
        let data = r#"event: ping
data: {"type":"ping"}

event: message_stop
data: {"type":"message_stop"}

"#;
        let chunks: Vec<std::result::Result<bytes::Bytes, std::io::Error>> = data
            .as_bytes()
            .chunks(10) // Very small chunks
            .map(|c| Ok(bytes::Bytes::copy_from_slice(c)))
            .collect();
        let stream = futures_util::stream::iter(chunks);
        let mut parser = SseParser::new(stream);

        let mut events = Vec::new();
        while let Some(result) = parser.next().await {
            events.push(result.expect("Expected valid event"));
        }

        assert_eq!(events.len(), 2);
        assert_eq!(events[0], StreamEvent::Ping);
        assert_eq!(events[1], StreamEvent::MessageCompleted);
    }

    #[tokio::test]
    async fn test_sse_parser_handles_crlf_line_endings() {
        // Simulate HTTP response with CRLF line endings (Windows-style / HTTP standard)
        let data = "event: ping\r\ndata: {\"type\":\"ping\"}\r\n\r\nevent: message_stop\r\ndata: {\"type\":\"message_stop\"}\r\n\r\n";
        let stream = mock_byte_stream(data);
        let mut parser = SseParser::new(stream);

        let mut events = Vec::new();
        while let Some(result) = parser.next().await {
            events.push(result.expect("Expected valid event"));
        }

        assert_eq!(events.len(), 2);
        assert_eq!(events[0], StreamEvent::Ping);
        assert_eq!(events[1], StreamEvent::MessageCompleted);
    }

    #[tokio::test]
    async fn test_sse_parser_handles_mixed_line_endings() {
        // First event uses LF, second uses CRLF - parser should find earliest delimiter
        let data = "event: ping\ndata: {\"type\":\"ping\"}\n\nevent: message_stop\r\ndata: {\"type\":\"message_stop\"}\r\n\r\n";
        let stream = mock_byte_stream(data);
        let mut parser = SseParser::new(stream);

        let mut events = Vec::new();
        while let Some(result) = parser.next().await {
            events.push(result.expect("Expected valid event"));
        }

        assert_eq!(events.len(), 2);
        assert_eq!(events[0], StreamEvent::Ping);
        assert_eq!(events[1], StreamEvent::MessageCompleted);
    }

    #[tokio::test]
    async fn test_sse_parser_handles_utf8_split_across_chunks() {
        // Test that multi-byte UTF-8 characters split across TCP chunks are handled correctly.
        // 👋 = F0 9F 91 8B (4 bytes) - splitting this would corrupt with from_utf8_lossy
        let data = r#"event: content_block_delta
data: {"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hello 👋 world"}}

"#;
        let bytes = data.as_bytes();

        // Find the start of the emoji (F0 byte) and split inside it
        let emoji_start = bytes
            .windows(4)
            .position(|w| w == [0xF0, 0x9F, 0x91, 0x8B])
            .expect("emoji not found");

        // Split right in the middle of the emoji (after 2 of 4 bytes)
        let split_point = emoji_start + 2;

        let chunks: Vec<std::result::Result<bytes::Bytes, std::io::Error>> = vec![
            Ok(bytes::Bytes::copy_from_slice(&bytes[..split_point])),
            Ok(bytes::Bytes::copy_from_slice(&bytes[split_point..])),
        ];

        let stream = futures_util::stream::iter(chunks);
        let mut parser = SseParser::new(stream);

        let event = parser
            .next()
            .await
            .unwrap()
            .expect("should parse valid event");

        // Verify the emoji is intact, not corrupted with replacement characters
        assert_eq!(
            event,
            StreamEvent::TextDelta {
                index: 0,
                text: "Hello 👋 world".to_string()
            }
        );
    }

    /// SSE fixture simulating a thinking response with interleaved thinking and text
    const SSE_THINKING_RESPONSE: &str = r#"event: message_start
data: {"type":"message_start","message":{"id":"msg_think","type":"message","role":"assistant","content":[],"model":"claude-sonnet-4-20250514","stop_reason":null,"stop_sequence":null,"usage":{"input_tokens":10,"output_tokens":1}}}

event: content_block_start
data: {"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":""}}

event: content_block_delta
data: {"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"Let me think"}}

event: content_block_delta
data: {"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":" about this..."}}

event: content_block_delta
data: {"type":"content_block_delta","index":0,"delta":{"type":"signature_delta","signature":"abc123sig"}}

event: content_block_stop
data: {"type":"content_block_stop","index":0}

event: content_block_start
data: {"type":"content_block_start","index":1,"content_block":{"type":"text","text":""}}

event: content_block_delta
data: {"type":"content_block_delta","index":1,"delta":{"type":"text_delta","text":"Here is my response."}}

event: content_block_stop
data: {"type":"content_block_stop","index":1}

event: message_delta
data: {"type":"message_delta","delta":{"stop_reason":"end_turn","stop_sequence":null},"usage":{"output_tokens":50}}

event: message_stop
data: {"type":"message_stop"}

"#;

    #[tokio::test]
    async fn test_sse_parser_thinking_response() {
        let stream = mock_byte_stream(SSE_THINKING_RESPONSE);
        let mut parser = SseParser::new(stream);

        let mut events = Vec::new();
        while let Some(result) = parser.next().await {
            events.push(result.expect("Expected valid event"));
        }

        // Verify we got all expected events
        assert_eq!(events.len(), 11);

        // Check message_start
        assert!(
            matches!(&events[0], StreamEvent::MessageStart { model, .. } if model == "claude-sonnet-4-20250514")
        );

        // Check thinking block start
        assert!(matches!(
            &events[1],
            StreamEvent::ContentBlockStart {
                index: 0,
                block_type,
                ..
            } if *block_type == ContentBlockType::Reasoning
        ));

        // Check thinking deltas
        assert_eq!(
            events[2],
            StreamEvent::ReasoningDelta {
                index: 0,
                reasoning: "Let me think".to_string()
            }
        );
        assert_eq!(
            events[3],
            StreamEvent::ReasoningDelta {
                index: 0,
                reasoning: " about this...".to_string()
            }
        );

        // Check signature delta
        assert_eq!(
            events[4],
            StreamEvent::ReasoningSignatureDelta {
                index: 0,
                signature: "abc123sig".to_string()
            }
        );

        // Check thinking block stop
        assert_eq!(events[5], StreamEvent::ContentBlockCompleted { index: 0 });

        // Check text block start
        assert!(matches!(
            &events[6],
            StreamEvent::ContentBlockStart {
                index: 1,
                block_type,
                ..
            } if *block_type == ContentBlockType::Text
        ));

        // Check text delta
        assert_eq!(
            events[7],
            StreamEvent::TextDelta {
                index: 1,
                text: "Here is my response.".to_string()
            }
        );

        // Check text block stop
        assert_eq!(events[8], StreamEvent::ContentBlockCompleted { index: 1 });

        // Check message delta and stop
        assert!(matches!(
            &events[9],
            StreamEvent::MessageDelta {
                stop_reason: Some(reason),
                ..
            } if reason == "end_turn"
        ));
        assert_eq!(events[10], StreamEvent::MessageCompleted);

        // Log actual events for debugging if needed
        // for (i, e) in events.iter().enumerate() {
        //     println!("{}: {:?}", i, e);
        // }
    }
}
