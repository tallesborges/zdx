//! Bash tool for executing shell commands.
//!
//! Allows the agent to run shell commands with safety guards.
//! Requires `--allow-bash` flag or the tool returns "denied".

use std::fs::File;
use std::io::Write;
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::Deserialize;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncRead};
use uuid::Uuid;

use super::{ToolContext, ToolDefinition, ToolOutput};
#[cfg(unix)]
use crate::process_supervisor::{BoundedWait, SupervisedChild, SupervisedCommand, WaitReason};

/// Maximum bytes per output stream (stdout/stderr) before truncation.
const MAX_OUTPUT_BYTES: usize = 40 * 1024; // 40KB

/// Grace period to wait for reader tasks to drain after the child exits.
///
/// Normally pipes close immediately once the child exits, but if a descendant
/// process inherited stdout/stderr and escaped the process group (e.g. via
/// `setsid`), it can keep the pipes open. After this grace expires, readers
/// are aborted so `bash_handler` isn't held hostage by orphan processes.
const READER_GRACE: Duration = Duration::from_millis(500);

/// Writes full output to a temp file and returns the file path.
///
/// Used when output is truncated so the AI can use the Read tool to access
/// the complete data with offset/limit parameters.
fn write_temp_file(bytes: &[u8], stream_name: &str) -> Option<String> {
    let temp_dir = std::env::temp_dir();
    let filename = format!("zdx-bash-{}-{}.txt", Uuid::new_v4(), stream_name);
    let path = temp_dir.join(filename);

    let mut file = File::create(&path).ok()?;
    file.write_all(bytes).ok()?;

    Some(path.to_string_lossy().into_owned())
}

/// Returns the tool definition for the bash tool.
pub fn definition() -> ToolDefinition {
    ToolDefinition {
        name: "Bash".to_string(),
        description: "Run shell and CLI workflows whose capability no dedicated tool provides: builds, tests, version control, package managers, other CLIs, and work on those commands' own output. Reading, discovering, and searching files is covered by the dedicated tools, which return structured, ignore-aware, paginated results; scoping or limiting such a result is part of that same capability. A long-running foreground command is never killed for being slow: one still running after the foreground bound is moved to the background and the result comes back with backgrounded: true and a bg_id to poll with background_output instead of an exit code. Long-lived commands use background mode, and truncated output can be inspected with Read."
            .to_string(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "command": {
                    "type": "string",
                    "description": "The shell command to execute"
                },
                "timeout_secs": {
                    "type": "integer",
                    "minimum": 0,
                    "description": "Optional foreground wait budget in seconds, for a command you expect to outrun the default bound. This is NOT a kill switch: when it elapses the command is normally moved to the background and keeps running, exactly as it would be at the default bound. Where backgrounding is unavailable the wait simply continues instead; either way this value never terminates the command. Omit it to use the default; 0 means the same. To stop a running command use background_kill. For a command that never exits, use background: true instead."
                },
                "background": {
                    "type": "boolean",
                    "description": "Run as a detached background process that OUTLIVES this turn (e.g. a dev server, watcher). Returns immediately with a bg_id instead of blocking. Output goes to log files; read it with background_output and stop it with background_kill. Do NOT use shell backgrounding (trailing &, nohup, setsid) for long-lived processes — use this flag instead."
                }
            },
            "required": ["command"],
            "additionalProperties": false
        }),
    }
}

#[derive(Debug, Deserialize)]
struct BashInput {
    command: String,
    #[serde(
        default,
        deserialize_with = "crate::u64_or_string::deserialize_optional"
    )]
    timeout_secs: Option<u64>,
}

/// Applies the caller's `timeout_secs` to the handoff as a foreground wait bound.
///
/// `timeout_secs` is not a deadline: it says how long to wait before relocating
/// the command, never whether to kill it. Omitted and `0` keep the configured
/// bound; a positive value replaces it.
///
/// With no handoff there is no bound to override and nothing to relocate into,
/// so the command simply keeps waiting in the foreground. A caller's wait
/// budget must never degrade into a kill.
#[cfg(unix)]
fn apply_wait_bound(handoff: Option<Handoff>, requested_secs: Option<u64>) -> Option<Handoff> {
    let mut handoff = handoff?;
    if let Some(secs) = requested_secs.filter(|secs| *secs > 0) {
        handoff.bound = Duration::from_secs(secs);
    }
    Some(handoff)
}

/// Output from a bash command execution.
#[derive(Debug)]
pub struct BashOutput {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: i32,
    pub timed_out: bool,
    pub stdout_truncated: bool,
    pub stderr_truncated: bool,
    pub stdout_total_bytes: usize,
    pub stderr_total_bytes: usize,
    /// Path to temp file containing full stdout (when truncated).
    pub stdout_file: Option<String>,
    /// Path to temp file containing full stderr (when truncated).
    pub stderr_file: Option<String>,
}

/// Auto-background handoff parameters for a foreground command.
///
/// Supplied by the engine, which owns the background registry, its log paths,
/// and the marker lifecycle. When `bound` elapses with the command still
/// running, the command is **moved to the background, never killed**.
#[cfg(unix)]
pub struct Handoff {
    /// How long the command may run in the foreground before being handed off.
    /// This is a relocation bound, not a deadline.
    pub bound: Duration,
    /// Background id the adopted job is registered under.
    pub bg_id: String,
    /// Append-mode log the command's stdout continues into after handoff.
    pub stdout_log: std::path::PathBuf,
    /// Append-mode log the command's stderr continues into after handoff.
    pub stderr_log: std::path::PathBuf,
    /// Registers the now-background job. Returns `false` when bookkeeping
    /// failed, which is reported as `tracking_failed` and never kills the job.
    pub on_adopt: Box<dyn FnOnce(crate::process_supervisor::TargetIdentity) -> bool + Send>,
    /// Records the job's exit once it ends.
    pub on_exit: crate::adopted::OnExit,
}

/// What a handoff could not set up. The job keeps running regardless — losing
/// bookkeeping is never a reason to kill work the user asked for.
#[cfg(unix)]
#[derive(Debug, Clone, Copy, Default)]
pub struct HandoffDegradation {
    /// The registry marker could not be written, so `background_output` and
    /// `background_kill` cannot find the job by id.
    pub registry_failed: bool,
    /// The output logs could not be opened, so anything the command writes
    /// after the handoff is lost.
    pub logging_failed: bool,
}

#[cfg(unix)]
impl HandoffDegradation {
    /// Whether the job is less than fully manageable.
    #[must_use]
    pub fn any(self) -> bool {
        self.registry_failed || self.logging_failed
    }
}

/// Result of a command that outran its foreground bound and was moved to the
/// background. The command is still running.
#[cfg(unix)]
#[derive(Debug)]
pub struct BackgroundedOutput {
    pub bg_id: String,
    pub pid: u32,
    pub pgid: i32,
    pub elapsed_secs: u64,
    pub stdout: String,
    pub stderr: String,
    pub stdout_truncated: bool,
    pub stderr_truncated: bool,
    pub stdout_total_bytes: usize,
    pub stderr_total_bytes: usize,
    pub stdout_file: Option<String>,
    pub stderr_file: Option<String>,
    pub stdout_log: String,
    pub stderr_log: String,
    pub degraded: HandoffDegradation,
}

/// How a foreground command ended.
pub enum CommandOutcome {
    /// The command reached a terminal state (exit, timeout kill, …).
    Completed(BashOutput),
    /// The command outran its foreground bound and is still running in the
    /// background.
    #[cfg(unix)]
    Backgrounded(Box<BackgroundedOutput>),
}

