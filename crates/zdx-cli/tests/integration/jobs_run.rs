//! Tests for the Rust side of jobs, which the Go jobs program runs:
//! `zdx jobs attempt` (prompt layers, the runtime context, the optional
//! per-job `root`, the result file), `zdx jobs resolve` (caller and daemon
//! settings), and `zdx jobs history`.

use std::fs;
use std::sync::{Arc, Mutex};

use predicates::prelude::*;
use tempfile::TempDir;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request};

use crate::fixtures::{MOCK_MODEL, sse_response, text_sse};

fn can_bind_localhost() -> bool {
    std::net::TcpListener::bind("127.0.0.1:0").is_ok()
}

/// The attempt input the jobs program sends for job `name`.
fn attempt_input(name: &str, fields: serde_json::Value) -> serde_json::Value {
    let mut input = serde_json::json!({
        "job": name, "prompt": "Summarize the workspace.", "run_id": "run-1",
        "thread_id": null, "no_thread": true,
    });
    let serde_json::Value::Object(fields) = fields else {
        panic!("fields must be an object");
    };
    for (key, value) in fields {
        input[key] = value;
    }
    input
}

/// Runs `zdx --root ROOT jobs attempt` in `root` and returns the command's
/// assertion and the result file.
fn run_attempt(
    zdx_home: &std::path::Path,
    root: &std::path::Path,
    provider: &str,
    input: &serde_json::Value,
) -> (assert_cmd::assert::Assert, serde_json::Value) {
    let result = zdx_home.join("attempt-result.json");
    let assert = crate::fixtures::zdx_cmd()
        .env("ZDX_HOME", zdx_home)
        .env("ANTHROPIC_API_KEY", "test-api-key")
        .env("ANTHROPIC_BASE_URL", provider)
        .current_dir(root)
        .args([
            "--root",
            root.to_str().unwrap(),
            "jobs",
            "attempt",
            "--result",
        ])
        .arg(&result)
        .write_stdin(input.to_string())
        .assert();
    let report = serde_json::from_str(&fs::read_to_string(&result).expect("result file")).unwrap();
    (assert, report)
}

/// Runs `zdx --root ROOT jobs resolve ARGS` with `jobs` on stdin.
fn resolve(
    zdx_home: &std::path::Path,
    home: Option<&std::path::Path>,
    root: &std::path::Path,
    args: &[&str],
    jobs: serde_json::Value,
) -> serde_json::Value {
    let mut cmd = crate::fixtures::zdx_cmd();
    cmd.env("ZDX_HOME", zdx_home).current_dir(root);
    if let Some(home) = home {
        cmd.env("HOME", home);
    }
    let output = cmd
        .args(["--root", root.to_str().unwrap(), "jobs", "resolve"])
        .args(args)
        .write_stdin(
            serde_json::Value::Object([("jobs".to_string(), jobs)].into_iter().collect())
                .to_string(),
        )
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    serde_json::from_slice(&output).unwrap()
}

/// One earlier run of `job` (two attempts) that posted one message.
fn seed_previous_run(zdx_home: &std::path::Path, job: &str) {
    let attempt = |n: u32, outcome: &str, day: u32| {
        serde_json::json!({
            "job": job, "run_id": "earlier-run", "trigger": "daemon",
            "thread_id": format!("job-{job}-earlier"), "attempt": n, "max_attempts": 2,
            "started_at": format!("2026-01-0{day}T08:00:00+00:00"),
            "finished_at": format!("2026-01-0{day}T08:01:00+00:00"),
            "duration_ms": 1, "ok": outcome == "succeeded", "outcome": outcome,
        })
        .to_string()
    };
    fs::write(
        zdx_home.join("jobs_runs.jsonl"),
        format!(
            "{}\n{}\n",
            attempt(1, "retrying", 1),
            attempt(2, "succeeded", 2)
        ),
    )
    .unwrap();
    let links = zdx_home.join("state").join("posted");
    fs::create_dir_all(&links).unwrap();
    fs::write(
        links.join(format!("job-{job}-earlier.jsonl")),
        serde_json::json!({
            "ts": "2026-01-02T08:00:30.000Z", "target_thread_id": "telegram--100-topic-7",
            "chat_id": -100, "topic_id": 7, "message_id": 70,
            "url": "https://t.me/c/100/7/70",
        })
        .to_string()
            + "\n",
    )
    .unwrap();
}

