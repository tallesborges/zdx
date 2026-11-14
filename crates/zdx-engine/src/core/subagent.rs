//! Subagent execution helpers.
//!
//! Provides a reusable way to run an isolated child `zdx exec` process and
//! capture response text only.

use std::collections::HashSet;
use std::ffi::OsString;
use std::io::Write;
use std::path::{Path, PathBuf};
#[cfg(not(unix))]
use std::process::Stdio;
use std::time::Duration;

use anyhow::{Context, Result, bail, ensure};
use serde_json::Value;
use tempfile::{Builder, TempPath};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, BufReader};
use tokio_util::sync::CancellationToken;
#[cfg(unix)]
use zdx_tools::process_supervisor::{SupervisedChild, SupervisedCommand, WaitReason};

use crate::core::agent::EventSender;
use crate::core::events::{AgentEvent, TurnStatus};

#[cfg(unix)]
pub type ExecChild = SupervisedChild;
#[cfg(not(unix))]
pub type ExecChild = tokio::process::Child;

/// Options for a child `zdx exec` subagent run.
#[derive(Debug, Clone, Default)]
pub struct ExecSubagentOptions {
    /// Optional model override (`-m provider:model[@thinking][@fast]`).
    pub model: Option<String>,
    /// Optional final system prompt override (`--effective-system-prompt`).
    pub system_prompt: Option<String>,
    /// Resolved thinking projection folded into `model` at the process boundary.
    pub thinking_level: Option<crate::config::ThinkingLevel>,
    /// Disable tools for the child run (`--no-tools`).
    pub no_tools: bool,
    /// Disable system prompt/context composition for the child run (`--no-system-prompt`).
    pub no_system_prompt: bool,
    /// Run the child without background execution in `Bash`
    /// (`--no-background-commands`). Set for subagents whose definition does
    /// not opt in; workers and helpers keep the default.
    pub no_background_commands: bool,
    /// Optional explicit tool allowlist for the child run (`--tools`).
    pub tools_override: Option<Vec<String>>,
    /// Optional event type filters for exec output (`--filter`).
    pub event_filter: Option<Vec<String>>,
    /// Optional timeout for the child process.
    pub timeout: Option<Duration>,
    /// Logical role for the child run (e.g. `"subagent"`).
    pub activity_kind: Option<String>,
    /// Parent thread id for the child run.
    pub activity_parent_thread_id: Option<String>,
    /// Named subagent (e.g. `"explorer"`) for the child run.
    pub activity_subagent_name: Option<String>,
    /// Origin kind recorded in the child thread's meta (e.g. `"subagent"`,
    /// `"helper:title"`). When set, the child persists a tagged thread instead
    /// of running throwaway. `None` leaves the child thread untagged.
    pub thread_origin_kind: Option<String>,
    /// Parent thread id recorded in the child thread's meta.
    pub thread_parent_id: Option<String>,
    /// Named subagent recorded in the child thread's meta (subagent runs).
    pub thread_subagent_name: Option<String>,
    /// Explicit thread id for the child run (`--thread`). The child appends to
    /// this existing thread (loading its prior history), so repeated runs
    /// against the same id behave like a resumable worker thread.
    pub thread_id: Option<String>,
    /// Worker steer file (`--steer-file`): the child reads new steers there
    /// at safe points and reports each one it adds on its event stream.
    pub steer_file: Option<PathBuf>,
    /// Environment of the run that starts the child, passed on top of the
    /// process environment (`ToolContext::env`). Empty: process env only.
    pub env: zdx_tools::ToolEnv,
}

#[derive(Debug)]
struct TempPromptFile {
    path: TempPath,
}

impl TempPromptFile {
    fn as_path(&self) -> &Path {
        &self.path
    }
}

/// Runs an isolated child `zdx exec` process and returns response text only.
///
/// The child persists its own thread (tagged via `thread_origin_kind` and
/// friends) so its usage is captured by thread-scanning stats.
///
/// # Errors
/// Returns an error if the child process fails, times out, or produces empty output.
pub async fn run_exec_subagent(
    root: &Path,
    prompt: &str,
    options: &ExecSubagentOptions,
) -> Result<String> {
    run_exec_subagent_with_cancel(root, prompt, options, None, None).await
}

/// Log a child subagent that exited non-zero. Shared by the buffered and
/// streaming paths so both report the same fields.
fn log_nonzero_exit(status: std::process::ExitStatus, stderr: &str) {
    tracing::warn!(
        status = status.code().unwrap_or(-1),
        stderr = %zdx_types::log_field(stderr, 300),
        "Subagent exited non-zero"
    );
}

/// Runs an isolated child `zdx exec` process with optional cancellation support.
///
/// # Errors
/// Returns an error if the child process fails, times out, is canceled, or
/// produces invalid/empty output.
pub async fn run_exec_subagent_with_cancel(
    root: &Path,
    prompt: &str,
    options: &ExecSubagentOptions,
    cancel: Option<CancellationToken>,
    stream: Option<SubagentStreamSink>,
) -> Result<String> {
    run_exec_subagent_holding(root, prompt, options, cancel, stream, ()).await
}

