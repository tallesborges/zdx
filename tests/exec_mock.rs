use assert_cmd::cargo::cargo_bin_cmd;
use predicates::prelude::*;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test]
async fn test_exec_returns_assistant_response() {
    let mock_server = MockServer::start().await;

    let response_body = serde_json::json!({
        "id": "msg_123",
        "type": "message",
        "role": "assistant",
        "content": [
            {
                "type": "text",
                "text": "Hello! I'm Claude, an AI assistant."
            }
        ],
        "model": "claude-sonnet-4-20250514",
        "stop_reason": "end_turn",
        "usage": {
            "input_tokens": 10,
            "output_tokens": 20
        }
    });

    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .and(header("x-api-key", "test-api-key"))
        .and(header("anthropic-version", "2023-06-01"))
        .respond_with(ResponseTemplate::new(200).set_body_json(response_body))
        .mount(&mock_server)
        .await;

    cargo_bin_cmd!("zdx-cli")
        .env("ANTHROPIC_API_KEY", "test-api-key")
        .env("ANTHROPIC_BASE_URL", mock_server.uri())
        .args(["exec", "-p", "hello"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Hello! I'm Claude, an AI assistant.",
        ));
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
