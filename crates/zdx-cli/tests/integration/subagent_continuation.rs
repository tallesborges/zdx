//! End-to-end tests for continuing a subagent with `invoke_subagent`
//! `thread_id`: the id every call returns, a second turn that sees the first
//! one, the same thread and artifact directory, and the refusals for a thread
//! that is missing, belongs to another subagent, or is still running.
//!
//! Parent and child share one mock provider. A child request is one whose
//! first message carries `CHILD_MARKER`; that stays true for a continued
//! child, whose history starts with the first prompt.

use std::process::Stdio;
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::time::{Duration, timeout};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request};

use crate::fixtures::{MOCK_MODEL, sse_response, text_sse, tool_use_sse};

const CHILD_MARKER: &str = "CHILD-TASK-continue";
const SECOND_TURN: &str = "SECOND-TURN-REQUEST";
const FIRST_ANSWER: &str = "FIRST-ANSWER-17";
const REMEMBERED: &str = "REMEMBERED-FIRST-ANSWER";

fn can_bind_localhost() -> bool {
    std::net::TcpListener::bind("127.0.0.1:0").is_ok()
}

/// A workspace with two subagents that have `bash`.
fn workspace() -> (TempDir, TempDir) {
    let zdx_home = TempDir::new().unwrap();
    let root = TempDir::new().unwrap();
    let dir = root.path().join(".zdx").join("subagents");
    std::fs::create_dir_all(&dir).unwrap();
    for name in ["helper", "other"] {
        std::fs::write(
            dir.join(format!("{name}.md")),
            "---\ndescription: Helper\ntools:\n  - bash\n  - read\n---\nYou help.\n",
        )
        .unwrap();
    }
    (zdx_home, root)
}

/// The tool result envelopes in a request, in order, parsed from their text.
fn tool_results(body: &Value) -> Vec<Value> {
    let mut results = Vec::new();
    for message in body["messages"].as_array().into_iter().flatten() {
        for block in message["content"].as_array().into_iter().flatten() {
            if block["type"] != "tool_result" {
                continue;
            }
            let text = match &block["content"] {
                Value::String(text) => text.clone(),
                Value::Array(parts) => parts
                    .iter()
                    .filter_map(|part| part["text"].as_str())
                    .collect::<String>(),
                _ => String::new(),
            };
            results.push(serde_json::from_str(&text).unwrap_or(Value::String(text)));
        }
    }
    results
}

fn invoke(step: usize, input: &Value) -> String {
    tool_use_sse(
        &format!("toolu_invoke_{step}"),
        "invoke_subagent",
        &input.to_string(),
    )
}

fn first_call() -> Value {
    json!({ "subagent": "helper", "prompt": format!("{CHILD_MARKER}: first task") })
}

fn continue_call(subagent: &str, thread_id: &str) -> Value {
    json!({ "subagent": subagent, "prompt": SECOND_TURN, "thread_id": thread_id })
}

/// The `child_thread_id` of the newest tool result in a parent request.
fn child_thread_in(body: &Value) -> String {
    let result = tool_results(body).pop().expect("a tool result");
    result["data"]["child_thread_id"]
        .as_str()
        .unwrap_or_else(|| panic!("no child_thread_id in {result}"))
        .to_string()
}

type ParentScript = dyn Fn(usize, &Value) -> String + Send + Sync;
type ChildScript = dyn Fn(&Value) -> (String, Duration) + Send + Sync;

/// The child records its thread and artifact directory with `bash` in each
/// turn, then answers. Its second turn answers `REMEMBERED` only when the
/// request still carries the first turn's answer.
fn child_reply(body: &Value, root: &std::path::Path) -> (String, Duration) {
    let raw = body.to_string();
    let results = tool_results(body).len();
    let second = raw.contains(SECOND_TURN);
    let file = root.join(if second { "turn2.env" } else { "turn1.env" });
    let record = format!(
        "echo \"$ZDX_THREAD_ID $ZDX_ARTIFACT_DIR\" > {}",
        file.display()
    );
    let reply = match (second, results) {
        (false, 0) | (true, 1) => tool_use_sse(
            &format!("toolu_child_{results}"),
            "bash",
            &json!({ "command": record, "description": "test command" }).to_string(),
        ),
        (false, _) => text_sse(FIRST_ANSWER),
        (true, _) if raw.contains(FIRST_ANSWER) => text_sse(REMEMBERED),
        (true, _) => text_sse("FORGOT"),
    };
    (reply, Duration::ZERO)
}