/// A mock provider that records the request it answers.
async fn capturing_provider() -> (MockServer, Arc<Mutex<Option<serde_json::Value>>>) {
    let mock_server = MockServer::start().await;
    let captured: Arc<Mutex<Option<serde_json::Value>>> = Arc::new(Mutex::new(None));
    let captured_clone = Arc::clone(&captured);
    let body = text_sse("done");
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(move |req: &Request| {
            *captured_clone.lock().unwrap() = req.body_json().ok();
            sse_response(&body)
        })
        .expect(1)
        .mount(&mock_server)
        .await;
    (mock_server, captured)
}

#[tokio::test]
async fn test_job_attempt_uses_declared_root_layers_and_runtime_context() {
    if !can_bind_localhost() {
        eprintln!("Skipping: cannot bind localhost TCP port in this environment.");
        return;
    }
    let zdx_home = TempDir::new().unwrap();
    let caller_root = TempDir::new().unwrap();
    fs::write(caller_root.path().join("AGENTS.md"), "CALLER-ROOT-NOTE").unwrap();
    let declared_root = TempDir::new().unwrap();
    fs::write(declared_root.path().join("AGENTS.md"), "DECLARED-ROOT-NOTE").unwrap();
    seed_previous_run(zdx_home.path(), "scoped");
    let (mock_server, captured) = capturing_provider().await;

    let input = attempt_input(
        "scoped",
        serde_json::json!({
            "model": format!("{MOCK_MODEL}@low"),
            "root": declared_root.path().display().to_string(),
        }),
    );
    let (assert, result) = run_attempt(
        zdx_home.path(),
        caller_root.path(),
        &mock_server.uri(),
        &input,
    );
    assert.success();

    let request = captured.lock().unwrap().clone().expect("captured request");
    let system = request["system"].to_string();
    assert!(system.contains("DECLARED-ROOT-NOTE"), "{system}");
    assert!(!system.contains("CALLER-ROOT-NOTE"), "{system}");
    assert!(system.contains("This run is non-interactive"), "{system}");
    assert!(system.contains("This run is a ZDX job"), "{system}");

    let first_user = request["messages"][0].to_string();
    assert!(
        first_user.contains("<runtime_context>"),
        "job run must carry the runtime-context snapshot: {first_user}"
    );
    for expected in [
        "# Previous Runs of This Job",
        "succeeded after 2 attempts · thread job-scoped-earlier",
        "posted a message (message 70, https://t.me/c/100/7/70) · conversation telegram--100-topic-7 · no user message there since",
        "never authorizes a new side effect",
    ] {
        assert!(
            first_user.contains(expected),
            "missing {expected:?}: {first_user}"
        );
    }

    assert_eq!(result["outcome"], "succeeded");
    assert_eq!(result["prepared"], true);
    assert!(result["error"].is_null());
    assert_eq!(result["model"], format!("{MOCK_MODEL}@low"));
    assert_eq!(
        result["root"],
        declared_root.path().to_string_lossy().as_ref()
    );
    let log = fs::read_to_string(zdx_home.path().join("jobs_runs.jsonl")).unwrap();
    assert_eq!(
        log.lines().count(),
        2,
        "an attempt never writes the run log"
    );
}

#[test]
fn test_job_attempt_with_missing_root_fails_unprepared_without_running() {
    let zdx_home = TempDir::new().unwrap();
    let caller_root = TempDir::new().unwrap();
    let missing = caller_root.path().join("missing-root");
    let input = attempt_input(
        "scoped",
        serde_json::json!({ "model": MOCK_MODEL, "root": missing.display().to_string() }),
    );

    let (assert, result) = run_attempt(
        zdx_home.path(),
        caller_root.path(),
        "http://127.0.0.1:9",
        &input,
    );
    assert.failure().stderr(predicate::str::contains(
        "does not exist or is not a directory",
    ));
    assert_eq!(result["outcome"], "failed");
    assert_eq!(result["prepared"], false);
    assert!(result["root"].is_null());
    assert!(
        result["error"]
            .as_str()
            .unwrap()
            .starts_with("prepare job run: ")
    );
}

