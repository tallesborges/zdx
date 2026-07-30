//! Background-process registry.
//!
//! Mirrors [`crate::agent_activity`] but for long-lived detached processes
//! started via the bash tool with `background: true`. Unlike agent runs,
//! background processes intentionally **outlive** the turn and the zdx process
//! that started them, so markers are NOT removed on `Drop`. Instead they are:
//!
//! - reaped (tombstoned) when the process is gone (liveness + identity check),
//! - killed explicitly with an **identity guard** that defends against PID
//!   reuse (a dead PID can be recycled by an unrelated process),
//! - retained briefly as an `exited` tombstone after exit/kill so
//!   `background_output` / exit-code reads still work, then pruned.
//!
//! Marker files live at `~/.zdx/run/background/<bg_id>.json`; durable logs at
//! `~/.zdx/run/background/logs/<bg_id>.{out,err}`.

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tempfile::NamedTempFile;

use crate::config::paths;
use crate::proc_liveness::{Liveness, current_birth, current_pgid, liveness};

/// How long an exited process's tombstone (and its logs) are retained so
/// output / exit-code reads still work after the process ends.
const TOMBSTONE_RETENTION: Duration = Duration::from_mins(5);

/// One background-process record, persisted as a marker JSON file.
///
/// `exited_at.is_none()` means the process is considered running; once set it
/// is an exited tombstone awaiting prune.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackgroundProcess {
    pub bg_id: String,
    pub pid: u32,
    /// Process-group id (== session id, since background mode uses `setsid`).
    pub pgid: i32,
    /// OS process start-time identity captured at spawn; defeats PID reuse on
    /// kill. Microseconds since the Unix epoch on macOS; platform starttime on
    /// Linux. `None` only if the OS identity could not be read.
    pub birth_id: Option<u64>,
    pub thread_id: Option<String>,
    pub command: String,
    pub cwd: String,
    pub started_at: String,
    /// RFC 3339 exit timestamp; `None` while running.
    #[serde(default)]
    pub exited_at: Option<String>,
    /// Exit code when known (`None` for killed / unknown).
    #[serde(default)]
    pub exit_code: Option<i32>,
}

impl BackgroundProcess {
    #[must_use]
    pub fn is_running(&self) -> bool {
        self.exited_at.is_none()
    }

    #[must_use]
    pub fn uptime(&self) -> String {
        crate::agent_activity::uptime_since(&self.started_at)
    }
}

/// Outcome of a [`kill_background`] request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KillOutcome {
    /// The process was signalled and confirmed gone.
    Killed,
    /// The record was already an exited tombstone (or the process was already
    /// gone / its PID reused) — nothing to kill.
    AlreadyExited,
    /// No marker with this `bg_id`.
    NotFound,
    /// The process is alive but its identity could not be verified as ours
    /// (e.g. `EPERM`, or missing birth id) — we refuse to signal (fail closed).
    Unverifiable,
}

fn background_run_dir() -> PathBuf {
    paths::zdx_home().join("run").join("background")
}

fn logs_dir() -> PathBuf {
    background_run_dir().join("logs")
}

#[must_use]
pub fn stdout_log_path(bg_id: &str) -> PathBuf {
    logs_dir().join(format!("{bg_id}.out"))
}

#[must_use]
pub fn stderr_log_path(bg_id: &str) -> PathBuf {
    logs_dir().join(format!("{bg_id}.err"))
}

fn marker_path_in(dir: &Path, bg_id: &str) -> PathBuf {
    dir.join(format!("{bg_id}.json"))
}

/// Ensures the registry + logs directories exist with user-only permissions.
///
/// # Errors
/// Returns an error if the directories cannot be created.
pub fn ensure_dirs() -> io::Result<()> {
    let dir = background_run_dir();
    fs::create_dir_all(&dir)?;
    fs::create_dir_all(logs_dir())?;
    set_dir_perms(&dir);
    set_dir_perms(&logs_dir());
    Ok(())
}