/// Like [`run_exec_subagent_with_cancel`], and keeps `hold` alive until the
/// child has exited.
///
/// On Unix `hold` outlives this future: if the future is dropped while the
/// child runs, the lease stops the child and `hold` is released only once the
/// child has been reaped.
///
/// # Errors
/// Same as [`run_exec_subagent_with_cancel`].
#[tracing::instrument(
    name = "subagent",
    skip_all,
    fields(
        subagent = options.activity_subagent_name.as_deref().unwrap_or("-"),
        model = options.model.as_deref().unwrap_or("-"),
    )
)]
pub async fn run_exec_subagent_holding(
    root: &Path,
    prompt: &str,
    options: &ExecSubagentOptions,
    cancel: Option<CancellationToken>,
    stream: Option<SubagentStreamSink>,
    hold: impl Send + 'static,
) -> Result<String> {
    tracing::info!(
        prompt_bytes = prompt.trim().len(),
        streaming = stream.is_some(),
        timeout = ?options.timeout,
        "Subagent started"
    );
    // Holds the prompt files until the child has exited.
    #[cfg_attr(not(unix), allow(unused_mut))]
    let (mut child, _prompt_files) = spawn_exec_subagent(root, prompt, options)
        .await?
        .into_parts();
    #[cfg(unix)]
    child.keep_until_exit(hold);
    #[cfg(not(unix))]
    let _hold = hold;

    if let Some(sink) = stream {
        return run_child_streaming(child, cancel, options.timeout, sink).await;
    }

    run_child_buffered(child, cancel, options.timeout).await
}

/// A spawned child `zdx exec` subagent, with the temp files it reads.
///
/// The prompt files are deleted when this is dropped, so it must live until
/// the child has exited.
pub struct SpawnedSubagent {
    child: ExecChild,
    prompt_files: PromptFiles,
}

impl SpawnedSubagent {
    /// Splits off the child, returning a guard that keeps its prompt files
    /// alive. Drop the guard only after the child has exited.
    #[must_use]
    pub fn into_parts(self) -> (ExecChild, impl Send + 'static) {
        (self.child, self.prompt_files)
    }
}

/// Temp files the child reads at startup.
struct PromptFiles {
    _prompt: TempPromptFile,
    _system_prompt: Option<TempPromptFile>,
}

/// Spawns a child `zdx exec` subagent with piped stdout/stderr.
///
/// On Unix the child runs under the shared lease supervisor, so closing the
/// lease (cancellation, dropping the handle, or this process dying) stops the
/// child and its process group.
///
/// # Errors
/// Returns an error if the prompt is empty or the child cannot be spawned.
pub async fn spawn_exec_subagent(
    root: &Path,
    prompt: &str,
    options: &ExecSubagentOptions,
) -> Result<SpawnedSubagent> {
    let prompt = prompt.trim();
    ensure!(!prompt.is_empty(), "Subagent prompt cannot be empty");

    let exe = std::env::current_exe().context("Failed to get executable path")?;
    let prompt_file = write_prompt_file(prompt)?;
    let effective_system_prompt_file = options
        .system_prompt
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(write_effective_system_prompt_file)
        .transpose()?;
    let args = build_exec_args(
        root,
        prompt_file.as_path(),
        options,
        effective_system_prompt_file
            .as_ref()
            .map(TempPromptFile::as_path),
    );

    #[cfg(unix)]
    let child = {
        let mut command = SupervisedCommand::new(exe);
        command.args(args).current_dir(root);
        for (key, value) in options.env.iter() {
            command.env(key, value);
        }
        command
            .spawn(Duration::from_secs(5))
            .await
            .map_err(|err| anyhow::anyhow!("Failed to spawn subagent: {err}"))?
    };

    #[cfg(not(unix))]
    let child = {
        let mut command = tokio::process::Command::new(exe);
        command
            .args(args)
            .current_dir(root)
            .envs(options.env.iter())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        command
            .spawn()
            .map_err(|err| anyhow::anyhow!("Failed to spawn subagent: {err}"))?
    };

    Ok(SpawnedSubagent {
        child,
        prompt_files: PromptFiles {
            _prompt: prompt_file,
            _system_prompt: effective_system_prompt_file,
        },
    })
}

/// How a supervised child run ended, before output processing.
#[cfg(not(unix))]
enum ChildWait {
    Exited(std::process::ExitStatus),
    Cancelled,
    TimedOut(Duration),
}

/// Waits for the child while honoring cancellation and timeout.
///
/// On cancellation or timeout the child's entire process group is terminated
/// (TERM, short grace, then KILL) and the direct child is reaped before this
/// returns, so no successor run can race a half-dead child appending to the
/// same worker thread JSONL.
#[cfg(unix)]
async fn wait_child(
    child: &mut ExecChild,
    cancel: Option<&CancellationToken>,
    timeout: Option<Duration>,
) -> Result<std::process::ExitStatus> {
    let outcome = child
        .wait_with(cancel, timeout)
        .await
        .context("Failed to wait for subagent")?;
    match outcome.reason {
        WaitReason::Exited => Ok(outcome.status),
        WaitReason::Cancelled => bail!("Subagent cancelled"),
        WaitReason::TimedOut => {
            let timeout = timeout.expect("timed out only when a timeout was configured");
            bail!("Subagent timed out after {} seconds", timeout.as_secs())
        }
    }
}

