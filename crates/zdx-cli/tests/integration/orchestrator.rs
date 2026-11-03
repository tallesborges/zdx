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
