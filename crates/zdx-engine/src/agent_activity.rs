//! Lightweight active-run registry for agent turns.
//!
//! Creates ephemeral JSON marker files under `~/.zdx/run/agents/` while an
//! agent turn is executing. The marker is removed automatically on `Drop`
//! (normal completion, error, or panic). Stale markers are filtered out when
//! listing active runs: the PID is gone, or it now belongs to another process
//! (see [`owner_is_running`]). A marker left by a killed process would
//! otherwise stay "active" for as long as its PID is reused, and block a
//! queued restart or a subagent continuation the whole time.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use tempfile::NamedTempFile;
use uuid::Uuid;

use crate::config::paths;
use crate::proc_liveness::{current_birth, is_alive};

/// Maximum characters kept in an [`ActiveToolCall`] summary.
const TOOL_SUMMARY_MAX_CHARS: usize = 200;

/// Maximum serialized size of an [`ActiveToolCall`] full input. Larger inputs
/// (e.g. a `write` with a whole file body) fall back to summary-only.
const TOOL_INPUT_MAX_BYTES: usize = 16 * 1024;

/// Maximum bytes of streaming output kept per in-flight tool (tail).
const TOOL_OUTPUT_TAIL_MAX_BYTES: usize = 8 * 1024;

/// Minimum interval between marker rewrites caused by streaming output, so a
/// chatty `bash` cannot turn every chunk into a disk write.
const OUTPUT_FLUSH_INTERVAL: std::time::Duration = std::time::Duration::from_secs(1);

/// How often a running turn rewrites its marker with a fresh `heartbeat_at`.
/// The same write carries the latest `last_activity_at`, so activity is at
/// most this stale on disk.
pub const HEARTBEAT_INTERVAL: std::time::Duration = std::time::Duration::from_secs(5);

/// A tool call of the current tool round within an active run.
///
/// A finished call stays in the marker (with `completed_at` set) until the
/// next round replaces the list, so readers can show it until the thread file
/// holds its result. Readers drop entries whose id is already persisted.
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
    /// Tail of the tool's streaming output (bounded by
    /// [`TOOL_OUTPUT_TAIL_MAX_BYTES`], flushed at most once per second).
    #[serde(default)]
    pub output_tail: String,
    pub started_at: String,
    /// When the call finished. `None` while it is still running.
    #[serde(default)]
    pub completed_at: Option<String>,
    /// Whether the finished call succeeded. `None` while running.
    #[serde(default)]
    pub ok: Option<bool>,
    /// Measured duration of the finished call, when recorded.
    #[serde(default)]
    pub duration_ms: Option<u64>,
}

impl ActiveToolCall {
    /// True until the call finishes.
    #[must_use]
    pub fn is_running(&self) -> bool {
        self.completed_at.is_none()
    }

    /// Builds an entry from a tool call's name and input, summarizing the
    /// primary command/target on one bounded line. A `bash` call's
    /// `description` is the summary when it has one.
    #[must_use]
    pub fn new(id: &str, name: &str, input: &serde_json::Value) -> Self {
        let raw = zdx_types::tool_summary_text(&name.to_ascii_lowercase(), input);
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
            output_tail: String::new(),
            started_at: now_rfc3339(),
            completed_at: None,
            ok: None,
            duration_ms: None,
        }
    }
}

fn now_rfc3339() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

/// Record stored in each marker file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunRecord {
    pub pid: u32,
    /// OS start-time identity of `pid` when the marker was written
    /// (`proc_liveness::current_birth`), so a reused PID is not taken for
    /// this run. `None` for older markers or when it could not be read.
    #[serde(default)]
    pub birth_id: Option<u64>,
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
    /// Tool calls of the current tool round: running ones, plus finished ones
    /// (`completed_at` set) until the next round starts. Empty before the
    /// first round.
    #[serde(default)]
    pub current_tools: Vec<ActiveToolCall>,
    /// What the run is doing right now: `waiting` (request sent, no tokens
    /// yet), `thinking`, `answering`, or `retrying`. Updated on transitions
    /// only. A tool round is reported through `current_tools` instead.
    #[serde(default)]
    pub phase: Option<String>,
    /// When the run last emitted any agent event (stream delta, tool event,
    /// tool output). Written with the next marker rewrite, so it can lag by
    /// up to [`HEARTBEAT_INTERVAL`].
    #[serde(default)]
    pub last_activity_at: Option<String>,
    /// When the run's process last rewrote this marker on its heartbeat
    /// timer. A fresh heartbeat shows the process loop is alive; it does not
    /// show that the run makes progress.
    #[serde(default)]
    pub heartbeat_at: Option<String>,
}