/// Atomically writes (or overwrites) a marker file for `rec`.
///
/// The JSON is staged in a same-directory temp file and renamed into place so
/// concurrent readers never observe partial JSON.
///
/// # Errors
/// Returns an error if the directory cannot be created or the marker cannot be
/// serialized/written.
pub fn write_marker(rec: &BackgroundProcess) -> io::Result<()> {
    write_marker_in(&background_run_dir(), rec)
}

/// Atomic marker write into an explicit registry dir (real dir or a test temp
/// dir). All of `list_in`'s reap writes go through here so a dir-scoped scan
/// never touches the global registry.
fn write_marker_in(dir: &Path, rec: &BackgroundProcess) -> io::Result<()> {
    fs::create_dir_all(dir)?;
    let json = serde_json::to_string(rec).map_err(io::Error::other)?;
    let mut tmp = NamedTempFile::new_in(dir)?;
    tmp.write_all(json.as_bytes())?;
    tmp.flush()?;
    tmp.persist(marker_path_in(dir, &rec.bg_id))
        .map_err(|e| e.error)?;
    Ok(())
}

fn read_marker_at(path: &Path) -> Option<BackgroundProcess> {
    let content = fs::read_to_string(path).ok()?;
    serde_json::from_str(&content).ok()
}

/// Reads a single record by id, if present.
#[must_use]
pub fn get(bg_id: &str) -> Option<BackgroundProcess> {
    read_marker_at(&marker_path_in(&background_run_dir(), bg_id))
}

/// Lists all current background records (running + not-yet-pruned tombstones),
/// oldest first. Side effects while scanning: a running record whose process is
/// gone/reused is tombstoned, and tombstones older than the retention window
/// are pruned (marker + logs removed).
#[must_use]
pub fn list_background() -> Vec<BackgroundProcess> {
    list_in(&background_run_dir())
}

fn list_in(dir: &Path) -> Vec<BackgroundProcess> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };

    let mut out = Vec::new();
    for entry in entries.filter_map(Result::ok) {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let Some(mut rec) = read_marker_at(&path) else {
            // Corrupt marker — atomic writes mean this should be rare.
            let _ = fs::remove_file(&path);
            continue;
        };

        // Reap: a "running" record whose process is definitively gone/reused
        // becomes a tombstone. Unknown (EPERM / unreadable identity) is left
        // alone — fail closed rather than falsely reaping a live process.
        if rec.is_running() && matches!(ownership(&rec), Ownership::Gone) {
            rec.exited_at = Some(now_rfc3339());
            rec.exit_code = None;
            let _ = write_marker_in(dir, &rec);
        }

        // Prune aged tombstones (marker + derived log files), all within `dir`.
        if let Some(exited_at) = rec.exited_at.as_deref()
            && tombstone_expired(exited_at)
        {
            let logs = dir.join("logs");
            let _ = fs::remove_file(&path);
            let _ = fs::remove_file(logs.join(format!("{}.out", rec.bg_id)));
            let _ = fs::remove_file(logs.join(format!("{}.err", rec.bg_id)));
            continue;
        }

        out.push(rec);
    }

    out.sort_by(|a, b| a.started_at.cmp(&b.started_at));
    out
}

/// Marks a record as exited with a known code (called by the spawn waiter).
pub fn mark_exited(bg_id: &str, code: Option<i32>) {
    if let Some(mut rec) = get(bg_id)
        && rec.is_running()
    {
        rec.exited_at = Some(now_rfc3339());
        rec.exit_code = code;
        let _ = write_marker(&rec);
    }
}