impl CommandOutcome {
    #[must_use]
    pub fn into_tool_output(self) -> ToolOutput {
        match self {
            Self::Completed(output) => output.into_tool_output(),
            #[cfg(unix)]
            Self::Backgrounded(output) => output.into_tool_output(),
        }
    }
}

#[cfg(unix)]
impl BackgroundedOutput {
    fn message(&self) -> String {
        use std::fmt::Write as _;

        let mut message = format!(
            "Command did not complete within its {}s foreground bound and was moved to the \
             background ({}). It is still running — do NOT re-run it. Read its output with \
             background_output (bg_id \"{}\"), passing wait_secs to wait for it rather than \
             reading it repeatedly, and stop it with background_kill. Status \"running\" with no \
             new output does NOT mean it is stuck or finished.",
            self.elapsed_secs, self.bg_id, self.bg_id
        );
        if self.degraded.registry_failed {
            let _ = write!(
                message,
                " Note: this job could not be recorded in the background registry, so \
                 background_output and background_kill will not find it; it is running as pid {}.",
                self.pid
            );
        }
        if self.degraded.logging_failed {
            message.push_str(
                " Note: its output logs could not be opened, so anything it writes from now on \
                 is not captured — the output above is all you will get.",
            );
        }
        message
    }

    /// Converts to structured envelope format.
    #[must_use]
    pub fn into_tool_output(self) -> ToolOutput {
        let message = self.message();
        let degraded = self.degraded;
        let mut data = json!({
            "backgrounded": true,
            "bg_id": self.bg_id,
            "pid": self.pid,
            "pgid": self.pgid,
            "status": "running",
            "exit_code": Value::Null,
            "timed_out": false,
            "elapsed_secs": self.elapsed_secs,
            "stdout": self.stdout,
            "stderr": self.stderr,
            "stdout_truncated": self.stdout_truncated,
            "stderr_truncated": self.stderr_truncated,
            "stdout_total_bytes": self.stdout_total_bytes,
            "stderr_total_bytes": self.stderr_total_bytes,
            "stdout_log": self.stdout_log,
            "stderr_log": self.stderr_log,
            "message": message,
        });
        if degraded.any() {
            data["tracking_failed"] = json!(true);
        }
        if degraded.logging_failed {
            data["logging_failed"] = json!(true);
        }
        if let Some(path) = self.stdout_file {
            data["stdout_file"] = json!(path);
        }
        if let Some(path) = self.stderr_file {
            data["stderr_file"] = json!(path);
        }
        ToolOutput::success(data)
    }
}

impl BashOutput {
    /// Converts to structured envelope format.
    pub fn into_tool_output(self) -> ToolOutput {
        let mut data = json!({
            "stdout": self.stdout,
            "stderr": self.stderr,
            "exit_code": self.exit_code,
            "timed_out": self.timed_out,
            "stdout_truncated": self.stdout_truncated,
            "stderr_truncated": self.stderr_truncated,
            "stdout_total_bytes": self.stdout_total_bytes,
            "stderr_total_bytes": self.stderr_total_bytes
        });

        // Add file paths when truncated (for AI to use Read tool)
        if let Some(path) = self.stdout_file {
            data["stdout_file"] = json!(path);
        }
        if let Some(path) = self.stderr_file {
            data["stderr_file"] = json!(path);
        }

        ToolOutput::success(data)
    }
}

/// Executes the bash tool and returns a structured envelope.
///
/// If `output_tx` is provided, stdout/stderr lines are streamed through the
/// channel as they arrive. Ownership is taken so the channel closes when this
/// function completes.
///
/// `handoff` enables auto-backgrounding: when the command outruns its
/// foreground bound it is moved to the background instead of being killed, and
/// the returned envelope carries `backgrounded: true` plus a `bg_id`. The
/// caller's `timeout_secs` sets that bound and is never destructive.
///
/// `timeout` is the run's configured tool deadline (an automation's
/// `timeout_secs` frontmatter). It is the only destructive one, it is not
/// reachable from the tool's arguments, and it suppresses the handoff so an
/// operator's deadline still kills.
pub async fn execute(
    input: &Value,
    ctx: &ToolContext,
    timeout: Option<Duration>,
    output_tx: Option<tokio::sync::mpsc::UnboundedSender<String>>,
    #[cfg(unix)] handoff: Option<Handoff>,
) -> ToolOutput {
    let input: BashInput = match serde_json::from_value(input.clone()) {
        Ok(i) => i,
        Err(e) => {
            return ToolOutput::failure(
                "invalid_input",
                format!("Invalid input for bash tool: {e}"),
                None,
            );
        }
    };

    if input.command.trim().is_empty() {
        return ToolOutput::failure("invalid_input", "command cannot be empty", None);
    }

    // Only the run's configured deadline kills. The caller's `timeout_secs`
    // bounds the foreground wait instead, so a slow command is relocated with
    // its work intact rather than destroyed.
    #[cfg(unix)]
    let handoff = if timeout.is_some() {
        None
    } else {
        apply_wait_bound(handoff, input.timeout_secs)
    };

    match run_command(
        &input.command,
        ctx,
        timeout,
        output_tx,
        #[cfg(unix)]
        handoff,
    )
    .await
    {
        Ok(outcome) => outcome.into_tool_output(),
        Err(e) => e,
    }
}

/// Executes a bash command directly (convenience wrapper).
///
/// This is a simpler API that takes the command string directly,
/// useful for direct user invocation (e.g., `$` shortcut). Such commands are
/// user-driven and are never auto-backgrounded.
///
/// If `output_tx` is provided, stdout/stderr lines are streamed through the
/// channel as they arrive.
pub async fn run(
    command: &str,
    ctx: &ToolContext,
    timeout: Option<Duration>,
    output_tx: Option<tokio::sync::mpsc::UnboundedSender<String>>,
) -> ToolOutput {
    if command.trim().is_empty() {
        return ToolOutput::failure("invalid_input", "command cannot be empty", None);
    }

    match run_command(
        command,
        ctx,
        timeout,
        output_tx,
        #[cfg(unix)]
        None,
    )
    .await
    {
        Ok(outcome) => outcome.into_tool_output(),
        Err(e) => e,
    }
}

/// Handle to a spawned background process.
///
/// The caller owns `child` and is responsible for `wait()`ing it (to reap the
/// zombie) and recording its exit. The process is detached in its own session
/// and is NOT killed when this handle is dropped.
#[cfg(unix)]
pub struct BackgroundSpawn {
    pub child: tokio::process::Child,
    pub pid: u32,
}

/// Spawns `command` as a detached background process in its own session
/// (`setsid`), with stdout/stderr redirected to the given log files and stdin
/// nulled. Does NOT wait for or kill the process — it survives this call and
/// the zdx process. Unix-only.
///
/// # Errors
/// Returns an error if the log files cannot be opened or the process cannot be
/// spawned.
#[cfg(unix)]
pub fn spawn_background(
    command: &str,
    cwd: &std::path::Path,
    stdout_log: &std::path::Path,
    stderr_log: &std::path::Path,
) -> std::io::Result<BackgroundSpawn> {
    use std::os::unix::fs::OpenOptionsExt;

    let open_log = |path: &std::path::Path| {
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .mode(0o600)
            .open(path)
    };
    let out = open_log(stdout_log)?;
    let err = open_log(stderr_log)?;

    let mut cmd = tokio::process::Command::new("sh");
    cmd.arg("-c")
        .arg(command)
        .current_dir(cwd)
        .env("TERM", "dumb")
        .env("NO_COLOR", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::from(out))
        .stderr(Stdio::from(err))
        // The process outlives this handle; never kill on drop.
        .kill_on_drop(false);

    // New session + process group; detaches from the controlling terminal so a
    // hangup doesn't SIGHUP the process, and gives it a stable pgid to kill.
    unsafe {
        cmd.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }

    let child = cmd.spawn()?;
    // A freshly spawned child always has a PID (it's only cleared after wait).
    // Falling back to 0 would mean "current process group" in later killpg — a
    // foot-gun — so surface the impossible case as an error instead.
    let pid = child
        .id()
        .ok_or_else(|| std::io::Error::other("spawned background process has no PID"))?;
    Ok(BackgroundSpawn { child, pid })
}

