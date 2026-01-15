//! Integration tests for login/logout commands.

use std::fs;

use assert_cmd::cargo::cargo_bin_cmd;
use predicates::prelude::*;
use tempfile::tempdir;

#[test]
fn test_logout_requires_provider_flag() {
    cargo_bin_cmd!("zdx")
        .arg("logout")
        .assert()
        .failure()
        .stderr(predicate::str::contains("Please specify a provider"));
}

#[test]
fn test_login_requires_provider_flag() {
    cargo_bin_cmd!("zdx")
        .arg("login")
        .assert()
        .failure()
        .stderr(predicate::str::contains("Please specify a provider"));
}

#[test]
fn test_logout_when_not_logged_in() {
    let temp = tempdir().unwrap();

    cargo_bin_cmd!("zdx")
        .env("ZDX_HOME", temp.path())
        .arg("logout")
        .arg("--anthropic")
        .assert()
        .success()
        .stdout(predicate::str::contains("Anthropic uses API keys."));
}

#[test]
fn test_logout_clears_credentials() {
    let temp = tempdir().unwrap();
    let oauth_path = temp.path().join("oauth.json");

    // Create an oauth.json with credentials in the new format
    fs::write(
        &oauth_path,
        r#"{"claude-cli": {"type": "oauth", "refresh": "refresh-token", "access": "access-token", "expires": 9999999999999}}"#,
    )
    .unwrap();

    cargo_bin_cmd!("zdx")
        .env("ZDX_HOME", temp.path())
        .arg("logout")
        .arg("--claude-cli")
        .assert()
        .success()
        .stdout(predicate::str::contains("Logged out from Claude CLI"));

    // Check the credentials were removed
    let contents = fs::read_to_string(&oauth_path).unwrap();
    assert!(
        !contents.contains("access-token"),
        "Token should be removed from oauth.json"
    );
}

#[test]
fn test_login_shows_oauth_instructions() {
    let temp = tempdir().unwrap();

    // Start login but don't provide input - it will fail but we can check the output
    let output = cargo_bin_cmd!("zdx")
        .env("ZDX_HOME", temp.path())
        .env("ZDX_NO_BROWSER", "1")
        .arg("login")
        .arg("--claude-cli")
        .output()
        .expect("Failed to run command");

    let stdout = String::from_utf8_lossy(&output.stdout);

    // Should show OAuth flow instructions
    assert!(
        stdout.contains("OAuth") || stdout.contains("oauth"),
        "Should mention OAuth in instructions"
    );
    assert!(
        stdout.contains("claude.ai"),
        "Should show authorization URL"
    );
    assert!(stdout.contains("code"), "Should mention authorization code");
}

#[test]
fn test_login_prompts_when_already_logged_in() {
    let temp = tempdir().unwrap();
    let oauth_path = temp.path().join("oauth.json");

    // Create existing credentials
    fs::write(
        &oauth_path,
        r#"{"claude-cli": {"type": "oauth", "refresh": "refresh-token", "access": "access-token", "expires": 9999999999999}}"#,
    )
    .unwrap();

    // Run login without providing confirmation
    let output = cargo_bin_cmd!("zdx")
        .env("ZDX_HOME", temp.path())
        .env("ZDX_NO_BROWSER", "1")
        .arg("login")
        .arg("--claude-cli")
        .output()
        .expect("Failed to run command");

    let stdout = String::from_utf8_lossy(&output.stdout);

    // Should mention already logged in
    assert!(
        stdout.contains("Already logged in"),
        "Should mention already logged in"
    );
}

#[cfg(unix)]
#[test]
fn test_oauth_file_permissions_on_logout() {
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

    let temp = tempdir().unwrap();
    let oauth_path = temp.path().join("oauth.json");

    // Create credentials with proper permissions (simulating what login would do)
    {
        use std::io::Write;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&oauth_path)
            .unwrap();
        file.write_all(
            br#"{"claude-cli": {"type": "oauth", "refresh": "r", "access": "a", "expires": 0}}"#,
        )
        .unwrap();
    }

    // Logout triggers save which should preserve permissions
    cargo_bin_cmd!("zdx")
        .env("ZDX_HOME", temp.path())
        .arg("logout")
        .arg("--claude-cli")
        .assert()
        .success();

    // Check permissions are preserved
    let metadata = fs::metadata(&oauth_path).expect("Should be able to read metadata");
    let mode = metadata.permissions().mode();
    assert_eq!(
        mode & 0o777,
        0o600,
        "oauth.json should have 0600 permissions"
    );
}
