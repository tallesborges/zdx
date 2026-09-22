//! Background-process tools + spawn/registration used by the Bash tool.
//!
//! - [`run_background`] is invoked by the `Bash` tool when `background: true`:
//!   it spawns a detached process (via [`zdx_tools::bash::spawn_background`]),
//!   registers it in [`crate::background_activity`], starts a reaping waiter,
//!   and returns a `bg_id`.
//! - [`prepare_handoff`] is invoked by the `Bash` tool for every *foreground*
//!   command: it supplies the registry hooks and log paths the tool needs to
//!   move the command to the background if it outruns its foreground bound.
//! - [`BackgroundOutput`] and [`BackgroundKill`] are agent tools that read a
//!   background process's output / stop it, scoped to the caller's thread.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use serde::Deserialize;
use serde_json::{Value, json};

use super::{Tool, ToolContext, ToolDefinition, ToolFuture};
use crate::background_activity::{self, BackgroundMode, BackgroundProcess, KillOutcome};
use crate::core::events::ToolOutput;

/// Max bytes returned per stream by `background_output`.
const OUTPUT_TAIL_BYTES: usize = 8 * 1024;

/// Strictly increasing sequence stamped on every `background_output` result.
///
/// Polling a background job that has not produced new output otherwise returns
/// a byte-identical result, which the turn's identical-tool-call detector would
/// abort as a loop. Making each read distinct keeps legitimate polling legal
/// while leaving genuinely repeated work detectable.
static READ_SEQ: AtomicU64 = AtomicU64::new(0);

/// Background-mode subset of the `Bash` tool input.
///
/// `timeout_secs` reuses the Bash tool's coercion so `0` and `"0"` mean the
/// same thing here: no wait bound, which is what a detached process already is.
#[derive(Debug, Deserialize)]
struct BackgroundInput {
    #[serde(default)]
    command: String,
    #[serde(
        default,
        deserialize_with = "zdx_tools::u64_or_string::deserialize_optional"
    )]
    timeout_secs: Option<u64>,
}

/// Spawns + registers a background process. Called by `Bash` on `background: true`.
#[allow(clippy::similar_names)] // pid / pgid are the natural names here
pub async fn run_background(input: &Value, ctx: &ToolContext) -> ToolOutput {
    let input: BackgroundInput = match serde_json::from_value(input.clone()) {
        Ok(i) => i,
        Err(e) => {
            return ToolOutput::failure(
                "invalid_input",
                format!("Invalid input for bash tool: {e}"),
                None,
            );
        }
    };

    let command = input.command.trim();
    if command.is_empty() {
        return ToolOutput::failure("invalid_input", "command cannot be empty", None);
    }
    // A detached process is never awaited, so there is no foreground wait for
    // `timeout_secs` to bound.
    if input.timeout_secs.is_some_and(|secs| secs > 0) {
        return ToolOutput::failure(
            "invalid_input",
            "timeout_secs must be omitted or 0 with background: true (a background process is \
             not awaited, so there is no foreground wait to bound)",
            None,
        );
    }

    if let Err(e) = background_activity::ensure_dirs() {
        return ToolOutput::failure(
            "io_error",
            format!("failed to prepare background dir: {e}"),
            None,
        );
    }

    let bg_id = format!("bg-{}", uuid::Uuid::new_v4());
    let stdout_log = background_activity::stdout_log_path(&bg_id);
    let stderr_log = background_activity::stderr_log_path(&bg_id);
    let cwd = ctx.root.clone();

    let spawn = match zdx_tools::bash::spawn_background(command, &cwd, &stdout_log, &stderr_log) {
        Ok(s) => s,
        Err(e) => {
            return ToolOutput::failure(
                "spawn_error",
                format!("failed to spawn background process: {e}"),
                None,
            );
        }
    };
    let pid = spawn.pid;

    // Capture OS identity (birth-time + pgid) for the PID-reuse kill guard.
    let (birth_id, pgid) = background_activity::capture_identity(pid);
    let Some(pgid) = pgid else {
        kill_failed_spawn(spawn, pid).await;
        return ToolOutput::failure(
            "spawn_error",
            "failed to capture background process identity",
            None,
        );
    };

    let rec = BackgroundProcess {
        bg_id: bg_id.clone(),
        pid,
        pgid,
        birth_id,
        thread_id: ctx.current_thread_id.clone(),
        command: command.to_string(),
        cwd: cwd.to_string_lossy().into_owned(),
        started_at: chrono::Utc::now().to_rfc3339(),
        mode: BackgroundMode::Detached,
        exited_at: None,
        exit_code: None,
    };

    // Commit-then-report: register before returning success. On failure, tear
    // down the spawned process so we never leak an untracked one.
    if let Err(e) = background_activity::write_marker(&rec) {
        kill_failed_spawn(spawn, pid).await;
        return ToolOutput::failure(
            "io_error",
            format!("failed to register background process: {e}"),
            None,
        );
    }
    background_activity::log_spawned(&rec);

    // Detached waiter: own the child, reap it on exit, and record the code.
    let waiter_id = bg_id.clone();
    let mut child = spawn.child;
    tokio::spawn(async move {
        let code = child.wait().await.ok().and_then(|s| s.code());
        background_activity::mark_exited(&waiter_id, code);
    });

    ToolOutput::success(json!({
        "bg_id": bg_id,
        "pid": pid,
        "status": "running",
        "stdout_log": stdout_log.to_string_lossy(),
        "stderr_log": stderr_log.to_string_lossy(),
        "message": format!(
            "Started background process {bg_id} (pid {pid}). It keeps running after this turn. \
             Use background_output to read its output (status \"running\" with no new output does \
             not mean it's ready) and background_kill to stop it."
        ),
    }))
}

