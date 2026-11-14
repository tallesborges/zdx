//! Durable job state under `$ZDX_HOME`, shared by every surface:
//!
//! - `jobs/*.md`: definitions; `jobs/archive/*.md`: finished once jobs.
//! - `jobs_runs.jsonl`: one record per attempt, plus missed and interrupted
//!   occurrences.
//! - `state/jobs/<name>/claims/<key>.json`: one claim per scheduled
//!   occurrence, created exclusively before the run starts, so the scheduler
//!   starts each occurrence at most once.
//! - `state/jobs/<name>/lock`: held (advisory `flock`) for the whole run, so a
//!   job has at most one live run. The OS releases it when the holder dies,
//!   so a claim without an outcome and a free lock means the run was
//!   interrupted.
//! - `state/jobs/<name>/last.json`: the latest final outcome. A final claim
//!   is authoritative: [`JobsHome::reconcile_last_runs`] repairs a `last.json`
//!   left stale by a crash between the two writes.
//! - `state/jobs/<name>/missed.json`: the latest recurring occurrence already
//!   recorded as missed, so a missed summary is not written twice.
//! - `state/jobs/scheduler.json`: when the scheduler started and last
//!   checked.

use std::fs::{self, File, OpenOptions, TryLockError};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result};
use chrono::{DateTime, FixedOffset, Utc};
use serde::{Deserialize, Serialize};

use super::schedule::{JobSchedule, ScheduleKind, once_occurrence};
use crate::config::paths;

/// Root of all job files for one `ZDX_HOME`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobsHome {
    zdx_home: PathBuf,
}

/// What started a run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RunTrigger {
    /// `zdx jobs run <name>`.
    Manual,
    /// The scheduler (`zdx jobs daemon`).
    Daemon,
}

impl RunTrigger {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Manual => "manual",
            Self::Daemon => "daemon",
        }
    }
}

/// Result of an attempt or an occurrence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Succeeded,
    Failed,
    /// An attempt failed and another attempt follows (run records only).
    Retrying,
    /// The occurrence did not start within its grace.
    Missed,
    /// The run stopped without a result (crash, kill, or interrupt).
    Interrupted,
}

impl Outcome {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Retrying => "retrying",
            Self::Missed => "missed",
            Self::Interrupted => "interrupted",
        }
    }
}

/// The durable record that one scheduled occurrence was taken.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Claim {
    pub job: String,
    pub key: String,
    pub run_id: String,
    pub trigger: RunTrigger,
    pub schedule_kind: ScheduleKind,
    pub scheduled_for: DateTime<Utc>,
    pub claimed_at: DateTime<Utc>,
    pub pid: u32,
    /// `None` while the run is in progress.
    pub outcome: Option<Outcome>,
    pub finished_at: Option<DateTime<Utc>>,
    pub error: Option<String>,
    pub thread_id: Option<String>,
}

impl Claim {
    /// A new, unfinished claim made by this process.
    #[must_use]
    pub fn new(
        job: &str,
        key: &str,
        trigger: RunTrigger,
        schedule_kind: ScheduleKind,
        scheduled_for: DateTime<Utc>,
    ) -> Self {
        Self {
            job: job.to_string(),
            key: key.to_string(),
            run_id: new_run_id(),
            trigger,
            schedule_kind,
            scheduled_for,
            claimed_at: Utc::now(),
            pid: std::process::id(),
            outcome: None,
            finished_at: None,
            error: None,
            thread_id: None,
        }
    }

    /// Marks the claim final.
    #[must_use]
    pub fn finished(mut self, outcome: Outcome, error: Option<String>) -> Self {
        self.outcome = Some(outcome);
        self.finished_at = Some(Utc::now());
        self.error = error;
        self
    }
}

impl Claim {
    /// The `last.json` view of a final claim; `None` while it runs.
    #[must_use]
    pub fn to_last_run(&self) -> Option<LastRun> {
        Some(LastRun {
            run_id: self.run_id.clone(),
            trigger: self.trigger,
            outcome: self.outcome?,
            scheduled_for: Some(self.scheduled_for),
            finished_at: self.finished_at?,
            error: self.error.clone(),
            thread_id: self.thread_id.clone(),
        })
    }
}

/// The later of two final outcomes (by `finished_at`).
#[must_use]
pub fn newer_last_run(a: Option<LastRun>, b: Option<LastRun>) -> Option<LastRun> {
    match (a, b) {
        (Some(a), Some(b)) => Some(if b.finished_at > a.finished_at { b } else { a }),
        (a, b) => a.or(b),
    }
}

/// Unique id for one run (all its attempts) or one recorded occurrence.
#[must_use]
pub fn new_run_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

/// Latest final outcome of a job, for listings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LastRun {
    pub run_id: String,
    pub trigger: RunTrigger,
    pub outcome: Outcome,
    pub scheduled_for: Option<DateTime<Utc>>,
    pub finished_at: DateTime<Utc>,
    pub error: Option<String>,
    pub thread_id: Option<String>,
}

