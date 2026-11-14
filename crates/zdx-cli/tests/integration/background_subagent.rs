//! End-to-end tests for `invoke_subagent` with `background: true`: the handle,
//! progress and result through `background_output`, `background_kill`, and the
//! rule that a background subagent never outlives the turn or the process
//! that started it.
//!
//! The parent and the child share one mock provider. Child requests are told
//! apart by a marker in their first message; parent requests are scripted by
//! how many tool results they already carry.

use std::process::Stdio;
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::time::{Duration, timeout};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request};

use crate::fixtures::{MOCK_MODEL, sse_response, text_sse, tool_use_sse};

const CHILD_MARKER: &str = "CHILD-TASK-bg-subagent";

fn can_bind_localhost() -> bool {
    std::net::TcpListener::bind("127.0.0.1:0").is_ok()
}

#[cfg(unix)]
fn process_alive(pid: i32) -> bool {
    if unsafe { libc::kill(pid, 0) } == 0 {
        return true;
    }
    std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

#[cfg(unix)]
async fn wait_gone(pid: i32, what: &str) {
    timeout(Duration::from_secs(15), async {
        while process_alive(pid) {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("{what} (pid {pid}) is still running"));
}

async fn wait_for_file(path: &std::path::Path) -> String {
    timeout(Duration::from_secs(30), async {
        loop {
            if let Ok(raw) = std::fs::read_to_string(path)
                && !raw.trim().is_empty()
            {
                break raw.trim().to_string();
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("{} never appeared", path.display()))
}

/// A workspace with two subagents that have `bash`: `helper` does not opt
/// into background commands, `drainer` does.
fn workspace() -> (TempDir, TempDir) {
    let zdx_home = TempDir::new().unwrap();
    let root = TempDir::new().unwrap();
    let dir = root.path().join(".zdx").join("subagents");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("helper.md"),
        "---\ndescription: Helper\ntools:\n  - bash\n  - read\n---\nYou help.\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("drainer.md"),
        "---\ndescription: Drainer\nbackground_commands: true\ntools:\n  - bash\n---\nYou help.\n",
    )
    .unwrap();
    (zdx_home, root)
}

fn invoke_background() -> String {
    invoke_background_of("helper")
}

fn invoke_background_of(subagent: &str) -> String {
    tool_use_sse(
        "toolu_invoke_bg",
        "invoke_subagent",
        &json!({
            "subagent": subagent,
            "prompt": format!("{CHILD_MARKER}: do the task"),
            "background": true,
        })
        .to_string(),
    )
}

fn bash(id: &str, command: &str) -> String {
    tool_use_sse(id, "bash", &json!({ "command": command }).to_string())
}

/// The first `bg-<uuid>` in a request body, i.e. the handle the parent got.
fn bg_id_in(body: &str) -> Option<String> {
    let mut rest = body;
    while let Some(at) = rest.find("bg-") {
        let candidate = &rest[at + 3..];
        if candidate.len() >= 36
            && candidate[..36]
                .chars()
                .all(|c| c.is_ascii_hexdigit() || c == '-')
        {
            return Some(format!("bg-{}", &candidate[..36]));
        }
        rest = &rest[at + 3..];
    }
    None
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

type ParentScript = dyn Fn(usize, &str) -> String + Send + Sync;
type ChildScript = dyn Fn(usize) -> String + Send + Sync;
/// A child script that also delays its reply, to make a child slow without
/// sleeping in a command.
type TimedChildScript = dyn Fn(usize) -> (String, Duration) + Send + Sync;

/// Mounts the shared mock. Parent and child replies are chosen by how many
/// tool results the request already carries; every parent request body is
/// kept.
async fn start_mock(
    parent: Arc<ParentScript>,
    child_reply: impl Into<ChildReply>,
) -> (MockServer, Arc<Mutex<Vec<Value>>>) {
    let child: Arc<TimedChildScript> = match child_reply.into() {
        ChildReply::Fixed(reply) => Arc::new(move |_| (reply.clone(), Duration::ZERO)),
        ChildReply::Script(script) => Arc::new(move |step| (script(step), Duration::ZERO)),
        ChildReply::Timed(script) => script,
    };
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
            let step = tool_results(&body).len();
            if is_child {
                let (reply, delay) = child(step);
                return sse_response(&reply).set_delay(delay);
            }
            let raw = String::from_utf8_lossy(&req.body).to_string();
            bodies.lock().unwrap().push(body);
            sse_response(&parent(step, &raw))
        })
        .mount(&server)
        .await;
    (server, parent_bodies)
}

enum ChildReply {
    Fixed(String),
    Script(Arc<ChildScript>),
    Timed(Arc<TimedChildScript>),
}

impl From<Arc<TimedChildScript>> for ChildReply {
    fn from(script: Arc<TimedChildScript>) -> Self {
        Self::Timed(script)
    }
}

impl From<String> for ChildReply {
    fn from(reply: String) -> Self {
        Self::Fixed(reply)
    }
}

impl From<Arc<ChildScript>> for ChildReply {
    fn from(script: Arc<ChildScript>) -> Self {
        Self::Script(script)
    }
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

async fn run_to_exit(mut cmd: tokio::process::Command) -> std::time::Duration {
    let started = std::time::Instant::now();
    let status = timeout(Duration::from_mins(1), cmd.status())
        .await
        .expect("zdx exec never exited")
        .expect("run zdx exec");
    assert!(status.success(), "zdx exec failed: {status}");
    started.elapsed()
}

fn last_result(bodies: &Arc<Mutex<Vec<Value>>>) -> Value {
    let bodies = bodies.lock().unwrap();
    let last = bodies.last().expect("a parent request");
    tool_results(last).pop().expect("a tool result")
}

/// The handle comes back at once with a `bg_id` and the child's thread; an exit
/// wait on it returns the child's final answer as `result`.
#[tokio::test]
async fn background_subagent_returns_a_handle_and_its_result() {
    if !can_bind_localhost() {
        eprintln!("Skipping: cannot bind localhost TCP port in this environment.");
        return;
    }
    let (zdx_home, root) = workspace();
    let parent: Arc<ParentScript> = Arc::new(|step, body| match step {
        0 => invoke_background(),
        1 => tool_use_sse(
            "toolu_collect",
            "background_output",
            &json!({
                "bg_id": bg_id_in(body).expect("handle in the invoke result"),
                "wait_secs": 30,
                "wait_for": "exit",
            })
            .to_string(),
        ),
        _ => text_sse("parent done"),
    });
    let (server, bodies) = start_mock(parent, text_sse("CHILD-RESULT-42")).await;

    run_to_exit(exec(&zdx_home, &root, &server)).await;

    let bodies_now = bodies.lock().unwrap().clone();
    assert_eq!(bodies_now.len(), 3, "parent requests: {bodies_now:?}");
    let handle = tool_results(&bodies_now[1]).pop().unwrap();
    assert_eq!(handle["ok"], true, "{handle}");
    assert_eq!(handle["data"]["status"], "running");
    assert_eq!(handle["data"]["subagent"], "helper");
    let child_thread = handle["data"]["child_thread_id"]
        .as_str()
        .expect("child thread id")
        .to_string();

    let collected = tool_results(&bodies_now[2]).pop().unwrap();
    let data = &collected["data"];
    assert_eq!(data["kind"], "subagent", "{collected}");
    assert_eq!(data["turn"], "completed", "{collected}");
    assert_eq!(data["result"], "CHILD-RESULT-42", "{collected}");
    // The answer is published when the child's turn ends, which is usually a
    // moment before its process exits; either is a valid end of the wait.
    assert!(
        data["waited_for"] == "result" || data["waited_for"] == "exit",
        "{collected}"
    );
    assert_eq!(data["child_thread_id"], child_thread.as_str());

    let meta = std::fs::read_to_string(
        zdx_home
            .path()
            .join("threads")
            .join(format!("{child_thread}.jsonl")),
    )
    .expect("child thread persisted");
    let first: Value = serde_json::from_str(meta.lines().next().unwrap()).unwrap();
    assert_eq!(first["origin_kind"], "subagent", "{first}");
    assert_eq!(first["subagent_name"], "helper", "{first}");
    assert!(meta.contains("CHILD-RESULT-42"));
}

/// `background_output` reports the child's running tool, and
/// `background_kill` stops the child together with the command it runs.
#[cfg(unix)]
#[tokio::test]
async fn background_subagent_reports_progress_and_can_be_killed() {
    if !can_bind_localhost() {
        eprintln!("Skipping: cannot bind localhost TCP port in this environment.");
        return;
    }
    let (zdx_home, root) = workspace();
    let pidfile = root.path().join("child-command.pid");
    let child_reply = bash(
        "toolu_child_bash",
        &format!("echo $$ > {}; sleep 60", pidfile.display()),
    );
    let wait_pid = format!("while [ ! -s {0} ]; do sleep 0.05; done", pidfile.display());
    let parent: Arc<ParentScript> = Arc::new(move |step, body| {
        let bg_id = || bg_id_in(body).expect("handle in an earlier result");
        match step {
            0 => invoke_background(),
            1 => bash("toolu_wait_pid", &wait_pid),
            2 => tool_use_sse(
                "toolu_peek",
                "background_output",
                &json!({ "bg_id": bg_id() }).to_string(),
            ),
            3 => tool_use_sse(
                "toolu_kill",
                "background_kill",
                &json!({ "bg_id": bg_id() }).to_string(),
            ),
            4 => tool_use_sse(
                "toolu_after",
                "background_output",
                &json!({ "bg_id": bg_id() }).to_string(),
            ),
            _ => text_sse("parent done"),
        }
    });
    let (server, bodies) = start_mock(parent, child_reply).await;

    let elapsed = run_to_exit(exec(&zdx_home, &root, &server)).await;
    assert!(elapsed < Duration::from_secs(45), "took {elapsed:?}");

    let bodies_now = bodies.lock().unwrap().clone();
    assert_eq!(bodies_now.len(), 6, "parent requests: {bodies_now:?}");
    let peek = tool_results(&bodies_now[3]).pop().unwrap();
    assert_eq!(peek["data"]["status"], "running", "{peek}");
    assert_eq!(peek["data"]["turn"], "running", "{peek}");
    assert_eq!(peek["data"]["current_tool"], "bash", "{peek}");
    assert!(
        peek["data"]["current_tool_input"]
            .as_str()
            .unwrap_or_default()
            .contains("sleep 60"),
        "{peek}"
    );

    let killed = tool_results(&bodies_now[4]).pop().unwrap();
    assert_eq!(killed["data"]["status"], "killed", "{killed}");

    let after = tool_results(&bodies_now[5]).pop().unwrap();
    assert_eq!(after["data"]["status"], "exited", "{after}");
    assert_eq!(after["data"]["turn"], "stopped", "{after}");
    assert!(after["data"].get("result").is_none(), "{after}");

    let command_pid: i32 = wait_for_file(&pidfile).await.parse().unwrap();
    wait_gone(command_pid, "the killed subagent's command").await;
}

/// The turn that started a background subagent stops it when the turn ends,
/// without the model asking, and before `exec` waits for an unrelated Bash
/// job that was moved to the background, which still runs to completion.
#[cfg(unix)]
#[tokio::test]
async fn turn_end_stops_the_subagent_but_not_an_unrelated_bash_job() {
    if !can_bind_localhost() {
        eprintln!("Skipping: cannot bind localhost TCP port in this environment.");
        return;
    }
    let (zdx_home, root) = workspace();
    let pidfile = root.path().join("child-command.pid");
    let verdict = root.path().join("verdict.txt");
    let child_reply = bash(
        "toolu_child_bash",
        &format!("echo $$ > {}; sleep 60", pidfile.display()),
    );
    let wait_pid = format!("while [ ! -s {0} ]; do sleep 0.05; done", pidfile.display());
    // Outlives its own 1s bound (`timeout_secs`, so the child's command limit
    // stays at the default), so it is moved to the background and drained at
    // exit. It records whether the subagent's command was still alive three
    // seconds after the turn moved on, i.e. while the drain was running.
    let judge = format!(
        "sleep 3; if kill -0 $(cat {pid}) 2>/dev/null; then echo alive > {out}; else echo stopped > {out}; fi",
        pid = pidfile.display(),
        out = verdict.display()
    );
    let parent: Arc<ParentScript> = Arc::new(move |step, _| match step {
        0 => invoke_background(),
        1 => tool_use_sse(
            "toolu_wait_pid",
            "bash",
            &json!({ "command": wait_pid, "timeout_secs": 30 }).to_string(),
        ),
        2 => tool_use_sse(
            "toolu_judge",
            "bash",
            &json!({ "command": judge, "timeout_secs": 1 }).to_string(),
        ),
        _ => text_sse("parent done without collecting"),
    });
    let (server, bodies) = start_mock(parent, child_reply).await;

    run_to_exit(exec(&zdx_home, &root, &server)).await;

    let judged = last_result(&bodies);
    assert_eq!(judged["data"]["backgrounded"], true, "{judged}");
    assert_eq!(
        std::fs::read_to_string(&verdict).unwrap().trim(),
        "stopped",
        "the subagent's command was still running after its turn ended"
    );
    let command_pid: i32 = wait_for_file(&pidfile).await.parse().unwrap();
    assert!(!process_alive(command_pid));
}

/// When the owner process dies (a cancelled worker is terminated through its
/// lease), the background subagent and its command die with it.
#[cfg(unix)]
#[tokio::test]
async fn owner_death_stops_the_subagent_and_its_command() {
    if !can_bind_localhost() {
        eprintln!("Skipping: cannot bind localhost TCP port in this environment.");
        return;
    }
    let (zdx_home, root) = workspace();
    let pidfile = root.path().join("child-command.pid");
    let child_reply = bash(
        "toolu_child_bash",
        &format!("echo $$ > {}; sleep 60", pidfile.display()),
    );
    let busy = format!(
        "while [ ! -s {0} ]; do sleep 0.05; done; sleep 60",
        pidfile.display()
    );
    let parent: Arc<ParentScript> = Arc::new(move |step, _| match step {
        0 => invoke_background(),
        1 => bash("toolu_busy", &busy),
        _ => text_sse("parent done"),
    });
    let (server, _bodies) = start_mock(parent, child_reply).await;

    let mut child = exec(&zdx_home, &root, &server).spawn().expect("spawn exec");
    let owner_pid = child.id().expect("exec pid") as i32;
    let command_pid: i32 = wait_for_file(&pidfile).await.parse().unwrap();

    let registry = zdx_home.path().join("run").join("background");
    let subagent_pid = std::fs::read_dir(&registry)
        .expect("registry dir")
        .flatten()
        .filter_map(|entry| std::fs::read_to_string(entry.path()).ok())
        .filter_map(|raw| serde_json::from_str::<Value>(&raw).ok())
        .find(|rec| rec["kind"] == "subagent")
        .and_then(|rec| rec["pid"].as_i64())
        .expect("subagent job registered") as i32;
    assert!(process_alive(subagent_pid));

    unsafe { libc::kill(owner_pid, libc::SIGTERM) };
    let _ = timeout(Duration::from_secs(15), child.wait())
        .await
        .expect("owner did not exit");

    wait_gone(subagent_pid, "the background subagent").await;
    wait_gone(command_pid, "the background subagent's command").await;
}

/// Without background execution, `invoke_subagent` has no `background`
/// option, the management tools are absent, and `background: true` is
/// refused before anything starts.
#[tokio::test]
async fn background_subagent_is_refused_without_background_execution() {
    if !can_bind_localhost() {
        eprintln!("Skipping: cannot bind localhost TCP port in this environment.");
        return;
    }
    let (zdx_home, root) = workspace();
    let parent: Arc<ParentScript> = Arc::new(|step, _| match step {
        0 => invoke_background(),
        _ => text_sse("parent done"),
    });
    let (server, bodies) = start_mock(parent, text_sse("must not run")).await;

    let mut cmd = exec(&zdx_home, &root, &server);
    cmd.arg("--no-background-commands");
    run_to_exit(cmd).await;

    let bodies_now = bodies.lock().unwrap().clone();
    let tools = bodies_now[0]["tools"].as_array().expect("tools");
    assert!(
        !tools.iter().any(|tool| tool["name"]
            .as_str()
            .unwrap_or_default()
            .starts_with("background_")),
        "management tools offered"
    );
    let invoke = tools
        .iter()
        .find(|tool| {
            tool["name"]
                .as_str()
                .unwrap_or_default()
                .eq_ignore_ascii_case("invoke_subagent")
        })
        .expect("invoke_subagent offered");
    assert!(
        invoke["input_schema"]["properties"]
            .get("background")
            .is_none()
    );

    let refused = last_result(&bodies);
    assert_eq!(refused["ok"], false, "{refused}");
    assert_eq!(refused["error"]["code"], "background_disabled", "{refused}");
    assert!(
        !zdx_home.path().join("threads").exists()
            || std::fs::read_dir(zdx_home.path().join("threads"))
                .unwrap()
                .next()
                .is_none(),
        "no child thread may be created"
    );
}

/// A child that opted into background commands answers, then keeps running
/// while it waits for its own backgrounded command. The answer is published
/// at once: an exit wait returns it with `waited_for: "result"` while the
/// process still runs, and it is still there after the child is stopped.
#[cfg(unix)]
#[tokio::test]
async fn answer_is_available_while_the_child_drains_and_after_it_is_stopped() {
    if !can_bind_localhost() {
        eprintln!("Skipping: cannot bind localhost TCP port in this environment.");
        return;
    }
    let (zdx_home, root) = workspace();
    let pidfile = root.path().join("drained-command.pid");
    let slow = json!({
        "command": format!("echo $$ > {}; sleep 60", pidfile.display()),
        "timeout_secs": 1,
    })
    .to_string();
    let child: Arc<ChildScript> = Arc::new(move |step| match step {
        0 => tool_use_sse("toolu_child_slow", "bash", &slow),
        _ => text_sse("CHILD-ANSWER-7"),
    });
    let parent: Arc<ParentScript> = Arc::new(|step, body| {
        let bg_id = || bg_id_in(body).expect("handle in an earlier result");
        match step {
            0 => invoke_background_of("drainer"),
            1 => tool_use_sse(
                "toolu_collect",
                "background_output",
                &json!({ "bg_id": bg_id(), "wait_secs": 30, "wait_for": "exit" }).to_string(),
            ),
            2 => tool_use_sse(
                "toolu_kill",
                "background_kill",
                &json!({ "bg_id": bg_id() }).to_string(),
            ),
            3 => tool_use_sse(
                "toolu_after",
                "background_output",
                &json!({ "bg_id": bg_id() }).to_string(),
            ),
            _ => text_sse("parent done"),
        }
    });
    let (server, bodies) = start_mock(parent, child).await;

    let elapsed = run_to_exit(exec(&zdx_home, &root, &server)).await;
    assert!(elapsed < Duration::from_secs(30), "took {elapsed:?}");

    let bodies_now = bodies.lock().unwrap().clone();
    assert_eq!(bodies_now.len(), 5, "parent requests: {bodies_now:?}");
    let collected = tool_results(&bodies_now[2]).pop().unwrap();
    let data = &collected["data"];
    assert_eq!(data["waited_for"], "result", "{collected}");
    assert_eq!(data["status"], "running", "{collected}");
    assert_eq!(data["turn"], "completed", "{collected}");
    assert_eq!(data["result"], "CHILD-ANSWER-7", "{collected}");
    assert!(
        data["message"].as_str().unwrap().contains("not exited yet"),
        "{collected}"
    );

    let killed = tool_results(&bodies_now[3]).pop().unwrap();
    assert_eq!(killed["data"]["status"], "killed", "{killed}");

    let after = tool_results(&bodies_now[4]).pop().unwrap();
    assert_eq!(after["data"]["status"], "exited", "{after}");
    assert_eq!(after["data"]["turn"], "completed", "{after}");
    assert_eq!(after["data"]["result"], "CHILD-ANSWER-7", "{after}");

    let command_pid: i32 = wait_for_file(&pidfile).await.parse().unwrap();
    wait_gone(command_pid, "the drained command of the stopped subagent").await;
}

/// A job's deadline (`timeout_secs`) stops a background subagent just as it
/// stops a blocking one, and the view says that is what happened.
#[cfg(unix)]
#[tokio::test]
async fn job_deadline_stops_a_background_subagent() {
    if !can_bind_localhost() {
        eprintln!("Skipping: cannot bind localhost TCP port in this environment.");
        return;
    }
    let (zdx_home, root) = workspace();
    let jobs = zdx_home.path().join("jobs");
    std::fs::create_dir_all(&jobs).unwrap();
    std::fs::write(
        jobs.join("bounded.md"),
        format!("---\nmodel: \"{MOCK_MODEL}\"\ntimeout_secs: 3\n---\nDelegate the task.\n"),
    )
    .unwrap();
    let pidfile = root.path().join("child-command.pid");
    let child_reply = bash(
        "toolu_child_bash",
        &format!("echo $$ > {}; sleep 60", pidfile.display()),
    );
    let parent: Arc<ParentScript> = Arc::new(|step, body| match step {
        0 => invoke_background(),
        1 => tool_use_sse(
            "toolu_collect",
            "background_output",
            &json!({
                "bg_id": bg_id_in(body).expect("handle in the invoke result"),
                "wait_secs": 60,
                "wait_for": "exit",
            })
            .to_string(),
        ),
        _ => text_sse("parent done"),
    });
    let (server, bodies) = start_mock(parent, child_reply).await;

    let mut cmd = tokio::process::Command::new(env!("CARGO_BIN_EXE_zdx"));
    cmd.env("ZDX_HOME", zdx_home.path())
        .env("ANTHROPIC_API_KEY", "test-api-key")
        .env("ANTHROPIC_BASE_URL", server.uri())
        .current_dir(root.path())
        .args([
            "--root",
            root.path().to_str().unwrap(),
            "--no-thread",
            "jobs",
            "run",
            "bounded",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let elapsed = run_to_exit(cmd).await;
    assert!(elapsed < Duration::from_secs(30), "took {elapsed:?}");

    let bodies_now = bodies.lock().unwrap().clone();
    assert_eq!(bodies_now.len(), 3, "parent requests: {bodies_now:?}");
    let handle = tool_results(&bodies_now[1]).pop().unwrap();
    assert_eq!(handle["data"]["deadline_secs"], 3, "{handle}");
    let collected = tool_results(&bodies_now[2]).pop().unwrap();
    let data = &collected["data"];
    assert_eq!(data["waited_for"], "exit", "{collected}");
    assert_eq!(data["status"], "exited", "{collected}");
    assert_eq!(data["turn"], "timed_out", "{collected}");
    assert!(
        data["message"].as_str().unwrap().contains("3 seconds"),
        "{collected}"
    );

    if let Ok(raw) = std::fs::read_to_string(&pidfile) {
        let command_pid: i32 = raw.trim().parse().unwrap();
        wait_gone(command_pid, "the timed-out subagent's command").await;
    }
}

// ---- Automatic handoff of normal (non-background) calls ----

fn invoke_normal() -> String {
    tool_use_sse(
        "toolu_invoke_normal",
        "invoke_subagent",
        &json!({
            "subagent": "helper",
            "prompt": format!("{CHILD_MARKER}: do the task"),
        })
        .to_string(),
    )
}

fn set_bound(zdx_home: &TempDir, secs: u32) {
    std::fs::write(
        zdx_home.path().join("config.toml"),
        format!("bash_foreground_bound_secs = {secs}\n"),
    )
    .unwrap();
}

/// A child that answers `answer` after `delay`, counting how many times a
/// child conversation starts (its first request), so a restart would show.
fn slow_child(
    answer: &'static str,
    delay: Duration,
    starts: Arc<std::sync::atomic::AtomicUsize>,
) -> Arc<TimedChildScript> {
    Arc::new(move |step| {
        if step == 0 {
            starts.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }
        (text_sse(answer), delay)
    })
}

fn subagent_markers(zdx_home: &TempDir) -> Vec<Value> {
    std::fs::read_dir(zdx_home.path().join("run").join("background"))
        .map(|entries| {
            entries
                .flatten()
                .filter_map(|entry| std::fs::read_to_string(entry.path()).ok())
                .filter_map(|raw| serde_json::from_str::<Value>(&raw).ok())
                .filter(|rec| rec["kind"] == "subagent")
                .collect()
        })
        .unwrap_or_default()
}

fn log_files(zdx_home: &TempDir) -> Vec<std::path::PathBuf> {
    std::fs::read_dir(zdx_home.path().join("run").join("background").join("logs"))
        .map(|entries| entries.flatten().map(|entry| entry.path()).collect())
        .unwrap_or_default()
}

/// A normal call whose subagent finishes within the bound returns the answer
/// and its thread, exactly like a blocking call, and leaves no job behind.
#[tokio::test]
async fn fast_normal_call_returns_the_answer_and_registers_nothing() {
    if !can_bind_localhost() {
        eprintln!("Skipping: cannot bind localhost TCP port in this environment.");
        return;
    }
    let (zdx_home, root) = workspace();
    set_bound(&zdx_home, 30);
    let parent: Arc<ParentScript> = Arc::new(|step, _| match step {
        0 => invoke_normal(),
        _ => text_sse("parent done"),
    });
    let (server, bodies) = start_mock(parent, text_sse("CHILD-FAST-ANSWER")).await;

    run_to_exit(exec(&zdx_home, &root, &server)).await;

    let answer = last_result(&bodies);
    assert_eq!(answer["ok"], true, "{answer}");
    assert_eq!(answer["data"]["result"], "CHILD-FAST-ANSWER", "{answer}");
    assert!(answer["data"]["child_thread_id"].is_string(), "{answer}");
    assert!(subagent_markers(&zdx_home).is_empty());
    assert!(
        log_files(&zdx_home).is_empty(),
        "{:?}",
        log_files(&zdx_home)
    );
}

/// A normal call whose subagent outruns the bound returns a background
/// handle instead of waiting; the same child keeps running (it is never
/// restarted), the progress made before the handoff is kept, and its answer
/// is collected later through `background_output`.
#[tokio::test]
async fn slow_normal_call_hands_off_the_same_child() {
    if !can_bind_localhost() {
        eprintln!("Skipping: cannot bind localhost TCP port in this environment.");
        return;
    }
    let (zdx_home, root) = workspace();
    // The bound must cover a child process start, one model request, and
    // one Bash call; 1s did not under heavy load (base revision too: 7 of 10
    // failed with every core busy). The answer comes well after the bound.
    set_bound(&zdx_home, 3);
    let starts = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let child_starts = Arc::clone(&starts);
    // A quick tool call first (progress before the handoff), then an answer
    // that arrives well after the bound.
    let child: Arc<TimedChildScript> = Arc::new(move |step| match step {
        0 => {
            child_starts.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            (bash("toolu_child_echo", "echo early"), Duration::ZERO)
        }
        _ => (text_sse("CHILD-SLOW-ANSWER"), Duration::from_secs(8)),
    });
    let parent: Arc<ParentScript> = Arc::new(|step, body| match step {
        0 => invoke_normal(),
        1 => tool_use_sse(
            "toolu_collect",
            "background_output",
            &json!({
                "bg_id": bg_id_in(body).expect("handle in the handoff result"),
                "wait_secs": 30,
                "wait_for": "exit",
            })
            .to_string(),
        ),
        _ => text_sse("parent done"),
    });
    let (server, bodies) = start_mock(parent, child).await;

    run_to_exit(exec(&zdx_home, &root, &server)).await;

    let bodies_now = bodies.lock().unwrap().clone();
    assert_eq!(bodies_now.len(), 3, "parent requests: {bodies_now:?}");
    let handoff = tool_results(&bodies_now[1]).pop().unwrap();
    let data = &handoff["data"];
    assert_eq!(data["backgrounded"], true, "{handoff}");
    assert_eq!(data["kind"], "subagent", "{handoff}");
    assert_eq!(data["status"], "running", "{handoff}");
    assert_eq!(data["turn"], "running", "{handoff}");
    assert_eq!(data["elapsed_secs"], 3, "{handoff}");
    assert_eq!(
        data["tool_calls"], 1,
        "progress before the handoff is kept: {handoff}"
    );
    assert!(data["child_thread_id"].is_string(), "{handoff}");
    assert!(
        data["message"]
            .as_str()
            .unwrap()
            .contains("do NOT invoke it again"),
        "{handoff}"
    );

    let collected = tool_results(&bodies_now[2]).pop().unwrap();
    assert_eq!(collected["data"]["turn"], "completed", "{collected}");
    assert_eq!(
        collected["data"]["result"], "CHILD-SLOW-ANSWER",
        "{collected}"
    );
    assert_eq!(
        collected["data"]["child_thread_id"], data["child_thread_id"],
        "{collected}"
    );
    assert_eq!(
        starts.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "the child must never be restarted"
    );
}

/// After a handoff, the caller can stop the subagent with `background_kill`.
#[cfg(unix)]
#[tokio::test]
async fn handed_off_subagent_can_be_killed() {
    if !can_bind_localhost() {
        eprintln!("Skipping: cannot bind localhost TCP port in this environment.");
        return;
    }
    let (zdx_home, root) = workspace();
    set_bound(&zdx_home, 1);
    let starts = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let child = slow_child("NEVER", Duration::from_mins(1), starts);
    let parent: Arc<ParentScript> = Arc::new(|step, body| {
        let bg_id = || bg_id_in(body).expect("handle in an earlier result");
        match step {
            0 => invoke_normal(),
            1 => tool_use_sse(
                "toolu_kill",
                "background_kill",
                &json!({ "bg_id": bg_id() }).to_string(),
            ),
            2 => tool_use_sse(
                "toolu_after",
                "background_output",
                &json!({ "bg_id": bg_id() }).to_string(),
            ),
            _ => text_sse("parent done"),
        }
    });
    let (server, bodies) = start_mock(parent, child).await;

    let elapsed = run_to_exit(exec(&zdx_home, &root, &server)).await;
    assert!(elapsed < Duration::from_secs(30), "took {elapsed:?}");

    let bodies_now = bodies.lock().unwrap().clone();
    assert_eq!(bodies_now.len(), 4, "parent requests: {bodies_now:?}");
    assert_eq!(
        tool_results(&bodies_now[1]).pop().unwrap()["data"]["backgrounded"],
        true
    );
    let killed = tool_results(&bodies_now[2]).pop().unwrap();
    assert_eq!(killed["data"]["status"], "killed", "{killed}");
    let after = tool_results(&bodies_now[3]).pop().unwrap();
    assert_eq!(after["data"]["status"], "exited", "{after}");
    assert_eq!(after["data"]["turn"], "stopped", "{after}");

    let marker = subagent_markers(&zdx_home).pop().expect("subagent marker");
    wait_gone(
        marker["pid"].as_i64().unwrap() as i32,
        "the killed subagent",
    )
    .await;
}

/// A handed-off subagent the caller never collects is stopped when the turn
/// ends; the process does not wait for it.
#[cfg(unix)]
#[tokio::test]
async fn handed_off_subagent_is_stopped_when_the_turn_ends() {
    if !can_bind_localhost() {
        eprintln!("Skipping: cannot bind localhost TCP port in this environment.");
        return;
    }
    let (zdx_home, root) = workspace();
    set_bound(&zdx_home, 1);
    let starts = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let child = slow_child("NEVER", Duration::from_mins(1), starts);
    let parent: Arc<ParentScript> = Arc::new(|step, _| match step {
        0 => invoke_normal(),
        _ => text_sse("parent done without collecting"),
    });
    let (server, bodies) = start_mock(parent, child).await;

    let elapsed = run_to_exit(exec(&zdx_home, &root, &server)).await;
    assert!(
        elapsed < Duration::from_secs(30),
        "exec waited for the uncollected subagent: {elapsed:?}"
    );
    assert_eq!(last_result(&bodies)["data"]["backgrounded"], true);
    let marker = subagent_markers(&zdx_home).pop().expect("subagent marker");
    wait_gone(
        marker["pid"].as_i64().unwrap() as i32,
        "the uncollected subagent",
    )
    .await;
}

/// Without background execution a normal call never hands off: it blocks
/// until the subagent finishes, as before, even past the bound.
#[tokio::test]
async fn normal_call_without_background_execution_blocks() {
    if !can_bind_localhost() {
        eprintln!("Skipping: cannot bind localhost TCP port in this environment.");
        return;
    }
    let (zdx_home, root) = workspace();
    set_bound(&zdx_home, 1);
    let starts = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let child = slow_child("CHILD-BLOCKING-ANSWER", Duration::from_secs(3), starts);
    let parent: Arc<ParentScript> = Arc::new(|step, _| match step {
        0 => invoke_normal(),
        _ => text_sse("parent done"),
    });
    let (server, bodies) = start_mock(parent, child).await;

    let mut cmd = exec(&zdx_home, &root, &server);
    cmd.arg("--no-background-commands");
    run_to_exit(cmd).await;

    let bodies_now = bodies.lock().unwrap().clone();
    let invoke = bodies_now[0]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|tool| {
            tool["name"]
                .as_str()
                .unwrap_or_default()
                .eq_ignore_ascii_case("invoke_subagent")
        })
        .unwrap()
        .clone();
    assert!(
        !invoke["description"]
            .as_str()
            .unwrap()
            .contains("moved to the background"),
        "{invoke}"
    );
    let answer = last_result(&bodies);
    assert_eq!(
        answer["data"]["result"], "CHILD-BLOCKING-ANSWER",
        "{answer}"
    );
    assert!(answer["data"]["child_thread_id"].is_string(), "{answer}");
    assert!(subagent_markers(&zdx_home).is_empty());
}

/// Writes a job and runs it with `zdx jobs run`.
async fn run_job(
    zdx_home: &TempDir,
    root: &TempDir,
    server: &MockServer,
    timeout_secs: u32,
) -> std::time::Duration {
    let jobs = zdx_home.path().join("jobs");
    std::fs::create_dir_all(&jobs).unwrap();
    std::fs::write(
        jobs.join("bounded.md"),
        format!(
            "---\nmodel: \"{MOCK_MODEL}\"\ntimeout_secs: {timeout_secs}\n---\nDelegate the task.\n"
        ),
    )
    .unwrap();
    let mut cmd = tokio::process::Command::new(env!("CARGO_BIN_EXE_zdx"));
    cmd.env("ZDX_HOME", zdx_home.path())
        .env("ANTHROPIC_API_KEY", "test-api-key")
        .env("ANTHROPIC_BASE_URL", server.uri())
        .current_dir(root.path())
        .args([
            "--root",
            root.path().to_str().unwrap(),
            "--no-thread",
            "jobs",
            "run",
            "bounded",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    run_to_exit(cmd).await
}

/// A run deadline later than the bound: the call hands off at the bound
/// (the handoff never stops the child), and the deadline still stops it.
#[cfg(unix)]
#[tokio::test]
async fn deadline_after_the_bound_still_stops_a_handed_off_subagent() {
    if !can_bind_localhost() {
        eprintln!("Skipping: cannot bind localhost TCP port in this environment.");
        return;
    }
    let (zdx_home, root) = workspace();
    set_bound(&zdx_home, 1);
    let starts = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let child = slow_child("NEVER", Duration::from_mins(1), starts);
    let parent: Arc<ParentScript> = Arc::new(|step, body| match step {
        0 => invoke_normal(),
        1 => tool_use_sse(
            "toolu_collect",
            "background_output",
            &json!({
                "bg_id": bg_id_in(body).expect("handle in the handoff result"),
                "wait_secs": 60,
                "wait_for": "exit",
            })
            .to_string(),
        ),
        _ => text_sse("parent done"),
    });
    let (server, bodies) = start_mock(parent, child).await;

    let elapsed = run_job(&zdx_home, &root, &server, 3).await;
    assert!(elapsed < Duration::from_secs(30), "took {elapsed:?}");

    let bodies_now = bodies.lock().unwrap().clone();
    assert_eq!(bodies_now.len(), 3, "parent requests: {bodies_now:?}");
    let handoff = tool_results(&bodies_now[1]).pop().unwrap();
    assert_eq!(handoff["data"]["backgrounded"], true, "{handoff}");
    assert_eq!(handoff["data"]["deadline_secs"], 3, "{handoff}");
    let collected = tool_results(&bodies_now[2]).pop().unwrap();
    assert_eq!(collected["data"]["turn"], "timed_out", "{collected}");
}

/// A run deadline earlier than the bound stops the subagent during the
/// normal wait, which then fails like a blocking call; nothing is handed off.
#[tokio::test]
async fn deadline_before_the_bound_fails_the_normal_call() {
    if !can_bind_localhost() {
        eprintln!("Skipping: cannot bind localhost TCP port in this environment.");
        return;
    }
    let (zdx_home, root) = workspace();
    set_bound(&zdx_home, 20);
    let starts = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let child = slow_child("NEVER", Duration::from_mins(1), starts);
    let parent: Arc<ParentScript> = Arc::new(|step, _| match step {
        0 => invoke_normal(),
        _ => text_sse("parent done"),
    });
    let (server, bodies) = start_mock(parent, child).await;

    let elapsed = run_job(&zdx_home, &root, &server, 2).await;
    assert!(elapsed < Duration::from_secs(15), "took {elapsed:?}");

    let failed = last_result(&bodies);
    assert_eq!(failed["ok"], false, "{failed}");
    assert_eq!(failed["error"]["code"], "execution_failed", "{failed}");
    assert!(
        failed.to_string().contains("timed out after 2 seconds"),
        "{failed}"
    );
    assert!(subagent_markers(&zdx_home).is_empty());
}

/// Thread files under `zdx_home` that hold nothing but their meta line.
fn meta_only_threads(zdx_home: &TempDir) -> Vec<std::path::PathBuf> {
    std::fs::read_dir(zdx_home.path().join("threads"))
        .map(|entries| {
            entries
                .flatten()
                .map(|entry| entry.path())
                .filter(|path| {
                    std::fs::read_to_string(path).is_ok_and(|content| {
                        content
                            .lines()
                            .filter(|line| !line.trim().is_empty())
                            .count()
                            == 1
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Interrupting a normal call before its handoff stops the child and the
/// command it runs, and leaves no logs, registry record, or empty child
/// thread behind.
#[cfg(unix)]
#[tokio::test]
async fn interrupt_before_handoff_stops_the_child_and_cleans_up() {
    if !can_bind_localhost() {
        eprintln!("Skipping: cannot bind localhost TCP port in this environment.");
        return;
    }
    let (zdx_home, root) = workspace();
    set_bound(&zdx_home, 30);
    let pidfile = root.path().join("child-command.pid");
    let child_reply = bash(
        "toolu_child_bash",
        &format!("echo $$ > {}; sleep 60", pidfile.display()),
    );
    let parent: Arc<ParentScript> = Arc::new(|step, _| match step {
        0 => invoke_normal(),
        _ => text_sse("parent done"),
    });
    let (server, _bodies) = start_mock(parent, child_reply).await;

    let mut child = exec(&zdx_home, &root, &server).spawn().expect("spawn exec");
    let owner_pid = child.id().expect("exec pid") as i32;
    let command_pid: i32 = wait_for_file(&pidfile).await.parse().unwrap();

    unsafe { libc::kill(owner_pid, libc::SIGINT) };
    let started = std::time::Instant::now();
    timeout(Duration::from_secs(20), child.wait())
        .await
        .expect("interrupted exec never exited")
        .expect("wait for exec");
    assert!(started.elapsed() < Duration::from_secs(15));

    wait_gone(command_pid, "the interrupted subagent's command").await;
    assert!(
        log_files(&zdx_home).is_empty(),
        "{:?}",
        log_files(&zdx_home)
    );
    assert!(subagent_markers(&zdx_home).is_empty());
    assert!(
        meta_only_threads(&zdx_home).is_empty(),
        "{:?}",
        meta_only_threads(&zdx_home)
    );
}

/// A child that already answered but whose process is still running at the
/// bound is handed off with its answer, and the message says not to wait
/// for it; `elapsed_secs` is the measured wait. Its leftover process is
/// stopped when the turn ends.
#[cfg(unix)]
#[tokio::test]
async fn handoff_after_the_answer_reports_it_and_says_not_to_wait() {
    if !can_bind_localhost() {
        eprintln!("Skipping: cannot bind localhost TCP port in this environment.");
        return;
    }
    let (zdx_home, root) = workspace();
    // Covers a child start, two model requests, and the child's 1s
    // foreground wait; 3s did not under heavy load.
    set_bound(&zdx_home, 6);
    let pidfile = root.path().join("drained-command.pid");
    // The opted-in child moves its own slow command to the background after
    // 1s, answers, and then stays alive draining that command.
    let slow = json!({
        "command": format!("echo $$ > {}; sleep 60", pidfile.display()),
        "timeout_secs": 1,
    })
    .to_string();
    let child: Arc<ChildScript> = Arc::new(move |step| match step {
        0 => tool_use_sse("toolu_child_slow", "bash", &slow),
        _ => text_sse("CHILD-EARLY-ANSWER"),
    });
    let parent: Arc<ParentScript> = Arc::new(|step, _| match step {
        0 => tool_use_sse(
            "toolu_invoke_normal",
            "invoke_subagent",
            &json!({
                "subagent": "drainer",
                "prompt": format!("{CHILD_MARKER}: do the task"),
            })
            .to_string(),
        ),
        _ => text_sse("parent done"),
    });
    let (server, bodies) = start_mock(parent, child).await;

    let elapsed = run_to_exit(exec(&zdx_home, &root, &server)).await;
    assert!(elapsed < Duration::from_secs(30), "took {elapsed:?}");

    let handoff = last_result(&bodies);
    let data = &handoff["data"];
    assert_eq!(data["backgrounded"], true, "{handoff}");
    assert_eq!(data["status"], "running", "{handoff}");
    assert_eq!(data["turn"], "completed", "{handoff}");
    assert_eq!(data["result"], "CHILD-EARLY-ANSWER", "{handoff}");
    assert!(data["elapsed_secs"].as_u64().unwrap() >= 6, "{handoff}");
    let message = data["message"].as_str().unwrap();
    assert!(message.contains("already ended its turn"), "{message}");
    assert!(
        message.contains("do NOT invoke it again or wait for it"),
        "{message}"
    );
    assert!(!message.contains("keeps working"), "{message}");

    let command_pid: i32 = wait_for_file(&pidfile).await.parse().unwrap();
    wait_gone(command_pid, "the handed-off child's drained command").await;
}

/// A default subagent (no background execution) runs a foreground command
/// past the handoff bound: the parent's normal call hands the subagent off,
/// the child's command keeps running in the foreground (its own run applies
/// no kill limit and moves nothing), and the parent can still see it and stop
/// it, which stops the command too.
#[cfg(unix)]
#[tokio::test]
async fn handed_off_subagent_keeps_its_foreground_command_and_can_be_killed() {
    if !can_bind_localhost() {
        eprintln!("Skipping: cannot bind localhost TCP port in this environment.");
        return;
    }
    let (zdx_home, root) = workspace();
    // The child reads the same config, so the same 1s bound applies to it.
    set_bound(&zdx_home, 1);
    let pidfile = root.path().join("child-command.pid");
    let child_reply = bash(
        "toolu_child_bash",
        &format!("echo $$ > {}; sleep 60", pidfile.display()),
    );
    // Runs in the parent (background on) with a raised wait budget, so it is
    // not itself handed off; it reports whether the child's command is still
    // alive three seconds after it started.
    let probe = json!({
        "command": format!(
            "while [ ! -s {0} ]; do sleep 0.05; done; sleep 3; \
             if kill -0 $(cat {0}) 2>/dev/null; then echo CHILD-CMD-ALIVE; fi",
            pidfile.display()
        ),
        "timeout_secs": 30,
    })
    .to_string();
    let parent: Arc<ParentScript> = Arc::new(move |step, body| {
        let bg_id = || bg_id_in(body).expect("handle in an earlier result");
        match step {
            0 => invoke_normal(),
            1 => tool_use_sse("toolu_probe", "bash", &probe),
            2 => tool_use_sse(
                "toolu_peek",
                "background_output",
                &json!({ "bg_id": bg_id() }).to_string(),
            ),
            3 => tool_use_sse(
                "toolu_kill",
                "background_kill",
                &json!({ "bg_id": bg_id() }).to_string(),
            ),
            _ => text_sse("parent done"),
        }
    });
    let (server, bodies) = start_mock(parent, child_reply).await;

    let elapsed = run_to_exit(exec(&zdx_home, &root, &server)).await;
    assert!(elapsed < Duration::from_secs(45), "took {elapsed:?}");

    let bodies_now = bodies.lock().unwrap().clone();
    assert_eq!(bodies_now.len(), 5, "parent requests: {bodies_now:?}");
    let handoff = tool_results(&bodies_now[1]).pop().unwrap();
    assert_eq!(handoff["data"]["backgrounded"], true, "{handoff}");

    let probe = tool_results(&bodies_now[2]).pop().unwrap();
    assert!(
        probe.to_string().contains("CHILD-CMD-ALIVE"),
        "the child's foreground command was stopped at the bound: {probe}"
    );

    let peek = tool_results(&bodies_now[3]).pop().unwrap();
    assert_eq!(peek["data"]["status"], "running", "{peek}");
    assert_eq!(peek["data"]["current_tool"], "bash", "{peek}");

    let killed = tool_results(&bodies_now[4]).pop().unwrap();
    assert_eq!(killed["data"]["status"], "killed", "{killed}");

    let command_pid: i32 = wait_for_file(&pidfile).await.parse().unwrap();
    wait_gone(command_pid, "the killed subagent's foreground command").await;
    // Only the subagent itself was ever registered; its command never was.
    assert_eq!(subagent_markers(&zdx_home).len(), 1);
}
