//! Tests for the tool use loop with wiremock.
//!
//! Simulates a two-step interaction:
//! 1. First response asks for `tool_use(read)`
//! 2. Second response returns final text
//!
//! Verifies that the second request includes `tool_result` block.

use std::fs;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use predicates::prelude::*;
use tempfile::TempDir;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, Request};

use crate::fixtures;
use crate::fixtures::{MOCK_MODEL, sse_response, text_and_tool_use_sse, text_sse, tool_use_sse};

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

    crate::fixtures::zdx_cmd()
        .env("ZDX_HOME", zdx_home.path())
        .env("ANTHROPIC_API_KEY", "test-api-key")
        .env("ANTHROPIC_BASE_URL", mock_server.uri())
        .args([
            "--root",
            temp_dir.path().to_str().unwrap(),
            "--no-thread",
            "exec",
            "-m",
            MOCK_MODEL,
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

    crate::fixtures::zdx_cmd()
        .env("ZDX_HOME", zdx_home.path())
        .env("ANTHROPIC_API_KEY", "test-api-key")
        .env("ANTHROPIC_BASE_URL", mock_server.uri())
        .args([
            "--root",
            temp_dir.path().to_str().unwrap(),
            "--no-thread",
            "exec",
            "-m",
            MOCK_MODEL,
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

    crate::fixtures::zdx_cmd()
        .env("ZDX_HOME", zdx_home.path())
        .env("ANTHROPIC_API_KEY", "test-api-key")
        .env("ANTHROPIC_BASE_URL", mock_server.uri())
        .args([
            "--root",
            root_dir.path().to_str().unwrap(),
            "--no-thread",
            "exec",
            "-m",
            MOCK_MODEL,
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

    crate::fixtures::zdx_cmd()
        .env("ZDX_HOME", zdx_home.path())
        .env("ANTHROPIC_API_KEY", "test-api-key")
        .env("ANTHROPIC_BASE_URL", mock_server.uri())
        .args([
            "--no-thread",
            "exec",
            "-m",
            MOCK_MODEL,
            "-p",
            "Show indicator",
        ])
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

    crate::fixtures::zdx_cmd()
        .env("ZDX_HOME", zdx_home.path())
        .env("ANTHROPIC_API_KEY", "test-api-key")
        .env("ANTHROPIC_BASE_URL", mock_server.uri())
        .args(["--no-thread", "exec", "-m", MOCK_MODEL, "-p", "Say hello"])
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

    crate::fixtures::zdx_cmd()
        .env("ZDX_HOME", zdx_home.path())
        .env("ANTHROPIC_API_KEY", "test-api-key")
        .env("ANTHROPIC_BASE_URL", mock_server.uri())
        .args(["--no-thread", "exec", "-m", MOCK_MODEL, "-p", "Say hello"])
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

    crate::fixtures::zdx_cmd()
        .env("ZDX_HOME", zdx_home.path())
        .env("ANTHROPIC_API_KEY", "test-api-key")
        .env("ANTHROPIC_BASE_URL", mock_server.uri())
        .args(["--no-thread", "exec", "-m", MOCK_MODEL, "-p", "Say hello"])
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

    crate::fixtures::zdx_cmd()
        .env("ZDX_HOME", zdx_home.path())
        .env("ANTHROPIC_API_KEY", "test-api-key")
        .env("ANTHROPIC_BASE_URL", mock_server.uri())
        .args([
            "--no-thread",
            "exec",
            "-m",
            MOCK_MODEL,
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

    crate::fixtures::zdx_cmd()
        .env("ZDX_HOME", zdx_home.path())
        .env("ANTHROPIC_API_KEY", "test-api-key")
        .env("ANTHROPIC_BASE_URL", mock_server.uri())
        .args([
            "--root",
            temp_dir.path().to_str().unwrap(),
            "--no-thread",
            "exec",
            "-m",
            MOCK_MODEL,
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

    crate::fixtures::zdx_cmd()
        .env("ZDX_HOME", zdx_home.path())
        .env("ANTHROPIC_API_KEY", "test-api-key")
        .env("ANTHROPIC_BASE_URL", mock_server.uri())
        .args([
            "--root",
            temp_dir.path().to_str().unwrap(),
            "--no-thread",
            "exec",
            "-m",
            MOCK_MODEL,
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

    crate::fixtures::zdx_cmd()
        .env("ZDX_HOME", zdx_home.path())
        .env("ANTHROPIC_API_KEY", "test-api-key")
        .env("ANTHROPIC_BASE_URL", mock_server.uri())
        .args(["--no-thread", "exec",
            "-m",
            MOCK_MODEL, "-p", "Run bash"])
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
    crate::fixtures::zdx_cmd()
        .env("ZDX_HOME", zdx_home.path())
        .env("ANTHROPIC_API_KEY", "test-api-key")
        .env("ANTHROPIC_BASE_URL", mock_uri)
        .args([
            "--root",
            temp_dir.path().to_str().unwrap(),
            "--thread",
            thread_id,
            "exec",
            "-m",
            MOCK_MODEL,
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

/// Regression: a completed `zdx exec --thread` turn must persist the
/// assistant's answer exactly once.
///
/// The turn's own persistence task already writes the assistant message when
/// it handles `TurnFinished`. `run_exec` used to append the same text a second
/// time tagged `phase: "final_answer"`, so every exec-created thread stored
/// the answer twice while interactive threads stored it once.
///
/// The duplicate was not cosmetic. Replay accumulates adjacent assistant
/// events into a single message, so resuming an affected thread re-sent two
/// identical content blocks to the provider, and every consumer that walks
/// message events — monitor, markdown export, thread and memory indexing —
/// counted the answer twice.
#[tokio::test]
async fn exec_persists_the_assistant_answer_once() {
    if !can_bind_localhost() {
        eprintln!("Skipping: cannot bind localhost TCP port in this environment.");
        return;
    }
    let zdx_home = temp_zdx_home();
    let temp_dir = TempDir::new().unwrap();
    let thread_id = "assistant-message-cardinality";
    let answer = "PONG";

    let mock_server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .and(header("x-api-key", "test-api-key"))
        .respond_with(sse_response(&text_sse(answer)))
        .expect(1)
        .mount(&mock_server)
        .await;

    crate::fixtures::zdx_cmd()
        .env("ZDX_HOME", zdx_home.path())
        .env("ANTHROPIC_API_KEY", "test-api-key")
        .env("ANTHROPIC_BASE_URL", mock_server.uri())
        .args([
            "--root",
            temp_dir.path().to_str().unwrap(),
            "--thread",
            thread_id,
            "exec",
            "-m",
            MOCK_MODEL,
            "-p",
            "say pong",
        ])
        .assert()
        .success();

    let thread_path = zdx_home
        .path()
        .join("threads")
        .join(format!("{thread_id}.jsonl"));
    let jsonl =
        fs::read_to_string(&thread_path).expect("thread JSONL should exist after the exec run");

    let assistant_messages: Vec<serde_json::Value> = jsonl
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .filter(|event| {
            event.get("type").and_then(serde_json::Value::as_str) == Some("message")
                && event.get("role").and_then(serde_json::Value::as_str) == Some("assistant")
        })
        .collect();

    assert_eq!(
        assistant_messages.len(),
        1,
        "exec should persist the assistant answer exactly once; got:\n{jsonl}"
    );
    assert_eq!(
        assistant_messages[0]
            .get("text")
            .and_then(serde_json::Value::as_str),
        Some(answer),
        "the persisted assistant message should carry the answer text; got:\n{jsonl}"
    );
}

/// SSE fixture: one assistant turn that thinks visibly and then answers.
fn thinking_first_turn_sse(reasoning: &str, answer: &str) -> String {
    let template = r#"event: message_start
data: {"type":"message_start","message":{"id":"msg_think","type":"message","role":"assistant","content":[],"model":"claude-sonnet-5","stop_reason":null,"stop_sequence":null,"usage":{"input_tokens":10,"output_tokens":1}}}

event: content_block_start
data: {"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":""}}

event: content_block_delta
data: {"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"__REASONING__"}}

event: content_block_delta
data: {"type":"content_block_delta","index":0,"delta":{"type":"signature_delta","signature":"sig_roundtrip"}}

event: content_block_stop
data: {"type":"content_block_stop","index":0}

event: content_block_start
data: {"type":"content_block_start","index":1,"content_block":{"type":"text","text":""}}

event: content_block_delta
data: {"type":"content_block_delta","index":1,"delta":{"type":"text_delta","text":"__ANSWER__"}}

event: content_block_stop
data: {"type":"content_block_stop","index":1}

event: message_delta
data: {"type":"message_delta","delta":{"stop_reason":"end_turn","stop_sequence":null},"usage":{"output_tokens":5}}

event: message_stop
data: {"type":"message_stop"}

"#;
    template
        .replace("__REASONING__", reasoning)
        .replace("__ANSWER__", answer)
}

/// Runs one `zdx exec --thread <id>` against the mock endpoint with an
/// explicit model spec (so thinking can be enabled).
fn run_exec_turn_with_model(
    zdx_home: &TempDir,
    temp_dir: &TempDir,
    mock_uri: &str,
    thread_id: &str,
    model: &str,
    prompt: &str,
) {
    crate::fixtures::zdx_cmd()
        .env("ZDX_HOME", zdx_home.path())
        .env("ANTHROPIC_API_KEY", "test-api-key")
        .env("ANTHROPIC_BASE_URL", mock_uri)
        .args([
            "--root",
            temp_dir.path().to_str().unwrap(),
            "--thread",
            thread_id,
            "exec",
            "-m",
            model,
            "-p",
            prompt,
        ])
        .assert()
        .success();
}

/// A previous turn's thinking must not be replayed on the next request.
///
/// Replaying it is dead weight: the API does not need prior-turn thinking,
/// and a proxy backend that does not filter it server-side counts every
/// earlier turn's reasoning as input tokens, which is what drove a real
/// thread past its context limit after a degenerate reasoning turn.
#[tokio::test]
async fn prior_turn_thinking_is_not_replayed() {
    if !can_bind_localhost() {
        eprintln!("Skipping: cannot bind localhost TCP port in this environment.");
        return;
    }
    let zdx_home = temp_zdx_home();
    let temp_dir = TempDir::new().unwrap();
    let thread_id = "thinking-replay";
    let reasoning = "Prior-turn reasoning that must not come back.";
    let answer = "first answer";

    let mock_server = MockServer::start().await;
    let call_count = Arc::new(AtomicUsize::new(0));
    let call_count_clone = Arc::clone(&call_count);
    let second_request_body = Arc::new(std::sync::Mutex::new(String::new()));
    let second_request_body_clone = Arc::clone(&second_request_body);

    let first_sse = thinking_first_turn_sse(reasoning, answer);
    let second_sse = text_sse("second answer");

    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .and(header("x-api-key", "test-api-key"))
        .respond_with(move |req: &Request| {
            let count = call_count_clone.fetch_add(1, Ordering::SeqCst);
            if count == 0 {
                fixtures::sse_response(&first_sse)
            } else {
                *second_request_body_clone.lock().unwrap() =
                    String::from_utf8_lossy(&req.body).to_string();
                fixtures::sse_response(&second_sse)
            }
        })
        .expect(2)
        .mount(&mock_server)
        .await;

    run_exec_turn_with_model(
        &zdx_home,
        &temp_dir,
        &mock_server.uri(),
        thread_id,
        "anthropic:claude-sonnet-5@high",
        "first prompt",
    );
    run_exec_turn_with_model(
        &zdx_home,
        &temp_dir,
        &mock_server.uri(),
        thread_id,
        "anthropic:claude-sonnet-5@high",
        "second prompt",
    );

    let body = second_request_body.lock().unwrap().clone();
    assert!(
        !body.contains(reasoning),
        "prior-turn thinking must not be replayed; body={body}"
    );
    assert!(
        body.contains(answer),
        "the prior assistant text must still be replayed; body={body}"
    );
}

/// SSE fixture: one assistant turn that repeats the same filler lines.
fn repeating_text_sse() -> String {
    let body = "Let me run.\n\nLet me go.\n\nLet me do it.\n\nLet me execute.\n\n".repeat(200);
    let mut sse = String::from(
        "event: message_start\n\
         data: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_loop\",\"type\":\"message\",\"role\":\"assistant\",\"content\":[],\"model\":\"claude-sonnet-5\",\"stop_reason\":null,\"stop_sequence\":null,\"usage\":{\"input_tokens\":10,\"output_tokens\":1}}}\n\n\
         event: content_block_start\n\
         data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n",
    );
    for chunk in body.as_bytes().chunks(400) {
        let text = String::from_utf8_lossy(chunk);
        sse.push_str(&format!(
            "event: content_block_delta\ndata: {{\"type\":\"content_block_delta\",\"index\":0,\"delta\":{{\"type\":\"text_delta\",\"text\":{}}}}}\n\n",
            serde_json::Value::String(text.to_string())
        ));
    }
    sse.push_str(
        "event: content_block_stop\n\
         data: {\"type\":\"content_block_stop\",\"index\":0}\n\n\
         event: message_delta\n\
         data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\",\"stop_sequence\":null},\"usage\":{\"output_tokens\":5}}\n\n\
         event: message_stop\n\
         data: {\"type\":\"message_stop\"}\n\n",
    );
    sse
}

/// A model that loops in its answer is stopped, reported, and retried once at
/// a lower thinking level — not failed.
#[tokio::test]
async fn repeated_answer_text_is_stopped_and_retried_at_a_lower_thinking_level() {
    if !can_bind_localhost() {
        eprintln!("Skipping: cannot bind localhost TCP port in this environment.");
        return;
    }
    let zdx_home = temp_zdx_home();
    let temp_dir = TempDir::new().unwrap();
    let thread_id = "text-loop-retry";

    let mock_server = MockServer::start().await;
    let call_count = Arc::new(AtomicUsize::new(0));
    let call_count_clone = Arc::clone(&call_count);
    let bodies = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let bodies_clone = Arc::clone(&bodies);

    let looping_sse = repeating_text_sse();
    let final_sse = text_sse("Recovered answer.");

    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .and(header("x-api-key", "test-api-key"))
        .respond_with(move |req: &Request| {
            let count = call_count_clone.fetch_add(1, Ordering::SeqCst);
            bodies_clone
                .lock()
                .unwrap()
                .push(String::from_utf8_lossy(&req.body).to_string());
            if count == 0 {
                fixtures::sse_response(&looping_sse)
            } else {
                fixtures::sse_response(&final_sse)
            }
        })
        .expect(2)
        .mount(&mock_server)
        .await;

    // `@high` leaves room to retry one level down.
    run_exec_turn_with_model(
        &zdx_home,
        &temp_dir,
        &mock_server.uri(),
        thread_id,
        "anthropic:claude-sonnet-5@high",
        "write the thing",
    );

    let captured = bodies.lock().unwrap().clone();
    assert_eq!(
        captured.len(),
        2,
        "the looping attempt must be retried once"
    );
    assert!(
        captured[0].contains(r#""effort":"high""#),
        "first attempt should use the configured level; body={}",
        captured[0]
    );
    assert!(
        captured[1].contains(r#""effort":"medium""#),
        "the retry should step one thinking level down; body={}",
        captured[1]
    );

    // The turn completes (exit status is asserted by the runner) and the
    // loop is reported in the thread so the user can see what happened.
    let jsonl = fs::read_to_string(
        zdx_home
            .path()
            .join("threads")
            .join(format!("{thread_id}.jsonl")),
    )
    .expect("thread JSONL should exist");
    assert!(
        jsonl.contains(r#""kind":"output_loop""#)
            && jsonl.contains("repeating itself in the answer"),
        "the loop must be persisted as a notice; got:\n{jsonl}"
    );
    assert!(
        jsonl.contains("Recovered answer."),
        "the retry's answer should be what persists; got:\n{jsonl}"
    );
}

/// A model that keeps re-issuing the same tool call against an unchanged
/// result is stopped, reported, and retried once at a lower thinking level.
#[tokio::test]
async fn repeated_identical_tool_call_is_stopped_and_retried() {
    if !can_bind_localhost() {
        eprintln!("Skipping: cannot bind localhost TCP port in this environment.");
        return;
    }
    let zdx_home = temp_zdx_home();
    let temp_dir = TempDir::new().unwrap();
    let thread_id = "tool-loop-retry";
    fs::write(temp_dir.path().join("test.txt"), "Stable content.").unwrap();

    let mock_server = MockServer::start().await;
    let call_count = Arc::new(AtomicUsize::new(0));
    let call_count_clone = Arc::clone(&call_count);
    let bodies = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let bodies_clone = Arc::clone(&bodies);

    // Every attempt asks for the same read of the same unchanged file, so the
    // tool call and its result are identical each round.
    let looping_sse = tool_use_sse("toolu_same", "read", r#"{"file_path": "test.txt"}"#);
    let final_sse = text_sse("Recovered answer.");

    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .and(header("x-api-key", "test-api-key"))
        .respond_with(move |req: &Request| {
            let count = call_count_clone.fetch_add(1, Ordering::SeqCst);
            bodies_clone
                .lock()
                .unwrap()
                .push(String::from_utf8_lossy(&req.body).to_string());
            if count < 3 {
                fixtures::sse_response(&looping_sse)
            } else {
                fixtures::sse_response(&final_sse)
            }
        })
        .expect(4)
        .mount(&mock_server)
        .await;

    run_exec_turn_with_model(
        &zdx_home,
        &temp_dir,
        &mock_server.uri(),
        thread_id,
        "anthropic:claude-sonnet-5@high",
        "read it",
    );

    let captured = bodies.lock().unwrap().clone();
    assert_eq!(
        captured.len(),
        4,
        "three identical tool turns then one retry"
    );
    assert!(
        captured[3].contains(r#""effort":"medium""#),
        "the retry should step one thinking level down; body={}",
        captured[3]
    );

    let jsonl = fs::read_to_string(
        zdx_home
            .path()
            .join("threads")
            .join(format!("{thread_id}.jsonl")),
    )
    .expect("thread JSONL should exist");
    assert!(
        jsonl.contains(r#""kind":"output_loop""#) && jsonl.contains("same tool call"),
        "the tool loop must be persisted as a notice; got:\n{jsonl}"
    );
}

/// With no lower thinking level to retry to, a loop closes the turn normally
/// with a notice instead of failing it.
#[tokio::test]
async fn tool_loop_without_a_lower_level_closes_the_turn_with_a_notice() {
    if !can_bind_localhost() {
        eprintln!("Skipping: cannot bind localhost TCP port in this environment.");
        return;
    }
    let zdx_home = temp_zdx_home();
    let temp_dir = TempDir::new().unwrap();
    let thread_id = "tool-loop-no-retry";
    fs::write(temp_dir.path().join("test.txt"), "Stable content.").unwrap();

    let mock_server = MockServer::start().await;
    let call_count = Arc::new(AtomicUsize::new(0));
    let call_count_clone = Arc::clone(&call_count);
    let looping_sse = tool_use_sse("toolu_same", "read", r#"{"file_path": "test.txt"}"#);

    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .and(header("x-api-key", "test-api-key"))
        .respond_with(move |_req: &Request| {
            call_count_clone.fetch_add(1, Ordering::SeqCst);
            fixtures::sse_response(&looping_sse)
        })
        .mount(&mock_server)
        .await;

    // `@low` is the bottom of the ladder: the guard has nothing to lower to.
    run_exec_turn_with_model(
        &zdx_home,
        &temp_dir,
        &mock_server.uri(),
        thread_id,
        "anthropic:claude-sonnet-5@low",
        "read it",
    );

    assert_eq!(
        call_count.load(Ordering::SeqCst),
        3,
        "the turn should stop after the third identical tool call"
    );

    let jsonl = fs::read_to_string(
        zdx_home
            .path()
            .join("threads")
            .join(format!("{thread_id}.jsonl")),
    )
    .expect("thread JSONL should exist");
    assert!(
        jsonl.contains(r#""kind":"output_loop""#) && jsonl.contains("same tool call"),
        "the loop must be persisted as a notice; got:\n{jsonl}"
    );
}

/// Real runaway content captured from live threads, trimmed to the prefix up to
/// just past the point where the guard fires.
///
/// Each fixture comes from an observed turn where the model ran to its full
/// output-token limit producing nothing but filler: three reasoning loops and
/// one answer-text loop. The degenerate filler tail ("Let me run." / "Let me
/// go." / "Emit." ...) is the real model output, byte for byte. Only the varied
/// preamble ahead of it is rewritten: each line is replaced by a synthetic
/// ASCII line of the *same byte length*, so the fixture reproduces the real trip
/// dynamics (an identical trip offset) without carrying the surrounding
/// project content. Each cut ends shortly past the guard's trip point, since
/// nothing beyond it is ever read.
struct LoopFixture {
    /// Fixture file name under `fixtures/loop`.
    file: &'static str,
    /// Streamed as a reasoning block, or as visible answer text.
    as_reasoning: bool,
    /// Byte offset at which the guard fires on the real record, and therefore
    /// on this fixture.
    trip_bytes: usize,
    /// Distinguishes which guard fired, via the notice message.
    notice_fragment: &'static str,
}

const LOOP_FIXTURES: &[LoopFixture] = &[
    LoopFixture {
        file: "reasoning_loop_a",
        as_reasoning: true,
        trip_bytes: 7_475,
        notice_fragment: "repeating itself in reasoning",
    },
    LoopFixture {
        file: "reasoning_loop_b",
        as_reasoning: true,
        trip_bytes: 7_288,
        notice_fragment: "repeating itself in reasoning",
    },
    LoopFixture {
        file: "reasoning_loop_c",
        as_reasoning: true,
        trip_bytes: 7_914,
        notice_fragment: "repeating itself in reasoning",
    },
    LoopFixture {
        file: "answer_loop_d",
        as_reasoning: false,
        trip_bytes: 7_694,
        notice_fragment: "repeating itself in the answer",
    },
];

/// Delta size the fixture is streamed in. The guard is evaluated per delta, so
/// the trip lands on the first chunk boundary at or after the real offset.
const LOOP_FIXTURE_CHUNK: usize = 256;

/// Chunks a fixture on character boundaries, so a multi-byte character is never
/// split across two deltas.
fn chunk_on_char_boundaries(text: &str, chunk: usize) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    for ch in text.chars() {
        current.push(ch);
        if current.len() >= chunk {
            out.push(std::mem::take(&mut current));
        }
    }
    if !current.is_empty() {
        out.push(current);
    }
    out
}

/// SSE that streams `content` as one reasoning or text block, in deltas.
fn looping_content_sse(content: &str, as_reasoning: bool) -> String {
    let (start, delta_type, field) = if as_reasoning {
        ("thinking", "thinking_delta", "thinking")
    } else {
        ("text", "text_delta", "text")
    };
    let mut sse = format!(
        "event: message_start\n\
         data: {{\"type\":\"message_start\",\"message\":{{\"id\":\"msg_loop\",\"type\":\"message\",\"role\":\"assistant\",\"content\":[],\"model\":\"claude-sonnet-5\",\"stop_reason\":null,\"stop_sequence\":null,\"usage\":{{\"input_tokens\":10,\"output_tokens\":1}}}}}}\n\n\
         event: content_block_start\n\
         data: {{\"type\":\"content_block_start\",\"index\":0,\"content_block\":{{\"type\":\"{start}\"}}}}\n\n"
    );
    for piece in chunk_on_char_boundaries(content, LOOP_FIXTURE_CHUNK) {
        sse.push_str(&format!(
            "event: content_block_delta\ndata: {{\"type\":\"content_block_delta\",\"index\":0,\"delta\":{{\"type\":\"{delta_type}\",\"{field}\":{}}}}}\n\n",
            serde_json::Value::String(piece)
        ));
    }
    sse.push_str(
        "event: content_block_stop\n\
         data: {\"type\":\"content_block_stop\",\"index\":0}\n\n\
         event: message_delta\n\
         data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\",\"stop_sequence\":null},\"usage\":{\"output_tokens\":5}}\n\n\
         event: message_stop\n\
         data: {\"type\":\"message_stop\"}\n\n",
    );
    sse
}

fn loop_fixture(fixture: &LoopFixture) -> String {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/integration/fixtures/loop")
        .join(format!("{}.txt", fixture.file));
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("read fixture {}: {e}", path.display()))
}

/// Recovers how much content had streamed when the guard fired, from the
/// truncation marker the engine leaves in the persisted block.
fn truncated_byte_count(thread_jsonl: &str) -> Option<usize> {
    for line in thread_jsonl.lines() {
        let Ok(event) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        if event.get("type").and_then(|v| v.as_str()) != Some("reasoning")
            && event.get("role").and_then(|v| v.as_str()) != Some("assistant")
        {
            continue;
        }
        let Some(text) = event.get("text").and_then(|v| v.as_str()) else {
            continue;
        };
        let Some(rest) = text.split("... (").nth(1) else {
            continue;
        };
        let Some(count) = rest.split(" bytes truncated)").next() else {
            continue;
        };
        if let Ok(n) = count.parse::<usize>() {
            // `truncate_middle` keeps a 2k head and a 2k tail, so the marker
            // reports `total - 4000`.
            return Some(n + 4_000);
        }
    }
    None
}

/// Each real runaway is stopped by the guard within its first ~8 KB — a few
/// seconds of generation — rather than running to the 160k-byte budget (or the
/// model's full 384k-token output limit). Run at `@low`, the bottom of the
/// thinking ladder, so no retry is available and the truncated block is what
/// persists.
#[tokio::test]
async fn real_runaway_content_trips_the_guard_early() {
    if !can_bind_localhost() {
        eprintln!("Skipping: cannot bind localhost TCP port in this environment.");
        return;
    }

    for fixture in LOOP_FIXTURES {
        let zdx_home = temp_zdx_home();
        let temp_dir = TempDir::new().unwrap();
        let thread_id = format!("loop-{}", fixture.file);
        let content = loop_fixture(fixture);
        let looping_sse = looping_content_sse(&content, fixture.as_reasoning);

        let mock_server = MockServer::start().await;
        let final_sse = text_sse("Recovered.");
        Mock::given(method("POST"))
            .and(path("/v1/messages"))
            .and(header("x-api-key", "test-api-key"))
            .respond_with(move |_req: &Request| fixtures::sse_response(&looping_sse))
            .mount(&mock_server)
            .await;
        let _ = final_sse;

        run_exec_turn_with_model(
            &zdx_home,
            &temp_dir,
            &mock_server.uri(),
            &thread_id,
            "anthropic:claude-sonnet-5@low",
            "keep going",
        );

        let jsonl = fs::read_to_string(
            zdx_home
                .path()
                .join("threads")
                .join(format!("{thread_id}.jsonl")),
        )
        .unwrap_or_else(|e| panic!("thread JSONL for {}: {e}", fixture.file));

        assert!(
            jsonl.contains(r#""kind":"output_loop""#) && jsonl.contains(fixture.notice_fragment),
            "{} should close with an output_loop notice; got:\n{jsonl}",
            fixture.file
        );

        let streamed = truncated_byte_count(&jsonl).unwrap_or_else(|| {
            panic!(
                "{} should persist a truncated block; got:\n{jsonl}",
                fixture.file
            )
        });

        // The guard fires on the first chunk boundary at or after the real
        // offset, and never on the 160k-byte budget.
        assert!(
            streamed >= fixture.trip_bytes && streamed < fixture.trip_bytes + LOOP_FIXTURE_CHUNK,
            "{} tripped at {streamed} bytes, expected [{}, {})",
            fixture.file,
            fixture.trip_bytes,
            fixture.trip_bytes + LOOP_FIXTURE_CHUNK
        );
        assert!(
            streamed < 160_000,
            "{} must trip on the repetition pattern, not the budget",
            fixture.file
        );
    }
}

/// The same real content, with a retry available, is retried once one thinking
/// level down — reasoning loop and answer loop both.
#[tokio::test]
async fn real_runaway_content_is_retried_at_a_lower_thinking_level() {
    if !can_bind_localhost() {
        eprintln!("Skipping: cannot bind localhost TCP port in this environment.");
        return;
    }

    for fixture in [&LOOP_FIXTURES[0], &LOOP_FIXTURES[3]] {
        let zdx_home = temp_zdx_home();
        let temp_dir = TempDir::new().unwrap();
        let thread_id = format!("loop-retry-{}", fixture.file);
        let content = loop_fixture(fixture);
        let looping_sse = looping_content_sse(&content, fixture.as_reasoning);
        let final_sse = text_sse("Recovered.");

        let mock_server = MockServer::start().await;
        let call_count = Arc::new(AtomicUsize::new(0));
        let call_count_clone = Arc::clone(&call_count);
        let bodies = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let bodies_clone = Arc::clone(&bodies);

        Mock::given(method("POST"))
            .and(path("/v1/messages"))
            .and(header("x-api-key", "test-api-key"))
            .respond_with(move |req: &Request| {
                let n = call_count_clone.fetch_add(1, Ordering::SeqCst);
                bodies_clone
                    .lock()
                    .unwrap()
                    .push(String::from_utf8_lossy(&req.body).to_string());
                if n == 0 {
                    fixtures::sse_response(&looping_sse)
                } else {
                    fixtures::sse_response(&final_sse)
                }
            })
            .expect(2)
            .mount(&mock_server)
            .await;

        run_exec_turn_with_model(
            &zdx_home,
            &temp_dir,
            &mock_server.uri(),
            &thread_id,
            "anthropic:claude-sonnet-5@high",
            "keep going",
        );

        let captured = bodies.lock().unwrap().clone();
        assert_eq!(
            captured.len(),
            2,
            "{} should be retried exactly once",
            fixture.file
        );
        assert!(
            captured[0].contains(r#""effort":"high""#)
                && captured[1].contains(r#""effort":"medium""#),
            "{} retry should step one level down; second body={}",
            fixture.file,
            captured[1]
        );

        let jsonl = fs::read_to_string(
            zdx_home
                .path()
                .join("threads")
                .join(format!("{thread_id}.jsonl")),
        )
        .unwrap_or_else(|e| panic!("thread JSONL for {}: {e}", fixture.file));
        assert!(
            jsonl.contains(r#""kind":"output_loop""#)
                && jsonl.contains("Retrying once at thinking level")
                && jsonl.contains("Recovered."),
            "{} should announce the retry and persist its answer; got:\n{jsonl}",
            fixture.file
        );
    }
}