#[cfg(not(unix))]
async fn wait_child(
    child: &mut ExecChild,
    cancel: Option<&CancellationToken>,
    timeout: Option<Duration>,
) -> Result<std::process::ExitStatus> {
    let outcome = {
        let wait = child.wait();
        tokio::pin!(wait);
        match (cancel, timeout) {
            (Some(cancel), Some(timeout)) => {
                tokio::select! {
                    () = cancel.cancelled() => ChildWait::Cancelled,
                    result = tokio::time::timeout(timeout, &mut wait) => match result {
                        Ok(status) => ChildWait::Exited(status.context("Failed to wait for subagent")?),
                        Err(_) => ChildWait::TimedOut(timeout),
                    },
                }
            }
            (Some(cancel), None) => {
                tokio::select! {
                    () = cancel.cancelled() => ChildWait::Cancelled,
                    status = &mut wait => ChildWait::Exited(status.context("Failed to wait for subagent")?),
                }
            }
            (None, Some(timeout)) => match tokio::time::timeout(timeout, &mut wait).await {
                Ok(status) => ChildWait::Exited(status.context("Failed to wait for subagent")?),
                Err(_) => ChildWait::TimedOut(timeout),
            },
            (None, None) => ChildWait::Exited(wait.await.context("Failed to wait for subagent")?),
        }
    };

    match outcome {
        ChildWait::Exited(status) => Ok(status),
        ChildWait::Cancelled => {
            terminate_process(child).await;
            bail!("Subagent cancelled")
        }
        ChildWait::TimedOut(timeout) => {
            terminate_process(child).await;
            bail!("Subagent timed out after {} seconds", timeout.as_secs())
        }
    }
}

/// Terminates the child's process group and reaps the direct child.
///
/// TERM first, a short grace period, then KILL. A final group KILL sweeps any
/// stragglers that detached from the direct child but stayed in the group.
#[cfg(not(unix))]
async fn terminate_process(child: &mut tokio::process::Child) {
    const GRACE: Duration = Duration::from_secs(5);

    let Some(pid) = child.id() else {
        // Already exited; reap and return.
        let _ = child.wait().await;
        return;
    };
    if tokio::time::timeout(GRACE, child.wait()).await.is_err() {
        let _ = child.kill().await;
        let _ = child.wait().await;
    }
    tracing::debug!(pid, "Subagent process group terminated");
}

/// Buffered path: drains stdout/stderr concurrently, waits for the child with
/// cancellation/timeout supervision, then processes the captured output.
async fn run_child_buffered(
    mut child: ExecChild,
    cancel: Option<CancellationToken>,
    timeout: Option<Duration>,
) -> Result<String> {
    let stdout = child
        .stdout
        .take()
        .context("Subagent stdout was not piped")?;
    let stderr = child
        .stderr
        .take()
        .context("Subagent stderr was not piped")?;

    let stdout_handle = tokio::spawn(read_to_end(stdout));
    let stderr_handle = tokio::spawn(read_to_end(stderr));

    let status = wait_child(&mut child, cancel.as_ref(), timeout).await?;

    let stdout = stdout_handle.await.unwrap_or_default();
    let stderr = stderr_handle.await.unwrap_or_default();

    if status.success() {
        tracing::info!(stdout_bytes = stdout.len(), "Subagent finished");
    } else {
        log_nonzero_exit(status, &String::from_utf8_lossy(&stderr));
    }

    process_subagent_output(&std::process::Output {
        status,
        stdout,
        stderr,
    })
}

async fn read_to_end<R: AsyncRead + Unpin>(reader: R) -> Vec<u8> {
    let mut buf = Vec::new();
    let mut reader = BufReader::new(reader);
    let _ = reader.read_to_end(&mut buf).await;
    buf
}

/// Live sink for relaying a child subagent's tool activity to the parent.
///
/// Each child tool lifecycle event is re-emitted as an `AgentEvent::ToolOutputDelta`
/// on the parent's event stream, keyed by the parent `invoke_subagent` tool id.
/// The `chunk` is a compact JSON object (`{"t":"start"|"input"|"done"|"error", ...}`)
/// that the TUI parses into a child tool activity list.
pub struct SubagentStreamSink {
    pub sender: EventSender,
    pub parent_tool_id: String,
}

impl SubagentStreamSink {
    pub(crate) fn emit(&self, chunk: &Value) {
        self.sender.send(AgentEvent::ToolOutputDelta {
            id: self.parent_tool_id.clone(),
            chunk: chunk.to_string(),
        });
    }
}

/// How a child subagent's turn ended, from its `TurnFinished` event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TurnOutcome {
    /// The turn finished; carries the final answer.
    Completed(String),
    /// The turn was interrupted; carries the partial text so far.
    Interrupted(String),
    /// The turn failed; carries the error message.
    Failed(String),
}

/// Relayed chunk `t` for a steer the child added to its turn (`id`: the
/// steer's sequence number).
pub const STEER_CHUNK: &str = "steer";
/// Relayed chunk `t` for the child's closing steer marker.
pub const STEER_CLOSED_CHUNK: &str = "steer_closed";
/// Line a child run with `--steer-file` writes on stdout after every other
/// event, so the parent knows its steer reports are complete.
pub const STEER_CLOSED_LINE: &str = r#"{"type":"steer_closed"}"#;

