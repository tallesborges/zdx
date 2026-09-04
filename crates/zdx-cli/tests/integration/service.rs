use predicates::prelude::*;
use serde_json::Value;
use tempfile::tempdir;

/// On a clean ZDX home with no launchd agents installed, `zdx service status
/// --json` reports both services as stopped and not installed, without touching
/// launchd.
#[test]
fn test_service_status_json_reports_stopped_and_not_installed() {
    let dir = tempdir().unwrap();

    let output = crate::fixtures::zdx_cmd()
        .env("ZDX_HOME", dir.path())
        .env("HOME", dir.path())
        .args(["service", "status", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let parsed: Value = serde_json::from_slice(&output).unwrap();
    let services = parsed["services"].as_array().expect("services array");
    assert_eq!(services.len(), 2);

    let names: Vec<&str> = services
        .iter()
        .filter_map(|s| s["service"].as_str())
        .collect();
    assert_eq!(names, vec!["bot", "daemon"]);

    for service in services {
        assert_eq!(service["status"], Value::String("stopped".to_string()));
        assert_eq!(service["installed"], Value::Bool(false));
        assert_eq!(service["pid"], Value::Null);
    }
    assert_eq!(services[0]["label"], "dev.zdx.bot");
    assert_eq!(services[1]["label"], "dev.zdx.daemon");
}

/// Lifecycle commands reject unknown service targets instead of guessing.
#[test]
fn test_service_rejects_unknown_target() {
    let dir = tempdir().unwrap();

    crate::fixtures::zdx_cmd()
        .env("ZDX_HOME", dir.path())
        .env("HOME", dir.path())
        .args(["service", "restart", "monitor"])
        .assert()
        .failure();
}

/// Control commands refuse to act when the launchd agent was never installed.
#[test]
fn test_service_restart_requires_install() {
    let dir = tempdir().unwrap();

    crate::fixtures::zdx_cmd()
        .env("ZDX_HOME", dir.path())
        .env("HOME", dir.path())
        .args(["service", "restart", "bot"])
        .assert()
        .failure();
}

#[test]
fn test_service_restart_help_lists_force_flag() {
    crate::fixtures::zdx_cmd()
        .args(["service", "restart", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("--force"));
}

#[test]
fn test_service_restart_blocks_active_agent_unless_forced() {
    let dir = tempdir().unwrap();
    let agents_dir = dir.path().join("run/agents");
    std::fs::create_dir_all(&agents_dir).unwrap();
    std::fs::write(
        agents_dir.join("active.json"),
        serde_json::to_vec(&serde_json::json!({
            "pid": std::process::id(),
            "started_at": "2026-01-01T00:00:00Z",
            "thread_id": "test-thread",
            "surface": "test",
            "model": "test-model"
        }))
        .unwrap(),
    )
    .unwrap();

    crate::fixtures::zdx_cmd()
        .env("ZDX_HOME", dir.path())
        .env("HOME", dir.path())
        .args(["service", "restart", "bot"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("restart blocked"))
        .stderr(predicate::str::contains("--force"));

    crate::fixtures::zdx_cmd()
        .env("ZDX_HOME", dir.path())
        .env("HOME", dir.path())
        .args(["service", "restart", "bot", "--force"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("not installed"));
}
