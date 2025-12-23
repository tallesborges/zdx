mod fixtures;

use assert_cmd::cargo::cargo_bin_cmd;
use fixtures::{
    error_sse, multi_chunk_text_sse, sse_response, text_response, text_sse_with_pings, tool_use_sse,
};
use predicates::prelude::*;
use tempfile::TempDir;
use wiremock::matchers::{header, header_regex, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// Creates a temp ZDX_HOME directory for test isolation.
/// Returns the TempDir (must be kept alive for the duration of the test).
fn temp_zdx_home() -> TempDir {
    TempDir::new().expect("create temp zdx home")
}

#[tokio::test]
async fn test_exec_streams_text_response() {
    let mock_server = MockServer::start().await;
    let zdx_home = temp_zdx_home();

    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .and(header("x-api-key", "test-api-key"))
        .and(header("anthropic-version", "2023-06-01"))
        .respond_with(text_response("Hello, world!"))
        .mount(&mock_server)
        .await;

    cargo_bin_cmd!("zdx-cli")
        .env("ZDX_HOME", zdx_home.path())
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
    let zdx_home = temp_zdx_home();

    cargo_bin_cmd!("zdx-cli")
        .env("ZDX_HOME", zdx_home.path())
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
        .code(1)
        // Structured error: "Error [http_status]: HTTP 401: Invalid API key"
        .stderr(predicate::str::contains("Error [http_status]"))
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
        .code(1)
        // Structured error: "Error [api_error]: overloaded_error: API is temporarily overloaded"
        .stderr(predicate::str::contains("Error [api_error]"))
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
        body.contains("\"text\":\"You are a Rust expert.\""),
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
        body.contains("\"text\":\"From flag\""),
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
    // System should contain only Claude Code prompt, not the config prompt
    assert!(
        body.contains("Claude Code"),
        "Request body should contain Claude Code system prompt. Got: {}",
        body
    );
    assert!(
        !body.contains("From config"),
        "Request body should not contain config system prompt. Got: {}",
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
        body.contains("# Project Context"),
        "Request body should contain Project Context header. Got: {}",
        body
    );
}

/// Tests that the config anthropic_base_url is used when env var is absent.
#[tokio::test]
async fn test_exec_uses_config_base_url_when_env_absent() {
    let temp_dir = tempfile::TempDir::new().unwrap();
    let mock_server = MockServer::start().await;

    // Write config with custom base URL
    std::fs::write(
        temp_dir.path().join("config.toml"),
        format!("anthropic_base_url = \"{}\"\n", mock_server.uri()),
    )
    .unwrap();

    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .and(header("x-api-key", "test-api-key"))
        .respond_with(text_response("Config URL works!"))
        .expect(1)
        .mount(&mock_server)
        .await;

    // Note: ANTHROPIC_BASE_URL is NOT set - should use config
    cargo_bin_cmd!("zdx-cli")
        .env("ZDX_HOME", temp_dir.path())
        .env("ANTHROPIC_API_KEY", "test-api-key")
        .env_remove("ANTHROPIC_BASE_URL")
        .args(["--no-save", "exec", "-p", "hello"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Config URL works!"));
}

/// Tests that empty anthropic_base_url in config uses the default.
#[tokio::test]
async fn test_exec_empty_config_base_url_uses_default() {
    let temp_dir = tempfile::TempDir::new().unwrap();

    // Write config with empty base URL
    std::fs::write(
        temp_dir.path().join("config.toml"),
        "anthropic_base_url = \"\"\n",
    )
    .unwrap();

    // This should fail because it tries to hit the real API without valid key
    // But if empty string was used, it would fail differently
    // We verify it tries the default by checking the error message
    cargo_bin_cmd!("zdx-cli")
        .env("ZDX_HOME", temp_dir.path())
        .env("ANTHROPIC_API_KEY", "test-api-key")
        .env_remove("ANTHROPIC_BASE_URL")
        .args(["--no-save", "exec", "-p", "hello"])
        .assert()
        .failure()
        // Should fail because it's hitting the real API (not a mock), not empty URL error
        .stderr(predicate::str::contains("401").or(predicate::str::contains("api.anthropic.com")));
}

/// Tests that env var takes precedence over config base URL.
#[tokio::test]
async fn test_exec_env_base_url_takes_precedence_over_config() {
    let temp_dir = tempfile::TempDir::new().unwrap();
    let mock_server = MockServer::start().await;
    let decoy_server = MockServer::start().await;

    // Write config with decoy URL that should NOT be hit
    std::fs::write(
        temp_dir.path().join("config.toml"),
        format!("anthropic_base_url = \"{}\"\n", decoy_server.uri()),
    )
    .unwrap();

    // The decoy should NOT receive any requests
    Mock::given(method("POST"))
        .respond_with(text_response("WRONG SERVER!"))
        .expect(0)
        .mount(&decoy_server)
        .await;

    // The env-specified server SHOULD receive the request
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(text_response("Env URL wins!"))
        .expect(1)
        .mount(&mock_server)
        .await;

    cargo_bin_cmd!("zdx-cli")
        .env("ZDX_HOME", temp_dir.path())
        .env("ANTHROPIC_API_KEY", "test-api-key")
        .env("ANTHROPIC_BASE_URL", mock_server.uri())
        .args(["--no-save", "exec", "-p", "hello"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Env URL wins!"));
}

/// Tests that invalid URL in config yields an error with exit code 2.
#[tokio::test]
async fn test_exec_invalid_config_base_url_exits_with_error() {
    let temp_dir = tempfile::TempDir::new().unwrap();

    // Write config with invalid URL
    std::fs::write(
        temp_dir.path().join("config.toml"),
        "anthropic_base_url = \"not-a-valid-url\"\n",
    )
    .unwrap();

    cargo_bin_cmd!("zdx-cli")
        .env("ZDX_HOME", temp_dir.path())
        .env("ANTHROPIC_API_KEY", "test-api-key")
        .env_remove("ANTHROPIC_BASE_URL")
        .args(["--no-save", "exec", "-p", "hello"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("Invalid Anthropic base URL"));
}

/// Tests that stdout contains ONLY assistant text during tool use.
/// Tool indicators and status messages must go to stderr only.
#[tokio::test]
async fn test_exec_stdout_contains_only_assistant_text() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    let mock_server = MockServer::start().await;
    let call_count = Arc::new(AtomicUsize::new(0));
    let call_count_clone = call_count.clone();

    // First response: tool_use request (model wants to run a command)
    let tool_use_body = tool_use_sse("toolu_test123", "bash", r#"{"command": "echo hello"}"#);
    // Second response: final text after tool result
    let final_response = fixtures::text_sse("The command ran successfully.");

    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(move |_req: &wiremock::Request| {
            let count = call_count_clone.fetch_add(1, Ordering::SeqCst);
            if count == 0 {
                sse_response(&tool_use_body)
            } else {
                sse_response(&final_response)
            }
        })
        .expect(2)
        .mount(&mock_server)
        .await;

    let output = cargo_bin_cmd!("zdx-cli")
        .env("ANTHROPIC_API_KEY", "test-api-key")
        .env("ANTHROPIC_BASE_URL", mock_server.uri())
        .args(["--no-save", "exec", "-p", "run echo hello"])
        .output()
        .expect("Failed to execute command");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    // stdout should contain only assistant text
    assert!(
        stdout.contains("The command ran successfully."),
        "stdout should contain assistant text. Got stdout: '{}'",
        stdout
    );

    // stdout should NOT contain tool indicators
    assert!(
        !stdout.contains("⚙"),
        "stdout should not contain tool indicator emoji. Got stdout: '{}'",
        stdout
    );
    assert!(
        !stdout.contains("Running"),
        "stdout should not contain 'Running' (tool status). Got stdout: '{}'",
        stdout
    );
    assert!(
        !stdout.contains("Done"),
        "stdout should not contain 'Done' (tool status). Got stdout: '{}'",
        stdout
    );

    // stderr should contain tool indicators
    assert!(
        stderr.contains("⚙ Running bash"),
        "stderr should contain tool indicator. Got stderr: '{}'",
        stderr
    );
    assert!(
        stderr.contains("Done"),
        "stderr should contain 'Done'. Got stderr: '{}'",
        stderr
    );
}

/// Tests that non-JSON error bodies still produce structured errors.
#[tokio::test]
async fn test_exec_handles_non_json_error_body() {
    let mock_server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(503).set_body_string("Service Temporarily Unavailable"))
        .mount(&mock_server)
        .await;

    cargo_bin_cmd!("zdx-cli")
        .env("ANTHROPIC_API_KEY", "test-api-key")
        .env("ANTHROPIC_BASE_URL", mock_server.uri())
        .args(["exec", "-p", "hello"])
        .assert()
        .failure()
        .code(1)
        // Structured error format with http_status kind
        .stderr(predicate::str::contains("Error [http_status]"))
        .stderr(predicate::str::contains("503"));
}

/// Tests that connection failures produce timeout-category errors with exit code 1.
#[tokio::test]
async fn test_exec_connection_refused_error() {
    // Use a port that's unlikely to have anything listening
    cargo_bin_cmd!("zdx-cli")
        .env("ANTHROPIC_API_KEY", "test-api-key")
        .env("ANTHROPIC_BASE_URL", "http://127.0.0.1:9999")
        .args(["--no-save", "exec", "-p", "hello"])
        .assert()
        .failure()
        .code(1)
        // Connection failures map to timeout category
        .stderr(predicate::str::contains("Error [timeout]"))
        .stderr(predicate::str::contains("Connection failed"));
}

/// Tests that error details are printed when available (JSON error body).
#[tokio::test]
async fn test_exec_error_shows_details_when_available() {
    let mock_server = MockServer::start().await;

    let error_body = serde_json::json!({
        "type": "error",
        "error": {
            "type": "rate_limit_error",
            "message": "Rate limit exceeded. Please retry after 60 seconds."
        }
    });

    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(429).set_body_json(&error_body))
        .mount(&mock_server)
        .await;

    let output = cargo_bin_cmd!("zdx-cli")
        .env("ANTHROPIC_API_KEY", "test-api-key")
        .env("ANTHROPIC_BASE_URL", mock_server.uri())
        .args(["exec", "-p", "hello"])
        .output()
        .expect("Failed to execute command");

    let stderr = String::from_utf8_lossy(&output.stderr);

    // Should have structured error prefix
    assert!(
        stderr.contains("Error [http_status]"),
        "stderr should contain structured error prefix. Got: '{}'",
        stderr
    );
    // Should have the extracted message in the one-liner
    assert!(
        stderr.contains("Rate limit exceeded"),
        "stderr should contain error message. Got: '{}'",
        stderr
    );
    // Should show details (raw JSON body)
    assert!(
        stderr.contains("Details:"),
        "stderr should show details section. Got: '{}'",
        stderr
    );
}

/// Tests that exit code is 1 for all provider errors.
#[tokio::test]
async fn test_exec_error_exit_code_is_one() {
    let mock_server = MockServer::start().await;

    // 500 Internal Server Error
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(500).set_body_string("Internal Server Error"))
        .mount(&mock_server)
        .await;

    cargo_bin_cmd!("zdx-cli")
        .env("ANTHROPIC_API_KEY", "test-api-key")
        .env("ANTHROPIC_BASE_URL", mock_server.uri())
        .args(["exec", "-p", "hello"])
        .assert()
        .failure()
        .code(1);
}

/// Test: OAuth authentication uses Bearer header instead of x-api-key (SPEC §OAuth).
#[tokio::test]
async fn test_exec_uses_oauth_bearer_header_when_logged_in() {
    let mock_server = MockServer::start().await;
    let zdx_home = temp_zdx_home();

    // Create OAuth credentials file in temp ZDX_HOME
    let oauth_json = serde_json::json!({
        "anthropic": {
            "type": "oauth",
            "refresh": "test-refresh-token",
            "access": "test-oauth-access-token",
            // Expires far in the future (year 2100)
            "expires": 4102444800000u64
        }
    });
    std::fs::write(
        zdx_home.path().join("oauth.json"),
        serde_json::to_string(&oauth_json).unwrap(),
    )
    .unwrap();

    // Mock expects Authorization: Bearer header (OAuth) instead of x-api-key
    // Also requires anthropic-beta header with oauth and tool streaming flags
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .and(header("Authorization", "Bearer test-oauth-access-token"))
        .and(header("anthropic-version", "2023-06-01"))
        .and(header_regex(
            "anthropic-beta",
            "oauth-2025-04-20.*fine-grained-tool-streaming.*interleaved-thinking",
        ))
        .respond_with(text_response("Hello from OAuth!"))
        .mount(&mock_server)
        .await;

    cargo_bin_cmd!("zdx-cli")
        .env("ZDX_HOME", zdx_home.path())
        .env("ANTHROPIC_BASE_URL", mock_server.uri())
        // Note: No ANTHROPIC_API_KEY needed when OAuth is available
        .env_remove("ANTHROPIC_API_KEY")
        .args(["exec", "-p", "hello"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Hello from OAuth!"));
}

/// Test: API key takes precedence when no OAuth credentials exist.
#[tokio::test]
async fn test_exec_uses_api_key_when_no_oauth() {
    let mock_server = MockServer::start().await;
    let zdx_home = temp_zdx_home();

    // No OAuth file created in zdx_home

    // Mock expects x-api-key header (API key auth) with beta header
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .and(header("x-api-key", "my-api-key"))
        .and(header("anthropic-version", "2023-06-01"))
        .and(header_regex(
            "anthropic-beta",
            "fine-grained-tool-streaming.*interleaved-thinking",
        ))
        .respond_with(text_response("Hello from API key!"))
        .mount(&mock_server)
        .await;

    cargo_bin_cmd!("zdx-cli")
        .env("ZDX_HOME", zdx_home.path())
        .env("ANTHROPIC_API_KEY", "my-api-key")
        .env("ANTHROPIC_BASE_URL", mock_server.uri())
        .args(["exec", "-p", "hello"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Hello from API key!"));
}