/// Waits for adopted background jobs to finish before a one-shot process exits.
///
/// `zdx exec` relocates a slow foreground command instead of killing it, but it
/// holds the job's supervisor lease, so exiting would kill exactly the work the
/// handoff preserved. This waits the job out with the lease held; its reader
/// tasks keep appending to the job's logs throughout.
///
/// It never kills a job on its own and has no deadline, so a legitimately long
/// build is waited out in full. An interrupt during the wait is the operator
/// asking to stop: the jobs are torn down through their leases, so nothing is
/// orphaned. A second Ctrl-C force-exits and the supervisor's own lease cleanup
/// handles the rest.
///
/// Callers must await this **inside** the tokio runtime. Dropping the runtime
/// drops the task that holds the lease, which terminates the job.
pub async fn drain_adopted_jobs() {
    #[cfg(unix)]
    {
        use crate::core::interrupt;

        if zdx_tools::adopted::live_jobs().is_empty() {
            return;
        }

        // The turn was already cancelled, so the operator has asked to stop:
        // tear the jobs down rather than waiting them out.
        if interrupt::is_interrupted() {
            zdx_tools::adopted::terminate_all().await;
            return;
        }

        tokio::select! {
            () = zdx_tools::adopted::drain() => {}
            () = interrupt::wait_for_interrupt() => {
                zdx_tools::adopted::terminate_all().await;
            }
        }
    }
}

/// Best-effort teardown of a spawn that failed to register.
async fn kill_failed_spawn(mut spawn: zdx_tools::bash::BackgroundSpawn, pid: u32) {
    #[cfg(unix)]
    unsafe {
        // setsid made the child its own session/group leader → pgid == pid.
        libc::killpg(pid as i32, libc::SIGKILL);
    }
    #[cfg(not(unix))]
    let _ = pid;
    let _ = spawn.child.wait().await;
}

/// Builds the auto-background handoff for a foreground bash command.
///
/// Returns `None` when the command must keep waiting in the foreground:
/// - the run is on a surface that can neither outlive the job nor drain it
///   (unknown or absent surfaces) — see
///   [`crate::tools::surface_keeps_background_jobs`];
/// - auto-backgrounding is disabled (`bash_foreground_bound_secs = 0`);
/// - the registry directories are unusable.
///
/// The returned `bound` is the configured default; a per-call `timeout_secs`
/// may replace it before the command runs.
///
/// The `bg_id` and log paths are reserved up front but nothing is written until
/// the command actually outruns its bound, so ordinary fast commands leave no
/// trace in the registry.
#[cfg(unix)]
pub fn prepare_handoff(command: &str, ctx: &ToolContext) -> Option<zdx_tools::bash::Handoff> {
    if !ctx.background_handoff {
        return None;
    }
    let bound = ctx.config.as_ref()?.bash_foreground_bound()?;
    background_activity::ensure_dirs().ok()?;

    let bg_id = format!("bg-{}", uuid::Uuid::new_v4());
    let stdout_log = background_activity::stdout_log_path(&bg_id);
    let stderr_log = background_activity::stderr_log_path(&bg_id);

    let adopt_id = bg_id.clone();
    let exit_id = bg_id.clone();
    let command = command.to_string();
    let cwd = ctx.root.to_string_lossy().into_owned();
    let thread_id = ctx.current_thread_id.clone();
    // The command starts now, so uptime stays honest across the handoff.
    let started_at = chrono::Utc::now().to_rfc3339();

    Some(zdx_tools::bash::Handoff {
        bound,
        bg_id,
        stdout_log,
        stderr_log,
        on_adopt: Box::new(move |identity| {
            let rec = BackgroundProcess {
                bg_id: adopt_id,
                pid: identity.pid,
                pgid: identity.pgid,
                // Identity guard for any process that has to fall back to
                // signalling, e.g. the monitor or a later zdx run.
                birth_id: background_activity::capture_identity(identity.pid).0,
                thread_id,
                command,
                cwd,
                started_at,
                mode: BackgroundMode::Adopted,
                exited_at: None,
                exit_code: None,
            };
            let registered = background_activity::write_marker(&rec).is_ok();
            if registered {
                background_activity::log_spawned(&rec);
            }
            registered
        }),
        on_exit: Box::new(move |code| background_activity::mark_exited(&exit_id, code)),
    })
}