/// One line of `jobs_runs.jsonl`. Fields added after the first release are
/// optional so older lines still parse.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobRunRecord {
    pub job: String,
    #[serde(default)]
    pub run_id: Option<String>,
    pub trigger: RunTrigger,
    #[serde(default)]
    pub schedule_kind: Option<ScheduleKind>,
    /// Scheduled instant of the occurrence (RFC 3339), for scheduled runs and
    /// manual runs that consumed a once job.
    #[serde(default)]
    pub scheduled_for: Option<String>,
    /// Seconds between `scheduled_for` and the start.
    #[serde(default)]
    pub lateness_secs: Option<i64>,
    #[serde(default)]
    pub thread_id: Option<String>,
    pub attempt: u32,
    pub max_attempts: u32,
    pub started_at: String,
    pub finished_at: String,
    pub duration_ms: u64,
    pub ok: bool,
    #[serde(default)]
    pub outcome: Option<Outcome>,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub schedule: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub subagent: Option<String>,
    /// Root the run used; `None` when it could not be resolved.
    #[serde(default)]
    pub root: Option<String>,
}

impl JobRunRecord {
    /// A record for an occurrence that never ran (missed or interrupted
    /// before a result), stamped now.
    #[must_use]
    pub fn not_run(
        job: &str,
        run_id: &str,
        trigger: RunTrigger,
        schedule: &JobSchedule,
        scheduled_for: DateTime<Utc>,
        outcome: Outcome,
        error: String,
    ) -> Self {
        let now = Utc::now();
        Self {
            job: job.to_string(),
            run_id: Some(run_id.to_string()),
            trigger,
            schedule_kind: Some(schedule.kind()),
            scheduled_for: Some(scheduled_for.to_rfc3339()),
            lateness_secs: Some((now - scheduled_for).num_seconds()),
            thread_id: None,
            attempt: 0,
            max_attempts: 0,
            started_at: now.to_rfc3339(),
            finished_at: now.to_rfc3339(),
            duration_ms: 0,
            ok: false,
            outcome: Some(outcome),
            error: Some(error),
            schedule: Some(schedule.describe()),
            model: None,
            subagent: None,
            root: None,
        }
    }
}

/// Scheduler bookkeeping that must survive restarts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SchedulerState {
    /// Recurring occurrences at or before this instant are never run or
    /// counted as missed. Set at the scheduler's first start on this home (so
    /// the first start after a migration does not repeat runs the old daemon
    /// did), and raised to the claim-pruning cutoff before claims are pruned
    /// (so a clock moved back cannot rerun an occurrence whose claim is gone).
    pub since: DateTime<Utc>,
    /// Latest tick. Never moves backward, even when the clock does.
    pub last_checked_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
struct MissedCheckpoint {
    through: DateTime<Utc>,
}

/// An exclusive per-job lock, released on drop or when the process dies.
#[derive(Debug)]
pub struct JobLock {
    _file: File,
}

impl JobsHome {
    /// The home of the current process (`$ZDX_HOME`).
    #[must_use]
    pub fn current() -> Self {
        Self::at(paths::zdx_home())
    }

    #[must_use]
    pub fn at(zdx_home: impl Into<PathBuf>) -> Self {
        Self {
            zdx_home: zdx_home.into(),
        }
    }

    #[must_use]
    pub fn zdx_home(&self) -> &Path {
        &self.zdx_home
    }

    /// `$ZDX_HOME/jobs`: one `<name>.md` per job.
    #[must_use]
    pub fn jobs_dir(&self) -> PathBuf {
        self.zdx_home.join("jobs")
    }

    /// `$ZDX_HOME/jobs/archive`: once jobs that reached a final outcome.
    #[must_use]
    pub fn archive_dir(&self) -> PathBuf {
        self.jobs_dir().join("archive")
    }

    /// `$ZDX_HOME/automations`: the directory used before the Jobs rename.
    #[must_use]
    pub fn legacy_dir(&self) -> PathBuf {
        self.zdx_home.join("automations")
    }

    #[must_use]
    pub fn runs_log(&self) -> PathBuf {
        self.zdx_home.join("jobs_runs.jsonl")
    }

    #[must_use]
    pub fn legacy_runs_log(&self) -> PathBuf {
        self.zdx_home.join("automations_runs.jsonl")
    }

    fn state_dir(&self) -> PathBuf {
        self.zdx_home.join("state").join("jobs")
    }

    fn job_state_dir(&self, job: &str) -> PathBuf {
        self.state_dir().join(job)
    }

    fn claim_path(&self, job: &str, key: &str) -> PathBuf {
        self.job_state_dir(job)
            .join("claims")
            .join(format!("{key}.json"))
    }

    // ---- claims ----

    /// Creates `claim` only if no claim exists for its occurrence. The file
    /// appears complete or not at all (written aside, then hard-linked).
    ///
    /// # Errors
    /// Returns an error on I/O failure.
    pub fn create_claim(&self, claim: &Claim) -> Result<bool> {
        let path = self.claim_path(&claim.job, &claim.key);
        let tmp = write_aside(&path, &serde_json::to_vec_pretty(claim)?)?;
        let linked = fs::hard_link(&tmp, &path);
        let _ = fs::remove_file(&tmp);
        match linked {
            Ok(()) => Ok(true),
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => Ok(false),
            Err(err) => Err(err).with_context(|| format!("create claim {}", path.display())),
        }
    }