/// Where one stream's lines go.
///
/// During the foreground wait a sink accumulates an in-memory tail and forwards
/// each line to the engine's delta channel. An auto-background handoff flips it
/// to append-mode log-file capture and stops the delta stream, so no output
/// event can arrive after the tool call has completed.
#[derive(Default)]
struct SinkState {
    buf: Vec<u8>,
    tx: Option<tokio::sync::mpsc::UnboundedSender<String>>,
    log: Option<File>,
    handed_off: bool,
}

/// Shared handle to one stream's sink.
type StreamSink = Arc<Mutex<SinkState>>;

fn new_sink(tx: Option<tokio::sync::mpsc::UnboundedSender<String>>) -> StreamSink {
    Arc::new(Mutex::new(SinkState {
        tx,
        ..SinkState::default()
    }))
}

fn push_line(sink: &StreamSink, line: &str) {
    let Ok(mut state) = sink.lock() else { return };
    if state.handed_off {
        // After handoff the in-memory tail is frozen and lines go to the log.
        // If no log could be opened they are dropped rather than growing
        // unbounded for a job nobody is waiting on; the handoff result reports
        // that as `logging_failed` so the loss is never silent.
        if let Some(log) = state.log.as_mut() {
            let _ = log.write_all(line.as_bytes());
        }
        return;
    }
    if let Some(tx) = state.tx.as_ref() {
        let _ = tx.send(line.to_string());
    }
    state.buf.extend_from_slice(line.as_bytes());
}

/// Flips a sink to log-file capture and returns everything captured so far,
/// plus whether the log is actually receiving output.
///
/// The pre-handoff bytes are flushed into the log first, so the log holds the
/// complete stream, and are also returned for the tool result so partial output
/// survives the handoff.
///
/// A `log` of `None` means the file could not be opened. The handoff still
/// proceeds — the job keeps running either way — but everything the command
/// writes from here on is unreadable, so the caller must report that rather
/// than leaving a job that looks healthy behind an empty log.
#[cfg(unix)]
fn hand_off_sink(sink: &StreamSink, log: Option<File>) -> (Vec<u8>, bool) {
    let Ok(mut state) = sink.lock() else {
        return (Vec::new(), false);
    };
    let captured = std::mem::take(&mut state.buf);
    let mut logging = false;
    if let Some(mut log) = log {
        logging = log.write_all(&captured).and_then(|()| log.flush()).is_ok();
        state.log = Some(log);
    }
    state.tx = None;
    state.handed_off = true;
    (captured, logging)
}

/// Spawns a line-buffered reader task that feeds `sink`.
///
/// The task owns its share of `sink`, which is released on task exit; that lets
/// bridges waiting on channel closure see EOF. The task is deliberately not
/// tied to the tool call: after a handoff its `JoinHandle` is dropped and the
/// task keeps draining the pipe into the adopted job's log.
fn spawn_stream_reader<H>(handle: Option<H>, sink: StreamSink) -> tokio::task::JoinHandle<()>
where
    H: AsyncRead + Unpin + Send + 'static,
{
    tokio::spawn(async move {
        let Some(handle) = handle else { return };
        let mut reader = tokio::io::BufReader::new(handle);
        let mut line = String::new();
        loop {
            line.clear();
            match reader.read_line(&mut line).await {
                Ok(0) | Err(_) => break,
                Ok(_) => push_line(&sink, &line),
            }
        }
    })
}

/// Waits for a reader task to finish within the grace period. If grace expires,
/// aborts the task (ensuring it drops its pipe handle and sender clone) and
/// awaits the abort to completion. Then extracts whatever was buffered.
///
/// This prevents hangs when an orphan descendant inherits the pipe FDs and
/// keeps the write-end open indefinitely. Partial output is preserved; only
/// any unterminated fragment still inside `BufReader` is lost on abort.
async fn finish_reader(mut task: tokio::task::JoinHandle<()>, sink: StreamSink) -> Vec<u8> {
    // Use select! with a sleep so we keep ownership of the JoinHandle on
    // expiry and can abort it — unlike tokio::time::timeout which consumes it.
    tokio::select! {
        _ = &mut task => {}
        () = tokio::time::sleep(READER_GRACE) => {
            task.abort();
            // Await the aborted task so its resources (pipe handle, sender
            // clone) are fully released before we return.
            let _ = task.await;
        }
    }

    sink.lock()
        .map(|mut state| std::mem::take(&mut state.buf))
        .unwrap_or_default()
}

