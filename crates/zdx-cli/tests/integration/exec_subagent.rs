//! Tests for `zdx exec --subagent <name>`.

use std::fs;
use std::sync::{Arc, Mutex};

use assert_cmd::cargo::cargo_bin_cmd;
use predicates::prelude::*;
use tempfile::TempDir;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request};

use crate::fixtures::{MOCK_MODEL, sse_response, text_sse};

fn can_bind_localhost() -> bool {
    std::net::TcpListener::bind("127.0.0.1:0").is_ok()
}

fn write_subagent(root: &std::path::Path) {
    let dir = root.join(".zdx").join("subagents");
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("tester.md"),
        "---\ndescription: Test subagent\ntools:\n  - read\n---\nYou are the tester subagent. SENTINEL-PROMPT-42.\n",
    )
    .unwrap();
}

#[tokio::test]
async fn test_exec_subagent_applies_prompt_and_tools() {
    if !can_bind_localhost() {
        eprintln!("Skipping: cannot bind localhost TCP port in this environment.");
        return;
    }
    let zdx_home = TempDir::new().unwrap();
    let root = TempDir::new().unwrap();
    write_subagent(root.path());

    let mock_server = MockServer::start().await;
    let captured: Arc<Mutex<Option<serde_json::Value>>> = Arc::new(Mutex::new(None));
    let captured_clone = Arc::clone(&captured);
    let body = text_sse("done");

    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(move |req: &Request| {
            *captured_clone.lock().unwrap() = req.body_json().ok();
            sse_response(&body)
        })
        .expect(1)
        .mount(&mock_server)
        .await;

    cargo_bin_cmd!("zdx")
        .env("ZDX_HOME", zdx_home.path())
        .env("ANTHROPIC_API_KEY", "test-api-key")
        .env("ANTHROPIC_BASE_URL", mock_server.uri())
        .args([
            "--root",
            root.path().to_str().unwrap(),
            "--no-thread",
            "exec",
            "-m",
            MOCK_MODEL,
            "--subagent",
            "tester",
            "-p",
            "hello",
        ])
        .assert()
        .success();

    let request = captured.lock().unwrap().clone().expect("captured request");

    let system = request["system"].to_string();
    assert!(
        system.contains("SENTINEL-PROMPT-42"),
        "subagent prompt body missing from system prompt: {system}"
    );

    let tools = request["tools"].as_array().expect("tools array");
    let names: Vec<String> = tools
        .iter()
        .map(|tool| tool["name"].as_str().unwrap_or_default().to_lowercase())
        .collect();
    assert_eq!(names, vec!["read".to_string()], "tools not restricted");
}

#[tokio::test]
async fn test_exec_unknown_subagent_fails() {
    let zdx_home = TempDir::new().unwrap();
    let root = TempDir::new().unwrap();

    cargo_bin_cmd!("zdx")
        .env("ZDX_HOME", zdx_home.path())
        .env("ANTHROPIC_API_KEY", "test-api-key")
        .args([
            "--root",
            root.path().to_str().unwrap(),
            "--no-thread",
            "exec",
            "-m",
            MOCK_MODEL,
            "--subagent",
            "nope",
            "-p",
            "hello",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("nope"));
}
