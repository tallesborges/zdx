//! Integration tests for `zdx threads export`.

use std::fs;
use std::time::Duration;

use assert_cmd::cargo::cargo_bin_cmd;
use predicates::prelude::*;
use serde_json::json;
use tempfile::TempDir;

fn create_thread(temp_dir: &TempDir, thread_id: &str) {
    let threads_dir = temp_dir.path().join("threads");
    fs::create_dir_all(&threads_dir).unwrap();

    let events = [
        json!({
            "type": "meta",
            "schema_version": 1,
            "ts": "2026-05-10T00:00:00Z"
        }),
        json!({
            "type": "message",
            "role": "user",
            "text": "hello\nthere",
            "ts": "2026-05-10T00:00:01Z"
        }),
        json!({
            "type": "tool_use",
            "id": "tool-1",
            "name": "read",
            "input": { "file_path": "notes.md" },
            "ts": "2026-05-10T00:00:02Z"
        }),
        json!({
            "type": "message",
            "role": "assistant",
            "text": "answer\twith   spaces",
            "ts": "2026-05-10T00:00:03Z"
        }),
    ];

    let content = events
        .iter()
        .map(|event| serde_json::to_string(event).unwrap())
        .collect::<Vec<_>>()
        .join("\n")
        + "\n";
    fs::write(threads_dir.join(format!("{thread_id}.jsonl")), content).unwrap();
}

#[test]
fn test_threads_export_creates_markdown_transcript() {
    let temp_dir = TempDir::new().unwrap();
    create_thread(&temp_dir, "thread-export");

    cargo_bin_cmd!("zdx")
        .env("ZDX_HOME", temp_dir.path())
        .args(["threads", "export"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "exported=1, skipped=0, removed=0, failed=0",
        ));

    let transcript = fs::read_to_string(
        temp_dir
            .path()
            .join("exports")
            .join("threads")
            .join("thread-export.md"),
    )
    .unwrap();
    assert_eq!(
        transcript,
        "# Thread thread-export\n\nUser: hello there\nAssistant: answer with spaces\n"
    );
}

#[test]
fn test_threads_export_skips_unchanged_threads() {
    let temp_dir = TempDir::new().unwrap();
    create_thread(&temp_dir, "thread-skip");

    cargo_bin_cmd!("zdx")
        .env("ZDX_HOME", temp_dir.path())
        .args(["threads", "export"])
        .assert()
        .success();

    cargo_bin_cmd!("zdx")
        .env("ZDX_HOME", temp_dir.path())
        .args(["threads", "export"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "exported=0, skipped=1, removed=0, failed=0",
        ));
}

#[test]
fn test_threads_export_force_regenerates_unchanged_threads() {
    let temp_dir = TempDir::new().unwrap();
    create_thread(&temp_dir, "thread-force");

    cargo_bin_cmd!("zdx")
        .env("ZDX_HOME", temp_dir.path())
        .args(["threads", "export"])
        .assert()
        .success();

    std::thread::sleep(Duration::from_millis(20));

    cargo_bin_cmd!("zdx")
        .env("ZDX_HOME", temp_dir.path())
        .args(["threads", "export", "--force"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "exported=1, skipped=0, removed=0, failed=0",
        ));
}

#[test]
fn test_threads_export_dry_run_does_not_write_files() {
    let temp_dir = TempDir::new().unwrap();
    create_thread(&temp_dir, "thread-dry-run");

    cargo_bin_cmd!("zdx")
        .env("ZDX_HOME", temp_dir.path())
        .args(["threads", "export", "--dry-run"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "exported=1, skipped=0, removed=0, failed=0",
        ));

    assert!(
        !temp_dir
            .path()
            .join("exports")
            .join("threads")
            .join("thread-dry-run.md")
            .exists()
    );
}

#[test]
fn test_threads_export_removes_stale_exports() {
    let temp_dir = TempDir::new().unwrap();
    let exports_dir = temp_dir.path().join("exports").join("threads");
    fs::create_dir_all(&exports_dir).unwrap();
    let stale_path = exports_dir.join("stale-thread.md");
    fs::write(&stale_path, "# stale").unwrap();

    cargo_bin_cmd!("zdx")
        .env("ZDX_HOME", temp_dir.path())
        .args(["threads", "export"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "exported=0, skipped=0, removed=1, failed=0",
        ));

    assert!(!stale_path.exists());
}

