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

#[tokio::test]
async fn test_exec_includes_system_prompt_from_config() {
    let temp_dir = tempfile::TempDir::new().unwrap();
    let root_dir = tempfile::TempDir::new().unwrap();
    std::fs::write(
        temp_dir.path().join("config.toml"),
        "system_prompt = \"You are a Rust expert.\"",
    )
    .unwrap();

    let mock_server = MockServer::start().await;
    let response = text_response("OK");
    let request_body = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
    let request_body_clone = request_body.clone();

    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(move |req: &wiremock::Request| {
            let body = String::from_utf8_lossy(&req.body).to_string();
            *request_body_clone.lock().unwrap() = body;
            response.clone()
        })
        .mount(&mock_server)
        .await;

    cargo_bin_cmd!("zdx-cli")
        .env("ZDX_HOME", temp_dir.path())
        .env("ANTHROPIC_API_KEY", "test-api-key")
        .env("ANTHROPIC_BASE_URL", mock_server.uri())
        .args([
            "--root",
            root_dir.path().to_str().unwrap(),
            "--no-save",
            "exec",
            "-p",
            "hello",
        ])
        .assert()
        .success();

    let body = request_body.lock().unwrap().clone();
    assert!(
        body.contains("\"system\":\"You are a Rust expert.\""),
        "Request body should contain system prompt. Got: {}",
        body
    );
}

#[tokio::test]
async fn test_exec_system_prompt_flag_overrides_config() {
    let temp_dir = tempfile::TempDir::new().unwrap();
    let root_dir = tempfile::TempDir::new().unwrap();
    std::fs::write(
        temp_dir.path().join("config.toml"),
        "system_prompt = \"From config\"",
    )
    .unwrap();

    let mock_server = MockServer::start().await;
    let response = text_response("OK");
    let request_body = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
    let request_body_clone = request_body.clone();

    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(move |req: &wiremock::Request| {
            let body = String::from_utf8_lossy(&req.body).to_string();
            *request_body_clone.lock().unwrap() = body;
            response.clone()
        })
        .mount(&mock_server)
        .await;

    cargo_bin_cmd!("zdx-cli")
        .env("ZDX_HOME", temp_dir.path())
        .env("ANTHROPIC_API_KEY", "test-api-key")
        .env("ANTHROPIC_BASE_URL", mock_server.uri())
        .args([
            "--root",
            root_dir.path().to_str().unwrap(),
            "--no-save",
            "--system-prompt",
            "From flag",
            "exec",
            "-p",
            "hello",
        ])
        .assert()
        .success();

    let body = request_body.lock().unwrap().clone();
    assert!(
        body.contains("\"system\":\"From flag\""),
        "Request body should contain system prompt override. Got: {}",
        body
    );
    assert!(
        !body.contains("From config"),
        "Request body should not contain config system prompt. Got: {}",
        body
    );
}

#[tokio::test]
async fn test_exec_system_prompt_flag_empty_clears_config() {
    let temp_dir = tempfile::TempDir::new().unwrap();
    let root_dir = tempfile::TempDir::new().unwrap();
    std::fs::write(
        temp_dir.path().join("config.toml"),
        "system_prompt = \"From config\"",
    )
    .unwrap();

    let mock_server = MockServer::start().await;
    let response = text_response("OK");
    let request_body = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
    let request_body_clone = request_body.clone();

    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(move |req: &wiremock::Request| {
            let body = String::from_utf8_lossy(&req.body).to_string();
            *request_body_clone.lock().unwrap() = body;
            response.clone()
        })
        .mount(&mock_server)
        .await;

    cargo_bin_cmd!("zdx-cli")
        .env("ZDX_HOME", temp_dir.path())
        .env("ANTHROPIC_API_KEY", "test-api-key")
        .env("ANTHROPIC_BASE_URL", mock_server.uri())
        .args([
            "--root",
            root_dir.path().to_str().unwrap(),
            "--no-save",
            "--system-prompt",
            "",
            "exec",
            "-p",
            "hello",
        ])
        .assert()
        .success();

    let body = request_body.lock().unwrap().clone();
    assert!(
        !body.contains("\"system\""),
        "Request body should omit system field when cleared. Got: {}",
        body
    );
}

#[tokio::test]
async fn test_exec_includes_agents_md_context() {
    let temp_dir = tempfile::TempDir::new().unwrap();
    std::fs::write(
        temp_dir.path().join("AGENTS.md"),
        "Use snake_case for all functions.",
    )
    .unwrap();

    let mock_server = MockServer::start().await;
    let response = text_response("OK");
    let request_body = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
    let request_body_clone = request_body.clone();

    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(move |req: &wiremock::Request| {
            let body = String::from_utf8_lossy(&req.body).to_string();
            *request_body_clone.lock().unwrap() = body;
            response.clone()
        })
        .mount(&mock_server)
        .await;

    cargo_bin_cmd!("zdx-cli")
        .env("ZDX_HOME", temp_dir.path())
        .env("ANTHROPIC_API_KEY", "test-api-key")
        .env("ANTHROPIC_BASE_URL", mock_server.uri())
        .args([
            "--root",
            temp_dir.path().to_str().unwrap(),
            "--no-save",
            "exec",
            "-p",
            "hello",
        ])
        .assert()
        .success();

    let body = request_body.lock().unwrap().clone();
    assert!(
        body.contains("Use snake_case for all functions."),
        "Request body should contain AGENTS.md content. Got: {}",
        body
    );
    assert!(
        body.contains("# Project Guidelines"),
        "Request body should contain Project Guidelines header. Got: {}",
        body
    );
}
