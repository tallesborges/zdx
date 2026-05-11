//! Integration tests for `zdx threads export`.

use std::fs;
use std::os::unix::fs::PermissionsExt;
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

fn write_fake_qmd(temp_dir: &TempDir) -> std::path::PathBuf {
    let qmd_path = temp_dir.path().join("qmd-fake");
    let log_path = temp_dir.path().join("qmd.log");
    let script = format!(
        "#!/bin/sh\n\
         set -eu\n\
         printf 'ARGS:%s\\n' \"$*\" >> {log:?}\n\
         printf 'XDG_CACHE_HOME:%s\\n' \"${{XDG_CACHE_HOME:-}}\" >> {log:?}\n\
         printf 'XDG_CONFIG_HOME:%s\\n' \"${{XDG_CONFIG_HOME:-}}\" >> {log:?}\n\
         printf 'XDG_DATA_HOME:%s\\n' \"${{XDG_DATA_HOME:-}}\" >> {log:?}\n\
         if [ \"${{1:-}}\" = collection ] && [ \"${{2:-}}\" = show ]; then\n\
           echo 'Collection not found: zdx-threads' >&2\n\
           exit 1\n\
         fi\n\
         exit 0\n",
        log = log_path.display().to_string()
    );
    fs::write(&qmd_path, script).unwrap();
    let mut permissions = fs::metadata(&qmd_path).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&qmd_path, permissions).unwrap();
    qmd_path
}

fn write_qmd_config(temp_dir: &TempDir, qmd_path: &std::path::Path) {
    let command = qmd_path.to_string_lossy().replace('\\', "\\\\");
    fs::write(
        temp_dir.path().join("config.toml"),
        format!("[qmd]\ncommand = \"{command}\"\n"),
    )
    .unwrap();
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
fn test_threads_index_exports_and_invokes_qmd() {
    let temp_dir = TempDir::new().unwrap();
    create_thread(&temp_dir, "thread-index");
    let qmd_path = write_fake_qmd(&temp_dir);
    write_qmd_config(&temp_dir, &qmd_path);

    cargo_bin_cmd!("zdx")
        .env("ZDX_HOME", temp_dir.path())
        .env_remove("XDG_CACHE_HOME")
        .env_remove("XDG_CONFIG_HOME")
        .env_remove("XDG_DATA_HOME")
        .args(["threads", "index"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "exported=1, skipped=0, removed=0, failed=0",
        ))
        .stdout(predicate::str::contains(
            "qmd collection: created zdx-threads",
        ))
        .stdout(predicate::str::contains(
            "qmd index: updated and embedded zdx-threads",
        ));

    assert!(
        temp_dir
            .path()
            .join("exports")
            .join("threads")
            .join("thread-index.md")
            .exists()
    );

    let log = fs::read_to_string(temp_dir.path().join("qmd.log")).unwrap();
    assert!(log.contains("ARGS:collection show zdx-threads"));
    assert!(log.contains("ARGS:collection add"));
    assert!(log.contains("--name zdx-threads --mask **/*.md"));
    assert!(log.contains("ARGS:update"));
    assert!(log.contains("ARGS:embed -c zdx-threads"));
    assert!(log.contains("XDG_CACHE_HOME:\n"));
    assert!(log.contains("XDG_CONFIG_HOME:\n"));
    assert!(log.contains("XDG_DATA_HOME:\n"));
}