/// Runs a shell command in the context's root directory.
#[allow(clippy::too_many_lines)]
async fn run_command(
    command: &str,
    ctx: &ToolContext,
    timeout: Option<Duration>,
    output_tx: Option<tokio::sync::mpsc::UnboundedSender<String>>,
    #[cfg(unix)] handoff: Option<Handoff>,
) -> Result<CommandOutcome, ToolOutput> {
    let started = std::time::Instant::now();

    #[cfg(unix)]
    let mut child = {
        let mut cmd = SupervisedCommand::new("/bin/sh");
        cmd.args(["-c", command])
            .current_dir(&ctx.root)
            .env("TERM", "dumb")
            .env("NO_COLOR", "1")
            .stdin_null();
        cmd.spawn(Duration::from_millis(300)).await.map_err(|e| {
            ToolOutput::failure(
                "spawn_error",
                format!("Failed to execute command '{command}'"),
                Some(format!("Error: {e}")),
            )
        })?
    };

    #[cfg(not(unix))]
    let mut child = {
        let mut cmd = tokio::process::Command::new("sh");
        cmd.arg("-c")
            .arg(command)
            .current_dir(&ctx.root)
            // Signal to programs that we are a non-interactive, dumb terminal.
            // This suppresses ANSI escape sequences, color output, and progress bars
            // in most well-behaved CLI tools (e.g. gcloud, npm, pip).
            .env("TERM", "dumb")
            .env("NO_COLOR", "1")
            // Force non-interactive stdin so child processes do not block waiting
            // for user input or keep client/daemon sessions alive (for example,
            // `gradlew` under piped exec environments).
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        cmd.kill_on_drop(true);

        cmd.spawn().map_err(|e| {
            ToolOutput::failure(
                "spawn_error",
                format!("Failed to execute command '{command}'"),
                Some(format!("Error: {e}")),
            )
        })?
    };

    // Take stdout/stderr handles before waiting so we can read incrementally.
    let child_stdout = child.stdout.take();
    let child_stderr = child.stderr.take();

    // Spawn stdout/stderr reader tasks. Shared sinks let us extract partial
    // output even if we have to abort a task that's stuck on a pipe held open
    // by an orphan descendant, and let an auto-background handoff redirect the
    // still-running readers to log files.
    let stdout_sink = new_sink(output_tx.clone());
    let stderr_sink = new_sink(output_tx.clone());

    let stdout_task = spawn_stream_reader(child_stdout, Arc::clone(&stdout_sink));
    let stderr_task = spawn_stream_reader(child_stderr, Arc::clone(&stderr_sink));

    // Wait for child exit, with optional timeout and optional foreground bound.
    // Reader tasks run independently — they'll see EOF after the process exits
    // (or is killed on timeout).
    #[cfg(unix)]
    let (timed_out, cancelled, exit_code, left_leftovers) = {
        let bound = handoff.as_ref().map(|handoff| handoff.bound);
        let outcome = child
            .wait_bounded(ctx.cancel_token.as_ref(), timeout, bound)
            .await
            .map_err(|err| {
                ToolOutput::failure(
                    "wait_error",
                    "Failed to wait for command",
                    Some(err.to_string()),
                )
            })?;

        let outcome = match outcome {
            BoundedWait::Finished(outcome) => outcome,
            BoundedWait::Bounded => {
                // The command keeps running: hand it off instead of killing it.
                // The reader tasks go with it and keep draining into the logs.
                let handoff = handoff.expect("a bound is only set alongside a handoff");
                return Ok(CommandOutcome::Backgrounded(Box::new(
                    hand_off_to_background(
                        child,
                        handoff,
                        command,
                        HandoffStreams {
                            stdout_sink: &stdout_sink,
                            stderr_sink: &stderr_sink,
                            readers: [stdout_task, stderr_task],
                        },
                        output_tx.as_ref(),
                        started.elapsed(),
                    ),
                )));
            }
        };

        (
            outcome.reason == WaitReason::TimedOut,
            outcome.reason == WaitReason::Cancelled,
            outcome.status.code().unwrap_or(-1),
            outcome.cleaned_leftovers,
        )
    };

    #[cfg(not(unix))]
    let (timed_out, cancelled, exit_code, left_leftovers) = match timeout {
        Some(dur) => {
            match tokio::time::timeout(dur, child.wait()).await {
                Ok(Ok(status)) => (false, false, status.code().unwrap_or(-1), false),
                Ok(Err(_)) => (false, false, -1, false),
                Err(_) => {
                    let _ = child.kill().await;

                    // Reap the child to avoid zombies.
                    let _ = child.wait().await;
                    (true, false, -1, false)
                }
            }
        }
        None => match child.wait().await {
            Ok(status) => (false, false, status.code().unwrap_or(-1), false),
            Err(_) => (false, false, -1, false),
        },
    };

    // Finish readers with a bounded grace period and extract their buffers.
    // This applies on every exit path — even normal completion can leave pipes
    // open if a descendant inherited stdout/stderr and escaped the process group.
    let stdout_buf = finish_reader(stdout_task, stdout_sink).await;
    let stderr_buf = finish_reader(stderr_task, stderr_sink).await;

    // Apply truncation to accumulated buffers.
    let (stdout, stdout_truncated, stdout_total_bytes) =
        super::truncate_bytes_to_byte_limit(&stdout_buf, MAX_OUTPUT_BYTES);
    let (stderr_text, stderr_truncated, stderr_total_bytes) =
        super::truncate_bytes_to_byte_limit(&stderr_buf, MAX_OUTPUT_BYTES);

    // Write full output to temp files when truncated
    let stdout_file = if stdout_truncated {
        write_temp_file(&stdout_buf, "stdout")
    } else {
        None
    };
    let stderr_file = if stderr_truncated {
        write_temp_file(&stderr_buf, "stderr")
    } else {
        None
    };

    if cancelled {
        return Err(ToolOutput::canceled("Interrupted by user"));
    }

    if timed_out {
        let timeout_msg = format!(
            "Command was killed after {} seconds by the deadline configured for this run (an automation's timeout_secs). The Bash timeout_secs argument cannot raise or remove it. Report that this run's operator-imposed limit stopped the command and what it needs instead, so the limit can be adjusted with the operator's approval.",
            timeout.map_or(0, |d| d.as_secs())
        );
        if let Some(ref tx) = output_tx {
            let _ = tx.send(timeout_msg.clone());
        }
        let stderr = if stderr_text.is_empty() {
            timeout_msg
        } else {
            format!("{stderr_text}\n{timeout_msg}")
        };
        return Ok(CommandOutcome::Completed(BashOutput {
            stdout,
            stderr,
            exit_code: -1,
            timed_out: true,
            stdout_truncated,
            stderr_truncated,
            stdout_total_bytes,
            stderr_total_bytes,
            stdout_file,
            stderr_file,
        }));
    }

    let stderr = if left_leftovers {
        let note = "note: this command left background descendants running; they were stopped. Use background: true to run a long-lived process (e.g. a dev server).";
        if stderr_text.is_empty() {
            note.to_string()
        } else {
            format!("{stderr_text}\n{note}")
        }
    } else {
        stderr_text
    };

    Ok(CommandOutcome::Completed(BashOutput {
        stdout,
        stderr,
        exit_code,
        timed_out: false,
        stdout_truncated,
        stderr_truncated,
        stdout_total_bytes,
        stderr_total_bytes,
        stdout_file,
        stderr_file,
    }))
}

/// The live stream plumbing handed over with a backgrounded command: the two
/// sinks to flip to log capture, and the reader tasks that keep feeding them.
#[cfg(unix)]
struct HandoffStreams<'a> {
    stdout_sink: &'a StreamSink,
    stderr_sink: &'a StreamSink,
    readers: [tokio::task::JoinHandle<()>; 2],
}

