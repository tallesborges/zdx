mod fixtures;

use std::fs;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use assert_cmd::cargo::cargo_bin_cmd;
use fixtures::{multi_chunk_text_sse, sse_response, text_and_tool_use_sse, text_sse};
use predicates::prelude::*;
use tempfile::TempDir;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

/// Wrap SSE body in a streaming response template.
fn streaming_text_response(text: &str) -> ResponseTemplate {
    sse_response(&text_sse(text))
}

/// Test that chat responds to user input and shows goodbye message.
///
/// After refactor (commit 2b):
/// - Assistant text "Hello there!" goes to stdout only
/// - REPL UI "Goodbye!" goes to stderr only
#[tokio::test]
async fn test_chat_responds_and_exits_on_quit() {
    let mock_server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .and(header("x-api-key", "test-api-key"))
        .and(header("anthropic-version", "2023-06-01"))
        .respond_with(streaming_text_response("Hello there!"))
        .expect(1)
        .mount(&mock_server)
        .await;

    cargo_bin_cmd!("zdx-cli")
        .env("ANTHROPIC_API_KEY", "test-api-key")
        .env("ANTHROPIC_BASE_URL", mock_server.uri())
        .args(["--no-save"])
        .write_stdin("hi\n:q\n")
        .assert()
        .success()
        // Assistant text goes to stdout
        .stdout(predicate::str::contains("Hello there!"))
        // REPL UI goes to stderr
        .stderr(predicate::str::contains("Goodbye!"));
}

#[tokio::test]
async fn test_chat_maintains_history() {
    let mock_server = MockServer::start().await;

    // First response
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(streaming_text_response("I'm Claude!"))
        .expect(1)
        .named("first_message")
        .mount(&mock_server)
        .await;

    cargo_bin_cmd!("zdx-cli")
        .env("ANTHROPIC_API_KEY", "test-api-key")
        .env("ANTHROPIC_BASE_URL", mock_server.uri())
        .args(["--no-save"])
        .write_stdin("hello\n:q\n")
        .assert()
        .success()
        .stdout(predicate::str::contains("I'm Claude!"));
}

#[tokio::test]
async fn test_chat_handles_empty_input() {
    let mock_server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(streaming_text_response("Got it!"))
        .expect(1)
        .mount(&mock_server)
        .await;

    // Empty lines should be skipped, only "test" should trigger API call
    cargo_bin_cmd!("zdx-cli")
        .env("ANTHROPIC_API_KEY", "test-api-key")
        .env("ANTHROPIC_BASE_URL", mock_server.uri())
        .args(["--no-save"])
        .write_stdin("\n\ntest\n:q\n")
        .assert()
        .success()
        .stdout(predicate::str::contains("Got it!"));
}

/// Test that welcome message and quit hint go to stderr.
///
/// After refactor (commit 2b):
/// - "ZDX Chat" and ":q to quit" go to stderr
/// - stdout is empty when no assistant response occurs
#[tokio::test]
async fn test_chat_shows_welcome_message() {
    let mock_server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(streaming_text_response("Hi!"))
        .mount(&mock_server)
        .await;

    cargo_bin_cmd!("zdx-cli")
        .env("ANTHROPIC_API_KEY", "test-api-key")
        .env("ANTHROPIC_BASE_URL", mock_server.uri())
        .args(["--no-save"])
        .write_stdin(":q\n")
        .assert()
        .success()
        // REPL UI goes to stderr
        .stderr(predicate::str::contains("ZDX Chat"))
        .stderr(predicate::str::contains(":q to quit"))
        // stdout is empty (no assistant response)
        .stdout(predicate::str::is_empty());
}

/// Test that API errors go to stderr, not stdout.
///
/// After refactor (commit 2b):
/// - Error messages go to stderr
/// - stdout remains empty (no assistant response)
#[tokio::test]
async fn test_chat_handles_api_error_gracefully() {
    let mock_server = MockServer::start().await;

    let error_body = serde_json::json!({
        "type": "error",
        "error": {
            "type": "rate_limit_error",
            "message": "Rate limit exceeded"
        }
    });

    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(429).set_body_json(error_body))
        .mount(&mock_server)
        .await;

    // Chat should show error but continue (user can still quit)
    cargo_bin_cmd!("zdx-cli")
        .env("ANTHROPIC_API_KEY", "test-api-key")
        .env("ANTHROPIC_BASE_URL", mock_server.uri())
        .args(["--no-save"])
        .write_stdin("hello\n:q\n")
        .assert()
        .success()
        // Errors go to stderr
        .stderr(predicate::str::contains("Error:"))
        .stderr(predicate::str::contains("429"))
        // stdout is empty (no assistant response)
        .stdout(predicate::str::is_empty());
}

