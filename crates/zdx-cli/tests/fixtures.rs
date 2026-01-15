//! SSE fixture helpers for integration tests.

#![allow(dead_code)]

use wiremock::ResponseTemplate;

// Load fixture templates at compile time
pub const SSE_TEXT: &str = include_str!("fixtures/sse_text_response.sse");
pub const SSE_TOOL_USE: &str = include_str!("fixtures/sse_tool_use_response.sse");
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
}
