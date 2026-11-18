//! Usage/cost aggregation over saved thread JSONL.
//!
//! Sums token usage across every saved thread under `threads_dir()`, grouped
//! per provider and per model, applying the shared `ModelPricing` cost path.
//! This is the data source for `zdx stats` and the monitor Usage tab.
//!
//! To stay fast at thousands of threads, results are backed by a derived,
//! disposable `SQLite` cache (`$ZDX_HOME/cache/usage-v3.sqlite`) that stores each
//! thread's partial aggregate keyed by `(thread_id, mtime, size)`; only
//! changed or new threads are re-scanned on each run. JSONL stays canonical —
//! the cache is rebuilt transparently if missing or corrupt, and the
//! aggregator falls back to a full lean scan if the cache is unavailable.
//!
//! The cache does not depend on the caller's `default_model`: usage that would
//! fall back to it is stored unattributed and attributed when read. Callers
//! with different configs (the bot, the Monitor, `zdx stats`) share it
//! without invalidating each other.
//!
//! Attribution: usage events carry per-request `model`/`provider`; older events
//! without them fall back to the thread's `model_override` or the supplied
//! `default_model`, and such rows are marked `estimated`.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior};

use crate::config::paths;
use crate::core::{thread_index, thread_persistence};
use crate::models::ModelOption;
use crate::providers::{self, ProviderKind};

/// One aggregated row (a provider total, or a single provider+model total).
#[derive(Debug, Clone, serde::Serialize)]
pub struct UsageRow {
    /// Provider id (e.g. `anthropic`, `claude-cli`).
    pub provider: String,
    /// Bare model id. `None` for provider-level rows.
    pub model: Option<String>,
    /// Number of usage events attributed to this row.
    pub requests: u64,
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write: u64,
    /// Billed USD. Zero for subscription providers and rows with no known pricing.
    pub cost_usd: f64,
    /// Provider is subscription-based (flat rate; token cost is not real spend).
    pub subscription: bool,
    /// Pricing was found in the registry for this row.
    pub cost_known: bool,
    /// Any usage in this row was attributed by best-effort fallback rather than
    /// an explicit per-request provider (see the aggregator resolution order).
    pub estimated: bool,
}

impl UsageRow {
    /// Total tokens across all token classes.
    pub fn tokens(&self) -> u64 {
        self.input + self.output + self.cache_read + self.cache_write
    }
}

/// Overall totals across all scanned threads.
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct UsageTotals {
    pub requests: u64,
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write: u64,
    /// Sum of billed USD (excludes subscription providers and unknown-pricing rows).
    pub billed_usd: f64,
    /// Tokens spent on subscription providers (not billed per-token).
    pub subscription_tokens: u64,
    /// Number of (provider, model) rows with no known pricing.
    pub unknown_pricing_rows: u64,
}

impl UsageTotals {
    pub fn tokens(&self) -> u64 {
        self.input + self.output + self.cache_read + self.cache_write
    }
}

/// Request and token counts of some usage.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
pub struct UsageCounts {
    pub requests: u64,
    /// Tokens across all classes.
    pub tokens: u64,
    /// Tokens without cache reads: input, output, and cache writes.
    pub uncached_tokens: u64,
}

impl UsageCounts {
    fn add_bucket(&mut self, bucket: &RawBucket) {
        self.requests += bucket.requests;
        self.tokens += bucket.tokens();
        self.uncached_tokens += bucket.input + bucket.output + bucket.cache_write;
    }

    fn add(&mut self, other: &UsageCounts) {
        self.requests += other.requests;
        self.tokens += other.tokens;
        self.uncached_tokens += other.uncached_tokens;
    }
}

/// Usage on one UTC day, for the daily usage chart.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct DailyUsage {
    /// UTC day number (days since the Unix epoch).
    pub day: i32,
    #[serde(flatten)]
    pub counts: UsageCounts,
    /// The day's usage per provider, sorted by provider id. Empty on a day
    /// with no usage.
    pub by_provider: Vec<DailyProviderUsage>,
}

/// One provider's usage on one day.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct DailyProviderUsage {
    pub provider: String,
    #[serde(flatten)]
    pub counts: UsageCounts,
}

/// Aggregated usage statistics ready for display.
#[derive(Debug, Clone, serde::Serialize)]
pub struct UsageStats {
    pub totals: UsageTotals,
    /// Per-provider rows, sorted by total tokens (descending).
    pub by_provider: Vec<UsageRow>,
    /// Per provider+model rows, sorted by total tokens (descending).
    pub by_model: Vec<UsageRow>,
    /// Per-day usage, ascending by day. A bounded window has one entry for
    /// every day from its first day through today, idle days included; all
    /// time has only the days with usage. Excludes usage with no parseable
    /// timestamp (`UNKNOWN_DAY`).
    pub daily: Vec<DailyUsage>,
    /// Usage in the same number of days just before a bounded window, for a
    /// comparison. `None` for all time.
    pub previous: Option<UsageCounts>,
    /// Number of thread files successfully scanned.
    pub threads_scanned: usize,
    /// Non-fatal issues (e.g. an unreadable thread file that was skipped).
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Default)]
struct RawBucket {
    requests: u64,
    input: u64,
    output: u64,
    cache_read: u64,
    cache_write: u64,
    /// True if any contributing event lacked an explicit provider and had to
    /// be attributed by best-effort fallback (thread `model_override` or bare
    /// model resolution).
    estimated: bool,
}

impl RawBucket {
    fn add_event(
        &mut self,
        input: u64,
        output: u64,
        cache_read: u64,
        cache_write: u64,
        estimated: bool,
    ) {
        self.requests += 1;
        self.input += input;
        self.output += output;
        self.cache_read += cache_read;
        self.cache_write += cache_write;
        self.estimated |= estimated;
    }

    fn merge(&mut self, other: &RawBucket) {
        self.requests += other.requests;
        self.input += other.input;
        self.output += other.output;
        self.cache_read += other.cache_read;
        self.cache_write += other.cache_write;
        self.estimated |= other.estimated;
    }

    fn tokens(&self) -> u64 {
        self.input + self.output + self.cache_read + self.cache_write
    }
}

/// Aggregate usage across all saved threads.
///
/// `default_model` is used to attribute usage for threads that have no
/// `model_override` (typically the active `config.model`).
///
/// Backed by a derived `SQLite` cache so repeat runs only re-scan changed
/// threads. Per-thread read failures are collected into `warnings` and
/// skipped. If the cache is unavailable, falls back to a full lean scan.
///
/// # Errors
/// Returns an error only if both the cache path and a full scan fail (e.g. the
/// threads directory cannot be read).
/// `since_day` restricts the aggregate to usage on or after that UTC day number
/// (days since the Unix epoch); `None` aggregates all time. Usage events whose
/// timestamp cannot be parsed are treated as unknown-day and only appear in the
/// all-time view.
pub fn aggregate_usage(default_model: &str, since_day: Option<i32>) -> Result<UsageStats> {
    aggregate_usage_at(
        &paths::threads_dir(),
        &cache_path(),
        default_model,
        since_day,
    )
}

/// One thread's usage for one `(provider, model)` within the requested window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThreadUsageRow {
    pub thread_id: String,
    pub provider: String,
    pub model: String,
    pub requests: u64,
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write: u64,
}

impl ThreadUsageRow {
    /// Total tokens across all token classes.
    pub fn tokens(&self) -> u64 {
        self.input + self.output + self.cache_read + self.cache_write
    }
}

/// Like [`aggregate_usage`], plus the same usage split per thread.
///
/// Both halves come from one cache sync and one read snapshot, so the
/// per-thread rows sum exactly to the returned totals. Every usage event lives
/// in the thread file of the run that made the request (a child run's usage is
/// never copied into its parent), so the rows never count a request twice.
///
/// # Errors
/// Same as [`aggregate_usage`].
pub fn aggregate_usage_by_thread(
    default_model: &str,
    since_day: Option<i32>,
) -> Result<(UsageStats, Vec<ThreadUsageRow>)> {
    aggregate_at(
        &paths::threads_dir(),
        &cache_path(),
        default_model,
        since_day,
        true,
    )
}

/// UTC day number (days since the Unix epoch) for the current time.
#[must_use]
pub fn today_utc() -> i32 {
    i32::try_from(chrono::Utc::now().timestamp().div_euclid(86_400)).unwrap_or(0)
}

/// UTC day number for an RFC3339 timestamp string. `None` when the timestamp is
/// missing or unparseable; such events are bucketed under `UNKNOWN_DAY`.
fn day_from_ts(ts: Option<&str>) -> i32 {
    ts.and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
        .and_then(|dt| i32::try_from(dt.timestamp().div_euclid(86_400)).ok())
        .unwrap_or(UNKNOWN_DAY)
}

/// Sentinel day for usage events with no parseable timestamp. Sorts before any
/// real day so a `day >= since_day` filter excludes it from bounded spans while
/// the all-time view (no filter) still counts it.
const UNKNOWN_DAY: i32 = i32::MIN;

