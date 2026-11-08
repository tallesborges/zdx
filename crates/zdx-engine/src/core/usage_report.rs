//! Usage report: recorded token usage in a day window, split by project and by
//! the kind of run that made the requests.
//!
//! This is the shared source for the Mini App Usage view and `zdx stats`.
//!
//! What the numbers mean:
//! - Tokens are what ZDX recorded from provider responses (`usage` events in
//!   thread files). They are not provider quota: a subscription meters its own
//!   way, and requests made outside ZDX are not seen.
//! - Every request is counted once. A child run (subagent, helper, worker)
//!   records its own usage in its own thread file and never in its parent's, so
//!   per-thread rows are disjoint and the groups of each split sum to the
//!   totals.
//! - Attribution comes from thread metadata in the thread index. A thread the
//!   index does not know, or one without a recorded root, stays unknown; no
//!   project or run kind is guessed for it.
//!
//! Project rule: a top-level thread or worker counts toward its own root. A
//! subagent or helper run counts toward the project of the nearest ancestor
//! that is not itself a child run (it works on that thread's behalf; some, like
//! artifact specialists, run inside an artifact directory). When that ancestor
//! is not in the index, the run falls back to its own root.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use anyhow::Result;
use serde::Serialize;

use crate::core::thread_index;
use crate::core::thread_persistence::ThreadSummary;
use crate::core::usage_stats::{self, ThreadUsageRow, UsageStats};

/// Window the Mini App and `zdx stats --days` start from.
pub const DEFAULT_DAYS: u32 = 7;

/// Parent hops followed to find a child run's project. Lineage is shallow in
/// practice (a helper inside a subagent inside a worker is three).
const MAX_LINEAGE_HOPS: usize = 8;

/// Token counts for one group.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct TokenCounts {
    pub requests: u64,
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write: u64,
}

impl TokenCounts {
    /// Total tokens across all token classes.
    pub fn tokens(&self) -> u64 {
        self.input + self.output + self.cache_read + self.cache_write
    }

    fn add(&mut self, other: &TokenCounts) {
        self.requests += other.requests;
        self.input += other.input;
        self.output += other.output;
        self.cache_read += other.cache_read;
        self.cache_write += other.cache_write;
    }

    fn add_row(&mut self, row: &ThreadUsageRow) {
        self.requests += row.requests;
        self.input += row.input;
        self.output += row.output;
        self.cache_read += row.cache_read;
        self.cache_write += row.cache_write;
    }
}

/// Usage of one project (a thread root directory).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProjectUsage {
    /// Root directory. `None` when the thread is not in the index or has no
    /// recorded root.
    pub project: Option<String>,
    /// Threads with usage in the window.
    pub threads: usize,
    pub tokens: u64,
    #[serde(flatten)]
    pub counts: TokenCounts,
}

/// The kind of run that made requests.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RunKind {
    /// Top-level thread started by the user, without a persistent profile.
    Agent,
    /// Top-level thread with a persistent profile (e.g. `orchestrator`).
    Profile,
    /// Orchestrator worker.
    Worker,
    /// Named subagent run (`invoke_subagent`).
    Subagent,
    /// Internal helper run (title, handoff, `read_thread`, ...).
    Helper,
    /// Child run with an origin kind this report does not know.
    Other,
    /// Thread not in the index.
    Unknown,
}

/// Usage of one run kind and name.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RunUsage {
    pub kind: RunKind,
    /// Profile, subagent, or helper name; the raw origin kind for `other`.
    pub name: Option<String>,
    pub threads: usize,
    pub tokens: u64,
    #[serde(flatten)]
    pub counts: TokenCounts,
}

/// Usage in a window with its project and run-kind splits.
#[derive(Debug, Clone, Serialize)]
pub struct UsageReport {
    /// Window length in days, ending today (UTC). `None` for all time.
    pub days: Option<u32>,
    /// Totals, providers, models, and the daily series.
    pub stats: UsageStats,
    /// Sorted by tokens, descending.
    pub by_project: Vec<ProjectUsage>,
    /// Sorted by tokens, descending.
    pub by_run: Vec<RunUsage>,
    /// Threads with usage in the window that the thread index does not know.
    pub unindexed_threads: usize,
}

/// Builds the report for the last `days` days (today included), or all time.
///
/// `default_model` attributes usage events without a recorded model, as in
/// [`usage_stats::aggregate_usage`].
///
/// # Errors
/// Returns an error when usage cannot be aggregated. A thread index failure
/// does not fail the report: attribution becomes unknown and a warning is
/// added.
pub fn report(default_model: &str, days: Option<u32>) -> Result<UsageReport> {
    let since_day = days.map(|days| {
        let back = i32::try_from(days.max(1) - 1).unwrap_or(i32::MAX);
        usage_stats::today_utc().saturating_sub(back)
    });
    let (mut stats, rows) = usage_stats::aggregate_usage_by_thread(default_model, since_day)?;
    let lineage = match load_lineage(&rows) {
        Ok(lineage) => lineage,
        Err(err) => {
            stats.warnings.push(format!(
                "thread index unavailable, attribution unknown: {err:#}"
            ));
            HashMap::new()
        }
    };
    Ok(build_report(days, stats, &rows, &lineage))
}

