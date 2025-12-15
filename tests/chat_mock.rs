use assert_cmd::cargo::cargo_bin_cmd;
use predicates::prelude::*;
use std::fs;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use tempfile::TempDir;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

fn mock_response(text: &str) -> serde_json::Value {
    serde_json::json!({
        "id": "msg_123",
        "type": "message",
        "role": "assistant",
        "content": [
            {
                "type": "text",
                "text": text
            }
        ],
        "model": "claude-sonnet-4-20250514",
        "stop_reason": "end_turn",
        "usage": {
            "input_tokens": 10,
            "output_tokens": 20
        }
    })
}

#[tokio::test]
async fn test_chat_responds_and_exits_on_quit() {
    let mock_server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .and(header("x-api-key", "test-api-key"))
        .and(header("anthropic-version", "2023-06-01"))
        .respond_with(ResponseTemplate::new(200).set_body_json(mock_response("Hello there!")))
        .expect(1)
        .mount(&mock_server)
        .await;

    cargo_bin_cmd!("zdx-cli")
        .env("ANTHROPIC_API_KEY", "test-api-key")
        .env("ANTHROPIC_BASE_URL", mock_server.uri())
        .args(["chat"])
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
        .respond_with(ResponseTemplate::new(200).set_body_json(mock_response("I'm Claude!")))
        .expect(1)
        .named("first_message")
        .mount(&mock_server)
        .await;

    cargo_bin_cmd!("zdx-cli")
        .env("ANTHROPIC_API_KEY", "test-api-key")
        .env("ANTHROPIC_BASE_URL", mock_server.uri())
        .args(["chat"])
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
        .respond_with(ResponseTemplate::new(200).set_body_json(mock_response("Got it!")))
        .expect(1)
        .mount(&mock_server)
        .await;

    // Empty lines should be skipped, only "test" should trigger API call
    cargo_bin_cmd!("zdx-cli")
        .env("ANTHROPIC_API_KEY", "test-api-key")
        .env("ANTHROPIC_BASE_URL", mock_server.uri())
        .args(["chat"])
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
        .respond_with(ResponseTemplate::new(200).set_body_json(mock_response("Hi!")))
        .mount(&mock_server)
        .await;

    cargo_bin_cmd!("zdx-cli")
        .env("ANTHROPIC_API_KEY", "test-api-key")
        .env("ANTHROPIC_BASE_URL", mock_server.uri())
        .args(["chat"])
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
        .args(["chat"])
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
        .args(["chat"])
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

    let first_response = serde_json::json!({
        "id": "msg_001",
        "type": "message",
        "role": "assistant",
        "content": [
            {
                "type": "text",
                "text": "Let me read that file."
            },
            {
                "type": "tool_use",
                "id": "toolu_chat_001",
                "name": "read",
                "input": {"path": "hello.txt"}
            }
        ],
        "model": "claude-sonnet-4-20250514",
        "stop_reason": "tool_use",
        "usage": {"input_tokens": 10, "output_tokens": 20}
    });

    let second_response = serde_json::json!({
        "id": "msg_002",
        "type": "message",
        "role": "assistant",
        "content": [
            {
                "type": "text",
                "text": "The file says: Hello from chat tool!"
            }
        ],
        "model": "claude-sonnet-4-20250514",
        "stop_reason": "end_turn",
        "usage": {"input_tokens": 30, "output_tokens": 15}
    });

    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .and(header("x-api-key", "test-api-key"))
        .respond_with(move |_req: &Request| {
            let count = call_count_clone.fetch_add(1, Ordering::SeqCst);
            if count == 0 {
                ResponseTemplate::new(200).set_body_json(first_response.clone())
            } else {
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
            "chat",
            "--root",
            temp_dir.path().to_str().unwrap(),
            "--no-save",
        ])
        .write_stdin("read hello.txt\n:q\n")
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "The file says: Hello from chat tool!",
        ));

    assert_eq!(call_count.load(Ordering::SeqCst), 2);
}