#[test]
fn test_memory_index_exports_and_builds_native_sqlite() {
    let temp_dir = TempDir::new().unwrap();
    create_thread(&temp_dir, "thread-index");

    cargo_bin_cmd!("zdx")
        .env("ZDX_HOME", temp_dir.path())
        .args(["memory", "index"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "exported=1, skipped=0, removed=0, failed=0",
        ))
        .stdout(predicate::str::contains("Native memory: documents=1"));

    assert!(
        temp_dir
            .path()
            .join("exports")
            .join("threads")
            .join("thread-index.md")
            .exists()
    );

    assert!(temp_dir.path().join("cache").join("memory.sqlite").exists());
}

#[test]
fn test_memory_index_second_run_reads_no_unchanged_thread_files() {
    let temp_dir = TempDir::new().unwrap();
    create_thread(&temp_dir, "thread-incremental");

    let first = cargo_bin_cmd!("zdx")
        .env("ZDX_HOME", temp_dir.path())
        .args(["memory", "index", "--json"])
        .assert()
        .success();
    let first: serde_json::Value = serde_json::from_slice(&first.get_output().stdout).unwrap();
    assert_eq!(first["thread_cache"]["metas_read"], 1);
    assert_eq!(first["thread_exports"]["exported"], 1);

    let second = cargo_bin_cmd!("zdx")
        .env("ZDX_HOME", temp_dir.path())
        .args(["memory", "index", "--json"])
        .assert()
        .success();
    let second: serde_json::Value = serde_json::from_slice(&second.get_output().stdout).unwrap();
    assert_eq!(second["thread_cache"]["files_enumerated"], 1);
    assert_eq!(second["thread_cache"]["metas_read"], 0);
    assert_eq!(second["thread_exports"]["exported"], 0);
    assert_eq!(second["thread_exports"]["skipped"], 1);

    // Deleting the derived cache rebuilds it and re-exports without data loss.
    fs::remove_file(temp_dir.path().join("cache").join("threads.sqlite")).unwrap();
    let rebuilt = cargo_bin_cmd!("zdx")
        .env("ZDX_HOME", temp_dir.path())
        .args(["memory", "index", "--json"])
        .assert()
        .success();
    let rebuilt: serde_json::Value = serde_json::from_slice(&rebuilt.get_output().stdout).unwrap();
    assert_eq!(rebuilt["thread_cache"]["metas_read"], 1);
    assert_eq!(rebuilt["thread_exports"]["exported"], 1);

    cargo_bin_cmd!("zdx")
        .env("ZDX_HOME", temp_dir.path())
        .args(["memory", "search", "hello", "--source", "thread", "--json"])
        .assert()
        .success()
        .stdout(predicate::str::contains(r#""docid": "zdxmem:v1:thread:"#));
}

#[test]
fn test_memory_index_reexports_changed_thread_only() {
    let temp_dir = TempDir::new().unwrap();
    create_thread(&temp_dir, "thread-a");
    create_thread(&temp_dir, "thread-b");

    cargo_bin_cmd!("zdx")
        .env("ZDX_HOME", temp_dir.path())
        .args(["memory", "index"])
        .assert()
        .success();

    std::thread::sleep(Duration::from_millis(20));
    let thread_a = temp_dir.path().join("threads").join("thread-a.jsonl");
    let mut content = fs::read_to_string(&thread_a).unwrap();
    content.push_str(
        &serde_json::to_string(&json!({
            "type": "message",
            "role": "user",
            "text": "a brand new message",
            "ts": "2026-05-10T00:00:04Z"
        }))
        .unwrap(),
    );
    content.push('\n');
    fs::write(&thread_a, content).unwrap();

    let summary = cargo_bin_cmd!("zdx")
        .env("ZDX_HOME", temp_dir.path())
        .args(["memory", "index", "--json"])
        .assert()
        .success();
    let summary: serde_json::Value = serde_json::from_slice(&summary.get_output().stdout).unwrap();
    assert_eq!(summary["thread_cache"]["files_enumerated"], 2);
    assert_eq!(summary["thread_cache"]["metas_read"], 1);
    assert_eq!(summary["thread_exports"]["exported"], 1);
    assert_eq!(summary["thread_exports"]["skipped"], 1);
}

#[test]
fn test_memory_search_returns_native_docids() {
    let temp_dir = TempDir::new().unwrap();
    create_thread(&temp_dir, "thread-native");

    cargo_bin_cmd!("zdx")
        .env("ZDX_HOME", temp_dir.path())
        .args(["memory", "index"])
        .assert()
        .success();

    cargo_bin_cmd!("zdx")
        .env("ZDX_HOME", temp_dir.path())
        .args(["memory", "search", "hello", "--source", "thread", "--json"])
        .assert()
        .success()
        .stdout(predicate::str::contains(r#""docid": "zdxmem:v1:thread:"#))
        .stdout(predicate::str::contains(r#""source": "thread""#))
        .stdout(predicate::str::contains(
            r#""file": "thread://thread-native.md""#,
        ));
}
