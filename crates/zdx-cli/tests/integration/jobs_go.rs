//! `zdx jobs` starts the Go jobs program in its own place (exec), except for
//! `history` and the internal `attempt` and `resolve`. The program gets
//! `jobs --root ROOT`, the global `--system-prompt`, `--no-thread`, and
//! `--thread`, then the subcommand and its flags; this `ZDX_HOME`; and this
//! binary as `ZDX_BIN`. Its exit status is the command's. These tests use a
//! fake program; the opt-in test at the end runs a built `zdx-go` named by
//! `ZDX_GO_TEST_BIN` against a mock provider.

#![cfg(unix)]

use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use tempfile::{TempDir, tempdir};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer};

use crate::fixtures::{MOCK_MODEL, sse_response, text_sse};

struct Sandbox {
    _dir: TempDir,
    home: PathBuf,
    zdx_home: PathBuf,
    root: PathBuf,
    out: PathBuf,
}

fn sandbox() -> Sandbox {
    let dir = tempdir().unwrap();
    let base = dir.path().canonicalize().unwrap();
    let home = base.join("home");
    let zdx_home = base.join("zdx-home");
    let root = base.join("project");
    for path in [&home, &zdx_home, &root] {
        std::fs::create_dir_all(path).unwrap();
    }
    Sandbox {
        out: base.join("jobs.out"),
        _dir: dir,
        home,
        zdx_home,
        root,
    }
}

/// `zdx <globals> jobs <args>` with only a temp home and a minimal PATH.
fn zdx_jobs(sb: &Sandbox, program: Option<&Path>, globals: &[&str], args: &[&str]) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_zdx"));
    cmd.env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", &sb.home)
        .env("ZDX_HOME", &sb.zdx_home)
        .current_dir(&sb.root)
        .args(["--root", sb.root.to_str().unwrap()])
        .args(globals)
        .arg("jobs")
        .args(args)
        .stdin(Stdio::null());
    if let Some(program) = program {
        cmd.env("ZDX_GO", program);
    }
    cmd
}

