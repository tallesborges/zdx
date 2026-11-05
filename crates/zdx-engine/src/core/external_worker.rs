//! External worker runner: runs one worker turn through a configured launcher
//! (`[workers.external]`), which executes `zdx exec` somewhere else (a
//! container, VM, or sandbox) and returns the result. The contract is SPEC §18,
//! External worker runner. ZDX owns the request, the live activity stream, the
//! transcript import, and when a turn counts as done; the launcher owns the
//! sandbox provider, workspace mapping, credentials, and file transfer.

use std::ffi::OsStr;
use std::path::{Component, Path, PathBuf};
use std::sync::Weak;
use std::time::Duration;

use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncReadExt};
use zdx_tools::process_supervisor::{SupervisedCommand, WaitOutcome, WaitReason};

use crate::config::{Config, ExternalWorkerConfig, paths};
use crate::core::subagent::{ChildEvent, read_stdout_events};
use crate::core::thread_persistence;
use crate::core::workers::{WorkerActivity, WorkerEvent, WorkerManager, WorkerRunRequest};

/// Version of the request and result files. A launcher must refuse a request
/// with a version it does not know.
pub const PROTOCOL_VERSION: u32 = 1;

const REQUEST_FILE: &str = "request.json";
const TRANSCRIPT_FILE: &str = "transcript.jsonl";
const RESULT_FILE: &str = "result.json";
const STDERR_TAIL_BYTES: usize = 4096;
const ERROR_STDERR_CHARS: usize = 500;

/// The request file ZDX writes for one worker turn (`request.json`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LaunchRequest {
    pub protocol: u32,
    /// Unique per turn; names the job directory.
    pub turn_id: String,
    pub worker_thread_id: String,
    /// Thread that queued this prompt; its result is delivered there.
    pub requester_thread_id: String,
    /// The worker's recorded root on this host. An identity for the launcher
    /// to map to a remote workspace; it need not exist on this host.
    pub host_root: PathBuf,
    /// The full prompt for this turn.
    pub prompt: String,
    /// Model spec to pass as `zdx exec -m` (`provider:model[@thinking]`).
    pub model: String,
    /// `worker_override` (the worker's pinned model) or `host_config` (the
    /// host config layers for `host_root`).
    pub model_source: String,
    /// Thread JSONL to resume (a copy; the launcher may read but not edit it).
    pub transcript: PathBuf,
    /// Where the launcher writes its result file.
    pub result: PathBuf,
    /// Local directory for files the launcher brings back from the turn.
    pub artifact_dir: PathBuf,
    /// Turn deadline, when configured.
    pub timeout_secs: Option<u64>,
    /// Seconds the launcher gets after `SIGTERM` before it is killed.
    pub cancel_grace_secs: u64,
}

/// Terminal state the launcher reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LaunchStatus {
    Completed,
    Failed,
    Cancelled,
}

/// The result file the launcher writes (`result.json`) before it exits.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LaunchResult {
    pub protocol: u32,
    pub status: LaunchStatus,
    #[serde(default)]
    pub final_text: Option<String>,
    #[serde(default)]
    pub error: Option<String>,
    /// The thread JSONL after the turn (relative to the job directory or
    /// absolute). Required for `completed`.
    #[serde(default)]
    pub transcript: Option<PathBuf>,
    /// Files placed under `artifact_dir`, as paths relative to it.
    #[serde(default)]
    pub artifacts: Vec<PathBuf>,
}

/// Runs worker turns through the configured launcher.
#[derive(Debug, Clone)]
pub struct ExternalRunner {
    program: PathBuf,
    args: Vec<String>,
    timeout: Option<Duration>,
    grace: Duration,
}

impl ExternalRunner {
    /// Validates `[workers.external]`.
    ///
    /// # Errors
    /// Returns an error when `command` is empty or its program is not an
    /// absolute path.
    pub fn from_config(config: &ExternalWorkerConfig) -> Result<Self> {
        let (program, args) = config
            .command
            .split_first()
            .context("[workers.external] command must name the launcher")?;
        let program = PathBuf::from(program);
        ensure!(
            program.is_absolute(),
            "[workers.external] command[0] must be an absolute path, got {}",
            program.display()
        );
        Ok(Self {
            program,
            args: args.to_vec(),
            timeout: (config.timeout_secs > 0).then(|| Duration::from_secs(config.timeout_secs)),
            grace: Duration::from_secs(config.cancel_grace_secs.max(1)),
        })
    }