/// Loads the record for the request's `bg_id` and enforces thread ownership.
/// On any failure returns the `ToolOutput` the tool should return directly.
fn resolve_owned(input: &Value, ctx: &ToolContext) -> Result<BackgroundProcess, ToolOutput> {
    let bg_id = input.get("bg_id").and_then(Value::as_str).unwrap_or("");
    if bg_id.is_empty() {
        return Err(ToolOutput::failure(
            "invalid_input",
            "bg_id is required",
            None,
        ));
    }
    let Some(rec) = background_activity::get(bg_id) else {
        return Err(ToolOutput::failure(
            "not_found",
            format!("no background process with bg_id {bg_id}"),
            None,
        ));
    };
    if rec.thread_id != ctx.current_thread_id {
        return Err(ToolOutput::failure(
            "not_found",
            format!("background process {bg_id} was not started by this thread"),
            None,
        ));
    }
    Ok(rec)
}

/// How often [`wait_for_change`] re-checks the job while waiting.
///
/// The signals are a log file's length and the registry marker, so there is
/// nothing to subscribe to; this is a poll. 100ms keeps a wait responsive
/// without making an idle wait expensive.
const WAIT_POLL_INTERVAL: Duration = Duration::from_millis(100);

/// Upper bound applied to `wait_secs` when auto-backgrounding is disabled and
/// there is therefore no configured foreground bound to clamp to.
const WAIT_CLAMP_FALLBACK: Duration = Duration::from_mins(2);

/// Why a `background_output` wait ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Waited {
    /// New bytes were appended to the job's logs.
    Output,
    /// The job exited.
    Exit,
    /// `wait_secs` elapsed with the job still running and quiet.
    Timeout,
}

impl Waited {
    fn as_str(self) -> &'static str {
        match self {
            Self::Output => "output",
            Self::Exit => "exit",
            Self::Timeout => "timeout",
        }
    }
}

/// Clamps a requested `wait_secs` to the foreground bound.
///
/// A wait is a blocked turn by another name, so it must never be allowed to
/// exceed the bound that exists precisely to stop turns blocking. When
/// auto-backgrounding is disabled there is no configured bound, so the default
/// (120s) still applies rather than letting the wait run unbounded.
fn clamp_wait(requested: u64, ctx: &ToolContext) -> Duration {
    let ceiling = ctx
        .config
        .as_ref()
        .and_then(crate::config::Config::bash_foreground_bound)
        .unwrap_or(WAIT_CLAMP_FALLBACK);
    Duration::from_secs(requested).min(ceiling)
}

/// Blocks until the job produces new output, exits, or `budget` elapses.
///
/// Purely an observer: it reads the job's log lengths and its registry marker
/// and never signals the process, so abandoning the wait — by cancellation or
/// by the budget running out — leaves the job running exactly as it was.
///
/// Works for both adopted and detached jobs: both record their exit in the
/// marker (adopted via the drain-owned waiter, detached via the spawn waiter).
async fn wait_for_change(rec: &BackgroundProcess, budget: Duration, ctx: &ToolContext) -> Waited {
    let stdout_log = background_activity::stdout_log_path(&rec.bg_id);
    let stderr_log = background_activity::stderr_log_path(&rec.bg_id);
    let baseline =
        background_activity::log_len(&stdout_log) + background_activity::log_len(&stderr_log);

    let deadline = tokio::time::Instant::now() + budget;
    let poll = async {
        loop {
            // Re-read the marker: an exit recorded by the owning waiter is the
            // authoritative "this job is done" signal for both job kinds.
            if background_activity::get(&rec.bg_id).is_none_or(|current| !current.is_running()) {
                return Waited::Exit;
            }
            let current = background_activity::log_len(&stdout_log)
                + background_activity::log_len(&stderr_log);
            if current != baseline {
                return Waited::Output;
            }
            tokio::time::sleep(WAIT_POLL_INTERVAL).await;
        }
    };

    tokio::select! {
        outcome = poll => outcome,
        () = tokio::time::sleep_until(deadline) => Waited::Timeout,
        // The turn was interrupted: stop waiting promptly and report what is
        // true right now. The job itself is untouched.
        () = cancelled_or_pending(ctx.cancel_token.as_ref()) => Waited::Timeout,
    }
}

