//! `zdx --thread <id> exec` runs the model selection persisted on the thread,
//! like every other surface: a `mode:<name>` reference resolves against the
//! root's config on each run, a concrete pin stays fixed, and `-m` still wins
//! for one run.

use std::fmt::Write as _;
use std::path::Path;
use std::sync::{Arc, Mutex};

use serde_json::Value;
use tempfile::TempDir;
use wiremock::matchers::method;
use wiremock::{Mock, MockServer, Request};

use crate::fixtures::sse_response;

const CHAT_COMPLETIONS_SSE: &str = concat!(
    "data: {\"id\":\"1\",\"object\":\"chat.completion.chunk\",\"choices\":",
    "[{\"index\":0,\"delta\":{\"content\":\"pong\"},\"finish_reason\":null}]}\n\n",
    "data: {\"id\":\"1\",\"object\":\"chat.completion.chunk\",\"choices\":",
    "[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
    "data: [DONE]\n\n"
);

fn can_bind_localhost() -> bool {
    std::net::TcpListener::bind("127.0.0.1:0").is_ok()
}

fn write_config(zdx_home: &Path, base_url: &str, smart: &str) {
    std::fs::write(
        zdx_home.join("config.toml"),
        format!(
            "model = \"proxy:default-model\"\n\
             [[model_modes]]\nname = \"smart\"\nprimary = \"{smart}\"\n\
             [providers.custom.proxy]\nbase_url = \"{base_url}/v1\"\napi_key = \"test-api-key\"\n\
             models = [\"default-model\", \"first-model\", \"second-model\"]\n"
        ),
    )
    .unwrap();
}

fn write_thread(zdx_home: &Path, id: &str, model_override: &str) {
    let threads = zdx_home.join("threads");
    std::fs::create_dir_all(&threads).unwrap();
    let meta = serde_json::json!({
        "type": "meta",
        "schema_version": 1,
        "model_override": model_override,
        "ts": "2026-01-01T00:00:00Z",
    });
    std::fs::write(threads.join(format!("{id}.jsonl")), format!("{meta}\n")).unwrap();
}

/// Runs one turn on `thread`, with the `smart` mode set to `smart`, and
/// returns the model the provider was asked for.
async fn requested_model(
    zdx_home: &Path,
    work_dir: &Path,
    thread: &str,
    smart: &str,
    extra: &[&str],
) -> String {
    let mock_server = MockServer::start().await;
    let seen: Arc<Mutex<Vec<Value>>> = Arc::new(Mutex::new(Vec::new()));
    let seen_clone = Arc::clone(&seen);
    Mock::given(method("POST"))
        .respond_with(move |req: &Request| {
            seen_clone
                .lock()
                .unwrap()
                .push(serde_json::from_slice(&req.body).unwrap_or(Value::Null));
            sse_response(CHAT_COMPLETIONS_SSE)
        })
        .expect(1)
        .mount(&mock_server)
        .await;

    write_config(zdx_home, &mock_server.uri(), smart);
    crate::fixtures::zdx_cmd()
        .env("ZDX_HOME", zdx_home)
        .args([
            "--root",
            work_dir.to_str().unwrap(),
            "--thread",
            thread,
            "exec",
        ])
        .args(extra)
        .args(["-p", "ping"])
        .assert()
        .success();
    drop(mock_server);

    let seen = seen.lock().unwrap();
    assert_eq!(seen.len(), 1, "expected exactly one upstream request");
    seen[0]["model"].as_str().unwrap().to_string()
}

#[tokio::test]
async fn resumed_thread_follows_its_mode_and_keeps_concrete_pins() {
    if !can_bind_localhost() {
        eprintln!("Skipping: cannot bind localhost TCP port in this environment.");
        return;
    }
    let zdx_home = TempDir::new().unwrap();
    let work_dir = TempDir::new().unwrap();
    let (home, root) = (zdx_home.path(), work_dir.path());

    write_thread(home, "mode-thread", "mode:smart");
    assert_eq!(
        requested_model(home, root, "mode-thread", "proxy:first-model", &[]).await,
        "first-model"
    );
    assert_eq!(
        requested_model(home, root, "mode-thread", "proxy:second-model", &[]).await,
        "second-model",
        "an edited mode moves the thread on its next run"
    );

    write_thread(home, "pinned-thread", "proxy:first-model");
    assert_eq!(
        requested_model(home, root, "pinned-thread", "proxy:second-model", &[]).await,
        "first-model",
        "a concrete pin ignores mode edits"
    );
    assert_eq!(
        requested_model(
            home,
            root,
            "pinned-thread",
            "proxy:second-model",
            &["-m", "mode:smart"]
        )
        .await,
        "second-model",
        "-m wins for one run"
    );
}

