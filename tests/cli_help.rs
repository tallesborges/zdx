use assert_cmd::cargo::cargo_bin_cmd;
use predicates::prelude::*;

#[test]
fn test_help_shows_all_commands() {
    cargo_bin_cmd!("zdx-cli")
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("exec"))
        .stdout(predicate::str::contains("chat"))
        .stdout(predicate::str::contains("sessions"))
        .stdout(predicate::str::contains("resume"));
}

#[test]
fn test_sessions_help_shows_subcommands() {
    cargo_bin_cmd!("zdx-cli")
        .args(["sessions", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("list"))
        .stdout(predicate::str::contains("show"));
}

#[test]
fn test_version_flag() {
    cargo_bin_cmd!("zdx-cli")
        .arg("--version")
        .assert()
        .success()
        .stdout(predicate::str::contains("0.1"));
}