    fn command(&self, verb: &str, request: &Path, job_dir: &Path) -> SupervisedCommand {
        let mut command = SupervisedCommand::new(&self.program);
        command
            .args(&self.args)
            .args([OsStr::new(verb), request.as_os_str()])
            .current_dir(job_dir)
            .stdin_null();
        command
    }

    /// Runs one worker turn. Returns the final text only when the launcher
    /// exited 0, reported `completed` with a non-empty final text, its
    /// transcript was imported, and every listed artifact exists.
    ///
    /// # Errors
    /// Returns an error for every other outcome; the message says whether
    /// remote cleanup was confirmed.
    pub async fn run(
        &self,
        request: WorkerRunRequest,
        manager: Weak<WorkerManager>,
    ) -> Result<String> {
        let parent = paths::zdx_home().join("run").join("workers");
        std::fs::create_dir_all(&parent).context("Failed to create worker job directory")?;
        let job = tempfile::Builder::new()
            .prefix(&format!("{}-", request.worker_thread_id))
            .tempdir_in(&parent)
            .context("Failed to create worker job directory")?;
        let job_dir = job.path().to_path_buf();

        let result = self.run_in(&request, &job_dir, manager).await;
        if result.is_ok() {
            drop(job);
        } else {
            let kept = job.keep();
            if request.cancel.is_cancelled() {
                tracing::debug!(job_dir = %kept.display(), worker = %request.worker_thread_id, "External worker turn cancelled; job directory kept");
            } else {
                tracing::warn!(job_dir = %kept.display(), worker = %request.worker_thread_id, "External worker turn failed; job directory kept");
            }
        }
        result
    }

    async fn run_in(
        &self,
        request: &WorkerRunRequest,
        job_dir: &Path,
        manager: Weak<WorkerManager>,
    ) -> Result<String> {
        let (launch, request_path) = prepare(request, job_dir, self)?;
        tracing::info!(worker = %request.worker_thread_id, turn = %launch.turn_id, "External worker turn started");
        let (outcome, stderr) = self
            .launch(request, &request_path, job_dir, manager)
            .await?;

        let result = read_result(&launch.result);
        let mut imported = None;
        if let Ok(result) = &result
            && let Some(path) = &result.transcript
        {
            let path = job_dir.join(path);
            imported = Some(
                thread_persistence::import_runner_events(
                    &request.worker_thread_id,
                    &launch.transcript,
                    &path,
                )
                .map_err(|err| format!("{err:#}")),
            );
        }

        let ended = match outcome.reason {
            WaitReason::Cancelled => Some("Worker cancelled".to_string()),
            WaitReason::TimedOut => Some(format!(
                "Worker timed out after {} seconds",
                self.timeout.map_or(0, |t| t.as_secs())
            )),
            WaitReason::Exited => None,
        };

        let result = match result {
            Ok(result) => result,
            Err(err) => {
                // No terminal state from the launcher: the remote side may still
                // run, so ask the launcher to clean it up.
                let cleanup = self.cleanup(&request_path, job_dir).await;
                let reason = ended.unwrap_or_else(|| {
                    format!(
                        "Worker launcher exited ({}) without a valid result",
                        outcome.status
                    )
                });
                bail!(
                    "{reason}: {err:#}; {}{}",
                    cleanup_note(&cleanup),
                    stderr_note(&stderr)
                );
            }
        };

        if let Some(reason) = ended {
            // A result file means the launcher stopped everything it started.
            bail!(
                "{reason}; the launcher confirmed the remote run stopped (reported `{}`){}",
                status_name(result.status),
                import_note(imported.as_ref())
            );
        }
        match result.status {
            LaunchStatus::Completed => {}
            LaunchStatus::Failed => bail!(
                "Worker failed: {}{}",
                result
                    .error
                    .as_deref()
                    .filter(|e| !e.trim().is_empty())
                    .unwrap_or("the launcher reported a failure without a message"),
                import_note(imported.as_ref())
            ),
            LaunchStatus::Cancelled => bail!(
                "The launcher reported the turn cancelled{}",
                import_note(imported.as_ref())
            ),
        }
        if let Some(Err(err)) = &imported {
            bail!("The worker transcript was not imported: {err}");
        }
        ensure!(
            outcome.status.success(),
            "Worker launcher reported completed but exited with {}{}",
            outcome.status,
            stderr_note(&stderr)
        );
        ensure!(
            imported.is_some(),
            "Worker result is completed but has no transcript"
        );
        check_artifacts(&launch.artifact_dir, &result.artifacts)?;
        let text = result.final_text.unwrap_or_default();
        ensure!(
            !text.trim().is_empty(),
            "Worker result is completed but has no final text"
        );
        tracing::info!(worker = %request.worker_thread_id, turn = %launch.turn_id, "External worker turn finished");
        Ok(text)
    }