/// Resolves when `cancel` is cancelled, or never when there is no token.
async fn cancelled_or_pending(cancel: Option<&tokio_util::sync::CancellationToken>) {
    match cancel {
        Some(token) => token.cancelled().await,
        None => std::future::pending().await,
    }
}

pub struct BackgroundOutput;

impl Tool for BackgroundOutput {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "background_output".to_string(),
            description:
                "Read the recent output (stdout + stderr tail) and status of a background \
                process started with the Bash tool's background: true, or of a foreground \
                command that was moved to the background after exceeding its foreground \
                bound. Status \"running\" with no new output does NOT mean the process is done \
                or ready — check the status field. To follow a job, prefer waiting on it with \
                wait_secs over re-reading it in a loop or sleeping between reads. Polling the \
                same bg_id repeatedly is expected and allowed; each read returns a new read_seq."
                    .to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "bg_id": {"type": "string", "description": "The bg_id returned when the process was started or backgrounded."},
                    "wait_secs": {
                        "type": "integer",
                        "minimum": 0,
                        "description": "Optional. Block until the job writes new output, exits, or this many seconds pass, whichever comes first, then return as usual plus waited_for (\"output\", \"exit\", or \"timeout\"). Clamped to the foreground bound (120s by default), so a longer value silently waits only that long. Waiting never affects the job: it keeps running whatever the wait returns. Omit it to read what is available right now."
                    }
                },
                "required": ["bg_id"],
                "additionalProperties": false
            }),
        }
    }

    fn execute(&self, input: &Value, ctx: &ToolContext) -> ToolFuture {
        let input = input.clone();
        let ctx = ctx.clone();
        Box::pin(async move {
            let rec = match resolve_owned(&input, &ctx) {
                Ok(rec) => rec,
                Err(out) => return out,
            };

            let requested = match wait_secs_of(&input) {
                Ok(secs) => secs,
                Err(out) => return out,
            };

            // Only wait on a job that could still change.
            let waited = match requested {
                Some(secs) if secs > 0 && rec.is_running() => {
                    Some(wait_for_change(&rec, clamp_wait(secs, &ctx), &ctx).await)
                }
                Some(_) if !rec.is_running() => Some(Waited::Exit),
                _ => None,
            };

            // Re-read after waiting so status, exit code and uptime describe the
            // job as of the moment this call returns, not when it started.
            let rec = background_activity::get(&rec.bg_id).unwrap_or(rec);
            let status = if rec.is_running() {
                "running"
            } else {
                "exited"
            };
            let mut data = json!({
                "bg_id": rec.bg_id,
                "pid": rec.pid,
                "status": status,
                "exit_code": rec.exit_code,
                "uptime": rec.uptime(),
                "read_seq": READ_SEQ.fetch_add(1, Ordering::Relaxed) + 1,
                "stdout": background_activity::read_log_tail(&background_activity::stdout_log_path(&rec.bg_id), OUTPUT_TAIL_BYTES),
                "stderr": background_activity::read_log_tail(&background_activity::stderr_log_path(&rec.bg_id), OUTPUT_TAIL_BYTES),
            });
            if let Some(waited) = waited {
                data["waited_for"] = json!(waited.as_str());
            }
            ToolOutput::success(data)
        })
    }
}

/// Reads the optional `wait_secs`, accepting the string forms models emit.
fn wait_secs_of(input: &Value) -> Result<Option<u64>, ToolOutput> {
    match input.get("wait_secs") {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Number(number)) => number.as_u64().map(Some).ok_or_else(|| {
            ToolOutput::failure("invalid_input", "wait_secs must be a whole number", None)
        }),
        Some(Value::String(raw)) => match raw.trim() {
            "" => Ok(None),
            trimmed => trimmed.parse::<u64>().map(Some).map_err(|err| {
                ToolOutput::failure(
                    "invalid_input",
                    "wait_secs must be a whole number",
                    Some(err.to_string()),
                )
            }),
        },
        Some(_) => Err(ToolOutput::failure(
            "invalid_input",
            "wait_secs must be a whole number",
            None,
        )),
    }
}

pub struct BackgroundKill;

