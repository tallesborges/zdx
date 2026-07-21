use assert_cmd::cargo::cargo_bin_cmd;
use predicates::prelude::*;
use serde_json::Value;
use tempfile::tempdir;

/// With no stored OAuth creds, `zdx quota --json` returns a well-formed
/// `providers` array where every provider is reported as not-authenticated
/// (no network calls are made — missing creds short-circuit before fetch).
#[test]
fn test_quota_json_reports_all_providers_not_logged_in() {
    let dir = tempdir().unwrap();

    let output = cargo_bin_cmd!("zdx")
        .env("ZDX_HOME", dir.path())
        .args(["quota", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let parsed: Value = serde_json::from_slice(&output).unwrap();
    let providers = parsed["providers"].as_array().expect("providers array");
    assert_eq!(providers.len(), 4);
    for provider in providers {
        assert!(provider["provider"].is_string());
        assert_eq!(provider["error"], Value::String("not logged in".to_string()));
    }
    let ids: Vec<&str> = providers
        .iter()
        .filter_map(|p| p["provider"].as_str())
        .collect();
    assert!(ids.contains(&"claude-cli"));
    assert!(ids.contains(&"openai-codex"));
    assert!(ids.contains(&"google-antigravity"));
    assert!(ids.contains(&"grok-build"));
}

#[test]
fn test_quota_help_lists_json_flag() {
    cargo_bin_cmd!("zdx")
        .args(["quota", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("--json"));
}