/// Aggregates usage/cost for a single thread by id, reusing the same lean
/// scan, attribution, and costing path as the global aggregator.
///
/// `default_model` attributes usage events that lack a per-request
/// model/provider (older transcripts), typically the active `config.model`.
///
/// # Errors
/// Returns an error if the thread file cannot be read or parsed.
pub fn thread_usage_stats(thread_id: &str, default_model: &str) -> Result<UsageStats> {
    thread_usage_stats_at(&paths::threads_dir(), thread_id, default_model)
}

fn thread_usage_stats_at(
    threads_dir: &Path,
    thread_id: &str,
    default_model: &str,
) -> Result<UsageStats> {
    let path = threads_dir.join(format!("{thread_id}.jsonl"));
    let scan = scan_thread_file(&path)?;
    let day_buckets = resolve_thread_buckets(&scan, Some(default_model));
    let (daily, previous) = daily_series(day_rows(&day_buckets), None, today_utc());
    let buckets = collapse_days(day_buckets, None);
    Ok(finalize(buckets, daily, previous, 1, Vec::new()))
}

fn aggregate_usage_at(
    threads_dir: &Path,
    cache_path: &Path,
    default_model: &str,
    since_day: Option<i32>,
) -> Result<UsageStats> {
    aggregate_at(threads_dir, cache_path, default_model, since_day, false).map(|(stats, _)| stats)
}

/// Shared entry: cache first, full scan as the fallback. Per-thread rows are
/// collected only when `by_thread` is set; otherwise the list is empty.
fn aggregate_at(
    threads_dir: &Path,
    cache_path: &Path,
    default_model: &str,
    since_day: Option<i32>,
    by_thread: bool,
) -> Result<(UsageStats, Vec<ThreadUsageRow>)> {
    match aggregate_cached(threads_dir, cache_path, default_model, since_day, by_thread) {
        Ok(result) => Ok(result),
        Err(err) => {
            tracing::debug!("usage cache unavailable ({err:#}); falling back to full scan");
            aggregate_scan_all(threads_dir, default_model, since_day, by_thread)
        }
    }
}

/// Full (uncached) scan of every thread. Used as the fallback when the cache
/// cannot be opened.
fn aggregate_scan_all(
    threads_dir: &Path,
    default_model: &str,
    since_day: Option<i32>,
    by_thread: bool,
) -> Result<(UsageStats, Vec<ThreadUsageRow>)> {
    let files = thread_persistence::list_thread_files(threads_dir)?;
    let mut all_days: BTreeMap<(String, String, i32), RawBucket> = BTreeMap::new();
    let mut thread_rows = Vec::new();
    let mut warnings = Vec::new();
    let mut threads_scanned = 0usize;

    for file in &files {
        match scan_thread_file(&file.path) {
            Ok(scan) => {
                threads_scanned += 1;
                let buckets = resolve_thread_buckets(&scan, Some(default_model));
                for (key, bucket) in &buckets {
                    all_days.entry(key.clone()).or_default().merge(bucket);
                }
                if by_thread {
                    for ((provider, model), bucket) in collapse_days(buckets, since_day) {
                        thread_rows.push(thread_row(file.id.clone(), provider, model, &bucket));
                    }
                }
            }
            Err(err) => warnings.push(format!("skipped thread {}: {err}", file.id)),
        }
    }

    let (daily, previous) = daily_series(day_rows(&all_days), since_day, today_utc());
    let raw = collapse_days(all_days, since_day);
    Ok((
        finalize(raw, daily, previous, threads_scanned, warnings),
        thread_rows,
    ))
}

fn thread_row(
    thread_id: String,
    provider: String,
    model: String,
    bucket: &RawBucket,
) -> ThreadUsageRow {
    ThreadUsageRow {
        thread_id,
        provider,
        model,
        requests: bucket.requests,
        input: bucket.input,
        output: bucket.output,
        cache_read: bucket.cache_read,
        cache_write: bucket.cache_write,
    }
}

/// Lists every thread with its `(mtime_ns, size)` change token.
///
/// Prefers the thread index, which already walks the threads directory on its
/// own freshness window and stores exactly these tokens: re-deriving them here
/// meant a second `stat()` of the whole corpus on the dashboard path, for
/// information already in a table. The tokens come from the same
/// `ThreadFileMeta` fields, so they compare directly against this cache's own.
///
/// Falls back to the directory walk when `threads_dir` is not the configured
/// one (tests point it at a temp dir) or when the index is unavailable.
fn thread_file_tokens(threads_dir: &Path) -> Result<Vec<thread_index::IndexedThreadFile>> {
    if threads_dir == paths::threads_dir() {
        match thread_index::indexed_thread_files() {
            Ok(files) => return Ok(files),
            Err(err) => {
                tracing::debug!("thread index unavailable for usage stats ({err:#}); walking");
            }
        }
    }
    Ok(thread_persistence::list_thread_files(threads_dir)?
        .into_iter()
        .map(|file| thread_index::IndexedThreadFile {
            id: file.id,
            mtime_ns: mtime_nanos(file.modified),
            size: i64_from(file.size),
        })
        .collect())
}

/// Cache-backed aggregation: syncs only changed/new threads into the `SQLite`
/// cache, then sums all cached per-thread buckets.
fn aggregate_cached(
    threads_dir: &Path,
    cache_path: &Path,
    default_model: &str,
    since_day: Option<i32>,
    by_thread: bool,
) -> Result<(UsageStats, Vec<ThreadUsageRow>)> {
    let files = thread_file_tokens(threads_dir)?;
    let conn = open_cache(cache_path)?;
    ensure_cache_valid(&conn)?;

    let cached_meta = load_thread_meta(&conn)?;
    let current_ids: HashSet<&str> = files.iter().map(|file| file.id.as_str()).collect();
    let changed: Vec<&thread_index::IndexedThreadFile> = files
        .iter()
        .filter(|file| cached_meta.get(&file.id) != Some(&(file.mtime_ns, file.size)))
        .collect();
    let removed: Vec<&str> = cached_meta
        .keys()
        .map(String::as_str)
        .filter(|id| !current_ids.contains(id))
        .collect();
    let mut threads_scanned = files.len() - changed.len();
    let mut warnings = Vec::new();

    // Transcripts are parsed with no lock held, and each chunk is written in
    // its own short write transaction. Holding the write lock across the scan
    // (a cold rebuild takes about a minute) made every concurrent caller fail
    // with `SQLITE_BUSY` and fall back to a full uncached scan.
    let chunks: Vec<_> = changed.chunks(SYNC_CHUNK).collect();
    // Concurrent rebuilds start at different chunks, so each one skips what
    // the others finished instead of scanning the same chunks in lockstep.
    let start = rotation_start(chunks.len());
    for chunk in chunks[start..].iter().chain(&chunks[..start]).copied() {
        // Another caller may have synced these since `cached_meta` was read
        // (it started its rebuild first); skip what is already current.
        let mut stale = Vec::with_capacity(chunk.len());
        for file in chunk {
            if cached_token(&conn, &file.id)? == Some((file.mtime_ns, file.size)) {
                threads_scanned += 1;
            } else {
                stale.push(*file);
            }
        }
        if stale.is_empty() {
            continue;
        }
        let scanned: Vec<_> = stale
            .into_iter()
            .map(|file| {
                let path = threads_dir.join(format!("{}.jsonl", file.id));
                let buckets =
                    scan_thread_file(&path).map(|scan| resolve_thread_buckets(&scan, None));
                (file, buckets)
            })
            .collect();
        let tx = Transaction::new_unchecked(&conn, TransactionBehavior::Immediate)?;
        for (file, buckets) in scanned {
            match buckets {
                Ok(buckets) => {
                    replace_thread_rows(&tx, &file.id, file.mtime_ns, file.size, &buckets)?;
                    threads_scanned += 1;
                }
                Err(err) => {
                    // Drop any now-stale cached rows so the aggregate matches a
                    // full scan (which omits unreadable threads), then warn.
                    delete_thread_rows(&tx, &file.id)?;
                    warnings.push(format!("skipped thread {}: {err}", file.id));
                }
            }
        }
        tx.commit()?;
    }
    // Drop cache rows for threads that no longer exist on disk.
    if !removed.is_empty() {
        let tx = Transaction::new_unchecked(&conn, TransactionBehavior::Immediate)?;
        for id in removed {
            delete_thread_rows(&tx, id)?;
        }
        tx.commit()?;
    }

    // One read transaction, so totals and the daily series come from the same
    // snapshot even while another caller commits.
    let tx = conn.unchecked_transaction()?;
    let default_key = default_attribution(default_model);
    let raw = load_all_buckets(&tx, since_day, &default_key)?;
    let today = today_utc();
    let rows = load_daily_rows(&tx, since_day.map(|since| previous_start(since, today)))?;
    let thread_rows = if by_thread {
        load_thread_rows(&tx, since_day, &default_key)?
    } else {
        Vec::new()
    };
    tx.commit()?;
    let rows = rows.into_iter().map(|(day, provider, counts)| {
        let provider = if provider == UNATTRIBUTED_PROVIDER {
            default_key.0.clone()
        } else {
            provider
        };
        (day, provider, counts)
    });
    let (daily, previous) = daily_series(rows, since_day, today);
    Ok((
        finalize(raw, daily, previous, threads_scanned, warnings),
        thread_rows,
    ))
}

