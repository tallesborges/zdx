//! Lightweight active-run registry for agent turns.
//!
//! Creates ephemeral JSON marker files under `~/.zdx/run/agents/` while an
//! agent turn is executing. The marker is removed automatically on `Drop`
//! (normal completion, error, or panic). Stale markers (dead PID) are
//! filtered out when listing active runs.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use tempfile::NamedTempFile;
use uuid::Uuid;

use crate::config::paths;
use crate::proc_liveness::is_alive;

/// Maximum characters kept in an [`ActiveToolCall`] summary.
const TOOL_SUMMARY_MAX_CHARS: usize = 200;

/// Maximum serialized size of an [`ActiveToolCall`] full input. Larger inputs
/// (e.g. a `write` with a whole file body) fall back to summary-only.
const TOOL_INPUT_MAX_BYTES: usize = 16 * 1024;

/// A tool call currently executing within an active run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActiveToolCall {
    /// Tool-use id (matches the `ToolStarted`/`ToolCompleted` event id).
    pub id: String,
    /// Tool name, e.g. `bash`.
    pub name: String,
    /// One-line input summary, e.g. the bash command or the edited path.
    /// Empty when the tool has no single obvious command.
    pub summary: String,
    /// Full tool input for detail views, or `Value::Null` when it exceeds
    /// [`TOOL_INPUT_MAX_BYTES`] (consumers fall back to `summary`).
    #[serde(default)]
    pub input: serde_json::Value,
    pub started_at: String,
}

impl ActiveToolCall {
    /// Builds an entry from a tool call's name and input, summarizing the
    /// primary command/target on one bounded line.
    #[must_use]
    pub fn new(id: &str, name: &str, input: &serde_json::Value) -> Self {
        let raw = zdx_types::tool_command_text(&name.to_ascii_lowercase(), input);
        let collapsed = raw.split_whitespace().collect::<Vec<_>>().join(" ");
        let summary = if collapsed.chars().count() > TOOL_SUMMARY_MAX_CHARS {
            let mut truncated: String = collapsed.chars().take(TOOL_SUMMARY_MAX_CHARS).collect();
            truncated.push('…');
            truncated
        } else {
            collapsed
        };
        let stored_input =
            if serde_json::to_string(input).is_ok_and(|s| s.len() <= TOOL_INPUT_MAX_BYTES) {
                input.clone()
            } else {
                serde_json::Value::Null
            };
        Self {
            id: id.to_string(),
            name: name.to_string(),
            summary,
            input: stored_input,
            started_at: chrono::Utc::now().to_rfc3339(),
        }
    }
}

/// Record stored in each marker file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunRecord {
    pub pid: u32,
    pub started_at: String,
    pub thread_id: Option<String>,
    pub surface: Option<String>,
    pub model: Option<String>,
    /// Provider id serving the request (e.g. `anthropic`, `claude-cli`, or a
    /// custom provider name).
    #[serde(default)]
    pub provider: Option<String>,
    /// Named OAuth account serving the request (e.g. `parity`), when the run
    /// targets a non-default account of a multi-account provider.
    #[serde(default)]
    pub account: Option<String>,
    /// Thinking/reasoning level for this run (e.g. `off`, `high`, `max`).
    #[serde(default)]
    pub thinking: Option<String>,
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
    /// Tool calls currently executing (empty between tool rounds).
    #[serde(default)]
    pub current_tools: Vec<ActiveToolCall>,
    /// What the run is doing right now: `waiting` (request sent, no tokens
    /// yet), `thinking`, `answering`, or `retrying`. Updated on transitions
    /// only. A tool round is reported through `current_tools` instead.
    #[serde(default)]
    pub phase: Option<String>,
}

/// Parameters for [`start`].
#[derive(Debug, Default, Clone, Copy)]
pub struct StartParams<'a> {
    pub thread_id: Option<&'a str>,
    pub surface: Option<&'a str>,
    pub model: Option<&'a str>,
    pub provider: Option<&'a str>,
    pub account: Option<&'a str>,
    pub thinking: Option<&'a str>,
    pub kind: Option<&'a str>,
    pub parent_thread_id: Option<&'a str>,
    pub subagent_name: Option<&'a str>,
}

/// Guard that creates a marker file on construction and removes it on drop.
pub struct RunGuard {
    path: PathBuf,
    record: Mutex<RunRecord>,
}

impl Drop for RunGuard {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

impl RunGuard {
    /// Replaces the current-tool list for a new tool round and rewrites the
    /// marker. Best-effort: write failures are ignored.
    pub fn set_tools_started(&self, tools: Vec<ActiveToolCall>) {
        let Ok(mut record) = self.record.lock() else {
            return;
        };
        record.current_tools = tools;
        write_record(&self.path, &record);
    }

    /// Removes one finished tool call from the marker.
    pub fn set_tool_finished(&self, id: &str) {
        let Ok(mut record) = self.record.lock() else {
            return;
        };
        record.current_tools.retain(|tool| tool.id != id);
        write_record(&self.path, &record);
    }

