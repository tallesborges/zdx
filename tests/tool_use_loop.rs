//! Tests for the tool use loop with wiremock.
//!
//! Simulates a two-step interaction:
//! 1. First response asks for tool_use(read)
//! 2. Second response returns final text
//! Verifies that the second request includes tool_result block.

use assert_cmd::cargo::cargo_bin_cmd;
use predicates::prelude::*;
use std::fs;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use tempfile::TempDir;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

/// Creates an SSE streaming response for a tool_use request with optional preceding text
fn tool_use_sse_response(
    tool_id: &str,
    tool_name: &str,
    input_json: &str,
    preceding_text: Option<&str>,
) -> String {
    let mut events = format!(
        r#"event: message_start
data: {{"type":"message_start","message":{{"id":"msg_001","type":"message","role":"assistant","content":[],"model":"claude-sonnet-4-20250514","stop_reason":null,"stop_sequence":null,"usage":{{"input_tokens":10,"output_tokens":1}}}}}}

"#
    );

    // If there's preceding text, add text content block
    let tool_index = if let Some(text) = preceding_text {
        events.push_str(&format!(
            r#"event: content_block_start
data: {{"type":"content_block_start","index":0,"content_block":{{"type":"text","text":""}}}}

event: content_block_delta
data: {{"type":"content_block_delta","index":0,"delta":{{"type":"text_delta","text":"{}"}}}}

event: content_block_stop
data: {{"type":"content_block_stop","index":0}}

"#,
            text.replace('"', "\\\"").replace('\n', "\\n")
        ));
        1
    } else {
        0
    };

    events.push_str(&format!(
        r#"event: content_block_start
data: {{"type":"content_block_start","index":{},"content_block":{{"type":"tool_use","id":"{}","name":"{}"}}}}

event: content_block_delta
data: {{"type":"content_block_delta","index":{},"delta":{{"type":"input_json_delta","partial_json":"{}"}}}}

event: content_block_stop
data: {{"type":"content_block_stop","index":{}}}

event: message_delta
data: {{"type":"message_delta","delta":{{"stop_reason":"tool_use","stop_sequence":null}},"usage":{{"output_tokens":20}}}}

event: message_stop
data: {{"type":"message_stop"}}

"#,
        tool_index,
        tool_id,
        tool_name,
        tool_index,
        input_json.replace('"', "\\\"").replace('\n', "\\n"),
        tool_index
    ));

    events
}

/// Creates an SSE streaming response for a final text response
fn text_sse_response(text: &str) -> String {
    format!(
        r#"event: message_start
data: {{"type":"message_start","message":{{"id":"msg_002","type":"message","role":"assistant","content":[],"model":"claude-sonnet-4-20250514","stop_reason":null,"stop_sequence":null,"usage":{{"input_tokens":30,"output_tokens":1}}}}}}

event: content_block_start
data: {{"type":"content_block_start","index":0,"content_block":{{"type":"text","text":""}}}}

event: content_block_delta
data: {{"type":"content_block_delta","index":0,"delta":{{"type":"text_delta","text":"{}"}}}}

event: content_block_stop
data: {{"type":"content_block_stop","index":0}}

event: message_delta
data: {{"type":"message_delta","delta":{{"stop_reason":"end_turn","stop_sequence":null}},"usage":{{"output_tokens":15}}}}

event: message_stop
data: {{"type":"message_stop"}}

"#,
        text.replace('"', "\\\"").replace('\n', "\\n")
    )
}