/// Terminates a background process by id, with a PID-reuse identity guard.
///
/// Only signals when the live process still matches the recorded
/// `pid + birth_id + pgid`. Sends `SIGTERM`, waits a short grace, then
/// `SIGKILL`, and tombstones the record on confirmed exit.
pub async fn kill_background(bg_id: &str) -> KillOutcome {
    let Some(mut rec) = get(bg_id) else {
        return KillOutcome::NotFound;
    };
    if !rec.is_running() {
        return KillOutcome::AlreadyExited;
    }

    match ownership(&rec) {
        Ownership::Gone => {
            // Process already gone (or its PID was reused) — tombstone, no signal.
            rec.exited_at = Some(now_rfc3339());
            rec.exit_code = None;
            let _ = write_marker(&rec);
            KillOutcome::AlreadyExited
        }
        Ownership::Unknown => KillOutcome::Unverifiable,
        Ownership::Ours => {
            signal_group(rec.pgid, &Signal::Term);
            if wait_gone(&rec, Duration::from_secs(3)).await {
                tombstone(&mut rec);
                return KillOutcome::Killed;
            }
            // Still ours after the grace — escalate.
            signal_group(rec.pgid, &Signal::Kill);
            let _ = wait_gone(&rec, Duration::from_secs(2)).await;
            tombstone(&mut rec);
            KillOutcome::Killed
        }
    }
}

fn tombstone(rec: &mut BackgroundProcess) {
    rec.exited_at = Some(now_rfc3339());
    rec.exit_code = None;
    let _ = write_marker(rec);
}

