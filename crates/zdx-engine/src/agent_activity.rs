//! Lightweight active-run registry for agent turns.
//!
//! Creates ephemeral JSON marker files under `~/.zdx/run/agents/` while an
//! agent turn is executing. The marker is removed automatically on `Drop`
//! (normal completion, error, or panic). Stale markers (dead PID) are
//! filtered out when listing active runs.

use std::fs;
use std::io::Write;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use tempfile::NamedTempFile;
use uuid::Uuid;

use crate::config::paths;

/// Record stored in each marker file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunRecord {
    pub pid: u32,
    pub started_at: String,
    pub thread_id: Option<String>,
    pub surface: Option<String>,
    pub model: Option<String>,
    /// Logical role of this run, e.g. `"chat"`, `"exec"`, `"telegram"`,
    /// `"subagent"`. `None` is allowed for older markers and ad-hoc runs.
    #[serde(default)]
    pub kind: Option<String>,
    /// When this run was spawned by another agent run, the originating
    /// thread id (useful for grouping subagents under their parent).
    #[serde(default)]
    pub parent_thread_id: Option<String>,
    /// For `invoke_subagent`: the named subagent invoked
    /// (e.g. `"explorer"`, `"oracle"`, `"task"`).
    #[serde(default)]
    pub subagent_name: Option<String>,
}

/// Parameters for [`start`].
#[derive(Debug, Default, Clone, Copy)]
pub struct StartParams<'a> {
    pub thread_id: Option<&'a str>,
    pub surface: Option<&'a str>,
    pub model: Option<&'a str>,
    pub kind: Option<&'a str>,
    pub parent_thread_id: Option<&'a str>,
    pub subagent_name: Option<&'a str>,
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
///
/// Marker writes are atomic — the JSON is staged in a same-directory temp
/// file and renamed into place — so concurrent readers in
/// [`list_active`] never observe partial JSON.
pub fn start(params: StartParams<'_>) -> Option<RunGuard> {
    let dir = agents_run_dir();
    fs::create_dir_all(&dir).ok()?;

    let pid = std::process::id();
    let started_at = chrono::Utc::now().to_rfc3339();
    let record = RunRecord {
        pid,
        started_at,
        thread_id: params.thread_id.map(String::from),
        surface: params.surface.map(String::from),
        model: params.model.map(String::from),
        kind: params.kind.map(String::from),
        parent_thread_id: params.parent_thread_id.map(String::from),
        subagent_name: params.subagent_name.map(String::from),
    };

    let filename = format!("{pid}-{}.json", Uuid::new_v4());
    let path = dir.join(filename);
    let json = serde_json::to_string(&record).ok()?;

    let mut tmp = NamedTempFile::new_in(&dir).ok()?;
    tmp.write_all(json.as_bytes()).ok()?;
    tmp.flush().ok()?;
    tmp.persist(&path).ok()?;

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
            // Corrupt marker — remove it. Atomic writes via tempfile+rename
            // mean we should never see a partial JSON here under normal use.
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
