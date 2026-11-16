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
    assert_eq!(names, vec!["bot", "scheduler", "engine"]);

    for service in services {
        assert_eq!(service["status"], Value::String("stopped".to_string()));
        assert_eq!(service["installed"], Value::Bool(false));
        assert_eq!(service["pid"], Value::Null);
    }
    assert_eq!(services[0]["label"], "dev.zdx.bot");
    assert_eq!(services[1]["label"], "dev.zdx.scheduler");
    assert_eq!(services[2]["label"], "dev.zdx.engine");
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

/// A service still installed under its old launchd name counts as installed
/// for `status`, and `start` names the restart that moves it. The old name is
/// no target, and a restart queued under it is read as the new one. Nothing
/// here reaches `launchctl`.
#[test]
fn test_a_service_installed_under_its_old_name() {
    let dir = tempdir().unwrap();
    let home = dir.path();
    let agents = home.join("Library").join("LaunchAgents");
    std::fs::create_dir_all(&agents).unwrap();
    std::fs::write(agents.join("dev.zdx.daemon.plist"), "<plist></plist>").unwrap();
    std::fs::write(agents.join("dev.zdx.orchestrator.plist"), "<plist></plist>").unwrap();
    let zdx = |args: &[&str]| {
        let mut cmd = crate::fixtures::zdx_cmd();
        cmd.env("ZDX_HOME", home).env("HOME", home).args(args);
        cmd
    };

    let status = |zdx: &dyn Fn(&[&str]) -> assert_cmd::Command| -> Value {
        let output = zdx(&["service", "status", "--json"])
            .assert()
            .success()
            .get_output()
            .stdout
            .clone();
        serde_json::from_slice(&output).unwrap()
    };
    let parsed = status(&zdx);
    assert_eq!(parsed["services"][1]["service"], "scheduler");
    assert_eq!(parsed["services"][1]["installed"], true);
    assert_eq!(parsed["services"][2]["service"], "engine");
    assert_eq!(parsed["services"][2]["installed"], true);

    zdx(&["service", "start", "scheduler"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "still installed under its old name `daemon`; `zdx service restart scheduler` moves it",
        ));
    zdx(&["service", "stop", "engine"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "still installed under its old name `orchestrator`; `zdx service restart engine` moves it",
        ));
    zdx(&["service", "restart", "daemon"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("now called scheduler"));
    zdx(&["service", "restart", "orchestrator"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("now called engine"));

    supervised_bot(home);
    write_queue(home, &["bot", "daemon", "orchestrator"], None);
    assert_eq!(
        status(&zdx)["queued_restart"]["services"],
        serde_json::json!(["bot", "scheduler", "engine"])
    );
}

/// A restart moves a service off its old launchd name, and `uninstall`
/// removes the old install. `launchctl` here is a fake on a `PATH` that has
/// nothing else, so the real launchd is never reached: it records its
/// arguments and reports every job as unloaded.
#[cfg(unix)]
#[allow(clippy::too_many_lines)] // one migration checked end to end
#[test]
fn test_restart_moves_a_service_off_its_old_name() {
    use std::os::unix::fs::PermissionsExt as _;

    let dir = tempdir().unwrap();
    let home = dir.path().canonicalize().unwrap();
    let fake_bin = home.join("fake-bin");
    std::fs::create_dir_all(&fake_bin).unwrap();
    let calls = home.join("launchctl.log");
    let launchctl = fake_bin.join("launchctl");
    std::fs::write(
        &launchctl,
        format!(
            "#!/bin/sh\necho \"$@\" >> '{}'\n[ \"$1\" = print ] && exit 1\nexit 0\n",
            calls.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&launchctl, std::fs::Permissions::from_mode(0o755)).unwrap();
    let bin = home.join(".local").join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    std::fs::write(bin.join("zdx"), "").unwrap();
    let root = home.join("root");
    std::fs::create_dir_all(&root).unwrap();
    let agents = home.join("Library").join("LaunchAgents");
    std::fs::create_dir_all(&agents).unwrap();
    let old_plist = |name: &str, args: &str| {
        format!(
            "<plist><dict><key>Label</key><string>dev.zdx.{name}</string>\
             <key>ProgramArguments</key><array><string>/bin/zsh</string><string>-c</string>\
             <string>exec '{zdx}' '--root' '{root}' {args}</string></array>\
             <key>WorkingDirectory</key><string>{root}</string>\
             <key>EnvironmentVariables</key><dict><key>ZDX_HOME</key><string>{home}</string></dict>\
             </dict></plist>",
            zdx = bin.join("zdx").display(),
            root = root.display(),
            home = home.display(),
        )
    };
    std::fs::write(
        agents.join("dev.zdx.daemon.plist"),
        old_plist("daemon", "'jobs' 'daemon'"),
    )
    .unwrap();
    std::fs::write(
        agents.join("dev.zdx.orchestrator.plist"),
        old_plist("orchestrator", "'orchestrator' 'serve'"),
    )
    .unwrap();
    let logs = home.join("run").join("logs");
    std::fs::create_dir_all(&logs).unwrap();
    std::fs::write(logs.join("daemon.err"), "old scheduler log").unwrap();
    std::fs::write(logs.join("orchestrator.err"), "old engine log").unwrap();
    let zdx = |args: &[&str]| {
        let mut cmd = crate::fixtures::zdx_cmd();
        cmd.env("ZDX_HOME", &home)
            .env("HOME", &home)
            .env("PATH", &fake_bin)
            .args(args);
        cmd
    };

    zdx(&["service", "restart", "scheduler"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Moved daemon to scheduler and started it",
        ));
    assert!(!agents.join("dev.zdx.daemon.plist").exists());
    let plist = std::fs::read_to_string(agents.join("dev.zdx.scheduler.plist")).unwrap();
    assert!(
        plist.contains(&format!("'--root' '{}' 'jobs' 'scheduler'", root.display())),
        "{plist}"
    );
    assert_eq!(
        std::fs::read_to_string(logs.join("scheduler.err")).unwrap(),
        "old scheduler log"
    );

    zdx(&["service", "uninstall", "engine"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Uninstalled engine"));
    assert!(!agents.join("dev.zdx.orchestrator.plist").exists());
    assert!(!agents.join("dev.zdx.engine.plist").exists());
    assert_eq!(
        std::fs::read_to_string(logs.join("engine.err")).unwrap(),
        "old engine log"
    );

    let calls = std::fs::read_to_string(&calls).unwrap();
    // SAFETY: getuid has no preconditions and cannot fail.
    let uid = unsafe { libc::getuid() };
    let lines: Vec<&str> = calls
        .lines()
        .filter(|line| !line.starts_with("print "))
        .collect();
    assert_eq!(
        lines,
        [
            format!("bootout gui/{uid}/dev.zdx.daemon"),
            format!(
                "bootstrap gui/{uid} {}",
                agents.join("dev.zdx.scheduler.plist").display()
            ),
            format!("bootout gui/{uid}/dev.zdx.orchestrator"),
        ],
        "{calls}"
    );
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
        .args(["service", "restart", "scheduler", "--queue"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Restart queued for scheduler"));

    crate::fixtures::zdx_cmd()
        .env("ZDX_HOME", dir.path())
        .env("HOME", dir.path())
        .env_remove("ZDX_THREAD_ID")
        .args(["service", "restart", "bot", "--queue"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "already queued; it now covers bot, scheduler",
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
    assert_eq!(queued["services"], serde_json::json!(["bot", "scheduler"]));
    assert_eq!(queued["thread_id"], "telegram--100-topic-7");
    assert!(queued["requested_at"].is_string());
}

fn write_queue(home: &std::path::Path, services: &[&str], thread_id: Option<&str>) {
    let run = home.join("run");
    std::fs::create_dir_all(&run).unwrap();
    // Later than the PID files written below, so the request is unfulfilled.
    let requested_at = chrono::Utc::now() + chrono::Duration::seconds(60);
    std::fs::write(
        run.join("restart-queue.json"),
        serde_json::to_vec(&serde_json::json!({
            "services": services,
            "requested_at": requested_at.to_rfc3339(),
            "thread_id": thread_id,
        }))
        .unwrap(),
    )
    .unwrap();
}

/// The bot's PID file and supervision marker, owned by this live test process.
fn supervised_bot(home: &std::path::Path) {
    let run = home.join("run");
    std::fs::create_dir_all(&run).unwrap();
    let pid = std::process::id().to_string();
    std::fs::write(run.join("bot.pid"), &pid).unwrap();
    std::fs::write(run.join("bot.supervised"), &pid).unwrap();
}

fn active_agent(home: &std::path::Path) {
    let agents_dir = home.join("run/agents");
    std::fs::create_dir_all(&agents_dir).unwrap();
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
}

fn run_queued(home: &std::path::Path) -> Value {
    let output = crate::fixtures::zdx_cmd()
        .env("ZDX_HOME", home)
        .env("HOME", home)
        .args(["service", "run-queued-restart", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    serde_json::from_slice(&output).unwrap()
}

/// The queued-restart step: nothing pending, waiting behind an agent run,
/// the bot target reported as `restart_bot` (never restarted here), dropped
/// without a supervisor, and a failed peer restart. No case reaches
/// `launchctl`: nothing is installed in the temp home.
#[test]
fn test_run_queued_restart_outcomes() {
    let dir = tempdir().unwrap();
    let home = dir.path();
    let queue = home.join("run/restart-queue.json");

    assert_eq!(run_queued(home)["status"], "none");

    supervised_bot(home);
    write_queue(home, &["bot"], Some("telegram--100-topic-7"));
    active_agent(home);
    let waiting = run_queued(home);
    assert_eq!(waiting["status"], "waiting");
    assert_eq!(waiting["restart_bot"], false);
    assert!(queue.is_file(), "a waiting request stays queued");
    std::fs::remove_dir_all(home.join("run/agents")).unwrap();

    let done = run_queued(home);
    assert_eq!(done["status"], "done");
    assert_eq!(done["restart_bot"], true);
    assert_eq!(done["thread_id"], "telegram--100-topic-7");
    assert!(
        done["message"].as_str().unwrap().contains("exiting bot"),
        "{done}"
    );
    assert!(!queue.exists(), "a done request is cleared");

    // A bot plist from before zdx-go: exiting would restart the old command.
    let agents = home.join("Library/LaunchAgents");
    std::fs::create_dir_all(&agents).unwrap();
    let plist = agents.join("dev.zdx.bot.plist");
    std::fs::write(
        &plist,
        "<plist><dict><key>ProgramArguments</key><array><string>/bin/zsh</string><string>-c</string>\
         <string>exec '/x/.local/bin/zdx-gateway' '--root' '/r' 'bot'</string></array>\
         <key>WorkingDirectory</key><string>/r</string></dict></plist>",
    )
    .unwrap();
    write_queue(home, &["bot"], None);
    let outdated = run_queued(home);
    assert_eq!(outdated["status"], "done");
    assert_eq!(outdated["restart_bot"], false, "{outdated}");
    assert!(
        outdated["message"]
            .as_str()
            .unwrap()
            .contains("zdx service restart bot"),
        "{outdated}"
    );
    assert!(!queue.exists());
    std::fs::remove_file(&plist).unwrap();

    std::fs::remove_file(home.join("run/bot.supervised")).unwrap();
    write_queue(home, &["bot"], None);
    let dropped = run_queued(home);
    assert_eq!(dropped["status"], "dropped");
    assert_eq!(dropped["restart_bot"], false);
    assert!(
        dropped["message"]
            .as_str()
            .unwrap()
            .contains("no active supervisor"),
        "{dropped}"
    );
    assert!(!queue.exists());

    write_queue(home, &["scheduler"], None);
    let failed = run_queued(home);
    assert_eq!(failed["status"], "failed");
    assert_eq!(failed["restart_bot"], false);
    assert!(
        failed["message"]
            .as_str()
            .unwrap()
            .contains("not installed"),
        "{failed}"
    );
    assert!(!queue.exists());

    std::fs::write(&queue, "not json").unwrap();
    assert_eq!(run_queued(home)["status"], "dropped");
    assert!(!queue.exists());
}

/// `restart-peers --json`: a gate refusal is a successful `blocked` answer;
/// a failed restart (here: not installed) is an error.
#[test]
fn test_restart_peers_json() {
    let dir = tempdir().unwrap();
    let home = dir.path();
    crate::fixtures::zdx_cmd()
        .env("ZDX_HOME", home)
        .env("HOME", home)
        .args(["service", "restart-peers", "--json"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("not installed"));

    active_agent(home);
    let output = crate::fixtures::zdx_cmd()
        .env("ZDX_HOME", home)
        .env("HOME", home)
        .args(["service", "restart-peers", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let blocked: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(blocked["outcome"], "blocked");
    assert!(
        blocked["text"]
            .as_str()
            .unwrap()
            .contains("restart blocked"),
        "{blocked}"
    );
}

/// `restart --queue --thread ID --json` records the announcement thread and
/// reports a merge into a pending request.
#[test]
fn test_queue_restart_json_with_thread() {
    let dir = tempdir().unwrap();
    let home = dir.path();
    let queue = |thread: &str| {
        let output = crate::fixtures::zdx_cmd()
            .env("ZDX_HOME", home)
            .env("HOME", home)
            .env_remove("ZDX_THREAD_ID")
            .args([
                "service", "restart", "all", "--queue", "--json", "--thread", thread,
            ])
            .assert()
            .success()
            .get_output()
            .stdout
            .clone();
        serde_json::from_slice::<Value>(&output).unwrap()
    };
    crate::fixtures::zdx_cmd()
        .env("ZDX_HOME", home)
        .env("HOME", home)
        .args(["service", "restart", "all", "--queue", "--json"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("not running under a supervisor"));

    supervised_bot(home);
    let first = queue("telegram--100-topic-7");
    assert_eq!(first["already_pending"], false);
    assert_eq!(first["active_runs"], 0);
    assert_eq!(first["request"]["thread_id"], "telegram--100-topic-7");
    assert_eq!(
        first["request"]["services"],
        serde_json::json!(["bot", "scheduler", "engine"])
    );
    let second = queue("telegram--100-topic-8");
    assert_eq!(second["already_pending"], true);
    assert_eq!(second["request"]["thread_id"], "telegram--100-topic-8");
}

/// A bot program other than `zdx` must pass its read-only preflight before
/// it replaces the bot service. Every refusal here happens before launchd is
/// touched or a plist is written.
#[cfg(unix)]
#[test]
fn test_install_bot_program_requires_a_ready_preflight() {
    use std::os::unix::fs::PermissionsExt as _;

    let dir = tempdir().unwrap();
    let home = dir.path();
    let program = home.join("zdx-go-stub");
    let install = |script: &str| {
        std::fs::write(&program, format!("#!/bin/sh\n{script}\n")).unwrap();
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();
        crate::fixtures::zdx_cmd()
            .env("ZDX_HOME", home)
            .env("HOME", home)
            .args(["--root", home.to_str().unwrap()])
            .args([
                "service",
                "install",
                "bot",
                "--program",
                program.to_str().unwrap(),
            ])
            .assert()
            .failure()
            .get_output()
            .stderr
            .clone()
    };
    let stderr = |bytes: Vec<u8>| String::from_utf8_lossy(&bytes).into_owned();

    let not_ready = stderr(install(
        r#"echo '{"service":"bot","ready":false,"problems":["missing feature"],"warnings":[]}'"#,
    ));
    assert!(not_ready.contains("missing feature"), "{not_ready}");
    let failed = stderr(install("exit 2"));
    assert!(failed.contains("preflight failed"), "{failed}");
    let garbage = stderr(install("echo hello"));
    assert!(garbage.contains("no valid report"), "{garbage}");
    let args = stderr(install(
        r#"[ "$1" = gateway ] && [ "$2" = preflight ] && [ "$5" = --json ] && [ "${ZDX_BIN##*/}" = zdx ] || exit 3; echo '{"service":"scheduler","ready":true}'"#,
    ));
    assert!(args.contains("not bot"), "{args}");
    assert!(
        !home.join("Library/LaunchAgents/dev.zdx.bot.plist").exists(),
        "no plist written"
    );
}

/// The bot service runs the gateway: without `--program` it is
/// `~/.local/bin/zdx-go`, a `zdx` bot program is refused, and
/// `--program` names one service only. Every program is checked before any
/// service is installed, so these refusals write no plist and never reach
/// launchd.
#[cfg(unix)]
#[test]
fn test_install_checks_every_program_before_installing() {
    let dir = tempdir().unwrap();
    let home = dir.path();
    let install = |args: &[&str]| {
        let output = crate::fixtures::zdx_cmd()
            .env("ZDX_HOME", home)
            .env("HOME", home)
            .args(["--root", home.to_str().unwrap(), "service", "install"])
            .args(args)
            .assert()
            .failure()
            .get_output()
            .stderr
            .clone();
        String::from_utf8_lossy(&output).into_owned()
    };

    let all = install(&[]);
    assert!(all.contains(".local/bin/zdx-go not found"), "{all}");
    assert!(all.contains("just go-install"), "{all}");
    let bot = install(&["bot"]);
    assert!(bot.contains("just go-install"), "{bot}");

    let zdx = home.join("zdx");
    std::fs::write(&zdx, "").unwrap();
    let refused = install(&["bot", "--program", zdx.to_str().unwrap()]);
    assert!(
        refused.contains("runs the Go gateway, not zdx"),
        "{refused}"
    );

    let several = install(&["all", "--program", zdx.to_str().unwrap()]);
    assert!(
        several.contains("--program sets one service's program"),
        "{several}"
    );

    assert!(
        !home.join("Library/LaunchAgents").exists(),
        "no plist written"
    );
}
