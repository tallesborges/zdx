//! Tests for the tool use loop with wiremock.
//!
//! Simulates a two-step interaction:
//! 1. First response asks for tool_use(read)
//! 2. Second response returns final text
//!
//! Verifies that the second request includes tool_result block.

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

/// Creates a temp ZDX_HOME directory for test isolation.
fn temp_zdx_home() -> TempDir {
    TempDir::new().expect("create temp zdx home")
}

#[tokio::test]
async fn test_tool_use_loop_reads_file() {
    let zdx_home = temp_zdx_home();
    let temp_dir = TempDir::new().unwrap();
    let test_file = temp_dir.path().join("test.txt");
    fs::write(&test_file, "Hello from file!").unwrap();

    let mock_server = MockServer::start().await;
    let call_count = Arc::new(AtomicUsize::new(0));
    let call_count_clone = call_count.clone();

    let first_response = text_and_tool_use_sse(
        "I'll read that file for you.",
        "toolu_001",
        "read",
        r#"{"path": "test.txt"}"#,
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
            "--no-save",
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
    let temp_dir = TempDir::new().unwrap();
    let test_file = temp_dir.path().join("data.txt");
    fs::write(&test_file, "secret data").unwrap();

    let mock_server = MockServer::start().await;
    let call_count = Arc::new(AtomicUsize::new(0));
    let call_count_clone = call_count.clone();
    let second_request_body = Arc::new(std::sync::Mutex::new(String::new()));
    let second_request_body_clone = second_request_body.clone();

    let first_response = tool_use_sse("toolu_abc123", "read", r#"{"path": "data.txt"}"#);
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
        .env("ANTHROPIC_API_KEY", "test-api-key")
        .env("ANTHROPIC_BASE_URL", mock_server.uri())
        .args([
            "--root",
            temp_dir.path().to_str().unwrap(),
            "--no-save",
            "exec",
            "-p",
            "Read data.txt",
        ])
        .assert()
        .success();

    let body = second_request_body.lock().unwrap().clone();
    assert!(
        body.contains("tool_result"),
        "Second request should contain tool_result block. Got: {}",
        body
    );
    assert!(
        body.contains("toolu_abc123"),
        "Second request should reference the tool_use_id. Got: {}",
        body
    );
    assert!(
        body.contains("secret data"),
        "Second request should contain the file content. Got: {}",
        body
    );
}

#[tokio::test]
async fn test_tool_read_outside_root_allowed() {
    let root_dir = TempDir::new().unwrap();
    let outside_dir = TempDir::new().unwrap();
    let outside_file = outside_dir.path().join("outside.txt");
    fs::write(&outside_file, "outside content").unwrap();
    let outside_path = outside_file.to_str().unwrap().to_string();

    let mock_server = MockServer::start().await;
    let call_count = Arc::new(AtomicUsize::new(0));
    let call_count_clone = call_count.clone();
    let second_request_body = Arc::new(std::sync::Mutex::new(String::new()));
    let second_request_body_clone = second_request_body.clone();

    let input_json = format!(r#"{{"path": "{}"}}"#, outside_path);
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
        .env("ANTHROPIC_API_KEY", "test-api-key")
        .env("ANTHROPIC_BASE_URL", mock_server.uri())
        .args([
            "--root",
            root_dir.path().to_str().unwrap(),
            "--no-save",
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
        "Tool result should include outside file content. Got: {}",
        body
    );
    assert!(
        !body.contains("\"is_error\":true"),
        "Tool result should not be marked as error. Got: {}",
        body
    );
}

#[tokio::test]
async fn test_tool_shows_activity_indicator() {
    let mock_server = MockServer::start().await;
    let first_response = tool_use_sse("toolu_indicator", "read", r#"{"path": "nonexistent.txt"}"#);
    let second_response = text_sse("Done.");

    let call_count = Arc::new(AtomicUsize::new(0));
    let call_count_clone = call_count.clone();

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
        .env("ANTHROPIC_API_KEY", "test-api-key")
        .env("ANTHROPIC_BASE_URL", mock_server.uri())
        .args(["--no-save", "exec", "-p", "Show indicator"])
        .assert()
        .success()
        .stderr(predicate::str::contains("⚙ Running read... Done."));
}

#[tokio::test]
async fn test_tool_use_loop_writes_file() {
    let temp_dir = TempDir::new().unwrap();

    let mock_server = MockServer::start().await;
    let call_count = Arc::new(AtomicUsize::new(0));
    let call_count_clone = call_count.clone();
    let second_request_body = Arc::new(std::sync::Mutex::new(String::new()));
    let second_request_body_clone = second_request_body.clone();

    let first_response = tool_use_sse(
        "toolu_write001",
        "write",
        r#"{"path": "output.txt", "content": "Hello from write tool!"}"#,
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
        .env("ANTHROPIC_API_KEY", "test-api-key")
        .env("ANTHROPIC_BASE_URL", mock_server.uri())
        .args([
            "--root",
            temp_dir.path().to_str().unwrap(),
            "--no-save",
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
        "Second request should contain tool_result block. Got: {}",
        body
    );
    assert!(
        body.contains("toolu_write001"),
        "Second request should reference the tool_use_id. Got: {}",
        body
    );
    // The ok:true appears inside a JSON string, so it's escaped as \"ok\":true
    assert!(
        body.contains(r#"\"ok\":true"#),
        "Tool result should indicate success. Got: {}",
        body
    );
}

#[tokio::test]
async fn test_tool_use_loop_edits_file() {
    let temp_dir = TempDir::new().unwrap();
    let test_file = temp_dir.path().join("target.txt");
    fs::write(&test_file, "Hello world! This is a test.").unwrap();

    let mock_server = MockServer::start().await;
    let call_count = Arc::new(AtomicUsize::new(0));
    let call_count_clone = call_count.clone();
    let second_request_body = Arc::new(std::sync::Mutex::new(String::new()));
    let second_request_body_clone = second_request_body.clone();

    let first_response = tool_use_sse(
        "toolu_edit001",
        "edit",
        r#"{"path": "target.txt", "old": "world", "new": "Rust"}"#,
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
        .env("ANTHROPIC_API_KEY", "test-api-key")
        .env("ANTHROPIC_BASE_URL", mock_server.uri())
        .args([
            "--root",
            temp_dir.path().to_str().unwrap(),
            "--no-save",
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
        "Second request should contain tool_result block. Got: {}",
        body
    );
    assert!(
        body.contains("toolu_edit001"),
        "Second request should reference the tool_use_id. Got: {}",
        body
    );
    // The ok:true appears inside a JSON string, so it's escaped as \"ok\":true
    assert!(
        body.contains(r#"\"ok\":true"#),
        "Tool result should indicate success. Got: {}",
        body
    );
    assert!(
        body.contains(r#"\"replacements\":1"#),
        "Tool result should show 1 replacement. Got: {}",
        body
    );
}

#[tokio::test]
async fn test_bash_tool_shows_debug_lines() {
    let mock_server = MockServer::start().await;
    let first_response = tool_use_sse("toolu_bash", "bash", r#"{"command": "echo hello"}"#);
    let second_response = text_sse("Command executed.");

    let call_count = Arc::new(AtomicUsize::new(0));
    let call_count_clone = call_count.clone();

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
        .env("ANTHROPIC_API_KEY", "test-api-key")
        .env("ANTHROPIC_BASE_URL", mock_server.uri())
        .args(["--no-save", "exec", "-p", "Run bash"])
        .assert()
        .success()
        .stderr(predicate::str::contains(
            "Tool requested: bash command=\"echo hello\"",
        ))
        // Check for duration format: Done. (X.XXs)
        .stderr(predicate::str::is_match(r"Done\. \(\d+\.\d{2}s\)").unwrap())
        .stderr(predicate::str::contains("Tool finished: bash exit=0"));
}
