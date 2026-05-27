//! Lightweight PID file management for service status.

use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result};

use crate::config::paths;

/// Ensure no other instance of the named service is already running.
///
/// Checks the PID file; if it exists and the process is alive, returns an error.
/// Stale PID files (dead process) are cleaned up automatically.
///
/// # Errors
///
/// Returns an error if another instance is already running.
pub fn ensure_unique(name: &str) -> Result<()> {
    match status(name) {
        ServiceStatus::Running { pid, .. } => {
            anyhow::bail!(
                "{name} is already running (PID {pid}). Stop the existing process first."
            );
        }
        ServiceStatus::Stopped => Ok(()),
    }
}

/// Write a PID file for the given service name.
/// Creates `~/.zdx/run/{name}.pid` with the current process PID.
///
/// # Errors
///
/// Returns an error if the run directory cannot be created or the PID file cannot be written.
pub fn write(name: &str) -> Result<PidGuard> {
    let path = pid_path(name);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("create run dir {}", parent.display()))?;
    }
    let pid = std::process::id();
    fs::write(&path, pid.to_string())
        .with_context(|| format!("write PID file {}", path.display()))?;
    Ok(PidGuard { path })
}

/// Read a service's PID file and check if the process is alive.
pub fn status(name: &str) -> ServiceStatus {
    let path = pid_path(name);
    let Ok(content) = fs::read_to_string(&path) else {
        return ServiceStatus::Stopped;
    };
    let Ok(pid) = content.trim().parse::<u32>() else {
        return ServiceStatus::Stopped;
    };
    if is_alive(pid) {
        let mtime = fs::metadata(&path).and_then(|m| m.modified()).ok();
        ServiceStatus::Running {
            pid,
            started: mtime,
        }
    } else {
        // Stale PID file — clean up
        let _ = fs::remove_file(&path);
        ServiceStatus::Stopped
    }
}

/// Sends a graceful termination signal to a running named service.
///
/// Returns the PID when a running service was signaled, or `None` when stopped.
///
/// # Errors
/// Returns an error if signaling fails.
pub fn terminate(name: &str) -> Result<Option<u32>> {
    match status(name) {
        ServiceStatus::Running { pid, .. } => {
            terminate_pid(pid).with_context(|| format!("terminate {name} (PID {pid})"))?;
            Ok(Some(pid))
        }
        ServiceStatus::Stopped => Ok(None),
    }
}

/// Remove a service's PID file. Silent no-op if the file is absent.
///
/// Use this before `std::process::exit()` to release the PID slot without
/// relying on [`PidGuard`]'s `Drop` (which `process::exit` skips).
pub fn remove(name: &str) {
    let _ = fs::remove_file(pid_path(name));
}

/// Status of a service.
pub enum ServiceStatus {
    Running {
        pid: u32,
        started: Option<std::time::SystemTime>,
    },
    Stopped,
}

/// Guard that removes the PID file on drop.
pub struct PidGuard {
    path: PathBuf,
}

impl Drop for PidGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

fn pid_path(name: &str) -> PathBuf {
    paths::zdx_home().join("run").join(format!("{name}.pid"))
}

fn supervised_path(name: &str) -> PathBuf {
    paths::zdx_home()
        .join("run")
        .join(format!("{name}.supervised"))
}

/// Mark `name` as actively supervised by the current process.
///
/// Writes the current PID to `~/.zdx/run/{name}.supervised`. The supervised
/// service can later call [`is_supervised`] to check whether its supervisor is
/// alive before performing exit-and-be-restarted flows.
///
/// # Errors
///
/// Returns an error if the run directory cannot be created or the marker file
/// cannot be written.
pub fn mark_supervised(name: &str) -> Result<()> {
    let path = supervised_path(name);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("create run dir {}", parent.display()))?;
    }
    let pid = std::process::id();
    fs::write(&path, pid.to_string())
        .with_context(|| format!("write supervised marker {}", path.display()))?;
    Ok(())
}

/// Remove the supervision marker for `name`. Silent no-op if the file is absent.
pub fn unmark_supervised(name: &str) {
    let _ = fs::remove_file(supervised_path(name));
}

/// Returns true when `name` has a supervision marker whose owning process is alive.
///
/// A stale marker (process gone) is treated as unsupervised and is cleaned up.
pub fn is_supervised(name: &str) -> bool {
    let path = supervised_path(name);
    let Ok(content) = fs::read_to_string(&path) else {
        return false;
    };
    let Ok(pid) = content.trim().parse::<u32>() else {
        let _ = fs::remove_file(&path);
        return false;
    };
    if is_alive(pid) {
        true
    } else {
        let _ = fs::remove_file(&path);
        false
    }
}

#[cfg(unix)]
fn is_alive(pid: u32) -> bool {
    // kill(pid, 0) checks if process exists without sending a signal
    unsafe { libc::kill(pid as i32, 0) == 0 }
}

#[cfg(unix)]
fn terminate_pid(pid: u32) -> Result<()> {
    let rc = unsafe { libc::kill(pid as i32, libc::SIGTERM) };
    if rc == 0 {
        Ok(())
    } else {
        anyhow::bail!("failed to send SIGTERM")
    }
}

#[cfg(not(unix))]
fn is_alive(_pid: u32) -> bool {
    true
}

#[cfg(not(unix))]
fn terminate_pid(_pid: u32) -> Result<()> {
    anyhow::bail!("service termination is unsupported on this platform")
}
