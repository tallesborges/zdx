//! Background-process tools + spawn/registration used by the Bash tool.
//!
//! - [`run_background`] is invoked by the `Bash` tool when `background: true`:
//!   it spawns a detached process (via [`zdx_tools::bash::spawn_background`]),
//!   registers it in [`crate::background_activity`], starts a reaping waiter,
//!   and returns a `bg_id`.
//! - [`BackgroundOutput`] and [`BackgroundKill`] are agent tools that read a
//!   background process's output / stop it, scoped to the caller's thread.

use serde::Deserialize;
use serde_json::{Value, json};

use super::{Tool, ToolContext, ToolDefinition, ToolFuture};
use crate::background_activity::{self, BackgroundProcess, KillOutcome};
use crate::core::events::ToolOutput;

/// Max bytes returned per stream by `background_output`.
const OUTPUT_TAIL_BYTES: usize = 8 * 1024;

/// Background-mode subset of the `Bash` tool input.
///
/// `timeout_secs` reuses the Bash tool's coercion so `0` and `"0"` mean the
/// same thing here: no timeout, which is what a background process already is.
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
    // A background process is never awaited, so only "no timeout" is coherent.
    if input.timeout_secs.is_some_and(|secs| secs > 0) {
        return ToolOutput::failure(
            "invalid_input",
            "timeout_secs must be omitted or 0 with background: true (a background process is \
             not awaited and never times out)",
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

fn read_tail(path: &std::path::Path, max_bytes: usize) -> String {
    let Ok(bytes) = std::fs::read(path) else {
        return String::new();
    };
    let start = bytes.len().saturating_sub(max_bytes);
    String::from_utf8_lossy(&bytes[start..]).into_owned()
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

pub struct BackgroundOutput;

impl Tool for BackgroundOutput {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "background_output".to_string(),
            description:
                "Read the recent output (stdout + stderr tail) and status of a background \
                process started with the Bash tool's background: true. Status \"running\" with no \
                new output does NOT mean the process is done or ready — check the status field."
                    .to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "bg_id": {"type": "string", "description": "The bg_id returned when the process was started."}
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

            let status = if rec.is_running() {
                "running"
            } else {
                "exited"
            };
            ToolOutput::success(json!({
                "bg_id": rec.bg_id,
                "pid": rec.pid,
                "status": status,
                "exit_code": rec.exit_code,
                "uptime": rec.uptime(),
                "stdout": read_tail(&background_activity::stdout_log_path(&rec.bg_id), OUTPUT_TAIL_BYTES),
                "stderr": read_tail(&background_activity::stderr_log_path(&rec.bg_id), OUTPUT_TAIL_BYTES),
            }))
        })
    }
}

pub struct BackgroundKill;

impl Tool for BackgroundKill {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "background_kill".to_string(),
            description:
                "Stop a background process started with the Bash tool's background: true, \
                by its bg_id."
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
                !secs.is_some_and(|secs| secs > 0),
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
}
