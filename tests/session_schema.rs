//! Integration tests for session schema versioning and tool event persistence.
//!
//! Tests the schema v1 format including:
//! - meta event as first line
//! - tool_use and tool_result events during tool loops
//! - backward compatibility with pre-v1 sessions
//! - resume with full tool history

mod fixtures;

use std::fs;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use assert_cmd::cargo::cargo_bin_cmd;
use fixtures::{sse_response, text_sse, tool_use_sse};
use predicates::prelude::*;
use tempfile::TempDir;
use wiremock::matchers::{body_string_contains, method, path};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

fn streaming_text_response(text: &str) -> ResponseTemplate {
    sse_response(&text_sse(text))
}

#[tokio::test]
async fn test_new_session_starts_with_meta_event() {
    let mock_server = MockServer::start().await;
    let temp_dir = TempDir::new().unwrap();

    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(streaming_text_response("Hello!"))
        .mount(&mock_server)
        .await;

    cargo_bin_cmd!("zdx-cli")
        .env("ZDX_HOME", temp_dir.path())
        .env("ANTHROPIC_API_KEY", "test-api-key")
        .env("ANTHROPIC_BASE_URL", mock_server.uri())
        .args(["exec", "-p", "hello"])
        .assert()
        .success();

    // Find the session file
    let sessions_dir = temp_dir.path().join("sessions");
    let entries: Vec<_> = fs::read_dir(&sessions_dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().is_some_and(|ext| ext == "jsonl"))
        .collect();

    assert_eq!(entries.len(), 1);

    let content = fs::read_to_string(entries[0].path()).unwrap();
    let lines: Vec<&str> = content.lines().collect();

    // First line should be meta event with schema_version: 1
    let meta: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
    assert_eq!(meta["type"], "meta");
    assert_eq!(meta["schema_version"], 1);
    assert!(meta["ts"].is_string(), "meta should have timestamp");
}

