//! Lightweight PID file management for service status.

use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result};

use crate::config::paths;

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

#[cfg(unix)]
fn is_alive(pid: u32) -> bool {
    // kill(pid, 0) checks if process exists without sending a signal
    unsafe { libc::kill(pid as i32, 0) == 0 }
}

#[cfg(not(unix))]
fn is_alive(_pid: u32) -> bool {
    true
}
