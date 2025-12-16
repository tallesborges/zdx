mod fixtures;

use assert_cmd::cargo::cargo_bin_cmd;
use fixtures::{multi_chunk_text_sse, sse_response, text_and_tool_use_sse, text_sse};
use predicates::prelude::*;
use std::fs;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use tempfile::TempDir;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

/// Wrap SSE body in a streaming response template.
fn streaming_text_response(text: &str) -> ResponseTemplate {
    sse_response(&text_sse(text))
}

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
        .write_stdin("hi\n:q\n")
        .assert()
        .success()
        .stdout(predicate::str::contains("Hello there!"))
        .stdout(predicate::str::contains("Goodbye!"));
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
        .write_stdin("\n\ntest\n:q\n")
        .assert()
        .success()
        .stdout(predicate::str::contains("Got it!"));
}

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
        .write_stdin(":q\n")
        .assert()
        .success()
        .stdout(predicate::str::contains("ZDX Chat"))
        .stdout(predicate::str::contains(":q to quit"));
}

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
        .write_stdin("hello\n:q\n")
        .assert()
        .success()
        .stdout(predicate::str::contains("Error:"))
        .stdout(predicate::str::contains("429"));
}

#[tokio::test]
async fn test_chat_fails_without_api_key() {
    cargo_bin_cmd!("zdx-cli")
        .env_remove("ANTHROPIC_API_KEY")
        .write_stdin(":q\n")
        .assert()
        .failure()
        .stderr(predicate::str::contains("ANTHROPIC_API_KEY"));
}

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
        .stdout(predicate::str::contains(
            "The file says: Hello from chat tool!",
        ));

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
        .write_stdin("test streaming\n:q\n")
        .assert()
        .success()
        // Verify all chunks are combined in order
        .stdout(predicate::str::contains("Hello, this is streaming!"));
}

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
        .write_stdin("hello\n:q\n")
        .assert()
        .success()
        .stdout(predicate::str::contains("Error:"))
        .stdout(predicate::str::contains("overloaded_error"))
        .stdout(predicate::str::contains("Goodbye!"));
}