#[tokio::test]
async fn test_tool_use_persisted_to_session() {
    let mock_server = MockServer::start().await;
    let temp_dir = TempDir::new().unwrap();

    // Create a file to read
    let test_file = temp_dir.path().join("test.txt");
    fs::write(&test_file, "file content").unwrap();

    let call_count = Arc::new(AtomicUsize::new(0));
    let call_count_clone = call_count.clone();

    let first_response = tool_use_sse("tool-123", "read", r#"{"path": "test.txt"}"#);
    let second_response = text_sse("The file contains: file content");

    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(move |_req: &Request| {
            let count = call_count_clone.fetch_add(1, Ordering::SeqCst);
            if count == 0 {
                sse_response(&first_response)
            } else {
                sse_response(&second_response)
            }
        })
        .expect(2)
        .mount(&mock_server)
        .await;

    cargo_bin_cmd!("zdx-cli")
        .env("ZDX_HOME", temp_dir.path())
        .env("ANTHROPIC_API_KEY", "test-api-key")
        .env("ANTHROPIC_BASE_URL", mock_server.uri())
        .args(["--root", temp_dir.path().to_str().unwrap()])
        .args(["exec", "-p", "read test.txt"])
        .assert()
        .success()
        .stdout(predicate::str::contains("file contains"));

    // Find the session file
    let sessions_dir = temp_dir.path().join("sessions");
    let entries: Vec<_> = fs::read_dir(&sessions_dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().is_some_and(|ext| ext == "jsonl"))
        .collect();

    assert_eq!(entries.len(), 1);

    let content = fs::read_to_string(entries[0].path()).unwrap();
    let events: Vec<serde_json::Value> = content
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();

    // Should have: meta, user, tool_use, tool_result, assistant
    assert!(events.len() >= 5, "session should have at least 5 events");

    // Find tool_use event
    let tool_use = events
        .iter()
        .find(|e| e["type"] == "tool_use")
        .expect("session should contain tool_use event");
    assert_eq!(tool_use["name"], "read");
    assert_eq!(tool_use["id"], "tool-123");
    assert!(tool_use["input"]["path"].is_string());

    // Find tool_result event
    let tool_result = events
        .iter()
        .find(|e| e["type"] == "tool_result")
        .expect("session should contain tool_result event");
    assert_eq!(tool_result["tool_use_id"], "tool-123");
    assert!(tool_result["ok"].is_boolean());
}

/// Test that legacy sessions (without meta event) are backward compatible.
///
/// After refactor (commit 2b):
/// - "Loaded N previous messages" goes to stderr
/// - Assistant text goes to stdout
#[tokio::test]
async fn test_legacy_session_backward_compatible() {
    let mock_server = MockServer::start().await;
    let temp_dir = TempDir::new().unwrap();

    // Create a legacy format session file (without meta event)
    let sessions_dir = temp_dir.path().join("sessions");
    fs::create_dir_all(&sessions_dir).unwrap();
    let session_path = sessions_dir.join("legacy-session.jsonl");

    let legacy_content = r#"{"type":"message","role":"user","text":"What is 2+2?","ts":"2025-01-01T00:00:00Z"}
{"type":"message","role":"assistant","text":"The answer is 4.","ts":"2025-01-01T00:00:01Z"}"#;
    fs::write(&session_path, legacy_content).unwrap();

    // Resume the legacy session
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .and(body_string_contains("What is 2+2?"))
        .and(body_string_contains("The answer is 4."))
        .respond_with(streaming_text_response("3+3 is 6"))
        .expect(1)
        .mount(&mock_server)
        .await;

    cargo_bin_cmd!("zdx-cli")
        .env("ZDX_HOME", temp_dir.path())
        .env("ANTHROPIC_API_KEY", "test-api-key")
        .env("ANTHROPIC_BASE_URL", mock_server.uri())
        .args(["resume", "legacy-session"])
        .write_stdin("And 3+3?\n:q\n")
        .assert()
        .success()
        // REPL UI goes to stderr
        .stderr(predicate::str::contains("Loaded 2 previous messages"))
        // Assistant text goes to stdout
        .stdout(predicate::str::contains("3+3 is 6"));
}

#[tokio::test]
async fn test_sessions_show_displays_tool_events() {
    let temp_dir = TempDir::new().unwrap();

    // Create a session with tool events
    let sessions_dir = temp_dir.path().join("sessions");
    fs::create_dir_all(&sessions_dir).unwrap();
    let session_path = sessions_dir.join("tool-session.jsonl");

    let session_content = r#"{"type":"meta","schema_version":1,"ts":"2025-01-01T00:00:00Z"}
{"type":"message","role":"user","text":"read main.rs","ts":"2025-01-01T00:00:01Z"}
{"type":"tool_use","id":"t1","name":"read","input":{"path":"main.rs"},"ts":"2025-01-01T00:00:02Z"}
{"type":"tool_result","tool_use_id":"t1","output":{"ok":true,"data":{"content":"fn main() {}"}},"ok":true,"ts":"2025-01-01T00:00:03Z"}
{"type":"message","role":"assistant","text":"Here is the file.","ts":"2025-01-01T00:00:04Z"}"#;
    fs::write(&session_path, session_content).unwrap();

    cargo_bin_cmd!("zdx-cli")
        .env("ZDX_HOME", temp_dir.path())
        .args(["sessions", "show", "tool-session"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Session (schema v1)"))
        .stdout(predicate::str::contains("### Tool: read"))
        .stdout(predicate::str::contains("### Result ✓"));
}

/// Test that resume with tool history includes tool context in API request.
///
/// After refactor (commit 2b):
/// - "Loaded N previous messages" goes to stderr
/// - Assistant text goes to stdout
#[tokio::test]
async fn test_resume_with_tool_history_includes_tools_in_context() {
    let mock_server = MockServer::start().await;
    let temp_dir = TempDir::new().unwrap();

    // Create a session with tool use history
    let sessions_dir = temp_dir.path().join("sessions");
    fs::create_dir_all(&sessions_dir).unwrap();
    let session_path = sessions_dir.join("resume-tools.jsonl");

    let session_content = r#"{"type":"meta","schema_version":1,"ts":"2025-01-01T00:00:00Z"}
{"type":"message","role":"user","text":"read file.txt","ts":"2025-01-01T00:00:01Z"}
{"type":"tool_use","id":"t1","name":"read","input":{"path":"file.txt"},"ts":"2025-01-01T00:00:02Z"}
{"type":"tool_result","tool_use_id":"t1","output":{"ok":true,"data":{"content":"hello world"}},"ok":true,"ts":"2025-01-01T00:00:03Z"}
{"type":"message","role":"assistant","text":"The file contains hello world.","ts":"2025-01-01T00:00:04Z"}"#;
    fs::write(&session_path, session_content).unwrap();

    // Resume should include the tool context
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        // Should have tool_use in the conversation history
        .and(body_string_contains("tool_use"))
        .and(body_string_contains("read"))
        // Should have tool_result in the conversation history
        .and(body_string_contains("tool_result"))
        .respond_with(streaming_text_response(
            "Okay, continuing from where we left off.",
        ))
        .expect(1)
        .mount(&mock_server)
        .await;

    cargo_bin_cmd!("zdx-cli")
        .env("ZDX_HOME", temp_dir.path())
        .env("ANTHROPIC_API_KEY", "test-api-key")
        .env("ANTHROPIC_BASE_URL", mock_server.uri())
        .args(["resume", "resume-tools"])
        .write_stdin("continue\n:q\n")
        .assert()
        .success()
        // REPL UI goes to stderr
        .stderr(predicate::str::contains("Loaded 4 previous messages"))
        // Assistant text goes to stdout
        .stdout(predicate::str::contains(
            "Okay, continuing from where we left off.",
        ));
}

#[tokio::test]
async fn test_interrupted_session_mid_tool_is_resumable() {
    let temp_dir = TempDir::new().unwrap();

    // Create a session that was interrupted during tool execution
    let sessions_dir = temp_dir.path().join("sessions");
    fs::create_dir_all(&sessions_dir).unwrap();
    let session_path = sessions_dir.join("interrupted.jsonl");

    let session_content = r#"{"type":"meta","schema_version":1,"ts":"2025-01-01T00:00:00Z"}
{"type":"message","role":"user","text":"run ls","ts":"2025-01-01T00:00:01Z"}
{"type":"tool_use","id":"t1","name":"bash","input":{"command":"ls"},"ts":"2025-01-01T00:00:02Z"}
{"type":"interrupted","ts":"2025-01-01T00:00:03Z"}"#;
    fs::write(&session_path, session_content).unwrap();

    // Session should be loadable and resumable
    cargo_bin_cmd!("zdx-cli")
        .env("ZDX_HOME", temp_dir.path())
        .args(["sessions", "show", "interrupted"])
        .assert()
        .success()
        .stdout(predicate::str::contains("### Interrupted"));
}
