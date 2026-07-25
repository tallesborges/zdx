//! `zdx bg` — list and kill background processes started with the Bash tool's
//! `background: true` (see `zdx_engine::background_activity`).

use anyhow::Result;
use zdx_engine::background_activity::{self, KillOutcome};

/// `zdx bg list` — show current background processes (running + recently exited).
///
/// # Errors
/// Returns an error only if JSON serialization fails.
pub fn list(json: bool) -> Result<()> {
    let procs = background_activity::list_background();

    if json {
        let items: Vec<serde_json::Value> = procs
            .iter()
            .map(|p| {
                serde_json::json!({
                    "bg_id": p.bg_id,
                    "pid": p.pid,
                    "status": if p.is_running() { "running" } else { "exited" },
                    "exit_code": p.exit_code,
                    "thread_id": p.thread_id,
                    "command": p.command,
                    "cwd": p.cwd,
                    "uptime": p.uptime(),
                })
            })
            .collect();
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({ "processes": items }))?
        );
        return Ok(());
    }

    if procs.is_empty() {
        println!("No background processes.");
        return Ok(());
    }

    println!("Background processes ({})", procs.len());
    for p in &procs {
        let status = if p.is_running() {
            format!("running · {}", p.uptime())
        } else {
            p.exit_code
                .map_or_else(|| "exited".to_string(), |c| format!("exited ({c})"))
        };
        let thread = p.thread_id.as_deref().unwrap_or("-");
        println!("  {}  pid {:<7} [{status}]", p.bg_id, p.pid);
        println!("      thread {thread}  ·  {}", p.command);
    }
    Ok(())
}

/// `zdx bg kill <bg_id>` — stop a background process.
///
/// # Errors
/// Never returns an error; prints the outcome.
pub async fn kill(bg_id: &str) -> Result<()> {
    match background_activity::kill_background(bg_id).await {
        KillOutcome::Killed => println!("Stopped {bg_id}."),
        KillOutcome::AlreadyExited => println!("{bg_id} had already exited."),
        KillOutcome::NotFound => println!("No background process with bg_id {bg_id}."),
        KillOutcome::Unverifiable => {
            println!("Refusing to kill {bg_id}: process identity could not be verified.");
        }
    }
    Ok(())
}
