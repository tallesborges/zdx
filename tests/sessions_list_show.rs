//! Integration tests for `zdx sessions list` and `zdx sessions show`.

use std::fs;

use assert_cmd::cargo::cargo_bin_cmd;
use predicates::prelude::*;
use serde_json::json;
use tempfile::TempDir;

/// Creates a fake session file with the given events.
fn create_session_file(temp_dir: &TempDir, session_id: &str, events: &[(String, String, String)]) {
    let sessions_dir = temp_dir.path().join("sessions");
    fs::create_dir_all(&sessions_dir).unwrap();

    let session_path = sessions_dir.join(format!("{}.jsonl", session_id));
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

    fs::write(&session_path, content).unwrap();
}

fn create_session_with_meta(
    temp_dir: &TempDir,
    session_id: &str,
    title: Option<&str>,
    events: &[(String, String, String)],
) {
    let sessions_dir = temp_dir.path().join("sessions");
    fs::create_dir_all(&sessions_dir).unwrap();

    let session_path = sessions_dir.join(format!("{}.jsonl", session_id));
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

    fs::write(&session_path, content).unwrap();
}

#[test]
fn test_sessions_list_empty() {
    let temp_dir = TempDir::new().unwrap();

    cargo_bin_cmd!("zdx")
        .env("ZDX_HOME", temp_dir.path())
        .args(["sessions", "list"])
        .assert()
        .success()
        .stdout(predicate::str::contains("No sessions found."));
}

#[test]
fn test_sessions_list_shows_ids() {
    let temp_dir = TempDir::new().unwrap();

    // Create two fake session files
    create_session_file(
        &temp_dir,
        "session-abc",
        &[(
            "user".to_string(),
            "hello".to_string(),
            "123:000Z".to_string(),
        )],
    );
    create_session_file(
        &temp_dir,
        "session-xyz",
        &[(
            "user".to_string(),
            "world".to_string(),
            "456:000Z".to_string(),
        )],
    );

    cargo_bin_cmd!("zdx")
        .env("ZDX_HOME", temp_dir.path())
        .args(["sessions", "list"])
        .assert()
        .success()
        .stdout(predicate::str::contains("session-abc"))
        .stdout(predicate::str::contains("session-xyz"));
}

#[test]
fn test_sessions_list_ignores_non_jsonl() {
    let temp_dir = TempDir::new().unwrap();

    // Create sessions directory with one valid and one invalid file
    let sessions_dir = temp_dir.path().join("sessions");
    fs::create_dir_all(&sessions_dir).unwrap();

    // Valid session file
    create_session_file(
        &temp_dir,
        "valid-session",
        &[(
            "user".to_string(),
            "test".to_string(),
            "123:000Z".to_string(),
        )],
    );

    // Invalid file (not .jsonl)
    fs::write(sessions_dir.join("notes.txt"), "some notes").unwrap();

    cargo_bin_cmd!("zdx")
        .env("ZDX_HOME", temp_dir.path())
        .args(["sessions", "list"])
        .assert()
        .success()
        .stdout(predicate::str::contains("valid-session"))
        .stdout(predicate::str::contains("notes").not());
}

#[test]
fn test_sessions_show_prints_transcript() {
    let temp_dir = TempDir::new().unwrap();

    create_session_file(
        &temp_dir,
        "my-session",
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
        .args(["sessions", "show", "my-session"])
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
fn test_sessions_show_nonexistent() {
    let temp_dir = TempDir::new().unwrap();

    cargo_bin_cmd!("zdx")
        .env("ZDX_HOME", temp_dir.path())
        .args(["sessions", "show", "does-not-exist"])
        .assert()
        .success()
        .stdout(predicate::str::contains("empty or not found"));
}

#[test]
fn test_sessions_list_shows_multiple_sorted() {
    let temp_dir = TempDir::new().unwrap();

    // Create sessions with different content
    create_session_file(
        &temp_dir,
        "first-session",
        &[(
            "user".to_string(),
            "first".to_string(),
            "100:000Z".to_string(),
        )],
    );

    // Small delay to ensure different modification times
    std::thread::sleep(std::time::Duration::from_millis(10));

    create_session_file(
        &temp_dir,
        "second-session",
        &[(
            "user".to_string(),
            "second".to_string(),
            "200:000Z".to_string(),
        )],
    );

    let output = cargo_bin_cmd!("zdx")
        .env("ZDX_HOME", temp_dir.path())
        .args(["sessions", "list"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let output_str = String::from_utf8_lossy(&output);

    // Both sessions should be listed
    assert!(output_str.contains("first-session"));
    assert!(output_str.contains("second-session"));

    // second-session should appear before first-session (newer first)
    let first_pos = output_str.find("first-session").unwrap();
    let second_pos = output_str.find("second-session").unwrap();
    assert!(
        second_pos < first_pos,
        "Sessions should be sorted by modification time (newest first)"
    );
}

#[test]
fn test_sessions_list_shows_title_from_meta() {
    let temp_dir = TempDir::new().unwrap();

    create_session_with_meta(&temp_dir, "titled-session", Some("My Session Title"), &[]);

    let output = cargo_bin_cmd!("zdx")
        .env("ZDX_HOME", temp_dir.path())
        .args(["sessions", "list"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let output_str = String::from_utf8_lossy(&output);
    assert!(output_str.contains("My Session Title"));
    assert!(output_str.contains("titled-session"));
}

#[test]
fn test_sessions_rename_updates_title() {
    let temp_dir = TempDir::new().unwrap();

    create_session_with_meta(
        &temp_dir,
        "rename-session",
        Some("Old Title"),
        &[(
            "user".to_string(),
            "hello".to_string(),
            "123:000Z".to_string(),
        )],
    );

    cargo_bin_cmd!("zdx")
        .env("ZDX_HOME", temp_dir.path())
        .args(["sessions", "rename", "rename-session", "New Title"])
        .assert()
        .success()
        .stdout(predicate::str::contains("New Title"));

    // Ensure list reflects new title
    let output = cargo_bin_cmd!("zdx")
        .env("ZDX_HOME", temp_dir.path())
        .args(["sessions", "list"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let output_str = String::from_utf8_lossy(&output);
    assert!(output_str.contains("New Title"));

    // Verify meta line was updated on disk
    let session_path = temp_dir
        .path()
        .join("sessions")
        .join("rename-session.jsonl");
    let first_line = fs::read_to_string(session_path)
        .unwrap()
        .lines()
        .next()
        .unwrap()
        .to_string();
    let meta: serde_json::Value = serde_json::from_str(&first_line).unwrap();
    assert_eq!(meta["title"], json!("New Title"));
}

#[test]
fn test_sessions_rename_missing_session_fails() {
    let temp_dir = TempDir::new().unwrap();
    let missing_path = temp_dir
        .path()
        .join("sessions")
        .join("missing-session.jsonl");

    cargo_bin_cmd!("zdx")
        .env("ZDX_HOME", temp_dir.path())
        .args(["sessions", "rename", "missing-session", "New Title"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("Session 'missing-session' not found"));

    assert!(
        !missing_path.exists(),
        "Renaming a missing session should not create a session file"
    );
}
