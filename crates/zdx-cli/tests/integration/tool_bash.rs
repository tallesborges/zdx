//! Integration tests for the bash tool.
//!
//! Verifies that the bash tool executes commands and captures output correctly.

use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use tempfile::TempDir;
use tokio::time::{Duration, timeout};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request};

use crate::fixtures;
use crate::fixtures::{MOCK_MODEL, sse_response, tool_use_sse};

/// Creates a temp `ZDX_HOME` directory for test isolation.
fn temp_zdx_home() -> TempDir {
    TempDir::new().expect("create temp zdx home")
}

fn can_bind_localhost() -> bool {
    std::net::TcpListener::bind("127.0.0.1:0").is_ok()
}

#[tokio::test]
async fn test_bash_executes_command() {
    if !can_bind_localhost() {
        eprintln!("Skipping: cannot bind localhost TCP port in this environment.");
        return;
    }
    let zdx_home = temp_zdx_home();
    let temp_dir = TempDir::new().unwrap();
    let mock_server = MockServer::start().await;

    let call_count = Arc::new(AtomicUsize::new(0));
    let call_count_clone = Arc::clone(&call_count);
    let second_request_body = Arc::new(std::sync::Mutex::new(String::new()));
    let second_request_body_clone = Arc::clone(&second_request_body);

    let first_response = tool_use_sse(
        "toolu_bash_001",
        "bash",
        r#"{"command": "echo hello_from_bash"}"#,
    );
    let second_response = fixtures::text_sse("Bash executed successfully.");

    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(move |req: &Request| {
            let count = call_count_clone.fetch_add(1, Ordering::SeqCst);
            if count == 0 {
                sse_response(&first_response)
            } else {
                let body = String::from_utf8_lossy(&req.body).to_string();
                *second_request_body_clone.lock().unwrap() = body;
                sse_response(&second_response)
            }
        })
        .expect(2)
        .mount(&mock_server)
        .await;

    crate::fixtures::zdx_cmd()
        .env("ZDX_HOME", zdx_home.path())
        .env("ANTHROPIC_API_KEY", "test-api-key")
        .env("ANTHROPIC_BASE_URL", mock_server.uri())
        .args([
            "--root",
            temp_dir.path().to_str().unwrap(),
            "--no-thread",
            "exec",
            "-m",
            MOCK_MODEL,
            "-p",
            "Run echo hello",
        ])
        .assert()
        .success();

    let body = second_request_body.lock().unwrap().clone();
    assert!(
        body.contains("hello_from_bash"),
        "Tool result should contain command output. Got: {body}"
    );
    // New structured envelope format (escaped in JSON content):
    // {"ok":true,"data":{"stdout":"...","exit_code":0,...}}
    assert!(
        body.contains(r#"\"exit_code\":0"#),
        "Tool result should contain exit_code in escaped JSON format. Got: {body}"
    );
    assert!(
        body.contains(r#"\"ok\":true"#),
        "Tool result should use structured envelope format. Got: {body}"
    );
}

#[tokio::test]
async fn test_bash_runs_in_root_directory() {
    if !can_bind_localhost() {
        eprintln!("Skipping: cannot bind localhost TCP port in this environment.");
        return;
    }
    let zdx_home = temp_zdx_home();
    let temp_dir = TempDir::new().unwrap();
    std::fs::write(temp_dir.path().join("marker.txt"), "marker content").unwrap();

    let mock_server = MockServer::start().await;

    let call_count = Arc::new(AtomicUsize::new(0));
    let call_count_clone = Arc::clone(&call_count);
    let second_request_body = Arc::new(std::sync::Mutex::new(String::new()));
    let second_request_body_clone = Arc::clone(&second_request_body);

    let first_response = tool_use_sse("toolu_bash_002", "bash", r#"{"command": "ls"}"#);
    let second_response = fixtures::text_sse("Listed files.");

    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(move |req: &Request| {
            let count = call_count_clone.fetch_add(1, Ordering::SeqCst);
            if count == 0 {
                sse_response(&first_response)
            } else {
                let body = String::from_utf8_lossy(&req.body).to_string();
                *second_request_body_clone.lock().unwrap() = body;
                sse_response(&second_response)
            }
        })
        .expect(2)
        .mount(&mock_server)
        .await;

    crate::fixtures::zdx_cmd()
        .env("ZDX_HOME", zdx_home.path())
        .env("ANTHROPIC_API_KEY", "test-api-key")
        .env("ANTHROPIC_BASE_URL", mock_server.uri())
        .args([
            "--root",
            temp_dir.path().to_str().unwrap(),
            "--no-thread",
            "exec",
            "-m",
            MOCK_MODEL,
            "-p",
            "List files",
        ])
        .assert()
        .success();

    let body = second_request_body.lock().unwrap().clone();
    assert!(
        body.contains("marker.txt"),
        "ls should show marker.txt from root dir. Got: {body}"
    );
}

#[tokio::test]
async fn test_bash_times_out_with_per_call_timeout() {
    if !can_bind_localhost() {
        eprintln!("Skipping: cannot bind localhost TCP port in this environment.");
        return;
    }
    let temp_dir = TempDir::new().unwrap();
    let zdx_home = temp_zdx_home();

    let mock_server = MockServer::start().await;

    let call_count = Arc::new(AtomicUsize::new(0));
    let call_count_clone = Arc::clone(&call_count);
    let second_request_body = Arc::new(std::sync::Mutex::new(String::new()));
    let second_request_body_clone = Arc::clone(&second_request_body);

    // The per-call `timeout_secs` is the explicit kill deadline: the command is
    // killed at the bound and the group torn down, rather than relocated.
    let first_response = tool_use_sse(
        "toolu_bash_timeout",
        "bash",
        r#"{"command": "sleep 2", "timeout_secs": 1}"#,
    );
    let second_response = fixtures::text_sse("Done.");

    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(move |req: &Request| {
            let count = call_count_clone.fetch_add(1, Ordering::SeqCst);
            if count == 0 {
                sse_response(&first_response)
            } else {
                let body = String::from_utf8_lossy(&req.body).to_string();
                *second_request_body_clone.lock().unwrap() = body;
                sse_response(&second_response)
            }
        })
        .expect(2)
        .mount(&mock_server)
        .await;

    crate::fixtures::zdx_cmd()
        .env("ZDX_HOME", zdx_home.path())
        .env("ANTHROPIC_API_KEY", "test-api-key")
        .env("ANTHROPIC_BASE_URL", mock_server.uri())
        .args([
            "--root",
            temp_dir.path().to_str().unwrap(),
            "--no-thread",
            "exec",
            "-m",
            MOCK_MODEL,
            "-p",
            "Run a slow command",
        ])
        .assert()
        .success();

    let body = second_request_body.lock().unwrap().clone();
    // New structured envelope format (escaped in JSON content):
    // {"ok":true,"data":{"timed_out":true,...}}
    assert!(
        body.contains(r#"\"timed_out\":true"#),
        "Tool result should indicate timeout with timed_out field in escaped JSON. Got: {body}"
    );
}

#[tokio::test]
async fn test_bash_does_not_inherit_open_stdin() {
    if !can_bind_localhost() {
        eprintln!("Skipping: cannot bind localhost TCP port in this environment.");
        return;
    }

    let zdx_home = temp_zdx_home();
    let temp_dir = TempDir::new().unwrap();
    let mock_server = MockServer::start().await;

    let call_count = Arc::new(AtomicUsize::new(0));
    let call_count_clone = Arc::clone(&call_count);
    let second_request_body = Arc::new(std::sync::Mutex::new(String::new()));
    let second_request_body_clone = Arc::clone(&second_request_body);

    let first_response = tool_use_sse(
        "toolu_bash_stdin",
        "bash",
        r#"{"command": "if read line; then echo inherited:$line; else echo stdin_closed; fi"}"#,
    );
    let second_response = fixtures::text_sse("Done.");

    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(move |req: &Request| {
            let count = call_count_clone.fetch_add(1, Ordering::SeqCst);
            if count == 0 {
                sse_response(&first_response)
            } else {
                let body = String::from_utf8_lossy(&req.body).to_string();
                *second_request_body_clone.lock().unwrap() = body;
                sse_response(&second_response)
            }
        })
        .expect(2)
        .mount(&mock_server)
        .await;

    let mut cmd = tokio::process::Command::new(env!("CARGO_BIN_EXE_zdx"));
    cmd.env("ZDX_HOME", zdx_home.path())
        .env("ANTHROPIC_API_KEY", "test-api-key")
        .env("ANTHROPIC_BASE_URL", mock_server.uri())
        .args([
            "--root",
            temp_dir.path().to_str().unwrap(),
            "--no-thread",
            "exec",
            "-m",
            MOCK_MODEL,
            "-p",
            "Check stdin handling",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null());

    let mut child = cmd.spawn().expect("spawn zdx with open stdin");
    let _stdin_guard = child.stdin.take().expect("keep stdin pipe open");

    let status = if let Ok(result) = timeout(Duration::from_secs(5), child.wait()).await {
        result.expect("wait for zdx")
    } else {
        child.kill().await.expect("kill timed out zdx process");
        panic!("zdx hung while bash command had an open stdin pipe");
    };

    assert!(status.success(), "zdx should exit successfully: {status}");
    assert_eq!(
        call_count.load(Ordering::SeqCst),
        2,
        "expected bash tool turn to complete"
    );

    let body = second_request_body.lock().unwrap().clone();
    assert!(
        body.contains("stdin_closed"),
        "bash command should see closed stdin instead of inheriting the parent pipe. Got: {body}"
    );
}

/// A foreground command that outruns the bound is moved to the background and
/// the `exec` process must then **wait for it**, not kill it: `zdx exec` owns
/// the job's supervisor lease, so exiting early would kill exactly the slow
/// work the handoff exists to preserve.
///
/// This is the orchestrator-worker / subagent case, which all run as `zdx exec`.
#[tokio::test]
async fn test_exec_drains_backgrounded_job_before_exiting() {
    if !can_bind_localhost() {
        eprintln!("Skipping: cannot bind localhost TCP port in this environment.");
        return;
    }
    let temp_dir = TempDir::new().unwrap();
    let zdx_home = temp_zdx_home();
    std::fs::write(
        zdx_home.path().join("config.toml"),
        "bash_foreground_bound_secs = 1\n",
    )
    .unwrap();

    let mock_server = MockServer::start().await;
    let call_count = Arc::new(AtomicUsize::new(0));
    let call_count_clone = Arc::clone(&call_count);

    // Runs well past the 1s bound, writing before and after it, then marks
    // completion on disk. The marker only exists if the job ran to the end.
    let done_marker = temp_dir.path().join("job-done.txt");
    let command = format!(
        "echo early; sleep 3; echo late; echo finished > {}",
        done_marker.display()
    );
    let first_response = tool_use_sse(
        "toolu_bash_drain",
        "bash",
        &serde_json::json!({ "command": command }).to_string(),
    );
    let second_response = fixtures::text_sse("Done.");

    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(move |_req: &Request| {
            if call_count_clone.fetch_add(1, Ordering::SeqCst) == 0 {
                sse_response(&first_response)
            } else {
                sse_response(&second_response)
            }
        })
        .expect(2)
        .mount(&mock_server)
        .await;

    let started = std::time::Instant::now();
    let mut cmd = tokio::process::Command::new(env!("CARGO_BIN_EXE_zdx"));
    cmd.env("ZDX_HOME", zdx_home.path())
        .env("ANTHROPIC_API_KEY", "test-api-key")
        .env("ANTHROPIC_BASE_URL", mock_server.uri())
        .args([
            "--root",
            temp_dir.path().to_str().unwrap(),
            "--no-thread",
            "exec",
            "-m",
            MOCK_MODEL,
            "-p",
            "Run a slow command",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());

    let mut child = cmd.spawn().expect("spawn zdx exec");
    let status = timeout(Duration::from_mins(1), child.wait())
        .await
        .expect("zdx exec never exited while draining")
        .expect("wait for zdx");

    assert!(
        status.success(),
        "zdx exec should exit successfully: {status}"
    );
    assert!(
        done_marker.exists(),
        "exec exited before the backgrounded job finished; the job was killed"
    );
    assert_eq!(
        std::fs::read_to_string(&done_marker).unwrap().trim(),
        "finished"
    );
    // The turn itself returns at the 1s bound; the process stays alive for the
    // rest of the job, so total runtime must cover the full command.
    assert!(
        started.elapsed() >= Duration::from_secs(3),
        "exec did not actually wait for the job: {:?}",
        started.elapsed()
    );
}

/// A command that finishes inside the bound is never adopted, so the process
/// must exit immediately with nothing to drain.
#[tokio::test]
async fn test_exec_without_backgrounded_jobs_exits_immediately() {
    if !can_bind_localhost() {
        eprintln!("Skipping: cannot bind localhost TCP port in this environment.");
        return;
    }
    let temp_dir = TempDir::new().unwrap();
    let zdx_home = temp_zdx_home();
    std::fs::write(
        zdx_home.path().join("config.toml"),
        "bash_foreground_bound_secs = 30\n",
    )
    .unwrap();

    let mock_server = MockServer::start().await;
    let call_count = Arc::new(AtomicUsize::new(0));
    let call_count_clone = Arc::clone(&call_count);

    let first_response = tool_use_sse("toolu_bash_fast", "bash", r#"{"command": "echo quick"}"#);
    let second_response = fixtures::text_sse("Done.");

    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(move |_req: &Request| {
            if call_count_clone.fetch_add(1, Ordering::SeqCst) == 0 {
                sse_response(&first_response)
            } else {
                sse_response(&second_response)
            }
        })
        .expect(2)
        .mount(&mock_server)
        .await;

    let started = std::time::Instant::now();
    let mut cmd = tokio::process::Command::new(env!("CARGO_BIN_EXE_zdx"));
    cmd.env("ZDX_HOME", zdx_home.path())
        .env("ANTHROPIC_API_KEY", "test-api-key")
        .env("ANTHROPIC_BASE_URL", mock_server.uri())
        .args([
            "--root",
            temp_dir.path().to_str().unwrap(),
            "--no-thread",
            "exec",
            "-m",
            MOCK_MODEL,
            "-p",
            "Run a fast command",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());

    let mut child = cmd.spawn().expect("spawn zdx exec");
    let status = timeout(Duration::from_secs(30), child.wait())
        .await
        .expect("fast exec should not hang")
        .expect("wait for zdx");

    assert!(
        status.success(),
        "zdx exec should exit successfully: {status}"
    );
    assert!(
        started.elapsed() < Duration::from_secs(20),
        "a run with nothing adopted must not wait: {:?}",
        started.elapsed()
    );
}

/// Interrupting `exec` while it drains stops the adopted job through its lease
/// rather than orphaning it. The command traps SIGTERM, so only the
/// supervisor's TERM→KILL escalation can reap it.
#[cfg(unix)]
#[tokio::test]
async fn test_exec_interrupt_during_drain_leaves_no_orphan() {
    if !can_bind_localhost() {
        eprintln!("Skipping: cannot bind localhost TCP port in this environment.");
        return;
    }
    let temp_dir = TempDir::new().unwrap();
    let zdx_home = temp_zdx_home();
    std::fs::write(
        zdx_home.path().join("config.toml"),
        "bash_foreground_bound_secs = 1\n",
    )
    .unwrap();

    let mock_server = MockServer::start().await;
    let call_count = Arc::new(AtomicUsize::new(0));
    let call_count_clone = Arc::clone(&call_count);

    // Never exits on its own and ignores TERM: the process can only leave the
    // drain by being interrupted, and the job can only die via the lease.
    let pidfile = temp_dir.path().join("job.pid");
    let command = format!(
        "trap '' TERM; echo $$ > {}; while :; do sleep 1; done",
        pidfile.display()
    );
    let first_response = tool_use_sse(
        "toolu_bash_interrupt",
        "bash",
        &serde_json::json!({ "command": command }).to_string(),
    );
    let second_response = fixtures::text_sse("Done.");

    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(move |_req: &Request| {
            if call_count_clone.fetch_add(1, Ordering::SeqCst) == 0 {
                sse_response(&first_response)
            } else {
                sse_response(&second_response)
            }
        })
        .mount(&mock_server)
        .await;

    let mut cmd = tokio::process::Command::new(env!("CARGO_BIN_EXE_zdx"));
    cmd.env("ZDX_HOME", zdx_home.path())
        .env("ANTHROPIC_API_KEY", "test-api-key")
        .env("ANTHROPIC_BASE_URL", mock_server.uri())
        .args([
            "--root",
            temp_dir.path().to_str().unwrap(),
            "--no-thread",
            "exec",
            "-m",
            MOCK_MODEL,
            "-p",
            "Run a never-ending command",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());

    let mut child = cmd.spawn().expect("spawn zdx exec");
    let exec_pid = child.id().expect("exec pid") as i32;

    // Wait for the job to be running and adopted (past the 1s bound).
    let job_pid: i32 = timeout(Duration::from_secs(30), async {
        loop {
            if let Ok(raw) = std::fs::read_to_string(&pidfile)
                && let Ok(pid) = raw.trim().parse()
            {
                break pid;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("job never started");
    tokio::time::sleep(Duration::from_secs(2)).await;

    assert!(
        process_alive(job_pid),
        "job should still be running while exec drains"
    );

    // Ctrl-C the draining exec process.
    unsafe { libc::kill(exec_pid, libc::SIGINT) };

    let status = timeout(Duration::from_secs(30), child.wait())
        .await
        .expect("interrupted exec never exited")
        .expect("wait for zdx");
    let _ = status;

    timeout(Duration::from_secs(15), async {
        while process_alive(job_pid) {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("interrupted drain left an orphaned job");
}

#[cfg(unix)]
fn process_alive(pid: i32) -> bool {
    if unsafe { libc::kill(pid, 0) } == 0 {
        return true;
    }
    std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}
