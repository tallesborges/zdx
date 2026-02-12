//! Integration tests for `zdx threads list` and `zdx threads show`.

use std::fs;

use assert_cmd::cargo::cargo_bin_cmd;
use predicates::prelude::*;
use serde_json::json;
use tempfile::TempDir;

/// Creates a fake thread file with the given events.
fn create_thread_file(temp_dir: &TempDir, thread_id: &str, events: &[(String, String, String)]) {
    let threads_dir = temp_dir.path().join("threads");
    fs::create_dir_all(&threads_dir).unwrap();

    let thread_path = threads_dir.join(format!("{thread_id}.jsonl"));
    let mut content = String::new();

    for (role, text, ts) in events {
        let event = serde_json::json!({
            "type": "message",
            "role": role,
            "text": text,
            "ts": ts
        });
        content.push_str(&serde_json::to_string(&event).unwrap());
        content.push('\n');
    }

    fs::write(&thread_path, content).unwrap();
}

fn create_thread_with_meta(
    temp_dir: &TempDir,
    thread_id: &str,
    title: Option<&str>,
    events: &[(String, String, String)],
) {
    let threads_dir = temp_dir.path().join("threads");
    fs::create_dir_all(&threads_dir).unwrap();

    let thread_path = threads_dir.join(format!("{thread_id}.jsonl"));
    let mut content = String::new();

    let mut meta = json!({
        "type": "meta",
        "schema_version": 1,
        "ts": "2024-01-01T00:00:00Z"
    });
    if let Some(t) = title {
        meta["title"] = json!(t);
    }
    content.push_str(&serde_json::to_string(&meta).unwrap());
    content.push('\n');

    for (role, text, ts) in events {
        let event = serde_json::json!({
            "type": "message",
            "role": role,
            "text": text,
            "ts": ts
        });
        content.push_str(&serde_json::to_string(&event).unwrap());
        content.push('\n');
    }

    fs::write(&thread_path, content).unwrap();
}

#[test]
fn test_threads_list_empty() {
    let temp_dir = TempDir::new().unwrap();

    cargo_bin_cmd!("zdx")
        .env("ZDX_HOME", temp_dir.path())
        .args(["threads", "list"])
        .assert()
        .success()
        .stdout(predicate::str::contains("No threads found."));
}

#[test]
fn test_threads_list_shows_ids() {
    let temp_dir = TempDir::new().unwrap();

    // Create two fake thread files
    create_thread_file(
        &temp_dir,
        "thread-abc",
        &[(
            "user".to_string(),
            "hello".to_string(),
            "123:000Z".to_string(),
        )],
    );
    create_thread_file(
        &temp_dir,
        "thread-xyz",
        &[(
            "user".to_string(),
            "world".to_string(),
            "456:000Z".to_string(),
        )],
    );

    cargo_bin_cmd!("zdx")
        .env("ZDX_HOME", temp_dir.path())
        .args(["threads", "list"])
        .assert()
        .success()
        .stdout(predicate::str::contains("thread-abc"))
        .stdout(predicate::str::contains("thread-xyz"));
}

#[test]
fn test_threads_list_ignores_non_jsonl() {
    let temp_dir = TempDir::new().unwrap();

    // Create threads directory with one valid and one invalid file
    let threads_dir = temp_dir.path().join("threads");
    fs::create_dir_all(&threads_dir).unwrap();

    // Valid thread file
    create_thread_file(
        &temp_dir,
        "valid-thread",
        &[(
            "user".to_string(),
            "test".to_string(),
            "123:000Z".to_string(),
        )],
    );

    // Invalid file (not .jsonl)
    fs::write(threads_dir.join("notes.txt"), "some notes").unwrap();

    cargo_bin_cmd!("zdx")
        .env("ZDX_HOME", temp_dir.path())
        .args(["threads", "list"])
        .assert()
        .success()
        .stdout(predicate::str::contains("valid-thread"))
        .stdout(predicate::str::contains("notes").not());
}