#[test]
fn test_jobs_resolve_reports_caller_settings_and_errors() {
    let zdx_home = TempDir::new().unwrap();
    let caller_root = TempDir::new().unwrap();
    let view = resolve(
        zdx_home.path(),
        None,
        caller_root.path(),
        &[],
        serde_json::json!([
            { "job": "explicit", "model": format!("{MOCK_MODEL}@low") },
            { "job": "broken", "root": "relative/path" },
        ]),
    );
    assert!(view["daemon"].is_null());
    let settings = &view["results"][0]["settings"];
    assert_eq!(settings["model"]["spec"], format!("{MOCK_MODEL}@low"));
    assert_eq!(settings["model"]["origin"], "job");
    assert_eq!(settings["model"]["thinking"], "low");
    assert_eq!(settings["root"]["origin"], "caller");
    assert_eq!(
        settings["root"]["path"],
        caller_root.path().to_string_lossy().as_ref()
    );
    let broken = &view["results"][1];
    assert!(broken["settings"].is_null());
    assert!(
        broken["error"]
            .as_str()
            .unwrap()
            .contains("must be an absolute path")
    );

    let prepared = resolve(
        zdx_home.path(),
        None,
        caller_root.path(),
        &["--prepare"],
        serde_json::json!([{ "job": "delegated", "subagent": "missing-subagent" }]),
    );
    assert!(
        prepared["results"][0]["error"]
            .as_str()
            .unwrap()
            .contains("missing-subagent")
    );
}

/// A subagent body replaces the default system prompt. The job must
/// still carry project instructions (once), the Safety/Git rules, both
/// non-interactive layers, and the memory index, while keeping the profile's tools.
#[tokio::test]
async fn test_subagent_job_keeps_safeguards_and_context() {
    if !can_bind_localhost() {
        eprintln!("Skipping: cannot bind localhost TCP port in this environment.");
        return;
    }
    let zdx_home = TempDir::new().unwrap();
    let notes = zdx_home.path().join("memory").join("Notes");
    fs::create_dir_all(&notes).unwrap();
    fs::write(notes.join("MEMORY.md"), "MEMORY-INDEX-SENTINEL").unwrap();

    let root = TempDir::new().unwrap();
    fs::write(root.path().join("AGENTS.md"), "PROJECT-RULE-SENTINEL").unwrap();
    let subagents = root.path().join(".zdx").join("subagents");
    fs::create_dir_all(&subagents).unwrap();
    fs::write(
        subagents.join("helper.md"),
        "---\ndescription: Helper\ntools:\n  - read\n---\nHELPER-BODY. {% if project_context %}{{ project_context }}{% endif %}\n",
    )
    .unwrap();
    let (mock_server, captured) = capturing_provider().await;

    let input = attempt_input(
        "delegated",
        serde_json::json!({ "model": MOCK_MODEL, "subagent": "helper" }),
    );
    run_attempt(zdx_home.path(), root.path(), &mock_server.uri(), &input)
        .0
        .success();

    let request = captured.lock().unwrap().clone().expect("captured request");
    let system = request["system"].to_string();
    assert!(system.contains("HELPER-BODY."), "{system}");
    assert_eq!(
        system.matches("PROJECT-RULE-SENTINEL").count(),
        1,
        "{system}"
    );
    assert_eq!(system.matches("## Safety").count(), 1, "{system}");
    assert_eq!(system.matches("## Git").count(), 1, "{system}");
    assert_eq!(
        system.matches("This run is non-interactive").count(),
        1,
        "{system}"
    );
    assert_eq!(
        system.matches("This run is a ZDX job").count(),
        1,
        "{system}"
    );

    let first_user = request["messages"][0].to_string();
    assert!(first_user.contains("<runtime_context>"), "{first_user}");
    assert!(first_user.contains("MEMORY-INDEX-SENTINEL"), "{first_user}");
    assert!(
        !first_user.contains("<available_skills>"),
        "subagent jobs keep the profile's skill allowlist: {first_user}"
    );
    assert!(
        !first_user.contains("invoke_subagent"),
        "no delegation catalog for a profile without invoke_subagent: {first_user}"
    );

    let tools = request["tools"].as_array().expect("tools array");
    let names: Vec<String> = tools
        .iter()
        .map(|tool| tool["name"].as_str().unwrap_or_default().to_lowercase())
        .collect();
    assert_eq!(names, vec!["read".to_string()]);
}

#[test]
fn test_jobs_reject_global_model_flag() {
    crate::fixtures::zdx_cmd()
        .args(["--model", MOCK_MODEL, "jobs", "run", "any"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "--model is not supported for jobs",
        ));
}

fn write_daemon_config(root: &std::path::Path, model: &str) {
    fs::create_dir_all(root.join(".zdx")).unwrap();
    fs::write(
        root.join(".zdx").join("config.toml"),
        format!("model = \"{model}\"\n"),
    )
    .unwrap();
}

fn daemon_resolve(
    zdx_home: &std::path::Path,
    home: &std::path::Path,
    caller_root: &std::path::Path,
    jobs: serde_json::Value,
) -> serde_json::Value {
    resolve(zdx_home, Some(home), caller_root, &["--daemon"], jobs)
}