/// Changed threads parsed and written per write transaction during a sync.
const SYNC_CHUNK: usize = 256;

/// A random index below `len` (0 when empty).
fn rotation_start(len: usize) -> usize {
    let seed =
        std::hash::BuildHasher::hash_one(&std::collections::hash_map::RandomState::new(), ());
    u64::try_from(len)
        .ok()
        .filter(|len| *len > 0)
        .and_then(|len| usize::try_from(seed % len).ok())
        .unwrap_or(0)
}

/// Provider of cached rows whose usage falls back to the caller's
/// `default_model`. No real provider id is empty, so it cannot collide.
const UNATTRIBUTED_PROVIDER: &str = "";

/// `(provider, model)` that unattributed usage is credited to for this caller.
fn default_attribution(default_model: &str) -> (String, String) {
    let selection = providers::resolve_provider(default_model);
    (selection.kind.id().to_string(), selection.model)
}

/// Resolves a thread's usage into per-`(provider, model, day)` buckets, applying
/// the thread-level fallback (`model_override`, else `default_model`) to usage
/// that lacks a per-request provider/model. With `default_model` `None` (the
/// cache), usage that would need it is keyed under [`UNATTRIBUTED_PROVIDER`].
fn resolve_thread_buckets(
    scan: &ThreadUsageScan,
    default_model: Option<&str>,
) -> BTreeMap<(String, String, i32), RawBucket> {
    // A `mode:<name>` override names no model: what it meant when the usage
    // was recorded is unknown, so it gives no attribution of its own.
    let thread_model = scan
        .model_override
        .as_deref()
        .filter(|model| crate::config::mode_ref_name(model).is_none());
    let fallback_key = match (thread_model, default_model) {
        (Some(model), _) | (None, Some(model)) => default_attribution(model),
        (None, None) => (UNATTRIBUTED_PROVIDER.to_string(), String::new()),
    };

    let mut buckets: BTreeMap<(String, String, i32), RawBucket> = BTreeMap::new();
    for usage in &scan.usages {
        let ((provider, model), estimated) = attribute_event(
            usage.provider.as_deref(),
            usage.model.as_deref(),
            &fallback_key,
        );
        buckets
            .entry((provider, model, usage.day))
            .or_default()
            .add_event(
                usage.input,
                usage.output,
                usage.cache_read,
                usage.cache_write,
                estimated,
            );
    }
    buckets
}

/// Collapses day-keyed buckets into `(provider, model)` totals, keeping only
/// days on or after `since_day` (all days when `None`).
fn collapse_days(
    day_buckets: BTreeMap<(String, String, i32), RawBucket>,
    since_day: Option<i32>,
) -> BTreeMap<(String, String), RawBucket> {
    let mut out: BTreeMap<(String, String), RawBucket> = BTreeMap::new();
    for ((provider, model, day), bucket) in day_buckets {
        if since_day.is_some_and(|min| day < min) {
            continue;
        }
        out.entry((provider, model)).or_default().merge(&bucket);
    }
    out
}

/// `(day, provider, counts)` rows of day-keyed buckets.
fn day_rows(
    day_buckets: &BTreeMap<(String, String, i32), RawBucket>,
) -> impl Iterator<Item = (i32, String, UsageCounts)> + '_ {
    day_buckets.iter().map(|((provider, _, day), bucket)| {
        let mut counts = UsageCounts::default();
        counts.add_bucket(bucket);
        (*day, provider.clone(), counts)
    })
}

/// First day of the span just before a window that starts on `since_day` and
/// ends `today`: the span has the window's length.
fn previous_start(since_day: i32, today: i32) -> i32 {
    let len = today.saturating_sub(since_day).saturating_add(1).max(1);
    since_day.saturating_sub(len)
}

/// Builds the daily series and the previous-span totals from
/// `(day, provider, counts)` rows. With `since_day`, the series has every day
/// from `since_day` through `today` (and any later day with usage), so idle
/// days are present with zero counts, and the previous span is
/// [`previous_start`] up to `since_day`. Without it, the series has only the
/// days with usage and there is no previous span. Unknown-day usage
/// (`UNKNOWN_DAY`) is never in either.
fn daily_series(
    rows: impl IntoIterator<Item = (i32, String, UsageCounts)>,
    since_day: Option<i32>,
    today: i32,
) -> (Vec<DailyUsage>, Option<UsageCounts>) {
    let mut by_day: BTreeMap<i32, BTreeMap<String, UsageCounts>> = BTreeMap::new();
    let mut previous = since_day.map(|_| UsageCounts::default());
    let previous_from = since_day.map(|since| previous_start(since, today));
    for (day, provider, counts) in rows {
        if day == UNKNOWN_DAY {
            continue;
        }
        match since_day {
            Some(since) if day < since => {
                if let (Some(previous), Some(from)) = (previous.as_mut(), previous_from)
                    && day >= from
                {
                    previous.add(&counts);
                }
            }
            _ => by_day
                .entry(day)
                .or_default()
                .entry(provider)
                .or_default()
                .add(&counts),
        }
    }
    if let Some(since) = since_day {
        let last = by_day
            .keys()
            .next_back()
            .copied()
            .unwrap_or(today)
            .max(today);
        for day in since..=last {
            by_day.entry(day).or_default();
        }
    }
    let daily = by_day
        .into_iter()
        .map(|(day, providers)| {
            let mut counts = UsageCounts::default();
            let by_provider = providers
                .into_iter()
                .map(|(provider, provider_counts)| {
                    counts.add(&provider_counts);
                    DailyProviderUsage {
                        provider,
                        counts: provider_counts,
                    }
                })
                .collect();
            DailyUsage {
                day,
                counts,
                by_provider,
            }
        })
        .collect();
    (daily, previous)
}

/// Converts a file mtime to nanoseconds-since-epoch for the cache key.
fn mtime_nanos(modified: Option<SystemTime>) -> i64 {
    modified
        .and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok())
        .map_or(0, |d| i64::try_from(d.as_nanos()).unwrap_or(i64::MAX))
}

// ── Derived SQLite cache ────────────────────────────────────────────────────

/// Bumped when the cache table layout or row meaning changes; a mismatch drops
/// and rebuilds the per-thread tables.
const CACHE_SCHEMA_VERSION: &str = "3";

const CREATE_META_SQL: &str =
    "CREATE TABLE IF NOT EXISTS cache_meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);";

const CREATE_DATA_SQL: &str = "\
CREATE TABLE IF NOT EXISTS thread_meta (
    thread_id TEXT PRIMARY KEY,
    mtime_ns INTEGER NOT NULL,
    size INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS thread_usage (
    thread_id TEXT NOT NULL,
    provider TEXT NOT NULL,
    model TEXT NOT NULL,
    day INTEGER NOT NULL,
    requests INTEGER NOT NULL,
    input INTEGER NOT NULL,
    output INTEGER NOT NULL,
    cache_read INTEGER NOT NULL,
    cache_write INTEGER NOT NULL,
    estimated INTEGER NOT NULL,
    PRIMARY KEY (thread_id, provider, model, day)
);
CREATE INDEX IF NOT EXISTS idx_thread_usage_day ON thread_usage(day);";

/// A new file name (not only a schema bump) for the default-model-free layout:
/// builds that still write `usage.sqlite` wipe it whenever their default model
/// or schema differs, and would keep wiping a shared file.
fn cache_path() -> PathBuf {
    paths::zdx_home().join("cache").join("usage-v3.sqlite")
}

/// Opens the cache, recreating the file once only if it is genuinely
/// corrupt/not-a-database. Transient errors (locks, permissions) propagate so
/// the caller falls back to a full scan instead of deleting a live cache.
fn open_cache(path: &Path) -> Result<Connection> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).context("create cache dir")?;
    }
    match try_open_cache(path) {
        Ok(conn) => Ok(conn),
        Err(err) if is_cache_corruption(&err) => {
            // Drop the corrupt file and its WAL/SHM sidecars, then rebuild.
            let _ = std::fs::remove_file(path);
            let _ = std::fs::remove_file(path.with_extension("sqlite-wal"));
            let _ = std::fs::remove_file(path.with_extension("sqlite-shm"));
            try_open_cache(path).context("recreate usage cache")
        }
        Err(err) => Err(err),
    }
}

/// True only for errors that mean the file isn't a usable `SQLite` database
/// (a not-a-database / corruption code). Lock, permission, and other
/// transient errors return false.
fn is_cache_corruption(err: &anyhow::Error) -> bool {
    err.downcast_ref::<rusqlite::Error>().is_some_and(|e| {
        matches!(
            e,
            rusqlite::Error::SqliteFailure(ffi, _)
                if matches!(
                    ffi.code,
                    rusqlite::ErrorCode::NotADatabase | rusqlite::ErrorCode::DatabaseCorrupt
                )
        )
    })
}

