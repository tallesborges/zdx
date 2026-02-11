use assert_cmd::cargo::cargo_bin_cmd;
use predicates::prelude::*;

#[test]
fn test_help_shows_all_commands() {
    cargo_bin_cmd!("zdx")
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("exec"))
        .stdout(predicate::str::contains("daemon"))
        .stdout(predicate::str::contains("threads"));
}

#[test]
fn test_threads_help_shows_subcommands() {
    cargo_bin_cmd!("zdx")
        .args(["threads", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("list"))
        .stdout(predicate::str::contains("show"))
        .stdout(predicate::str::contains("resume"));
}

#[test]
fn test_automations_help_shows_subcommands() {
    cargo_bin_cmd!("zdx")
        .args(["automations", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("list"))
        .stdout(predicate::str::contains("runs"))
        .stdout(predicate::str::contains("validate"))
        .stdout(predicate::str::contains("run"));
}

#[test]
fn test_daemon_help_shows_poll_interval() {
    cargo_bin_cmd!("zdx")
        .args(["daemon", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("poll-interval-secs"));
}

#[test]
fn test_version_flag() {
    cargo_bin_cmd!("zdx")
        .arg("--version")
        .assert()
        .success()
        .stdout(predicate::str::contains("0.1"));
}
