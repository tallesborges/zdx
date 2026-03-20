//! Lightweight active-run registry for agent turns.
//!
//! Creates ephemeral JSON marker files under `~/.zdx/run/agents/` while an
//! agent turn is executing. The marker is removed automatically on `Drop`
//! (normal completion, error, or panic). Stale markers (dead PID) are
//! filtered out when listing active runs.

use std::fs;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::config::paths;

/// Record stored in each marker file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunRecord {
    pub pid: u32,
    pub started_at: String,
    pub thread_id: Option<String>,
    pub surface: Option<String>,
}

/// Guard that creates a marker file on construction and removes it on drop.
pub struct RunGuard {
    path: PathBuf,
}

impl Drop for RunGuard {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

/// Creates a `RunGuard` that writes a marker file for the current agent turn.
///
/// Best-effort: returns `None` if the marker cannot be written (e.g. permissions).
pub fn start(thread_id: Option<&str>, surface: Option<&str>) -> Option<RunGuard> {
    let dir = agents_run_dir();
    fs::create_dir_all(&dir).ok()?;

    let pid = std::process::id();
    let started_at = chrono::Utc::now().to_rfc3339();
    let record = RunRecord {
        pid,
        started_at,
        thread_id: thread_id.map(String::from),
        surface: surface.map(String::from),
    };

    let filename = format!("{pid}.json");
    let path = dir.join(filename);
    let json = serde_json::to_string(&record).ok()?;
    fs::write(&path, json).ok()?;

    Some(RunGuard { path })
}

/// Lists all currently active agent runs, filtering out stale markers.
pub fn list_active() -> Vec<RunRecord> {
    let dir = agents_run_dir();
    let Ok(entries) = fs::read_dir(&dir) else {
        return Vec::new();
    };

    let mut runs = Vec::new();
    for entry in entries.filter_map(Result::ok) {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let Ok(content) = fs::read_to_string(&path) else {
            continue;
        };
        let Ok(record) = serde_json::from_str::<RunRecord>(&content) else {
            // Corrupt marker — remove it
            let _ = fs::remove_file(&path);
            continue;
        };
        if is_alive(record.pid) {
            runs.push(record);
        } else {
            // Stale marker — clean up
            let _ = fs::remove_file(&path);
        }
    }

    // Sort by started_at ascending (oldest first)
    runs.sort_by(|a, b| a.started_at.cmp(&b.started_at));
    runs
}

/// Returns the number of currently active agent runs.
pub fn count_active() -> usize {
    list_active().len()
}

/// Computes uptime string from an RFC 3339 timestamp.
pub fn uptime_since(started_at: &str) -> String {
    let Ok(started) = chrono::DateTime::parse_from_rfc3339(started_at) else {
        return String::new();
    };
    let elapsed = chrono::Utc::now()
        .signed_duration_since(started)
        .to_std()
        .unwrap_or_default();
    format_duration(elapsed)
}

fn agents_run_dir() -> PathBuf {
    paths::zdx_home().join("run").join("agents")
}

fn format_duration(d: std::time::Duration) -> String {
    let secs = d.as_secs();
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3600 {
        format!("{}m {}s", secs / 60, secs % 60)
    } else {
        format!("{}h {}m", secs / 3600, (secs % 3600) / 60)
    }
}

#[cfg(unix)]
fn is_alive(pid: u32) -> bool {
    unsafe { libc::kill(pid as i32, 0) == 0 }
}

#[cfg(not(unix))]
fn is_alive(_pid: u32) -> bool {
    true
}
