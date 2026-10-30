//! Tests for `zdx jobs run`: non-interactive prompt layers, the runtime
//! context, and the optional per-job `root`.

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

fn write_job(zdx_home: &std::path::Path, name: &str, frontmatter: &str) {
    let dir = zdx_home.join("jobs");
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join(format!("{name}.md")),
        format!("---\n{frontmatter}---\nSummarize the workspace.\n"),
    )
    .unwrap();
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

fn read_run_records(zdx_home: &std::path::Path) -> Vec<serde_json::Value> {
    fs::read_to_string(zdx_home.join("jobs_runs.jsonl"))
        .unwrap_or_default()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

#[tokio::test]
async fn test_job_run_uses_declared_root_layers_and_runtime_context() {
    if !can_bind_localhost() {
        eprintln!("Skipping: cannot bind localhost TCP port in this environment.");
        return;
    }
    let zdx_home = TempDir::new().unwrap();
    let caller_root = TempDir::new().unwrap();
    fs::write(caller_root.path().join("AGENTS.md"), "CALLER-ROOT-NOTE").unwrap();
    let declared_root = TempDir::new().unwrap();
    fs::write(declared_root.path().join("AGENTS.md"), "DECLARED-ROOT-NOTE").unwrap();
    write_job(
        zdx_home.path(),
        "scoped",
        &format!(
            "model: \"{MOCK_MODEL}@low\"\nroot: \"{}\"\n",
            declared_root.path().display()
        ),
    );
    seed_previous_run(zdx_home.path(), "scoped");

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

    crate::fixtures::zdx_cmd()
        .env("ZDX_HOME", zdx_home.path())
        .env("ANTHROPIC_API_KEY", "test-api-key")
        .env("ANTHROPIC_BASE_URL", mock_server.uri())
        .current_dir(caller_root.path())
        .args([
            "--root",
            caller_root.path().to_str().unwrap(),
            "--no-thread",
            "jobs",
            "run",
            "scoped",
        ])
        .assert()
        .success();

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

    let records = read_run_records(zdx_home.path());
    assert_eq!(records.len(), 3);
    let records = &records[2..];
    assert_eq!(records[0]["ok"], true);
    assert_eq!(records[0]["model"], format!("{MOCK_MODEL}@low"));
    assert_eq!(
        records[0]["root"],
        declared_root.path().to_string_lossy().as_ref()
    );
}

#[tokio::test]
async fn test_job_run_with_missing_root_fails_without_running() {
    let zdx_home = TempDir::new().unwrap();
    let caller_root = TempDir::new().unwrap();
    let missing = caller_root.path().join("missing-root");
    write_job(
        zdx_home.path(),
        "scoped",
        &format!("model: \"{MOCK_MODEL}\"\nroot: \"{}\"\n", missing.display()),
    );

    crate::fixtures::zdx_cmd()
        .env("ZDX_HOME", zdx_home.path())
        .env("ANTHROPIC_API_KEY", "test-api-key")
        .env("ANTHROPIC_BASE_URL", "http://127.0.0.1:9")
        .current_dir(caller_root.path())
        .args([
            "--root",
            caller_root.path().to_str().unwrap(),
            "--no-thread",
            "jobs",
            "run",
            "scoped",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "does not exist or is not a directory",
        ));

    let records = read_run_records(zdx_home.path());
    assert_eq!(records.len(), 1, "a failed start is still recorded");
    assert_eq!(records[0]["ok"], false);
    assert!(records[0]["root"].is_null());
}

#[test]
fn test_jobs_list_json_reports_resolved_settings_and_errors() {
    let zdx_home = TempDir::new().unwrap();
    let caller_root = TempDir::new().unwrap();
    write_job(
        zdx_home.path(),
        "explicit",
        &format!("schedule: \"0 8 * * *\"\nmodel: \"{MOCK_MODEL}@low\"\n"),
    );
    write_job(zdx_home.path(), "broken", "root: \"relative/path\"\n");

    let output = crate::fixtures::zdx_cmd()
        .env("ZDX_HOME", zdx_home.path())
        .current_dir(caller_root.path())
        .args([
            "--root",
            caller_root.path().to_str().unwrap(),
            "jobs",
            "list",
            "--json",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let list: serde_json::Value = serde_json::from_slice(&output).unwrap();
    let items = list.as_array().unwrap();

    let broken = items.iter().find(|item| item["name"] == "broken").unwrap();
    assert!(broken["settings"].is_null());
    assert!(
        broken["error"]
            .as_str()
            .unwrap()
            .contains("must be an absolute path")
    );

    let explicit = items
        .iter()
        .find(|item| item["name"] == "explicit")
        .unwrap();
    assert_eq!(explicit["declared_model"], format!("{MOCK_MODEL}@low"));
    let settings = &explicit["settings"];
    assert_eq!(settings["model"]["spec"], format!("{MOCK_MODEL}@low"));
    assert_eq!(settings["model"]["origin"], "job");
    assert_eq!(settings["model"]["thinking"], "low");
    assert_eq!(settings["root"]["origin"], "caller");
    assert_eq!(
        settings["root"]["path"],
        caller_root.path().to_string_lossy().as_ref()
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
    write_job(
        zdx_home.path(),
        "delegated",
        &format!("model: \"{MOCK_MODEL}\"\nsubagent: helper\n"),
    );

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

    crate::fixtures::zdx_cmd()
        .env("ZDX_HOME", zdx_home.path())
        .env("ANTHROPIC_API_KEY", "test-api-key")
        .env("ANTHROPIC_BASE_URL", mock_server.uri())
        .current_dir(root.path())
        .args([
            "--root",
            root.path().to_str().unwrap(),
            "--no-thread",
            "jobs",
            "run",
            "delegated",
        ])
        .assert()
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
    let zdx_home = TempDir::new().unwrap();
    write_job(zdx_home.path(), "any", "");

    crate::fixtures::zdx_cmd()
        .env("ZDX_HOME", zdx_home.path())
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

fn daemon_list(
    zdx_home: &std::path::Path,
    home: &std::path::Path,
    caller_root: &std::path::Path,
) -> serde_json::Value {
    let output = crate::fixtures::zdx_cmd()
        .env("ZDX_HOME", zdx_home)
        .env("HOME", home)
        .current_dir(caller_root)
        .args([
            "--root",
            caller_root.to_str().unwrap(),
            "jobs",
            "list",
            "--daemon",
            "--json",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    serde_json::from_slice(&output).unwrap()
}

fn find<'a>(view: &'a serde_json::Value, name: &str) -> &'a serde_json::Value {
    view["jobs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["name"] == name)
        .unwrap()
}

/// The caller's own root and config never stand in for the daemon's.
#[test]
fn test_list_daemon_uses_running_daemon_context_not_caller_root() {
    let zdx_home = TempDir::new().unwrap();
    let home = TempDir::new().unwrap();
    let daemon_root = TempDir::new().unwrap();
    write_daemon_config(daemon_root.path(), "claude-cli@work:claude-opus-5@high");
    let caller_root = TempDir::new().unwrap();
    write_daemon_config(caller_root.path(), "openai-codex:gpt-5.4@low");
    let scoped_root = TempDir::new().unwrap();
    write_job(zdx_home.path(), "inherited", "");
    write_job(
        zdx_home.path(),
        "scoped",
        &format!(
            "root: \"{}\"\nmodel: \"{MOCK_MODEL}@medium\"\n",
            scoped_root.path().display()
        ),
    );

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

    let view = daemon_list(zdx_home.path(), home.path(), caller_root.path());
    assert_eq!(view["daemon"]["status"], "available");
    assert_eq!(view["daemon"]["context"]["source"], "running");
    assert_eq!(view["daemon"]["context"]["pid"], pid);

    let inherited = &find(&view, "inherited")["settings"];
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

    let scoped = &find(&view, "scoped")["settings"];
    assert_eq!(
        scoped["root"]["path"],
        scoped_root.path().to_string_lossy().as_ref()
    );
    assert_eq!(scoped["root"]["origin"], "job");
    assert_eq!(scoped["model"]["spec"], format!("{MOCK_MODEL}@medium"));
    assert_eq!(scoped["model"]["origin"], "job");
}

#[test]
fn test_list_daemon_reports_installed_service_when_stopped() {
    let zdx_home = TempDir::new().unwrap();
    let home = TempDir::new().unwrap();
    let daemon_root = TempDir::new().unwrap();
    write_daemon_config(daemon_root.path(), "claude-cli:claude-sonnet-5@low");
    let caller_root = TempDir::new().unwrap();
    write_job(zdx_home.path(), "inherited", "");

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

    let view = daemon_list(zdx_home.path(), home.path(), caller_root.path());
    let context = &view["daemon"]["context"];
    assert_eq!(context["source"], "installed");
    assert!(context["pid"].is_null());
    assert!(context["note"].as_str().unwrap().contains("not running"));
    let inherited = &find(&view, "inherited")["settings"];
    assert_eq!(inherited["root"]["path"], root.to_string());
    assert_eq!(inherited["model"]["spec"], "claude-cli:claude-sonnet-5@low");
}

#[test]
fn test_list_daemon_unavailable_never_falls_back_to_caller_root() {
    let zdx_home = TempDir::new().unwrap();
    let home = TempDir::new().unwrap();
    let caller_root = TempDir::new().unwrap();
    let scoped_root = TempDir::new().unwrap();
    write_job(zdx_home.path(), "inherited", "");
    write_job(
        zdx_home.path(),
        "scoped",
        &format!("root: \"{}\"\n", scoped_root.path().display()),
    );

    let view = daemon_list(zdx_home.path(), home.path(), caller_root.path());
    assert_eq!(view["daemon"]["status"], "unavailable");
    assert!(
        view["daemon"]["reason"]
            .as_str()
            .unwrap()
            .contains("not running")
    );
    let inherited = find(&view, "inherited");
    assert!(inherited["settings"].is_null());
    assert!(
        inherited["error"]
            .as_str()
            .unwrap()
            .contains("daemon settings unavailable")
    );
    assert_eq!(
        find(&view, "scoped")["settings"]["root"]["path"],
        scoped_root.path().to_string_lossy().as_ref()
    );
}

#[test]
fn test_set_model_command_edits_only_the_model_line() {
    let zdx_home = TempDir::new().unwrap();
    let dir = zdx_home.path().join("jobs");
    fs::create_dir_all(&dir).unwrap();
    let path = dir.join("paused.md");
    let original = "---\n# paused: no schedule\ntimeout_secs: 60\n---\nDo the thing.\n";
    fs::write(&path, original).unwrap();

    let run = |args: &[&str]| {
        crate::fixtures::zdx_cmd()
            .env("ZDX_HOME", zdx_home.path())
            .args(["jobs", "set-model"])
            .args(args)
            .assert()
    };

    run(&["paused", "claude-cli@work:claude-opus-5@high"])
        .success()
        .stdout(predicate::str::contains(
            "paused: model = claude-cli@work:claude-opus-5@high",
        ));
    assert_eq!(
        fs::read_to_string(&path).unwrap(),
        original.replace(
            "timeout_secs: 60\n---",
            "timeout_secs: 60\nmodel: \"claude-cli@work:claude-opus-5@high\"\n---"
        )
    );

    run(&["paused", "--clear"])
        .success()
        .stdout(predicate::str::contains("model cleared"));
    assert_eq!(fs::read_to_string(&path).unwrap(), original);

    run(&["paused"]).failure();
    run(&["paused", "bad \"spec"]).failure();
    assert_eq!(fs::read_to_string(&path).unwrap(), original);
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
    write_job(
        zdx_home.path(),
        "shell",
        &format!("model: \"{MOCK_MODEL}\"\nsubagent: shell-helper\n"),
    );

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

    crate::fixtures::zdx_cmd()
        .env("ZDX_HOME", zdx_home.path())
        .env("ANTHROPIC_API_KEY", "test-api-key")
        .env("ANTHROPIC_BASE_URL", mock_server.uri())
        .current_dir(root.path())
        .args([
            "--root",
            root.path().to_str().unwrap(),
            "--no-thread",
            "jobs",
            "run",
            "shell",
        ])
        .assert()
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
