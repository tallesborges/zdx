//! Integration tests for session persistence.

mod fixtures;

use assert_cmd::cargo::cargo_bin_cmd;
use fixtures::text_response;
use predicates::prelude::*;
use std::fs;
use tempfile::TempDir;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer};

#[tokio::test]
async fn test_exec_creates_session_file() {
    let mock_server = MockServer::start().await;
    let temp_dir = TempDir::new().unwrap();

    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(text_response("Hello from assistant!"))
        .mount(&mock_server)
        .await;

    cargo_bin_cmd!("zdx-cli")
        .env("ZDX_HOME", temp_dir.path())
        .env("ANTHROPIC_API_KEY", "test-api-key")
        .env("ANTHROPIC_BASE_URL", mock_server.uri())
        .args(["exec", "-p", "hello"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Hello from assistant!"));

    // Assert sessions directory exists
    let sessions_dir = temp_dir.path().join("sessions");
    assert!(sessions_dir.exists(), "sessions directory should exist");

    // Assert exactly one JSONL file exists
    let entries: Vec<_> = fs::read_dir(&sessions_dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().is_some_and(|ext| ext == "jsonl"))
        .collect();

    assert_eq!(entries.len(), 1, "should have exactly one session file");

    // Read and verify the session file contains both roles
    let session_path = entries[0].path();
    let content = fs::read_to_string(&session_path).unwrap();
    let lines: Vec<&str> = content.lines().collect();

    assert_eq!(
        lines.len(),
        2,
        "session should have 2 lines (user + assistant)"
    );

    // Parse and verify user message
    let user_event: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
    assert_eq!(user_event["type"], "message");
    assert_eq!(user_event["role"], "user");
    assert_eq!(user_event["text"], "hello");

    // Parse and verify assistant message
    let assistant_event: serde_json::Value = serde_json::from_str(lines[1]).unwrap();
    assert_eq!(assistant_event["type"], "message");
    assert_eq!(assistant_event["role"], "assistant");
    assert_eq!(assistant_event["text"], "Hello from assistant!");
}

#[tokio::test]
async fn test_exec_no_save_skips_session() {
    let mock_server = MockServer::start().await;
    let temp_dir = TempDir::new().unwrap();

    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(text_response("Hello!"))
        .mount(&mock_server)
        .await;

    cargo_bin_cmd!("zdx-cli")
        .env("ZDX_HOME", temp_dir.path())
        .env("ANTHROPIC_API_KEY", "test-api-key")
        .env("ANTHROPIC_BASE_URL", mock_server.uri())
        .args(["--no-save", "exec", "-p", "hello"])
        .assert()
        .success();

    // Sessions directory should not exist when --no-save is used
    let sessions_dir = temp_dir.path().join("sessions");
    assert!(
        !sessions_dir.exists(),
        "sessions directory should not exist with --no-save"
    );
}

#[tokio::test]
async fn test_exec_appends_to_existing_session() {
    let mock_server = MockServer::start().await;
    let temp_dir = TempDir::new().unwrap();

    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(text_response("Response!"))
        .expect(2)
        .mount(&mock_server)
        .await;

    // First exec creates a session
    cargo_bin_cmd!("zdx-cli")
        .env("ZDX_HOME", temp_dir.path())
        .env("ANTHROPIC_API_KEY", "test-api-key")
        .env("ANTHROPIC_BASE_URL", mock_server.uri())
        .args(["--session", "test-session", "exec", "-p", "first message"])
        .assert()
        .success();

    // Second exec appends to the same session
    cargo_bin_cmd!("zdx-cli")
        .env("ZDX_HOME", temp_dir.path())
        .env("ANTHROPIC_API_KEY", "test-api-key")
        .env("ANTHROPIC_BASE_URL", mock_server.uri())
        .args(["--session", "test-session", "exec", "-p", "second message"])
        .assert()
        .success();

    // Verify the session file has 4 lines (2 user + 2 assistant)
    let session_path = temp_dir.path().join("sessions").join("test-session.jsonl");
    let content = fs::read_to_string(&session_path).unwrap();
    let lines: Vec<&str> = content.lines().collect();

    assert_eq!(
        lines.len(),
        4,
        "session should have 4 lines after two execs"
    );

    // Verify order: user, assistant, user, assistant
    let events: Vec<serde_json::Value> = lines
        .iter()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();

    assert_eq!(events[0]["role"], "user");
    assert_eq!(events[0]["text"], "first message");
    assert_eq!(events[1]["role"], "assistant");
    assert_eq!(events[2]["role"], "user");
    assert_eq!(events[2]["text"], "second message");
    assert_eq!(events[3]["role"], "assistant");
}