/// Polls until the recorded process is no longer ours (gone/reused) or the
/// deadline elapses. Returns `true` if it is gone.
async fn wait_gone(rec: &BackgroundProcess, budget: Duration) -> bool {
    let deadline = std::time::Instant::now() + budget;
    loop {
        if !matches!(ownership(rec), Ownership::Ours) {
            return true;
        }
        if std::time::Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// Whether a recorded process is still the one we started.
enum Ownership {
    /// Alive and identity matches (`pid` + `birth_id` + `pgid`).
    Ours,
    /// Definitively gone, or alive but a different identity (PID reused).
    Gone,
    /// Alive but unverifiable (EPERM or unreadable identity) — fail closed.
    Unknown,
}

/// Captures the process's OS start-time identity + pgid at spawn time. Callers
/// store `birth_id` in the record so kills can defend against PID reuse.
#[must_use]
pub fn capture_identity(pid: u32) -> (Option<u64>, Option<i32>) {
    (current_birth(pid), current_pgid(pid))
}

fn ownership(rec: &BackgroundProcess) -> Ownership {
    match liveness(rec.pid) {
        Liveness::Dead => Ownership::Gone,
        Liveness::Unverifiable => Ownership::Unknown,
        Liveness::Alive => {
            let (Some(recorded), Some(current)) = (rec.birth_id, current_birth(rec.pid)) else {
                // Can't compare birth identity → don't claim ownership.
                return Ownership::Unknown;
            };
            let pgid_ok = current_pgid(rec.pid) == Some(rec.pgid);
            if recorded == current && pgid_ok {
                Ownership::Ours
            } else {
                // Alive but a different process now holds this PID.
                Ownership::Gone
            }
        }
    }
}

enum Signal {
    Term,
    Kill,
}

#[cfg(unix)]
fn signal_group(pgid: i32, sig: &Signal) {
    let signum = match sig {
        Signal::Term => libc::SIGTERM,
        Signal::Kill => libc::SIGKILL,
    };
    unsafe {
        libc::killpg(pgid, signum);
    }
}

#[cfg(not(unix))]
fn signal_group(_pgid: i32, _sig: &Signal) {}

fn now_rfc3339() -> String {
    chrono::Utc::now().to_rfc3339()
}

fn tombstone_expired(exited_at: &str) -> bool {
    let Ok(exited) = chrono::DateTime::parse_from_rfc3339(exited_at) else {
        return false;
    };
    chrono::Utc::now()
        .signed_duration_since(exited)
        .to_std()
        .is_ok_and(|elapsed| elapsed > TOMBSTONE_RETENTION)
}

#[cfg(unix)]
fn set_dir_perms(dir: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = fs::set_permissions(dir, fs::Permissions::from_mode(0o700));
}

#[cfg(not(unix))]
fn set_dir_perms(_dir: &Path) {}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(bg_id: &str) -> BackgroundProcess {
        BackgroundProcess {
            bg_id: bg_id.to_string(),
            pid: 4321,
            pgid: 4321,
            birth_id: Some(1_700_000_000_000_000),
            thread_id: Some("thread-abc".to_string()),
            command: "npm run dev".to_string(),
            cwd: "/tmp/proj".to_string(),
            started_at: now_rfc3339(),
            exited_at: None,
            exit_code: None,
        }
    }

    #[test]
    fn serde_round_trip() {
        let rec = sample("bg-1");
        let json = serde_json::to_string(&rec).unwrap();
        let back: BackgroundProcess = serde_json::from_str(&json).unwrap();
        assert_eq!(back.bg_id, rec.bg_id);
        assert_eq!(back.pgid, rec.pgid);
        assert_eq!(back.birth_id, rec.birth_id);
        assert!(back.is_running());
    }

    #[test]
    fn aged_tombstone_is_pruned_and_running_kept() {
        let dir = tempfile::tempdir().unwrap();

        // A running record (fake dead pid → will be reaped to a tombstone, then
        // kept because its tombstone is fresh).
        let mut running = sample("bg-run");
        running.pid = 999_999_999; // almost certainly not alive
        let p1 = marker_path_in(dir.path(), &running.bg_id);
        fs::write(&p1, serde_json::to_string(&running).unwrap()).unwrap();

        // An already-exited record with an old exit timestamp → pruned.
        let mut old = sample("bg-old");
        old.exited_at = Some((chrono::Utc::now() - chrono::Duration::hours(1)).to_rfc3339());
        let p2 = marker_path_in(dir.path(), &old.bg_id);
        fs::write(&p2, serde_json::to_string(&old).unwrap()).unwrap();

        let listed = list_in(dir.path());

        assert!(!p2.exists(), "aged tombstone marker should be pruned");
        // The reaped record must be rewritten into the scan dir, not the global
        // registry (test isolation).
        assert!(
            p1.exists(),
            "reaped tombstone should stay in the scanned dir"
        );
        assert!(
            !background_run_dir().join("bg-run.json").exists(),
            "list_in must not write into the global registry"
        );
        assert!(
            listed.iter().any(|r| r.bg_id == "bg-run"),
            "fresh record should remain listed"
        );
        assert!(
            listed.iter().all(|r| r.bg_id != "bg-old"),
            "aged tombstone should not be listed"
        );
    }

    #[test]
    fn tombstone_expiry_window() {
        let fresh = now_rfc3339();
        assert!(!tombstone_expired(&fresh));
        let old = (chrono::Utc::now() - chrono::Duration::hours(1)).to_rfc3339();
        assert!(tombstone_expired(&old));
    }

    // Exercises the real spawn primitive + identity guard + group kill against a
    // live process (macOS birth-time via proc_pidinfo).
    #[cfg(target_os = "macos")]
    #[tokio::test]
    // `pgid` reads as too close to `pid`, but both are the real domain names here.
    #[allow(clippy::similar_names)]
    async fn real_process_identity_guard_and_kill() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("o.log");
        let err = dir.path().join("e.log");

        let spawn = zdx_tools::bash::spawn_background("sleep 30", dir.path(), &out, &err).unwrap();
        let pid = spawn.pid;
        assert!(pid > 0);

        let (birth, pgid) = capture_identity(pid);
        assert!(
            birth.is_some(),
            "should capture a birth id for a live process"
        );
        let pgid = pgid.expect("live process has a pgid");

        let mut rec = sample("bg-live");
        rec.pid = pid;
        rec.pgid = pgid;
        rec.birth_id = birth;

        // Correct identity matches; a tampered birth id must not (PID-reuse defense).
        assert!(matches!(ownership(&rec), Ownership::Ours));
        let mut wrong = rec.clone();
        wrong.birth_id = Some(birth.unwrap().wrapping_add(1));
        assert!(!matches!(ownership(&wrong), Ownership::Ours));

        signal_group(pgid, &Signal::Kill);
        let mut child = spawn.child;
        let _ = child.wait().await;
        tokio::time::sleep(Duration::from_millis(150)).await;
        assert!(matches!(liveness(pid), Liveness::Dead));
    }
}
