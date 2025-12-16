mod fixtures;

use assert_cmd::cargo::cargo_bin_cmd;
use fixtures::{
    error_sse, multi_chunk_text_sse, sse_response, text_response, text_sse_with_pings, tool_use_sse,
};
use predicates::prelude::*;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test]
async fn test_exec_streams_text_response() {
    let mock_server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .and(header("x-api-key", "test-api-key"))
        .and(header("anthropic-version", "2023-06-01"))
        .respond_with(text_response("Hello, world!"))
        .mount(&mock_server)
        .await;

    cargo_bin_cmd!("zdx-cli")
        .env("ANTHROPIC_API_KEY", "test-api-key")
        .env("ANTHROPIC_BASE_URL", mock_server.uri())
        .args(["exec", "-p", "hello"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Hello, world!"));
}

#[tokio::test]
async fn test_exec_streaming_preserves_text_order() {
    let mock_server = MockServer::start().await;

    let body = multi_chunk_text_sse(&["Rust ", "is ", "a ", "systems ", "language."]);

    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(sse_response(&body))
        .mount(&mock_server)
        .await;

    cargo_bin_cmd!("zdx-cli")
        .env("ANTHROPIC_API_KEY", "test-api-key")
        .env("ANTHROPIC_BASE_URL", mock_server.uri())
        .args(["exec", "-p", "describe rust"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Rust is a systems language."));
}

#[tokio::test]
async fn test_exec_handles_empty_delta_events() {
    let mock_server = MockServer::start().await;

    // Include empty deltas that should be skipped
    let body = multi_chunk_text_sse(&["", "Hello", ""]);

    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(sse_response(&body))
        .mount(&mock_server)
        .await;

    cargo_bin_cmd!("zdx-cli")
        .env("ANTHROPIC_API_KEY", "test-api-key")
        .env("ANTHROPIC_BASE_URL", mock_server.uri())
        .args(["exec", "-p", "hello"])
        .assert()
        .success()
        // Only "Hello" should appear, empty deltas skipped
        .stdout(predicate::str::is_match(r"^Hello\n$").unwrap());
}

#[tokio::test]
async fn test_exec_fails_without_api_key() {
    cargo_bin_cmd!("zdx-cli")
        .env_remove("ANTHROPIC_API_KEY")
        .args(["exec", "-p", "hello"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("ANTHROPIC_API_KEY"));
}

#[tokio::test]
async fn test_exec_handles_api_error() {
    let mock_server = MockServer::start().await;

    let error_body = serde_json::json!({
        "type": "error",
        "error": {
            "type": "authentication_error",
            "message": "Invalid API key"
        }
    });

    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(401).set_body_json(error_body))
        .mount(&mock_server)
        .await;

    cargo_bin_cmd!("zdx-cli")
        .env("ANTHROPIC_API_KEY", "invalid-key")
        .env("ANTHROPIC_BASE_URL", mock_server.uri())
        .args(["exec", "-p", "hello"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("401"));
}

#[tokio::test]
async fn test_exec_handles_api_error_midstream() {
    let mock_server = MockServer::start().await;

    let body = error_sse("overloaded_error", "API is temporarily overloaded");

    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(sse_response(&body))
        .mount(&mock_server)
        .await;

    cargo_bin_cmd!("zdx-cli")
        .env("ANTHROPIC_API_KEY", "test-api-key")
        .env("ANTHROPIC_BASE_URL", mock_server.uri())
        .args(["exec", "-p", "hello"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("overloaded_error"))
        .stderr(predicate::str::contains("API is temporarily overloaded"));
}

#[tokio::test]
async fn test_exec_tool_use_midstream() {
    let mock_server = MockServer::start().await;

    // First response: tool_use request
    let tool_use_body = tool_use_sse("toolu_abc123", "bash", r#"{"command": "echo hello"}"#);

    // Second response: final text after tool result
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(sse_response(&tool_use_body))
        .up_to_n_times(1)
        .mount(&mock_server)
        .await;

    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(text_response("The command output was: hello"))
        .mount(&mock_server)
        .await;

    cargo_bin_cmd!("zdx-cli")
        .env("ANTHROPIC_API_KEY", "test-api-key")
        .env("ANTHROPIC_BASE_URL", mock_server.uri())
        .args(["exec", "-p", "run echo hello"])
        .assert()
        .success()
        .stdout(predicate::str::contains("The command output was: hello"));
}

#[tokio::test]
async fn test_exec_handles_ping_events() {
    let mock_server = MockServer::start().await;

    let body = text_sse_with_pings("Pong!");

    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(sse_response(&body))
        .mount(&mock_server)
        .await;

    cargo_bin_cmd!("zdx-cli")
        .env("ANTHROPIC_API_KEY", "test-api-key")
        .env("ANTHROPIC_BASE_URL", mock_server.uri())
        .args(["exec", "-p", "ping"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Pong!"));
}
