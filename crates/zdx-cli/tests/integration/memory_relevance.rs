//! Judged relevance fixtures for native lexical memory search (Phase 5).
//!
//! Acceptance thresholds encoded here (and documented in
//! `docs/plans/drafts/integrated-memory-index-and-embeddings.md`):
//! - success@k = 100% on the fixed judged query set (exact names, paths,
//!   URLs, error strings, commands, broad recall, long-thread, notes,
//!   calendar);
//! - warm per-query CLI wall time < 5s (coarse regression net; library-level
//!   search is expected well under 300ms);
//! - fixture index size < 5 MB for the ~100 KB corpus;
//! - a missing index fails with actionable stale-index guidance;

use std::fmt::Write as _;
use std::fs;
use std::time::{Duration, Instant};

use assert_cmd::cargo::cargo_bin_cmd;
use predicates::prelude::*;
use serde_json::json;
use tempfile::TempDir;

const QUERY_TIME_BUDGET: Duration = Duration::from_secs(5);
const MAX_INDEX_BYTES: u64 = 5 * 1024 * 1024;

fn build_corpus(temp_dir: &TempDir) {
    let notes = temp_dir.path().join("memory").join("Notes");
    let calendar = temp_dir.path().join("memory").join("Calendar");
    fs::create_dir_all(notes.join("People")).unwrap();
    fs::create_dir_all(&calendar).unwrap();

    fs::write(
        notes.join("People").join("Robert Manship.md"),
        "# Robert Manship\n\nWeekly private English classes with Robert Manship.\n",
    )
    .unwrap();
    fs::write(
        notes.join("Parity Links.md"),
        "# Parity Links\n\nMan pages live at https://manpage.paseo.li/trinity for the whole stack.\n\nAsk questions with `gh issue create --repo paritytech/man` instead of PRs.\n\nStorage architecture is led by Robert Klotzner.\n",
    )
    .unwrap();
    fs::write(
        notes.join("Solar.md"),
        "# Solar\n\nSolar panel sizing for the chácara roof: around 5 kWp with inverter budget headroom.\n",
    )
    .unwrap();
    fs::write(
        notes.join("Errors.md"),
        "# Errors\n\nSeen while refactoring crates/zdx-engine/src/core/native_memory.rs:\n\nerror[E0433]: cannot find type `HashMap` in this scope\n",
    )
    .unwrap();

    // Large note, kept to exercise chunking of oversized sources.
    let mut big = String::from("# Big Doc\n\nbigdoc-marker anchor paragraph.\n\n");
    for index in 0..900 {
        writeln!(
            &mut big,
            "Paragraph {index}: filler prose about long documents and bounded snapshot reads.\n"
        )
        .unwrap();
    }
    fs::write(notes.join("Big Doc.md"), big).unwrap();

    fs::write(
        calendar.join("2026-08-01.md"),
        "# 2026-08-01\n\nEnglish class: IPA pronunciation practice and connected speech.\n",
    )
    .unwrap();
    fs::write(
        calendar.join("2026-07-15.md"),
        "# 2026-07-15\n\nDeployed identity backend to the ibv2 server.\n",
    )
    .unwrap();

    write_thread(
        temp_dir,
        "alpha-thread",
        &[
            ("user", "how is the agent-bench proxy sweep going?"),
            (
                "assistant",
                "the agent-bench proxy sweep passed on four models",
            ),
        ],
    );

    // Long thread with the judged marker buried near the end.
    let mut events: Vec<(&str, String)> = Vec::new();
    for index in 0..240 {
        let role = if index % 2 == 0 { "user" } else { "assistant" };
        events.push((
            role,
            format!("routine message {index} about unrelated project chatter"),
        ));
    }
    events.push((
        "assistant",
        "found it: the quantum-flamingo deployment key lives in the vault".to_string(),
    ));
    let events: Vec<(&str, &str)> = events
        .iter()
        .map(|(role, text)| (*role, text.as_str()))
        .collect();
    write_thread(temp_dir, "long-thread", &events);
}

fn write_thread(temp_dir: &TempDir, thread_id: &str, messages: &[(&str, &str)]) {
    let threads_dir = temp_dir.path().join("threads");
    fs::create_dir_all(&threads_dir).unwrap();
    let mut content = String::new();
    content.push_str(
        &serde_json::to_string(&json!({
            "type": "meta",
            "schema_version": 1,
            "ts": "2026-05-10T00:00:00Z"
        }))
        .unwrap(),
    );
    content.push('\n');
    for (index, (role, text)) in messages.iter().enumerate() {
        content.push_str(
            &serde_json::to_string(&json!({
                "type": "message",
                "role": role,
                "text": text,
                "ts": format!("2026-05-10T00:{:02}:{:02}Z", index / 60, index % 60)
            }))
            .unwrap(),
        );
        content.push('\n');
    }
    fs::write(threads_dir.join(format!("{thread_id}.jsonl")), content).unwrap();
}