#[test]
fn test_threads_show_prints_transcript() {
    let temp_dir = TempDir::new().unwrap();

    create_thread_file(
        &temp_dir,
        "my-thread",
        &[
            (
                "user".to_string(),
                "What is Rust?".to_string(),
                "100:000Z".to_string(),
            ),
            (
                "assistant".to_string(),
                "Rust is a systems programming language.".to_string(),
                "101:000Z".to_string(),
            ),
            (
                "user".to_string(),
                "Thanks!".to_string(),
                "102:000Z".to_string(),
            ),
        ],
    );

    cargo_bin_cmd!("zdx")
        .env("ZDX_HOME", temp_dir.path())
        .args(["threads", "show", "my-thread"])
        .assert()
        .success()
        .stdout(predicate::str::contains("### You"))
        .stdout(predicate::str::contains("What is Rust?"))
        .stdout(predicate::str::contains("### Assistant"))
        .stdout(predicate::str::contains(
            "Rust is a systems programming language.",
        ))
        .stdout(predicate::str::contains("Thanks!"));
}

#[test]
fn test_threads_show_nonexistent() {
    let temp_dir = TempDir::new().unwrap();

    cargo_bin_cmd!("zdx")
        .env("ZDX_HOME", temp_dir.path())
        .args(["threads", "show", "does-not-exist"])
        .assert()
        .success()
        .stdout(predicate::str::contains("empty or not found"));
}