    /// Starts `<command> run <request.json>`, relays its stream as worker
    /// activity, and waits for it under the cancel token and turn deadline.
    async fn launch(
        &self,
        request: &WorkerRunRequest,
        request_path: &Path,
        job_dir: &Path,
        manager: Weak<WorkerManager>,
    ) -> Result<(WaitOutcome, String)> {
        let mut child = self
            .command("run", request_path, job_dir)
            .spawn(self.grace)
            .await
            .context("Failed to start the worker launcher")?;
        let stdout = child
            .stdout
            .take()
            .context("Launcher stdout was not piped")?;
        let stderr = child
            .stderr
            .take()
            .context("Launcher stderr was not piped")?;
        let stderr_task = tokio::spawn(read_tail(stderr));
        let worker_id = request.worker_thread_id.clone();
        let events = request.events.clone();
        let stdout_task = tokio::spawn(async move {
            read_stdout_events(stdout, |event| {
                let ChildEvent::Tool(chunk) = event else {
                    return;
                };
                let Some(activity) = WorkerActivity::from_chunk_value(chunk) else {
                    return;
                };
                if let Some(manager) = manager.upgrade() {
                    manager.record_activity(&worker_id, &activity);
                }
                let _ = events.send(WorkerEvent::Activity {
                    worker_thread_id: worker_id.clone(),
                    activity,
                });
            })
            .await
        });

        let outcome = child
            .wait_with(Some(&request.cancel), self.timeout)
            .await
            .context("Failed to wait for the worker launcher")?;
        // The stream is only a progress view; a read error does not decide
        // the turn, the result file does.
        let _ = stdout_task.await;
        let stderr = stderr_task.await.unwrap_or_default();
        Ok((outcome, stderr))
    }

    /// `<command> cleanup <request.json>`: asks the launcher to stop and remove
    /// whatever the run started, bounded by the grace period.
    async fn cleanup(&self, request_path: &Path, job_dir: &Path) -> Result<()> {
        let mut child = self
            .command("cleanup", request_path, job_dir)
            .spawn(Duration::from_secs(5))
            .await
            .context("Failed to start the launcher cleanup")?;
        let stdout = child.stdout.take().map(|out| tokio::spawn(read_tail(out)));
        let stderr = child.stderr.take().map(|err| tokio::spawn(read_tail(err)));
        let outcome = child
            .wait_with(None, Some(self.grace))
            .await
            .context("Failed to wait for the launcher cleanup")?;
        if let Some(task) = stdout {
            let _ = task.await;
        }
        let stderr = match stderr {
            Some(task) => task.await.unwrap_or_default(),
            None => String::new(),
        };
        match outcome.reason {
            WaitReason::Exited if outcome.status.success() => Ok(()),
            WaitReason::Exited => bail!("exited with {}{}", outcome.status, stderr_note(&stderr)),
            WaitReason::TimedOut | WaitReason::Cancelled => {
                bail!("did not finish within {} seconds", self.grace.as_secs())
            }
        }
    }
}

