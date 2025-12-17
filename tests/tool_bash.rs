//! Integration tests for the bash tool.
//!
//! Verifies that the bash tool executes commands and captures output correctly.

mod fixtures;

use assert_cmd::cargo::cargo_bin_cmd;
use fixtures::{sse_response, tool_use_sse};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use tempfile::TempDir;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request};

#[tokio::test]
async fn test_bash_executes_command() {
    let temp_dir = TempDir::new().unwrap();
    let mock_server = MockServer::start().await;

    let call_count = Arc::new(AtomicUsize::new(0));
    let call_count_clone = call_count.clone();
    let second_request_body = Arc::new(std::sync::Mutex::new(String::new()));
    let second_request_body_clone = second_request_body.clone();

    let first_response = tool_use_sse(
        "toolu_bash_001",
        "bash",
        r#"{"command": "echo hello_from_bash"}"#,
    );
    let second_response = fixtures::text_sse("Bash executed successfully.");

    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(move |req: &Request| {
            let count = call_count_clone.fetch_add(1, Ordering::SeqCst);
            if count == 0 {
                sse_response(&first_response)
            } else {
                let body = String::from_utf8_lossy(&req.body).to_string();
                *second_request_body_clone.lock().unwrap() = body;
                sse_response(&second_response)
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

    let first_response = tool_use_sse("toolu_bash_002", "bash", r#"{"command": "ls"}"#);
    let second_response = fixtures::text_sse("Listed files.");

    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(move |req: &Request| {
            let count = call_count_clone.fetch_add(1, Ordering::SeqCst);
            if count == 0 {
                sse_response(&first_response)
            } else {
                let body = String::from_utf8_lossy(&req.body).to_string();
                *second_request_body_clone.lock().unwrap() = body;
                sse_response(&second_response)
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

    let body = second_request_body.lock().unwrap().clone();
    assert!(
        body.contains("marker.txt"),
        "ls should show marker.txt from root dir. Got: {}",
        body
    );
}

#[tokio::test]
async fn test_bash_times_out_when_configured() {
    let temp_dir = TempDir::new().unwrap();
    let zdx_home = TempDir::new().unwrap();
    std::fs::write(
        zdx_home.path().join("config.toml"),
        "tool_timeout_secs = 1\n",
    )
    .unwrap();

    let mock_server = MockServer::start().await;

    let call_count = Arc::new(AtomicUsize::new(0));
    let call_count_clone = call_count.clone();
    let second_request_body = Arc::new(std::sync::Mutex::new(String::new()));
    let second_request_body_clone = second_request_body.clone();

    let first_response = tool_use_sse("toolu_bash_timeout", "bash", r#"{"command": "sleep 2"}"#);
    let second_response = fixtures::text_sse("Done.");

    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(move |req: &Request| {
            let count = call_count_clone.fetch_add(1, Ordering::SeqCst);
            if count == 0 {
                sse_response(&first_response)
            } else {
                let body = String::from_utf8_lossy(&req.body).to_string();
                *second_request_body_clone.lock().unwrap() = body;
                sse_response(&second_response)
            }
        })
        .expect(2)
        .mount(&mock_server)
        .await;

    cargo_bin_cmd!("zdx-cli")
        .env("ZDX_HOME", zdx_home.path())
        .env("ANTHROPIC_API_KEY", "test-api-key")
        .env("ANTHROPIC_BASE_URL", mock_server.uri())
        .args([
            "--root",
            temp_dir.path().to_str().unwrap(),
            "--no-save",
            "exec",
            "-p",
            "Run a slow command",
        ])
        .assert()
        .success();

    let body = second_request_body.lock().unwrap().clone();
    assert!(
        body.contains("Tool execution timed out"),
        "Tool result should indicate timeout. Got: {}",
        body
    );
}
