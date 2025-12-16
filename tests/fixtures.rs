//! SSE fixture helpers for integration tests.
//!
//! Load `.sse` templates from `tests/fixtures/` and substitute placeholders.

#![allow(dead_code)]

use wiremock::ResponseTemplate;

// Load fixture templates at compile time
pub const SSE_TEXT: &str = include_str!("fixtures/sse_text_response.sse");
pub const SSE_TOOL_USE: &str = include_str!("fixtures/sse_tool_use_response.sse");
pub const SSE_ERROR: &str = include_str!("fixtures/sse_error_midstream.sse");
pub const SSE_TEXT_WITH_PINGS: &str = include_str!("fixtures/sse_text_with_pings.sse");
pub const SSE_TEXT_WITH_TOOL_USE: &str = include_str!("fixtures/sse_text_with_tool_use.sse");

/// Create a text SSE response with the given content.
pub fn text_sse(text: &str) -> String {
    SSE_TEXT.replace("{{TEXT}}", &escape_json(text))
}

/// Create a tool_use SSE response.
pub fn tool_use_sse(tool_id: &str, tool_name: &str, input_json: &str) -> String {
    SSE_TOOL_USE
        .replace("{{TOOL_ID}}", tool_id)
        .replace("{{TOOL_NAME}}", tool_name)
        .replace("{{INPUT_JSON}}", &escape_json(input_json))
}

/// Create a tool_use SSE response with preceding text.
pub fn text_and_tool_use_sse(
    text: &str,
    tool_id: &str,
    tool_name: &str,
    input_json: &str,
) -> String {
    SSE_TEXT_WITH_TOOL_USE
        .replace("{{TEXT}}", &escape_json(text))
        .replace("{{TOOL_ID}}", tool_id)
        .replace("{{TOOL_NAME}}", tool_name)
        .replace("{{INPUT_JSON}}", &escape_json(input_json))
}

/// Create an error SSE response (mid-stream).
pub fn error_sse(error_type: &str, message: &str) -> String {
    SSE_ERROR
        .replace("{{ERROR_TYPE}}", error_type)
        .replace("{{ERROR_MESSAGE}}", &escape_json(message))
}

/// Create a text SSE response with ping events.
pub fn text_sse_with_pings(text: &str) -> String {
    SSE_TEXT_WITH_PINGS.replace("{{TEXT}}", &escape_json(text))
}

/// Build SSE response with multiple text chunks (for streaming tests).
pub fn multi_chunk_text_sse(chunks: &[&str]) -> String {
    let mut events = vec![
        r#"event: message_start
data: {"type":"message_start","message":{"id":"msg_123","type":"message","role":"assistant","content":[],"model":"claude-sonnet-4-20250514","stop_reason":null,"stop_sequence":null,"usage":{"input_tokens":10,"output_tokens":1}}}"#.to_string(),
        r#"event: content_block_start
data: {"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}"#.to_string(),
    ];

    for chunk in chunks {
        events.push(format!(
            r#"event: content_block_delta
data: {{"type":"content_block_delta","index":0,"delta":{{"type":"text_delta","text":"{}"}}}}"#,
            escape_json(chunk)
        ));
    }

    events.push(
        r#"event: content_block_stop
data: {"type":"content_block_stop","index":0}"#
            .to_string(),
    );
    events.push(
        r#"event: message_delta
data: {"type":"message_delta","delta":{"stop_reason":"end_turn","stop_sequence":null},"usage":{"output_tokens":5}}"#.to_string(),
    );
    events.push(
        r#"event: message_stop
data: {"type":"message_stop"}"#
            .to_string(),
    );

    events.join("\n\n") + "\n\n"
}

/// Wrap SSE body string in a ResponseTemplate.
pub fn sse_response(body: &str) -> ResponseTemplate {
    ResponseTemplate::new(200)
        .insert_header("content-type", "text/event-stream")
        .set_body_string(body.to_string())
}

/// Convenience: text SSE wrapped in ResponseTemplate.
pub fn text_response(text: &str) -> ResponseTemplate {
    sse_response(&text_sse(text))
}

/// Convenience: tool_use SSE wrapped in ResponseTemplate.
pub fn tool_use_response(tool_id: &str, tool_name: &str, input_json: &str) -> ResponseTemplate {
    sse_response(&tool_use_sse(tool_id, tool_name, input_json))
}

/// Escape special characters for JSON string embedding.
fn escape_json(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
        .replace('\t', "\\t")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_text_sse_substitution() {
        let result = text_sse("Hello, world!");
        assert!(result.contains(r#""text":"Hello, world!""#));
        assert!(result.contains("event: message_start"));
        assert!(result.contains("event: message_stop"));
    }

    #[test]
    fn test_tool_use_sse_substitution() {
        let result = tool_use_sse("toolu_123", "read", r#"{"path":"file.txt"}"#);
        assert!(result.contains(r#""id":"toolu_123""#));
        assert!(result.contains(r#""name":"read""#));
        assert!(result.contains(r#"\"path\":\"file.txt\""#));
    }

    #[test]
    fn test_escape_json_handles_quotes() {
        let result = escape_json(r#"say "hello""#);
        assert_eq!(result, r#"say \"hello\""#);
    }

    #[test]
    fn test_escape_json_handles_newlines() {
        let result = escape_json("line1\nline2");
        assert_eq!(result, r"line1\nline2");
    }

    #[test]
    fn test_multi_chunk_produces_multiple_deltas() {
        let result = multi_chunk_text_sse(&["Hello", ", ", "world!"]);
        // Count "event: content_block_delta" occurrences (each delta event line)
        let delta_count = result.matches("event: content_block_delta").count();
        assert_eq!(delta_count, 3);
    }
}