fn try_open_cache(path: &Path) -> Result<Connection> {
    let conn = Connection::open(path)?;
    conn.busy_timeout(CACHE_BUSY_TIMEOUT)?;
    enable_wal(&conn)?;
    conn.execute_batch("PRAGMA synchronous=NORMAL;")?;
    // No `integrity_check` here: it scans every page, so the cost grows with
    // the cache and every open pays it. The schema statement below fails with
    // a corruption code on a broken file, which is what `is_cache_corruption`
    // recovers from.
    conn.execute_batch(CREATE_META_SQL)?;
    Ok(conn)
}

const CACHE_BUSY_TIMEOUT: Duration = Duration::from_secs(5);

/// Switches the cache to WAL. The switch does not wait on the busy handler, so
/// callers that open a new cache file at the same time get `SQLITE_BUSY` here
/// (verified 2026-10-14); retry until one of them has switched the file.
fn enable_wal(conn: &Connection) -> Result<()> {
    let deadline = Instant::now() + CACHE_BUSY_TIMEOUT;
    loop {
        match conn.query_row("PRAGMA journal_mode=WAL", [], |_| Ok(())) {
            Err(rusqlite::Error::SqliteFailure(ffi, _))
                if ffi.code == rusqlite::ErrorCode::DatabaseBusy && Instant::now() < deadline =>
            {
                std::thread::sleep(Duration::from_millis(10));
            }
            result => return result.map_err(Into::into),
        }
    }
}

/// Ensures the cache matches the current schema version; a mismatch drops and
/// recreates the data tables.
///
/// A current cache takes no lock. Otherwise the version is checked again under
/// the write lock, so a caller that lost the race does not drop the tables that
/// another caller has just created and started to fill.
fn ensure_cache_valid(conn: &Connection) -> Result<()> {
    if schema_is_current(conn)? {
        return Ok(());
    }
    let tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)?;
    if !schema_is_current(&tx)? {
        tx.execute_batch("DROP TABLE IF EXISTS thread_meta; DROP TABLE IF EXISTS thread_usage;")?;
        tx.execute_batch(CREATE_DATA_SQL)?;
        write_meta(&tx, "schema_version", CACHE_SCHEMA_VERSION)?;
    }
    tx.commit()?;
    Ok(())
}

fn schema_is_current(conn: &Connection) -> Result<bool> {
    Ok(read_meta(conn, "schema_version")?.as_deref() == Some(CACHE_SCHEMA_VERSION))
}

fn read_meta(conn: &Connection, key: &str) -> Result<Option<String>> {
    conn.query_row("SELECT value FROM cache_meta WHERE key = ?1", [key], |r| {
        r.get::<_, String>(0)
    })
    .optional()
    .map_err(Into::into)
}

fn write_meta(conn: &Connection, key: &str, value: &str) -> Result<()> {
    conn.execute(
        "INSERT INTO cache_meta(key, value) VALUES(?1, ?2) \
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        (key, value),
    )?;
    Ok(())
}

fn load_thread_meta(conn: &Connection) -> Result<HashMap<String, (i64, i64)>> {
    let mut stmt = conn.prepare("SELECT thread_id, mtime_ns, size FROM thread_meta")?;
    let rows = stmt.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            (r.get::<_, i64>(1)?, r.get::<_, i64>(2)?),
        ))
    })?;
    let mut map = HashMap::new();
    for row in rows {
        let (id, meta) = row?;
        map.insert(id, meta);
    }
    Ok(map)
}

fn cached_token(conn: &Connection, id: &str) -> Result<Option<(i64, i64)>> {
    conn.prepare_cached("SELECT mtime_ns, size FROM thread_meta WHERE thread_id = ?1")?
        .query_row([id], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)))
        .optional()
        .map_err(Into::into)
}