#[test]
fn test_threads_list_shows_multiple_sorted() {
    let temp_dir = TempDir::new().unwrap();

    // Create threads with different content
    create_thread_file(
        &temp_dir,
        "first-thread",
        &[(
            "user".to_string(),
            "first".to_string(),
            "100:000Z".to_string(),
        )],
    );

    // Small delay to ensure different modification times
    std::thread::sleep(std::time::Duration::from_millis(10));

    create_thread_file(
        &temp_dir,
        "second-thread",
        &[(
            "user".to_string(),
            "second".to_string(),
            "200:000Z".to_string(),
        )],
    );

    let output = cargo_bin_cmd!("zdx")
        .env("ZDX_HOME", temp_dir.path())
        .args(["threads", "list"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let output_str = String::from_utf8_lossy(&output);

    // Both threads should be listed
    assert!(output_str.contains("first-thread"));
    assert!(output_str.contains("second-thread"));

    // second-thread should appear before first-thread (newer first)
    let first_pos = output_str.find("first-thread").unwrap();
    let second_pos = output_str.find("second-thread").unwrap();
    assert!(
        second_pos < first_pos,
        "Threads should be sorted by modification time (newest first)"
    );
}

#[test]
fn test_threads_list_shows_title_from_meta() {
    let temp_dir = TempDir::new().unwrap();

    create_thread_with_meta(&temp_dir, "titled-thread", Some("My Thread Title"), &[]);

    let output = cargo_bin_cmd!("zdx")
        .env("ZDX_HOME", temp_dir.path())
        .args(["threads", "list"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let output_str = String::from_utf8_lossy(&output);
    assert!(output_str.contains("My Thread Title"));
    assert!(output_str.contains("titled-thread"));
}

#[test]
fn test_threads_rename_updates_title() {
    let temp_dir = TempDir::new().unwrap();

    create_thread_with_meta(
        &temp_dir,
        "rename-thread",
        Some("Old Title"),
        &[(
            "user".to_string(),
            "hello".to_string(),
            "123:000Z".to_string(),
        )],
    );

    cargo_bin_cmd!("zdx")
        .env("ZDX_HOME", temp_dir.path())
        .args(["threads", "rename", "rename-thread", "New Title"])
        .assert()
        .success()
        .stdout(predicate::str::contains("New Title"));

    // Ensure list reflects new title
    let output = cargo_bin_cmd!("zdx")
        .env("ZDX_HOME", temp_dir.path())
        .args(["threads", "list"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let output_str = String::from_utf8_lossy(&output);
    assert!(output_str.contains("New Title"));

    // Verify meta line was updated on disk
    let thread_path = temp_dir.path().join("threads").join("rename-thread.jsonl");
    let first_line = fs::read_to_string(thread_path)
        .unwrap()
        .lines()
        .next()
        .unwrap()
        .to_string();
    let meta: serde_json::Value = serde_json::from_str(&first_line).unwrap();
    assert_eq!(meta["title"], json!("New Title"));
}

#[test]
fn test_threads_rename_missing_thread_fails() {
    let temp_dir = TempDir::new().unwrap();
    let missing_path = temp_dir.path().join("threads").join("missing-thread.jsonl");

    cargo_bin_cmd!("zdx")
        .env("ZDX_HOME", temp_dir.path())
        .args(["threads", "rename", "missing-thread", "New Title"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "Thread 'missing-thread' not found",
        ));

    assert!(
        !missing_path.exists(),
        "Renaming a missing thread should not create a thread file"
    );
}

#[test]
fn test_threads_search_query_matches_content() {
    let temp_dir = TempDir::new().unwrap();

    create_thread_with_meta(
        &temp_dir,
        "topic-thread",
        Some("Thread search planning"),
        &[(
            "user".to_string(),
            "we should improve the thread search flow".to_string(),
            "2026-02-12T10:00:00Z".to_string(),
        )],
    );
    create_thread_with_meta(
        &temp_dir,
        "other-thread",
        Some("Unrelated"),
        &[(
            "user".to_string(),
            "totally different topic".to_string(),
            "2026-02-12T11:00:00Z".to_string(),
        )],
    );

    let output = cargo_bin_cmd!("zdx")
        .env("ZDX_HOME", temp_dir.path())
        .args(["threads", "search", "thread search"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let output_str = String::from_utf8_lossy(&output);
    assert!(output_str.contains("topic-thread"));
    assert!(!output_str.contains("other-thread"));
}

#[test]
fn test_threads_search_filters_by_date() {
    let temp_dir = TempDir::new().unwrap();

    create_thread_with_meta(
        &temp_dir,
        "feb-thread",
        Some("February work"),
        &[(
            "user".to_string(),
            "worked on reports".to_string(),
            "2026-02-12T09:00:00Z".to_string(),
        )],
    );
    create_thread_with_meta(
        &temp_dir,
        "jan-thread",
        Some("January work"),
        &[(
            "user".to_string(),
            "worked on setup".to_string(),
            "2026-01-10T09:00:00Z".to_string(),
        )],
    );

    let output = cargo_bin_cmd!("zdx")
        .env("ZDX_HOME", temp_dir.path())
        .args(["threads", "search", "--date", "2026-02-12"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let output_str = String::from_utf8_lossy(&output);
    assert!(output_str.contains("feb-thread"));
    assert!(!output_str.contains("jan-thread"));
}

#[test]
fn test_threads_search_json_output() {
    let temp_dir = TempDir::new().unwrap();

    create_thread_with_meta(
        &temp_dir,
        "json-thread",
        Some("Automation report thread"),
        &[(
            "user".to_string(),
            "daily report generated".to_string(),
            "2026-02-12T12:00:00Z".to_string(),
        )],
    );

    let output = cargo_bin_cmd!("zdx")
        .env("ZDX_HOME", temp_dir.path())
        .args(["threads", "search", "report", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let parsed: serde_json::Value = serde_json::from_slice(&output).unwrap();
    let first = parsed.as_array().and_then(|arr| arr.first()).unwrap();
    assert_eq!(first["thread_id"], json!("json-thread"));
    assert_eq!(first["title"], json!("Automation report thread"));
    assert!(first["preview"].as_str().is_some());
}
