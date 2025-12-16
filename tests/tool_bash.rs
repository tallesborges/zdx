//! Integration tests for the bash tool.
//!
//! Verifies that the bash tool executes commands and captures output correctly.

use assert_cmd::cargo::cargo_bin_cmd;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use tempfile::TempDir;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

/// Creates an SSE streaming response for a tool_use request
fn tool_use_sse_response(tool_id: &str, tool_name: &str, input_json: &str) -> String {
    format!(
        r#"event: message_start
data: {{"type":"message_start","message":{{"id":"msg_001","type":"message","role":"assistant","content":[],"model":"claude-sonnet-4-20250514","stop_reason":null,"stop_sequence":null,"usage":{{"input_tokens":10,"output_tokens":1}}}}}}

event: content_block_start
data: {{"type":"content_block_start","index":0,"content_block":{{"type":"tool_use","id":"{}","name":"{}"}}}}

event: content_block_delta
data: {{"type":"content_block_delta","index":0,"delta":{{"type":"input_json_delta","partial_json":"{}"}}}}

event: content_block_stop
data: {{"type":"content_block_stop","index":0}}

event: message_delta
data: {{"type":"message_delta","delta":{{"stop_reason":"tool_use","stop_sequence":null}},"usage":{{"output_tokens":20}}}}

event: message_stop
data: {{"type":"message_stop"}}

"#,
        tool_id,
        tool_name,
        input_json.replace('"', "\\\"").replace('\n', "\\n")
    )
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
data: {{"type":"message_delta","delta":{{"stop_reason":"end_turn","stop_sequence":null}},"usage":{{"output_tokens":5}}}}

event: message_stop
data: {{"type":"message_stop"}}

"#,
        text.replace('"', "\\\"").replace('\n', "\\n")
    )
}

#[tokio::test]
async fn test_bash_executes_command() {
    let temp_dir = TempDir::new().unwrap();
    let mock_server = MockServer::start().await;

    let call_count = Arc::new(AtomicUsize::new(0));
    let call_count_clone = call_count.clone();
    let second_request_body = Arc::new(std::sync::Mutex::new(String::new()));
    let second_request_body_clone = second_request_body.clone();

    // First response: model requests to run bash command (SSE)
    let first_response = tool_use_sse_response(
        "toolu_bash_001",
        "bash",
        r#"{"command": "echo hello_from_bash"}"#,
    );

    // Second response: final answer (SSE)
    let second_response = text_sse_response("Bash executed successfully.");

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
            temp_dir.path().to_str().unwrap(),
            "--no-save",
            "exec",
            "-p",
            "Run echo hello",
        ])
        .assert()
        .success();

    // Check that the tool result contains the output
    let body = second_request_body.lock().unwrap().clone();
    assert!(
        body.contains("hello_from_bash"),
        "Tool result should contain command output. Got: {}",
        body
    );
    assert!(
        body.contains("exit_code: 0"),
        "Tool result should contain exit_code. Got: {}",
        body
    );
}

#[tokio::test]
async fn test_bash_runs_in_root_directory() {
    let temp_dir = TempDir::new().unwrap();
    std::fs::write(temp_dir.path().join("marker.txt"), "marker content").unwrap();

    let mock_server = MockServer::start().await;

    let call_count = Arc::new(AtomicUsize::new(0));
    let call_count_clone = call_count.clone();
    let second_request_body = Arc::new(std::sync::Mutex::new(String::new()));
    let second_request_body_clone = second_request_body.clone();

    // First response: model lists files (SSE)
    let first_response = tool_use_sse_response("toolu_bash_002", "bash", r#"{"command": "ls"}"#);

    // Second response (SSE)
    let second_response = text_sse_response("Listed files.");

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
            temp_dir.path().to_str().unwrap(),
            "--no-save",
            "exec",
            "-p",
            "List files",
        ])
        .assert()
        .success();

    // Check that the command ran in the correct directory
    let body = second_request_body.lock().unwrap().clone();
    assert!(
        body.contains("marker.txt"),
        "ls should show marker.txt from root dir. Got: {}",
        body
    );
}
