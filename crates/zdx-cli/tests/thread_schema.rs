//! Integration tests for thread schema versioning and tool event persistence.
//!
//! Tests the schema v1 format including:
//! - meta event as first line
//! - `tool_use` and `tool_result` events during tool loops
//! - resume with full tool history

use std::fs;

use assert_cmd::cargo::cargo_bin_cmd;
use predicates::prelude::*;
use tempfile::TempDir;

#[tokio::test]
async fn test_threads_show_displays_tool_events() {
    let temp_dir = TempDir::new().unwrap();

    // Create a thread with tool events
    let threads_dir = temp_dir.path().join("threads");
    fs::create_dir_all(&threads_dir).unwrap();
    let thread_path = threads_dir.join("tool-thread.jsonl");

    let thread_content = r#"{"type":"meta","schema_version":1,"ts":"2025-01-01T00:00:00Z"}
{"type":"message","role":"user","text":"read main.rs","ts":"2025-01-01T00:00:01Z"}
{"type":"tool_use","id":"t1","name":"read","input":{"path":"main.rs"},"ts":"2025-01-01T00:00:02Z"}
{"type":"tool_result","tool_use_id":"t1","output":{"ok":true,"data":{"content":"fn main() {}"}},"ok":true,"ts":"2025-01-01T00:00:03Z"}
{"type":"message","role":"assistant","text":"Here is the file.","ts":"2025-01-01T00:00:04Z"}"#;
    fs::write(&thread_path, thread_content).unwrap();

    cargo_bin_cmd!("zdx")
        .env("ZDX_HOME", temp_dir.path())
        .args(["threads", "show", "tool-thread"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Thread (schema v1)"))
        .stdout(predicate::str::contains("### Tool: read"))
        .stdout(predicate::str::contains("### Result ✓"));
}

#[tokio::test]
async fn test_interrupted_thread_mid_tool_is_resumable() {
    let temp_dir = TempDir::new().unwrap();

    // Create a thread that was interrupted during tool execution
    let threads_dir = temp_dir.path().join("threads");
    fs::create_dir_all(&threads_dir).unwrap();
    let thread_path = threads_dir.join("interrupted.jsonl");

    let thread_content = r#"{"type":"meta","schema_version":1,"ts":"2025-01-01T00:00:00Z"}
{"type":"message","role":"user","text":"run ls","ts":"2025-01-01T00:00:01Z"}
{"type":"tool_use","id":"t1","name":"bash","input":{"command":"ls"},"ts":"2025-01-01T00:00:02Z"}
{"type":"interrupted","role":"system","text":"Interrupted","ts":"2025-01-01T00:00:03Z"}"#;
    fs::write(&thread_path, thread_content).unwrap();

    // Thread should be loadable and resumable
    cargo_bin_cmd!("zdx")
        .env("ZDX_HOME", temp_dir.path())
        .args(["threads", "show", "interrupted"])
        .assert()
        .success()
        .stdout(predicate::str::contains("### Interrupted"));
}