/// Test that missing API key error is reported on stderr.
///
/// Note: In interactive chat mode, errors are shown but the user can continue
/// (e.g., to quit gracefully). This is more user-friendly than crashing.
/// The error is shown when attempting to make a request.
#[tokio::test]
async fn test_chat_handles_missing_api_key_gracefully() {
    cargo_bin_cmd!("zdx-cli")
        .env_remove("ANTHROPIC_API_KEY")
        .args(["--no-save"])
        .write_stdin("hello\n:q\n")
        .assert()
        .success()
        // Error is shown on stderr
        .stderr(predicate::str::contains("ANTHROPIC_API_KEY"))
        // User can still quit
        .stderr(predicate::str::contains("Goodbye!"));
}

/// Test that tool use loop works correctly with files.
///
/// After refactor (commit 2b):
/// - Tool status indicators go to stderr (via renderer)
/// - Final assistant text goes to stdout
#[tokio::test]
async fn test_chat_tool_use_loop_reads_file() {
    let temp_dir = TempDir::new().unwrap();
    let test_file = temp_dir.path().join("hello.txt");
    fs::write(&test_file, "Hello from chat tool!").unwrap();

    let mock_server = MockServer::start().await;
    let call_count = Arc::new(AtomicUsize::new(0));
    let call_count_clone = call_count.clone();

    // First response: tool_use request (SSE streaming)
    let first_response = text_and_tool_use_sse(
        "Let me read that file.",
        "toolu_chat_001",
        "read",
        r#"{"path": "hello.txt"}"#,
    );

    // Second response: final text after tool result (SSE streaming)
    let second_response = text_sse("The file says: Hello from chat tool!");

    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .and(header("x-api-key", "test-api-key"))
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
        .env("ANTHROPIC_API_KEY", "test-api-key")
        .env("ANTHROPIC_BASE_URL", mock_server.uri())
        .args(["--root", temp_dir.path().to_str().unwrap(), "--no-save"])
        .write_stdin("read hello.txt\n:q\n")
        .assert()
        .success()
        // Assistant text goes to stdout
        .stdout(predicate::str::contains(
            "The file says: Hello from chat tool!",
        ))
        // Tool indicators go to stderr
        .stderr(predicate::str::contains("Running read"));

    assert_eq!(call_count.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn test_chat_streams_multi_chunk_response() {
    let mock_server = MockServer::start().await;

    // Simulate chunked streaming - tokens arrive one by one
    let body = multi_chunk_text_sse(&["Hello", ", ", "this ", "is ", "streaming!"]);

    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(sse_response(&body))
        .expect(1)
        .mount(&mock_server)
        .await;

    cargo_bin_cmd!("zdx-cli")
        .env("ANTHROPIC_API_KEY", "test-api-key")
        .env("ANTHROPIC_BASE_URL", mock_server.uri())
        .args(["--no-save"])
        .write_stdin("test streaming\n:q\n")
        .assert()
        .success()
        // Verify all chunks are combined in order
        .stdout(predicate::str::contains("Hello, this is streaming!"));
}

/// Test that mid-stream SSE errors are handled gracefully.
///
/// After refactor (commit 2b):
/// - Partial assistant text (before error) goes to stdout
/// - Error messages and REPL UI go to stderr
#[tokio::test]
async fn test_chat_handles_sse_error_midstream() {
    let mock_server = MockServer::start().await;

    // Error occurs mid-stream after message_start
    let body = fixtures::error_sse("overloaded_error", "API is temporarily overloaded");

    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(sse_response(&body))
        .mount(&mock_server)
        .await;

    // Chat should show error but continue running (user can still quit)
    cargo_bin_cmd!("zdx-cli")
        .env("ANTHROPIC_API_KEY", "test-api-key")
        .env("ANTHROPIC_BASE_URL", mock_server.uri())
        .args(["--no-save"])
        .write_stdin("hello\n:q\n")
        .assert()
        .success()
        // Partial text before error goes to stdout
        .stdout(predicate::str::contains("Starting..."))
        // Error and REPL UI go to stderr
        .stderr(predicate::str::contains("Error:"))
        .stderr(predicate::str::contains("overloaded_error"))
        .stderr(predicate::str::contains("Goodbye!"));
}
