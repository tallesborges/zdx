//! Read-only view of `$ZDX_HOME/jobs_runs.jsonl`, the run log the jobs
//! program writes (docs/SPEC.md §8 Jobs, Records). Rust reads it for a job's
//! history; it never writes it.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::config::paths;

/// One `ZDX_HOME`, for the jobs files Rust reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobsHome {
    zdx_home: PathBuf,
}

/// What started a run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RunTrigger {
    Manual,
    /// Records written before the scheduler was renamed say `daemon`.
    #[serde(alias = "daemon")]
    Scheduler,
}

impl RunTrigger {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Manual => "manual",
            Self::Scheduler => "scheduler",
        }
    }
}

/// Result of an attempt or an occurrence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Succeeded,
    Failed,
    Retrying,
    Missed,
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

/// The fields of one run-log line that history reads. Fields added after the
/// first release are optional so older lines still parse.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobRunRecord {
    pub job: String,
    #[serde(default)]
    pub run_id: Option<String>,
    pub trigger: RunTrigger,
    #[serde(default)]
    pub scheduled_for: Option<String>,
    #[serde(default)]
    pub thread_id: Option<String>,
    pub attempt: u32,
    pub started_at: String,
    pub finished_at: String,
    pub ok: bool,
    #[serde(default)]
    pub outcome: Option<Outcome>,
    #[serde(default)]
    pub error: Option<String>,
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

    #[must_use]
    pub fn runs_log(&self) -> PathBuf {
        self.zdx_home.join("jobs_runs.jsonl")
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
}