/// Shared live view of one active run, derived from its marker. Every surface
/// (Mini App, Monitor, orchestrator tools) reads this instead of interpreting
/// marker fields itself.
///
/// The ages are observations, not verdicts: a quiet tool or a long model
/// stream can leave `seconds_since_activity` high while the run is healthy,
/// so nothing here declares a run stuck.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RunStatus {
    /// `tool` while a tool call runs, else the marker phase (`waiting`,
    /// `thinking`, `answering`, `retrying`), or `working` when unknown.
    pub state: String,
    /// One line for display, e.g. `bash: cargo test (+1 more)` or
    /// `waiting for model`.
    pub label: String,
    /// When the current turn started (RFC 3339).
    pub started_at: String,
    pub turn_elapsed_secs: Option<u64>,
    /// Seconds since the run last emitted any agent event. `None` for markers
    /// written before this field existed.
    pub seconds_since_activity: Option<u64>,
    /// Seconds since the run's process last rewrote its marker on its
    /// heartbeat timer. `None` for markers written before this field existed.
    pub seconds_since_heartbeat: Option<u64>,
    /// Tool calls still running in the current round.
    pub running_tools: usize,
    /// Tool calls of the current round that finished but may not be in the
    /// thread file yet.
    pub finished_tools: usize,
}

impl RunRecord {
    /// Tool calls of the current round that are still running.
    pub fn running_tools(&self) -> impl Iterator<Item = &ActiveToolCall> {
        self.current_tools.iter().filter(|tool| tool.is_running())
    }

    /// Derives the shared live status at `now`.
    #[must_use]
    pub fn status(&self, now: chrono::DateTime<chrono::Utc>) -> RunStatus {
        let running: Vec<&ActiveToolCall> = self.running_tools().collect();
        let (state, label) = if let Some(newest) = running.last() {
            let name = newest.name.to_ascii_lowercase();
            let mut label = if newest.summary.is_empty() {
                name
            } else {
                format!("{name}: {}", newest.summary)
            };
            if running.len() > 1 {
                use std::fmt::Write as _;
                let _ = write!(label, " (+{} more)", running.len() - 1);
            }
            ("tool".to_string(), label)
        } else {
            let phase = self.phase.as_deref().unwrap_or("working");
            let label = match phase {
                "waiting" => "waiting for model",
                other => other,
            };
            (phase.to_string(), label.to_string())
        };
        RunStatus {
            state,
            label,
            started_at: self.started_at.clone(),
            turn_elapsed_secs: secs_since(&self.started_at, now),
            seconds_since_activity: self
                .last_activity_at
                .as_deref()
                .and_then(|at| secs_since(at, now)),
            seconds_since_heartbeat: self
                .heartbeat_at
                .as_deref()
                .and_then(|at| secs_since(at, now)),
            running_tools: running.len(),
            finished_tools: self.current_tools.len() - running.len(),
        }
    }
}

/// Whole seconds from an RFC 3339 timestamp to `now`, clamped at zero.
fn secs_since(at: &str, now: chrono::DateTime<chrono::Utc>) -> Option<u64> {
    let at = chrono::DateTime::parse_from_rfc3339(at).ok()?;
    Some(
        now.signed_duration_since(at)
            .num_seconds()
            .max(0)
            .unsigned_abs(),
    )
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
    /// When streaming output last caused a marker rewrite (throttling).
    last_output_flush: Mutex<Option<std::time::Instant>>,
}

