//! Integration tests for login/logout commands.

use std::fs;
use std::io::Write;
use std::process::{Command, Stdio};

use assert_cmd::prelude::*;
use predicates::prelude::*;
use tempfile::tempdir;

/// Test: logout without --anthropic shows error.
#[test]
fn test_logout_requires_provider_flag() {
    Command::cargo_bin("zdx-cli")
        .unwrap()
        .arg("logout")
        .assert()
        .failure()
        .stderr(predicate::str::contains("Please specify a provider"));
}

/// Test: login without --anthropic shows error.
#[test]
fn test_login_requires_provider_flag() {
    Command::cargo_bin("zdx-cli")
        .unwrap()
        .arg("login")
        .assert()
        .failure()
        .stderr(predicate::str::contains("Please specify a provider"));
}

/// Test: logout --anthropic when not logged in shows message.
#[test]
fn test_logout_when_not_logged_in() {
    let temp = tempdir().unwrap();

    Command::cargo_bin("zdx-cli")
        .unwrap()
        .env("ZDX_HOME", temp.path())
        .arg("logout")
        .arg("--anthropic")
        .assert()
        .success()
        .stdout(predicate::str::contains("Not logged in to Anthropic"));
}

/// Test: login --anthropic writes token to oauth.json.
#[test]
fn test_login_stores_token() {
    let temp = tempdir().unwrap();
    let oauth_path = temp.path().join("oauth.json");

    // Simulate pasting a token via stdin
    let mut child = Command::cargo_bin("zdx-cli")
        .unwrap()
        .env("ZDX_HOME", temp.path())
        .env("ZDX_NO_BROWSER", "1")
        .arg("login")
        .arg("--anthropic")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("Failed to spawn command");

    // Write the token to stdin
    {
        let stdin = child.stdin.as_mut().expect("Failed to open stdin");
        stdin
            .write_all(b"sk-ant-test-token-12345678901234567890\n")
            .expect("Failed to write to stdin");
    }

    let output = child.wait_with_output().expect("Failed to read output");
    assert!(output.status.success(), "Command failed: {:?}", output);

    // Check the token was saved
    assert!(oauth_path.exists(), "oauth.json should exist");

    let contents = fs::read_to_string(&oauth_path).unwrap();
    assert!(
        contents.contains("sk-ant-test-token-12345678901234567890"),
        "Token should be in oauth.json"
    );

    // Check output mentions success
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("Logged in to Anthropic"),
        "Should show success message"
    );
}

/// Test: logout --anthropic clears token from oauth.json.
#[test]
fn test_logout_clears_token() {
    let temp = tempdir().unwrap();
    let oauth_path = temp.path().join("oauth.json");

    // First, create an oauth.json with a token
    fs::write(
        &oauth_path,
        r#"{"anthropic": {"access_token": "sk-ant-test-token"}}"#,
    )
    .unwrap();

    Command::cargo_bin("zdx-cli")
        .unwrap()
        .env("ZDX_HOME", temp.path())
        .arg("logout")
        .arg("--anthropic")
        .assert()
        .success()
        .stdout(predicate::str::contains("Logged out from Anthropic"));

    // Check the token was removed
    let contents = fs::read_to_string(&oauth_path).unwrap();
    assert!(
        !contents.contains("sk-ant-test-token"),
        "Token should be removed from oauth.json"
    );
}

/// Test: login validates token format (rejects empty).
#[test]
fn test_login_rejects_empty_token() {
    let temp = tempdir().unwrap();

    let mut child = Command::cargo_bin("zdx-cli")
        .unwrap()
        .env("ZDX_HOME", temp.path())
        .env("ZDX_NO_BROWSER", "1")
        .arg("login")
        .arg("--anthropic")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("Failed to spawn command");

    // Write empty token
    {
        let stdin = child.stdin.as_mut().expect("Failed to open stdin");
        stdin.write_all(b"\n").expect("Failed to write to stdin");
    }

    let output = child.wait_with_output().expect("Failed to read output");
    assert!(!output.status.success(), "Should fail with empty token");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("empty") || stderr.contains("Token"),
        "Should mention token issue"
    );
}

/// Test: login validates token format (rejects short token).
#[test]
fn test_login_rejects_short_token() {
    let temp = tempdir().unwrap();

    let mut child = Command::cargo_bin("zdx-cli")
        .unwrap()
        .env("ZDX_HOME", temp.path())
        .env("ZDX_NO_BROWSER", "1")
        .arg("login")
        .arg("--anthropic")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("Failed to spawn command");

    {
        let stdin = child.stdin.as_mut().expect("Failed to open stdin");
        stdin.write_all(b"short\n").expect("Failed to write to stdin");
    }

    let output = child.wait_with_output().expect("Failed to read output");
    assert!(!output.status.success(), "Should fail with short token");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("short"),
        "Should mention token is too short"
    );
}

/// Test: oauth.json has restricted permissions on Unix.
#[cfg(unix)]
#[test]
fn test_oauth_file_permissions() {
    use std::os::unix::fs::PermissionsExt;

    let temp = tempdir().unwrap();
    let oauth_path = temp.path().join("oauth.json");

    // Login to create the file
    let mut child = Command::cargo_bin("zdx-cli")
        .unwrap()
        .env("ZDX_HOME", temp.path())
        .env("ZDX_NO_BROWSER", "1")
        .arg("login")
        .arg("--anthropic")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("Failed to spawn command");

    {
        let stdin = child.stdin.as_mut().expect("Failed to open stdin");
        stdin
            .write_all(b"sk-ant-test-token-12345678901234567890\n")
            .expect("Failed to write to stdin");
    }

    let output = child.wait_with_output().expect("Failed to read output");
    assert!(output.status.success(), "Command should succeed");

    // Check permissions
    let metadata = fs::metadata(&oauth_path).expect("Should be able to read metadata");
    let mode = metadata.permissions().mode();
    assert_eq!(
        mode & 0o777,
        0o600,
        "oauth.json should have 0600 permissions"
    );
}