/// Writes the job directory: the transcript snapshot and `request.json`.
fn prepare(
    request: &WorkerRunRequest,
    job_dir: &Path,
    runner: &ExternalRunner,
) -> Result<(LaunchRequest, PathBuf)> {
    let (model, model_source) = resolve_model(request)?;
    let transcript = job_dir.join(TRANSCRIPT_FILE);
    thread_persistence::snapshot_thread_file(&request.worker_thread_id, &transcript)?;
    let artifact_dir = paths::artifact_dir_for_thread(Some(&request.worker_thread_id));
    std::fs::create_dir_all(&artifact_dir).context("Failed to create artifact directory")?;
    let launch = LaunchRequest {
        protocol: PROTOCOL_VERSION,
        turn_id: uuid::Uuid::new_v4().to_string(),
        worker_thread_id: request.worker_thread_id.clone(),
        requester_thread_id: request.owner_thread_id.clone(),
        host_root: request.root.clone(),
        prompt: request.prompt.clone(),
        model,
        model_source: model_source.to_string(),
        transcript,
        result: job_dir.join(RESULT_FILE),
        artifact_dir,
        timeout_secs: runner.timeout.map(|t| t.as_secs()),
        cancel_grace_secs: runner.grace.as_secs(),
    };
    let request_path = job_dir.join(REQUEST_FILE);
    std::fs::write(&request_path, serde_json::to_vec_pretty(&launch)?)
        .context("Failed to write worker request")?;
    Ok((launch, request_path))
}

/// The model the remote `zdx exec` must run: the worker's pinned override, or
/// the host config layers for its root (the same chain a local child uses),
/// so the request never depends on the remote config.
fn resolve_model(request: &WorkerRunRequest) -> Result<(String, &'static str)> {
    if let Some(model) = request.model.as_deref() {
        let model = request.thinking_level.map_or_else(
            || model.to_string(),
            |level| crate::models::format_model_thinking(model, level),
        );
        return Ok((model, "worker_override"));
    }
    let config = Config::load_layered(&paths::config_layer_paths_for(&request.root))
        .context("Failed to load the host config for the worker root")?;
    Ok((
        crate::models::format_model_thinking(&config.model, config.thinking_level),
        "host_config",
    ))
}

fn read_result(path: &Path) -> Result<LaunchResult> {
    let bytes = std::fs::read(path).context("the result file is missing")?;
    let result: LaunchResult =
        serde_json::from_slice(&bytes).context("the result file is not valid JSON")?;
    ensure!(
        result.protocol == PROTOCOL_VERSION,
        "the result file has protocol {}, expected {PROTOCOL_VERSION}",
        result.protocol
    );
    Ok(result)
}

fn check_artifacts(artifact_dir: &Path, artifacts: &[PathBuf]) -> Result<()> {
    for artifact in artifacts {
        ensure!(
            !artifact.as_os_str().is_empty()
                && artifact
                    .components()
                    .all(|part| matches!(part, Component::Normal(_))),
            "Worker artifact path must be relative to the artifact directory: {}",
            artifact.display()
        );
        let path = artifact_dir.join(artifact);
        let is_file = std::fs::symlink_metadata(&path).is_ok_and(|meta| meta.is_file());
        ensure!(
            is_file,
            "Worker artifact {} is missing from {}",
            artifact.display(),
            artifact_dir.display()
        );
    }
    Ok(())
}

const fn status_name(status: LaunchStatus) -> &'static str {
    match status {
        LaunchStatus::Completed => "completed",
        LaunchStatus::Failed => "failed",
        LaunchStatus::Cancelled => "cancelled",
    }
}

fn cleanup_note(cleanup: &Result<()>) -> String {
    match cleanup {
        Ok(()) => "the launcher cleanup call succeeded".to_string(),
        Err(err) => format!("remote cleanup NOT confirmed: the launcher cleanup call {err:#}"),
    }
}

fn import_note(imported: Option<&std::result::Result<usize, String>>) -> String {
    match imported {
        Some(Ok(_)) => "; the partial transcript was imported".to_string(),
        Some(Err(err)) => format!("; the partial transcript was not imported: {err}"),
        None => String::new(),
    }
}

fn stderr_note(stderr: &str) -> String {
    let stderr = stderr.trim();
    if stderr.is_empty() {
        return String::new();
    }
    let tail: String = stderr
        .chars()
        .rev()
        .take(ERROR_STDERR_CHARS)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    format!("; launcher stderr: {tail}")
}