/// A thread kept on a mode the root no longer defines, or an explicit
/// `-m mode:<missing>`, fails with an actionable error and sends nothing,
/// instead of running the default (possibly another account).
#[tokio::test]
async fn missing_mode_fails_without_reaching_a_provider() {
    if !can_bind_localhost() {
        eprintln!("Skipping: cannot bind localhost TCP port in this environment.");
        return;
    }
    let zdx_home = TempDir::new().unwrap();
    let work_dir = TempDir::new().unwrap();
    let mock_server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(sse_response(CHAT_COMPLETIONS_SSE))
        .expect(0)
        .mount(&mock_server)
        .await;
    write_config(zdx_home.path(), &mock_server.uri(), "proxy:first-model");
    write_thread(zdx_home.path(), "gone-thread", "mode:gone");

    let run = |extra: &[&str], thread: Option<&str>| {
        let mut cmd = crate::fixtures::zdx_cmd();
        cmd.env("ZDX_HOME", zdx_home.path())
            .args(["--root", work_dir.path().to_str().unwrap()]);
        match thread {
            Some(id) => cmd.args(["--thread", id]),
            None => cmd.arg("--no-thread"),
        };
        cmd.arg("exec").args(extra).args(["-p", "ping"]);
        cmd.assert().failure()
    };

    run(&[], Some("gone-thread"))
        .stderr(predicates::str::contains(
            "`mode:gone` names no configured model mode",
        ))
        .stderr(predicates::str::contains("configured: smart"));
    run(&["-m", "mode:nope"], None).stderr(predicates::str::contains(
        "`mode:nope` names no configured model mode",
    ));
    drop(mock_server);
}

/// A mode means what the workspace at `--root` defines, not the process cwd:
/// the run root's overlay redefines `smart`.
#[tokio::test]
async fn modes_resolve_in_the_run_root() {
    if !can_bind_localhost() {
        eprintln!("Skipping: cannot bind localhost TCP port in this environment.");
        return;
    }
    let zdx_home = TempDir::new().unwrap();
    let work_dir = TempDir::new().unwrap();
    std::fs::create_dir_all(work_dir.path().join(".zdx")).unwrap();
    std::fs::write(
        work_dir.path().join(".zdx/config.toml"),
        "[[model_modes]]\nname = \"smart\"\nprimary = \"proxy:second-model\"\n",
    )
    .unwrap();
    write_thread(zdx_home.path(), "root-thread", "mode:smart");
    assert_eq!(
        requested_model(
            zdx_home.path(),
            work_dir.path(),
            "root-thread",
            "proxy:first-model",
            &[]
        )
        .await,
        "second-model"
    );
}

/// A subagent override naming a mode resolves from config on every run (an
/// edit applies to the next one); one naming an undefined mode fails with an
/// actionable error and sends nothing, instead of running the definition's
/// model.
#[tokio::test]
async fn subagent_override_modes_resolve_per_run_or_fail() {
    if !can_bind_localhost() {
        eprintln!("Skipping: cannot bind localhost TCP port in this environment.");
        return;
    }
    let zdx_home = TempDir::new().unwrap();
    let work_dir = TempDir::new().unwrap();
    let run = |override_model: &str, smart: &str| {
        let zdx_home = zdx_home.path().to_path_buf();
        let work_dir = work_dir.path().to_path_buf();
        let override_model = override_model.to_string();
        let smart = smart.to_string();
        async move {
            let mock_server = MockServer::start().await;
            let seen: Arc<Mutex<Vec<Value>>> = Arc::new(Mutex::new(Vec::new()));
            let seen_clone = Arc::clone(&seen);
            Mock::given(method("POST"))
                .respond_with(move |req: &Request| {
                    seen_clone
                        .lock()
                        .unwrap()
                        .push(serde_json::from_slice(&req.body).unwrap_or(Value::Null));
                    sse_response(CHAT_COMPLETIONS_SSE)
                })
                .mount(&mock_server)
                .await;
            write_config(&zdx_home, &mock_server.uri(), &smart);
            let mut config = std::fs::read_to_string(zdx_home.join("config.toml")).unwrap();
            writeln!(
                config,
                "[subagents.overrides.explorer]\nmodel = \"{override_model}\""
            )
            .unwrap();
            std::fs::write(zdx_home.join("config.toml"), config).unwrap();
            let output = crate::fixtures::zdx_cmd()
                .env("ZDX_HOME", &zdx_home)
                .args(["--root", work_dir.to_str().unwrap(), "--no-thread", "exec"])
                .args(["--subagent", "explorer", "-p", "ping"])
                .output()
                .unwrap();
            drop(mock_server);
            let models: Vec<String> = seen
                .lock()
                .unwrap()
                .iter()
                .filter_map(|body| body["model"].as_str().map(str::to_string))
                .collect();
            (output, models)
        }
    };

    let (output, models) = run("mode:smart", "proxy:first-model").await;
    assert!(output.status.success(), "{output:?}");
    assert_eq!(models, vec!["first-model"]);
    let (output, models) = run("mode:smart", "proxy:second-model").await;
    assert!(output.status.success(), "{output:?}");
    assert_eq!(
        models,
        vec!["second-model"],
        "an edited mode applies to the next run"
    );

    let (output, models) = run("mode:gone", "proxy:first-model").await;
    assert!(!output.status.success());
    assert!(models.is_empty(), "nothing is sent: {models:?}");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("subagent 'explorer' override model")
            && stderr.contains("`mode:gone` names no configured model mode"),
        "{stderr}"
    );
}
