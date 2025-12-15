//! Integration tests for the bash tool.
//!
//! Verifies that the bash tool executes commands and captures output correctly.

use assert_cmd::cargo::cargo_bin_cmd;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use tempfile::TempDir;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

#[tokio::test]
async fn test_bash_executes_command() {
    let temp_dir = TempDir::new().unwrap();
    let mock_server = MockServer::start().await;

    let call_count = Arc::new(AtomicUsize::new(0));
    let call_count_clone = call_count.clone();
    let second_request_body = Arc::new(std::sync::Mutex::new(String::new()));
    let second_request_body_clone = second_request_body.clone();

    // First response: model requests to run bash command
    let first_response = serde_json::json!({
        "id": "msg_001",
        "type": "message",
        "role": "assistant",
        "content": [
            {
                "type": "tool_use",
                "id": "toolu_bash_001",
                "name": "bash",
                "input": {"command": "echo hello_from_bash"}
            }
        ],
        "model": "claude-haiku-4-5",
        "stop_reason": "tool_use",
        "usage": {"input_tokens": 10, "output_tokens": 20}
    });

    // Second response: final answer
    let second_response = serde_json::json!({
        "id": "msg_002",
        "type": "message",
        "role": "assistant",
        "content": [{"type": "text", "text": "Bash executed successfully."}],
        "model": "claude-haiku-4-5",
        "stop_reason": "end_turn",
        "usage": {"input_tokens": 30, "output_tokens": 5}
    });

    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(move |req: &Request| {
            let count = call_count_clone.fetch_add(1, Ordering::SeqCst);
            if count == 0 {
                ResponseTemplate::new(200).set_body_json(first_response.clone())
            } else {
                let body = String::from_utf8_lossy(&req.body).to_string();
                *second_request_body_clone.lock().unwrap() = body;
                ResponseTemplate::new(200).set_body_json(second_response.clone())
            }
        })
        .expect(2)
        .mount(&mock_server)
        .await;

    cargo_bin_cmd!("zdx-cli")
        .env("ANTHROPIC_API_KEY", "test-api-key")
        .env("ANTHROPIC_BASE_URL", mock_server.uri())
        .args([
            "exec",
            "-p",
            "Run echo hello",
            "--root",
            temp_dir.path().to_str().unwrap(),
            "--no-save",
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

    // First response: model lists files
    let first_response = serde_json::json!({
        "id": "msg_001",
        "type": "message",
        "role": "assistant",
        "content": [
            {
                "type": "tool_use",
                "id": "toolu_bash_002",
                "name": "bash",
                "input": {"command": "ls"}
            }
        ],
        "model": "claude-haiku-4-5",
        "stop_reason": "tool_use",
        "usage": {"input_tokens": 10, "output_tokens": 20}
    });

    // Second response
    let second_response = serde_json::json!({
        "id": "msg_002",
        "type": "message",
        "role": "assistant",
        "content": [{"type": "text", "text": "Listed files."}],
        "model": "claude-haiku-4-5",
        "stop_reason": "end_turn",
        "usage": {"input_tokens": 30, "output_tokens": 5}
    });

    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(move |req: &Request| {
            let count = call_count_clone.fetch_add(1, Ordering::SeqCst);
            if count == 0 {
                ResponseTemplate::new(200).set_body_json(first_response.clone())
            } else {
                let body = String::from_utf8_lossy(&req.body).to_string();
                *second_request_body_clone.lock().unwrap() = body;
                ResponseTemplate::new(200).set_body_json(second_response.clone())
            }
        })
        .expect(2)
        .mount(&mock_server)
        .await;

    cargo_bin_cmd!("zdx-cli")
        .env("ANTHROPIC_API_KEY", "test-api-key")
        .env("ANTHROPIC_BASE_URL", mock_server.uri())
        .args([
            "exec",
            "-p",
            "List files",
            "--root",
            temp_dir.path().to_str().unwrap(),
            "--no-save",
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