impl Drop for RunGuard {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

impl RunGuard {
    /// Replaces the tool list for a new tool round (dropping the previous
    /// round's finished calls) and rewrites the marker. Best-effort: write
    /// failures are ignored.
    pub fn set_tools_started(&self, tools: Vec<ActiveToolCall>) {
        let Ok(mut record) = self.record.lock() else {
            return;
        };
        record.current_tools = tools;
        record.last_activity_at = Some(now_rfc3339());
        write_record(&self.path, &record);
    }

    /// Marks one tool call finished. It stays in the marker until the next
    /// round, so readers can show it until the thread file holds its result.
    pub fn set_tool_finished(&self, id: &str, ok: bool, duration_ms: Option<u64>) {
        let Ok(mut record) = self.record.lock() else {
            return;
        };
        let now = now_rfc3339();
        if let Some(tool) = record
            .current_tools
            .iter_mut()
            .find(|tool| tool.id == id && tool.is_running())
        {
            tool.completed_at = Some(now.clone());
            tool.ok = Some(ok);
            tool.duration_ms = duration_ms;
        }
        record.last_activity_at = Some(now);
        write_record(&self.path, &record);
    }

    /// Records that the run emitted an agent event. In memory only: the next
    /// marker rewrite (at the latest the next heartbeat) carries it.
    pub fn touch_activity(&self) {
        if let Ok(mut record) = self.record.lock() {
            record.last_activity_at = Some(now_rfc3339());
        }
    }