/// The caller's own root and config never stand in for the daemon's.
#[test]
fn test_resolve_daemon_uses_running_daemon_context_not_caller_root() {
    let zdx_home = TempDir::new().unwrap();
    let home = TempDir::new().unwrap();
    let daemon_root = TempDir::new().unwrap();
    write_daemon_config(daemon_root.path(), "claude-cli@work:claude-opus-5@high");
    let caller_root = TempDir::new().unwrap();
    write_daemon_config(caller_root.path(), "openai-codex:gpt-5.4@low");
    let scoped_root = TempDir::new().unwrap();

    let run = zdx_home.path().join("run");
    fs::create_dir_all(&run).unwrap();
    let pid = std::process::id();
    fs::write(run.join("daemon.pid"), pid.to_string()).unwrap();
    fs::write(
        run.join("daemon.context.json"),
        serde_json::json!({
            "pid": pid,
            "root": daemon_root.path(),
            "config_dir": daemon_root.path(),
            "started_at": "2026-10-12T08:00:00Z",
        })
        .to_string(),
    )
    .unwrap();

    let view = daemon_resolve(
        zdx_home.path(),
        home.path(),
        caller_root.path(),
        serde_json::json!([
            { "job": "inherited" },
            {
                "job": "scoped",
                "root": scoped_root.path().display().to_string(),
                "model": format!("{MOCK_MODEL}@medium"),
            },
        ]),
    );
    assert_eq!(view["daemon"]["status"], "available");
    assert_eq!(view["daemon"]["context"]["source"], "running");
    assert_eq!(view["daemon"]["context"]["pid"], pid);

    let inherited = &view["results"][0]["settings"];
    assert_eq!(
        inherited["root"]["path"],
        daemon_root.path().to_string_lossy().as_ref()
    );
    assert_eq!(inherited["root"]["origin"], "daemon");
    assert_eq!(
        inherited["model"]["spec"],
        "claude-cli@work:claude-opus-5@high"
    );
    assert_eq!(inherited["model"]["account"], "work");
    assert_eq!(inherited["model"]["thinking"], "high");
    assert_eq!(inherited["model"]["origin"], "config");

    let scoped = &view["results"][1]["settings"];
    assert_eq!(
        scoped["root"]["path"],
        scoped_root.path().to_string_lossy().as_ref()
    );
    assert_eq!(scoped["root"]["origin"], "job");
    assert_eq!(scoped["model"]["spec"], format!("{MOCK_MODEL}@medium"));
    assert_eq!(scoped["model"]["origin"], "job");
}

#[test]
fn test_resolve_daemon_reports_installed_service_when_stopped() {
    let zdx_home = TempDir::new().unwrap();
    let home = TempDir::new().unwrap();
    let daemon_root = TempDir::new().unwrap();
    write_daemon_config(daemon_root.path(), "claude-cli:claude-sonnet-5@low");
    let caller_root = TempDir::new().unwrap();

    let agents = home.path().join("Library").join("LaunchAgents");
    fs::create_dir_all(&agents).unwrap();
    let root = daemon_root.path().display();
    fs::write(
        agents.join("dev.zdx.daemon.plist"),
        format!(
            "<plist><dict>\n<key>ProgramArguments</key><array>\n<string>/bin/zsh</string>\n\
             <string>-c</string>\n<string>exec '/bin/zdx' '--root' '{root}' 'jobs' \
             'daemon'</string>\n</array>\n<key>WorkingDirectory</key>\n<string>{root}</string>\n\
             <key>EnvironmentVariables</key><dict>\n<key>ZDX_HOME</key>\n<string>{}</string>\n\
             </dict></dict></plist>\n",
            zdx_home.path().display()
        ),
    )
    .unwrap();

    let view = daemon_resolve(
        zdx_home.path(),
        home.path(),
        caller_root.path(),
        serde_json::json!([{ "job": "inherited" }]),
    );
    let context = &view["daemon"]["context"];
    assert_eq!(context["source"], "installed");
    assert!(context["pid"].is_null());
    assert!(context["note"].as_str().unwrap().contains("not running"));
    let inherited = &view["results"][0]["settings"];
    assert_eq!(inherited["root"]["path"], root.to_string());
    assert_eq!(inherited["model"]["spec"], "claude-cli:claude-sonnet-5@low");
}

