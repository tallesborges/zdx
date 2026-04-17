//! Tests for the tool use loop with wiremock.
//!
//! Simulates a two-step interaction:
//! 1. First response asks for `tool_use(read)`
//! 2. Second response returns final text
//!
//! Verifies that the second request includes `tool_result` block.

mod fixtures;

use std::fs;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use assert_cmd::cargo::cargo_bin_cmd;
use fixtures::{sse_response, text_and_tool_use_sse, text_sse, tool_use_sse};
use predicates::prelude::*;
use tempfile::TempDir;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, Request};

/// Creates a temp `ZDX_HOME` directory for test isolation.
fn temp_zdx_home() -> TempDir {
    TempDir::new().expect("create temp zdx home")
}

fn can_bind_localhost() -> bool {
    std::net::TcpListener::bind("127.0.0.1:0").is_ok()
}

#[tokio::test]
async fn test_tool_use_loop_reads_file() {
    if !can_bind_localhost() {
        eprintln!("Skipping: cannot bind localhost TCP port in this environment.");
        return;
    }
    let zdx_home = temp_zdx_home();
    let temp_dir = TempDir::new().unwrap();
    let test_file = temp_dir.path().join("test.txt");
    fs::write(&test_file, "Hello from file!").unwrap();

    let mock_server = MockServer::start().await;
    let call_count = Arc::new(AtomicUsize::new(0));
    let call_count_clone = Arc::clone(&call_count);

    let first_response = text_and_tool_use_sse(
        "I'll read that file for you.",
        "toolu_001",
        "read",
        r#"{"file_path": "test.txt"}"#,
    );
    let second_response = text_sse("The file contains: Hello from file!");

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

    cargo_bin_cmd!("zdx")
        .env("ZDX_HOME", zdx_home.path())
        .env("ANTHROPIC_API_KEY", "test-api-key")
        .env("ANTHROPIC_BASE_URL", mock_server.uri())
        .args([
            "--root",
            temp_dir.path().to_str().unwrap(),
            "--no-thread",
            "exec",
            "-p",
            "Read test.txt",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "The file contains: Hello from file!",
        ));

    assert_eq!(call_count.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn test_tool_use_loop_second_request_has_tool_result() {
    if !can_bind_localhost() {
        eprintln!("Skipping: cannot bind localhost TCP port in this environment.");
        return;
    }
    let zdx_home = temp_zdx_home();
    let temp_dir = TempDir::new().unwrap();
    let test_file = temp_dir.path().join("data.txt");
    fs::write(&test_file, "secret data").unwrap();

    let mock_server = MockServer::start().await;
    let call_count = Arc::new(AtomicUsize::new(0));
    let call_count_clone = Arc::clone(&call_count);
    let second_request_body = Arc::new(std::sync::Mutex::new(String::new()));
    let second_request_body_clone = Arc::clone(&second_request_body);

    let first_response = tool_use_sse("toolu_abc123", "read", r#"{"file_path": "data.txt"}"#);
    let second_response = text_sse("Done!");

    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(move |req: &Request| {
            let count = call_count_clone.fetch_add(1, Ordering::SeqCst);
            if count == 0 {
                sse_response(&first_response)
            } else {
                let body = String::from_utf8_lossy(&req.body).to_string();
                *second_request_body_clone.lock().unwrap() = body;
                sse_response(&second_response)
            }
        })
        .expect(2)
        .mount(&mock_server)
        .await;

    cargo_bin_cmd!("zdx")
        .env("ZDX_HOME", zdx_home.path())
        .env("ANTHROPIC_API_KEY", "test-api-key")
        .env("ANTHROPIC_BASE_URL", mock_server.uri())
        .args([
            "--root",
            temp_dir.path().to_str().unwrap(),
            "--no-thread",
            "exec",
            "-p",
            "Read data.txt",
        ])
        .assert()
        .success();

    let body = second_request_body.lock().unwrap().clone();
    assert!(
        body.contains("tool_result"),
        "Second request should contain tool_result block. Got: {body}"
    );
    assert!(
        body.contains("toolu_abc123"),
        "Second request should reference the tool_use_id. Got: {body}"
    );
    assert!(
        body.contains("secret data"),
        "Second request should contain the file content. Got: {body}"
    );
}

#[tokio::test]
async fn test_tool_read_outside_root_allowed() {
    if !can_bind_localhost() {
        eprintln!("Skipping: cannot bind localhost TCP port in this environment.");
        return;
    }
    let zdx_home = temp_zdx_home();
    let root_dir = TempDir::new().unwrap();
    let outside_dir = TempDir::new().unwrap();
    let outside_file = outside_dir.path().join("outside.txt");
    fs::write(&outside_file, "outside content").unwrap();
    let outside_path = outside_file.to_str().unwrap().to_string();

    let mock_server = MockServer::start().await;
    let call_count = Arc::new(AtomicUsize::new(0));
    let call_count_clone = Arc::clone(&call_count);
    let second_request_body = Arc::new(std::sync::Mutex::new(String::new()));
    let second_request_body_clone = Arc::clone(&second_request_body);

    let input_json = format!(r#"{{"file_path": "{outside_path}"}}"#);
    let first_response = tool_use_sse("toolu_evil", "read", &input_json);
    let second_response = text_sse("File read successfully.");

    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(move |req: &Request| {
            let count = call_count_clone.fetch_add(1, Ordering::SeqCst);
            if count == 0 {
                sse_response(&first_response)
            } else {
                let body = String::from_utf8_lossy(&req.body).to_string();
                *second_request_body_clone.lock().unwrap() = body;
                sse_response(&second_response)
            }
        })
        .expect(2)
        .mount(&mock_server)
        .await;

    cargo_bin_cmd!("zdx")
        .env("ZDX_HOME", zdx_home.path())
        .env("ANTHROPIC_API_KEY", "test-api-key")
        .env("ANTHROPIC_BASE_URL", mock_server.uri())
        .args([
            "--root",
            root_dir.path().to_str().unwrap(),
            "--no-thread",
            "exec",
            "-p",
            "Read outside file",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("File read successfully."));

    let body = second_request_body.lock().unwrap().clone();
    assert!(
        body.contains("outside content"),
        "Tool result should include outside file content. Got: {body}"
    );
    assert!(
        !body.contains("\"is_error\":true"),
        "Tool result should not be marked as error. Got: {body}"
    );
}

#[tokio::test]
async fn test_tool_shows_activity_indicator() {
    if !can_bind_localhost() {
        eprintln!("Skipping: cannot bind localhost TCP port in this environment.");
        return;
    }
    let zdx_home = temp_zdx_home();
    let mock_server = MockServer::start().await;
    let first_response = tool_use_sse(
        "toolu_indicator",
        "read",
        r#"{"file_path": "nonexistent.txt"}"#,
    );
    let second_response = text_sse("Done.");

    let call_count = Arc::new(AtomicUsize::new(0));
    let call_count_clone = Arc::clone(&call_count);

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

    cargo_bin_cmd!("zdx")
        .env("ZDX_HOME", zdx_home.path())
        .env("ANTHROPIC_API_KEY", "test-api-key")
        .env("ANTHROPIC_BASE_URL", mock_server.uri())
        .args(["--no-thread", "exec", "-p", "Show indicator"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "\"type\":\"tool_started\",\"id\":\"toolu_indicator\",\"name\":\"read\"",
        ))
        .stdout(predicate::str::contains(
            "\"type\":\"tool_completed\",\"id\":\"toolu_indicator\"",
        ));
}

#[tokio::test]
async fn test_exec_omits_assistant_deltas_from_stdout() {
    if !can_bind_localhost() {
        eprintln!("Skipping: cannot bind localhost TCP port in this environment.");
        return;
    }
    let zdx_home = temp_zdx_home();
    let mock_server = MockServer::start().await;
    let response = text_sse("Hello world");

    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(sse_response(&response))
        .expect(1)
        .mount(&mock_server)
        .await;

    cargo_bin_cmd!("zdx")
        .env("ZDX_HOME", zdx_home.path())
        .env("ANTHROPIC_API_KEY", "test-api-key")
        .env("ANTHROPIC_BASE_URL", mock_server.uri())
        .args(["--no-thread", "exec", "-p", "Say hello"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "\"type\":\"assistant_completed\",\"text\":\"Hello world\"",
        ))
        .stdout(predicate::str::contains("\"type\":\"assistant_delta\"").not());
}

#[tokio::test]
async fn test_exec_omits_empty_reasoning_completed_from_stdout() {
    if !can_bind_localhost() {
        eprintln!("Skipping: cannot bind localhost TCP port in this environment.");
        return;
    }
    let zdx_home = temp_zdx_home();
    let mock_server = MockServer::start().await;
    let response = text_sse("Hello world");

    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(sse_response(&response))
        .expect(1)
        .mount(&mock_server)
        .await;

    cargo_bin_cmd!("zdx")
        .env("ZDX_HOME", zdx_home.path())
        .env("ANTHROPIC_API_KEY", "test-api-key")
        .env("ANTHROPIC_BASE_URL", mock_server.uri())
        .args(["--no-thread", "exec", "-p", "Say hello"])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"type\":\"reasoning_completed\"").not());
}

#[tokio::test]
async fn test_exec_keeps_reasoning_text_without_replay_in_stdout() {
    if !can_bind_localhost() {
        eprintln!("Skipping: cannot bind localhost TCP port in this environment.");
        return;
    }
    let zdx_home = temp_zdx_home();
    let mock_server = MockServer::start().await;
    let response = text_sse("Hello world");

    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(sse_response(&response))
        .expect(1)
        .mount(&mock_server)
        .await;

    cargo_bin_cmd!("zdx")
        .env("ZDX_HOME", zdx_home.path())
        .env("ANTHROPIC_API_KEY", "test-api-key")
        .env("ANTHROPIC_BASE_URL", mock_server.uri())
        .args(["--no-thread", "exec", "-p", "Say hello"])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"replay\":").not());
}

#[tokio::test]
async fn test_exec_filter_turn_finished_only_emits_turn_finished() {
    if !can_bind_localhost() {
        eprintln!("Skipping: cannot bind localhost TCP port in this environment.");
        return;
    }
    let zdx_home = temp_zdx_home();
    let mock_server = MockServer::start().await;
    let response = text_sse("Hello world");

    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(sse_response(&response))
        .expect(1)
        .mount(&mock_server)
        .await;

    cargo_bin_cmd!("zdx")
        .env("ZDX_HOME", zdx_home.path())
        .env("ANTHROPIC_API_KEY", "test-api-key")
        .env("ANTHROPIC_BASE_URL", mock_server.uri())
        .args([
            "--no-thread",
            "exec",
            "--filter",
            "turn_finished",
            "-p",
            "Say hello",
        ])
        .assert()
        .success()
        .stdout(
            predicate::str::contains("\"type\":\"turn_finished\"")
                .and(predicate::str::contains("\"final_text\":\"Hello world\"")),
        )
        .stdout(predicate::str::contains("\"type\":\"assistant_completed\"").not());
}

#[tokio::test]
async fn test_tool_use_loop_writes_file() {
    if !can_bind_localhost() {
        eprintln!("Skipping: cannot bind localhost TCP port in this environment.");
        return;
    }
    let zdx_home = temp_zdx_home();
    let temp_dir = TempDir::new().unwrap();

    let mock_server = MockServer::start().await;
    let call_count = Arc::new(AtomicUsize::new(0));
    let call_count_clone = Arc::clone(&call_count);
    let second_request_body = Arc::new(std::sync::Mutex::new(String::new()));
    let second_request_body_clone = Arc::clone(&second_request_body);

    let first_response = tool_use_sse(
        "toolu_write001",
        "write",
        r#"{"file_path": "output.txt", "content": "Hello from write tool!"}"#,
    );
    let second_response = text_sse("File written successfully!");

    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(move |req: &Request| {
            let count = call_count_clone.fetch_add(1, Ordering::SeqCst);
            if count == 0 {
                sse_response(&first_response)
            } else {
                let body = String::from_utf8_lossy(&req.body).to_string();
                *second_request_body_clone.lock().unwrap() = body;
                sse_response(&second_response)
            }
        })
        .expect(2)
        .mount(&mock_server)
        .await;

    cargo_bin_cmd!("zdx")
        .env("ZDX_HOME", zdx_home.path())
        .env("ANTHROPIC_API_KEY", "test-api-key")
        .env("ANTHROPIC_BASE_URL", mock_server.uri())
        .args([
            "--root",
            temp_dir.path().to_str().unwrap(),
            "--no-thread",
            "exec",
            "-p",
            "Write output.txt with greeting",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("File written successfully!"));

    assert_eq!(call_count.load(Ordering::SeqCst), 2);

    // Assert the file was actually written
    let file_path = temp_dir.path().join("output.txt");
    assert!(file_path.exists(), "File should have been created");
    let content = fs::read_to_string(&file_path).unwrap();
    assert_eq!(content, "Hello from write tool!");

    // Assert tool_result was sent in the second request
    let body = second_request_body.lock().unwrap().clone();
    assert!(
        body.contains("tool_result"),
        "Second request should contain tool_result block. Got: {body}"
    );
    assert!(
        body.contains("toolu_write001"),
        "Second request should reference the tool_use_id. Got: {body}"
    );
    // The ok:true appears inside a JSON string, so it's escaped as \"ok\":true
    assert!(
        body.contains(r#"\"ok\":true"#),
        "Tool result should indicate success. Got: {body}"
    );
}

#[tokio::test]
async fn test_tool_use_loop_edits_file() {
    if !can_bind_localhost() {
        eprintln!("Skipping: cannot bind localhost TCP port in this environment.");
        return;
    }
    let zdx_home = temp_zdx_home();
    let temp_dir = TempDir::new().unwrap();
    let test_file = temp_dir.path().join("target.txt");
    fs::write(&test_file, "Hello world! This is a test.").unwrap();

    let mock_server = MockServer::start().await;
    let call_count = Arc::new(AtomicUsize::new(0));
    let call_count_clone = Arc::clone(&call_count);
    let second_request_body = Arc::new(std::sync::Mutex::new(String::new()));
    let second_request_body_clone = Arc::clone(&second_request_body);

    let first_response = tool_use_sse(
        "toolu_edit001",
        "edit",
        r#"{"file_path": "target.txt", "old_string": "world", "new_string": "Rust"}"#,
    );
    let second_response = text_sse("File edited successfully!");

    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(move |req: &Request| {
            let count = call_count_clone.fetch_add(1, Ordering::SeqCst);
            if count == 0 {
                sse_response(&first_response)
            } else {
                let body = String::from_utf8_lossy(&req.body).to_string();
                *second_request_body_clone.lock().unwrap() = body;
                sse_response(&second_response)
            }
        })
        .expect(2)
        .mount(&mock_server)
        .await;

    cargo_bin_cmd!("zdx")
        .env("ZDX_HOME", zdx_home.path())
        .env("ANTHROPIC_API_KEY", "test-api-key")
        .env("ANTHROPIC_BASE_URL", mock_server.uri())
        .args([
            "--root",
            temp_dir.path().to_str().unwrap(),
            "--no-thread",
            "exec",
            "-p",
            "Edit target.txt: replace world with Rust",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("File edited successfully!"));

    assert_eq!(call_count.load(Ordering::SeqCst), 2);

    // Assert the file was actually edited
    let content = fs::read_to_string(&test_file).unwrap();
    assert_eq!(content, "Hello Rust! This is a test.");

    // Assert tool_result was sent in the second request
    let body = second_request_body.lock().unwrap().clone();
    assert!(
        body.contains("tool_result"),
        "Second request should contain tool_result block. Got: {body}"
    );
    assert!(
        body.contains("toolu_edit001"),
        "Second request should reference the tool_use_id. Got: {body}"
    );
    // The ok:true appears inside a JSON string, so it's escaped as \"ok\":true
    assert!(
        body.contains(r#"\"ok\":true"#),
        "Tool result should indicate success. Got: {body}"
    );
    assert!(
        body.contains(r#"\"replacements\":1"#),
        "Tool result should show 1 replacement. Got: {body}"
    );
}

#[tokio::test]
async fn test_bash_tool_shows_debug_lines() {
    if !can_bind_localhost() {
        eprintln!("Skipping: cannot bind localhost TCP port in this environment.");
        return;
    }
    let zdx_home = temp_zdx_home();
    let mock_server = MockServer::start().await;
    let first_response = tool_use_sse("toolu_bash", "bash", r#"{"command": "echo hello"}"#);
    let second_response = text_sse("Command executed.");

    let call_count = Arc::new(AtomicUsize::new(0));
    let call_count_clone = Arc::clone(&call_count);

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

    cargo_bin_cmd!("zdx")
        .env("ZDX_HOME", zdx_home.path())
        .env("ANTHROPIC_API_KEY", "test-api-key")
        .env("ANTHROPIC_BASE_URL", mock_server.uri())
        .args(["--no-thread", "exec", "-p", "Run bash"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "\"type\":\"tool_input_completed\",\"id\":\"toolu_bash\",\"name\":\"bash\",\"input\":{\"command\":\"echo hello\"}}",
        ))
        .stdout(predicate::str::contains(
            "\"type\":\"tool_completed\",\"id\":\"toolu_bash\"",
        ))
        .stdout(predicate::str::contains("\"stdout\":\"hello\\n\""));
}

/// Builds an Anthropic SSE body whose only reasoning block is a
/// `redacted_thinking` content block carrying `blob` as its opaque
/// `data` payload, followed by a one-token text block. Inlined (rather
/// than imported from the provider crate) so the new fixture stays local
/// to this test without leaking a private provider constant.
fn redacted_thinking_first_turn_sse(blob: &str) -> String {
    let template = r#"event: message_start
data: {"type":"message_start","message":{"id":"msg_redacted_roundtrip","type":"message","role":"assistant","content":[],"model":"claude-haiku-4-5","stop_reason":null,"stop_sequence":null,"usage":{"input_tokens":10,"output_tokens":1}}}

event: content_block_start
data: {"type":"content_block_start","index":0,"content_block":{"type":"redacted_thinking","data":"__BLOB__"}}

event: content_block_stop
data: {"type":"content_block_stop","index":0}

event: content_block_start
data: {"type":"content_block_start","index":1,"content_block":{"type":"text","text":""}}

event: content_block_delta
data: {"type":"content_block_delta","index":1,"delta":{"type":"text_delta","text":"ok"}}

event: content_block_stop
data: {"type":"content_block_stop","index":1}

event: message_delta
data: {"type":"message_delta","delta":{"stop_reason":"end_turn","stop_sequence":null},"usage":{"output_tokens":5}}

event: message_stop
data: {"type":"message_stop"}

"#;
    template.replace("__BLOB__", blob)
}

/// Runs a single `zdx exec --thread <id>` invocation against a mock
/// Anthropic endpoint using the same env-var redirection pattern as the
/// rest of this file.
fn run_exec_turn(
    zdx_home: &TempDir,
    temp_dir: &TempDir,
    mock_uri: &str,
    thread_id: &str,
    prompt: &str,
) {
    cargo_bin_cmd!("zdx")
        .env("ZDX_HOME", zdx_home.path())
        .env("ANTHROPIC_API_KEY", "test-api-key")
        .env("ANTHROPIC_BASE_URL", mock_uri)
        .args([
            "--root",
            temp_dir.path().to_str().unwrap(),
            "--thread",
            thread_id,
            "exec",
            "-p",
            prompt,
        ])
        .assert()
        .success();
}

/// Extracts the `data` payload of the first assistant-authored
/// `redacted_thinking` content block in a serialized Anthropic
/// `/v1/messages` request body, or panics with the full body on miss.
fn extract_assistant_redacted_thinking_data(body: &str) -> String {
    let parsed: serde_json::Value = serde_json::from_str(body)
        .unwrap_or_else(|e| panic!("request body should be valid JSON: {e}; body={body}"));
    let messages = parsed
        .get("messages")
        .and_then(serde_json::Value::as_array)
        .unwrap_or_else(|| panic!("request body should have a `messages` array; body={body}"));
    messages
        .iter()
        .filter(|m| m.get("role").and_then(serde_json::Value::as_str) == Some("assistant"))
        .flat_map(|m| {
            m.get("content")
                .and_then(serde_json::Value::as_array)
                .cloned()
                .unwrap_or_default()
        })
        .find(|block| {
            block.get("type").and_then(serde_json::Value::as_str) == Some("redacted_thinking")
        })
        .and_then(|block| {
            block
                .get("data")
                .and_then(serde_json::Value::as_str)
                .map(std::string::ToString::to_string)
        })
        .unwrap_or_else(|| {
            panic!(
                "request messages should include an assistant `redacted_thinking` block; body={body}"
            )
        })
}

/// End-to-end contract: an Anthropic `redacted_thinking` block's opaque
/// `data` blob must survive a full persistence round-trip. Turn 1 receives
/// a `redacted_thinking` content block; turn 2 reloads the thread from
/// its JSONL file on disk and MUST re-send the exact same encrypted bytes
/// back to Anthropic inside a `{"type":"redacted_thinking","data":"..."}`
/// block in the outbound `/v1/messages` request body.
///
/// This is the composed contract that the per-layer unit tests cannot
/// jointly prove: SSE parse -> engine turn builder -> `ReasoningCompleted`
/// event -> `spawn_thread_persist_task` -> JSONL line ->
/// `load_thread_as_messages` -> `MessageReplay` -> `ChatMessage` ->
/// `ApiMessage::from_chat_message` -> outbound JSON.
#[tokio::test]
async fn redacted_thinking_data_round_trips_through_thread_persistence() {
    if !can_bind_localhost() {
        eprintln!("Skipping: cannot bind localhost TCP port in this environment.");
        return;
    }
    let zdx_home = temp_zdx_home();
    let temp_dir = TempDir::new().unwrap();
    let thread_id = "redacted-roundtrip";
    let blob = "enc_blob_roundtrip_xyz==";

    let mock_server = MockServer::start().await;
    let call_count = Arc::new(AtomicUsize::new(0));
    let call_count_clone = Arc::clone(&call_count);
    let second_request_body = Arc::new(std::sync::Mutex::new(String::new()));
    let second_request_body_clone = Arc::clone(&second_request_body);

    let first_sse = redacted_thinking_first_turn_sse(blob);
    let second_sse = fixtures::text_sse("done");

    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .and(header("x-api-key", "test-api-key"))
        .respond_with(move |req: &Request| {
            let count = call_count_clone.fetch_add(1, Ordering::SeqCst);
            if count == 0 {
                fixtures::sse_response(&first_sse)
            } else {
                let body = String::from_utf8_lossy(&req.body).to_string();
                *second_request_body_clone.lock().unwrap() = body;
                fixtures::sse_response(&second_sse)
            }
        })
        .expect(2)
        .mount(&mock_server)
        .await;

    // Turn 1: creates the thread and persists the redacted block to JSONL.
    run_exec_turn(
        &zdx_home,
        &temp_dir,
        &mock_server.uri(),
        thread_id,
        "first prompt with redacted reasoning",
    );

    // Belt-and-suspenders: the JSONL on disk should mention the exact blob.
    // The primary assertion is the structural JSON check on turn 2 below.
    let thread_path = zdx_home
        .path()
        .join("threads")
        .join(format!("{thread_id}.jsonl"));
    let jsonl = fs::read_to_string(&thread_path)
        .expect("thread JSONL should exist after the first exec run");
    assert!(
        jsonl.contains(blob),
        "thread JSONL at {} should persist the redacted_thinking data blob; got:\n{jsonl}",
        thread_path.display()
    );

    // Turn 2: reloads the thread from JSONL via `load_thread_as_messages`
    // and issues a new outbound request whose body the mock captures above.
    run_exec_turn(
        &zdx_home,
        &temp_dir,
        &mock_server.uri(),
        thread_id,
        "second prompt after reload",
    );

    assert_eq!(
        call_count.load(Ordering::SeqCst),
        2,
        "both exec invocations should have hit the mock Anthropic endpoint",
    );

    let body = second_request_body.lock().unwrap().clone();
    assert!(
        !body.is_empty(),
        "second outbound request body was not captured",
    );

    // Primary structural assertion: after a full JSONL round-trip, the
    // second outbound request's `messages` must contain an assistant
    // message whose `content` has a `redacted_thinking` block with the
    // EXACT opaque bytes from turn 1, unmodified.
    let replayed_data = extract_assistant_redacted_thinking_data(&body);
    assert_eq!(
        replayed_data, blob,
        "redacted_thinking `data` must round-trip byte-for-byte; body={body}",
    );

    // Secondary substring check: the raw serialized body must also contain
    // the blob, guarding against any future serde tag drift on
    // `ReplayToken::AnthropicRedacted` that might accidentally hide the
    // payload behind a different JSON key.
    assert!(
        body.contains(blob),
        "second request raw body should contain the opaque blob verbatim; body={body}",
    );
}
