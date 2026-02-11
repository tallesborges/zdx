use std::fs;

use assert_cmd::cargo::cargo_bin_cmd;
use predicates::prelude::*;
use tempfile::tempdir;

#[test]
fn test_config_path_command() {
    let dir = tempdir().unwrap();

    cargo_bin_cmd!("zdx")
        .env("ZDX_HOME", dir.path())
        .args(["config", "path"])
        .assert()
        .success()
        .stdout(predicate::str::contains("config.toml"));
}

#[test]
fn test_config_init_creates_file() {
    let dir = tempdir().unwrap();
    let config_path = dir.path().join("config.toml");

    assert!(!config_path.exists());

    cargo_bin_cmd!("zdx")
        .env("ZDX_HOME", dir.path())
        .args(["config", "init"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Created config at"));

    assert!(config_path.exists());

    let contents = fs::read_to_string(&config_path).unwrap();
    assert!(contents.contains("model ="));
    assert!(contents.contains("# max_tokens ="));
}

#[test]
fn test_config_init_fails_if_exists() {
    let dir = tempdir().unwrap();
    let config_path = dir.path().join("config.toml");

    fs::write(&config_path, "# existing config").unwrap();

    cargo_bin_cmd!("zdx")
        .env("ZDX_HOME", dir.path())
        .args(["config", "init"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("already exists"));
}

#[test]
fn test_config_help_shows_subcommands() {
    cargo_bin_cmd!("zdx")
        .args(["config", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("path"))
        .stdout(predicate::str::contains("init"));
}

#[test]
fn test_automations_list_empty() {
    let dir = tempdir().unwrap();

    cargo_bin_cmd!("zdx")
        .env("ZDX_HOME", dir.path())
        .args(["automations", "list"])
        .assert()
        .success()
        .stdout(predicate::str::contains("No automations found."));
}

#[test]
fn test_automations_validate_single_file() {
    let user_home = tempdir().unwrap();
    let automations_dir = user_home.path().join("automations");
    fs::create_dir_all(&automations_dir).unwrap();
    fs::write(
        automations_dir.join("morning-report.md"),
        "---\nschedule: \"0 8 * * *\"\n---\nGenerate morning report.",
    )
    .unwrap();

    cargo_bin_cmd!("zdx")
        .env("ZDX_HOME", user_home.path())
        .args(["automations", "validate"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Validated 1 automation(s)."))
        .stdout(predicate::str::contains("morning-report"));
}

#[test]
fn test_automations_runs_reads_jsonl_log() {
    let dir = tempdir().unwrap();
    let runs_path = dir.path().join("automations_runs.jsonl");

    fs::write(
        &runs_path,
        concat!(
            r#"{"automation":"morning-report","trigger":"manual","attempt":1,"max_attempts":1,"started_at":"2026-02-11T08:00:00Z","finished_at":"2026-02-11T08:00:01Z","duration_ms":1000,"ok":true,"error":null,"schedule":"0 8 * * *","model":"gemini-cli:gemini-2.5-flash"}"#,
            "\n"
        ),
    )
    .unwrap();

    cargo_bin_cmd!("zdx")
        .env("ZDX_HOME", dir.path())
        .args(["automations", "runs", "morning-report"])
        .assert()
        .success()
        .stdout(predicate::str::contains("morning-report"))
        .stdout(predicate::str::contains("manual"))
        .stdout(predicate::str::contains("ok"));
}