#[test]
fn test_resolve_daemon_unavailable_never_falls_back_to_caller_root() {
    let zdx_home = TempDir::new().unwrap();
    let home = TempDir::new().unwrap();
    let caller_root = TempDir::new().unwrap();
    let scoped_root = TempDir::new().unwrap();

    let view = daemon_resolve(
        zdx_home.path(),
        home.path(),
        caller_root.path(),
        serde_json::json!([
            { "job": "inherited" },
            { "job": "scoped", "root": scoped_root.path().display().to_string() },
        ]),
    );
    assert_eq!(view["daemon"]["status"], "unavailable");
    assert!(
        view["daemon"]["reason"]
            .as_str()
            .unwrap()
            .contains("not running")
    );
    let inherited = &view["results"][0];
    assert!(inherited["settings"].is_null());
    assert!(
        inherited["error"]
            .as_str()
            .unwrap()
            .contains("daemon settings unavailable")
    );
    assert_eq!(
        view["results"][1]["settings"]["root"]["path"],
        scoped_root.path().to_string_lossy().as_ref()
    );
}

/// Runs one job whose subagent declares `frontmatter_extra`, and returns the
/// tools the job's model was offered.
async fn subagent_job_tools(frontmatter_extra: &str) -> Vec<serde_json::Value> {
    let zdx_home = TempDir::new().unwrap();
    let root = TempDir::new().unwrap();
    let subagents = root.path().join(".zdx").join("subagents");
    fs::create_dir_all(&subagents).unwrap();
    fs::write(
        subagents.join("shell-helper.md"),
        format!("---\ndescription: Shell helper\n{frontmatter_extra}tools:\n  - bash\n---\nYou run commands.\n"),
    )
    .unwrap();
    let (mock_server, captured) = capturing_provider().await;
    let input = attempt_input(
        "shell",
        serde_json::json!({ "model": MOCK_MODEL, "subagent": "shell-helper" }),
    );
    run_attempt(zdx_home.path(), root.path(), &mock_server.uri(), &input)
        .0
        .success();

    let request = captured.lock().unwrap().clone().expect("captured request");
    request["tools"].as_array().cloned().expect("tools array")
}

fn job_tool_names(tools: &[serde_json::Value]) -> Vec<String> {
    let mut names: Vec<String> = tools
        .iter()
        .map(|tool| tool["name"].as_str().unwrap_or_default().to_lowercase())
        .collect();
    names.sort();
    names
}

fn job_bash_has_background(tools: &[serde_json::Value]) -> bool {
    tools
        .iter()
        .find(|tool| {
            tool["name"]
                .as_str()
                .unwrap_or_default()
                .eq_ignore_ascii_case("bash")
        })
        .expect("bash offered")["input_schema"]["properties"]
        .get("background")
        .is_some()
}

/// A job with a subagent follows the subagent's background policy, like any
/// other run of that subagent: off unless the definition opts in.
#[tokio::test]
async fn test_subagent_job_follows_the_subagent_background_policy() {
    if !can_bind_localhost() {
        eprintln!("Skipping: cannot bind localhost TCP port in this environment.");
        return;
    }

    let default = subagent_job_tools("").await;
    assert_eq!(job_tool_names(&default), vec!["bash"]);
    assert!(!job_bash_has_background(&default));

    let opted_in = subagent_job_tools("background_commands: true\n").await;
    assert_eq!(
        job_tool_names(&opted_in),
        vec!["background_kill", "background_output", "bash"]
    );
    assert!(job_bash_has_background(&opted_in));
}

#[test]
fn test_jobs_history_groups_attempts_and_lists_posts() {
    let zdx_home = TempDir::new().unwrap();
    seed_previous_run(zdx_home.path(), "alerts");

    let output = crate::fixtures::zdx_cmd()
        .env("ZDX_HOME", zdx_home.path())
        .args(["jobs", "history", "alerts", "--json"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let runs: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let runs = runs.as_array().expect("array");
    assert_eq!(runs.len(), 1, "two attempts are one run");
    assert_eq!(runs[0]["outcome"], "succeeded");
    assert_eq!(runs[0]["attempts"], 2);
    assert_eq!(runs[0]["thread_id"], "job-alerts-earlier");
    assert_eq!(
        runs[0]["posts"][0]["target_thread_id"],
        "telegram--100-topic-7"
    );
    assert_eq!(runs[0]["posts"][0]["user_messages_after"], 0);

    crate::fixtures::zdx_cmd()
        .env("ZDX_HOME", zdx_home.path())
        .args(["jobs", "history", "missing"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "No earlier recorded run of `missing`",
        ));
}
