//! Tests for `zdx orchestrator`: the service, its CLI client, and the rule
//! that only the service runs turns on a persistent-profile thread.

use std::fs;
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tempfile::TempDir;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request};

use crate::fixtures::{MOCK_MODEL, sse_response, text_sse};

fn can_bind_localhost() -> bool {
    std::net::TcpListener::bind("127.0.0.1:0").is_ok()
}

/// A message sent with `--wait` runs one turn of the built-in orchestrator in
/// the service and prints its events until the thread settles. The thread
/// then belongs to the service: `exec` refuses it, and `status` reports it.
#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn test_orchestrator_send_wait_runs_the_turn_in_the_service() {
    if !can_bind_localhost() {
        eprintln!("Skipping: cannot bind localhost TCP port in this environment.");
        return;
    }
    let zdx_home = TempDir::new().unwrap();
    let root = TempDir::new().unwrap();
    let mock_server = MockServer::start().await;
    let captured: Arc<Mutex<Option<serde_json::Value>>> = Arc::new(Mutex::new(None));
    let captured_clone = Arc::clone(&captured);
    let body = text_sse("ORCHESTRATED-ANSWER");
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(move |req: &Request| {
            *captured_clone.lock().unwrap() = req.body_json().ok();
            sse_response(&body)
        })
        .mount(&mock_server)
        .await;

    let log = zdx_home.path().join("service.log");
    let mut service = tokio::process::Command::new(env!("CARGO_BIN_EXE_zdx"))
        .env("ZDX_HOME", zdx_home.path())
        .env("ANTHROPIC_API_KEY", "test-api-key")
        .env("ANTHROPIC_BASE_URL", mock_server.uri())
        .current_dir(root.path())
        .args(["--root", root.path().to_str().unwrap()])
        .args(["orchestrator", "serve"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(fs::File::create(&log).unwrap())
        .kill_on_drop(true)
        .spawn()
        .expect("spawn zdx orchestrator serve");
    let socket = zdx_home.path().join("run").join("orchestrator.sock");
    let mut waited = Duration::ZERO;
    while !socket.exists() {
        assert!(
            waited < Duration::from_secs(30),
            "service never listened: {}",
            fs::read_to_string(&log).unwrap_or_default()
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
        waited += Duration::from_millis(100);
    }

    let output = crate::fixtures::zdx_cmd()
        .env("ZDX_HOME", zdx_home.path())
        .args(["--root", root.path().to_str().unwrap()])
        .args([
            "orchestrator",
            "send",
            "--thread",
            "issue-1",
            "--model",
            MOCK_MODEL,
            "-p",
            "triage this",
            "--wait",
        ])
        .timeout(Duration::from_mins(1))
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let stdout = String::from_utf8(output).unwrap();
    let lines: Vec<serde_json::Value> = stdout
        .lines()
        .map(|line| serde_json::from_str(line).expect("JSON line"))
        .collect();
    assert_eq!(lines[0]["reply"], "submitted");
    assert_eq!(lines[0]["thread_id"], "issue-1");
    assert!(
        lines.iter().any(|line| line["event"]["type"] == "agent"
            && line["event"]["event"]["type"] == "turn_finished"
            && line["event"]["event"]["final_text"]
                .as_str()
                .is_some_and(|text| text.contains("ORCHESTRATED-ANSWER"))),
        "{stdout}"
    );
    assert_eq!(
        lines.last().unwrap()["event"]["type"],
        "settled",
        "{stdout}"
    );

    // The orchestrator profile ran, with the non-interactive layer.
    let request = captured.lock().unwrap().clone().expect("model request");
    let system = request["system"].to_string();
    assert!(system.contains("ZDX Orchestrator"), "{system}");
    assert!(system.contains("This run is non-interactive"), "{system}");
    let tools: Vec<String> = request["tools"]
        .as_array()
        .expect("tools")
        .iter()
        .map(|tool| tool["name"].as_str().unwrap_or_default().to_lowercase())
        .collect();
    assert!(tools.contains(&"create_thread".to_string()), "{tools:?}");
    assert!(!tools.contains(&"bash".to_string()), "{tools:?}");

    let transcript = fs::read_to_string(zdx_home.path().join("threads").join("issue-1.jsonl"))
        .expect("thread file");
    assert!(transcript.contains("triage this"));
    assert!(transcript.contains("ORCHESTRATED-ANSWER"));

    // Only the service writes this thread.
    crate::fixtures::zdx_cmd()
        .env("ZDX_HOME", zdx_home.path())
        .env("ANTHROPIC_API_KEY", "test-api-key")
        .env("ANTHROPIC_BASE_URL", mock_server.uri())
        .args(["--root", root.path().to_str().unwrap()])
        .args([
            "--thread", "issue-1", "exec", "-m", MOCK_MODEL, "-p", "direct",
        ])
        .assert()
        .failure()
        .stderr(predicates::str::contains(
            "zdx orchestrator send --thread issue-1",
        ));

    let status = crate::fixtures::zdx_cmd()
        .env("ZDX_HOME", zdx_home.path())
        .args(["orchestrator", "status", "--thread", "issue-1", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let status: serde_json::Value = serde_json::from_slice(&status).unwrap();
    assert_eq!(status["settled"], true);
    assert_eq!(status["threads"][0]["thread_id"], "issue-1");
    assert_eq!(status["threads"][0]["profile"], "orchestrator");
    assert_eq!(status["threads"][0]["state"], "idle");

    service.kill().await.unwrap();
}

/// Without a running service the client says how to start one.
#[test]
fn test_orchestrator_send_without_service_names_the_fix() {
    let zdx_home = TempDir::new().unwrap();
    crate::fixtures::zdx_cmd()
        .env("ZDX_HOME", zdx_home.path())
        .args(["orchestrator", "send", "-p", "hello"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("zdx service start orchestrator"));
}

/// Starts `zdx orchestrator serve` against a mock Anthropic endpoint and waits
/// until its socket listens.
async fn start_service(
    zdx_home: &TempDir,
    root: &TempDir,
    mock_server: &MockServer,
) -> tokio::process::Child {
    let log = zdx_home.path().join("service.log");
    let service = tokio::process::Command::new(env!("CARGO_BIN_EXE_zdx"))
        .env("ZDX_HOME", zdx_home.path())
        .env("HOME", zdx_home.path())
        .env("ANTHROPIC_API_KEY", "test-api-key")
        .env("ANTHROPIC_BASE_URL", mock_server.uri())
        .current_dir(root.path())
        .args(["--root", root.path().to_str().unwrap()])
        .args(["orchestrator", "serve"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(fs::File::create(&log).unwrap())
        .kill_on_drop(true)
        .spawn()
        .expect("spawn zdx orchestrator serve");
    let socket = zdx_home.path().join("run").join("orchestrator.sock");
    let mut waited = Duration::ZERO;
    while !socket.exists() {
        assert!(
            waited < Duration::from_secs(30),
            "service never listened: {}",
            fs::read_to_string(&log).unwrap_or_default()
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
        waited += Duration::from_millis(100);
    }
    service
}

/// `--ordinary` runs a Telegram topic's agent turn in the service: every
/// builtin tool, the Telegram instruction layer, and a thread that stays
/// ordinary. The routed turn is held for delivery until `send --wait`
/// releases it, then the thread settles. The service reports the capability.
#[allow(clippy::too_many_lines)] // one flow checked end to end
#[tokio::test]
async fn test_orchestrator_send_ordinary_runs_a_routed_agent_turn() {
    if !can_bind_localhost() {
        eprintln!("Skipping: cannot bind localhost TCP port in this environment.");
        return;
    }
    let zdx_home = TempDir::new().unwrap();
    let root = TempDir::new().unwrap();
    let mock_server = MockServer::start().await;
    let captured: Arc<Mutex<Option<serde_json::Value>>> = Arc::new(Mutex::new(None));
    let captured_clone = Arc::clone(&captured);
    let body = text_sse("ORDINARY-ANSWER");
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(move |req: &Request| {
            *captured_clone.lock().unwrap() = req.body_json().ok();
            sse_response(&body)
        })
        .expect(1)
        .mount(&mock_server)
        .await;
    let mut service = start_service(&zdx_home, &root, &mock_server).await;

    let status = crate::fixtures::zdx_cmd()
        .env("ZDX_HOME", zdx_home.path())
        .args(["orchestrator", "status", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let status: serde_json::Value = serde_json::from_slice(&status).unwrap();
    assert_eq!(status["protocol"], 2);
    assert!(
        status["capabilities"]
            .as_array()
            .unwrap()
            .iter()
            .any(|name| name == "ordinary_submit"),
        "{status}"
    );

    let thread = "telegram--100-topic-7";
    let output = crate::fixtures::zdx_cmd()
        .env("ZDX_HOME", zdx_home.path())
        .args(["--root", root.path().to_str().unwrap()])
        .args([
            "orchestrator",
            "send",
            "--ordinary",
            "--thread",
            thread,
            "--route-chat",
            "-100",
            "--route-topic",
            "7",
            "--model",
            MOCK_MODEL,
            "-p",
            "hello topic",
            "--wait",
        ])
        .timeout(Duration::from_mins(1))
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let stdout = String::from_utf8(output).unwrap();
    let lines: Vec<serde_json::Value> = stdout
        .lines()
        .map(|line| serde_json::from_str(line).expect("JSON line"))
        .collect();
    assert_eq!(lines[0]["reply"], "submitted", "{stdout}");
    let event = |kind: &str| {
        lines
            .iter()
            .find(|line| line["event"]["type"] == kind)
            .unwrap_or_else(|| panic!("no {kind} event: {stdout}"))
            .clone()
    };
    assert_eq!(event("turn_started")["event"]["route"]["chat"], -100);
    assert_eq!(event("turn_ended")["event"]["held"], true, "{stdout}");
    assert!(
        lines
            .iter()
            .any(|line| line["event"]["event"]["type"] == "turn_finished"
                && line["event"]["event"]["final_text"]
                    .as_str()
                    .is_some_and(|text| text.contains("ORDINARY-ANSWER"))),
        "{stdout}"
    );
    assert_eq!(
        lines.last().unwrap()["event"]["type"],
        "settled",
        "{stdout}"
    );

    let request = captured.lock().unwrap().clone().expect("model request");
    let system = request["system"].to_string();
    assert!(system.contains("Telegram reply guide"), "{system}");
    assert!(!system.contains("ZDX Orchestrator"), "{system}");
    let tools: Vec<String> = request["tools"]
        .as_array()
        .expect("tools")
        .iter()
        .map(|tool| tool["name"].as_str().unwrap_or_default().to_lowercase())
        .collect();
    assert!(tools.contains(&"bash".to_string()), "{tools:?}");

    let transcript = fs::read_to_string(
        zdx_home
            .path()
            .join("threads")
            .join(format!("{thread}.jsonl")),
    )
    .expect("thread file");
    let meta: serde_json::Value = serde_json::from_str(transcript.lines().next().unwrap()).unwrap();
    assert_eq!(meta["type"], "meta");
    assert!(
        meta.get("subagent_name")
            .is_none_or(serde_json::Value::is_null),
        "{meta}"
    );
    assert_eq!(
        meta["root_path"],
        root.path().canonicalize().unwrap().display().to_string()
    );
    assert!(transcript.contains("hello topic"));
    assert!(transcript.contains("ORDINARY-ANSWER"));

    // The thread stays ordinary: a persistent-profile submit is refused.
    crate::fixtures::zdx_cmd()
        .env("ZDX_HOME", zdx_home.path())
        .args([
            "orchestrator",
            "send",
            "--thread",
            thread,
            "-p",
            "as profile",
        ])
        .assert()
        .failure()
        .stderr(predicates::str::contains("ordinary"));

    // Views come from the service (its status lists them) with the Mini App
    // route bodies.
    let view = |request: serde_json::Value| -> serde_json::Value {
        let output = crate::fixtures::zdx_cmd()
            .env("ZDX_HOME", zdx_home.path())
            .args(["--root", root.path().to_str().unwrap()])
            .args(["views", "--file", "-"])
            .write_stdin(request.to_string())
            .assert()
            .success()
            .get_output()
            .stdout
            .clone();
        serde_json::from_slice(&output).unwrap()
    };
    let thread_view = view(serde_json::json!({"op":"thread_view","thread_id":thread}));
    assert_eq!(thread_view["reply"], "view", "{thread_view}");
    assert_eq!(thread_view["source_id"], thread);
    assert_eq!(thread_view["body"]["id"], thread);
    assert_eq!(thread_view["body"]["delivering"], false);
    assert!(
        thread_view["body"]["activity"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item["text"]
                .as_str()
                .is_some_and(|t| t.contains("ORDINARY-ANSWER"))),
        "{thread_view}"
    );
    let list = view(serde_json::json!({"op":"threads_list"}));
    assert!(
        list["body"]["threads"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item["id"] == thread),
        "{list}"
    );
    let agents = view(serde_json::json!({"op":"active_agents"}));
    assert!(agents["body"].is_array(), "{agents}");
    let snapshot = view(serde_json::json!({"op":"monitor_snapshot"}));
    assert!(snapshot["body"]["services"].is_array(), "{snapshot}");
    // Quotas are not part of the snapshot: the gateway overlays its own.
    assert_eq!(snapshot["body"]["subscriptions"], serde_json::json!([]));
    assert_eq!(snapshot["body"]["subscriptions_status"], "pending");
    assert!(
        snapshot["body"]["subscriptions_age"].is_null(),
        "{snapshot}"
    );
    // `zdx views` refuses the retired quota view before it reaches the
    // service.
    crate::fixtures::zdx_cmd()
        .env("ZDX_HOME", zdx_home.path())
        .args(["views", "--file", "-"])
        .write_stdin(r#"{"op":"subscription_quotas"}"#)
        .assert()
        .failure()
        .stdout(predicates::str::is_empty())
        .stderr(predicates::str::contains("parse view request"));
    let usage = view(serde_json::json!({"op":"usage_report","days":7}));
    assert!(usage["body"].is_object(), "{usage}");
    let artifacts = view(serde_json::json!({"op":"artifacts_list","thread_id":thread}));
    assert!(artifacts["body"]["artifacts"].is_array(), "{artifacts}");
    crate::fixtures::zdx_cmd()
        .env("ZDX_HOME", zdx_home.path())
        .args(["views", "--file", "-"])
        .write_stdin(r#"{"op":"usage_report","days":0}"#)
        .assert()
        .failure()
        .stderr(predicates::str::contains("days must be between 1 and 365"));
    let status = crate::fixtures::zdx_cmd()
        .env("ZDX_HOME", zdx_home.path())
        .args(["orchestrator", "status", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let status: serde_json::Value = serde_json::from_slice(&status).unwrap();
    assert_eq!(
        status["views_root"]
            .as_str()
            .and_then(|path| std::path::Path::new(path).canonicalize().ok()),
        Some(root.path().canonicalize().unwrap()),
        "{status}"
    );
    for capability in [
        "thread_views",
        "artifact_views",
        "monitor_views",
        "git_views",
    ] {
        assert!(
            status["capabilities"]
                .as_array()
                .unwrap()
                .iter()
                .any(|name| name == capability),
            "{status}"
        );
    }

    service.kill().await.unwrap();
}

/// Thread writes from the CLI go through the running service, which owns the
/// queue: a reset is refused while a turn runs there. With no service the
/// CLI applies the same operation itself.
#[tokio::test]
async fn test_thread_writes_go_through_the_running_service() {
    if !can_bind_localhost() {
        eprintln!("Skipping: cannot bind localhost TCP port in this environment.");
        return;
    }
    let zdx_home = TempDir::new().unwrap();
    let root = TempDir::new().unwrap();
    let mock_server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(sse_response(&text_sse("SLOW")).set_delay(Duration::from_secs(4)))
        .mount(&mock_server)
        .await;
    let mut service = start_service(&zdx_home, &root, &mock_server).await;
    let thread = "telegram--100-topic-9";
    crate::fixtures::zdx_cmd()
        .env("ZDX_HOME", zdx_home.path())
        .args(["--root", root.path().to_str().unwrap()])
        .args([
            "orchestrator",
            "send",
            "--ordinary",
            "--thread",
            thread,
            "--model",
            MOCK_MODEL,
            "-p",
            "work",
        ])
        .assert()
        .success();

    crate::fixtures::zdx_cmd()
        .env("ZDX_HOME", zdx_home.path())
        .args(["threads", "reset", thread])
        .assert()
        .failure()
        .stderr(predicates::str::contains("cancel it first"));
    assert!(
        zdx_home
            .path()
            .join("threads")
            .join(format!("{thread}.jsonl"))
            .exists()
    );

    let output = crate::fixtures::zdx_cmd()
        .env("ZDX_HOME", zdx_home.path())
        .args(["threads", "update", thread, "--file", "-"])
        .write_stdin(r#"{"title":"Renamed"}"#)
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let reply: serde_json::Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(reply["reply"], "thread_updated");
    assert_eq!(reply["title"], "Renamed");

    service.kill().await.unwrap();
    let _ = service.wait().await;
    // The pid file of a killed service is stale; the CLI sees no service and
    // resets directly.
    crate::fixtures::zdx_cmd()
        .env("ZDX_HOME", zdx_home.path())
        .args(["threads", "reset", thread])
        .assert()
        .success();
    assert!(
        !zdx_home
            .path()
            .join("threads")
            .join(format!("{thread}.jsonl"))
            .exists()
    );
}

/// Holds a thread's run lease the way another process's turn would.
fn hold_run_lease(zdx_home: &TempDir, thread: &str) -> fs::File {
    let dir = zdx_home.path().join("run").join("thread-runs");
    fs::create_dir_all(&dir).unwrap();
    let file = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(dir.join(format!("{thread}.lock")))
        .unwrap();
    file.try_lock().unwrap();
    file
}

/// One writer per thread across surfaces: while another holder has the
/// thread's run lease, a service turn and a `zdx exec` turn are both refused
/// before they write anything, and no model request is made. Once the lease
/// is free, the same service turn runs.
#[allow(clippy::too_many_lines)] // one flow checked end to end
#[tokio::test]
async fn test_run_lease_refuses_a_second_writer_on_every_path() {
    if !can_bind_localhost() {
        eprintln!("Skipping: cannot bind localhost TCP port in this environment.");
        return;
    }
    let zdx_home = TempDir::new().unwrap();
    let root = TempDir::new().unwrap();
    let mock_server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(sse_response(&text_sse("LEASED-ANSWER")))
        .expect(1)
        .mount(&mock_server)
        .await;
    let mut service = start_service(&zdx_home, &root, &mock_server).await;
    let thread = "telegram--100-topic-9";
    let thread_file = zdx_home
        .path()
        .join("threads")
        .join(format!("{thread}.jsonl"));

    let send = || {
        crate::fixtures::zdx_cmd()
            .env("ZDX_HOME", zdx_home.path())
            .args(["--root", root.path().to_str().unwrap()])
            .args([
                "orchestrator",
                "send",
                "--ordinary",
                "--thread",
                thread,
                "-p",
                "hello",
                "--wait",
            ])
            .timeout(Duration::from_mins(1))
            .output()
            .unwrap()
    };

    let lease = hold_run_lease(&zdx_home, thread);
    let refused = crate::fixtures::zdx_cmd()
        .env("ZDX_HOME", zdx_home.path())
        .args(["--root", root.path().to_str().unwrap()])
        .args([
            "orchestrator",
            "send",
            "--ordinary",
            "--thread",
            thread,
            "--model",
            MOCK_MODEL,
            "-p",
            "hello",
            "--wait",
        ])
        .timeout(Duration::from_mins(1))
        .output()
        .unwrap();
    let output = format!(
        "{}{}",
        String::from_utf8_lossy(&refused.stdout),
        String::from_utf8_lossy(&refused.stderr)
    );
    assert!(
        output.contains("turn running elsewhere"),
        "new thread not refused: {output}"
    );
    assert!(!thread_file.exists(), "a refused submit wrote the thread");

    // An existing thread: the submit is queued, and its turn is refused
    // before it records the user message.
    drop(lease);
    fs::create_dir_all(thread_file.parent().unwrap()).unwrap();
    let meta = serde_json::json!({
        "type": "meta", "schema_version": 1, "ts": "2026-01-01T00:00:00Z",
        "root_path": root.path(), "model_override": MOCK_MODEL,
    });
    fs::write(&thread_file, format!("{meta}\n")).unwrap();
    let before = fs::read_to_string(&thread_file).unwrap();
    let lease = hold_run_lease(&zdx_home, thread);
    let refused = send();
    let output = format!(
        "{}{}",
        String::from_utf8_lossy(&refused.stdout),
        String::from_utf8_lossy(&refused.stderr)
    );
    assert!(!refused.status.success());
    assert!(
        output.contains("turn running elsewhere"),
        "service submit not refused: {output}"
    );
    assert_eq!(
        fs::read_to_string(&thread_file).unwrap(),
        before,
        "a refused service turn wrote the thread"
    );

    let exec = crate::fixtures::zdx_cmd()
        .env("ZDX_HOME", zdx_home.path())
        .env("ANTHROPIC_API_KEY", "test-api-key")
        .env("ANTHROPIC_BASE_URL", mock_server.uri())
        .args(["--root", root.path().to_str().unwrap()])
        .args(["--thread", thread, "exec", "-m", MOCK_MODEL, "-p", "direct"])
        .timeout(Duration::from_mins(1))
        .output()
        .unwrap();
    assert!(!exec.status.success());
    assert!(
        String::from_utf8_lossy(&exec.stderr).contains("turn running elsewhere"),
        "{}",
        String::from_utf8_lossy(&exec.stderr)
    );
    assert_eq!(
        fs::read_to_string(&thread_file).unwrap(),
        before,
        "a refused exec turn wrote the thread"
    );

    drop(lease);
    let ran = send();
    assert!(
        String::from_utf8_lossy(&ran.stdout).contains("LEASED-ANSWER"),
        "{}",
        String::from_utf8_lossy(&ran.stdout)
    );
    assert!(thread_file.exists());
    service.kill().await.ok();
}

/// The service keeps a thread's run lease while a routed turn waits for its
/// delivery: `zdx exec` on that thread is refused until the renderer
/// releases the turn, then runs.
#[allow(clippy::too_many_lines)] // one flow checked end to end
#[tokio::test]
async fn test_run_lease_covers_the_delivery_hold() {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    if !can_bind_localhost() {
        eprintln!("Skipping: cannot bind localhost TCP port in this environment.");
        return;
    }
    let zdx_home = TempDir::new().unwrap();
    let root = TempDir::new().unwrap();
    let mock_server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(sse_response(&text_sse("HELD-ANSWER")))
        .expect(2)
        .mount(&mock_server)
        .await;
    let mut service = start_service(&zdx_home, &root, &mock_server).await;
    let thread = "telegram--100-topic-11";

    let socket = zdx_home.path().join("run").join("orchestrator.sock");
    let stream = tokio::net::UnixStream::connect(&socket).await.unwrap();
    let (read, mut write) = stream.into_split();
    write
        .write_all(b"{\"op\":\"subscribe\",\"renderer\":true}\n")
        .await
        .unwrap();
    let mut lines = BufReader::new(read).lines();
    let first = lines.next_line().await.unwrap().expect("subscribe reply");
    let first: serde_json::Value = serde_json::from_str(&first).unwrap();
    assert_eq!(first["reply"], "subscribed", "{first}");
    let holder = first["holder"].as_str().unwrap().to_string();

    crate::fixtures::zdx_cmd()
        .env("ZDX_HOME", zdx_home.path())
        .args(["--root", root.path().to_str().unwrap()])
        .args([
            "orchestrator",
            "send",
            "--ordinary",
            "--thread",
            thread,
            "--route-chat",
            "-100",
            "--route-topic",
            "11",
            "--model",
            MOCK_MODEL,
            "-p",
            "hello",
        ])
        .timeout(Duration::from_mins(1))
        .assert()
        .success();

    let turn_id = tokio::time::timeout(Duration::from_mins(1), async {
        loop {
            let line = lines.next_line().await.unwrap().expect("event");
            let value: serde_json::Value = serde_json::from_str(&line).unwrap();
            if value["event"]["type"] == "turn_ended" {
                assert_eq!(value["event"]["held"], true, "{line}");
                return value["event"]["turn_id"].as_u64().unwrap();
            }
        }
    })
    .await
    .expect("turn_ended");

    let exec = |prompt: &str| {
        crate::fixtures::zdx_cmd()
            .env("ZDX_HOME", zdx_home.path())
            .env("ANTHROPIC_API_KEY", "test-api-key")
            .env("ANTHROPIC_BASE_URL", mock_server.uri())
            .args(["--root", root.path().to_str().unwrap()])
            .args(["--thread", thread, "exec", "-m", MOCK_MODEL, "-p", prompt])
            .timeout(Duration::from_mins(1))
            .output()
            .unwrap()
    };
    let refused = exec("while held");
    assert!(!refused.status.success());
    assert!(
        String::from_utf8_lossy(&refused.stderr).contains("turn running elsewhere"),
        "{}",
        String::from_utf8_lossy(&refused.stderr)
    );

    crate::fixtures::zdx_cmd()
        .env("ZDX_HOME", zdx_home.path())
        .args([
            "orchestrator",
            "release",
            "--thread",
            thread,
            "--turn",
            &turn_id.to_string(),
            "--holder",
            "someone-else",
        ])
        .assert()
        .failure();
    crate::fixtures::zdx_cmd()
        .env("ZDX_HOME", zdx_home.path())
        .args([
            "orchestrator",
            "release",
            "--thread",
            thread,
            "--turn",
            &turn_id.to_string(),
            "--holder",
            &holder,
        ])
        .assert()
        .success();
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let line = lines.next_line().await.unwrap().expect("event");
            if line.contains("\"settled\"") {
                return;
            }
        }
    })
    .await
    .expect("settled");
    // The lease goes with the drain right after it reports the thread idle.
    let mut ran = exec("after release");
    for _ in 0..20 {
        if ran.status.success() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
        ran = exec("after release");
    }
    assert!(
        ran.status.success(),
        "{}",
        String::from_utf8_lossy(&ran.stderr)
    );
    service.kill().await.ok();
}

/// Mini App feedback through the real service: a typed refusal queues
/// nothing; a saved submission is queued as a routed turn whose user message
/// carries the feedback prompt and the labelled note image (inline); a retry
/// of the same submission is a `duplicate` and queues nothing.
#[tokio::test]
#[allow(clippy::too_many_lines)] // one flow checked end to end
async fn test_feedback_submit_queues_a_turn_once() {
    // A valid 1×1 PNG.
    const PNG: &str = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNkYPhfDwAChwGA60e6kgAAAABJRU5ErkJggg==";

    if !can_bind_localhost() {
        eprintln!("Skipping: cannot bind localhost TCP port in this environment.");
        return;
    }
    let zdx_home = TempDir::new().unwrap();
    let root = TempDir::new().unwrap();
    fs::write(
        zdx_home.path().join("config.toml"),
        "[telegram]\nallowlist_user_ids = [7]\nallowlist_chat_ids = [-100]\n",
    )
    .unwrap();
    let mock_server = MockServer::start().await;
    let bodies: Arc<Mutex<Vec<serde_json::Value>>> = Arc::default();
    let captured = Arc::clone(&bodies);
    let body = text_sse("FEEDBACK-SEEN");
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(move |req: &Request| {
            captured.lock().unwrap().extend(req.body_json().ok());
            sse_response(&body)
        })
        .expect(2)
        .mount(&mock_server)
        .await;
    let mut service = start_service(&zdx_home, &root, &mock_server).await;
    let thread = "telegram--100-topic-21";

    crate::fixtures::zdx_cmd()
        .env("ZDX_HOME", zdx_home.path())
        .args(["--root", root.path().to_str().unwrap()])
        .args([
            "orchestrator",
            "send",
            "--ordinary",
            "--thread",
            thread,
            "--model",
            MOCK_MODEL,
            "-p",
            "make a page",
            "--wait",
        ])
        .timeout(Duration::from_mins(1))
        .assert()
        .success();
    let artifacts = zdx_home
        .path()
        .join("artifacts")
        .join("threads")
        .join(thread);
    fs::create_dir_all(&artifacts).unwrap();
    fs::write(
        artifacts.join("report.html"),
        "<html><body><h1>Hi</h1></body></html>",
    )
    .unwrap();
    let version = crate::fixtures::zdx_cmd()
        .env("ZDX_HOME", zdx_home.path())
        .args(["--root", root.path().to_str().unwrap()])
        .args(["views", "--file", "-"])
        .write_stdin(
            serde_json::json!({"op":"artifact_version","thread_id":thread,"path":"report.html"})
                .to_string(),
        )
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let version: serde_json::Value = serde_json::from_slice(&version).unwrap();
    let sha = version["body"]["sha256"]
        .as_str()
        .expect("sha256")
        .to_string();
    let submission = serde_json::json!({
        "submission_id": "sub-00000001",
        "artifact_path": "report.html",
        "artifact_sha256": sha,
        "note": "Overall looks good",
        "annotations": [{
            "intent": "change",
            "comment": "Make the heading blue",
            "target": { "kind": "page" },
            "media": [{ "kind": "image", "mime": "image/png", "data": PNG }],
        }],
    })
    .to_string();
    let feedback = |user: &str, thread: &str, body: &str| {
        crate::fixtures::zdx_cmd()
            .env("ZDX_HOME", zdx_home.path())
            .args(["--root", root.path().to_str().unwrap()])
            .args([
                "orchestrator",
                "feedback",
                "--thread",
                thread,
                "--user-id",
                user,
            ])
            .write_stdin(body.to_string())
            .timeout(Duration::from_mins(1))
            .output()
            .unwrap()
    };
    let refused = |output: std::process::Output, code: &str| {
        assert!(!output.status.success());
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains(code), "expected {code}: {stderr}");
    };
    // Codes (`json_syntax`, `json_data`, ...) are checked in the engine's
    // unit tests; the CLI prints the message.
    refused(feedback("7", thread, "{not json"), "Invalid feedback body");
    refused(
        feedback("7", thread, r#"{"submission_id":1}"#),
        "Invalid feedback body",
    );
    refused(
        feedback("8", thread, &submission),
        "You cannot send feedback",
    );
    refused(feedback("7", "active", &submission), "Reopen the thread");
    refused(
        feedback("7", "plain-cli-thread", &submission),
        "does not belong to a Telegram chat",
    );

    let queued = feedback("7", thread, &submission);
    assert!(
        queued.status.success(),
        "{}",
        String::from_utf8_lossy(&queued.stderr)
    );
    let receipt: serde_json::Value = serde_json::from_slice(&queued.stdout).unwrap();
    assert_eq!(receipt["reply"], "feedback", "{receipt}");
    assert_eq!(receipt["status"], "queued", "{receipt}");
    assert_eq!(receipt["thread_id"], thread);
    assert_eq!(receipt["annotations"], 1);
    assert_eq!(receipt["artifact_name"], "report.html");
    assert_eq!(receipt["route"]["chat"], -100);
    assert!(receipt["input_id"].is_u64(), "{receipt}");

    let again = feedback("7", thread, &submission);
    assert!(again.status.success());
    let again: serde_json::Value = serde_json::from_slice(&again.stdout).unwrap();
    assert_eq!(again["status"], "duplicate", "{again}");

    // The queued turn runs (unrouted to a renderer here, so it is not held).
    let deadline = tokio::time::Instant::now() + Duration::from_mins(1);
    while bodies.lock().unwrap().len() < 2 {
        assert!(
            tokio::time::Instant::now() < deadline,
            "feedback turn never ran"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let request = bodies.lock().unwrap()[1].clone();
    let last = request["messages"]
        .as_array()
        .unwrap()
        .last()
        .unwrap()
        .clone();
    let content = last["content"].to_string();
    assert!(content.contains("Make the heading blue"), "{content}");
    assert!(content.contains("image attachment saved at"), "{content}");
    assert!(
        last["content"]
            .as_array()
            .is_some_and(|blocks| blocks.iter().any(|block| block["type"] == "image")),
        "{content}"
    );

    // The same service lists the new capabilities and prepares an outgoing
    // photo (a PNG becomes a JPEG; base64 data only when it changed).
    let status = crate::fixtures::zdx_cmd()
        .env("ZDX_HOME", zdx_home.path())
        .args(["orchestrator", "status", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let status: serde_json::Value = serde_json::from_slice(&status).unwrap();
    for capability in [
        "feedback_submit",
        "prepare_photo",
        "touched_workers",
        "worker_mirror_route",
        "hold_tokens",
    ] {
        assert!(
            status["capabilities"]
                .as_array()
                .unwrap()
                .iter()
                .any(|name| name == capability),
            "{capability}: {status}"
        );
    }
    let photo = root.path().join("chart.png");
    {
        use std::io::Write as _;
        let png = std::process::Command::new("base64")
            .arg("-d")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()
            .and_then(|mut child| {
                child.stdin.take().unwrap().write_all(PNG.as_bytes())?;
                child.wait_with_output()
            })
            .unwrap();
        fs::write(&photo, png.stdout).unwrap();
    }
    let socket = zdx_home.path().join("run").join("orchestrator.sock");
    let reply = {
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
        let stream = tokio::net::UnixStream::connect(&socket).await.unwrap();
        let (read, mut write) = stream.into_split();
        let request = serde_json::json!({"op":"prepare_photo","path":photo}).to_string() + "\n";
        write.write_all(request.as_bytes()).await.unwrap();
        let line = BufReader::new(read)
            .lines()
            .next_line()
            .await
            .unwrap()
            .unwrap();
        serde_json::from_str::<serde_json::Value>(&line).unwrap()
    };
    assert_eq!(reply["reply"], "prepared_photo", "{reply}");
    assert_eq!(reply["changed"], true, "{reply}");
    assert_eq!(reply["mime_type"], "image/jpeg");
    assert_eq!(reply["file_name"], "chart.jpg");
    assert!(reply["data"].is_string());
    service.kill().await.ok();
}

/// Helper ops through the real service and their CLI twin: a generated title
/// and recap from the mocked model, the `/btw` seed, and a seeded topic's
/// start (an unresolved source mode refuses creation; a pick resolves).
#[allow(clippy::too_many_lines)] // one flow checked end to end
#[tokio::test]
async fn test_helper_ops_generate_seed_and_topic_start() {
    if !can_bind_localhost() {
        eprintln!("Skipping: cannot bind localhost TCP port in this environment.");
        return;
    }
    let zdx_home = TempDir::new().unwrap();
    let root = TempDir::new().unwrap();
    fs::write(
        zdx_home.path().join("config.toml"),
        format!("title_model = \"{MOCK_MODEL}\"\ntldr_model = \"{MOCK_MODEL}\"\n"),
    )
    .unwrap();
    let mock_server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(sse_response(&text_sse("Helper Output")))
        .mount(&mock_server)
        .await;
    let mut service = start_service(&zdx_home, &root, &mock_server).await;
    let thread = "telegram--100-topic-31";
    crate::fixtures::zdx_cmd()
        .env("ZDX_HOME", zdx_home.path())
        .args(["--root", root.path().to_str().unwrap()])
        .args([
            "orchestrator",
            "send",
            "--ordinary",
            "--thread",
            thread,
            "--model",
            MOCK_MODEL,
            "-p",
            "plan the release",
            "--wait",
        ])
        .timeout(Duration::from_mins(1))
        .assert()
        .success();

    let socket = zdx_home.path().join("run").join("orchestrator.sock");
    let call = |request: serde_json::Value| {
        let socket = socket.clone();
        async move {
            use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
            let stream = tokio::net::UnixStream::connect(&socket).await.unwrap();
            let (read, mut write) = stream.into_split();
            write
                .write_all(format!("{request}\n").as_bytes())
                .await
                .unwrap();
            let line = BufReader::new(read)
                .lines()
                .next_line()
                .await
                .unwrap()
                .unwrap();
            serde_json::from_str::<serde_json::Value>(&line).unwrap()
        }
    };
    let title = call(serde_json::json!({
        "op": "generate", "kind": "title", "root": root.path(), "text": "plan the release",
        "thread_id": thread,
    }))
    .await;
    assert_eq!(title["reply"], "generated", "{title}");
    // The retired quota view is an invalid request on the socket.
    let retired = call(serde_json::json!({"op": "subscription_quotas"})).await;
    assert_eq!(retired["reply"], "error", "{retired}");
    assert_eq!(retired["code"], "invalid", "{retired}");
    assert!(
        title["text"].as_str().unwrap().contains("Helper Output"),
        "{title}"
    );
    let tldr = call(serde_json::json!({
        "op": "generate", "kind": "tldr", "root": root.path(), "thread_id": thread,
    }))
    .await;
    assert_eq!(tldr["reply"], "generated", "{tldr}");
    let missing = call(serde_json::json!({
        "op": "generate", "kind": "tldr", "root": root.path(), "thread_id": "no-such-thread",
    }))
    .await;
    assert_eq!(missing["code"], "not_found", "{missing}");

    let seed =
        call(serde_json::json!({"op":"btw_seed","thread_id":thread,"question":"why?"})).await;
    assert_eq!(seed["reply"], "text", "{seed}");
    assert!(seed["text"].as_str().unwrap().ends_with("why?"), "{seed}");

    let start = call(serde_json::json!({
        "op": "topic_start", "root": root.path(), "source_thread_id": thread,
    }))
    .await;
    assert_eq!(start["reply"], "topic_start", "{start}");
    assert_eq!(
        start["selection"],
        MOCK_MODEL.to_string() + "@off",
        "{start}"
    );

    fs::write(
        zdx_home.path().join("threads").join("gone-mode.jsonl"),
        format!(
            "{}\n",
            serde_json::json!({"type":"meta","schema_version":1,"ts":"2026-01-01T00:00:00Z","model_override":"mode:gone"})
        ),
    )
    .unwrap();
    let refused = call(serde_json::json!({
        "op": "topic_start", "root": root.path(), "source_thread_id": "gone-mode",
    }))
    .await;
    assert_eq!(refused["code"], "invalid", "{refused}");
    // The CLI twin answers the same, without the service.
    let output = crate::fixtures::zdx_cmd()
        .env("ZDX_HOME", zdx_home.path())
        .args(["helper", "--file", "-"])
        .write_stdin(
            serde_json::json!({
                "op": "topic_start", "root": root.path(), "source_thread_id": "gone-mode",
                "pick": MOCK_MODEL,
            })
            .to_string(),
        )
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let picked: serde_json::Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(picked["reply"], "topic_start", "{picked}");
    assert!(
        picked["selection"]
            .as_str()
            .unwrap()
            .starts_with(MOCK_MODEL),
        "{picked}"
    );

    // Catalogs: the topic's status card, its picker, the launcher, custom
    // commands, resumable threads, and the chat default-model writer.
    let catalog = |query: serde_json::Value| {
        let call = &call;
        async move {
            let reply = call(query).await;
            assert_eq!(reply["reply"], "catalog", "{reply}");
            reply["body"].clone()
        }
    };
    let status = catalog(serde_json::json!({
        "op": "catalog", "kind": "thread_status", "root": root.path(), "thread_id": thread,
    }))
    .await;
    assert_eq!(status["model"], format!("{MOCK_MODEL}@off"), "{status}");
    assert_eq!(status["model_source"], "topic", "{status}");
    assert!(status["usage"]["output"].as_u64().unwrap() > 0, "{status}");
    assert_eq!(status["orchestrator"], false);
    let topic_picker = catalog(serde_json::json!({
        "op": "catalog", "kind": "picker", "root": root.path(), "scope": "topic",
        "thread_id": thread,
    }))
    .await;
    assert_eq!(topic_picker["overridden"], true, "{topic_picker}");
    assert!(
        topic_picker["models"]
            .as_array()
            .is_some_and(|m| !m.is_empty()),
        "{topic_picker}"
    );
    let launcher = catalog(serde_json::json!({
        "op": "catalog", "kind": "launcher", "root": root.path(), "role": "orchestrator",
    }))
    .await;
    assert!(launcher["default_model"].is_string(), "{launcher}");
    let project_commands = root.path().join(".zdx").join("commands");
    fs::create_dir_all(&project_commands).unwrap();
    fs::write(
        project_commands.join("review.md"),
        "---\ndescription: Review the diff\n---\nReview the current diff.",
    )
    .unwrap();
    let commands = catalog(serde_json::json!({
        "op": "catalog", "kind": "custom_commands", "root": root.path(), "native": ["model"],
    }))
    .await;
    assert_eq!(commands["commands"][0]["name"], "review", "{commands}");
    assert_eq!(commands["commands"][0]["source"], "project");
    let resumable = catalog(serde_json::json!({
        "op": "catalog", "kind": "resumable", "root": root.path(),
    }))
    .await;
    assert!(resumable["threads"].is_array(), "{resumable}");
    let set = call(serde_json::json!({
        "op": "set_chat_model", "root": root.path(), "model": "mode:gone",
    }))
    .await;
    assert_eq!(set["code"], "invalid", "{set}");
    let set = call(serde_json::json!({
        "op": "set_chat_model", "root": root.path(), "model": MOCK_MODEL,
    }))
    .await;
    assert_eq!(set["reply"], "chat_model", "{set}");
    service.kill().await.ok();
}
