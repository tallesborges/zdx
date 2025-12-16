use assert_cmd::cargo::cargo_bin_cmd;
use predicates::prelude::*;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// Helper to create an SSE response from event strings
fn sse_response(events: &[&str]) -> ResponseTemplate {
    let body = events.join("\n\n") + "\n\n";
    ResponseTemplate::new(200)
        .insert_header("content-type", "text/event-stream")
        .set_body_string(body)
}

/// Creates SSE events for a simple text streaming response
fn simple_text_sse(text_chunks: &[&str]) -> Vec<String> {
    let mut events = vec![
        r#"event: message_start
data: {"type":"message_start","message":{"id":"msg_123","type":"message","role":"assistant","content":[],"model":"claude-sonnet-4-20250514","stop_reason":null,"stop_sequence":null,"usage":{"input_tokens":10,"output_tokens":1}}}"#.to_string(),
        r#"event: content_block_start
data: {"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}"#.to_string(),
    ];

    for chunk in text_chunks {
        events.push(format!(
            r#"event: content_block_delta
data: {{"type":"content_block_delta","index":0,"delta":{{"type":"text_delta","text":"{}"}}}}"#,
            chunk
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

    events
}

#[tokio::test]
async fn test_exec_streams_text_response() {
    let mock_server = MockServer::start().await;

    let events = simple_text_sse(&["Hello", ", ", "world", "!"]);
    let events_refs: Vec<&str> = events.iter().map(|s| s.as_str()).collect();

    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .and(header("x-api-key", "test-api-key"))
        .and(header("anthropic-version", "2023-06-01"))
        .respond_with(sse_response(&events_refs))
        .mount(&mock_server)
        .await;

    cargo_bin_cmd!("zdx-cli")
        .env("ANTHROPIC_API_KEY", "test-api-key")
        .env("ANTHROPIC_BASE_URL", mock_server.uri())
        .args(["exec", "-p", "hello"])
        .assert()
        .success()
        // Verify the full text was received (order preserved from streaming)
        .stdout(predicate::str::contains("Hello, world!"));
}

#[tokio::test]
async fn test_exec_streaming_preserves_text_order() {
    let mock_server = MockServer::start().await;

    // Send chunks in specific order - the output should preserve this order
    let events = simple_text_sse(&["Rust ", "is ", "a ", "systems ", "language."]);
    let events_refs: Vec<&str> = events.iter().map(|s| s.as_str()).collect();

    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(sse_response(&events_refs))
        .mount(&mock_server)
        .await;

    cargo_bin_cmd!("zdx-cli")
        .env("ANTHROPIC_API_KEY", "test-api-key")
        .env("ANTHROPIC_BASE_URL", mock_server.uri())
        .args(["exec", "-p", "describe rust"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Rust is a systems language."));
}

#[tokio::test]
async fn test_exec_handles_empty_delta_events() {
    let mock_server = MockServer::start().await;

    // Include empty deltas that should be skipped
    let events = vec![
        r#"event: message_start
data: {"type":"message_start","message":{"id":"msg_123","type":"message","role":"assistant","content":[],"model":"claude-sonnet-4-20250514","stop_reason":null,"stop_sequence":null,"usage":{"input_tokens":10,"output_tokens":1}}}"#.to_string(),
        r#"event: content_block_start
data: {"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}"#.to_string(),
        // Empty delta - should be skipped
        r#"event: content_block_delta
data: {"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":""}}"#.to_string(),
        r#"event: content_block_delta
data: {"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hello"}}"#.to_string(),
        // Another empty delta
        r#"event: content_block_delta
data: {"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":""}}"#.to_string(),
        r#"event: content_block_stop
data: {"type":"content_block_stop","index":0}"#.to_string(),
        r#"event: message_delta
data: {"type":"message_delta","delta":{"stop_reason":"end_turn","stop_sequence":null},"usage":{"output_tokens":1}}"#.to_string(),
        r#"event: message_stop
data: {"type":"message_stop"}"#.to_string(),
    ];

    let events_refs: Vec<&str> = events.iter().map(|s| s.as_str()).collect();

    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(sse_response(&events_refs))
        .mount(&mock_server)
        .await;

    cargo_bin_cmd!("zdx-cli")
        .env("ANTHROPIC_API_KEY", "test-api-key")
        .env("ANTHROPIC_BASE_URL", mock_server.uri())
        .args(["exec", "-p", "hello"])
        .assert()
        .success()
        // Only "Hello" should appear, empty deltas skipped
        .stdout(predicate::str::is_match(r"^Hello\n$").unwrap());
}

#[tokio::test]
async fn test_exec_fails_without_api_key() {
    cargo_bin_cmd!("zdx-cli")
        .env_remove("ANTHROPIC_API_KEY")
        .args(["exec", "-p", "hello"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("ANTHROPIC_API_KEY"));
}

#[tokio::test]
async fn test_exec_handles_api_error() {
    let mock_server = MockServer::start().await;

    let error_body = serde_json::json!({
        "type": "error",
        "error": {
            "type": "authentication_error",
            "message": "Invalid API key"
        }
    });

    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(401).set_body_json(error_body))
        .mount(&mock_server)
        .await;

    cargo_bin_cmd!("zdx-cli")
        .env("ANTHROPIC_API_KEY", "invalid-key")
        .env("ANTHROPIC_BASE_URL", mock_server.uri())
        .args(["exec", "-p", "hello"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("401"));
}

#[tokio::test]
async fn test_exec_handles_api_error_midstream() {
    let mock_server = MockServer::start().await;

    // Start streaming, then send an error
    let events = vec![
        r#"event: message_start
data: {"type":"message_start","message":{"id":"msg_123","type":"message","role":"assistant","content":[],"model":"claude-sonnet-4-20250514","stop_reason":null,"stop_sequence":null,"usage":{"input_tokens":10,"output_tokens":1}}}"#,
        r#"event: content_block_start
data: {"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}"#,
        r#"event: content_block_delta
data: {"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Starting..."}}"#,
        r#"event: error
data: {"type":"error","error":{"type":"overloaded_error","message":"API is temporarily overloaded"}}"#,
    ];

    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(sse_response(&events))
        .mount(&mock_server)
        .await;

    cargo_bin_cmd!("zdx-cli")
        .env("ANTHROPIC_API_KEY", "test-api-key")
        .env("ANTHROPIC_BASE_URL", mock_server.uri())
        .args(["exec", "-p", "hello"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("overloaded_error"))
        .stderr(predicate::str::contains("API is temporarily overloaded"));
}

#[tokio::test]
async fn test_exec_tool_use_midstream() {
    let mock_server = MockServer::start().await;

    // First response: tool_use request
    let tool_use_events = vec![
        r#"event: message_start
data: {"type":"message_start","message":{"id":"msg_123","type":"message","role":"assistant","content":[],"model":"claude-sonnet-4-20250514","stop_reason":null,"stop_sequence":null,"usage":{"input_tokens":10,"output_tokens":1}}}"#,
        r#"event: content_block_start
data: {"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"toolu_abc123","name":"bash"}}"#,
        r#"event: content_block_delta
data: {"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{\"command\""}}"#,
        r#"event: content_block_delta
data: {"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":": \"echo hello\"}"}}"#,
        r#"event: content_block_stop
data: {"type":"content_block_stop","index":0}"#,
        r#"event: message_delta
data: {"type":"message_delta","delta":{"stop_reason":"tool_use","stop_sequence":null},"usage":{"output_tokens":20}}"#,
        r#"event: message_stop
data: {"type":"message_stop"}"#,
    ];

    // Second response: final text after tool result
    let final_events = simple_text_sse(&["The command output was: hello"]);
    let final_events_refs: Vec<&str> = final_events.iter().map(|s| s.as_str()).collect();

    // Mount first response (tool use)
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(sse_response(&tool_use_events))
        .up_to_n_times(1)
        .mount(&mock_server)
        .await;

    // Mount second response (final text after tool execution)
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(sse_response(&final_events_refs))
        .mount(&mock_server)
        .await;

    cargo_bin_cmd!("zdx-cli")
        .env("ANTHROPIC_API_KEY", "test-api-key")
        .env("ANTHROPIC_BASE_URL", mock_server.uri())
        .args(["exec", "-p", "run echo hello"])
        .assert()
        .success()
        // Final response after tool execution
        .stdout(predicate::str::contains("The command output was: hello"));
}

#[tokio::test]
async fn test_exec_handles_ping_events() {
    let mock_server = MockServer::start().await;

    // Include ping events which should be ignored
    let events = vec![
        r#"event: message_start
data: {"type":"message_start","message":{"id":"msg_123","type":"message","role":"assistant","content":[],"model":"claude-sonnet-4-20250514","stop_reason":null,"stop_sequence":null,"usage":{"input_tokens":10,"output_tokens":1}}}"#,
        r#"event: ping
data: {"type":"ping"}"#,
        r#"event: content_block_start
data: {"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}"#,
        r#"event: ping
data: {"type":"ping"}"#,
        r#"event: content_block_delta
data: {"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Pong!"}}"#,
        r#"event: content_block_stop
data: {"type":"content_block_stop","index":0}"#,
        r#"event: message_delta
data: {"type":"message_delta","delta":{"stop_reason":"end_turn","stop_sequence":null},"usage":{"output_tokens":1}}"#,
        r#"event: message_stop
data: {"type":"message_stop"}"#,
    ];

    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(sse_response(&events))
        .mount(&mock_server)
        .await;

    cargo_bin_cmd!("zdx-cli")
        .env("ANTHROPIC_API_KEY", "test-api-key")
        .env("ANTHROPIC_BASE_URL", mock_server.uri())
        .args(["exec", "-p", "ping"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Pong!"));
}
