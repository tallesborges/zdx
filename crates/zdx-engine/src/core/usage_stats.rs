//! Usage/cost aggregation over saved thread JSONL.
//!
//! Sums token usage across every saved thread under `threads_dir()`, grouped
//! per provider and per model, applying the shared `ModelPricing` cost path.
//! This is the data source for `zdx stats` and the monitor Usage tab.
//!
//! To stay fast at thousands of threads, results are backed by a derived,
//! disposable `SQLite` cache (`$ZDX_HOME/cache/usage.sqlite`) that stores each
//! thread's partial aggregate keyed by `(thread_id, mtime, size)`; only
//! changed or new threads are re-scanned on each run. JSONL stays canonical —
//! the cache is rebuilt transparently if missing, corrupt, or built for a
//! different attribution `default_model`, and the aggregator falls back to a
//! full lean scan if the cache is unavailable.
//!
//! Attribution: usage events carry per-request `model`/`provider`; older events
//! without them fall back to the thread's `model_override` or the supplied
//! `default_model`, and such rows are marked `estimated`.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension};

use crate::config::paths;
use crate::models::ModelOption;
use crate::providers::{self, ProviderKind};

/// One aggregated row (a provider total, or a single provider+model total).
#[derive(Debug, Clone)]
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
#[derive(Debug, Clone, Default)]
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