    /// Replaces an existing claim (for example with its outcome).
    ///
    /// # Errors
    /// Returns an error on I/O failure.
    pub fn update_claim(&self, claim: &Claim) -> Result<()> {
        let path = self.claim_path(&claim.job, &claim.key);
        write_atomic(&path, &serde_json::to_vec_pretty(claim)?)
    }

    /// # Errors
    /// Returns an error when the claim exists but cannot be read or parsed.
    pub fn read_claim(&self, job: &str, key: &str) -> Result<Option<Claim>> {
        read_json(&self.claim_path(job, key))
    }

    /// Removes finished recurring claims scheduled before `before`. Once-job
    /// claims stay: they keep a restored file from running again.
    ///
    /// # Errors
    /// Returns an error when the state directory cannot be read.
    pub fn prune_recurring_claims(&self, before: DateTime<Utc>) -> Result<usize> {
        let mut removed = 0;
        for claim in self.all_claims()? {
            if claim.schedule_kind == ScheduleKind::Recurring
                && claim.outcome.is_some()
                && claim.scheduled_for < before
                && fs::remove_file(self.claim_path(&claim.job, &claim.key)).is_ok()
            {
                removed += 1;
            }
        }
        Ok(removed)
    }

    /// Every claim of every job. Scans the state directory, so call it at
    /// daemon start and on its periodic rescan, not per tick.
    ///
    /// # Errors
    /// Returns an error when the state directory cannot be read.
    pub fn all_claims(&self) -> Result<Vec<Claim>> {
        let mut out = Vec::new();
        let state = self.state_dir();
        if !state.is_dir() {
            return Ok(out);
        }
        for job in fs::read_dir(&state).with_context(|| format!("read {}", state.display()))? {
            let claims = job?.path().join("claims");
            let Ok(entries) = fs::read_dir(&claims) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().is_some_and(|ext| ext == "json") {
                    match read_json::<Claim>(&path) {
                        Ok(Some(claim)) => out.push(claim),
                        Ok(None) => {}
                        Err(err) => tracing::warn!(
                            path = %path.display(),
                            err = format!("{err:#}"),
                            "Skipping unreadable job claim"
                        ),
                    }
                }
            }
        }
        Ok(out)
    }

    // ---- lock ----

    /// Takes the job's run lock, or `None` when another process holds it.
    ///
    /// # Errors
    /// Returns an error when the lock file cannot be opened or locked.
    pub fn try_lock_job(&self, job: &str) -> Result<Option<JobLock>> {
        let path = self.job_state_dir(job).join("lock");
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
        }
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&path)
            .with_context(|| format!("open job lock {}", path.display()))?;
        // A status probe holds the lock for microseconds; retry briefly so
        // it does not turn into "already running".
        for attempt in 0..3 {
            match file.try_lock() {
                Ok(()) => return Ok(Some(JobLock { _file: file })),
                Err(TryLockError::WouldBlock) if attempt < 2 => {
                    std::thread::sleep(Duration::from_millis(20));
                }
                Err(TryLockError::WouldBlock) => return Ok(None),
                Err(TryLockError::Error(err)) => {
                    return Err(err).with_context(|| format!("lock {}", path.display()));
                }
            }
        }
        Ok(None)
    }

    /// Whether some process holds the job's run lock right now.
    #[must_use]
    pub fn is_job_running(&self, job: &str) -> bool {
        let path = self.job_state_dir(job).join("lock");
        let Ok(file) = File::open(&path) else {
            return false;
        };
        matches!(file.try_lock_shared(), Err(TryLockError::WouldBlock))
    }

    // ---- last outcome ----

    /// # Errors
    /// Returns an error on I/O failure.
    pub fn write_last_run(&self, job: &str, last: &LastRun) -> Result<()> {
        write_atomic(
            &self.job_state_dir(job).join("last.json"),
            &serde_json::to_vec_pretty(last)?,
        )
    }

    #[must_use]
    pub fn last_run(&self, job: &str) -> Option<LastRun> {
        read_json(&self.job_state_dir(job).join("last.json"))
            .ok()
            .flatten()
    }

    /// Rewrites `last.json` of each job whose newest final claim is newer
    /// than it (or which has none), from `claims`. Jobs whose lock is held
    /// are skipped: their run writes `last.json` itself. Returns the jobs
    /// repaired.
    ///
    /// # Errors
    /// Returns an error when a lock or `last.json` cannot be written.
    pub fn reconcile_last_runs(&self, claims: &[Claim]) -> Result<Vec<String>> {
        let mut newest: std::collections::BTreeMap<&str, LastRun> =
            std::collections::BTreeMap::new();
        for claim in claims {
            if let Some(last) = claim.to_last_run()
                && newest
                    .get(claim.job.as_str())
                    .is_none_or(|current| current.finished_at < last.finished_at)
            {
                newest.insert(&claim.job, last);
            }
        }
        let mut repaired = Vec::new();
        for (job, from_claim) in newest {
            let stale = self
                .last_run(job)
                .is_none_or(|last| last.finished_at < from_claim.finished_at);
            if !stale {
                continue;
            }
            let Some(_lock) = self.try_lock_job(job)? else {
                continue;
            };
            // Re-read under the lock: the run may have just written it.
            if self
                .last_run(job)
                .is_none_or(|last| last.finished_at < from_claim.finished_at)
            {
                self.write_last_run(job, &from_claim)?;
                repaired.push(job.to_string());
            }
        }
        Ok(repaired)
    }

    /// Latest recurring occurrence already recorded as missed.
    #[must_use]
    pub fn missed_through(&self, job: &str) -> Option<DateTime<Utc>> {
        read_json::<MissedCheckpoint>(&self.job_state_dir(job).join("missed.json"))
            .ok()
            .flatten()
            .map(|checkpoint| checkpoint.through)
    }

    /// Records that recurring occurrences up to `through` were reported as
    /// missed. Never moves backward.
    ///
    /// # Errors
    /// Returns an error on I/O failure.
    pub fn set_missed_through(&self, job: &str, through: DateTime<Utc>) -> Result<()> {
        let through = self
            .missed_through(job)
            .map_or(through, |old| old.max(through));
        write_atomic(
            &self.job_state_dir(job).join("missed.json"),
            &serde_json::to_vec_pretty(&MissedCheckpoint { through })?,
        )
    }

    // ---- run log ----

    /// Appends one record to `jobs_runs.jsonl`. Appends from concurrent runs
    /// are serialized by a lock file, and each record is one write.
    ///
    /// # Errors
    /// Returns an error on I/O failure.
    pub fn append_run_record(&self, record: &JobRunRecord) -> Result<()> {
        let path = self.runs_log();
        let _lock = self.lock_runs_log()?;
        let mut line = serde_json::to_vec(record).context("serialize job run record")?;
        line.push(b'\n');
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .with_context(|| format!("open job runs log {}", path.display()))?;
        file.write_all(&line)
            .with_context(|| format!("append {}", path.display()))
    }

    /// Exclusive lock for writers of `jobs_runs.jsonl` (appends and the
    /// migration's rewrite). Blocks until free.
    pub(crate) fn lock_runs_log(&self) -> Result<File> {
        fs::create_dir_all(&self.zdx_home)
            .with_context(|| format!("create {}", self.zdx_home.display()))?;
        let path = self.zdx_home.join("jobs_runs.lock");
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&path)
            .with_context(|| format!("open {}", path.display()))?;
        file.lock()
            .with_context(|| format!("lock {}", path.display()))?;
        Ok(file)
    }

    /// Reads `jobs_runs.jsonl` (empty when it does not exist). A line that
    /// does not parse is skipped with a warning, so one damaged line does not
    /// hide the history.
    ///
    /// # Errors
    /// Returns an error when the log cannot be read.
    pub fn read_run_records(&self) -> Result<Vec<JobRunRecord>> {
        let path = self.runs_log();
        let raw = match fs::read_to_string(&path) {
            Ok(raw) => raw,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(err) => return Err(err).with_context(|| format!("read {}", path.display())),
        };
        Ok(raw
            .lines()
            .enumerate()
            .filter(|(_, line)| !line.trim().is_empty())
            .filter_map(|(index, line)| match serde_json::from_str(line.trim()) {
                Ok(record) => Some(record),
                Err(err) => {
                    tracing::warn!(
                        path = %path.display(),
                        line = index + 1,
                        err = %err,
                        "Skipping unreadable job run record"
                    );
                    None
                }
            })
            .collect())
    }

    // ---- scheduler state ----

    #[must_use]
    pub fn scheduler_state(&self) -> Option<SchedulerState> {
        read_json(&self.state_dir().join("scheduler.json"))
            .ok()
            .flatten()
    }

    /// # Errors
    /// Returns an error on I/O failure.
    pub fn save_scheduler_state(&self, state: &SchedulerState) -> Result<()> {
        write_atomic(
            &self.state_dir().join("scheduler.json"),
            &serde_json::to_vec_pretty(state)?,
        )
    }

    // ---- archive ----

    /// Moves the once job at `path` to `jobs/archive/<name>--<key>.md` when it
    /// still declares `run_at`. A file that was rescheduled, made invalid, or
    /// removed stays where it is. Returns the archive path when moved.
    ///
    /// Job files have no writer lock, so the move is built to lose nothing if
    /// the file is edited meanwhile: the archive name is hard-linked
    /// (never replacing an existing file) and checked again after the link;
    /// then whatever is at `path` is taken aside with one rename, and if that
    /// is not the archived file (it was replaced), it is put back.
    ///
    /// # Errors
    /// Returns an error when a step fails. On error, every version of the
    /// file is still at `path`, in the archive, or beside `path` as
    /// `.<name>.md.archiving-<uuid>`, never lost.
    pub fn archive_once_job(
        &self,
        path: &Path,
        run_at: DateTime<FixedOffset>,
    ) -> Result<Option<PathBuf>> {
        self.archive_once_job_with(path, run_at, || {})
    }

    fn archive_once_job_with(
        &self,
        path: &Path,
        run_at: DateTime<FixedOffset>,
        after_link: impl FnOnce(),
    ) -> Result<Option<PathBuf>> {
        let Some(definition) = once_definition_at(path, run_at)? else {
            return Ok(None);
        };
        let dir = self.archive_dir();
        fs::create_dir_all(&dir).with_context(|| format!("create {}", dir.display()))?;
        let stem = format!("{}--{}", definition.name, once_occurrence(run_at).key);
        let mut n = 1;
        let dest = loop {
            let dest = if n == 1 {
                dir.join(format!("{stem}.md"))
            } else {
                dir.join(format!("{stem}-{n}.md"))
            };
            match fs::hard_link(path, &dest) {
                Ok(()) => break dest,
                Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => n += 1,
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
                Err(err) => {
                    return Err(err)
                        .with_context(|| format!("link {} to {}", path.display(), dest.display()));
                }
            }
        };
        after_link();

        // An in-place edit since the first check changes the linked file too.
        if once_definition_at(&dest, run_at)?.is_none() {
            fs::remove_file(&dest).with_context(|| format!("remove {}", dest.display()))?;
            return Ok(None);
        }

        let file_name = path
            .file_name()
            .and_then(std::ffi::OsStr::to_str)
            .context("invalid job file name")?;
        let aside = path.with_file_name(format!(".{file_name}.archiving-{}", uuid::Uuid::new_v4()));
        match fs::rename(path, &aside) {
            Ok(()) => {}
            // Removed meanwhile: the archived copy is the only one left.
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Some(dest)),
            Err(err) => {
                return Err(err).with_context(|| format!("move {} aside", path.display()));
            }
        }
        if same_file(&aside, &dest)? {
            fs::remove_file(&aside).with_context(|| format!("remove {}", aside.display()))?;
            return Ok(Some(dest));
        }
        // `path` was replaced by a newer version after the link: keep it.
        fs::hard_link(&aside, path).with_context(|| {
            format!(
                "{} was replaced while archiving and a newer file now exists there; the replaced \
                 version is kept at {}",
                path.display(),
                aside.display()
            )
        })?;
        fs::remove_file(&aside).with_context(|| format!("remove {}", aside.display()))?;
        Ok(Some(dest))
    }

    // ---- archived listing ----

    /// Once jobs in `jobs/archive/`, newest scheduled first, each with the
    /// final outcome of its claim. Reads one claim per archived file.
    ///
    /// # Errors
    /// Returns an error when the archive directory cannot be listed.
    pub fn archived(&self) -> Result<Vec<ArchivedJob>> {
        let mut out = Vec::new();
        for path in super::markdown_files(&self.archive_dir())? {
            let stem = path
                .file_stem()
                .and_then(std::ffi::OsStr::to_str)
                .unwrap_or_default()
                .to_string();
            let (name, key) = split_archive_stem(&stem);
            let claim = key
                .as_deref()
                .and_then(|key| self.read_claim(&name, key).ok().flatten());
            out.push(ArchivedJob {
                name,
                path,
                scheduled_for: claim.as_ref().map(|claim| claim.scheduled_for),
                last_run: claim.as_ref().and_then(Claim::to_last_run),
                key,
            });
        }
        out.sort_by(|a, b| {
            b.scheduled_for
                .cmp(&a.scheduled_for)
                .then_with(|| a.name.cmp(&b.name))
        });
        Ok(out)
    }
}