    /// Stamps `heartbeat_at` and rewrites the marker.
    pub fn heartbeat(&self) {
        let Ok(mut record) = self.record.lock() else {
            return;
        };
        record.heartbeat_at = Some(now_rfc3339());
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

    /// Appends streaming output to an in-flight tool's bounded tail. Marker
    /// rewrites are throttled to [`OUTPUT_FLUSH_INTERVAL`]; the in-memory
    /// tail always stays current so the next flush carries everything.
    pub fn append_tool_output(&self, id: &str, chunk: &str) {
        let Ok(mut record) = self.record.lock() else {
            return;
        };
        let Some(tool) = record
            .current_tools
            .iter_mut()
            .find(|tool| tool.id == id && tool.is_running())
        else {
            return;
        };
        tool.output_tail.push_str(chunk);
        if tool.output_tail.len() > TOOL_OUTPUT_TAIL_MAX_BYTES {
            let mut cut = tool.output_tail.len() - TOOL_OUTPUT_TAIL_MAX_BYTES;
            while !tool.output_tail.is_char_boundary(cut) {
                cut += 1;
            }
            tool.output_tail.drain(..cut);
        }

        let Ok(mut last_flush) = self.last_output_flush.lock() else {
            return;
        };
        let now = std::time::Instant::now();
        if last_flush.is_some_and(|at| now.duration_since(at) < OUTPUT_FLUSH_INTERVAL) {
            return;
        }
        *last_flush = Some(now);
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
    let started_at = now_rfc3339();
    let record = RunRecord {
        pid,
        birth_id: current_birth(pid),
        started_at: started_at.clone(),
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
        last_activity_at: Some(started_at.clone()),
        heartbeat_at: Some(started_at),
    };

    let filename = format!("{pid}-{}.json", Uuid::new_v4());
    let path = dir.join(filename);
    write_record(&path, &record)?;

    Some(RunGuard {
        path,
        record: Mutex::new(record),
        last_output_flush: Mutex::new(None),
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
        if owner_is_running(&record) {
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

/// Whether the process that wrote `record` is still running.
///
/// Process identity, not heartbeat age: a healthy run can be quiet for a long
/// time, and a sleeping Mac writes no heartbeat at all, so neither activity nor
/// heartbeat age says the owner is gone. A live PID whose OS start time differs
/// from the recorded one is another process that reused the PID. Markers
/// without `birth_id` (written before it existed) fall back to their
/// `started_at` where the start time is wall-clock (macOS): a process that
/// started after the marker was written cannot have written it. Where the
/// start time cannot be read, a live PID is kept, as before.
fn owner_is_running(record: &RunRecord) -> bool {
    if !is_alive(record.pid) {
        return false;
    }
    let Some(current) = current_birth(record.pid) else {
        return true;
    };
    match record.birth_id {
        Some(recorded) => recorded == current,
        None => !started_after_marker(current, &record.started_at),
    }
}

/// Slack for clock steps between a process start and its first marker write.
#[cfg(target_os = "macos")]
const LEGACY_BIRTH_SLACK_MICROS: i64 = 60_000_000;

/// Whether a process with wall-clock start `birth_micros` started after the
/// marker's `started_at` (plus slack), so it is not the marker's writer.
#[cfg(target_os = "macos")]
fn started_after_marker(birth_micros: u64, started_at: &str) -> bool {
    let Ok(started) = chrono::DateTime::parse_from_rfc3339(started_at) else {
        return false;
    };
    let Ok(birth) = i64::try_from(birth_micros) else {
        return false;
    };
    birth > started.timestamp_micros() + LEGACY_BIRTH_SLACK_MICROS
}

/// Linux start times count clock ticks since boot, not wall time, so a legacy
/// marker cannot be compared; it is kept while its PID lives, as before.
#[cfg(not(target_os = "macos"))]
fn started_after_marker(_birth: u64, _started_at: &str) -> bool {
    false
}

/// The newest active run bound to `thread_id`, if any.
#[must_use]
pub fn active_run_for_thread(thread_id: &str) -> Option<RunRecord> {
    run_for_thread(&list_active(), thread_id).cloned()
}

/// The newest run in `runs` bound to `thread_id`. Lets callers that need
/// several threads read the marker directory once.
#[must_use]
pub fn run_for_thread<'a>(runs: &'a [RunRecord], thread_id: &str) -> Option<&'a RunRecord> {
    if thread_id.is_empty() {
        return None;
    }
    runs.iter()
        .rev()
        .find(|run| run.thread_id.as_deref() == Some(thread_id))
}

/// Spawns the heartbeat for a running turn: every [`HEARTBEAT_INTERVAL`] it
/// rewrites the marker with a fresh `heartbeat_at` (and the latest activity
/// time). Holds only a weak reference, so it stops on the first tick after
/// the guard drops, and never delays the marker removal.
pub fn spawn_heartbeat(guard: &std::sync::Arc<RunGuard>) -> tokio::task::JoinHandle<()> {
    let guard = std::sync::Arc::downgrade(guard);
    tokio::spawn(async move {
        let mut ticks = tokio::time::interval(HEARTBEAT_INTERVAL);
        ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        // The first tick fires at once; `start` already wrote a heartbeat.
        ticks.tick().await;
        loop {
            ticks.tick().await;
            let Some(guard) = guard.upgrade() else {
                break;
            };
            guard.heartbeat();
        }
    })
}

/// Formats whole seconds as a short age, e.g. `42s`, `3m 5s`, `1h 2m`.
#[must_use]
pub fn format_secs(secs: u64) -> String {
    format_duration(std::time::Duration::from_secs(secs))
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
            birth_id: current_birth(std::process::id()),
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
            last_activity_at: None,
            heartbeat_at: None,
        }
    }

    /// A marker whose PID now belongs to another process is stale: the
    /// recorded start time does not match, or (older markers) the process
    /// started after the marker was written. The real owner stays active,
    /// however old its heartbeat, and a dead PID is stale as before.
    #[test]
    fn markers_of_a_reused_pid_are_stale_and_quiet_owners_stay_active() {
        let me = std::process::id();
        let Some(birth) = current_birth(me) else {
            return;
        };
        let long_ago = (chrono::Utc::now() - chrono::Duration::hours(6)).to_rfc3339();

        let mut own = test_record();
        own.heartbeat_at = Some(long_ago.clone());
        own.last_activity_at = Some(long_ago.clone());
        assert!(
            owner_is_running(&own),
            "a quiet run with a stale heartbeat is alive"
        );

        let mut reused = test_record();
        reused.birth_id = Some(birth.wrapping_add(1));
        assert!(
            !owner_is_running(&reused),
            "another process now holds the PID"
        );

        let mut legacy = test_record();
        legacy.birth_id = None;
        assert!(
            owner_is_running(&legacy),
            "a legacy marker of the live owner is kept"
        );
        legacy.started_at = long_ago;
        // This test process started well after a marker written 6 h ago.
        assert_eq!(owner_is_running(&legacy), !cfg!(target_os = "macos"));

        let mut child = std::process::Command::new("true").spawn().expect("spawn");
        let dead = child.id();
        child.wait().expect("reap");
        let mut gone = test_record();
        gone.pid = dead;
        assert!(!owner_is_running(&gone));
    }

    /// Through the marker directory: a leftover marker of a killed run whose
    /// PID was reused is removed and not listed; the live run is listed.
    #[test]
    fn list_active_drops_markers_of_a_reused_pid() {
        let _home = crate::test_support::temp_zdx_home();
        let Some(birth) = current_birth(std::process::id()) else {
            return;
        };
        let live = start(StartParams {
            thread_id: Some("live-thread"),
            ..StartParams::default()
        })
        .expect("marker");
        let dir = agents_run_dir();
        let leftover = dir.join(format!("{}-leftover.json", std::process::id()));
        let mut record = test_record();
        record.thread_id = Some("killed-thread".to_string());
        record.birth_id = Some(birth.wrapping_add(1));
        write_record(&leftover, &record).expect("write");

        let threads: Vec<_> = list_active()
            .into_iter()
            .filter_map(|run| run.thread_id)
            .collect();
        assert_eq!(threads, ["live-thread"]);
        assert!(!leftover.exists(), "the stale marker is removed");
        drop(live);
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
    fn active_tool_call_prefers_the_bash_description() {
        let call = ActiveToolCall::new(
            "id1",
            "Bash",
            &serde_json::json!({ "command": "cargo test", "description": "Run the test suite" }),
        );
        assert_eq!(call.summary, "Run the test suite");

        // Old calls without a description keep summarizing the command.
        let call = ActiveToolCall::new(
            "id2",
            "Bash",
            &serde_json::json!({ "command": "cargo  build\n  --release" }),
        );
        assert_eq!(call.summary, "cargo build --release");
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
            last_output_flush: Mutex::new(None),
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

        guard.set_tool_finished("tool1", true, Some(1200));
        let on_disk: RunRecord = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(
            on_disk.current_tools.len(),
            1,
            "finished call stays until the next round"
        );
        assert!(!on_disk.current_tools[0].is_running());
        assert_eq!(on_disk.current_tools[0].ok, Some(true));
        assert_eq!(on_disk.current_tools[0].duration_ms, Some(1200));
        assert_eq!(on_disk.running_tools().count(), 0);

        guard.set_tools_started(Vec::new());
        let on_disk: RunRecord = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert!(on_disk.current_tools.is_empty());

        drop(guard);
        assert!(!path.exists());
    }

    #[test]
    fn append_tool_output_keeps_bounded_tail() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("marker.json");
        let mut record = test_record();
        record.current_tools = vec![ActiveToolCall::new(
            "tool1",
            "bash",
            &serde_json::json!({ "command": "make" }),
        )];
        write_record(&path, &record).unwrap();
        let guard = RunGuard {
            path: path.clone(),
            record: Mutex::new(record),
            last_output_flush: Mutex::new(None),
        };

        guard.append_tool_output("tool1", "hello ");
        let on_disk: RunRecord = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(on_disk.current_tools[0].output_tail, "hello ");

        // Unknown tool ids are ignored.
        guard.append_tool_output("nope", "x");

        // The tail stays bounded even when appended output exceeds the cap.
        guard.append_tool_output("tool1", &"y".repeat(TOOL_OUTPUT_TAIL_MAX_BYTES * 2));
        let record = guard.record.lock().unwrap();
        assert_eq!(
            record.current_tools[0].output_tail.len(),
            TOOL_OUTPUT_TAIL_MAX_BYTES
        );
        assert!(
            record.current_tools[0]
                .output_tail
                .chars()
                .all(|c| c == 'y')
        );
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
            last_output_flush: Mutex::new(None),
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

    #[test]
    fn status_reports_newest_running_tool_and_counts_finished_ones() {
        let now = chrono::Utc::now();
        let ago = |secs: i64| {
            (now - chrono::Duration::seconds(secs))
                .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
        };
        let mut record = test_record();
        record.started_at = ago(90);
        record.phase = Some("answering".to_string());
        record.last_activity_at = Some(ago(40));
        record.heartbeat_at = Some(ago(3));
        let mut finished =
            ActiveToolCall::new("a", "Read", &serde_json::json!({ "path": "src/lib.rs" }));
        finished.completed_at = Some(ago(50));
        finished.ok = Some(true);
        record.current_tools = vec![
            finished,
            ActiveToolCall::new("b", "Bash", &serde_json::json!({ "command": "cargo test" })),
            ActiveToolCall::new("c", "Bash", &serde_json::json!({ "command": "sleep 30" })),
        ];

        let status = record.status(now);
        assert_eq!(status.state, "tool");
        assert_eq!(status.label, "bash: sleep 30 (+1 more)");
        assert_eq!(status.running_tools, 2);
        assert_eq!(status.finished_tools, 1);
        assert_eq!(status.turn_elapsed_secs, Some(90));
        assert_eq!(status.seconds_since_activity, Some(40));
        assert_eq!(status.seconds_since_heartbeat, Some(3));

        // Once every call finished, the phase describes the run again.
        for tool in &mut record.current_tools {
            tool.completed_at.get_or_insert_with(|| ago(1));
        }
        record.phase = Some("waiting".to_string());
        let status = record.status(now);
        assert_eq!(status.state, "waiting");
        assert_eq!(status.label, "waiting for model");
        assert_eq!(status.running_tools, 0);
        assert_eq!(status.finished_tools, 3);
    }

    #[test]
    fn markers_without_activity_fields_report_unknown_ages() {
        let json = r#"{"pid":123,"started_at":"2026-08-26T00:00:00Z","thread_id":"t","surface":null,"model":null,"current_tools":[{"id":"x","name":"bash","summary":"ls","started_at":"2026-08-26T00:00:01Z"}]}"#;
        let record: RunRecord = serde_json::from_str(json).unwrap();
        let status = record.status(chrono::Utc::now());
        assert_eq!(status.state, "tool");
        assert_eq!(status.seconds_since_activity, None);
        assert_eq!(status.seconds_since_heartbeat, None);
    }

    #[test]
    fn activity_is_kept_in_memory_until_the_next_write() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("marker.json");
        let record = test_record();
        write_record(&path, &record).unwrap();
        let guard = RunGuard {
            path: path.clone(),
            record: Mutex::new(record),
            last_output_flush: Mutex::new(None),
        };

        guard.touch_activity();
        let on_disk: RunRecord = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert!(
            on_disk.last_activity_at.is_none(),
            "touch alone must not write"
        );

        guard.heartbeat();
        let on_disk: RunRecord = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert!(on_disk.heartbeat_at.is_some());
        assert!(
            on_disk.last_activity_at.is_some(),
            "heartbeat write carries the latest activity"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn heartbeat_task_stops_after_the_guard_drops() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("marker.json");
        let record = test_record();
        write_record(&path, &record).unwrap();
        let guard = std::sync::Arc::new(RunGuard {
            path: path.clone(),
            record: Mutex::new(record),
            last_output_flush: Mutex::new(None),
        });

        let task = spawn_heartbeat(&guard);
        tokio::time::sleep(HEARTBEAT_INTERVAL + std::time::Duration::from_millis(10)).await;
        let on_disk: RunRecord = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert!(on_disk.heartbeat_at.is_some());

        drop(guard);
        assert!(
            !path.exists(),
            "the weak heartbeat must not keep the marker"
        );
        tokio::time::sleep(HEARTBEAT_INTERVAL * 2).await;
        assert!(task.is_finished());
        assert!(!path.exists());
    }
}