#[tokio::test]
async fn test_tool_use_loop_reads_file() {
    // Create a temp directory with a test file
    let temp_dir = TempDir::new().unwrap();
    let test_file = temp_dir.path().join("test.txt");
    fs::write(&test_file, "Hello from file!").unwrap();

    let mock_server = MockServer::start().await;
    let call_count = Arc::new(AtomicUsize::new(0));
    let call_count_clone = call_count.clone();

    // First response: model requests to read a file (SSE)
    let first_response = tool_use_sse_response(
        "toolu_001",
        "read",
        r#"{"path": "test.txt"}"#,
        Some("I'll read that file for you."),
    );

    // Second response: model gives final answer (SSE)
    let second_response = text_sse_response("The file contains: Hello from file!");

    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .and(header("x-api-key", "test-api-key"))
        .respond_with(move |_req: &Request| {
            let count = call_count_clone.fetch_add(1, Ordering::SeqCst);
            if count == 0 {
                ResponseTemplate::new(200)
                    .insert_header("content-type", "text/event-stream")
                    .set_body_string(first_response.clone())
            } else {
                ResponseTemplate::new(200)
                    .insert_header("content-type", "text/event-stream")
                    .set_body_string(second_response.clone())
            }
        })
        .expect(2) // Expect exactly 2 calls
        .mount(&mock_server)
        .await;

    // Run the CLI with --root pointing to temp dir
    cargo_bin_cmd!("zdx-cli")
        .env("ANTHROPIC_API_KEY", "test-api-key")
        .env("ANTHROPIC_BASE_URL", mock_server.uri())
        .args([
            "--root",
            temp_dir.path().to_str().unwrap(),
            "--no-save",
            "exec",
            "-p",
            "Read test.txt",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "The file contains: Hello from file!",
        ));

    // Verify two calls were made
    assert_eq!(call_count.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn test_tool_use_loop_second_request_has_tool_result() {
    let temp_dir = TempDir::new().unwrap();
    let test_file = temp_dir.path().join("data.txt");
    fs::write(&test_file, "secret data").unwrap();

    let mock_server = MockServer::start().await;
    let call_count = Arc::new(AtomicUsize::new(0));
    let call_count_clone = call_count.clone();
    let second_request_body = Arc::new(std::sync::Mutex::new(String::new()));
    let second_request_body_clone = second_request_body.clone();

    // First response: model requests to read a file (SSE)
    let first_response =
        tool_use_sse_response("toolu_abc123", "read", r#"{"path": "data.txt"}"#, None);

    // Second response: final answer (SSE)
    let second_response = text_sse_response("Done!");

    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(move |req: &Request| {
            let count = call_count_clone.fetch_add(1, Ordering::SeqCst);
            if count == 0 {
                ResponseTemplate::new(200)
                    .insert_header("content-type", "text/event-stream")
                    .set_body_string(first_response.clone())
            } else {
                // Capture the second request body for inspection
                let body = String::from_utf8_lossy(&req.body).to_string();
                *second_request_body_clone.lock().unwrap() = body;
                ResponseTemplate::new(200)
                    .insert_header("content-type", "text/event-stream")
                    .set_body_string(second_response.clone())
            }
        })
        .expect(2)
        .mount(&mock_server)
        .await;

    cargo_bin_cmd!("zdx-cli")
        .env("ANTHROPIC_API_KEY", "test-api-key")
        .env("ANTHROPIC_BASE_URL", mock_server.uri())
        .args([
            "--root",
            temp_dir.path().to_str().unwrap(),
            "--no-save",
            "exec",
            "-p",
            "Read data.txt",
        ])
        .assert()
        .success();

    // Check that the second request contains the tool_result
    let body = second_request_body.lock().unwrap().clone();
    assert!(
        body.contains("tool_result"),
        "Second request should contain tool_result block. Got: {}",
        body
    );
    assert!(
        body.contains("toolu_abc123"),
        "Second request should reference the tool_use_id. Got: {}",
        body
    );
    assert!(
        body.contains("secret data"),
        "Second request should contain the file content. Got: {}",
        body
    );
}

#[tokio::test]
async fn test_tool_read_outside_root_allowed() {
    let root_dir = TempDir::new().unwrap();
    let outside_dir = TempDir::new().unwrap();
    let outside_file = outside_dir.path().join("outside.txt");
    fs::write(&outside_file, "outside content").unwrap();
    let outside_path = outside_file.to_str().unwrap().to_string();

    let mock_server = MockServer::start().await;
    let call_count = Arc::new(AtomicUsize::new(0));
    let call_count_clone = call_count.clone();
    let second_request_body = Arc::new(std::sync::Mutex::new(String::new()));
    let second_request_body_clone = second_request_body.clone();

    // First response: model tries to read outside root (should be allowed) (SSE)
    let input_json = format!(r#"{{"path": "{}"}}"#, outside_path);
    let first_response = tool_use_sse_response("toolu_evil", "read", &input_json, None);

    // Second response: model acknowledges successful read (SSE)
    let second_response = text_sse_response("File read successfully.");

    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(move |req: &Request| {
            let count = call_count_clone.fetch_add(1, Ordering::SeqCst);
            if count == 0 {
                ResponseTemplate::new(200)
                    .insert_header("content-type", "text/event-stream")
                    .set_body_string(first_response.clone())
            } else {
                let body = String::from_utf8_lossy(&req.body).to_string();
                *second_request_body_clone.lock().unwrap() = body;
                ResponseTemplate::new(200)
                    .insert_header("content-type", "text/event-stream")
                    .set_body_string(second_response.clone())
            }
        })
        .expect(2)
        .mount(&mock_server)
        .await;

    cargo_bin_cmd!("zdx-cli")
        .env("ANTHROPIC_API_KEY", "test-api-key")
        .env("ANTHROPIC_BASE_URL", mock_server.uri())
        .args([
            "--root",
            root_dir.path().to_str().unwrap(),
            "--no-save",
            "exec",
            "-p",
            "Read outside file",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("File read successfully."));

    // Verify the tool_result contained the outside file content without an error flag
    let body = second_request_body.lock().unwrap().clone();
    assert!(
        body.contains("outside content"),
        "Tool result should include outside file content. Got: {}",
        body
    );
    assert!(
        !body.contains("\"is_error\":true"),
        "Tool result should not be marked as error. Got: {}",
        body
    );
}