impl Tool for BackgroundKill {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "background_kill".to_string(),
            description:
                "Stop a background process started with the Bash tool's background: true, or a \
                foreground command that was moved to the background, by its bg_id."
                    .to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "bg_id": {"type": "string", "description": "The bg_id of the process to stop."}
                },
                "required": ["bg_id"],
                "additionalProperties": false
            }),
        }
    }

    fn execute(&self, input: &Value, ctx: &ToolContext) -> ToolFuture {
        let input = input.clone();
        let ctx = ctx.clone();
        Box::pin(async move {
            let rec = match resolve_owned(&input, &ctx) {
                Ok(rec) => rec,
                Err(out) => return out,
            };
            let bg_id = rec.bg_id.as_str();

            let (status, message) = match background_activity::kill_background(bg_id).await {
                KillOutcome::Killed => ("killed", format!("Stopped background process {bg_id}.")),
                KillOutcome::AlreadyExited => (
                    "already_exited",
                    format!("Background process {bg_id} had already exited."),
                ),
                KillOutcome::NotFound => {
                    return ToolOutput::failure(
                        "not_found",
                        format!("no background process with bg_id {bg_id}"),
                        None,
                    );
                }
                KillOutcome::Unverifiable => {
                    return ToolOutput::failure(
                        "unverifiable",
                        format!(
                            "refusing to kill {bg_id}: its process identity could not be verified \
                             (it may have exited and the PID been reused)"
                        ),
                        None,
                    );
                }
            };
            ToolOutput::success(json!({ "bg_id": bg_id, "status": status, "message": message }))
        })
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::BackgroundInput;

    fn timeout_of(value: &serde_json::Value) -> Option<u64> {
        serde_json::from_value::<BackgroundInput>(
            json!({ "command": "x", "background": true, "timeout_secs": value }),
        )
        .expect("input should parse")
        .timeout_secs
    }

    /// `background: true` with a zero timeout must not be rejected: models send
    /// `0` (or `"0"`) to mean "no timeout", which is what background already is.
    #[test]
    fn zero_timeout_is_accepted_with_background() {
        for zero in [json!(0), json!("0"), json!(" 0 "), json!(""), json!(null)] {
            let secs = timeout_of(&zero);
            assert!(
                secs.is_none_or(|secs| secs == 0),
                "expected {zero:?} to be treated as no timeout, got {secs:?}"
            );
        }
        let absent: BackgroundInput =
            serde_json::from_value(json!({ "command": "x", "background": true })).unwrap();
        assert_eq!(absent.timeout_secs, None);
    }

    #[test]
    fn positive_timeout_still_conflicts_with_background() {
        for value in [json!(120), json!("120")] {
            let secs = timeout_of(&value);
            assert!(
                secs.is_some_and(|secs| secs > 0),
                "expected {value:?} to conflict with background, got {secs:?}"
            );
        }
    }

    /// Polling an unchanged job must not produce identical results, or the
    /// turn's identical-tool-call detector aborts legitimate polling as a loop.
    #[test]
    fn each_background_output_read_is_distinct() {
        use std::sync::atomic::Ordering;

        use super::READ_SEQ;

        let first = READ_SEQ.fetch_add(1, Ordering::Relaxed);
        let second = READ_SEQ.fetch_add(1, Ordering::Relaxed);
        assert!(second > first);
    }
}

/// End-to-end over the real registry: a foreground command that outruns its
/// bound is registered as an adopted job and killed through its lease.
#[cfg(all(test, unix))]
mod handoff_registry_tests {
    use std::time::Duration;

    use serde_json::json;

    use super::*;
    use crate::background_activity::{self, BackgroundMode, KillOutcome};
    use crate::config::Config;

    fn process_exists(pid: u32) -> bool {
        if unsafe { libc::kill(pid as i32, 0) } == 0 {
            return true;
        }
        std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
    }