/// Mounts the shared mock and keeps every parent request body.
async fn start_mock(
    parent: Arc<ParentScript>,
    child: Arc<ChildScript>,
) -> (MockServer, Arc<Mutex<Vec<Value>>>) {
    let server = MockServer::start().await;
    let parent_bodies: Arc<Mutex<Vec<Value>>> = Arc::new(Mutex::new(Vec::new()));
    let bodies = Arc::clone(&parent_bodies);
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(move |req: &Request| {
            let body: Value = req.body_json().unwrap_or_default();
            let is_child = body["messages"]
                .as_array()
                .and_then(|messages| messages.first())
                .is_some_and(|message| message.to_string().contains(CHILD_MARKER));
            if is_child {
                let (reply, delay) = child(&body);
                return sse_response(&reply).set_delay(delay);
            }
            let step = tool_results(&body).len();
            let reply = parent(step, &body);
            bodies.lock().unwrap().push(body);
            sse_response(&reply)
        })
        .mount(&server)
        .await;
    (server, parent_bodies)
}

fn exec(zdx_home: &TempDir, root: &TempDir, server: &MockServer) -> tokio::process::Command {
    let mut cmd = tokio::process::Command::new(env!("CARGO_BIN_EXE_zdx"));
    cmd.env("ZDX_HOME", zdx_home.path())
        .env("ANTHROPIC_API_KEY", "test-api-key")
        .env("ANTHROPIC_BASE_URL", server.uri())
        .args([
            "--root",
            root.path().to_str().unwrap(),
            "--no-thread",
            "exec",
            "-m",
            MOCK_MODEL,
            "-p",
            "delegate",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    cmd
}

async fn run_to_exit(mut cmd: tokio::process::Command) {
    let status = timeout(Duration::from_mins(1), cmd.status())
        .await
        .expect("zdx exec never exited")
        .expect("run zdx exec");
    assert!(status.success(), "zdx exec failed: {status}");
}

fn bodies(shared: &Arc<Mutex<Vec<Value>>>) -> Vec<Value> {
    shared.lock().unwrap().clone()
}

fn read_env(root: &TempDir, name: &str) -> (String, String) {
    let raw = std::fs::read_to_string(root.path().join(name))
        .unwrap_or_else(|err| panic!("{name}: {err}"));
    let mut parts = raw.split_whitespace();
    (
        parts.next().unwrap_or_default().to_string(),
        parts.next().unwrap_or_default().to_string(),
    )
}

/// Two calls continue one child: the first returns its thread, the second
/// passes it back and gets an answer built on the first turn, in the same
/// thread and artifact directory.
async fn continues_with_full_history(extra_args: &[&str]) {
    let (zdx_home, root) = workspace();
    let parent: Arc<ParentScript> = Arc::new(|step, body| match step {
        0 => invoke(0, &first_call()),
        1 => invoke(1, &continue_call("helper", &child_thread_in(body))),
        _ => text_sse("parent done"),
    });
    let child_root = root.path().to_path_buf();
    let (server, shared) =
        start_mock(parent, Arc::new(move |body| child_reply(body, &child_root))).await;

    let mut cmd = exec(&zdx_home, &root, &server);
    cmd.args(extra_args);
    run_to_exit(cmd).await;

    let bodies = bodies(&shared);
    assert_eq!(bodies.len(), 3, "parent requests: {bodies:?}");
    let first = tool_results(&bodies[1]).pop().unwrap();
    assert_eq!(first["data"]["result"], FIRST_ANSWER, "{first}");
    let thread = first["data"]["child_thread_id"]
        .as_str()
        .unwrap()
        .to_string();

    let second = tool_results(&bodies[2]).pop().unwrap();
    assert_eq!(second["ok"], true, "{second}");
    assert_eq!(second["data"]["result"], REMEMBERED, "{second}");
    assert_eq!(
        second["data"]["child_thread_id"],
        thread.as_str(),
        "{second}"
    );

    let turn1 = read_env(&root, "turn1.env");
    let turn2 = read_env(&root, "turn2.env");
    assert_eq!(turn1.0, thread);
    assert_eq!(turn1, turn2, "same thread and artifact directory");
    assert!(turn1.1.ends_with(&thread), "{turn1:?}");

    let transcript = std::fs::read_to_string(
        zdx_home
            .path()
            .join("threads")
            .join(format!("{thread}.jsonl")),
    )
    .unwrap();
    let users = transcript
        .lines()
        .filter(|line| line.contains("\"role\":\"user\""))
        .count();
    assert_eq!(users, 2, "both turns in one thread: {transcript}");
    let meta: Value = serde_json::from_str(transcript.lines().next().unwrap()).unwrap();
    assert_eq!(meta["origin_kind"], "subagent", "{meta}");
    assert_eq!(meta["subagent_name"], "helper", "{meta}");
}

/// A normal call that finishes within the handoff bound.
#[tokio::test]
async fn foreground_call_returns_its_thread_and_continues() {
    if !can_bind_localhost() {
        eprintln!("Skipping: cannot bind localhost TCP port in this environment.");
        return;
    }
    continues_with_full_history(&[]).await;
}

/// The blocking path, with background execution off.
#[tokio::test]
async fn blocking_call_returns_its_thread_and_continues() {
    if !can_bind_localhost() {
        eprintln!("Skipping: cannot bind localhost TCP port in this environment.");
        return;
    }
    continues_with_full_history(&["--no-background-commands"]).await;
}

/// A child started with `background: true` is continued through the
/// `child_thread_id` of its handle once its result is collected.
#[tokio::test]
async fn background_child_continues_by_its_handle_id() {
    if !can_bind_localhost() {
        eprintln!("Skipping: cannot bind localhost TCP port in this environment.");
        return;
    }
    let (zdx_home, root) = workspace();
    let parent: Arc<ParentScript> = Arc::new(|step, body| match step {
        0 => {
            let mut input = first_call();
            input["background"] = json!(true);
            invoke(0, &input)
        }
        1 => tool_use_sse(
            "toolu_collect",
            "background_output",
            &json!({
                "bg_id": tool_results(body).pop().unwrap()["data"]["bg_id"],
                "wait_secs": 30,
                "wait_for": "exit",
            })
            .to_string(),
        ),
        2 => invoke(2, &continue_call("helper", &child_thread_in(body))),
        _ => text_sse("parent done"),
    });
    let child_root = root.path().to_path_buf();
    let (server, shared) =
        start_mock(parent, Arc::new(move |body| child_reply(body, &child_root))).await;

    run_to_exit(exec(&zdx_home, &root, &server)).await;

    let bodies = bodies(&shared);
    assert_eq!(bodies.len(), 4, "parent requests: {bodies:?}");
    let handle = tool_results(&bodies[1]).pop().unwrap();
    let thread = handle["data"]["child_thread_id"]
        .as_str()
        .unwrap()
        .to_string();
    let collected = tool_results(&bodies[2]).pop().unwrap();
    assert_eq!(collected["data"]["result"], FIRST_ANSWER, "{collected}");
    assert_eq!(collected["data"]["child_thread_id"], thread.as_str());

    let second = tool_results(&bodies[3]).pop().unwrap();
    assert_eq!(second["data"]["result"], REMEMBERED, "{second}");
    assert_eq!(
        second["data"]["child_thread_id"],
        thread.as_str(),
        "{second}"
    );
    assert_eq!(read_env(&root, "turn1.env"), read_env(&root, "turn2.env"));
}

/// A missing thread, a thread of another subagent, and a thread that is still
/// running are refused before anything runs against them.
#[tokio::test]
async fn invalid_continuations_are_refused() {
    if !can_bind_localhost() {
        eprintln!("Skipping: cannot bind localhost TCP port in this environment.");
        return;
    }
    let (zdx_home, root) = workspace();
    let parent: Arc<ParentScript> = Arc::new(|step, body| {
        // The first result carries the background handle of a slow child.
        let thread = || {
            tool_results(body).first().unwrap()["data"]["child_thread_id"]
                .as_str()
                .unwrap()
                .to_string()
        };
        match step {
            0 => {
                let mut input = first_call();
                input["background"] = json!(true);
                invoke(0, &input)
            }
            1 => invoke(1, &continue_call("helper", &thread())),
            2 => invoke(2, &continue_call("helper", "no-such-thread")),
            3 => invoke(3, &continue_call("other", &thread())),
            _ => text_sse("parent done"),
        }
    });
    // The child takes long enough to still be running during the checks; the
    // parent's turn end stops it.
    let child = Arc::new(|_: &Value| (text_sse(FIRST_ANSWER), Duration::from_secs(20)));
    let (server, shared) = start_mock(parent, child).await;

    run_to_exit(exec(&zdx_home, &root, &server)).await;

    let bodies = bodies(&shared);
    assert_eq!(bodies.len(), 5, "parent requests: {bodies:?}");
    let code = |index: usize| {
        let result = tool_results(&bodies[index]).pop().unwrap();
        assert_eq!(result["ok"], false, "{result}");
        result["error"]["code"].as_str().unwrap().to_string()
    };
    assert_eq!(code(2), "thread_busy");
    assert_eq!(code(3), "thread_not_found");
    assert_eq!(code(4), "thread_mismatch");
}