/// Drains a pipe, keeping only its last bytes.
async fn read_tail<R: AsyncRead + Unpin>(mut reader: R) -> String {
    let mut tail = Vec::new();
    let mut buf = [0_u8; 8192];
    while let Ok(read) = reader.read(&mut buf).await {
        if read == 0 {
            break;
        }
        tail.extend_from_slice(&buf[..read]);
        if tail.len() > STDERR_TAIL_BYTES {
            tail.drain(..tail.len() - STDERR_TAIL_BYTES);
        }
    }
    String::from_utf8_lossy(&tail).into_owned()
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;
    use std::sync::Arc;

    use tokio::sync::mpsc;

    use super::*;
    use crate::config::{WorkerRunnerKind, WorkersConfig};
    use crate::core::workers::{CompletionEvent, WorkerStatus};
    use crate::test_support::temp_zdx_home;

    /// Fake launcher. `$1` is the mode, `$2` the verb, `$3` the request file;
    /// the working directory is the job directory. It records what it got in
    /// the marks directory.
    const FAKE_LAUNCHER: &str = r#"#!/bin/sh
mode="$1"; verb="$2"; req="$3"; marks="__MARKS__"
if [ "$verb" = cleanup ]; then
  [ -f "$req" ] && echo cleaned >> "$marks/cleanup"
  exit 0
fi
n=$(cat "$marks/count" 2>/dev/null || echo 0); n=$((n+1)); echo "$n" > "$marks/count"
cp request.json "$marks/request-$n"
cp transcript.jsonl "$marks/transcript-$n"
echo '{"type":"tool_started","id":"t1","name":"bash"}'
echo 'launcher log line, not an event'
result() { printf '%s' "$1" > result.tmp && mv result.tmp result.json; }
extend() {
  cp transcript.jsonl out.jsonl
  echo '{"type":"message","role":"user","text":"remote prompt '"$n"'","ts":"2026-01-01T00:00:00Z"}' >> out.jsonl
  echo '{"type":"message","role":"assistant","text":"remote answer '"$n"'","ts":"2026-01-01T00:00:01Z"}' >> out.jsonl
}
case "$mode" in
  complete)
    extend
    dir=$(sed -n 's/^ *"artifact_dir": "\(.*\)",$/\1/p' request.json)
    echo report > "$dir/report.txt"
    echo '{"type":"tool_completed","id":"t1","result":{"ok":true,"data":null}}'
    result '{"protocol":1,"status":"completed","final_text":"remote answer '"$n"'","transcript":"out.jsonl","artifacts":["report.txt"]}'
    ;;
  missing-artifact)
    extend
    result '{"protocol":1,"status":"completed","final_text":"x","transcript":"out.jsonl","artifacts":["nope.txt"]}'
    ;;
  tampered)
    head -n 1 transcript.jsonl > out.jsonl
    echo '{"type":"message","role":"assistant","text":"rewritten history","ts":"2026-01-01T00:00:01Z"}' >> out.jsonl
    result '{"protocol":1,"status":"completed","final_text":"x","transcript":"out.jsonl"}'
    ;;
  fail)
    echo 'remote zdx failed' >&2
    result '{"protocol":1,"status":"failed","error":"boom"}'
    exit 1
    ;;
  no-result)
    exit 0
    ;;
  cooperative)
    trap 'kill $pid 2>/dev/null; extend; result "{\"protocol\":1,\"status\":\"cancelled\",\"transcript\":\"out.jsonl\"}"; exit 0' TERM
    sleep 30 & pid=$!
    wait $pid
    ;;
  stubborn)
    trap '' TERM
    sleep 30
    ;;
