//! Integration tests for zdx resume command.
//!
//! Verifies that resume loads previous session history and includes it
//! in API requests.
//!
//! After refactor (commit 2b):
//! - Assistant text goes to stdout only
//! - REPL UI (session info, loaded messages, goodbye) goes to stderr only

mod fixtures;

use std::fs;

use assert_cmd::cargo::cargo_bin_cmd;
use fixtures::{sse_response, text_sse};
use predicates::prelude::*;
use tempfile::TempDir;
use wiremock::matchers::{body_string_contains, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn streaming_text_response(text: &str) -> ResponseTemplate {
    sse_response(&text_sse(text))
}

fn create_session_file(temp_dir: &TempDir, session_id: &str, events: &[(&str, &str)]) {
    let sessions_dir = temp_dir.path().join("sessions");
    fs::create_dir_all(&sessions_dir).unwrap();

    let session_path = sessions_dir.join(format!("{}.jsonl", session_id));
    let content: String = events
        .iter()
        .map(|(role, text)| {
            serde_json::json!({
                "type": "message",
                "role": role,
                "text": text,
                "ts": "0:000Z"
            })
            .to_string()
        })
        .collect::<Vec<_>>()
        .join("\n");

    fs::write(&session_path, content + "\n").unwrap();
}

/// Test that resume loads history and sends it to the API.
///
/// After refactor (commit 2b):
/// - "Loaded N previous messages" goes to stderr
/// - Assistant response goes to stdout
#[tokio::test]
async fn test_resume_loads_history_into_api_request() {
    let mock_server = MockServer::start().await;
    let temp_dir = TempDir::new().unwrap();

    // Create a session with known history
    create_session_file(
        &temp_dir,
        "test-session-123",
        &[("user", "What is 2+2?"), ("assistant", "The answer is 4.")],
    );

    // The mock should expect the previous messages in the request body
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .and(body_string_contains("What is 2+2?"))
        .and(body_string_contains("The answer is 4."))
        .and(body_string_contains("And what is 3+3?"))
        .respond_with(streaming_text_response("The answer is 6."))
        .expect(1)
        .mount(&mock_server)
        .await;

    cargo_bin_cmd!("zdx-cli")
        .env("ZDX_HOME", temp_dir.path())
        .env("ANTHROPIC_API_KEY", "test-api-key")
        .env("ANTHROPIC_BASE_URL", mock_server.uri())
        .args(["resume", "test-session-123"])
        .write_stdin("And what is 3+3?\n:q\n")
        .assert()
        .success()
        // REPL UI goes to stderr
        .stderr(predicate::str::contains("Loaded 2 previous messages"))
        // Assistant text goes to stdout
        .stdout(predicate::str::contains("The answer is 6."));
}

/// Test that resume without explicit ID uses the most recently modified session.
///
/// After refactor (commit 2b):
/// - "Loaded N previous messages" and session ID go to stderr
/// - Assistant text goes to stdout
#[tokio::test]
async fn test_resume_without_id_uses_latest_session() {
    let mock_server = MockServer::start().await;
    let temp_dir = TempDir::new().unwrap();

    // Create two sessions with different timestamps
    create_session_file(
        &temp_dir,
        "old-session",
        &[("user", "old message"), ("assistant", "old response")],
    );

    // Sleep to ensure different modification times
    std::thread::sleep(std::time::Duration::from_millis(50));

    create_session_file(
        &temp_dir,
        "new-session",
        &[("user", "new message"), ("assistant", "new response")],
    );

    // Mock should see the "new message" from the latest session
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .and(body_string_contains("new message"))
        .and(body_string_contains("new response"))
        .respond_with(streaming_text_response("Continuing..."))
        .expect(1)
        .mount(&mock_server)
        .await;

    cargo_bin_cmd!("zdx-cli")
        .env("ZDX_HOME", temp_dir.path())
        .env("ANTHROPIC_API_KEY", "test-api-key")
        .env("ANTHROPIC_BASE_URL", mock_server.uri())
        .args(["resume"])
        .write_stdin("hello\n:q\n")
        .assert()
        .success()
        // REPL UI goes to stderr
        .stderr(predicate::str::contains("Loaded 2 previous messages"))
        .stderr(predicate::str::contains("new-session"))
        // Assistant text goes to stdout
        .stdout(predicate::str::contains("Continuing..."));
}

#[tokio::test]
async fn test_resume_appends_to_session_file() {
    let mock_server = MockServer::start().await;
    let temp_dir = TempDir::new().unwrap();

    create_session_file(
        &temp_dir,
        "append-session",
        &[("user", "first"), ("assistant", "response")],
    );

    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(streaming_text_response("Second response!"))
        .mount(&mock_server)
        .await;

    cargo_bin_cmd!("zdx-cli")
        .env("ZDX_HOME", temp_dir.path())
        .env("ANTHROPIC_API_KEY", "test-api-key")
        .env("ANTHROPIC_BASE_URL", mock_server.uri())
        .args(["resume", "append-session"])
        .write_stdin("second message\n:q\n")
        .assert()
        .success();

    // Verify the session file now has 4 lines
    let session_path = temp_dir
        .path()
        .join("sessions")
        .join("append-session.jsonl");
    let content = fs::read_to_string(&session_path).unwrap();
    let lines: Vec<&str> = content.lines().filter(|l| !l.is_empty()).collect();

    assert_eq!(lines.len(), 4, "session should have 4 lines after resume");

    // Verify the new messages were appended
    let events: Vec<serde_json::Value> = lines
        .iter()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();

    assert_eq!(events[2]["role"], "user");
    assert_eq!(events[2]["text"], "second message");
    assert_eq!(events[3]["role"], "assistant");
    assert_eq!(events[3]["text"], "Second response!");
}

#[tokio::test]
async fn test_resume_no_sessions_exits_with_error() {
    let temp_dir = TempDir::new().unwrap();

    cargo_bin_cmd!("zdx-cli")
        .env("ZDX_HOME", temp_dir.path())
        .env("ANTHROPIC_API_KEY", "test-api-key")
        .args(["resume"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("No sessions found to resume"));
}

/// Test that resume with nonexistent ID creates a new session.
///
/// Note: Resuming a nonexistent session creates a new (empty) session
/// with that ID. This is a valid use case.
///
/// After refactor (commit 2b):
/// - Session info and goodbye go to stderr
/// - stdout is empty (no assistant response when quitting immediately)
#[tokio::test]
async fn test_resume_nonexistent_session_creates_empty_session() {
    let temp_dir = TempDir::new().unwrap();
    let sessions_dir = temp_dir.path().join("sessions");
    fs::create_dir_all(&sessions_dir).unwrap();

    cargo_bin_cmd!("zdx-cli")
        .env("ZDX_HOME", temp_dir.path())
        .env("ANTHROPIC_API_KEY", "test-api-key")
        .env("ANTHROPIC_BASE_URL", "http://localhost:9999") // won't be called
        .args(["resume", "nonexistent-id"])
        .write_stdin(":q\n")
        .assert()
        .success()
        // REPL UI goes to stderr
        .stderr(predicate::str::contains("Session: nonexistent-id"))
        .stderr(predicate::str::contains("Goodbye!"))
        // stdout is empty (no assistant response)
        .stdout(predicate::str::is_empty());
}