fn replace_thread_rows(
    conn: &Connection,
    id: &str,
    mtime_ns: i64,
    size: i64,
    buckets: &BTreeMap<(String, String, i32), RawBucket>,
) -> Result<()> {
    conn.execute("DELETE FROM thread_usage WHERE thread_id = ?1", [id])?;
    {
        let mut stmt = conn.prepare_cached(
            "INSERT INTO thread_usage \
             (thread_id, provider, model, day, requests, input, output, cache_read, cache_write, estimated) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        )?;
        for ((provider, model, day), bucket) in buckets {
            stmt.execute(rusqlite::params![
                id,
                provider.as_str(),
                model.as_str(),
                day,
                i64_from(bucket.requests),
                i64_from(bucket.input),
                i64_from(bucket.output),
                i64_from(bucket.cache_read),
                i64_from(bucket.cache_write),
                bucket.estimated,
            ])?;
        }
    }
    conn.execute(
        "INSERT INTO thread_meta(thread_id, mtime_ns, size) VALUES(?1, ?2, ?3) \
         ON CONFLICT(thread_id) DO UPDATE SET mtime_ns = excluded.mtime_ns, size = excluded.size",
        (id, mtime_ns, size),
    )?;
    Ok(())
}

fn delete_thread_rows(conn: &Connection, id: &str) -> Result<()> {
    conn.execute("DELETE FROM thread_usage WHERE thread_id = ?1", [id])?;
    conn.execute("DELETE FROM thread_meta WHERE thread_id = ?1", [id])?;
    Ok(())
}

/// Loads and collapses all cached rows into `(provider, model)` totals, keeping
/// only days on or after `since_day` (all days when `None`). Unattributed rows
/// are credited to `default_key`.
fn load_all_buckets(
    conn: &Connection,
    since_day: Option<i32>,
    default_key: &(String, String),
) -> Result<BTreeMap<(String, String), RawBucket>> {
    let min_day = since_day.unwrap_or(i32::MIN);
    let mut stmt = conn.prepare(
        "SELECT provider, model, requests, input, output, cache_read, cache_write, estimated \
         FROM thread_usage WHERE day >= ?1",
    )?;
    let rows = stmt.query_map([min_day], |r| {
        Ok((
            (r.get::<_, String>(0)?, r.get::<_, String>(1)?),
            RawBucket {
                requests: u64_from(r.get::<_, i64>(2)?),
                input: u64_from(r.get::<_, i64>(3)?),
                output: u64_from(r.get::<_, i64>(4)?),
                cache_read: u64_from(r.get::<_, i64>(5)?),
                cache_write: u64_from(r.get::<_, i64>(6)?),
                estimated: r.get::<_, bool>(7)?,
            },
        ))
    })?;

    let mut map: BTreeMap<(String, String), RawBucket> = BTreeMap::new();
    for row in rows {
        let (key, bucket) = row?;
        let key = if key.0 == UNATTRIBUTED_PROVIDER {
            default_key.clone()
        } else {
            key
        };
        map.entry(key).or_default().merge(&bucket);
    }
    Ok(map)
}

/// Loads per-thread `(provider, model)` totals on or after `since_day`, summed
/// in SQL. Unattributed rows are credited to `default_key`, as in
/// [`load_all_buckets`].
fn load_thread_rows(
    conn: &Connection,
    since_day: Option<i32>,
    default_key: &(String, String),
) -> Result<Vec<ThreadUsageRow>> {
    let min_day = since_day.unwrap_or(i32::MIN);
    let mut stmt = conn.prepare(
        "SELECT thread_id, provider, model, SUM(requests), SUM(input), SUM(output), \
         SUM(cache_read), SUM(cache_write) \
         FROM thread_usage WHERE day >= ?1 GROUP BY thread_id, provider, model",
    )?;
    let rows = stmt.query_map([min_day], |r| {
        Ok((
            (
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
            ),
            RawBucket {
                requests: u64_from(r.get::<_, i64>(3)?),
                input: u64_from(r.get::<_, i64>(4)?),
                output: u64_from(r.get::<_, i64>(5)?),
                cache_read: u64_from(r.get::<_, i64>(6)?),
                cache_write: u64_from(r.get::<_, i64>(7)?),
                estimated: false,
            },
        ))
    })?;

    let mut merged: BTreeMap<(String, String, String), RawBucket> = BTreeMap::new();
    for row in rows {
        let ((thread_id, provider, model), bucket) = row?;
        let (provider, model) = if provider == UNATTRIBUTED_PROVIDER {
            default_key.clone()
        } else {
            (provider, model)
        };
        merged
            .entry((thread_id, provider, model))
            .or_default()
            .merge(&bucket);
    }
    Ok(merged
        .into_iter()
        .map(|((thread_id, provider, model), bucket)| {
            thread_row(thread_id, provider, model, &bucket)
        })
        .collect())
}

/// Loads the per-day token series from the cache, keeping only days on or after
/// `since_day` and excluding unknown-day usage. Sorted ascending by day.
/// Per `(day, provider)` counts on or after `min_day` (all days when `None`),
/// summed in SQL, without unknown-day usage. Unattributed rows keep
/// [`UNATTRIBUTED_PROVIDER`].
fn load_daily_rows(
    conn: &Connection,
    min_day: Option<i32>,
) -> Result<Vec<(i32, String, UsageCounts)>> {
    let mut stmt = conn.prepare(
        "SELECT day, provider, SUM(requests), SUM(input + output + cache_read + cache_write), \
         SUM(input + output + cache_write) \
         FROM thread_usage WHERE day >= ?1 AND day > ?2 GROUP BY day, provider",
    )?;
    let rows = stmt.query_map([min_day.unwrap_or(i32::MIN), UNKNOWN_DAY], |r| {
        Ok((
            r.get::<_, i32>(0)?,
            r.get::<_, String>(1)?,
            UsageCounts {
                requests: u64_from(r.get::<_, i64>(2)?),
                tokens: u64_from(r.get::<_, i64>(3)?),
                uncached_tokens: u64_from(r.get::<_, i64>(4)?),
            },
        ))
    })?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .map_err(Into::into)
}

fn i64_from(v: u64) -> i64 {
    i64::try_from(v).unwrap_or(i64::MAX)
}

fn u64_from(v: i64) -> u64 {
    u64::try_from(v).unwrap_or(0)
}

/// A minimally-parsed usage record from a thread's JSONL. Only the fields the
/// aggregator needs are deserialized.
struct LeanUsage {
    input: u64,
    output: u64,
    cache_read: u64,
    cache_write: u64,
    model: Option<String>,
    provider: Option<String>,
    /// UTC day number (days since the Unix epoch), or `UNKNOWN_DAY` when the
    /// event carried no parseable timestamp.
    day: i32,
}

/// Result of a lean per-thread scan: the thread's `model_override` (for
/// fallback attribution) and its usage records.
struct ThreadUsageScan {
    model_override: Option<String>,
    usages: Vec<LeanUsage>,
}

#[derive(serde::Deserialize)]
struct LineTag {
    #[serde(rename = "type")]
    ty: String,
}

#[derive(serde::Deserialize)]
struct UsageLine {
    #[serde(default)]
    input_tokens: u64,
    #[serde(default)]
    output_tokens: u64,
    #[serde(default)]
    cache_read_tokens: u64,
    #[serde(default)]
    cache_write_tokens: u64,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    provider: Option<String>,
    #[serde(default)]
    ts: Option<String>,
}

#[derive(serde::Deserialize)]
struct MetaLine {
    #[serde(default)]
    model_override: Option<String>,
}

/// Scans one thread's JSONL, extracting only `usage` and `meta` data. Each
/// line's `type` is peeked first — serde skips the other fields without
/// allocating them — so large message/reasoning/tool lines cost only
/// tokenization instead of a full `ThreadEvent` deserialization. Malformed
/// lines are skipped, matching the resilience of `read_events`.
fn scan_thread_file(path: &Path) -> Result<ThreadUsageScan> {
    let file = File::open(path).with_context(|| format!("open {}", path.display()))?;
    scan_usage_reader(BufReader::new(file)).with_context(|| format!("read {}", path.display()))
}

fn scan_usage_reader<R: BufRead>(reader: R) -> std::io::Result<ThreadUsageScan> {
    let mut model_override = None;
    let mut usages = Vec::new();

    for line in reader.lines() {
        let line = line?;
        if line.is_empty() {
            continue;
        }
        let Ok(tag) = serde_json::from_str::<LineTag>(&line) else {
            continue;
        };
        match tag.ty.as_str() {
            "usage" => {
                if let Ok(u) = serde_json::from_str::<UsageLine>(&line) {
                    usages.push(LeanUsage {
                        input: u.input_tokens,
                        output: u.output_tokens,
                        cache_read: u.cache_read_tokens,
                        cache_write: u.cache_write_tokens,
                        model: u.model,
                        provider: u.provider,
                        day: day_from_ts(u.ts.as_deref()),
                    });
                }
            }
            "meta" if model_override.is_none() => {
                if let Ok(m) = serde_json::from_str::<MetaLine>(&line) {
                    model_override = m.model_override;
                }
            }
            _ => {}
        }
    }

    Ok(ThreadUsageScan {
        model_override,
        usages,
    })
}

/// Resolves the `(provider, model)` bucket key for a usage event.
///
/// Resolution order: (a) explicit provider on the event → accurate; (b) model
/// present without provider → resolve any `provider:` prefix, else estimated;
/// (c) no attribution → thread-level fallback, estimated.
fn attribute_event(
    provider: Option<&str>,
    model: Option<&str>,
    fallback_key: &(String, String),
) -> ((String, String), bool) {
    let provider = provider.filter(|p| !p.is_empty());
    let model = model.filter(|m| !m.is_empty());
    match (provider, model) {
        (Some(provider), Some(model)) => ((provider.to_string(), model.to_string()), false),
        (Some(provider), None) => ((provider.to_string(), String::new()), true),
        (None, Some(model)) => {
            let selection = providers::resolve_provider(model);
            ((selection.kind.id().to_string(), selection.model), true)
        }
        (None, None) => (fallback_key.clone(), true),
    }
}

fn finalize(
    raw: BTreeMap<(String, String), RawBucket>,
    daily: Vec<DailyUsage>,
    previous: Option<UsageCounts>,
    threads_scanned: usize,
    warnings: Vec<String>,
) -> UsageStats {
    let mut by_model = Vec::with_capacity(raw.len());
    let mut provider_raw: BTreeMap<String, RawBucket> = BTreeMap::new();
    let mut provider_subscription: BTreeMap<String, bool> = BTreeMap::new();
    let mut provider_cost: BTreeMap<String, f64> = BTreeMap::new();
    let mut provider_unknown: BTreeMap<String, bool> = BTreeMap::new();
    let mut totals = UsageTotals::default();

    for ((provider, model), bucket) in raw {
        let subscription =
            ProviderKind::from_id(&provider).is_some_and(ProviderKind::is_subscription);
        let pricing = ModelOption::find_by_provider_and_id(&provider, &model).map(|m| m.pricing);
        let cost_known = pricing.is_some();
        let cost_usd = if subscription {
            0.0
        } else {
            pricing.map_or(0.0, |p| {
                p.cost(
                    bucket.input,
                    bucket.output,
                    bucket.cache_read,
                    bucket.cache_write,
                )
            })
        };

        totals.requests += bucket.requests;
        totals.input += bucket.input;
        totals.output += bucket.output;
        totals.cache_read += bucket.cache_read;
        totals.cache_write += bucket.cache_write;
        if subscription {
            totals.subscription_tokens += bucket.tokens();
        } else if cost_known {
            totals.billed_usd += cost_usd;
        } else {
            totals.unknown_pricing_rows += 1;
        }

        provider_raw
            .entry(provider.clone())
            .or_default()
            .merge(&bucket);
        provider_subscription.insert(provider.clone(), subscription);
        *provider_cost.entry(provider.clone()).or_insert(0.0) += cost_usd;
        let unknown = !subscription && !cost_known;
        let entry = provider_unknown.entry(provider.clone()).or_insert(false);
        *entry = *entry || unknown;

        by_model.push(UsageRow {
            provider,
            model: Some(model),
            requests: bucket.requests,
            input: bucket.input,
            output: bucket.output,
            cache_read: bucket.cache_read,
            cache_write: bucket.cache_write,
            cost_usd,
            subscription,
            cost_known,
            estimated: bucket.estimated,
        });
    }

    let mut by_provider: Vec<UsageRow> = provider_raw
        .into_iter()
        .map(|(provider, bucket)| {
            let subscription = provider_subscription
                .get(&provider)
                .copied()
                .unwrap_or(false);
            let has_unknown = provider_unknown.get(&provider).copied().unwrap_or(false);
            UsageRow {
                cost_usd: provider_cost.get(&provider).copied().unwrap_or(0.0),
                requests: bucket.requests,
                input: bucket.input,
                output: bucket.output,
                cache_read: bucket.cache_read,
                cache_write: bucket.cache_write,
                subscription,
                cost_known: !has_unknown,
                estimated: bucket.estimated,
                provider,
                model: None,
            }
        })
        .collect();

    by_provider.sort_by(|a, b| {
        b.tokens()
            .cmp(&a.tokens())
            .then(a.provider.cmp(&b.provider))
    });
    by_model.sort_by(|a, b| {
        b.tokens()
            .cmp(&a.tokens())
            .then(a.provider.cmp(&b.provider))
    });

    UsageStats {
        totals,
        by_provider,
        by_model,
        daily,
        previous,
        threads_scanned,
        warnings,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_thread(dir: &Path, id: &str, lines: &[String]) {
        let mut body = lines.join("\n");
        body.push('\n');
        std::fs::write(dir.join(format!("{id}.jsonl")), body).unwrap();
    }

    fn usage_line(input: u64, output: u64, model: Option<&str>, provider: Option<&str>) -> String {
        usage_line_at(input, output, model, provider, "t")
    }

    fn usage_line_at(
        input: u64,
        output: u64,
        model: Option<&str>,
        provider: Option<&str>,
        ts: &str,
    ) -> String {
        let attribution = match (model, provider) {
            (Some(m), Some(p)) => format!(r#","model":"{m}","provider":"{p}""#),
            _ => String::new(),
        };
        format!(
            r#"{{"type":"usage","input_tokens":{input},"output_tokens":{output},"cache_read_tokens":0,"cache_write_tokens":0{attribution},"ts":"{ts}"}}"#
        )
    }

    /// RFC3339 timestamp `days` before now, for building day-scoped fixtures.
    fn ts_days_ago(days: i64) -> String {
        (chrono::Utc::now() - chrono::Duration::days(days))
            .to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
    }

    #[test]
    fn thread_usage_stats_scopes_to_one_thread() {
        let dir = tempfile::tempdir().unwrap();
        let threads = dir.path().join("threads");
        std::fs::create_dir_all(&threads).unwrap();

        write_thread(
            &threads,
            "child",
            &[
                usage_line(100, 50, Some("gpt-5.5"), Some("openai-codex")),
                usage_line(20, 10, Some("gpt-5.5"), Some("openai-codex")),
            ],
        );
        // A sibling thread must NOT contribute to the single-thread stats.
        write_thread(
            &threads,
            "other",
            &[usage_line(999, 999, Some("gpt-5.5"), Some("openai-codex"))],
        );

        let stats = thread_usage_stats_at(&threads, "child", "claude-opus-4-8").unwrap();
        assert_eq!(stats.threads_scanned, 1);
        assert_eq!(stats.totals.requests, 2);
        assert_eq!((stats.totals.input, stats.totals.output), (120, 60));
        assert_eq!(stats.totals.tokens(), 180);
    }

    #[test]
    fn per_thread_rows_match_totals_in_cache_and_scan() {
        let dir = tempfile::tempdir().unwrap();
        let threads = dir.path().join("threads");
        std::fs::create_dir_all(&threads).unwrap();
        let cache = dir.path().join("cache/usage.sqlite");

        write_thread(
            &threads,
            "a",
            &[
                usage_line_at(
                    100,
                    50,
                    Some("gpt-5.5"),
                    Some("openai-codex"),
                    &ts_days_ago(1),
                ),
                // No attribution: credited to the caller's default model.
                usage_line_at(10, 5, None, None, &ts_days_ago(1)),
                // Outside the window.
                usage_line_at(
                    1000,
                    0,
                    Some("gpt-5.5"),
                    Some("openai-codex"),
                    &ts_days_ago(20),
                ),
            ],
        );
        write_thread(
            &threads,
            "b",
            &[usage_line_at(
                7,
                3,
                Some("gpt-5.5"),
                Some("openai-codex"),
                &ts_days_ago(0),
            )],
        );

        let since = Some(today_utc() - 6);
        let (cached_stats, mut cached) =
            aggregate_cached(&threads, &cache, "claude-opus-4-8", since, true).unwrap();
        let (scanned_stats, mut scanned) =
            aggregate_scan_all(&threads, "claude-opus-4-8", since, true).unwrap();
        cached.sort_by(|x, y| (&x.thread_id, &x.model).cmp(&(&y.thread_id, &y.model)));
        scanned.sort_by(|x, y| (&x.thread_id, &x.model).cmp(&(&y.thread_id, &y.model)));
        assert_eq!(cached, scanned);

        let default = default_attribution("claude-opus-4-8");
        assert_eq!(cached.len(), 3);
        assert!(cached.iter().any(|row| row.thread_id == "a"
            && (row.provider.as_str(), row.model.as_str())
                == (default.0.as_str(), default.1.as_str())
            && row.tokens() == 15));
        for stats in [&cached_stats, &scanned_stats] {
            assert_eq!(stats.totals.tokens(), 175);
            assert_eq!(
                cached.iter().map(ThreadUsageRow::tokens).sum::<u64>(),
                stats.totals.tokens()
            );
        }

        // Without `by_thread` no rows are collected.
        let (_, none) =
            aggregate_cached(&threads, &cache, "claude-opus-4-8", since, false).unwrap();
        assert!(none.is_empty());
    }

    #[test]
    fn cache_incremental_reflects_new_changed_and_deleted_threads() {
        let dir = tempfile::tempdir().unwrap();
        let threads = dir.path().join("threads");
        std::fs::create_dir_all(&threads).unwrap();
        let cache = dir.path().join("cache/usage.sqlite");

        write_thread(
            &threads,
            "a",
            &[usage_line(100, 50, Some("gpt-5.5"), Some("openai-codex"))],
        );
        let s1 = aggregate_usage_at(&threads, &cache, "claude-opus-4-8", None).unwrap();
        assert_eq!(s1.threads_scanned, 1);
        assert_eq!((s1.totals.input, s1.totals.output), (100, 50));

        // New thread is picked up incrementally.
        write_thread(
            &threads,
            "b",
            &[usage_line(7, 3, Some("gpt-5.5"), Some("openai-codex"))],
        );
        let s2 = aggregate_usage_at(&threads, &cache, "claude-opus-4-8", None).unwrap();
        assert_eq!(s2.threads_scanned, 2);
        assert_eq!((s2.totals.input, s2.totals.output), (107, 53));

        // A changed (larger) thread is re-scanned and its rows replaced.
        write_thread(
            &threads,
            "a",
            &[
                usage_line(100, 50, Some("gpt-5.5"), Some("openai-codex")),
                usage_line(200, 20, Some("gpt-5.5"), Some("openai-codex")),
            ],
        );
        let s3 = aggregate_usage_at(&threads, &cache, "claude-opus-4-8", None).unwrap();
        assert_eq!((s3.totals.input, s3.totals.output), (307, 73));

        // A deleted thread drops out of the aggregate.
        std::fs::remove_file(threads.join("b.jsonl")).unwrap();
        let s4 = aggregate_usage_at(&threads, &cache, "claude-opus-4-8", None).unwrap();
        assert_eq!(s4.threads_scanned, 1);
        assert_eq!((s4.totals.input, s4.totals.output), (300, 70));
    }

    #[test]
    fn cache_rebuilds_when_corrupt() {
        let dir = tempfile::tempdir().unwrap();
        let threads = dir.path().join("threads");
        std::fs::create_dir_all(&threads).unwrap();
        let cache = dir.path().join("cache/usage.sqlite");

        write_thread(
            &threads,
            "a",
            &[usage_line(100, 50, Some("gpt-5.5"), Some("openai-codex"))],
        );
        aggregate_usage_at(&threads, &cache, "claude-opus-4-8", None).unwrap();

        // Corrupt the cache; the next run must rebuild it and still be correct.
        std::fs::write(&cache, b"definitely not a sqlite database").unwrap();
        let s = aggregate_usage_at(&threads, &cache, "claude-opus-4-8", None).unwrap();
        assert_eq!(s.threads_scanned, 1);
        assert_eq!((s.totals.input, s.totals.output), (100, 50));
    }

    #[test]
    fn cache_attributes_legacy_usage_per_caller_without_rebuilding() {
        let dir = tempfile::tempdir().unwrap();
        let threads = dir.path().join("threads");
        std::fs::create_dir_all(&threads).unwrap();
        let cache = dir.path().join("cache/usage.sqlite");

        // Legacy usage: no per-request model/provider and no model_override, so
        // attribution falls back to `default_model`.
        write_thread(&threads, "legacy", &[usage_line(100, 50, None, None)]);

        let (m1, m2) = ("claude-opus-4-8", "gpt-5.5");
        let s1 = aggregate_usage_at(&threads, &cache, m1, None).unwrap();
        // Same size and mtime, different content: only a rescan would see it.
        let path = threads.join("legacy.jsonl");
        let mtime = std::fs::metadata(&path).unwrap().modified().unwrap();
        write_thread(&threads, "legacy", &[usage_line(900, 50, None, None)]);
        std::fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(mtime)
            .unwrap();
        let s2 = aggregate_usage_at(&threads, &cache, m2, None).unwrap();
        let s1_again = aggregate_usage_at(&threads, &cache, m1, None).unwrap();
        assert_eq!(
            (s2.totals.input, s1_again.totals.input),
            (100, 100),
            "a caller with another default_model must not rebuild the cache"
        );

        let expect1 = providers::resolve_provider(m1).model;
        let expect2 = providers::resolve_provider(m2).model;
        assert_ne!(expect1, expect2, "test models must resolve differently");
        assert_eq!(s1.by_model[0].model.as_deref(), Some(expect1.as_str()));
        assert_eq!(
            s2.by_model[0].model.as_deref(),
            Some(expect2.as_str()),
            "legacy usage follows each caller's default_model"
        );
        assert_eq!(s1_again.by_model[0].model, s1.by_model[0].model);
    }

    #[test]
    fn cached_attribution_follows_caller_provider_and_span() {
        let dir = tempfile::tempdir().unwrap();
        let threads = dir.path().join("threads");
        std::fs::create_dir_all(&threads).unwrap();
        let cache = dir.path().join("cache/usage.sqlite");
        let (today, old) = (ts_days_ago(0), ts_days_ago(60));

        write_thread(
            &threads,
            "mixed",
            &[
                usage_line_at(100, 10, Some("gpt-5.5"), Some("openai"), &today),
                usage_line_at(20, 2, None, None, &today),
                usage_line_at(1000, 100, None, None, &old),
            ],
        );
        write_thread(
            &threads,
            "explicit",
            &[usage_line_at(
                7,
                3,
                Some("claude-opus-4-8"),
                Some("claude-cli"),
                &today,
            )],
        );

        let billed = "openai:gpt-5.5";
        let subscription = "claude-cli:claude-opus-4-8";
        let since = Some(today_utc() - 6);
        let row = |stats: &UsageStats, provider: &str| {
            stats
                .by_model
                .iter()
                .find(|row| row.provider == provider)
                .cloned()
                .unwrap()
        };

        // Unattributed usage merges into the caller's explicit row, which
        // then counts as estimated; the other provider's row stays exact.
        let a = aggregate_usage_at(&threads, &cache, billed, since).unwrap();
        let (openai, claude) = (row(&a, "openai"), row(&a, "claude-cli"));
        assert_eq!((openai.requests, openai.input, openai.output), (2, 120, 12));
        assert!(openai.estimated && !openai.subscription);
        assert_eq!((claude.input, claude.output), (7, 3));
        assert!(!claude.estimated && claude.subscription);
        assert_eq!(a.totals.subscription_tokens, 10);

        let b = aggregate_usage_at(&threads, &cache, subscription, since).unwrap();
        let (openai, claude) = (row(&b, "openai"), row(&b, "claude-cli"));
        assert_eq!((openai.input, openai.output), (100, 10));
        assert!(!openai.estimated);
        assert_eq!((claude.requests, claude.input, claude.output), (2, 27, 5));
        assert!(claude.estimated && claude.subscription);
        assert!(claude.cost_usd.abs() < f64::EPSILON);
        assert_eq!(b.totals.subscription_tokens, 32);
        let claude_provider = b
            .by_provider
            .iter()
            .find(|row| row.provider == "claude-cli")
            .unwrap();
        assert!(claude_provider.estimated && claude_provider.subscription);

        // The bounded span drops the old unattributed usage; all time keeps it.
        let all = aggregate_usage_at(&threads, &cache, subscription, None).unwrap();
        assert_eq!(row(&all, "claude-cli").input, 1027);
        assert_eq!(all.totals.tokens(), b.totals.tokens() + 1100);

        // Attribution never changes the daily series.
        let days = |stats: &UsageStats| {
            stats
                .daily
                .iter()
                .filter(|d| d.counts.tokens > 0)
                .map(|d| (d.day, d.counts.tokens))
                .collect::<Vec<_>>()
        };
        assert_eq!(days(&a), vec![(today_utc(), 142)]);
        assert_eq!(days(&a), days(&b));
        assert_eq!(all.daily.len(), 2);
    }

    #[test]
    fn concurrent_callers_sync_a_fresh_cache_without_falling_back() {
        let dir = tempfile::tempdir().unwrap();
        let threads = dir.path().join("threads");
        std::fs::create_dir_all(&threads).unwrap();
        let cache = dir.path().join("cache/usage.sqlite");
        let count: u64 = 3 * SYNC_CHUNK as u64;
        for i in 0..count {
            write_thread(
                &threads,
                &format!("t{i}"),
                &[
                    usage_line(1, 0, Some("gpt-5.5"), Some("openai")),
                    usage_line(1, 0, None, None),
                ],
            );
        }

        let models = [
            "claude-opus-4-8",
            "claude-cli:claude-opus-4-8",
            "gemini:gemini-3-pro",
        ];
        let barrier = std::sync::Barrier::new(models.len() * 2);
        std::thread::scope(|scope| {
            let callers: Vec<_> = models
                .iter()
                .chain(models.iter())
                .map(|model| {
                    let (threads, cache, barrier) = (&threads, &cache, &barrier);
                    scope.spawn(move || {
                        barrier.wait();
                        // Not `aggregate_usage_at`: a fallback scan would hide a
                        // cache failure behind a correct result.
                        let (stats, _) = aggregate_cached(threads, cache, model, None, false)
                            .unwrap_or_else(|err| panic!("{model}: {err:#}"));
                        (
                            stats.totals.input,
                            stats.threads_scanned,
                            stats.by_provider.len(),
                        )
                    })
                })
                .collect();
            for caller in callers {
                assert_eq!(caller.join().unwrap(), (2 * count, 3 * SYNC_CHUNK, 2));
            }
        });

        let conn = open_cache(&cache).unwrap();
        let rows: i64 = conn
            .query_row("SELECT COUNT(*) FROM thread_meta", [], |r| r.get(0))
            .unwrap();
        assert_eq!(rows, i64::try_from(count).unwrap());
    }

    #[test]
    fn since_day_filters_usage_by_span() {
        let dir = tempfile::tempdir().unwrap();
        let threads = dir.path().join("threads");
        std::fs::create_dir_all(&threads).unwrap();
        let cache = dir.path().join("cache/usage.sqlite");

        // One recent event (today) and one old event (60 days ago).
        write_thread(
            &threads,
            "t",
            &[
                usage_line_at(
                    100,
                    50,
                    Some("gpt-5.5"),
                    Some("openai-codex"),
                    &ts_days_ago(0),
                ),
                usage_line_at(
                    999,
                    999,
                    Some("gpt-5.5"),
                    Some("openai-codex"),
                    &ts_days_ago(60),
                ),
            ],
        );

        // All-time counts both events.
        let all = aggregate_usage_at(&threads, &cache, "claude-opus-4-8", None).unwrap();
        assert_eq!(all.totals.requests, 2);
        assert_eq!((all.totals.input, all.totals.output), (1099, 1049));

        // Last 7 days keeps only the recent event.
        let recent =
            aggregate_usage_at(&threads, &cache, "claude-opus-4-8", Some(today_utc() - 6)).unwrap();
        assert_eq!(recent.totals.requests, 1);
        assert_eq!((recent.totals.input, recent.totals.output), (100, 50));

        // Both spans read from the same synced cache without re-scanning.
        assert_eq!(recent.threads_scanned, 1);

        // Daily series: all-time has both days; last-7d has all 7 days, with
        // usage only today.
        assert_eq!(all.daily.len(), 2);
        assert_eq!(all.daily.iter().map(|d| d.counts.tokens).sum::<u64>(), 2148);
        assert_eq!(recent.daily.len(), 7);
        assert_eq!(recent.daily[6].day, today_utc());
        assert_eq!(recent.daily[6].counts.tokens, 150);
        assert!(recent.daily[..6].iter().all(|d| d.counts.tokens == 0));
    }

    #[test]
    fn daily_series_fills_idle_days_and_sums_the_span_before() {
        let counts = |requests, tokens, uncached_tokens| UsageCounts {
            requests,
            tokens,
            uncached_tokens,
        };
        let rows = vec![
            (100, "openai-codex".to_string(), counts(1, 10, 4)),
            (100, "claude-cli".to_string(), counts(2, 30, 6)),
            (102, "claude-cli".to_string(), counts(1, 5, 5)),
            // The span before a 3-day window ending on day 102 is 97..=99.
            (99, "claude-cli".to_string(), counts(4, 40, 8)),
            (97, "openai-codex".to_string(), counts(1, 1, 1)),
            (96, "claude-cli".to_string(), counts(9, 90, 9)),
            (UNKNOWN_DAY, "claude-cli".to_string(), counts(7, 70, 7)),
        ];

        let (daily, previous) = daily_series(rows.clone(), Some(100), 102);
        assert_eq!(
            daily.iter().map(|d| d.day).collect::<Vec<_>>(),
            vec![100, 101, 102]
        );
        assert_eq!(daily[0].counts, counts(3, 40, 10));
        assert_eq!(
            daily[0]
                .by_provider
                .iter()
                .map(|p| (p.provider.as_str(), p.counts.tokens))
                .collect::<Vec<_>>(),
            vec![("claude-cli", 30), ("openai-codex", 10)]
        );
        assert_eq!(daily[1].counts, UsageCounts::default());
        assert!(daily[1].by_provider.is_empty());
        assert_eq!(previous, Some(counts(5, 41, 9)));

        // All time: only days with usage, no span before, no unknown day.
        let (daily, previous) = daily_series(rows, None, 102);
        assert_eq!(
            daily.iter().map(|d| d.day).collect::<Vec<_>>(),
            vec![96, 97, 99, 100, 102]
        );
        assert_eq!(previous, None);
    }

    #[test]
    fn cached_and_scanned_daily_series_agree() {
        let dir = tempfile::tempdir().unwrap();
        let threads = dir.path().join("threads");
        std::fs::create_dir_all(&threads).unwrap();
        let cache = dir.path().join("cache/usage.sqlite");
        let cached_read = r#"{"type":"usage","input_tokens":3,"output_tokens":2,"cache_read_tokens":100,"cache_write_tokens":1,"ts":"TS"}"#
            .replace("TS", &ts_days_ago(0));
        write_thread(
            &threads,
            "t",
            &[
                usage_line_at(
                    10,
                    5,
                    Some("gpt-5.5"),
                    Some("openai-codex"),
                    &ts_days_ago(0),
                ),
                cached_read,
                usage_line_at(7, 1, Some("gpt-5.5"), Some("openai-codex"), &ts_days_ago(2)),
                usage_line_at(
                    50,
                    0,
                    Some("gpt-5.5"),
                    Some("openai-codex"),
                    &ts_days_ago(8),
                ),
                usage_line_at(
                    900,
                    0,
                    Some("gpt-5.5"),
                    Some("openai-codex"),
                    &ts_days_ago(20),
                ),
            ],
        );
        let since = Some(today_utc() - 6);
        let cached =
            aggregate_usage_at(&threads, &cache, "claude-cli:claude-opus-4-8", since).unwrap();
        let (scanned, _) =
            aggregate_scan_all(&threads, "claude-cli:claude-opus-4-8", since, false).unwrap();
        assert_eq!(cached.daily, scanned.daily);
        assert_eq!(cached.previous, scanned.previous);

        assert_eq!(cached.daily.len(), 7);
        let today = &cached.daily[6];
        assert_eq!(today.counts.requests, 2);
        assert_eq!(today.counts.tokens, 121);
        assert_eq!(today.counts.uncached_tokens, 21);
        // The unattributed event goes to the caller's default provider.
        assert_eq!(
            today
                .by_provider
                .iter()
                .map(|p| (p.provider.as_str(), p.counts.tokens))
                .collect::<Vec<_>>(),
            vec![("claude-cli", 106), ("openai-codex", 15)]
        );
        assert_eq!(cached.daily[4].counts.tokens, 8);
        assert_eq!(
            cached.previous,
            Some(UsageCounts {
                requests: 1,
                tokens: 50,
                uncached_tokens: 50
            })
        );
    }

    #[test]
    fn daily_series_excludes_unknown_day_usage() {
        let dir = tempfile::tempdir().unwrap();
        let threads = dir.path().join("threads");
        std::fs::create_dir_all(&threads).unwrap();
        let cache = dir.path().join("cache/usage.sqlite");

        // One dated event today plus one legacy event with no parseable ts.
        write_thread(
            &threads,
            "t",
            &[
                usage_line_at(
                    10,
                    5,
                    Some("gpt-5.5"),
                    Some("openai-codex"),
                    &ts_days_ago(0),
                ),
                usage_line(100, 50, Some("gpt-5.5"), Some("openai-codex")),
            ],
        );

        let all = aggregate_usage_at(&threads, &cache, "claude-opus-4-8", None).unwrap();
        // Totals count both, but the daily chart omits the undated event.
        assert_eq!(all.totals.requests, 2);
        assert_eq!(all.daily.len(), 1);
        assert_eq!(all.daily[0].counts.tokens, 15);
    }

    #[test]
    fn unknown_day_usage_only_in_all_time_span() {
        let dir = tempfile::tempdir().unwrap();
        let threads = dir.path().join("threads");
        std::fs::create_dir_all(&threads).unwrap();
        let cache = dir.path().join("cache/usage.sqlite");

        // Legacy event with an unparseable timestamp (`"ts":"t"`).
        write_thread(
            &threads,
            "legacy",
            &[usage_line(100, 50, Some("gpt-5.5"), Some("openai-codex"))],
        );

        let all = aggregate_usage_at(&threads, &cache, "claude-opus-4-8", None).unwrap();
        assert_eq!(all.totals.requests, 1);

        let bounded =
            aggregate_usage_at(&threads, &cache, "claude-opus-4-8", Some(today_utc() - 6)).unwrap();
        assert_eq!(
            bounded.totals.requests, 0,
            "unknown-day usage must not appear in a bounded span"
        );
    }

    #[test]
    fn lean_reader_extracts_usage_and_meta_skips_other_lines() {
        // A realistic thread: meta first, a large message line (must be
        // ignored), two usage events (one with per-request model/provider,
        // one legacy without), and a malformed line (must be skipped).
        let big_text = "x".repeat(10_000);
        let jsonl = format!(
            concat!(
                r#"{{"type":"meta","schema_version":1,"model_override":"claude-opus-4-8"}}"#,
                "\n",
                r#"{{"type":"message","role":"assistant","content":[{{"type":"text","text":"{big}"}}],"ts":"t"}}"#,
                "\n",
                r#"{{"type":"usage","input_tokens":100,"output_tokens":50,"cache_read_tokens":10,"cache_write_tokens":5,"model":"gpt-5.5","provider":"openai-codex","ts":"t"}}"#,
                "\n",
                r#"{{"type":"usage","input_tokens":7,"output_tokens":3,"cache_read_tokens":0,"cache_write_tokens":0,"ts":"t"}}"#,
                "\n",
                r#"{{"type":"usage","input_tokens": BROKEN"#,
                "\n",
            ),
            big = big_text,
        );

        let scan = scan_usage_reader(std::io::Cursor::new(jsonl)).unwrap();

        assert_eq!(scan.model_override.as_deref(), Some("claude-opus-4-8"));
        assert_eq!(scan.usages.len(), 2, "message + malformed lines skipped");

        let first = &scan.usages[0];
        assert_eq!(
            (
                first.input,
                first.output,
                first.cache_read,
                first.cache_write
            ),
            (100, 50, 10, 5)
        );
        assert_eq!(first.model.as_deref(), Some("gpt-5.5"));
        assert_eq!(first.provider.as_deref(), Some("openai-codex"));

        let second = &scan.usages[1];
        assert_eq!((second.input, second.output), (7, 3));
        assert!(second.model.is_none() && second.provider.is_none());
    }

    #[test]
    #[allow(clippy::float_cmp)]
    fn unknown_provider_and_model_is_bucketed_without_cost() {
        let mut raw = BTreeMap::new();
        raw.insert(
            (
                "faketest-provider".to_string(),
                "faketest-model".to_string(),
            ),
            RawBucket {
                requests: 2,
                input: 1_000,
                output: 500,
                cache_read: 0,
                cache_write: 0,
                estimated: false,
            },
        );

        let stats = finalize(raw, Vec::new(), None, 1, Vec::new());

        assert_eq!(stats.by_model.len(), 1);
        let row = &stats.by_model[0];
        assert!(!row.cost_known);
        assert_eq!(row.cost_usd, 0.0);
        assert_eq!(stats.totals.unknown_pricing_rows, 1);
        assert_eq!(stats.totals.requests, 2);
        assert_eq!(stats.totals.tokens(), 1_500);
        assert_eq!(stats.totals.billed_usd, 0.0);
    }

    #[test]
    #[allow(clippy::float_cmp)]
    fn subscription_provider_tokens_excluded_from_billed() {
        let Some(sub_provider) = ProviderKind::all()
            .iter()
            .find(|k| k.is_subscription())
            .map(|k| k.id().to_string())
        else {
            return; // no subscription providers registered; nothing to assert
        };

        let mut raw = BTreeMap::new();
        raw.insert(
            (sub_provider.clone(), "any-model".to_string()),
            RawBucket {
                requests: 3,
                input: 2_000,
                output: 1_000,
                cache_read: 500,
                cache_write: 0,
                estimated: false,
            },
        );

        let stats = finalize(raw, Vec::new(), None, 1, Vec::new());

        let row = &stats.by_model[0];
        assert!(row.subscription);
        assert_eq!(row.cost_usd, 0.0);
        assert_eq!(stats.totals.subscription_tokens, 3_500);
        assert_eq!(stats.totals.billed_usd, 0.0);
        assert_eq!(stats.totals.unknown_pricing_rows, 0);
    }

    /// Manual benchmark against a real thread corpus (read-only). Run with
    /// `ZDX_USAGE_BENCH_THREADS=<dir>` and `--ignored --nocapture`.
    #[test]
    #[ignore = "manual benchmark over a real thread corpus"]
    fn bench_concurrent_callers_on_real_corpus() {
        let Ok(threads) = std::env::var("ZDX_USAGE_BENCH_THREADS") else {
            return;
        };
        let threads = PathBuf::from(threads);
        // A temp home keeps the corpus off the live thread index: callers then
        // walk the directory instead of syncing the index.
        let dir = crate::test_support::temp_zdx_home();
        let models = [
            "claude-opus-4-8",
            "openai:gpt-5.5",
            "claude-cli:claude-opus-4-8",
        ];
        // (phase, cache file, start delay in seconds for each caller)
        let phases: [(&str, &str, &[u64]); 4] = [
            ("solo-cold", "solo.sqlite", &[0]),
            ("cold", "a.sqlite", &[0, 0, 0]),
            ("warm", "a.sqlite", &[0, 0, 0]),
            ("staggered-cold", "b.sqlite", &[0, 15, 30]),
        ];
        for (phase, cache, delays) in phases {
            let cache = dir.path().join("cache").join(cache);
            let barrier = std::sync::Barrier::new(delays.len());
            std::thread::scope(|scope| {
                let handles: Vec<_> = models
                    .iter()
                    .zip(delays.iter().copied())
                    .map(|(model, delay)| {
                        let (threads, cache, barrier) = (&threads, &cache, &barrier);
                        scope.spawn(move || {
                            barrier.wait();
                            std::thread::sleep(std::time::Duration::from_secs(delay));
                            let start = std::time::Instant::now();
                            let result = aggregate_cached(
                                threads,
                                cache,
                                model,
                                Some(today_utc() - 29),
                                false,
                            );
                            (
                                model,
                                start.elapsed(),
                                result.map(|(s, _)| s.totals.tokens()),
                            )
                        })
                    })
                    .collect();
                for handle in handles {
                    let (model, elapsed, result) = handle.join().unwrap();
                    eprintln!("{phase} {model}: {elapsed:.2?} {result:?}");
                }
            });
        }
    }
}