/// Summaries for every thread in `rows`, plus the ancestors of child runs.
fn load_lineage(rows: &[ThreadUsageRow]) -> Result<HashMap<String, ThreadSummary>> {
    let ids: BTreeSet<&str> = rows.iter().map(|row| row.thread_id.as_str()).collect();
    let ids: Vec<&str> = ids.into_iter().collect();
    let mut lineage = thread_index::summaries_for_ids(&ids)?;
    for _ in 0..MAX_LINEAGE_HOPS {
        let missing: BTreeSet<String> = lineage
            .values()
            .filter(|summary| summary.is_child_run())
            .filter_map(|summary| summary.parent_thread_id.clone())
            .filter(|parent| !lineage.contains_key(parent))
            .collect();
        if missing.is_empty() {
            break;
        }
        let missing: Vec<&str> = missing.iter().map(String::as_str).collect();
        let found = thread_index::summaries_for_ids(&missing)?;
        if found.is_empty() {
            break;
        }
        lineage.extend(found);
    }
    Ok(lineage)
}

/// Groups per-thread rows by project and run kind. Pure; `lineage` holds what
/// the index knows.
fn build_report(
    days: Option<u32>,
    stats: UsageStats,
    rows: &[ThreadUsageRow],
    lineage: &HashMap<String, ThreadSummary>,
) -> UsageReport {
    let mut per_thread: BTreeMap<&str, TokenCounts> = BTreeMap::new();
    for row in rows {
        per_thread
            .entry(row.thread_id.as_str())
            .or_default()
            .add_row(row);
    }

    let mut projects: BTreeMap<Option<String>, (usize, TokenCounts)> = BTreeMap::new();
    let mut runs: BTreeMap<(RunKind, Option<String>), (usize, TokenCounts)> = BTreeMap::new();
    let mut unindexed_threads = 0;
    for (id, counts) in &per_thread {
        let summary = lineage.get(*id);
        if summary.is_none() {
            unindexed_threads += 1;
        }
        let project = summary.and_then(|summary| project_of(summary, lineage));
        let entry = projects.entry(project).or_default();
        entry.0 += 1;
        entry.1.add(counts);

        let entry = runs.entry(run_of(summary)).or_default();
        entry.0 += 1;
        entry.1.add(counts);
    }

    let mut by_project: Vec<ProjectUsage> = projects
        .into_iter()
        .map(|(project, (threads, counts))| ProjectUsage {
            project,
            threads,
            tokens: counts.tokens(),
            counts,
        })
        .collect();
    by_project.sort_by(|a, b| b.tokens.cmp(&a.tokens).then(a.project.cmp(&b.project)));

    let mut by_run: Vec<RunUsage> = runs
        .into_iter()
        .map(|((kind, name), (threads, counts))| RunUsage {
            kind,
            name,
            threads,
            tokens: counts.tokens(),
            counts,
        })
        .collect();
    by_run.sort_by(|a, b| {
        b.tokens
            .cmp(&a.tokens)
            .then(a.kind.cmp(&b.kind))
            .then(a.name.cmp(&b.name))
    });

    UsageReport {
        days,
        stats,
        by_project,
        by_run,
        unindexed_threads,
    }
}

/// The project a thread's usage counts toward (see the module docs).
fn project_of(summary: &ThreadSummary, lineage: &HashMap<String, ThreadSummary>) -> Option<String> {
    let mut current = summary;
    for _ in 0..MAX_LINEAGE_HOPS {
        if !current.is_child_run() {
            break;
        }
        match current
            .parent_thread_id
            .as_deref()
            .and_then(|parent| lineage.get(parent))
        {
            Some(parent) => current = parent,
            None => break,
        }
    }
    current.root_path.clone()
}