/// A fake jobs program that records its arguments and environment, then
/// exits with `code`.
fn fake_program(sb: &Sandbox, code: i32) -> PathBuf {
    let program = sb.home.join("fake-jobs");
    std::fs::write(
        &program,
        format!(
            "#!/bin/sh\n{{ for a in \"$@\"; do echo \"arg=$a\"; done; echo \"home=$ZDX_HOME\"; echo \"bin=$ZDX_BIN\"; }} > '{}'\nexit {code}\n",
            sb.out.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();
    program
}

fn recorded_args(sb: &Sandbox) -> Vec<String> {
    std::fs::read_to_string(&sb.out)
        .unwrap()
        .lines()
        .filter_map(|line| line.strip_prefix("arg="))
        .map(str::to_string)
        .collect()
}

#[test]
fn test_jobs_subcommands_exec_the_go_program_with_their_flags() {
    let sb = sandbox();
    let program = fake_program(&sb, 0);
    let root = sb.root.to_str().unwrap().to_string();
    let cases: [(&[&str], &[&str], Vec<&str>); 6] = [
        (
            &[],
            &["list", "--json", "--daemon"],
            vec!["list", "--json", "--daemon"],
        ),
        (&[], &["validate"], vec!["validate"]),
        (
            &["--system-prompt", "Be brief.", "--no-thread"],
            &["daemon", "--poll-interval-secs", "5"],
            vec!["daemon", "--poll-interval-secs", "5"],
        ),
        (
            &["--thread", "custom"],
            &["run", "daily", "--keep"],
            vec!["run", "daily", "--keep"],
        ),
        (
            &[],
            &["runs", "daily", "--date-start", "2026-10-01", "--json"],
            vec!["runs", "daily", "--date-start", "2026-10-01", "--json"],
        ),
        (
            &[],
            &["set-model", "daily", "--clear"],
            vec!["set-model", "daily", "--clear"],
        ),
    ];
    for (globals, args, expected) in cases {
        let status = zdx_jobs(&sb, Some(&program), globals, args)
            .status()
            .unwrap();
        assert!(status.success(), "{args:?}");
        let mut want = vec!["jobs".to_string(), "--root".to_string(), root.clone()];
        let mut rest = globals.iter();
        while let Some(flag) = rest.next() {
            want.push((*flag).to_string());
            if *flag != "--no-thread" {
                want.push((*rest.next().unwrap()).to_string());
            }
        }
        want.extend(expected.iter().map(|arg| (*arg).to_string()));
        assert_eq!(recorded_args(&sb), want, "{args:?}");
    }
    let out = std::fs::read_to_string(&sb.out).unwrap();
    assert!(
        out.contains(&format!("home={}\n", sb.zdx_home.display())),
        "{out}"
    );
    let zdx = PathBuf::from(env!("CARGO_BIN_EXE_zdx"))
        .canonicalize()
        .unwrap();
    assert!(out.contains(&format!("bin={}\n", zdx.display())), "{out}");
}

#[test]
fn test_jobs_keep_the_go_programs_exit_status() {
    let sb = sandbox();
    let program = fake_program(&sb, 75);
    let status = zdx_jobs(&sb, Some(&program), &[], &["run", "busy"])
        .status()
        .unwrap();
    assert_eq!(status.code(), Some(75), "a busy job keeps exit code 75");
}

#[test]
fn test_jobs_fail_closed_without_a_usable_program() {
    let sb = sandbox();
    let output = zdx_jobs(&sb, None, &[], &["list"]).output().unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("zdx-go not found"), "{stderr}");
    assert!(stderr.contains("just go-install"), "{stderr}");

    let relative = zdx_jobs(&sb, Some(Path::new("zdx-go")), &[], &["list"])
        .output()
        .unwrap();
    assert!(String::from_utf8_lossy(&relative.stderr).contains("must be an absolute path"));
}

#[test]
fn test_jobs_history_stays_in_rust() {
    let sb = sandbox();
    let program = fake_program(&sb, 9);
    let output = zdx_jobs(&sb, Some(&program), &[], &["history", "missing"])
        .output()
        .unwrap();
    assert!(output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("No earlier recorded run of `missing`")
    );
    assert!(!sb.out.exists(), "history does not run the jobs program");
}

/// Opt-in: with `ZDX_GO_TEST_BIN` naming a built `zdx-go`, a manual run
/// of a once job goes through the Go bookkeeping and the Rust attempt: it
/// consumes the occurrence, archives the file, and records one attempt.
#[tokio::test]
async fn test_real_jobs_program_runs_a_once_job_through_rust() {
    let Some(bin) = std::env::var_os("ZDX_GO_TEST_BIN").filter(|bin| !bin.is_empty()) else {
        eprintln!("skipped: ZDX_GO_TEST_BIN not set");
        return;
    };
    let sb = sandbox();
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(sse_response(&text_sse("hello")))
        .expect(1)
        .mount(&server)
        .await;
    let jobs_dir = sb.zdx_home.join("jobs");
    std::fs::create_dir_all(&jobs_dir).unwrap();
    std::fs::write(
        jobs_dir.join("remind.md"),
        format!(
            "---\nmodel: \"{MOCK_MODEL}\"\nrun_at: \"2030-01-02T09:00:00-03:00\"\n---\nSay hello.\n"
        ),
    )
    .unwrap();

    let output = zdx_jobs(
        &sb,
        Some(Path::new(&bin)),
        &["--no-thread"],
        &["run", "remind"],
    )
    .env("ANTHROPIC_API_KEY", "test-api-key")
    .env("ANTHROPIC_BASE_URL", server.uri())
    .output()
    .unwrap();
    assert!(output.status.success(), "{output:?}");
    assert!(String::from_utf8_lossy(&output.stdout).contains("Archived 'remind'"));
    assert!(
        jobs_dir
            .join("archive/remind--20300102T120000Z.md")
            .exists()
    );
    let claim: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(
            sb.zdx_home
                .join("state/jobs/remind/claims/20300102T120000Z.json"),
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(claim["outcome"], "succeeded");
    let log = std::fs::read_to_string(sb.zdx_home.join("jobs_runs.jsonl")).unwrap();
    let record: serde_json::Value = serde_json::from_str(log.lines().next().unwrap()).unwrap();
    assert_eq!(record["outcome"], "succeeded");
    assert_eq!(record["model"], format!("{MOCK_MODEL}@off"));
    assert_eq!(record["run_id"], claim["run_id"]);
}

/// Opt-in: with `ZDX_GO_TEST_BIN` naming a built `zdx-go`, `zdx jobs
/// daemon` runs the Go scheduler: it runs a due once job through a run child
/// and a Rust attempt, marks a run that died with the previous daemon as
/// interrupted, and removes its PID file and context record when stopped.
#[tokio::test]
async fn test_real_jobs_daemon_runs_and_recovers_once_jobs() {
    let Some(bin) = std::env::var_os("ZDX_GO_TEST_BIN").filter(|bin| !bin.is_empty()) else {
        eprintln!("skipped: ZDX_GO_TEST_BIN not set");
        return;
    };
    let sb = sandbox();
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(sse_response(&text_sse("hello")))
        .expect(1)
        .mount(&server)
        .await;
    let jobs_dir = sb.zdx_home.join("jobs");
    std::fs::create_dir_all(&jobs_dir).unwrap();
    let now = chrono::Utc::now();
    let soon = now + chrono::TimeDelta::seconds(2);
    let crashed = now - chrono::TimeDelta::minutes(5);
    let rfc =
        |at: chrono::DateTime<chrono::Utc>| at.to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let key = |at: chrono::DateTime<chrono::Utc>| at.format("%Y%m%dT%H%M%SZ").to_string();
    for (name, at) in [("remind", soon), ("crashed", crashed)] {
        std::fs::write(
            jobs_dir.join(format!("{name}.md")),
            format!(
                "---\nmodel: \"{MOCK_MODEL}\"\nrun_at: \"{}\"\n---\nSay hello.\n",
                rfc(at)
            ),
        )
        .unwrap();
    }
    let claims = sb.zdx_home.join("state/jobs/crashed/claims");
    std::fs::create_dir_all(&claims).unwrap();
    std::fs::write(
        claims.join(format!("{}.json", key(crashed))),
        serde_json::json!({
            "job": "crashed", "key": key(crashed), "run_id": "run-from-a-dead-daemon",
            "trigger": "daemon", "schedule_kind": "once", "scheduled_for": rfc(crashed),
            "claimed_at": rfc(crashed), "pid": 1, "outcome": null, "finished_at": null,
            "error": null, "thread_id": null,
        })
        .to_string(),
    )
    .unwrap();

    let log = std::fs::File::create(sb.home.join("daemon.log")).unwrap();
    let mut daemon = zdx_jobs(
        &sb,
        Some(Path::new(&bin)),
        &["--no-thread"],
        &["daemon", "--poll-interval-secs", "1"],
    )
    .env("ANTHROPIC_API_KEY", "test-api-key")
    .env("ANTHROPIC_BASE_URL", server.uri())
    .stdout(log.try_clone().unwrap())
    .stderr(log)
    .spawn()
    .unwrap();
    let done = jobs_dir.join(format!("archive/remind--{}.md", key(soon)));
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(45);
    while !done.exists() && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
    let run = sb.zdx_home.join("run");
    assert!(run.join("daemon.pid").exists() && run.join("daemon.context.json").exists());
    // SAFETY: `daemon.id()` is the child spawned above, still running.
    unsafe { libc::kill(daemon.id() as i32, libc::SIGTERM) };
    let status = daemon.wait().unwrap();
    let log = std::fs::read_to_string(sb.home.join("daemon.log")).unwrap();
    assert!(done.exists(), "once job was not run and archived:\n{log}");
    assert!(status.success(), "{status}\n{log}");

    let claim = |job: &str, at| -> serde_json::Value {
        let path = sb
            .zdx_home
            .join(format!("state/jobs/{job}/claims/{}.json", key(at)));
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
    };
    assert_eq!(claim("remind", soon)["outcome"], "succeeded");
    assert_eq!(claim("crashed", crashed)["outcome"], "interrupted");
    assert!(
        jobs_dir
            .join(format!("archive/crashed--{}.md", key(crashed)))
            .exists()
    );
    assert!(!run.join("daemon.pid").exists(), "{log}");
    assert!(!run.join("daemon.context.json").exists(), "{log}");
}