fn search_json(temp_dir: &TempDir, query: &str, source: Option<&str>) -> serde_json::Value {
    let mut args = vec!["memory", "search", query, "--json"];
    if let Some(source) = source {
        args.push("--source");
        args.push(source);
    }
    let output = cargo_bin_cmd!("zdx")
        .env("ZDX_HOME", temp_dir.path())
        .args(&args)
        .assert()
        .success();
    serde_json::from_slice(&output.get_output().stdout).unwrap()
}

/// `(label, query, source filter, expected file, judged top-k)` fixture set.
fn judged_queries() -> &'static [(
    &'static str,
    &'static str,
    Option<&'static str>,
    &'static str,
    usize,
)] {
    &[
        (
            "exact-name",
            "Robert Manship",
            Some("note"),
            "People/Robert Manship.md",
            1,
        ),
        (
            "path",
            "crates/zdx-engine/src/core/native_memory.rs",
            None,
            "Errors.md",
            1,
        ),
        (
            "url",
            "https://manpage.paseo.li/trinity",
            None,
            "Parity Links.md",
            1,
        ),
        (
            "error-string",
            "error[E0433]: cannot find type",
            None,
            "Errors.md",
            1,
        ),
        (
            "command",
            "gh issue create --repo paritytech/man",
            None,
            "Parity Links.md",
            1,
        ),
        (
            "broad-recall",
            "solar panel sizing roof",
            None,
            "Solar.md",
            3,
        ),
        (
            "accent-insensitive",
            "chacara inverter budget",
            None,
            "Solar.md",
            3,
        ),
        (
            "long-thread",
            "quantum-flamingo deployment key",
            None,
            "long-thread.md",
            1,
        ),
        (
            "calendar",
            "IPA pronunciation practice",
            Some("calendar"),
            "2026-08-01.md",
            1,
        ),
        (
            "thread-scoped",
            "agent-bench proxy sweep",
            Some("thread"),
            "alpha-thread.md",
            1,
        ),
    ]
}

/// Judged fixture set: every query must place its expected document within
/// the judged top-k (success@k = 100% on this deterministic corpus).
#[test]
fn test_lexical_relevance_fixture_set() {
    let temp_dir = TempDir::new().unwrap();
    build_corpus(&temp_dir);

    cargo_bin_cmd!("zdx")
        .env("ZDX_HOME", temp_dir.path())
        .args(["memory", "index"])
        .assert()
        .success();

    let mut failures = Vec::new();
    for (label, query, source, expected_file, k) in judged_queries() {
        let started = Instant::now();
        let output = search_json(&temp_dir, query, *source);
        let elapsed = started.elapsed();
        assert!(
            elapsed < QUERY_TIME_BUDGET,
            "{label}: query took {elapsed:?}, budget {QUERY_TIME_BUDGET:?}"
        );

        let paths: Vec<&str> = output["results"]
            .as_array()
            .unwrap()
            .iter()
            .take(*k)
            .filter_map(|result| result["path"].as_str())
            .collect();
        if !paths.iter().any(|path| path.ends_with(expected_file)) {
            failures.push(format!(
                "{label}: expected {expected_file} in top-{k}, got {paths:?}"
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "relevance fixture failures:\n{}",
        failures.join("\n")
    );

    // Index size threshold for this fixture corpus.
    let index_bytes = fs::metadata(temp_dir.path().join("cache").join("memory.sqlite"))
        .unwrap()
        .len();
    assert!(
        index_bytes < MAX_INDEX_BYTES,
        "memory.sqlite is {index_bytes} bytes, threshold {MAX_INDEX_BYTES}"
    );
}

/// A missing index must fail with actionable guidance, not empty results.
#[test]
fn test_search_without_index_reports_stale_index_guidance() {
    let temp_dir = TempDir::new().unwrap();
    build_corpus(&temp_dir);

    cargo_bin_cmd!("zdx")
        .env("ZDX_HOME", temp_dir.path())
        .args(["memory", "search", "solar"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("run `zdx memory index`"));
}