/// Moves a still-running foreground command to the background.
///
/// The command is never signalled: the live supervised handle (lease,
/// completion, process group) moves into the adopted-jobs registry, the stream
/// sinks flip from the in-memory tail to append-mode logs, and the delta stream
/// stops so no output event can follow the tool result.
///
/// Bookkeeping failures are non-fatal by design — a job we failed to record is
/// still a job the user asked for, so it keeps running and is reported with
/// `tracking_failed`.
#[cfg(unix)]
fn hand_off_to_background(
    child: SupervisedChild,
    handoff: Handoff,
    command: &str,
    streams: HandoffStreams<'_>,
    output_tx: Option<&tokio::sync::mpsc::UnboundedSender<String>>,
    elapsed: Duration,
) -> BackgroundedOutput {
    let HandoffStreams {
        stdout_sink,
        stderr_sink,
        readers,
    } = streams;
    let Handoff {
        bound: _,
        bg_id,
        stdout_log,
        stderr_log,
        on_adopt,
        on_exit,
    } = handoff;
    let identity = child.identity;

    let open_log = |path: &std::path::Path| {
        use std::os::unix::fs::OpenOptionsExt as _;
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .mode(0o600)
            .open(path)
            .ok()
    };
    let (stdout_buf, stdout_logging) = hand_off_sink(stdout_sink, open_log(&stdout_log));
    let (stderr_buf, stderr_logging) = hand_off_sink(stderr_sink, open_log(&stderr_log));

    let registry_failed = !on_adopt(identity);
    crate::adopted::adopt(bg_id.clone(), child, command.to_string(), readers, on_exit);

    let (stdout, stdout_truncated, stdout_total_bytes) =
        super::truncate_bytes_to_byte_limit(&stdout_buf, MAX_OUTPUT_BYTES);
    let (stderr, stderr_truncated, stderr_total_bytes) =
        super::truncate_bytes_to_byte_limit(&stderr_buf, MAX_OUTPUT_BYTES);

    let output = BackgroundedOutput {
        bg_id,
        pid: identity.pid,
        pgid: identity.pgid,
        elapsed_secs: elapsed.as_secs(),
        stdout,
        stderr,
        stdout_truncated,
        stderr_truncated,
        stdout_total_bytes,
        stderr_total_bytes,
        stdout_file: stdout_truncated
            .then(|| write_temp_file(&stdout_buf, "stdout"))
            .flatten(),
        stderr_file: stderr_truncated
            .then(|| write_temp_file(&stderr_buf, "stderr"))
            .flatten(),
        stdout_log: stdout_log.to_string_lossy().into_owned(),
        stderr_log: stderr_log.to_string_lossy().into_owned(),
        degraded: HandoffDegradation {
            registry_failed,
            // Either stream losing its log means output is going missing from
            // here on, which the caller must be told about.
            logging_failed: !stdout_logging || !stderr_logging,
        },
    };

    // Surfaces streaming this tool's output see why it stopped. Sent before the
    // tool result, so it cannot arrive after the call completes.
    if let Some(tx) = output_tx {
        let _ = tx.send(format!("{}\n", output.message()));
    }

    output
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use super::*;

    /// `execute` with auto-backgrounding off, which is what every test that
    /// isn't about the handoff wants.
    async fn execute_plain(
        input: &Value,
        ctx: &ToolContext,
        timeout: Option<Duration>,
    ) -> ToolOutput {
        execute(
            input,
            ctx,
            timeout,
            None,
            #[cfg(unix)]
            None,
        )
        .await
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn foreground_backgrounded_child_is_cleaned_up() {
        // A foreground command that shell-backgrounds a long sleep and records
        // its PID. After the command returns, the sleep must be gone — proving
        // `cmd &` no longer silently survives (only `background: true` should).
        let temp = TempDir::new().unwrap();
        let pidfile = temp.path().join("child.pid");
        let ctx = ToolContext::new(temp.path().to_path_buf(), None);
        let input = json!({
            "command": format!("sleep 30 & echo $! > {}", pidfile.display()),
        });

        let result = execute_plain(&input, &ctx, None).await;
        assert!(result.is_ok());

        let pid: i32 = std::fs::read_to_string(&pidfile)
            .unwrap()
            .trim()
            .parse()
            .unwrap();

        // Give the graceful TERM→KILL a moment to land.
        tokio::time::sleep(Duration::from_millis(400)).await;
        let alive = unsafe { libc::kill(pid, 0) == 0 };
        assert!(
            !alive,
            "backgrounded child (pid {pid}) should have been killed"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn foreground_command_honors_invocation_cancellation() {
        let temp = TempDir::new().unwrap();
        let pidfile = temp.path().join("foreground.pid");
        let cancel = tokio_util::sync::CancellationToken::new();
        let mut ctx = ToolContext::new(temp.path().to_path_buf(), None);
        ctx.cancel_token = Some(cancel.clone());
        let input = json!({
            "command": format!(
                "trap '' TERM; echo $$ > {}; while :; do sleep 1; done",
                pidfile.display()
            ),
        });

        let task = tokio::spawn(async move { execute_plain(&input, &ctx, None).await });
        let pid: i32 = tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                if let Ok(raw) = std::fs::read_to_string(&pidfile)
                    && let Ok(pid) = raw.trim().parse()
                {
                    break pid;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("foreground command did not publish its pid");

        cancel.cancel();
        let result = task.await.unwrap();
        assert!(matches!(result, ToolOutput::Canceled { .. }));
        assert_eq!(unsafe { libc::kill(pid, 0) }, -1);
    }

    /// The caller's `timeout_secs` only tunes how long the foreground wait
    /// lasts. There is no input that turns it into a deadline.
    #[cfg(unix)]
    #[test]
    fn test_caller_timeout_only_tunes_the_wait_bound() {
        fn bound_for(requested: Option<u64>) -> Option<Duration> {
            let handoff = Handoff {
                bound: Duration::from_secs(120),
                bg_id: "bg-test".to_string(),
                stdout_log: std::path::PathBuf::from("/dev/null"),
                stderr_log: std::path::PathBuf::from("/dev/null"),
                on_adopt: Box::new(|_| true),
                on_exit: Box::new(|_| {}),
            };
            apply_wait_bound(Some(handoff), requested).map(|handoff| handoff.bound)
        }

        // Omitted and 0 both keep the configured bound.
        assert_eq!(bound_for(None), Some(Duration::from_secs(120)));
        assert_eq!(bound_for(Some(0)), Some(Duration::from_secs(120)));
        // A positive value replaces it, in either direction.
        assert_eq!(bound_for(Some(5)), Some(Duration::from_secs(5)));
        assert_eq!(bound_for(Some(600)), Some(Duration::from_mins(10)));
        // With no handoff there is no bound to set and nothing to relocate into.
        assert!(apply_wait_bound(None, Some(600)).is_none());
    }

    #[tokio::test]
    async fn test_bash_executes_command() {
        let temp = TempDir::new().unwrap();
        let ctx = ToolContext::new(temp.path().to_path_buf(), None);
        let input = json!({"command": "echo hello"});

        let result = execute_plain(&input, &ctx, None).await;
        assert!(result.is_ok());
        let data = result.data().expect("should have data");
        assert!(data["stdout"].as_str().unwrap().contains("hello"));
        assert_eq!(data["exit_code"], 0);
        assert_eq!(data["timed_out"], false);
        assert_eq!(data["stdout_truncated"], false);
        assert_eq!(data["stderr_truncated"], false);
    }

    #[tokio::test]
    async fn test_bash_captures_stderr() {
        let temp = TempDir::new().unwrap();
        let ctx = ToolContext::new(temp.path().to_path_buf(), None);
        let input = json!({"command": "echo error >&2"});

        let result = execute_plain(&input, &ctx, None).await;
        assert!(result.is_ok());
        let data = result.data().expect("should have data");
        assert!(data["stderr"].as_str().unwrap().contains("error"));
    }

    #[tokio::test]
    async fn test_bash_captures_exit_code() {
        let temp = TempDir::new().unwrap();
        let ctx = ToolContext::new(temp.path().to_path_buf(), None);
        let input = json!({"command": "exit 42"});

        let result = execute_plain(&input, &ctx, None).await;
        assert!(result.is_ok());
        let data = result.data().expect("should have data");
        assert_eq!(data["exit_code"], 42);
    }

    #[tokio::test]
    async fn test_bash_runs_in_root_directory() {
        let temp = TempDir::new().unwrap();
        std::fs::write(temp.path().join("test.txt"), "content").unwrap();

        let ctx = ToolContext::new(temp.path().to_path_buf(), None);
        let input = json!({"command": "ls"});

        let result = execute_plain(&input, &ctx, None).await;
        assert!(result.is_ok());
        let data = result.data().expect("should have data");
        assert!(data["stdout"].as_str().unwrap().contains("test.txt"));
    }

    #[tokio::test]
    async fn test_bash_timeout() {
        let temp = TempDir::new().unwrap();
        let ctx = ToolContext::new(temp.path().to_path_buf(), None);
        let input = json!({"command": "sleep 5"});

        let result = execute_plain(&input, &ctx, Some(Duration::from_millis(100))).await;
        assert!(result.is_ok()); // timed_out is success with timed_out=true
        let data = result.data().expect("should have data");
        assert_eq!(data["timed_out"], true);
        assert_eq!(data["stdout_truncated"], false);
        assert_eq!(data["stderr_truncated"], false);
        // The kill is the operator's, so the model is told to report the limit
        // rather than to retry around it.
        assert!(
            data["stderr"]
                .as_str()
                .unwrap_or_default()
                .contains("operator-imposed limit"),
            "timeout should name the operator limit, got: {}",
            data["stderr"]
        );
        assert!(
            !data["stderr"]
                .as_str()
                .unwrap_or_default()
                .contains("background: true"),
            "timeout must not advise bypassing the operator deadline, got: {}",
            data["stderr"]
        );
    }

    /// With no handoff to relocate into, `timeout_secs` has nothing to bound —
    /// and must still never kill. The command runs to completion.
    #[tokio::test]
    async fn test_bash_timeout_secs_never_kills_without_a_handoff() {
        let temp = TempDir::new().unwrap();
        let ctx = ToolContext::new(temp.path().to_path_buf(), None);
        let input = json!({"command": "sleep 1; echo done", "timeout_secs": 1});

        let result = execute_plain(&input, &ctx, None).await;
        assert!(result.is_ok());
        let data = result.data().expect("should have data");
        assert_eq!(data["timed_out"], false);
        assert_eq!(data["exit_code"], 0);
        assert!(data["stdout"].as_str().unwrap().contains("done"));
    }

    /// The run's configured deadline is the operator's, so a caller's
    /// `timeout_secs` cannot switch it off — not even with the `0` that used to
    /// mean "no timeout".
    #[tokio::test]
    async fn test_bash_timeout_secs_zero_cannot_disable_configured_deadline() {
        let temp = TempDir::new().unwrap();
        let ctx = ToolContext::new(temp.path().to_path_buf(), None);
        let input = json!({"command": "sleep 30", "timeout_secs": "0"});

        let result = execute_plain(&input, &ctx, Some(Duration::from_millis(100))).await;
        assert!(result.is_ok());
        let data = result.data().expect("should have data");
        assert_eq!(data["timed_out"], true);
    }

    #[tokio::test]
    async fn test_bash_invalid_input() {
        let temp = TempDir::new().unwrap();
        let ctx = ToolContext::new(temp.path().to_path_buf(), None);
        let input = json!({"wrong_field": "ls"});

        let result = execute_plain(&input, &ctx, None).await;
        assert!(!result.is_ok());
        let json_str = result.to_json_string();
        assert!(json_str.contains(r#""code":"invalid_input""#));
    }

    #[tokio::test]
    async fn test_bash_rejects_empty_command() {
        let temp = TempDir::new().unwrap();
        let ctx = ToolContext::new(temp.path().to_path_buf(), None);
        let input = json!({"command": "   "});

        let result = execute_plain(&input, &ctx, None).await;
        assert!(!result.is_ok());
        let payload = serde_json::to_value(result).unwrap();
        assert_eq!(payload["error"]["code"], "invalid_input");
        assert_eq!(payload["error"]["message"], "command cannot be empty");
    }

    #[tokio::test]
    async fn test_bash_run_rejects_empty_command() {
        let temp = TempDir::new().unwrap();
        let ctx = ToolContext::new(temp.path().to_path_buf(), None);

        let result = run("", &ctx, None, None).await;
        assert!(!result.is_ok());
        let payload = serde_json::to_value(result).unwrap();
        assert_eq!(payload["error"]["code"], "invalid_input");
        assert_eq!(payload["error"]["message"], "command cannot be empty");
    }

    #[tokio::test]
    async fn test_bash_stdout_truncated_writes_temp_file() {
        let temp = TempDir::new().unwrap();
        let ctx = ToolContext::new(temp.path().to_path_buf(), None);
        // Generate more than 40KB of output (50KB of 'x' characters)
        let input = json!({"command": "head -c 51200 /dev/zero | tr '\\0' 'x'"});

        let result = execute_plain(&input, &ctx, None).await;
        assert!(result.is_ok());
        let data = result.data().expect("should have data");

        // Should have a stdout_file path
        let stdout_file = data["stdout_file"]
            .as_str()
            .expect("should have stdout_file");
        assert!(stdout_file.contains("zdx-bash-"));
        assert!(stdout_file.contains("-stdout.txt"));

        // File should exist and contain full output
        let file_contents = std::fs::read_to_string(stdout_file).expect("should read temp file");
        assert_eq!(file_contents.len(), 51200);
        assert!(file_contents.chars().all(|c| c == 'x'));

        // Clean up
        let _ = std::fs::remove_file(stdout_file);
    }

    #[tokio::test]
    async fn test_bash_stderr_truncated_writes_temp_file() {
        let temp = TempDir::new().unwrap();
        let ctx = ToolContext::new(temp.path().to_path_buf(), None);
        // Generate more than 40KB of stderr output (50KB)
        let input = json!({"command": "head -c 51200 /dev/zero | tr '\\0' 'y' >&2"});

        let result = execute_plain(&input, &ctx, None).await;
        assert!(result.is_ok());
        let data = result.data().expect("should have data");

        // Should have a stderr_file path
        let stderr_file = data["stderr_file"]
            .as_str()
            .expect("should have stderr_file");
        assert!(stderr_file.contains("zdx-bash-"));
        assert!(stderr_file.contains("-stderr.txt"));

        // File should exist and contain full output
        let file_contents = std::fs::read_to_string(stderr_file).expect("should read temp file");
        assert_eq!(file_contents.len(), 51200);
        assert!(file_contents.chars().all(|c| c == 'y'));

        // Clean up
        let _ = std::fs::remove_file(stderr_file);
    }

    #[tokio::test]
    async fn test_bash_no_truncation_no_temp_file() {
        let temp = TempDir::new().unwrap();
        let ctx = ToolContext::new(temp.path().to_path_buf(), None);
        // Generate less than 40KB of output (1KB)
        let input = json!({"command": "head -c 1024 /dev/zero | tr '\\0' 'z'"});

        let result = execute_plain(&input, &ctx, None).await;
        assert!(result.is_ok());
        let data = result.data().expect("should have data");

        // Should NOT have stdout_file or stderr_file
        assert!(data.get("stdout_file").is_none());
        assert!(data.get("stderr_file").is_none());
    }

    #[test]
    fn test_write_temp_file() {
        let content = b"Hello, temp file!";
        let path = write_temp_file(content, "test").expect("should write temp file");

        assert!(path.contains("zdx-bash-"));
        assert!(path.contains("-test.txt"));

        // Verify file contents
        let read_content = std::fs::read_to_string(&path).expect("should read temp file");
        assert_eq!(read_content.as_bytes(), content);

        // Clean up
        let _ = std::fs::remove_file(&path);
    }
}

/// Auto-background handoff: a foreground command that outruns its bound is
/// relocated, never killed.
#[cfg(all(test, unix))]
mod handoff_tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicI64, AtomicUsize, Ordering};
    use std::time::Duration;

    use serde_json::{Value, json};
    use tempfile::TempDir;

    use super::*;

    const UNSET: i64 = i64::MIN;

    /// Captures what the engine would record, so tests can assert on the
    /// registry hooks without depending on the engine crate.
    struct Recorder {
        dir: TempDir,
        adopted: Arc<AtomicUsize>,
        exit_code: Arc<AtomicI64>,
        identity: Arc<Mutex<Option<crate::process_supervisor::TargetIdentity>>>,
    }

    impl Recorder {
        fn new() -> Self {
            Self {
                dir: TempDir::new().unwrap(),
                adopted: Arc::new(AtomicUsize::new(0)),
                exit_code: Arc::new(AtomicI64::new(UNSET)),
                identity: Arc::new(Mutex::new(None)),
            }
        }

        fn stdout_log(&self) -> std::path::PathBuf {
            self.dir.path().join("job.out")
        }

        fn stderr_log(&self) -> std::path::PathBuf {
            self.dir.path().join("job.err")
        }

        fn handoff(&self, bound: Duration) -> Handoff {
            let adopted = Arc::clone(&self.adopted);
            let identity = Arc::clone(&self.identity);
            let exit_code = Arc::clone(&self.exit_code);
            Handoff {
                bound,
                bg_id: format!("bg-{}", Uuid::new_v4()),
                stdout_log: self.stdout_log(),
                stderr_log: self.stderr_log(),
                on_adopt: Box::new(move |target| {
                    *identity.lock().unwrap() = Some(target);
                    adopted.fetch_add(1, Ordering::SeqCst);
                    true
                }),
                on_exit: Box::new(move |code| {
                    exit_code.store(code.map_or(-1, i64::from), Ordering::SeqCst);
                }),
            }
        }

        fn pid(&self) -> u32 {
            self.identity.lock().unwrap().expect("adopted").pid
        }

        async fn await_exit(&self, budget: Duration) -> i64 {
            tokio::time::timeout(budget, async {
                loop {
                    let code = self.exit_code.load(Ordering::SeqCst);
                    if code != UNSET {
                        return code;
                    }
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
            })
            .await
            .expect("adopted job never reported an exit")
        }
    }

    fn process_exists(pid: u32) -> bool {
        if unsafe { libc::kill(pid as i32, 0) } == 0 {
            return true;
        }
        std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
    }

    async fn wait_until_gone(pid: u32) {
        tokio::time::timeout(Duration::from_secs(5), async {
            while process_exists(pid) {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("backgrounded target survived cleanup");
    }

    async fn run(input: &Value, root: &std::path::Path, handoff: Option<Handoff>) -> ToolOutput {
        let ctx = ToolContext::new(root.to_path_buf(), None);
        execute(input, &ctx, None, None, handoff).await
    }

    /// The core contract: at the bound the command is relocated, the result is
    /// a success carrying a `bg_id` and the output captured so far, and the
    /// process is still alive.
    #[tokio::test]
    async fn bound_moves_command_to_background_with_partial_output() {
        let recorder = Recorder::new();
        let temp = TempDir::new().unwrap();
        let input = json!({"command": "echo first; echo early >&2; sleep 30"});

        let result = run(
            &input,
            temp.path(),
            Some(recorder.handoff(Duration::from_millis(300))),
        )
        .await;

        assert!(result.is_ok(), "handoff is a success, not a failure");
        let data = result.data().expect("should have data");
        assert_eq!(data["backgrounded"], true);
        assert_eq!(data["status"], "running");
        assert_eq!(data["exit_code"], Value::Null);
        assert_eq!(data["timed_out"], false);
        assert!(data["bg_id"].as_str().unwrap().starts_with("bg-"));
        assert!(data.get("tracking_failed").is_none());

        // Output produced before the handoff survives into the result...
        assert!(data["stdout"].as_str().unwrap().contains("first"));
        assert!(data["stderr"].as_str().unwrap().contains("early"));
        // ...and the model is told not to re-run it.
        let message = data["message"].as_str().unwrap();
        assert!(message.contains("moved to the background"), "{message}");
        assert!(message.contains("do NOT re-run"), "{message}");
        assert!(message.contains("background_output"), "{message}");

        assert_eq!(recorder.adopted.load(Ordering::SeqCst), 1);
        let pid = recorder.pid();
        assert!(process_exists(pid), "handoff must not kill the command");

        // The same bytes are also flushed to the log, so a later poll sees them.
        let logged = std::fs::read_to_string(recorder.stdout_log()).unwrap();
        assert!(
            logged.contains("first"),
            "log should hold pre-handoff output"
        );

        assert!(crate::adopted::terminate(data["bg_id"].as_str().unwrap()).await);
        wait_until_gone(pid).await;
    }

    /// A command that keeps streaming past its bound must finish normally:
    /// relocation moves the wait, not the work. This is the Cargo-build case.
    #[tokio::test]
    async fn streaming_command_completes_unharmed_after_handoff() {
        let recorder = Recorder::new();
        let temp = TempDir::new().unwrap();
        // Emits well before, across, and after the bound, then exits non-zero
        // so both the completion and its real status have to survive.
        let input = json!({
            "command": "for i in 1 2 3 4 5 6; do echo line-$i; sleep 0.2; done; exit 5"
        });

        let result = run(
            &input,
            temp.path(),
            Some(recorder.handoff(Duration::from_millis(400))),
        )
        .await;
        let data = result.data().expect("should have data");
        assert_eq!(data["backgrounded"], true);

        assert_eq!(recorder.await_exit(Duration::from_secs(15)).await, 5);

        // Every line is in the log: the ones captured before the handoff were
        // flushed into it, and the rest were appended by the same readers.
        let logged = std::fs::read_to_string(recorder.stdout_log()).unwrap();
        for i in 1..=6 {
            assert!(logged.contains(&format!("line-{i}")), "log was: {logged}");
        }
        wait_until_gone(recorder.pid()).await;
    }

    /// Killing an adopted job routes through the lease and leaves nothing
    /// behind, even for a target that ignores SIGTERM.
    #[tokio::test]
    async fn adopted_job_is_cancellable_and_leaves_no_orphan() {
        let recorder = Recorder::new();
        let temp = TempDir::new().unwrap();
        let input = json!({"command": "trap '' TERM; echo up; while :; do sleep 1; done"});

        let result = run(
            &input,
            temp.path(),
            Some(recorder.handoff(Duration::from_millis(300))),
        )
        .await;
        let data = result.data().expect("should have data");
        let bg_id = data["bg_id"].as_str().unwrap();
        let pid = recorder.pid();

        assert!(crate::adopted::terminate(bg_id).await);
        wait_until_gone(pid).await;
        assert!(!crate::adopted::is_adopted(bg_id));
        // The exit is still recorded, so the registry can be tombstoned.
        recorder.await_exit(Duration::from_secs(5)).await;
    }

    /// Regression for the lost-build-work incident: a model that volunteers
    /// `timeout_secs` on a slow build must not destroy it. The value lowers the
    /// wait, then the command is relocated with its work intact.
    #[tokio::test]
    async fn caller_timeout_lowers_the_bound_and_relocates_instead_of_killing() {
        let recorder = Recorder::new();
        let temp = TempDir::new().unwrap();
        // A string, exactly as the incident sent it.
        let input = json!({"command": "sleep 30", "timeout_secs": "1"});

        let started = std::time::Instant::now();
        let result = run(
            &input,
            temp.path(),
            // Far longer than the caller's value: relocating promptly proves
            // the caller lowered the bound rather than being ignored.
            Some(recorder.handoff(Duration::from_secs(20))),
        )
        .await;
        let elapsed = started.elapsed();

        let data = result.data().expect("should have data");
        assert_eq!(data["backgrounded"], true);
        assert_eq!(data["timed_out"], false);
        assert_eq!(recorder.adopted.load(Ordering::SeqCst), 1);
        assert!(
            elapsed < Duration::from_secs(10),
            "caller's 1s bound should apply, waited {elapsed:?}"
        );

        let pid = recorder.pid();
        assert!(
            process_exists(pid),
            "a caller timeout must never kill the command"
        );

        assert!(crate::adopted::terminate(data["bg_id"].as_str().unwrap()).await);
        wait_until_gone(pid).await;
    }

    /// The same parameter raising the wait: a command the caller expects to be
    /// slow finishes in the foreground instead of being relocated at the
    /// default bound.
    #[tokio::test]
    async fn caller_timeout_raises_the_bound() {
        let recorder = Recorder::new();
        let temp = TempDir::new().unwrap();
        let input = json!({"command": "sleep 1; echo done", "timeout_secs": 10});

        let result = run(
            &input,
            temp.path(),
            Some(recorder.handoff(Duration::from_millis(200))),
        )
        .await;

        let data = result.data().expect("should have data");
        assert!(
            data.get("backgrounded").is_none(),
            "the raised bound should keep it in the foreground"
        );
        assert_eq!(data["exit_code"], 0);
        assert_eq!(data["timed_out"], false);
        assert!(data["stdout"].as_str().unwrap().contains("done"));
        assert_eq!(recorder.adopted.load(Ordering::SeqCst), 0);
    }

    /// The operator's deadline (an automation's `timeout_secs` frontmatter) is
    /// the only destructive one, and it suppresses the handoff so it still
    /// kills. The caller's `0` must not reach it.
    #[tokio::test]
    async fn configured_deadline_still_kills_and_never_backgrounds() {
        let recorder = Recorder::new();
        let temp = TempDir::new().unwrap();
        let input = json!({"command": "sleep 30", "timeout_secs": 0});

        let ctx = ToolContext::new(temp.path().to_path_buf(), None);
        let result = execute(
            &input,
            &ctx,
            Some(Duration::from_secs(1)),
            None,
            Some(recorder.handoff(Duration::from_millis(200))),
        )
        .await;

        let data = result.data().expect("should have data");
        assert_eq!(data["timed_out"], true);
        assert!(data.get("backgrounded").is_none());
        assert_eq!(recorder.adopted.load(Ordering::SeqCst), 0);
        assert!(
            data["stderr"]
                .as_str()
                .unwrap()
                .contains("cannot raise or remove it"),
            "the kill should name the operator deadline, got: {}",
            data["stderr"]
        );
    }

    /// Without a handoff the foreground wait is unbounded, as before.
    #[tokio::test]
    async fn no_handoff_keeps_waiting_in_the_foreground() {
        let temp = TempDir::new().unwrap();
        let input = json!({"command": "sleep 0.5; echo done"});

        let result = run(&input, temp.path(), None).await;
        let data = result.data().expect("should have data");
        assert!(data.get("backgrounded").is_none());
        assert_eq!(data["exit_code"], 0);
        assert!(data["stdout"].as_str().unwrap().contains("done"));
    }

    /// A command that finishes inside its bound is completely unaffected.
    #[tokio::test]
    async fn fast_command_is_not_backgrounded() {
        let recorder = Recorder::new();
        let temp = TempDir::new().unwrap();
        let input = json!({"command": "echo quick"});

        let result = run(
            &input,
            temp.path(),
            Some(recorder.handoff(Duration::from_secs(30))),
        )
        .await;

        let data = result.data().expect("should have data");
        assert!(data.get("backgrounded").is_none());
        assert_eq!(data["exit_code"], 0);
        assert!(data["stdout"].as_str().unwrap().contains("quick"));
        assert_eq!(recorder.adopted.load(Ordering::SeqCst), 0);
        assert!(
            !recorder.stdout_log().exists(),
            "a command that never backgrounds must not create a log"
        );
    }

    /// Bookkeeping failure must never cost the user their running command.
    #[tokio::test]
    async fn failed_registration_keeps_the_job_running() {
        let recorder = Recorder::new();
        let temp = TempDir::new().unwrap();
        let mut handoff = recorder.handoff(Duration::from_millis(300));
        handoff.on_adopt = Box::new(|_| false);

        let input = json!({"command": "sleep 30"});
        let result = run(&input, temp.path(), Some(handoff)).await;

        assert!(result.is_ok());
        let data = result.data().expect("should have data");
        assert_eq!(data["backgrounded"], true);
        assert_eq!(data["tracking_failed"], true);
        let pid = u32::try_from(data["pid"].as_u64().unwrap()).unwrap();
        assert!(
            process_exists(pid),
            "a job we failed to record must live on"
        );

        assert!(crate::adopted::terminate(data["bg_id"].as_str().unwrap()).await);
        wait_until_gone(pid).await;
    }

    /// After a handoff the delta channel must close, or the engine's streaming
    /// bridge would wait forever on a tool call that already returned. The
    /// relocation notice is the last chunk, sent before the result.
    #[tokio::test]
    async fn handoff_closes_the_streaming_channel() {
        let recorder = Recorder::new();
        let temp = TempDir::new().unwrap();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<String>();
        let ctx = ToolContext::new(temp.path().to_path_buf(), None);
        let input = json!({"command": "echo streamed; sleep 30"});

        let (result, chunks) = tokio::join!(
            execute(
                &input,
                &ctx,
                None,
                Some(tx),
                Some(recorder.handoff(Duration::from_millis(300))),
            ),
            async {
                let mut chunks = Vec::new();
                // Returns only once every sender is dropped.
                while let Some(chunk) = rx.recv().await {
                    chunks.push(chunk);
                }
                chunks
            }
        );

        let data = result.data().expect("should have data");
        assert_eq!(data["backgrounded"], true);
        let joined = chunks.join("");
        assert!(joined.contains("streamed"), "streamed lines: {joined}");
        assert!(
            joined.contains("moved to the background"),
            "surfaces should be told why streaming stopped: {joined}"
        );

        let pid = recorder.pid();
        assert!(crate::adopted::terminate(data["bg_id"].as_str().unwrap()).await);
        wait_until_gone(pid).await;
    }

    /// A log we cannot open means every later line is lost. The job still runs
    /// — losing output is never a reason to kill work — but the result must say
    /// so instead of showing a healthy job behind a permanently empty log.
    #[tokio::test]
    async fn unusable_log_is_reported_not_silently_dropped() {
        let recorder = Recorder::new();
        let temp = TempDir::new().unwrap();
        let mut handoff = recorder.handoff(Duration::from_millis(300));
        // A directory can never be opened as an append-mode log file.
        let blocked = recorder.dir.path().join("blocked");
        std::fs::create_dir(&blocked).unwrap();
        handoff.stdout_log = blocked;

        let input = json!({"command": "echo before; sleep 30"});
        let result = run(&input, temp.path(), Some(handoff)).await;

        assert!(result.is_ok(), "a lost log must not fail the handoff");
        let data = result.data().expect("should have data");
        assert_eq!(data["backgrounded"], true);
        assert_eq!(data["logging_failed"], true);
        assert_eq!(data["tracking_failed"], true);
        assert!(
            data["message"].as_str().unwrap().contains("not captured"),
            "message must explain the loss: {}",
            data["message"]
        );
        // Output captured before the handoff is still returned.
        assert!(data["stdout"].as_str().unwrap().contains("before"));

        let pid = recorder.pid();
        assert!(process_exists(pid), "the job keeps running regardless");
        assert!(crate::adopted::terminate(data["bg_id"].as_str().unwrap()).await);
        wait_until_gone(pid).await;
    }

    /// Regression for the 2026-09-18 incident: an unscoped recursive grep whose
    /// `head -20` never caps the scan blocked a turn for 487s. Reproduced on an
    /// isolated fixture only — never against a real workspace.
    ///
    /// The fixture mirrors the incident's shape (a heavy build-output directory
    /// that `--include=*` drags in) and adds a FIFO so the walk cannot finish,
    /// which makes "outruns the bound" deterministic instead of load-dependent.
    #[tokio::test]
    async fn unscoped_recursive_grep_is_relocated_not_killed() {
        let recorder = Recorder::new();
        let temp = TempDir::new().unwrap();
        let root = temp.path();

        std::fs::write(root.join("spec.md"), "FR-005 must hold\n").unwrap();
        let build = root.join("target/debug/deps");
        std::fs::create_dir_all(&build).unwrap();
        for i in 0..64 {
            std::fs::write(build.join(format!("lib-{i}.rlib")), b"\0\0FR-005\0\0").unwrap();
        }

        // Blocks the walk indefinitely: grep opens the FIFO and waits on a
        // writer that never comes.
        let fifo = std::ffi::CString::new(root.join("pipe.sock").to_str().unwrap()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);

        let input = json!({
            "command": r#"grep -rn "FR-005" --include=* . 2>/dev/null | grep -v "/build/" | head -20"#
        });
        let result = run(
            &input,
            root,
            Some(recorder.handoff(Duration::from_millis(500))),
        )
        .await;

        let data = result.data().expect("should have data");
        assert_eq!(
            data["backgrounded"], true,
            "an unscoped grep that outruns the bound must be relocated"
        );
        let pid = recorder.pid();
        assert!(process_exists(pid), "relocation must not kill the scan");

        assert!(crate::adopted::terminate(data["bg_id"].as_str().unwrap()).await);
        wait_until_gone(pid).await;
    }
}