/// One event a child subagent's stdout reports, in the order it arrives.
#[derive(Debug)]
pub enum ChildEvent<'a> {
    /// A tool lifecycle chunk (`{"t":"start"|"input"|"done"|"error", ...}`).
    Tool(&'a Value),
    /// The child added the steer with this id to its turn.
    SteerInjected(&'a str),
    /// The child wrote every steer report it will write.
    SteerClosed,
    /// The child's turn ended. Reported when the event arrives, which can be
    /// well before the child process exits (for example while it waits for
    /// its own background commands).
    TurnEnded(&'a TurnOutcome),
}

/// Result of draining a child subagent's stdout event stream.
#[derive(Debug, Default)]
pub struct StreamOutcome {
    /// How the child's turn ended, if it reported that before exiting.
    pub turn: Option<TurnOutcome>,
}

/// Streams a child `zdx exec` process: relays tool activity live via `sink`,
/// drains stdout/stderr concurrently to avoid pipe deadlocks, and returns the
/// final turn text (preserving the same completion/failure semantics as the
/// non-streaming path).
///
/// Cancellation and timeout supervise the child's actual exit via
/// [`wait_child`], not merely stdout EOF, so a child that closes stdout and
/// then hangs is still terminated and reaped.
async fn run_child_streaming(
    mut child: ExecChild,
    cancel: Option<CancellationToken>,
    timeout: Option<Duration>,
    sink: SubagentStreamSink,
) -> Result<String> {
    let stdout = child
        .stdout
        .take()
        .context("Subagent stdout was not piped")?;
    let stderr = child
        .stderr
        .take()
        .context("Subagent stderr was not piped")?;

    let stderr_handle = tokio::spawn(read_to_end(stderr));
    let stdout_handle = tokio::spawn(async move {
        let outcome = read_stdout_events(stdout, |event| match event {
            ChildEvent::Tool(chunk) => sink.emit(chunk),
            ChildEvent::SteerInjected(id) => {
                sink.emit(&serde_json::json!({ "t": STEER_CHUNK, "id": id }));
            }
            ChildEvent::SteerClosed => {
                sink.emit(&serde_json::json!({ "t": STEER_CLOSED_CHUNK }));
            }
            ChildEvent::TurnEnded(_) => {}
        })
        .await;
        drop(sink);
        outcome
    });

    // On cancel/timeout `wait_child` terminates the process group and reaps
    // the child before erroring; the detached reader tasks then finish at EOF.
    let status = wait_child(&mut child, cancel.as_ref(), timeout).await?;

    let outcome = stdout_handle
        .await
        .context("Subagent stdout reader task failed")??;
    let stderr_buf = stderr_handle.await.unwrap_or_default();

    if let Some(TurnOutcome::Failed(message)) = &outcome.turn {
        tracing::warn!(error = %zdx_types::log_field(message, 200), "Subagent turn failed");
        bail!("Subagent turn failed: {message}");
    }

    if !status.success() {
        let stderr = String::from_utf8_lossy(&stderr_buf);
        let stderr = stderr.trim();
        log_nonzero_exit(status, stderr);
        if stderr.is_empty() {
            let code = status.code().unwrap_or(-1);
            bail!("Subagent failed with exit code {code}");
        }
        bail!("Subagent failed: {stderr}");
    }

    // An interrupted turn still returns its partial text, as before.
    let final_text = match outcome.turn {
        Some(TurnOutcome::Completed(text) | TurnOutcome::Interrupted(text)) => Some(text),
        Some(TurnOutcome::Failed(_)) | None => None,
    };
    match final_text {
        Some(text) if !text.trim().is_empty() => {
            tracing::info!(output_bytes = text.len(), "Subagent finished");
            Ok(text)
        }
        _ => {
            tracing::warn!("Subagent returned empty output");
            bail!("Subagent returned empty output")
        }
    }
}

/// Reads JSONL agent events from a child's stdout until EOF, passing each
/// tool lifecycle chunk and the turn's end to `on_event` as they arrive, and
/// returning how the turn ended.
///
/// # Errors
/// Returns an error if reading the child's stdout fails.
pub async fn read_stdout_events<R: AsyncRead + Unpin>(
    reader: R,
    mut on_event: impl FnMut(ChildEvent<'_>),
) -> Result<StreamOutcome> {
    let mut lines = BufReader::new(reader).lines();
    let mut seen_ids: HashSet<String> = HashSet::new();
    let mut turn = None;

    while let Some(line) = lines
        .next_line()
        .await
        .context("Failed to read subagent stdout")?
    {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if trimmed == STEER_CLOSED_LINE {
            on_event(ChildEvent::SteerClosed);
            continue;
        }
        let Ok(event) = serde_json::from_str::<AgentEvent>(trimmed) else {
            continue;
        };
        match event {
            AgentEvent::ToolInputCompleted { id, name, input } => {
                let name = name.to_ascii_lowercase();
                if seen_ids.insert(id.clone()) {
                    on_event(ChildEvent::Tool(
                        &serde_json::json!({ "t": "start", "id": id, "name": name }),
                    ));
                }
                if let Some(arg) = extract_key_arg(&name, &input) {
                    on_event(ChildEvent::Tool(
                        &serde_json::json!({ "t": "input", "id": id, "arg": arg }),
                    ));
                }
            }
            AgentEvent::ToolStarted { id, name } if seen_ids.insert(id.clone()) => {
                let name = name.to_ascii_lowercase();
                on_event(ChildEvent::Tool(
                    &serde_json::json!({ "t": "start", "id": id, "name": name }),
                ));
            }
            AgentEvent::ToolCompleted { id, result, .. } => {
                let t = if result.is_ok() { "done" } else { "error" };
                on_event(ChildEvent::Tool(&serde_json::json!({ "t": t, "id": id })));
            }
            AgentEvent::UserMessageInjected { id, .. } => {
                on_event(ChildEvent::SteerInjected(&id));
            }
            AgentEvent::TurnFinished {
                status,
                final_text: text,
                ..
            } => {
                let outcome = match status {
                    TurnStatus::Completed => TurnOutcome::Completed(text),
                    TurnStatus::Interrupted => TurnOutcome::Interrupted(text),
                    TurnStatus::Failed { message, .. } => TurnOutcome::Failed(message),
                };
                on_event(ChildEvent::TurnEnded(&outcome));
                turn = Some(outcome);
            }
            _ => {}
        }
    }

    Ok(StreamOutcome { turn })
}

/// Extracts the most useful single argument to display for a child tool call.
///
/// Tool names are matched lowercase (the engine normalizes them). Reads only
/// the needed field so large `write`/`edit` inputs are not cloned wholesale.
fn extract_key_arg(tool_name: &str, input: &Value) -> Option<String> {
    let raw = zdx_types::tool_command_text(tool_name, input);
    let preview = tool_input_preview(&raw);
    (!preview.is_empty()).then_some(preview)
}

pub(crate) fn tool_input_preview(input: &str) -> String {
    const MAX_CHARS: usize = 200;
    let mut chars = input
        .split_whitespace()
        .enumerate()
        .flat_map(|(i, word)| (i > 0).then_some(' ').into_iter().chain(word.chars()));
    let mut preview: String = chars.by_ref().take(MAX_CHARS).collect();
    if chars.next().is_some() {
        preview.pop();
        preview.push('…');
    }
    preview
}

fn build_exec_args(
    root: &Path,
    prompt_file: &Path,
    options: &ExecSubagentOptions,
    effective_system_prompt_file: Option<&Path>,
) -> Vec<OsString> {
    let mut args = vec![OsString::from("--root"), root.as_os_str().to_os_string()];

    // Global thread-lineage flags (before the subcommand) so the persisted
    // child thread records its subagent/helper origin in its meta line.
    if let Some(id) = normalize_optional(options.thread_id.as_deref()) {
        args.push(OsString::from("--thread"));
        args.push(OsString::from(id));
    }
    if let Some(kind) = normalize_optional(options.thread_origin_kind.as_deref()) {
        args.push(OsString::from("--thread-origin-kind"));
        args.push(OsString::from(kind));
    }
    if let Some(parent) = normalize_optional(options.thread_parent_id.as_deref()) {
        args.push(OsString::from("--thread-parent-id"));
        args.push(OsString::from(parent));
    }
    if let Some(name) = normalize_optional(options.thread_subagent_name.as_deref()) {
        args.push(OsString::from("--thread-subagent-name"));
        args.push(OsString::from(name));
    }

    args.extend([
        OsString::from("exec"),
        OsString::from("--prompt-file"),
        prompt_file.as_os_str().to_os_string(),
    ]);

    if options.no_tools {
        args.push(OsString::from("--no-tools"));
    }

    if options.no_system_prompt {
        args.push(OsString::from("--no-system-prompt"));
    }

    if options.no_background_commands {
        args.push(OsString::from("--no-background-commands"));
    }

    if let Some(path) = &options.steer_file {
        args.push(OsString::from("--steer-file"));
        args.push(path.as_os_str().to_os_string());
    }

    if let Some(tools) = options
        .tools_override
        .as_ref()
        .filter(|tools| !tools.is_empty())
    {
        args.push(OsString::from("--tools"));
        args.push(OsString::from(tools.join(",")));
    }

    if let Some(filters) = options
        .event_filter
        .as_ref()
        .filter(|filters| !filters.is_empty())
    {
        args.push(OsString::from("--filter"));
        args.push(OsString::from(filters.join(",")));
    }

    if let Some(model) = normalize_optional(options.model.as_deref()) {
        let model = options.thinking_level.map_or_else(
            || model.to_string(),
            |level| crate::models::format_model_thinking(model, level),
        );
        args.push(OsString::from("-m"));
        args.push(OsString::from(model));
    }

    if let Some(system_prompt_file) = effective_system_prompt_file {
        args.push(OsString::from("--effective-system-prompt-file"));
        args.push(system_prompt_file.as_os_str().to_os_string());
    }

    if let Some(kind) = normalize_optional(options.activity_kind.as_deref()) {
        args.push(OsString::from("--activity-kind"));
        args.push(OsString::from(kind));
    }
    if let Some(parent) = normalize_optional(options.activity_parent_thread_id.as_deref()) {
        args.push(OsString::from("--activity-parent-thread-id"));
        args.push(OsString::from(parent));
    }
    if let Some(name) = normalize_optional(options.activity_subagent_name.as_deref()) {
        args.push(OsString::from("--activity-subagent-name"));
        args.push(OsString::from(name));
    }

    args
}

fn normalize_optional(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|s| !s.is_empty())
}

fn write_prompt_file(prompt: &str) -> Result<TempPromptFile> {
    write_temp_text_file("zdx-subagent-prompt", prompt)
}

fn write_effective_system_prompt_file(system_prompt: &str) -> Result<TempPromptFile> {
    write_temp_text_file("zdx-effective-system-prompt", system_prompt)
}

fn write_temp_text_file(prefix: &str, contents: &str) -> Result<TempPromptFile> {
    let mut file = Builder::new()
        .prefix(prefix)
        .suffix(".md")
        .tempfile()
        .context("create temp text file")?;
    file.write_all(contents.as_bytes())
        .with_context(|| format!("write temp text file {}", file.path().display()))?;
    file.flush()
        .with_context(|| format!("flush temp text file {}", file.path().display()))?;
    let path = file.into_temp_path();
    Ok(TempPromptFile { path })
}

fn process_subagent_output(output: &std::process::Output) -> Result<String> {
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stderr = stderr.trim();
        if stderr.is_empty() {
            let code = output.status.code().unwrap_or(-1);
            bail!("Subagent failed with exit code {code}");
        }
        bail!("Subagent failed: {stderr}");
    }

    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    ensure!(!stdout.is_empty(), "Subagent returned empty output");

    if let Some(final_text) = extract_turn_finished_text(&stdout)? {
        return Ok(final_text);
    }

    Ok(stdout)
}

fn extract_turn_finished_text(stdout: &str) -> Result<Option<String>> {
    let mut saw_json_event = false;
    let mut final_text = None;

    for line in stdout
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
    {
        match serde_json::from_str::<AgentEvent>(line) {
            Ok(AgentEvent::TurnFinished {
                status,
                final_text: text,
                ..
            }) => {
                saw_json_event = true;
                match status {
                    crate::core::events::TurnStatus::Completed
                    | crate::core::events::TurnStatus::Interrupted => {
                        final_text = Some(text);
                    }
                    crate::core::events::TurnStatus::Failed { message, .. } => {
                        bail!("Subagent turn failed: {message}");
                    }
                }
            }
            Ok(_) => {
                saw_json_event = true;
            }
            Err(_) => {
                if saw_json_event {
                    bail!("Subagent produced malformed JSONL output");
                }
                return Ok(None);
            }
        }
    }

    if saw_json_event {
        return final_text
            .filter(|text| !text.trim().is_empty())
            .map(Some)
            .ok_or_else(|| {
                anyhow::anyhow!("Subagent JSONL output missing turn_finished.final_text")
            });
    }

    Ok(None)
}

#[cfg(test)]
mod tests {
    use std::os::unix::process::ExitStatusExt;
    use std::path::Path;

    use super::*;

    #[test]
    fn build_exec_args_includes_required_flags() {
        let args = build_exec_args(
            Path::new("/tmp/project"),
            Path::new("/tmp/subagent-prompt.md"),
            &ExecSubagentOptions::default(),
            None,
        );
        let args: Vec<String> = args
            .iter()
            .map(|s| s.to_string_lossy().to_string())
            .collect();

        assert_eq!(
            args,
            vec![
                "--root",
                "/tmp/project",
                "exec",
                "--prompt-file",
                "/tmp/subagent-prompt.md"
            ]
        );
    }

    #[test]
    fn build_exec_args_includes_optional_flags() {
        let prompt_file = Path::new("/tmp/subagent-prompt.md");
        let system_prompt_file = Path::new("/tmp/effective-system-prompt.md");
        let args = build_exec_args(
            Path::new("/tmp/project"),
            prompt_file,
            &ExecSubagentOptions {
                model: Some("openai:gpt-5.2".to_string()),
                system_prompt: Some("You are a focused assistant".to_string()),
                thinking_level: Some(crate::config::ThinkingLevel::Low),
                no_tools: false,
                no_system_prompt: true,
                no_background_commands: true,
                tools_override: Some(vec!["read".to_string(), "glob".to_string()]),
                event_filter: Some(vec!["turn_finished".to_string()]),
                timeout: None,
                activity_kind: None,
                activity_parent_thread_id: None,
                activity_subagent_name: None,
                thread_origin_kind: None,
                thread_parent_id: None,
                thread_subagent_name: None,
                thread_id: None,
                steer_file: None,
                env: zdx_tools::ToolEnv::default(),
            },
            Some(system_prompt_file),
        );
        let args: Vec<String> = args
            .iter()
            .map(|s| s.to_string_lossy().to_string())
            .collect();

        assert_eq!(
            args,
            vec![
                "--root",
                "/tmp/project",
                "exec",
                "--prompt-file",
                "/tmp/subagent-prompt.md",
                "--no-system-prompt",
                "--no-background-commands",
                "--tools",
                "read,glob",
                "--filter",
                "turn_finished",
                "-m",
                "openai:gpt-5.2@low",
                "--effective-system-prompt-file",
                "/tmp/effective-system-prompt.md"
            ]
        );
    }

    #[test]
    fn build_exec_args_emits_thread_lineage_flags() {
        let args = build_exec_args(
            Path::new("/tmp/project"),
            Path::new("/tmp/subagent-prompt.md"),
            &ExecSubagentOptions {
                thread_origin_kind: Some("subagent".to_string()),
                thread_parent_id: Some("thread-parent".to_string()),
                thread_subagent_name: Some("explorer".to_string()),
                ..Default::default()
            },
            None,
        );
        let args: Vec<String> = args
            .iter()
            .map(|s| s.to_string_lossy().to_string())
            .collect();

        assert_eq!(
            args,
            vec![
                "--root",
                "/tmp/project",
                "--thread-origin-kind",
                "subagent",
                "--thread-parent-id",
                "thread-parent",
                "--thread-subagent-name",
                "explorer",
                "exec",
                "--prompt-file",
                "/tmp/subagent-prompt.md"
            ]
        );
    }

    #[test]
    fn build_exec_args_emits_explicit_thread_id_before_subcommand() {
        let args = build_exec_args(
            Path::new("/tmp/project"),
            Path::new("/tmp/subagent-prompt.md"),
            &ExecSubagentOptions {
                thread_id: Some("worker-thread-1".to_string()),
                ..Default::default()
            },
            None,
        );
        let args: Vec<String> = args
            .iter()
            .map(|s| s.to_string_lossy().to_string())
            .collect();

        assert_eq!(
            args,
            vec![
                "--root",
                "/tmp/project",
                "--thread",
                "worker-thread-1",
                "exec",
                "--prompt-file",
                "/tmp/subagent-prompt.md"
            ]
        );
    }

    #[test]
    fn build_exec_args_propagates_activity_metadata_when_tracking() {
        let args = build_exec_args(
            Path::new("/tmp/project"),
            Path::new("/tmp/subagent-prompt.md"),
            &ExecSubagentOptions {
                activity_kind: Some("subagent".to_string()),
                activity_parent_thread_id: Some("thread-parent".to_string()),
                activity_subagent_name: Some("explorer".to_string()),
                ..Default::default()
            },
            None,
        );
        let args: Vec<String> = args
            .iter()
            .map(|s| s.to_string_lossy().to_string())
            .collect();

        assert_eq!(
            args,
            vec![
                "--root",
                "/tmp/project",
                "exec",
                "--prompt-file",
                "/tmp/subagent-prompt.md",
                "--activity-kind",
                "subagent",
                "--activity-parent-thread-id",
                "thread-parent",
                "--activity-subagent-name",
                "explorer"
            ]
        );
    }

    #[test]
    fn process_subagent_output_extracts_turn_finished_text() {
        let terminal = serde_json::to_string(&AgentEvent::TurnFinished {
            status: crate::core::events::TurnStatus::Completed,
            final_text: "final answer".to_string(),
            messages: Vec::new(),
            prior_message_count: 0,
        })
        .unwrap();
        let output = std::process::Output {
            status: std::process::ExitStatus::from_raw(0),
            stdout: format!(
                "{{\"type\":\"usage_update\",\"input_tokens\":1,\"output_tokens\":2,\"cache_read_input_tokens\":0,\"cache_creation_input_tokens\":0}}\n{{\"type\":\"assistant_completed\",\"text\":\"partial\"}}\n{terminal}\n"
            )
            .into_bytes(),
            stderr: Vec::new(),
        };

        let text = process_subagent_output(&output).expect("should parse");
        assert_eq!(text, "final answer");
    }

    #[test]
    fn process_subagent_output_falls_back_to_plain_text() {
        let output = std::process::Output {
            status: std::process::ExitStatus::from_raw(0),
            stdout: b"plain text output\n".to_vec(),
            stderr: Vec::new(),
        };

        let text = process_subagent_output(&output).expect("should keep plain text");
        assert_eq!(text, "plain text output");
    }

    #[test]
    fn temp_prompt_file_is_removed_on_drop() {
        let file = write_effective_system_prompt_file("prompt body").unwrap();
        let path = file.as_path().to_path_buf();
        assert!(path.exists());

        drop(file);

        assert!(!path.exists());
    }

    #[test]
    fn prompt_file_is_removed_on_drop() {
        let file = write_prompt_file("prompt body").unwrap();
        let path = file.as_path().to_path_buf();
        assert!(path.exists());

        drop(file);

        assert!(!path.exists());
    }

    #[test]
    fn extract_key_arg_reads_expected_field_per_tool() {
        use serde_json::json;

        assert_eq!(
            extract_key_arg("read", &json!({ "file_path": "Cargo.toml" })).as_deref(),
            Some("Cargo.toml")
        );
        assert_eq!(
            extract_key_arg("read", &json!({ "path": "Cargo.toml" })).as_deref(),
            Some("Cargo.toml")
        );
        assert_eq!(
            extract_key_arg("write", &json!({ "file_path": "src/lib.rs" })).as_deref(),
            Some("src/lib.rs")
        );
        assert_eq!(
            extract_key_arg("glob", &json!({ "pattern": "**/*.rs" })).as_deref(),
            Some("**/*.rs")
        );
        assert_eq!(
            extract_key_arg("grep", &json!({ "pattern": "TODO" })).as_deref(),
            Some("TODO")
        );
        assert_eq!(
            extract_key_arg("grep", &json!({ "pattern": "TODO", "path": "src" })).as_deref(),
            Some("TODO src")
        );
        assert_eq!(
            extract_key_arg("fetch_webpage", &json!({ "url": "https://example.com" })).as_deref(),
            Some("https://example.com")
        );
        assert_eq!(
            extract_key_arg(
                "web_search",
                &json!({ "search_queries": ["rust async", "tokio"] })
            )
            .as_deref(),
            Some("rust async tokio")
        );
        assert_eq!(
            extract_key_arg(
                "invoke_subagent",
                &json!({ "subagent": "explorer", "prompt": "inspect the build" })
            )
            .as_deref(),
            Some("inspect the build")
        );
        assert_eq!(
            extract_key_arg("invoke_subagent", &json!({ "prompt": "go" })).as_deref(),
            Some("go")
        );
        // Unknown tools have no key arg.
        assert_eq!(extract_key_arg("todo_write", &json!({ "todos": [] })), None);
    }

    #[test]
    fn tool_input_preview_is_bounded_and_single_line() {
        let short = "cargo build";
        assert_eq!(tool_input_preview(short), short);
        assert_eq!(
            tool_input_preview(" \t./gradlew\n  assembleDebug  "),
            "./gradlew assembleDebug"
        );

        let long = "é".repeat(10000);
        let truncated = tool_input_preview(&long);
        assert!(truncated.ends_with('…'));
        assert_eq!(truncated.chars().count(), 200);
        assert_eq!(tool_input_preview(&truncated), truncated);
        for (name, input) in [
            ("bash", serde_json::json!({ "command": long })),
            ("invoke_subagent", serde_json::json!({ "prompt": long })),
            ("grep", serde_json::json!({ "pattern": long })),
        ] {
            assert_eq!(extract_key_arg(name, &input).unwrap().chars().count(), 200);
        }
    }

    fn make_sink() -> (SubagentStreamSink, crate::core::agent::AgentEventRx) {
        let (tx, rx) = crate::core::agent::create_event_channel();
        let sink = SubagentStreamSink {
            sender: EventSender::new(tx),
            parent_tool_id: "parent-tool".to_string(),
        };
        (sink, rx)
    }

    fn jsonl(events: &[AgentEvent]) -> String {
        events
            .iter()
            .map(|event| serde_json::to_string(event).unwrap())
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[tokio::test]
    async fn read_stdout_events_relays_child_tools_and_final_text() {
        let (sink, mut rx) = make_sink();
        // Engine order: ToolInputCompleted (carries the arg) before ToolStarted.
        let events = vec![
            AgentEvent::ToolInputCompleted {
                id: "tu1".to_string(),
                name: "Read".to_string(),
                input: serde_json::json!({ "file_path": "Cargo.toml" }),
            },
            AgentEvent::ToolStarted {
                id: "tu1".to_string(),
                name: "Read".to_string(),
            },
            AgentEvent::ToolCompleted {
                id: "tu1".to_string(),
                result: crate::core::events::ToolOutput::success(serde_json::json!("ok")),
                duration_ms: Some(1),
                started_at: None,
                completed_at: None,
            },
            AgentEvent::TurnFinished {
                status: crate::core::events::TurnStatus::Completed,
                final_text: "all done".to_string(),
                messages: Vec::new(),
                prior_message_count: 0,
            },
        ];
        let bytes = jsonl(&events);

        let outcome = read_stdout_events(bytes.as_bytes(), |event| {
            if let ChildEvent::Tool(chunk) = event {
                sink.emit(chunk);
            }
        })
        .await
        .unwrap();
        assert_eq!(
            outcome.turn,
            Some(TurnOutcome::Completed("all done".to_string()))
        );

        drop(sink);
        let mut kinds = Vec::new();
        while let Some(event) = rx.recv().await {
            if let AgentEvent::ToolOutputDelta { id, chunk } = event.as_ref() {
                assert_eq!(id, "parent-tool");
                let value: Value = serde_json::from_str(chunk).unwrap();
                assert_eq!(value["id"], "tu1");
                if value["t"] == "input" {
                    assert_eq!(value["arg"], "Cargo.toml");
                }
                kinds.push(value["t"].as_str().unwrap().to_string());
            }
        }
        // start + input from ToolInputCompleted (ToolStarted deduped), then done.
        assert_eq!(kinds, vec!["start", "input", "done"]);
    }

    #[tokio::test]
    async fn read_stdout_events_reports_turn_failure() {
        let (sink, _rx) = make_sink();
        let events = vec![AgentEvent::TurnFinished {
            status: crate::core::events::TurnStatus::Failed {
                kind: crate::core::events::ErrorKind::Internal,
                message: "boom".to_string(),
                details: None,
            },
            final_text: String::new(),
            messages: Vec::new(),
            prior_message_count: 0,
        }];
        let bytes = jsonl(&events);

        let outcome = read_stdout_events(bytes.as_bytes(), |event| {
            if let ChildEvent::Tool(chunk) = event {
                sink.emit(chunk);
            }
        })
        .await
        .unwrap();
        assert_eq!(outcome.turn, Some(TurnOutcome::Failed("boom".to_string())));
    }

    /// A worker child's steer reports and closing marker reach the parent in
    /// stream order, so a report always precedes the marker.
    #[tokio::test]
    async fn read_stdout_events_relays_steer_reports_then_the_closing_marker() {
        let mut bytes = jsonl(&[AgentEvent::UserMessageInjected {
            id: "7".to_string(),
            source: "orchestrator".to_string(),
            text: "detail".to_string(),
        }]);
        bytes.push('\n');
        bytes.push_str(STEER_CLOSED_LINE);
        let mut seen = Vec::new();
        read_stdout_events(bytes.as_bytes(), |event| match event {
            ChildEvent::SteerInjected(id) => seen.push(format!("steer:{id}")),
            ChildEvent::SteerClosed => seen.push("closed".to_string()),
            ChildEvent::Tool(_) | ChildEvent::TurnEnded(_) => {}
        })
        .await
        .unwrap();
        assert_eq!(seen, ["steer:7", "closed"]);
    }
}
