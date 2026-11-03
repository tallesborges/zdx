use predicates::prelude::*;
use serde_json::Value;
use tempfile::tempdir;

/// On a clean ZDX home with no launchd agents installed, `zdx service status
/// --json` reports every service as stopped and not installed, without
/// touching launchd.
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
    assert_eq!(services.len(), 3);

    let names: Vec<&str> = services
        .iter()
        .filter_map(|s| s["service"].as_str())
        .collect();
    assert_eq!(names, vec!["bot", "daemon", "orchestrator"]);

    for service in services {
        assert_eq!(service["status"], Value::String("stopped".to_string()));
        assert_eq!(service["installed"], Value::Bool(false));
        assert_eq!(service["pid"], Value::Null);
    }
    assert_eq!(services[0]["label"], "dev.zdx.bot");
    assert_eq!(services[1]["label"], "dev.zdx.daemon");
    assert_eq!(services[2]["label"], "dev.zdx.orchestrator");
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
    // A marker this live test process could have written: it started before
    // `started_at`. One dated before the process started would belong to an
    // earlier owner of the PID and be ignored as stale.
    std::fs::write(
        agents_dir.join("active.json"),
        serde_json::to_vec(&serde_json::json!({
            "pid": std::process::id(),
            "started_at": chrono::Utc::now().to_rfc3339(),
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
        .stderr(predicate::str::contains("--queue"))
        .stderr(predicate::str::contains("--force"));

    crate::fixtures::zdx_cmd()
        .env("ZDX_HOME", dir.path())
        .env("HOME", dir.path())
        .args(["service", "restart", "bot", "--force"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("not installed"));
}

/// `restart all` checks that every service is installed before it restarts
/// any, and names the install command for the missing one.
#[test]
fn test_service_restart_all_names_the_missing_install() {
    let dir = tempdir().unwrap();

    crate::fixtures::zdx_cmd()
        .env("ZDX_HOME", dir.path())
        .env("HOME", dir.path())
        .args(["service", "restart", "all", "--force"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "run `zdx service install bot` first",
        ));
}

/// Writes PID and supervision markers for a "bot" that is this test process,
/// so `--queue` sees a live supervised bot without launchd.
fn fake_supervised_bot(home: &std::path::Path) {
    let run = home.join("run");
    std::fs::create_dir_all(&run).unwrap();
    let pid = std::process::id().to_string();
    std::fs::write(run.join("bot.pid"), &pid).unwrap();
    std::fs::write(run.join("bot.supervised"), &pid).unwrap();
}

#[test]
fn test_service_restart_queue_requires_running_bot() {
    let dir = tempdir().unwrap();

    crate::fixtures::zdx_cmd()
        .env("ZDX_HOME", dir.path())
        .env("HOME", dir.path())
        .args(["service", "restart", "--queue"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("bot service is not running"));
    assert!(!dir.path().join("run/restart-queue.json").exists());
}

#[test]
fn test_service_restart_queue_rejects_force() {
    crate::fixtures::zdx_cmd()
        .args(["service", "restart", "--queue", "--force"])
        .assert()
        .failure();
}

/// `--queue` records one request for the bot to perform, merges a second
/// request into it, keeps the requesting thread for the announcement, and
/// reports it through `service status --json`.
#[test]
fn test_service_restart_queue_records_and_merges_request() {
    let dir = tempdir().unwrap();
    fake_supervised_bot(dir.path());

    crate::fixtures::zdx_cmd()
        .env("ZDX_HOME", dir.path())
        .env("HOME", dir.path())
        .env("ZDX_THREAD_ID", "telegram--100-topic-7")
        .args(["service", "restart", "daemon", "--queue"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Restart queued for daemon"));

    crate::fixtures::zdx_cmd()
        .env("ZDX_HOME", dir.path())
        .env("HOME", dir.path())
        .env_remove("ZDX_THREAD_ID")
        .args(["service", "restart", "bot", "--queue"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "already queued; it now covers bot, daemon",
        ));

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
    let queued = &parsed["queued_restart"];
    assert_eq!(queued["services"], serde_json::json!(["bot", "daemon"]));
    assert_eq!(queued["thread_id"], "telegram--100-topic-7");
    assert!(queued["requested_at"].is_string());
}