/// One archived once job.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ArchivedJob {
    pub name: String,
    pub path: PathBuf,
    /// Occurrence key from the file name; `None` for a file not named by
    /// the archiver.
    pub key: Option<String>,
    pub scheduled_for: Option<DateTime<Utc>>,
    /// Final outcome of the occurrence; `None` when its claim is missing.
    pub last_run: Option<LastRun>,
}

/// `<name>--<YYYYMMDDTHHMMSSZ>[-N]` → `(name, Some(key))`.
fn split_archive_stem(stem: &str) -> (String, Option<String>) {
    let Some((name, rest)) = stem.rsplit_once("--") else {
        return (stem.to_string(), None);
    };
    let key = rest.split_once('-').map_or(rest, |(key, _)| key);
    let valid = key.len() == 16
        && key.ends_with('Z')
        && chrono::NaiveDateTime::parse_from_str(key, "%Y%m%dT%H%M%SZ").is_ok();
    if valid {
        (name.to_string(), Some(key.to_string()))
    } else {
        (stem.to_string(), None)
    }
}

/// The definition at `path` when it parses and is a once job at `run_at`.
fn once_definition_at(
    path: &Path,
    run_at: DateTime<FixedOffset>,
) -> Result<Option<super::JobDefinition>> {
    let content = match fs::read_to_string(path) {
        Ok(content) => content,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(err).with_context(|| format!("read {}", path.display())),
    };
    let Ok(definition) = super::parse_job_content(path, &content, super::JobSource::User) else {
        return Ok(None);
    };
    Ok(
        matches!(definition.schedule, JobSchedule::Once { run_at: current } if current == run_at)
            .then_some(definition),
    )
}

