use assert_cmd::cargo::cargo_bin_cmd;
use predicates::prelude::*;

#[test]
fn test_help_shows_all_commands() {
    cargo_bin_cmd!("zdx")
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("exec"))
        .stdout(predicate::str::contains("imagine"))
        .stdout(predicate::str::contains("automations"))
        .stdout(predicate::str::contains("threads"))
        .stdout(predicate::str::contains("--model"))
        .stdout(predicate::str::contains("--thinking"));
}

#[test]
fn test_imagine_help_shows_flags() {
    cargo_bin_cmd!("zdx")
        .args(["imagine", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("--prompt"))
        .stdout(predicate::str::contains("--out"))
        .stdout(predicate::str::contains("--model"))
        .stdout(predicate::str::contains("--aspect"))
        .stdout(predicate::str::contains("--size"));
}

#[test]
fn test_imagine_rejects_invalid_aspect_ratio() {
    cargo_bin_cmd!("zdx")
        .args(["imagine", "-p", "test", "--aspect", "2:1"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("Invalid aspect ratio"));
}

#[test]
fn test_imagine_rejects_invalid_image_size() {
    cargo_bin_cmd!("zdx")
        .args(["imagine", "-p", "test", "--size", "8K"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("Invalid image size"));
}

#[test]
fn test_threads_help_shows_subcommands() {
    cargo_bin_cmd!("zdx")
        .args(["threads", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("list"))
        .stdout(predicate::str::contains("show"))
        .stdout(predicate::str::contains("resume"))
        .stdout(predicate::str::contains("search"));
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
        .args(["automations", "daemon", "--help"])
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