fn run_of(summary: Option<&ThreadSummary>) -> (RunKind, Option<String>) {
    let Some(summary) = summary else {
        return (RunKind::Unknown, None);
    };
    if summary.is_worker() {
        return (RunKind::Worker, None);
    }
    match summary.origin_kind.as_deref() {
        None => match &summary.subagent_name {
            Some(profile) => (RunKind::Profile, Some(profile.clone())),
            None => (RunKind::Agent, None),
        },
        Some("subagent") => (RunKind::Subagent, summary.subagent_name.clone()),
        Some(origin) => match origin.strip_prefix("helper:") {
            Some(helper) => (RunKind::Helper, Some(helper.to_string())),
            None => (RunKind::Other, Some(origin.to_string())),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::usage_stats::UsageTotals;

    fn row(thread_id: &str, model: &str, input: u64, cache_read: u64) -> ThreadUsageRow {
        ThreadUsageRow {
            thread_id: thread_id.to_string(),
            provider: "test-provider".to_string(),
            model: model.to_string(),
            requests: 1,
            input,
            output: 0,
            cache_read,
            cache_write: 0,
        }
    }

    fn summary(
        id: &str,
        root: Option<&str>,
        origin: Option<&str>,
        parent: Option<&str>,
        name: Option<&str>,
    ) -> ThreadSummary {
        ThreadSummary {
            id: id.to_string(),
            root_path: root.map(str::to_string),
            origin_kind: origin.map(str::to_string),
            parent_thread_id: parent.map(str::to_string),
            subagent_name: name.map(str::to_string),
            ..ThreadSummary::default()
        }
    }

    fn empty_stats() -> UsageStats {
        UsageStats {
            totals: UsageTotals::default(),
            by_provider: Vec::new(),
            by_model: Vec::new(),
            daily: Vec::new(),
            threads_scanned: 0,
            warnings: Vec::new(),
        }
    }

    #[test]
    fn groups_by_project_and_run_without_guessing() {
        let lineage: HashMap<String, ThreadSummary> = [
            summary("agent", Some("/p/app"), None, None, None),
            summary("orch", Some("/p"), None, None, Some("orchestrator")),
            summary("worker", Some("/p/lib"), None, Some("orch"), None),
            // Runs in an artifact directory; counts toward its parent's project.
            summary(
                "html",
                Some("/home/.zdx/artifacts/x"),
                Some("subagent"),
                Some("agent"),
                Some("html-artifact-specialist"),
            ),
            // Helper inside a subagent inside a worker: resolves to the worker.
            summary(
                "explore",
                Some("/p/lib"),
                Some("subagent"),
                Some("worker"),
                Some("explorer"),
            ),
            summary(
                "reader",
                None,
                Some("helper:read_thread"),
                Some("explore"),
                None,
            ),
            // Parent not indexed: falls back to its own root.
            summary(
                "orphan",
                Some("/p/other"),
                Some("subagent"),
                Some("gone"),
                Some("oracle"),
            ),
            summary("rootless", None, None, None, None),
        ]
        .into_iter()
        .map(|s| (s.id.clone(), s))
        .collect();

        let rows = vec![
            row("agent", "m1", 100, 0),
            row("agent", "m2", 10, 0),
            row("orch", "m1", 50, 0),
            row("worker", "m1", 40, 0),
            row("html", "m1", 30, 1000),
            row("explore", "m1", 20, 0),
            row("reader", "m1", 5, 0),
            row("orphan", "m1", 7, 0),
            row("rootless", "m1", 3, 0),
            row("missing", "m1", 2, 0),
        ];
        let report = build_report(Some(7), empty_stats(), &rows, &lineage);

        let projects: Vec<(Option<&str>, usize, u64)> = report
            .by_project
            .iter()
            .map(|p| (p.project.as_deref(), p.threads, p.tokens))
            .collect();
        assert_eq!(
            projects,
            vec![
                (Some("/p/app"), 2, 1140),
                (Some("/p/lib"), 3, 65),
                (Some("/p"), 1, 50),
                (Some("/p/other"), 1, 7),
                (None, 2, 5),
            ]
        );

        let runs: Vec<(RunKind, Option<&str>, usize, u64)> = report
            .by_run
            .iter()
            .map(|r| (r.kind, r.name.as_deref(), r.threads, r.tokens))
            .collect();
        assert_eq!(
            runs,
            vec![
                (RunKind::Subagent, Some("html-artifact-specialist"), 1, 1030),
                (RunKind::Agent, None, 2, 113),
                (RunKind::Profile, Some("orchestrator"), 1, 50),
                (RunKind::Worker, None, 1, 40),
                (RunKind::Subagent, Some("explorer"), 1, 20),
                (RunKind::Subagent, Some("oracle"), 1, 7),
                (RunKind::Helper, Some("read_thread"), 1, 5),
                (RunKind::Unknown, None, 1, 2),
            ]
        );
        assert_eq!(report.unindexed_threads, 1);

        // Each split counts every request exactly once.
        let total: u64 = rows.iter().map(ThreadUsageRow::tokens).sum();
        let by_project: u64 = report.by_project.iter().map(|p| p.tokens).sum();
        let by_run: u64 = report.by_run.iter().map(|r| r.tokens).sum();
        assert_eq!((by_project, by_run), (total, total));
        let requests: u64 = report.by_run.iter().map(|r| r.counts.requests).sum();
        assert_eq!(requests, 10);
    }
}
