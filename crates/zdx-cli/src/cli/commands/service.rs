//! `zdx service` — launchd control for the long-lived `bot` and `daemon`
//! services; thin wrapper over `zdx_engine::service`.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow};
use zdx_engine::agent_activity;
use zdx_engine::service::{self, Service};

/// `zdx service install [--program PATH]` — write and bootstrap the launchd agents.
///
/// # Errors
/// Returns an error if the target is unknown or launchd control fails.
pub fn install(target: &str, root: &Path, program: Option<PathBuf>) -> Result<()> {
    let program = program.unwrap_or_else(service::default_program);
    for svc in Service::parse_target(target)? {
        println!("{}", service::install(svc, &program, root)?);
    }
    println!("Logs: {}", service::Service::Bot.log_path(true).display());
    Ok(())
}

/// `zdx service uninstall` — boot the agents out and remove their plists.
///
/// # Errors
/// Returns an error if the target is unknown or a plist cannot be removed.
pub fn uninstall(target: &str) -> Result<()> {
    for svc in Service::parse_target(target)? {
        println!("{}", service::uninstall(svc)?);
    }
    Ok(())
}

/// `zdx service start <target>`.
///
/// # Errors
/// Returns an error if the target is unknown or launchd control fails.
pub fn start(target: &str) -> Result<()> {
    for svc in Service::parse_target(target)? {
        println!("{}", service::start(svc)?);
    }
    Ok(())
}

/// `zdx service stop <target>`.
///
/// # Errors
/// Returns an error if the target is unknown or launchd control fails.
pub fn stop(target: &str) -> Result<()> {
    for svc in Service::parse_target(target)? {
        println!("{}", service::stop(svc)?);
    }
    Ok(())
}

/// `zdx service restart <target> [--force]`.
///
/// # Errors
/// Returns an error if the target is unknown, active agent work blocks the
/// restart, or launchd control fails.
pub fn restart(target: &str, force: bool) -> Result<()> {
    let services = Service::parse_target(target)?;
    let statuses = service::restart_many(&services, force).map_err(|err| {
        if err.downcast_ref::<service::RestartBlocked>().is_some() {
            anyhow!(
                "{err}; queue it with `--queue`, wait for active work to finish, or retry with `--force` to interrupt it"
            )
        } else {
            err
        }
    })?;
    for status in statuses {
        println!("{status}");
    }
    Ok(())
}

/// `zdx service restart <target> --queue` — hand the restart to the bot,
/// which performs it once no agent run is active.
///
/// # Errors
/// Returns an error if the target is unknown, the bot service is not running
/// under a supervisor, or the request cannot be written.
pub fn queue_restart(target: &str) -> Result<()> {
    let services = Service::parse_target(target)?;
    // Lets the bot announce the restart in the Telegram topic that asked.
    let thread_id = std::env::var("ZDX_THREAD_ID")
        .ok()
        .filter(|id| !id.trim().is_empty());
    let outcome = service::queue_restart(&services, thread_id.as_deref())?;
    let active = agent_activity::list_active().len();
    let names = service_names(&outcome.request.services);
    if outcome.already_pending {
        println!("A restart was already queued; it now covers {names}.");
    } else {
        println!("Restart queued for {names}.");
    }
    println!(
        "The bot performs it as soon as no agent run is active ({active} active now). Check with `zdx service status`."
    );
    Ok(())
}

fn service_names(services: &[Service]) -> String {
    services
        .iter()
        .map(|s| s.name())
        .collect::<Vec<_>>()
        .join(", ")
}

/// `zdx service status [--json]`.
///
/// # Errors
/// Returns an error only if JSON serialization fails.
pub fn status(json: bool) -> Result<()> {
    let states: Vec<_> = Service::ALL.into_iter().map(service::state).collect();
    // An unreadable request is reported as none here; the bot drops it on its next check.
    let queued = service::queued_restart()
        .ok()
        .flatten()
        .filter(|request| !request.is_fulfilled());

    if json {
        let items: Vec<serde_json::Value> = states
            .iter()
            .map(|s| {
                serde_json::json!({
                    "service": s.service.name(),
                    "label": s.service.label(),
                    "installed": s.installed,
                    "status": if s.running() { "running" } else { "stopped" },
                    "pid": s.pid,
                    "uptime_secs": s.uptime.map(|d| d.as_secs()),
                })
            })
            .collect();
        println!(
            "{}",
            serde_json::to_string_pretty(
                &serde_json::json!({ "services": items, "queued_restart": queued })
            )?
        );
        return Ok(());
    }

    println!("Services");
    let name_width = Service::name_width();
    for s in &states {
        let launchd = if s.installed {
            "launchd"
        } else {
            "not installed"
        };
        let detail = match (s.pid, s.uptime) {
            (Some(pid), Some(up)) => {
                format!("running · PID {pid} · up {}", service::format_uptime(up))
            }
            (Some(pid), None) => format!("running · PID {pid}"),
            (None, _) => "stopped".to_string(),
        };
        println!("  {:<name_width$} {detail}  [{launchd}]", s.service.name());
    }
    if let Some(request) = queued {
        println!(
            "Queued restart: {} · requested {} · waits for agent runs to finish",
            service_names(&request.services),
            request
                .requested_at
                .with_timezone(&chrono::Local)
                .format("%Y-%m-%d %H:%M:%S")
        );
    }
    Ok(())
}

/// `zdx service logs <target> [--lines N] [--err]`.
///
/// # Errors
/// Returns an error if the target is unknown or a log file cannot be read.
pub fn logs(target: &str, lines: usize, err: bool) -> Result<()> {
    for svc in Service::parse_target(target)? {
        let path = svc.log_path(err);
        println!("── {} ──", path.display());
        if !path.is_file() {
            println!("(no log yet)");
            continue;
        }
        let content =
            std::fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
        let tail: Vec<&str> = content.lines().rev().take(lines).collect();
        for line in tail.into_iter().rev() {
            println!("{line}");
        }
    }
    Ok(())
}
