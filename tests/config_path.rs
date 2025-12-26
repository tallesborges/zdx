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

    // Ensure config doesn't exist
    assert!(!config_path.exists());

    // Run zdx config init
    cargo_bin_cmd!("zdx")
        .env("ZDX_HOME", dir.path())
        .args(["config", "init"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Created config at"));

    // Assert the file now exists
    assert!(config_path.exists());

    // Verify content
    let contents = fs::read_to_string(&config_path).unwrap();
    assert!(contents.contains("model ="));
    assert!(contents.contains("max_tokens ="));
}

#[test]
fn test_config_init_fails_if_exists() {
    let dir = tempdir().unwrap();
    let config_path = dir.path().join("config.toml");

    // Create an existing config
    fs::write(&config_path, "# existing config").unwrap();

    // Run zdx config init should fail
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