#[cfg(unix)]
fn same_file(a: &Path, b: &Path) -> Result<bool> {
    use std::os::unix::fs::MetadataExt;
    let a = fs::metadata(a).with_context(|| format!("stat {}", a.display()))?;
    let b = fs::metadata(b).with_context(|| format!("stat {}", b.display()))?;
    Ok(a.dev() == b.dev() && a.ino() == b.ino())
}

#[cfg(not(unix))]
fn same_file(a: &Path, b: &Path) -> Result<bool> {
    Ok(
        fs::read(a).with_context(|| format!("read {}", a.display()))?
            == fs::read(b).with_context(|| format!("read {}", b.display()))?,
    )
}

/// Writes `bytes` to a unique temp file beside `path` and syncs it.
fn write_aside(path: &Path, bytes: &[u8]) -> Result<PathBuf> {
    let parent = path.parent().context("state path has no parent")?;
    fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    let name = path
        .file_name()
        .and_then(std::ffi::OsStr::to_str)
        .context("invalid state file name")?;
    let tmp = parent.join(format!(".{name}.{}.tmp", uuid::Uuid::new_v4()));
    let mut file = File::create(&tmp).with_context(|| format!("create {}", tmp.display()))?;
    file.write_all(bytes)
        .and_then(|()| file.sync_all())
        .with_context(|| format!("write {}", tmp.display()))?;
    Ok(tmp)
}

fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let tmp = write_aside(path, bytes)?;
    fs::rename(&tmp, path).map_err(|err| {
        let _ = fs::remove_file(&tmp);
        anyhow::Error::new(err).context(format!("replace {}", path.display()))
    })
}

fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Result<Option<T>> {
    match fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map(Some)
            .with_context(|| format!("parse {}", path.display())),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(err).with_context(|| format!("read {}", path.display())),
    }
}

/// Error for a run refused because the job already has a live run. The CLI
/// exits with [`JobBusy::EXIT_CODE`] so the scheduler can try again later.
#[derive(Debug)]
pub struct JobBusy {
    pub job: String,
}

impl JobBusy {
    pub const EXIT_CODE: i32 = 75;
}

impl std::fmt::Display for JobBusy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "job '{}' is already running", self.job)
    }
}

impl std::error::Error for JobBusy {}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::*;

    fn claim(job: &str, key: &str) -> Claim {
        Claim::new(job, key, RunTrigger::Daemon, ScheduleKind::Once, Utc::now())
    }

    #[test]
    fn a_claim_is_created_once_and_finished_in_place() {
        let dir = tempdir().unwrap();
        let home = JobsHome::at(dir.path());
        let first = claim("demo", "20261020T120000Z");
        assert!(home.create_claim(&first).unwrap());
        assert!(
            !home
                .create_claim(&claim("demo", "20261020T120000Z"))
                .unwrap()
        );
        let stored = home
            .read_claim("demo", "20261020T120000Z")
            .unwrap()
            .unwrap();
        assert_eq!(stored.run_id, first.run_id, "the first claim wins");
        assert!(stored.outcome.is_none());

        home.update_claim(&stored.finished(Outcome::Succeeded, None))
            .unwrap();
        let claims = home.all_claims().unwrap();
        assert_eq!(claims.len(), 1);
        assert!(claims[0].outcome.is_some());
        let leftovers = fs::read_dir(dir.path().join("state/jobs/demo/claims"))
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name().to_string_lossy().ends_with(".tmp"))
            .count();
        assert_eq!(leftovers, 0);
    }

    #[test]
    fn the_lock_is_exclusive_and_released_on_drop() {
        let dir = tempdir().unwrap();
        let home = JobsHome::at(dir.path());
        assert!(!home.is_job_running("demo"));
        let lock = home.try_lock_job("demo").unwrap().expect("free");
        assert!(home.is_job_running("demo"));
        assert!(home.try_lock_job("demo").unwrap().is_none());
        drop(lock);
        assert!(!home.is_job_running("demo"));
        assert!(home.try_lock_job("demo").unwrap().is_some());
    }

    #[test]
    fn pruning_keeps_once_claims_and_recent_or_unfinished_recurring_ones() {
        let dir = tempdir().unwrap();
        let home = JobsHome::at(dir.path());
        let old = Utc::now() - chrono::TimeDelta::days(30);
        let mut recurring_old = claim("r", "20260901T0900");
        recurring_old.schedule_kind = ScheduleKind::Recurring;
        recurring_old.scheduled_for = old;
        let recurring_old = recurring_old.finished(Outcome::Succeeded, None);
        let mut recurring_running = claim("r", "20260902T0900");
        recurring_running.schedule_kind = ScheduleKind::Recurring;
        recurring_running.scheduled_for = old;
        let mut once_old = claim("o", "20260901T120000Z").finished(Outcome::Failed, None);
        once_old.scheduled_for = old;
        for claim in [&recurring_old, &recurring_running, &once_old] {
            assert!(home.create_claim(claim).unwrap());
        }
        let removed = home
            .prune_recurring_claims(Utc::now() - chrono::TimeDelta::days(8))
            .unwrap();
        assert_eq!(removed, 1);
        assert!(home.read_claim("r", "20260901T0900").unwrap().is_none());
        assert!(home.read_claim("r", "20260902T0900").unwrap().is_some());
        assert!(home.read_claim("o", "20260901T120000Z").unwrap().is_some());
    }

    #[test]
    fn archive_moves_only_a_file_that_still_has_the_same_run_at() {
        let dir = tempdir().unwrap();
        let home = JobsHome::at(dir.path());
        fs::create_dir_all(home.jobs_dir()).unwrap();
        let path = home.jobs_dir().join("remind.md");
        let run_at = DateTime::parse_from_rfc3339("2026-10-20T09:00:00-03:00").unwrap();

        fs::write(
            &path,
            "---\nrun_at: \"2026-10-20T10:00:00-03:00\"\n---\nRescheduled.\n",
        )
        .unwrap();
        assert!(home.archive_once_job(&path, run_at).unwrap().is_none());
        assert!(path.exists(), "a rescheduled job stays");

        fs::write(
            &path,
            "---\nrun_at: \"2026-10-20T12:00:00Z\"\n---\nSame instant.\n",
        )
        .unwrap();
        let archived = home.archive_once_job(&path, run_at).unwrap().unwrap();
        assert_eq!(
            archived,
            home.archive_dir().join("remind--20261020T120000Z.md")
        );
        assert!(!path.exists());

        fs::write(
            &path,
            "---\nrun_at: \"2026-10-20T12:00:00Z\"\n---\nRestored.\n",
        )
        .unwrap();
        let second = home.archive_once_job(&path, run_at).unwrap().unwrap();
        assert_eq!(
            second,
            home.archive_dir().join("remind--20261020T120000Z-2.md")
        );
        assert!(
            home.archive_once_job(&path, run_at).unwrap().is_none(),
            "gone"
        );
    }

    fn once_job(home: &JobsHome, name: &str, run_at: &str, body: &str) -> PathBuf {
        fs::create_dir_all(home.jobs_dir()).unwrap();
        let path = home.jobs_dir().join(format!("{name}.md"));
        fs::write(&path, format!("---\nrun_at: \"{run_at}\"\n---\n{body}\n")).unwrap();
        path
    }

    /// Replaces `path` the way editors and agents do: write aside, rename.
    fn replace(path: &Path, content: &str) {
        let tmp = path.with_extension("md.tmp");
        fs::write(&tmp, content).unwrap();
        fs::rename(&tmp, path).unwrap();
    }

    #[test]
    fn archive_keeps_a_file_replaced_during_the_move() {
        let dir = tempdir().unwrap();
        let home = JobsHome::at(dir.path());
        let run_at = DateTime::parse_from_rfc3339("2026-10-20T12:00:00Z").unwrap();
        let path = once_job(&home, "remind", "2026-10-20T12:00:00Z", "Old.");
        let rescheduled = "---\nrun_at: \"2026-10-21T12:00:00Z\"\n---\nNew.\n";

        let dest = home
            .archive_once_job_with(&path, run_at, || replace(&path, rescheduled))
            .unwrap()
            .unwrap();
        assert!(fs::read_to_string(&dest).unwrap().contains("Old."));
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            rescheduled,
            "new version kept"
        );
        let strays = fs::read_dir(home.jobs_dir())
            .unwrap()
            .filter_map(Result::ok)
            .filter(|e| e.file_name().to_string_lossy().contains("archiving"))
            .count();
        assert_eq!(strays, 0);
    }

    #[test]
    fn archive_skips_a_file_edited_in_place_during_the_move() {
        let dir = tempdir().unwrap();
        let home = JobsHome::at(dir.path());
        let run_at = DateTime::parse_from_rfc3339("2026-10-20T12:00:00Z").unwrap();
        let path = once_job(&home, "remind", "2026-10-20T12:00:00Z", "Old.");
        let rescheduled = "---\nrun_at: \"2026-10-21T12:00:00Z\"\n---\nNew.\n";

        let result = home
            .archive_once_job_with(&path, run_at, || fs::write(&path, rescheduled).unwrap())
            .unwrap();
        assert!(result.is_none());
        assert_eq!(fs::read_to_string(&path).unwrap(), rescheduled);
        assert!(home.archived().unwrap().is_empty(), "no archive copy left");

        // Removed during the move: the archived copy is the only one, kept.
        let path = once_job(&home, "gone", "2026-10-20T12:00:00Z", "Bye.");
        let dest = home
            .archive_once_job_with(&path, run_at, || fs::remove_file(&path).unwrap())
            .unwrap()
            .unwrap();
        assert!(fs::read_to_string(dest).unwrap().contains("Bye."));
    }

    #[test]
    fn archive_never_replaces_an_existing_archive_file() {
        let dir = tempdir().unwrap();
        let home = JobsHome::at(dir.path());
        let run_at = DateTime::parse_from_rfc3339("2026-10-20T12:00:00Z").unwrap();
        fs::create_dir_all(home.archive_dir()).unwrap();
        let taken = home.archive_dir().join("remind--20261020T120000Z.md");
        fs::write(&taken, "earlier archive").unwrap();
        let path = once_job(&home, "remind", "2026-10-20T12:00:00Z", "Again.");
        let dest = home.archive_once_job(&path, run_at).unwrap().unwrap();
        assert_eq!(
            dest,
            home.archive_dir().join("remind--20261020T120000Z-2.md")
        );
        assert_eq!(fs::read_to_string(taken).unwrap(), "earlier archive");
    }

    #[test]
    fn reconcile_repairs_a_stale_last_run_from_a_final_claim_only() {
        let dir = tempdir().unwrap();
        let home = JobsHome::at(dir.path());
        let t = |minutes| Utc::now() - chrono::TimeDelta::minutes(minutes);
        let last = |run_id: &str, at| LastRun {
            run_id: run_id.to_string(),
            trigger: RunTrigger::Manual,
            outcome: Outcome::Succeeded,
            scheduled_for: None,
            finished_at: at,
            error: None,
            thread_id: None,
        };
        // "stale": the run finalized its claim, then died before last.json.
        home.write_last_run("stale", &last("older", t(60))).unwrap();
        let mut done = claim("stale", "k1").finished(Outcome::Failed, Some("boom".into()));
        done.finished_at = Some(t(5));
        // "newer": a later manual run (no claim) already wrote last.json.
        home.write_last_run("newer", &last("manual", t(1))).unwrap();
        let mut old = claim("newer", "k2").finished(Outcome::Succeeded, None);
        old.finished_at = Some(t(30));
        // "busy": its lock is held, so its run will write last.json itself.
        let busy = claim("busy", "k3").finished(Outcome::Succeeded, None);
        let unfinished = claim("missing", "k4");
        let _lock = home.try_lock_job("busy").unwrap().unwrap();

        let claims = [done.clone(), old, busy, unfinished];
        let repaired = home.reconcile_last_runs(&claims).unwrap();
        assert_eq!(repaired, ["stale"]);
        let fixed = home.last_run("stale").unwrap();
        assert_eq!(fixed.run_id, done.run_id);
        assert_eq!(fixed.outcome, Outcome::Failed);
        assert_eq!(home.last_run("newer").unwrap().run_id, "manual");
        assert!(home.last_run("busy").is_none());
        assert!(home.last_run("missing").is_none());
        assert!(
            home.reconcile_last_runs(&claims).unwrap().is_empty(),
            "idempotent"
        );
    }

    #[test]
    fn missed_checkpoint_never_moves_backward() {
        let dir = tempdir().unwrap();
        let home = JobsHome::at(dir.path());
        let later = Utc::now();
        let earlier = later - chrono::TimeDelta::hours(1);
        assert!(home.missed_through("poll").is_none());
        home.set_missed_through("poll", later).unwrap();
        home.set_missed_through("poll", earlier).unwrap();
        assert_eq!(home.missed_through("poll"), Some(later));
    }

    #[test]
    fn archived_lists_each_file_with_its_claim_outcome() {
        let dir = tempdir().unwrap();
        let home = JobsHome::at(dir.path());
        let run_at = DateTime::parse_from_rfc3339("2026-10-20T12:00:00Z").unwrap();
        let path = once_job(&home, "a--b", "2026-10-20T12:00:00Z", "Dashes in the name.");
        let mut taken = Claim::new(
            "a--b",
            "20261020T120000Z",
            RunTrigger::Daemon,
            ScheduleKind::Once,
            run_at.with_timezone(&Utc),
        );
        home.create_claim(&taken).unwrap();
        taken = taken.finished(Outcome::Missed, Some("late".into()));
        home.update_claim(&taken).unwrap();
        home.archive_once_job(&path, run_at).unwrap().unwrap();
        fs::write(home.archive_dir().join("hand-made.md"), "---\n---\nx\n").unwrap();

        let archived = home.archived().unwrap();
        assert_eq!(archived.len(), 2);
        assert_eq!(archived[0].name, "a--b");
        assert_eq!(archived[0].key.as_deref(), Some("20261020T120000Z"));
        assert_eq!(
            archived[0].last_run.as_ref().unwrap().outcome,
            Outcome::Missed
        );
        assert_eq!(archived[1].name, "hand-made");
        assert!(archived[1].key.is_none() && archived[1].last_run.is_none());
    }

    #[test]
    fn run_records_round_trip_and_old_lines_still_parse() {
        let dir = tempdir().unwrap();
        let home = JobsHome::at(dir.path());
        assert!(home.read_run_records().unwrap().is_empty());
        fs::write(
            home.runs_log(),
            "{\"job\":\"a\",\"trigger\":\"manual\",\"attempt\":1,\"max_attempts\":1,\"started_at\":\"2026-02-11T08:00:00Z\",\"finished_at\":\"2026-02-11T08:00:01Z\",\"duration_ms\":1000,\"ok\":true,\"error\":null,\"schedule\":null,\"model\":null,\"subagent\":null}\n",
        )
        .unwrap();
        let schedule = JobSchedule::parse(None, Some("2026-10-20T12:00:00Z")).unwrap();
        fs::write(
            home.runs_log(),
            format!("{}not json\n", fs::read_to_string(home.runs_log()).unwrap()),
        )
        .unwrap();
        let record = JobRunRecord::not_run(
            "b",
            "run-1",
            RunTrigger::Daemon,
            &schedule,
            Utc::now(),
            Outcome::Missed,
            "late".to_string(),
        );
        home.append_run_record(&record).unwrap();
        let records = home.read_run_records().unwrap();
        assert_eq!(records.len(), 2);
        assert!(records[0].outcome.is_none());
        assert_eq!(records[1], record);
    }
}
