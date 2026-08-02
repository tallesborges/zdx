//! `zdx service` — launchd control for the long-lived `bot` and `daemon`
//! services; thin wrapper over `zdx_engine::service`.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
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

/// `zdx service restart <target>`.
///
/// # Errors
/// Returns an error if the target is unknown or launchd control fails.
pub fn restart(target: &str) -> Result<()> {
    for svc in Service::parse_target(target)? {
        println!("{}", service::restart(svc)?);
    }
    Ok(())
}

/// `zdx service status [--json]`.
///
/// # Errors
/// Returns an error only if JSON serialization fails.
pub fn status(json: bool) -> Result<()> {
    let states: Vec<_> = Service::ALL.into_iter().map(service::state).collect();

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
            serde_json::to_string_pretty(&serde_json::json!({ "services": items }))?
        );
        return Ok(());
    }

    println!("Services");
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
        println!("  {:<8} {detail}  [{launchd}]", s.service.name());
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