/// Aggregated usage statistics ready for display.
#[derive(Debug, Clone)]
pub struct UsageStats {
    pub totals: UsageTotals,
    /// Per-provider rows, sorted by total tokens (descending).
    pub by_provider: Vec<UsageRow>,
    /// Per provider+model rows, sorted by total tokens (descending).
    pub by_model: Vec<UsageRow>,
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
pub fn aggregate_usage(default_model: &str) -> Result<UsageStats> {
    aggregate_usage_at(&paths::threads_dir(), &cache_path(), default_model)
}

fn aggregate_usage_at(
    threads_dir: &Path,
    cache_path: &Path,
    default_model: &str,
) -> Result<UsageStats> {
    match aggregate_cached(threads_dir, cache_path, default_model) {
        Ok(stats) => Ok(stats),
        Err(err) => {
            tracing::debug!("usage cache unavailable ({err:#}); falling back to full scan");
            aggregate_scan_all(threads_dir, default_model)
        }
    }
}

/// Full (uncached) scan of every thread. Used as the fallback when the cache
/// cannot be opened.
fn aggregate_scan_all(threads_dir: &Path, default_model: &str) -> Result<UsageStats> {
    let files = list_thread_files(threads_dir)?;
    let mut raw: BTreeMap<(String, String), RawBucket> = BTreeMap::new();
    let mut warnings = Vec::new();
    let mut threads_scanned = 0usize;

    for file in &files {
        match scan_thread_file(&file.path) {
            Ok(scan) => {
                threads_scanned += 1;
                for (key, bucket) in resolve_thread_buckets(&scan, default_model) {
                    raw.entry(key).or_default().merge(&bucket);
                }
            }
            Err(err) => warnings.push(format!("skipped thread {}: {err}", file.id)),
        }
    }

    Ok(finalize(raw, threads_scanned, warnings))
}

/// Cache-backed aggregation: syncs only changed/new threads into the `SQLite`
/// cache, then sums all cached per-thread buckets.
fn aggregate_cached(
    threads_dir: &Path,
    cache_path: &Path,
    default_model: &str,
) -> Result<UsageStats> {
    let files = list_thread_files(threads_dir)?;
    let conn = open_cache(cache_path)?;
    ensure_cache_valid(&conn, default_model)?;

    let cached_meta = load_thread_meta(&conn)?;
    let mut current_ids: HashSet<&str> = HashSet::with_capacity(files.len());
    let mut warnings = Vec::new();
    let mut threads_scanned = 0usize;

    let tx = conn.unchecked_transaction()?;
    for file in &files {
        current_ids.insert(file.id.as_str());
        let unchanged = cached_meta
            .get(&file.id)
            .is_some_and(|(mtime, size)| *mtime == file.mtime_ns && *size == file.size);
        if unchanged {
            threads_scanned += 1;
            continue;
        }
        match scan_thread_file(&file.path) {
            Ok(scan) => {
                let buckets = resolve_thread_buckets(&scan, default_model);
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
    // Drop cache rows for threads that no longer exist on disk.
    for id in cached_meta.keys() {
        if !current_ids.contains(id.as_str()) {
            delete_thread_rows(&tx, id)?;
        }
    }
    tx.commit()?;

    let raw = load_all_buckets(&conn)?;
    Ok(finalize(raw, threads_scanned, warnings))
}

/// Resolves a thread's usage into per-`(provider, model)` buckets, applying the
/// thread-level fallback (`model_override` or `default_model`) to usage that
/// lacks a per-request provider/model.
fn resolve_thread_buckets(
    scan: &ThreadUsageScan,
    default_model: &str,
) -> BTreeMap<(String, String), RawBucket> {
    let fallback_model = scan
        .model_override
        .clone()
        .unwrap_or_else(|| default_model.to_string());
    let fallback_selection = providers::resolve_provider(&fallback_model);
    let fallback_key = (
        fallback_selection.kind.id().to_string(),
        fallback_selection.model.clone(),
    );

    let mut buckets: BTreeMap<(String, String), RawBucket> = BTreeMap::new();
    for usage in &scan.usages {
        let (key, estimated) = attribute_event(
            usage.provider.as_deref(),
            usage.model.as_deref(),
            &fallback_key,
        );
        buckets.entry(key).or_default().add_event(
            usage.input,
            usage.output,
            usage.cache_read,
            usage.cache_write,
            estimated,
        );
    }
    buckets
}

/// A thread `.jsonl` file on disk with the metadata used for cache invalidation.
struct ThreadFile {
    id: String,
    path: PathBuf,
    mtime_ns: i64,
    size: i64,
}

/// Lists `*.jsonl` thread files under `threads_dir` with their mtime and size.
/// A missing directory yields an empty list rather than an error.
fn list_thread_files(threads_dir: &Path) -> Result<Vec<ThreadFile>> {
    let entries = match std::fs::read_dir(threads_dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => return Err(err).context("read threads dir"),
    };

    let mut files = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().is_none_or(|ext| ext != "jsonl") {
            continue;
        }
        let Some(id) = path.file_stem().map(|s| s.to_string_lossy().into_owned()) else {
            continue;
        };
        let Ok(md) = entry.metadata() else {
            continue;
        };
        let size = i64::try_from(md.len()).unwrap_or(i64::MAX);
        let mtime_ns = md
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map_or(0, |d| i64::try_from(d.as_nanos()).unwrap_or(i64::MAX));
        files.push(ThreadFile {
            id,
            path,
            mtime_ns,
            size,
        });
    }
    Ok(files)
}

// ── Derived SQLite cache ────────────────────────────────────────────────────

/// Bumped when the cache table layout changes; a mismatch drops and rebuilds
/// the per-thread tables.
const CACHE_SCHEMA_VERSION: &str = "1";

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
    requests INTEGER NOT NULL,
    input INTEGER NOT NULL,
    output INTEGER NOT NULL,
    cache_read INTEGER NOT NULL,
    cache_write INTEGER NOT NULL,
    estimated INTEGER NOT NULL,
    PRIMARY KEY (thread_id, provider, model)
);";

fn cache_path() -> PathBuf {
    paths::zdx_home().join("cache").join("usage.sqlite")
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
/// (our integrity-check bail, or a not-a-database / corruption code). Lock,
/// permission, and other transient errors return false.
fn is_cache_corruption(err: &anyhow::Error) -> bool {
    if err
        .chain()
        .any(|cause| cause.to_string().contains("integrity check failed"))
    {
        return true;
    }
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
    conn.execute_batch(
        "PRAGMA busy_timeout=5000; PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL;",
    )?;
    let integrity: String = conn.query_row("PRAGMA integrity_check(1)", [], |r| r.get(0))?;
    if integrity != "ok" {
        anyhow::bail!("integrity check failed: {integrity}");
    }
    conn.execute_batch(CREATE_META_SQL)?;
    Ok(conn)
}

/// Ensures the cache matches the current schema version and attribution model.
/// A schema mismatch drops+recreates the data tables; a `default_model` change
/// clears the per-thread data so it re-syncs against the new fallback.
fn ensure_cache_valid(conn: &Connection, default_model: &str) -> Result<()> {
    let version_ok = read_meta(conn, "schema_version")?.as_deref() == Some(CACHE_SCHEMA_VERSION);
    let model_ok = read_meta(conn, "default_model")?.as_deref() == Some(default_model);

    if !version_ok {
        conn.execute_batch("DROP TABLE IF EXISTS thread_meta; DROP TABLE IF EXISTS thread_usage;")?;
    }
    conn.execute_batch(CREATE_DATA_SQL)?;

    if !version_ok || !model_ok {
        conn.execute_batch("DELETE FROM thread_meta; DELETE FROM thread_usage;")?;
        write_meta(conn, "schema_version", CACHE_SCHEMA_VERSION)?;
        write_meta(conn, "default_model", default_model)?;
    }
    Ok(())
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

fn replace_thread_rows(
    conn: &Connection,
    id: &str,
    mtime_ns: i64,
    size: i64,
    buckets: &BTreeMap<(String, String), RawBucket>,
) -> Result<()> {
    conn.execute("DELETE FROM thread_usage WHERE thread_id = ?1", [id])?;
    {
        let mut stmt = conn.prepare_cached(
            "INSERT INTO thread_usage \
             (thread_id, provider, model, requests, input, output, cache_read, cache_write, estimated) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        )?;
        for ((provider, model), bucket) in buckets {
            stmt.execute(rusqlite::params![
                id,
                provider.as_str(),
                model.as_str(),
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

fn load_all_buckets(conn: &Connection) -> Result<BTreeMap<(String, String), RawBucket>> {
    let mut stmt = conn.prepare(
        "SELECT provider, model, requests, input, output, cache_read, cache_write, estimated \
         FROM thread_usage",
    )?;
    let rows = stmt.query_map([], |r| {
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
        map.entry(key).or_default().merge(&bucket);
    }
    Ok(map)
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
        let attribution = match (model, provider) {
            (Some(m), Some(p)) => format!(r#","model":"{m}","provider":"{p}""#),
            _ => String::new(),
        };
        format!(
            r#"{{"type":"usage","input_tokens":{input},"output_tokens":{output},"cache_read_tokens":0,"cache_write_tokens":0{attribution},"ts":"t"}}"#
        )
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
        let s1 = aggregate_usage_at(&threads, &cache, "claude-opus-4-8").unwrap();
        assert_eq!(s1.threads_scanned, 1);
        assert_eq!((s1.totals.input, s1.totals.output), (100, 50));

        // New thread is picked up incrementally.
        write_thread(
            &threads,
            "b",
            &[usage_line(7, 3, Some("gpt-5.5"), Some("openai-codex"))],
        );
        let s2 = aggregate_usage_at(&threads, &cache, "claude-opus-4-8").unwrap();
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
        let s3 = aggregate_usage_at(&threads, &cache, "claude-opus-4-8").unwrap();
        assert_eq!((s3.totals.input, s3.totals.output), (307, 73));

        // A deleted thread drops out of the aggregate.
        std::fs::remove_file(threads.join("b.jsonl")).unwrap();
        let s4 = aggregate_usage_at(&threads, &cache, "claude-opus-4-8").unwrap();
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
        aggregate_usage_at(&threads, &cache, "claude-opus-4-8").unwrap();

        // Corrupt the cache; the next run must rebuild it and still be correct.
        std::fs::write(&cache, b"definitely not a sqlite database").unwrap();
        let s = aggregate_usage_at(&threads, &cache, "claude-opus-4-8").unwrap();
        assert_eq!(s.threads_scanned, 1);
        assert_eq!((s.totals.input, s.totals.output), (100, 50));
    }

    #[test]
    fn cache_reattributes_when_default_model_changes() {
        let dir = tempfile::tempdir().unwrap();
        let threads = dir.path().join("threads");
        std::fs::create_dir_all(&threads).unwrap();
        let cache = dir.path().join("cache/usage.sqlite");

        // Legacy usage: no per-request model/provider and no model_override, so
        // attribution falls back to `default_model`.
        write_thread(&threads, "legacy", &[usage_line(100, 50, None, None)]);

        let (m1, m2) = ("claude-opus-4-8", "gpt-5.5");
        let s1 = aggregate_usage_at(&threads, &cache, m1).unwrap();
        let s2 = aggregate_usage_at(&threads, &cache, m2).unwrap();

        let expect1 = providers::resolve_provider(m1).model;
        let expect2 = providers::resolve_provider(m2).model;
        assert_ne!(expect1, expect2, "test models must resolve differently");
        assert_eq!(s1.by_model[0].model.as_deref(), Some(expect1.as_str()));
        assert_eq!(
            s2.by_model[0].model.as_deref(),
            Some(expect2.as_str()),
            "changing default_model must wipe the cache and re-attribute legacy usage"
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

        let stats = finalize(raw, 1, Vec::new());

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

        let stats = finalize(raw, 1, Vec::new());

        let row = &stats.by_model[0];
        assert!(row.subscription);
        assert_eq!(row.cost_usd, 0.0);
        assert_eq!(stats.totals.subscription_tokens, 3_500);
        assert_eq!(stats.totals.billed_usd, 0.0);
        assert_eq!(stats.totals.unknown_pricing_rows, 0);
    }
}