esac
"#;

    struct Fake {
        _dir: tempfile::TempDir,
        script: PathBuf,
        marks: PathBuf,
    }

    fn fake_launcher() -> Fake {
        let dir = tempfile::tempdir().unwrap();
        let marks = dir.path().join("marks");
        std::fs::create_dir(&marks).unwrap();
        let script = dir.path().join("launcher.sh");
        std::fs::write(
            &script,
            FAKE_LAUNCHER.replace("__MARKS__", marks.to_str().unwrap()),
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        Fake {
            _dir: dir,
            script,
            marks,
        }
    }

    fn external_manager(
        fake: &Fake,
        mode: &str,
        timeout_secs: u64,
        cancel_grace_secs: u64,
    ) -> (Arc<WorkerManager>, mpsc::UnboundedReceiver<WorkerEvent>) {
        let config = WorkersConfig {
            runner: WorkerRunnerKind::External,
            external: Some(ExternalWorkerConfig {
                command: vec![fake.script.to_str().unwrap().to_string(), mode.to_string()],
                timeout_secs,
                cancel_grace_secs,
            }),
        };
        WorkerManager::from_config(&config).unwrap()
    }

    /// Collects events until the next completion; returns it with the
    /// activity seen on the way.
    async fn completion(
        rx: &mut mpsc::UnboundedReceiver<WorkerEvent>,
    ) -> (CompletionEvent, Vec<WorkerActivity>) {
        let mut activity = Vec::new();
        let wait = async {
            loop {
                match rx.recv().await.expect("worker events open") {
                    WorkerEvent::Completed(event) => return event,
                    WorkerEvent::Activity { activity: a, .. } => activity.push(a),
                    _ => {}
                }
            }
        };
        let event = tokio::time::timeout(Duration::from_secs(30), wait)
            .await
            .expect("worker turn should finish");
        (event, activity)
    }

    fn messages(thread_id: &str) -> Vec<(String, String)> {
        thread_persistence::load_thread_events(thread_id)
            .unwrap()
            .into_iter()
            .filter_map(|event| match event {
                thread_persistence::ThreadEvent::Message { role, text, .. } => Some((role, text)),
                _ => None,
            })
            .collect()
    }

    const REMOTE_ROOT: &str = "/remote-only/project";

    #[tokio::test]
    async fn completed_turns_stream_import_and_resume() {
        let _home = temp_zdx_home();
        let fake = fake_launcher();
        let (manager, mut rx) = external_manager(&fake, "complete", 0, 5);

        let worker = manager
            .create_worker("owner", Path::new(REMOTE_ROOT), "first", None, None, None)
            .unwrap();
        let (event, activity) = completion(&mut rx).await;
        assert_eq!(event.status, WorkerStatus::Completed, "{:?}", event.error);
        assert_eq!(event.owner_thread_id, "owner");
        assert_eq!(event.final_text.as_deref(), Some("remote answer 1"));
        assert!(activity.contains(&WorkerActivity::ToolStarted {
            id: "t1".into(),
            name: "bash".into()
        }));

        let request: LaunchRequest =
            serde_json::from_slice(&std::fs::read(fake.marks.join("request-1")).unwrap()).unwrap();
        assert_eq!(request.protocol, PROTOCOL_VERSION);
        assert_eq!(request.worker_thread_id, worker);
        assert_eq!(request.requester_thread_id, "owner");
        assert_eq!(request.host_root, PathBuf::from(REMOTE_ROOT));
        assert_eq!(request.prompt, "first");
        assert_eq!(request.model_source, "host_config");
        assert!(!request.model.is_empty());
        assert!(
            paths::artifact_dir_for_thread(Some(&worker))
                .join("report.txt")
                .is_file()
        );

        // Second turn resumes from the imported history.
        manager.send_message("owner", &worker, "second").unwrap();
        let (event, _) = completion(&mut rx).await;
        assert_eq!(event.status, WorkerStatus::Completed, "{:?}", event.error);
        let sent = std::fs::read_to_string(fake.marks.join("transcript-2")).unwrap();
        assert!(sent.contains("remote answer 1"));
        assert_eq!(
            messages(&worker),
            vec![
                ("user".into(), "remote prompt 1".into()),
                ("assistant".into(), "remote answer 1".into()),
                ("user".into(), "remote prompt 2".into()),
                ("assistant".into(), "remote answer 2".into()),
            ]
        );
        // Successful job directories are removed.
        let jobs = paths::zdx_home().join("run").join("workers");
        assert_eq!(std::fs::read_dir(jobs).unwrap().count(), 0);
        assert!(!fake.marks.join("cleanup").exists());
    }

    #[tokio::test]
    async fn pinned_model_is_sent_as_the_worker_override() {
        let _home = temp_zdx_home();
        let fake = fake_launcher();
        let (manager, mut rx) = external_manager(&fake, "complete", 0, 5);
        manager
            .create_worker(
                "owner",
                Path::new(REMOTE_ROOT),
                "go",
                None,
                Some("openai:gpt-test".into()),
                Some(crate::config::ThinkingLevel::High),
            )
            .unwrap();
        let (event, _) = completion(&mut rx).await;
        assert_eq!(event.status, WorkerStatus::Completed, "{:?}", event.error);
        let request: LaunchRequest =
            serde_json::from_slice(&std::fs::read(fake.marks.join("request-1")).unwrap()).unwrap();
        assert_eq!(request.model, "openai:gpt-test@high");
        assert_eq!(request.model_source, "worker_override");
    }

    #[tokio::test]
    async fn reported_failure_fails_the_turn_without_cleanup() {
        let _home = temp_zdx_home();
        let fake = fake_launcher();
        let (manager, mut rx) = external_manager(&fake, "fail", 0, 5);
        manager
            .create_worker("owner", Path::new(REMOTE_ROOT), "go", None, None, None)
            .unwrap();
        let (event, _) = completion(&mut rx).await;
        assert_eq!(event.status, WorkerStatus::Failed);
        assert!(event.error.as_deref().unwrap().contains("boom"));
        assert!(!fake.marks.join("cleanup").exists());
    }

    #[tokio::test]
    async fn exit_without_result_is_not_success_and_runs_cleanup() {
        let _home = temp_zdx_home();
        let fake = fake_launcher();
        let (manager, mut rx) = external_manager(&fake, "no-result", 0, 5);
        let worker = manager
            .create_worker("owner", Path::new(REMOTE_ROOT), "go", None, None, None)
            .unwrap();
        let (event, _) = completion(&mut rx).await;
        assert_eq!(event.status, WorkerStatus::Failed);
        let error = event.error.unwrap();
        assert!(error.contains("without a valid result"), "{error}");
        assert!(error.contains("cleanup call succeeded"), "{error}");
        assert!(fake.marks.join("cleanup").exists());
        assert!(messages(&worker).is_empty());
        // The failed job directory is kept for diagnosis.
        let jobs = paths::zdx_home().join("run").join("workers");
        assert_eq!(std::fs::read_dir(jobs).unwrap().count(), 1);
    }

    #[tokio::test]
    async fn rewritten_history_is_rejected_and_nothing_is_imported() {
        let _home = temp_zdx_home();
        let fake = fake_launcher();
        let (manager, mut rx) = external_manager(&fake, "complete", 0, 5);
        let worker = manager
            .create_worker("owner", Path::new(REMOTE_ROOT), "first", None, None, None)
            .unwrap();
        let (event, _) = completion(&mut rx).await;
        assert_eq!(event.status, WorkerStatus::Completed, "{:?}", event.error);
        let before = messages(&worker);

        let (tampering, mut rx) = external_manager(&fake, "tampered", 0, 5);
        tampering.send_message("owner", &worker, "second").unwrap();
        let (event, _) = completion(&mut rx).await;
        assert_eq!(event.status, WorkerStatus::Failed);
        assert!(
            event.error.as_deref().unwrap().contains("not imported"),
            "{:?}",
            event.error
        );
        assert_eq!(messages(&worker), before);
    }

    #[tokio::test]
    async fn missing_artifact_fails_a_completed_turn() {
        let _home = temp_zdx_home();
        let fake = fake_launcher();
        let (manager, mut rx) = external_manager(&fake, "missing-artifact", 0, 5);
        manager
            .create_worker("owner", Path::new(REMOTE_ROOT), "go", None, None, None)
            .unwrap();
        let (event, _) = completion(&mut rx).await;
        assert_eq!(event.status, WorkerStatus::Failed);
        assert!(event.error.as_deref().unwrap().contains("nope.txt"));
    }

    async fn wait_for_activity(rx: &mut mpsc::UnboundedReceiver<WorkerEvent>) {
        let wait = async {
            while let Some(event) = rx.recv().await {
                if matches!(event, WorkerEvent::Activity { .. }) {
                    return;
                }
            }
        };
        tokio::time::timeout(Duration::from_secs(30), wait)
            .await
            .expect("launcher should start");
    }

    #[tokio::test]
    async fn cancel_lets_the_launcher_confirm_cleanup() {
        let _home = temp_zdx_home();
        let fake = fake_launcher();
        let (manager, mut rx) = external_manager(&fake, "cooperative", 0, 10);
        let worker = manager
            .create_worker("owner", Path::new(REMOTE_ROOT), "go", None, None, None)
            .unwrap();
        wait_for_activity(&mut rx).await;
        manager.cancel(&worker).unwrap();
        let (event, _) = completion(&mut rx).await;
        assert_eq!(event.status, WorkerStatus::Cancelled);
        let error = event.error.unwrap();
        assert!(
            error.contains("confirmed the remote run stopped"),
            "{error}"
        );
        assert!(error.contains("partial transcript was imported"), "{error}");
        assert!(!fake.marks.join("cleanup").exists());
        assert_eq!(messages(&worker).len(), 2);
    }

    #[tokio::test]
    async fn cancel_of_a_stuck_launcher_kills_it_and_runs_cleanup() {
        let _home = temp_zdx_home();
        let fake = fake_launcher();
        let (manager, mut rx) = external_manager(&fake, "stubborn", 0, 1);
        let worker = manager
            .create_worker("owner", Path::new(REMOTE_ROOT), "go", None, None, None)
            .unwrap();
        wait_for_activity(&mut rx).await;
        manager.cancel(&worker).unwrap();
        let (event, _) = completion(&mut rx).await;
        assert_eq!(event.status, WorkerStatus::Cancelled);
        let error = event.error.unwrap();
        assert!(error.contains("Worker cancelled"), "{error}");
        assert!(error.contains("cleanup call succeeded"), "{error}");
        assert!(fake.marks.join("cleanup").exists());
    }

    #[tokio::test]
    async fn timeout_fails_the_turn_after_launcher_cleanup() {
        let _home = temp_zdx_home();
        let fake = fake_launcher();
        let (manager, mut rx) = external_manager(&fake, "cooperative", 1, 10);
        manager
            .create_worker("owner", Path::new(REMOTE_ROOT), "go", None, None, None)
            .unwrap();
        let (event, _) = completion(&mut rx).await;
        assert_eq!(event.status, WorkerStatus::Failed);
        let error = event.error.unwrap();
        assert!(error.contains("timed out after 1 seconds"), "{error}");
        assert!(
            error.contains("confirmed the remote run stopped"),
            "{error}"
        );
    }

    #[tokio::test]
    async fn external_roots_must_be_absolute_and_local_roots_must_exist() {
        let _home = temp_zdx_home();
        let fake = fake_launcher();
        let (external, _rx) = external_manager(&fake, "complete", 0, 5);
        assert!(
            external
                .create_worker(
                    "owner",
                    Path::new("relative/project"),
                    "go",
                    None,
                    None,
                    None
                )
                .is_err()
        );
        assert!(
            external
                .create_worker("owner", Path::new("/a/../b"), "go", None, None, None)
                .is_err()
        );

        let (local, _rx) = WorkerManager::from_config(&WorkersConfig::default()).unwrap();
        let err = local
            .create_worker("owner", Path::new(REMOTE_ROOT), "go", None, None, None)
            .unwrap_err();
        assert!(format!("{err:#}").contains("does not exist"));
    }

    #[test]
    fn external_runner_config_is_validated() {
        let missing = WorkersConfig {
            runner: WorkerRunnerKind::External,
            external: None,
        };
        assert!(WorkerManager::from_config(&missing).is_err());
        let relative = ExternalWorkerConfig {
            command: vec!["launcher".into()],
            ..ExternalWorkerConfig::default()
        };
        assert!(ExternalRunner::from_config(&relative).is_err());
        assert!(ExternalRunner::from_config(&ExternalWorkerConfig::default()).is_err());

        let parsed: crate::config::Config = toml::from_str(
            "[workers]\nrunner = \"external\"\n[workers.external]\ncommand = [\"/bin/launcher\", \"--x\"]\ntimeout_secs = 5\n",
        )
        .unwrap();
        assert_eq!(parsed.workers.runner, WorkerRunnerKind::External);
        let external = parsed.workers.external.unwrap();
        assert_eq!(external.command, vec!["/bin/launcher", "--x"]);
        assert_eq!(external.cancel_grace_secs, 60);
        assert!(
            toml::from_str::<crate::config::Config>("[workers]\nrunner = \"remote\"\n").is_err()
        );
    }
}