    /// Records the run's current phase and rewrites the marker. No-op when the
    /// phase is unchanged, so callers may invoke it once per streamed delta.
    pub fn set_phase(&self, phase: &str) {
        let Ok(mut record) = self.record.lock() else {
            return;
        };
        if record.phase.as_deref() == Some(phase) {
            return;
        }
        record.phase = Some(phase.to_string());
        write_record(&self.path, &record);
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
        provider: params.provider.map(String::from),
        account: params.account.map(String::from),
        thinking: params.thinking.map(String::from),
        kind: params.kind.map(String::from),
        parent_thread_id: params.parent_thread_id.map(String::from),
        subagent_name: params.subagent_name.map(String::from),
        current_tools: Vec::new(),
        phase: Some("waiting".to_string()),
    };

    let filename = format!("{pid}-{}.json", Uuid::new_v4());
    let path = dir.join(filename);
    write_record(&path, &record)?;

    Some(RunGuard {
        path,
        record: Mutex::new(record),
    })
}

/// Atomically writes a marker record (same-directory temp file + rename).
fn write_record(path: &Path, record: &RunRecord) -> Option<()> {
    let dir = path.parent()?;
    let json = serde_json::to_string(record).ok()?;
    let mut tmp = NamedTempFile::new_in(dir).ok()?;
    tmp.write_all(json.as_bytes()).ok()?;
    tmp.flush().ok()?;
    tmp.persist(path).ok()?;
    Some(())
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

#[cfg(test)]
mod tests {
    use super::*;

    fn test_record() -> RunRecord {
        RunRecord {
            pid: std::process::id(),
            started_at: chrono::Utc::now().to_rfc3339(),
            thread_id: Some("t1".to_string()),
            surface: None,
            model: None,
            provider: None,
            account: None,
            thinking: None,
            kind: None,
            parent_thread_id: None,
            subagent_name: None,
            current_tools: Vec::new(),
            phase: None,
        }
    }

    #[test]
    fn old_marker_without_current_tools_deserializes() {
        let json = r#"{"pid":123,"started_at":"2026-08-26T00:00:00Z","thread_id":null,"surface":null,"model":null}"#;
        let record: RunRecord = serde_json::from_str(json).unwrap();
        assert!(record.current_tools.is_empty());
    }

    #[test]
    fn active_tool_call_summarizes_and_bounds_input() {
        let call = ActiveToolCall::new(
            "id1",
            "Bash",
            &serde_json::json!({ "command": "cargo  build\n  --release" }),
        );
        assert_eq!(call.summary, "cargo build --release");
        assert_eq!(call.input["command"], "cargo  build\n  --release");

        let long = "x".repeat(500);
        let call = ActiveToolCall::new("id2", "bash", &serde_json::json!({ "command": long }));
        assert_eq!(call.summary.chars().count(), TOOL_SUMMARY_MAX_CHARS + 1);
        assert!(call.summary.ends_with('…'));

        let huge = "y".repeat(TOOL_INPUT_MAX_BYTES + 1);
        let call = ActiveToolCall::new("id4", "bash", &serde_json::json!({ "command": huge }));
        assert!(call.input.is_null(), "oversized input must not be stored");

        let call = ActiveToolCall::new("id3", "todo_write", &serde_json::json!({}));
        assert!(call.summary.is_empty());
    }

    #[test]
    fn run_guard_updates_current_tools_in_marker() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("marker.json");
        let record = test_record();
        write_record(&path, &record).unwrap();
        let guard = RunGuard {
            path: path.clone(),
            record: Mutex::new(record),
        };

        guard.set_tools_started(vec![ActiveToolCall::new(
            "tool1",
            "bash",
            &serde_json::json!({ "command": "sleep 30" }),
        )]);
        let on_disk: RunRecord = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(on_disk.current_tools.len(), 1);
        assert_eq!(on_disk.current_tools[0].name, "bash");
        assert_eq!(on_disk.current_tools[0].summary, "sleep 30");

        guard.set_tool_finished("tool1");
        let on_disk: RunRecord = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert!(on_disk.current_tools.is_empty());

        drop(guard);
        assert!(!path.exists());
    }

    #[test]
    fn set_phase_writes_only_on_transitions() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("marker.json");
        let record = test_record();
        write_record(&path, &record).unwrap();
        let guard = RunGuard {
            path: path.clone(),
            record: Mutex::new(record),
        };

        guard.set_phase("thinking");
        let on_disk: RunRecord = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(on_disk.phase.as_deref(), Some("thinking"));

        // Unchanged phase must not rewrite the marker: delete the file and
        // confirm a repeat call does not recreate it.
        fs::remove_file(&path).unwrap();
        guard.set_phase("thinking");
        assert!(!path.exists(), "no-op transition must not write");

        guard.set_phase("answering");
        let on_disk: RunRecord = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(on_disk.phase.as_deref(), Some("answering"));
    }
}