    #[tokio::test]
    async fn backgrounded_command_is_registered_adopted_and_killable() {
        let home = crate::test_support::temp_zdx_home();
        let root = home.path().join("work");
        std::fs::create_dir_all(&root).unwrap();

        let config = Config {
            bash_foreground_bound_secs: 1,
            ..Config::default()
        };
        let mut ctx = ToolContext::new(root.clone(), None);
        ctx.background_handoff = true;
        ctx.current_thread_id = Some("thread-handoff".to_string());
        ctx.config = Some(config);

        let command = "echo warming; sleep 60";
        let handoff = prepare_handoff(command, &ctx).expect("bound is configured");
        let leaf = ctx.as_leaf();
        let result = zdx_tools::bash::execute(
            &json!({ "command": command }),
            &leaf,
            None,
            None,
            Some(handoff),
        )
        .await;

        let data = result.data().expect("should have data");
        assert_eq!(data["backgrounded"], true);
        let bg_id = data["bg_id"].as_str().unwrap().to_string();
        let pid = u32::try_from(data["pid"].as_u64().unwrap()).unwrap();

        let rec = background_activity::get(&bg_id).expect("adopted job is registered");
        assert_eq!(rec.mode, BackgroundMode::Adopted);
        assert_eq!(rec.pid, pid);
        assert_eq!(rec.command, command);
        assert_eq!(rec.thread_id.as_deref(), Some("thread-handoff"));
        assert!(rec.is_running());
        assert!(process_exists(pid));

        // Pre-handoff output reached the registry log, so a poll can read it.
        let logged = background_activity::read_log_tail(
            &background_activity::stdout_log_path(&bg_id),
            8 * 1024,
        );
        assert!(logged.contains("warming"), "log was: {logged}");

        assert_eq!(
            background_activity::kill_background(&bg_id).await,
            KillOutcome::Killed
        );
        tokio::time::timeout(Duration::from_secs(5), async {
            while process_exists(pid) {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("adopted job survived background_kill");

        assert!(!background_activity::get(&bg_id).unwrap().is_running());
    }

    /// A surface that neither outlives its runs nor drains must keep the
    /// unbounded foreground wait: adopting there would close the lease at exit
    /// and kill the command precisely because it was slow.
    #[tokio::test]
    async fn unsupported_surfaces_never_relocate() {
        let home = crate::test_support::temp_zdx_home();
        let config = Config {
            bash_foreground_bound_secs: 1,
            ..Config::default()
        };

        for surface in [Some("unknown-surface"), None] {
            let mut ctx = ToolContext::new(home.path().to_path_buf(), None);
            ctx.config = Some(config.clone());
            ctx.background_handoff = crate::tools::surface_keeps_background_jobs(surface);

            assert!(
                prepare_handoff("sleep 1", &ctx).is_none(),
                "surface {surface:?} cannot keep a job alive and must keep waiting"
            );
        }
    }

    /// Every real surface relocates, by one of two mechanisms: `chat` and
    /// `telegram` outlive the run, while `exec` drains adopted jobs before the
    /// process exits. `exec` is also how subagents and orchestrator workers
    /// run, which is the case the handoff most needs to cover.
    #[test]
    fn supported_surfaces_keep_background_jobs() {
        use crate::tools::surface_keeps_background_jobs as keeps;

        assert!(keeps(Some("chat")), "interactive TUI outlives the run");
        assert!(keeps(Some("telegram")), "bot daemon outlives the run");
        assert!(keeps(Some("exec")), "exec drains before exiting");
        assert!(!keeps(Some("unknown-surface")));
        assert!(!keeps(None));
    }

    #[tokio::test]
    async fn zero_bound_disables_auto_backgrounding() {
        let home = crate::test_support::temp_zdx_home();
        let config = Config {
            bash_foreground_bound_secs: 0,
            ..Config::default()
        };
        let mut ctx = ToolContext::new(home.path().to_path_buf(), None);
        ctx.background_handoff = true;
        ctx.config = Some(config);

        assert!(prepare_handoff("sleep 1", &ctx).is_none());
    }

    /// `background: true` keeps its own semantics: detached at spawn, never
    /// adopted, and unaffected by the foreground bound.
    #[tokio::test]
    async fn explicit_background_is_still_detached() {
        let home = crate::test_support::temp_zdx_home();
        let root = home.path().join("work");
        std::fs::create_dir_all(&root).unwrap();

        let config = Config {
            bash_foreground_bound_secs: 1,
            ..Config::default()
        };
        let mut ctx = ToolContext::new(root, None);
        ctx.background_handoff = true;
        ctx.config = Some(config);

        let result =
            run_background(&json!({ "command": "sleep 60", "background": true }), &ctx).await;
        let data = result.data().expect("should have data");
        let bg_id = data["bg_id"].as_str().unwrap().to_string();

        let rec = background_activity::get(&bg_id).expect("registered");
        assert_eq!(rec.mode, BackgroundMode::Detached);
        assert!(
            !zdx_tools::adopted::is_adopted(&bg_id),
            "a detached process must not hold an invocation lease"
        );

        assert_eq!(
            background_activity::kill_background(&bg_id).await,
            KillOutcome::Killed
        );
    }
}

/// `wait_secs` on `background_output`: a bounded wait on a real detached job,
/// which is the path a model uses to follow a job instead of busy-polling.
#[cfg(all(test, unix))]
mod wait_tests {
    use std::time::{Duration, Instant};

    use serde_json::json;
    use tokio_util::sync::CancellationToken;

    use super::*;
    use crate::background_activity;
    use crate::config::Config;

    fn process_exists(pid: u32) -> bool {
        if unsafe { libc::kill(pid as i32, 0) } == 0 {
            return true;
        }
        std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
    }

    /// Starts a detached job and returns `(ctx, bg_id, pid)`.
    async fn start_job(home: &std::path::Path, command: &str) -> (ToolContext, String, u32) {
        let root = home.join("work");
        std::fs::create_dir_all(&root).unwrap();
        let mut ctx = ToolContext::new(root, None);
        ctx.current_thread_id = Some("thread-wait".to_string());
        ctx.config = Some(Config::default());

        let result = run_background(&json!({ "command": command, "background": true }), &ctx).await;
        let data = result.data().expect("spawned");
        let bg_id = data["bg_id"].as_str().unwrap().to_string();
        let pid = u32::try_from(data["pid"].as_u64().unwrap()).unwrap();
        (ctx, bg_id, pid)
    }

    async fn read_with_wait(ctx: &ToolContext, bg_id: &str, wait_secs: u64) -> serde_json::Value {
        let out = BackgroundOutput
            .execute(&json!({ "bg_id": bg_id, "wait_secs": wait_secs }), ctx)
            .await;
        out.data().expect("should have data").clone()
    }

    /// Returns as soon as the job appends output, well before the budget.
    #[tokio::test]
    async fn wait_returns_on_new_output() {
        let home = crate::test_support::temp_zdx_home();
        let (ctx, bg_id, pid) = start_job(home.path(), "sleep 1; echo appeared; sleep 30").await;

        let started = Instant::now();
        let data = read_with_wait(&ctx, &bg_id, 20).await;

        assert_eq!(data["waited_for"], "output");
        assert_eq!(data["status"], "running");
        assert!(data["stdout"].as_str().unwrap().contains("appeared"));
        assert!(
            started.elapsed() < Duration::from_secs(15),
            "wait should return on output, not ride out the budget: {:?}",
            started.elapsed()
        );
        assert!(process_exists(pid), "waiting must not disturb the job");

        background_activity::kill_background(&bg_id).await;
    }

    /// Returns when the job exits, reporting the exit in the same envelope.
    #[tokio::test]
    async fn wait_returns_on_exit() {
        let home = crate::test_support::temp_zdx_home();
        let (ctx, bg_id, _) = start_job(home.path(), "sleep 1; exit 7").await;

        let started = Instant::now();
        let data = read_with_wait(&ctx, &bg_id, 20).await;

        assert_eq!(data["waited_for"], "exit");
        assert_eq!(data["status"], "exited");
        assert_eq!(data["exit_code"], 7);
        assert!(started.elapsed() < Duration::from_secs(15));
    }

    /// A quiet, still-running job rides out the budget and says so.
    #[tokio::test]
    async fn wait_returns_on_timeout_without_disturbing_the_job() {
        let home = crate::test_support::temp_zdx_home();
        let (ctx, bg_id, pid) = start_job(home.path(), "sleep 30").await;

        let started = Instant::now();
        let data = read_with_wait(&ctx, &bg_id, 1).await;

        assert_eq!(data["waited_for"], "timeout");
        assert_eq!(data["status"], "running");
        assert!(
            started.elapsed() >= Duration::from_secs(1),
            "timeout must actually wait its budget"
        );
        assert!(
            process_exists(pid),
            "a timed-out wait must leave the job up"
        );

        background_activity::kill_background(&bg_id).await;
    }

    /// Cancelling the turn ends the wait promptly and leaves the job running:
    /// the wait is an observer, never a control.
    #[tokio::test]
    async fn cancellation_ends_the_wait_but_not_the_job() {
        let home = crate::test_support::temp_zdx_home();
        let (mut ctx, bg_id, pid) = start_job(home.path(), "sleep 30").await;

        let cancel = CancellationToken::new();
        ctx.cancel_token = Some(cancel.clone());

        let started = Instant::now();
        let waiting = {
            let ctx = ctx.clone();
            let bg_id = bg_id.clone();
            tokio::spawn(async move { read_with_wait(&ctx, &bg_id, 120).await })
        };

        tokio::time::sleep(Duration::from_millis(300)).await;
        cancel.cancel();

        let data = tokio::time::timeout(Duration::from_secs(10), waiting)
            .await
            .expect("cancelled wait did not return promptly")
            .unwrap();

        assert_eq!(data["waited_for"], "timeout");
        assert!(
            started.elapsed() < Duration::from_secs(30),
            "cancellation must not ride out the clamped budget"
        );
        assert!(
            process_exists(pid),
            "cancelling a wait must never kill the job"
        );

        background_activity::kill_background(&bg_id).await;
    }

    /// `wait_secs` is clamped to the foreground bound, so a wait can never
    /// recreate the blocked-turn problem the bound exists to prevent.
    #[test]
    fn wait_is_clamped_to_the_foreground_bound() {
        let mut ctx = ToolContext::new(std::path::PathBuf::from("."), None);

        ctx.config = Some(Config {
            bash_foreground_bound_secs: 30,
            ..Config::default()
        });
        assert_eq!(clamp_wait(5, &ctx), Duration::from_secs(5));
        assert_eq!(clamp_wait(3_600, &ctx), Duration::from_secs(30));

        // Auto-backgrounding disabled: the default ceiling still applies rather
        // than letting a wait run unbounded.
        ctx.config = Some(Config {
            bash_foreground_bound_secs: 0,
            ..Config::default()
        });
        assert_eq!(clamp_wait(3_600, &ctx), WAIT_CLAMP_FALLBACK);

        // No config at all (leaf/tool-only contexts) behaves the same way.
        ctx.config = None;
        assert_eq!(clamp_wait(3_600, &ctx), WAIT_CLAMP_FALLBACK);
    }

    /// Omitting `wait_secs` keeps today's read-now behavior, with no new field.
    #[tokio::test]
    async fn omitting_wait_secs_reads_immediately() {
        let home = crate::test_support::temp_zdx_home();
        let (ctx, bg_id, pid) = start_job(home.path(), "sleep 30").await;

        let started = Instant::now();
        let out = BackgroundOutput
            .execute(&json!({ "bg_id": bg_id }), &ctx)
            .await;
        let data = out.data().expect("should have data");

        assert!(data.get("waited_for").is_none());
        assert_eq!(data["status"], "running");
        assert!(started.elapsed() < Duration::from_secs(2));

        let _ = pid;
        background_activity::kill_background(&bg_id).await;
    }

    /// The same wait works for an *adopted* job — a foreground command moved to
    /// the background — which is the kind the handoff creates. Its exit is
    /// recorded by the drain-owned waiter rather than the spawn waiter, so the
    /// exit signal is worth pinning separately.
    #[tokio::test]
    async fn wait_works_for_an_adopted_job() {
        let home = crate::test_support::temp_zdx_home();
        let root = home.path().join("work");
        std::fs::create_dir_all(&root).unwrap();

        let mut ctx = ToolContext::new(root, None);
        ctx.current_thread_id = Some("thread-wait-adopted".to_string());
        ctx.config = Some(Config {
            bash_foreground_bound_secs: 1,
            ..Config::default()
        });
        ctx.background_handoff = true;

        // Outruns the 1s bound, then writes again and exits.
        let command = "echo early; sleep 2; echo later; exit 4";
        let handoff = prepare_handoff(command, &ctx).expect("bound is configured");
        let leaf = ctx.as_leaf();
        let backgrounded = zdx_tools::bash::execute(
            &json!({ "command": command }),
            &leaf,
            None,
            None,
            Some(handoff),
        )
        .await;
        let data = backgrounded.data().expect("should have data");
        assert_eq!(data["backgrounded"], true);
        let bg_id = data["bg_id"].as_str().unwrap().to_string();

        // Read with a realistic bound: the 1s bound above exists only to force
        // a fast handoff, and it would otherwise clamp every wait to 1s.
        let mut reader = ctx.clone();
        reader.config = Some(Config::default());

        // Waiting on the adopted job sees it through to its real exit code.
        let data = read_with_wait(&reader, &bg_id, 30).await;
        assert!(
            data["waited_for"] == "output" || data["waited_for"] == "exit",
            "unexpected wait outcome: {}",
            data["waited_for"]
        );

        let final_read = read_with_wait(&reader, &bg_id, 30).await;
        assert_eq!(final_read["status"], "exited");
        assert_eq!(final_read["exit_code"], 4);
        assert!(final_read["stdout"].as_str().unwrap().contains("later"));
    }

    /// Every read stamps a new `read_seq`, including one that waited, so
    /// following a job can never look like a repeated identical tool call.
    #[tokio::test]
    async fn waited_reads_still_advance_read_seq() {
        let home = crate::test_support::temp_zdx_home();
        let (ctx, bg_id, _) = start_job(home.path(), "sleep 30").await;

        let first = read_with_wait(&ctx, &bg_id, 1).await;
        let second = read_with_wait(&ctx, &bg_id, 1).await;

        assert_eq!(first["waited_for"], "timeout");
        assert!(second["read_seq"].as_u64().unwrap() > first["read_seq"].as_u64().unwrap());

        background_activity::kill_background(&bg_id).await;
    }
}
