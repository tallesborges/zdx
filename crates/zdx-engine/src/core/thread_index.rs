//! Derived thread index over canonical thread JSONL files.
//!
//! Owns `$ZDX_HOME/cache/threads.sqlite`: thread metadata, export dirty state,
//! an FTS index over titles plus user/assistant message text, and tool-call
//! rows. Canonical JSONL stays the source of truth; this cache is disposable
//! and rebuilt incrementally from `(mtime,size)` changes.
//!
//! ## Intentional semantic differences from the raw-JSONL file scan
//!
//! - Thread text search matches the title plus user/assistant message text
//!   only. Tool arguments/results, reasoning text, and JSONL structural noise
//!   are no longer searched (tool discovery uses the dedicated tool rows).
//! - Query words match as OR of case-insensitive token-prefix phrases via
//!   FTS5 (`unicode61`); mid-word substring matches come from a LIKE fallback
//!   that only runs when FTS finds nothing.
//! - `activity_at` always derives from indexed event timestamps (the file
//!   scan only did this when date filters were active).
//! - Tool matches use a deterministic `tool_ts DESC, thread_id, tool_use_id`
//!   ordering instead of the scan's per-thread insertion order for ties.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use anyhow::{Context, Result, bail};
use chrono::{DateTime, NaiveDate, SecondsFormat, Utc};
use rusqlite::types::Value as SqlValue;
use rusqlite::{Connection, OptionalExtension, params, params_from_iter};
use serde::Serialize;

use crate::config;
use crate::core::thread_export::{self, EXPORT_FORMAT_VERSION};
use crate::core::thread_persistence::{
    self, ThreadEvent, ThreadSearchOptions, ThreadSearchResult, ThreadSummary, ThreadToolMatch,
    ThreadToolSearchOptions,
};

const SCHEMA_VERSION: &str = "3";

const CREATE_SQL: &str = "\
CREATE TABLE IF NOT EXISTS cache_meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS thread_meta (
    thread_id TEXT PRIMARY KEY,
    mtime_ns INTEGER NOT NULL,
    size INTEGER NOT NULL,
    title TEXT,
    root_path TEXT,
    handoff_from TEXT,
    origin_kind TEXT,
    parent_thread_id TEXT,
    subagent_name TEXT,
    activity_at TEXT,
    modified_at TEXT,
    preview TEXT
);
CREATE TABLE IF NOT EXISTS thread_export_state (
    thread_id TEXT PRIMARY KEY,
    source_mtime_ns INTEGER NOT NULL,
    source_size INTEGER NOT NULL,
    export_mtime_ns INTEGER,
    export_size INTEGER,
    export_format_version TEXT,
    dirty INTEGER NOT NULL DEFAULT 1
);
CREATE VIRTUAL TABLE IF NOT EXISTS thread_fts USING fts5(
    thread_id UNINDEXED,
    title,
    text,
    tokenize = 'unicode61'
);
CREATE TABLE IF NOT EXISTS thread_tool (
    thread_id TEXT NOT NULL,
    tool_use_id TEXT NOT NULL,
    tool_name TEXT NOT NULL,
    tool_ts TEXT,
    tool_date TEXT,
    status TEXT NOT NULL,
    args_summary TEXT NOT NULL,
    error_code TEXT,
    error_message TEXT,
    error_details TEXT
);
CREATE INDEX IF NOT EXISTS idx_thread_tool_thread ON thread_tool(thread_id);
CREATE INDEX IF NOT EXISTS idx_thread_tool_ts ON thread_tool(tool_ts);";

/// Counters from one incremental `threads.sqlite` sync.
///
/// `files_enumerated` counts cheap directory stats; `metas_read` counts the
/// expensive canonical JSONL opens (meta line + full event parse) for
/// new/changed threads.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct ThreadCacheSyncSummary {
    pub files_enumerated: usize,
    pub metas_read: usize,
    pub rows_upserted: usize,
    pub rows_removed: usize,
}

/// Returns the `threads.sqlite` path.
#[must_use]
pub fn db_path() -> PathBuf {
    config::paths::zdx_home()
        .join("cache")
        .join("threads.sqlite")
}

/// Opens (creating/rebuilding as needed) the thread index and syncs it
/// incrementally, then exports transcripts selected by dirty state.
///
/// # Errors
/// Returns an error when the cache cannot be opened or synced.
pub fn sync_and_export(
    force: bool,
) -> Result<(ThreadCacheSyncSummary, thread_export::ThreadExportSummary)> {
    let conn = open(&db_path())?;
    let sync = sync(&conn).context("sync native thread metadata cache")?;
    let export =
        export_threads_from_cache(&conn, force).context("export changed thread transcripts")?;
    Ok((sync, export))
}

/// Returns top-level thread summaries from the cache, newest first.
///
/// Syncs incrementally first so results reflect on-disk reality; unchanged
/// threads are answered from the cache without opening their JSONL files.
///
/// # Errors
/// Returns an error when the cache cannot be opened, synced, or read.
pub fn list_threads_cached() -> Result<Vec<ThreadSummary>> {
    let conn = open(&db_path())?;
    sync(&conn)?;
    let mut stmt = conn.prepare_cached(
        "SELECT thread_id, mtime_ns, title, root_path, handoff_from,
                parent_thread_id, subagent_name
         FROM thread_meta WHERE origin_kind IS NULL
         ORDER BY mtime_ns DESC, thread_id ASC",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok(ThreadSummary {
            id: row.get(0)?,
            modified: system_time_from_nanos(row.get(1)?),
            title: row.get(2)?,
            root_path: row.get(3)?,
            handoff_from: row.get(4)?,
            origin_kind: None,
            parent_thread_id: row.get(5)?,
            subagent_name: row.get(6)?,
        })
    })?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .map_err(Into::into)
}

/// Searches threads via the FTS index, preserving the scan path's contract:
/// newest-first ordering, active-thread exclusion before limiting, date
/// filters on event-derived activity, and stored previews.
///
/// # Errors
/// Returns an error when the cache cannot be opened, synced, or queried.
pub fn search_threads_indexed(options: &ThreadSearchOptions) -> Result<Vec<ThreadSearchResult>> {
    let conn = open(&db_path())?;
    sync(&conn)?;

    let normalized_query = options
        .query
        .as_deref()
        .map(str::trim)
        .filter(|q| !q.is_empty());
    let limit = options.limit.max(1);

    let matching_ids: Option<HashSet<String>> = match normalized_query {
        Some(query) => Some(matching_thread_ids(&conn, query)?),
        None => None,
    };
    if matching_ids.as_ref().is_some_and(HashSet::is_empty) {
        return Ok(Vec::new());
    }

    let mut stmt = conn.prepare_cached(
        "SELECT thread_id, title, root_path, activity_at, preview
         FROM thread_meta WHERE origin_kind IS NULL
         ORDER BY mtime_ns DESC, thread_id ASC",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, Option<String>>(1)?,
            row.get::<_, Option<String>>(2)?,
            row.get::<_, Option<String>>(3)?,
            row.get::<_, Option<String>>(4)?,
        ))
    })?;

    let mut results = Vec::new();
    for row in rows {
        let (thread_id, title, root_path, activity_at, preview) = row?;
        if options
            .exclude_thread_id
            .as_ref()
            .is_some_and(|excluded| excluded == &thread_id)
        {
            continue;
        }
        if let Some(ids) = &matching_ids
            && !ids.contains(&thread_id)
        {
            continue;
        }
        if !matches_date_filters(
            activity_at.as_deref(),
            options.date,
            options.date_start,
            options.date_end,
        ) {
            continue;
        }
        results.push(ThreadSearchResult {
            thread_id,
            title,
            root_path,
            activity_at,
            preview: preview.unwrap_or_default(),
        });
        if results.len() >= limit {
            break;
        }
    }
    Ok(results)
}

/// Searches tool calls via indexed tool rows, honoring `limit` in SQL before
/// materializing results.
///
/// # Errors
/// Returns an error when the cache cannot be opened, synced, or queried.
pub fn search_thread_tools_indexed(
    options: &ThreadToolSearchOptions,
) -> Result<Vec<ThreadToolMatch>> {
    let conn = open(&db_path())?;
    sync(&conn)?;

    let mut sql = String::from(
        "SELECT t.thread_id, m.title, t.tool_use_id, t.tool_name, t.tool_ts, t.status,
                t.args_summary, t.error_code, t.error_message, t.error_details
         FROM thread_tool t JOIN thread_meta m ON m.thread_id = t.thread_id
         WHERE m.origin_kind IS NULL",
    );
    let mut sql_params: Vec<SqlValue> = Vec::new();
    if let Some(tool_name) = options
        .tool_name
        .as_deref()
        .map(str::trim)
        .filter(|name| !name.is_empty())
    {
        sql.push_str(" AND lower(t.tool_name) = ?");
        sql_params.push(SqlValue::Text(tool_name.to_ascii_lowercase()));
    }
    if options.failed_only {
        sql.push_str(" AND t.status = 'failed'");
    }
    if let Some(date) = options.date {
        sql.push_str(" AND t.tool_date = ?");
        sql_params.push(SqlValue::Text(date.to_string()));
    }
    if let Some(start) = options.date_start {
        sql.push_str(" AND t.tool_date >= ?");
        sql_params.push(SqlValue::Text(start.to_string()));
    }
    if let Some(end) = options.date_end {
        sql.push_str(" AND t.tool_date <= ?");
        sql_params.push(SqlValue::Text(end.to_string()));
    }
    sql.push_str(" ORDER BY t.tool_ts DESC, t.thread_id ASC, t.tool_use_id ASC LIMIT ?");
    sql_params.push(SqlValue::Integer(
        i64::try_from(options.limit.max(1)).unwrap_or(i64::MAX),
    ));

    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(params_from_iter(sql_params), |row| {
        Ok(ThreadToolMatch {
            thread_id: row.get(0)?,
            title: row.get(1)?,
            tool_use_id: row.get(2)?,
            tool_name: row.get(3)?,
            tool_ts: row.get(4)?,
            status: row.get(5)?,
            args_summary: row.get(6)?,
            error_code: row.get(7)?,
            error_message: row.get(8)?,
            error_details: row.get(9)?,
        })
    })?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .map_err(Into::into)
}

/// `(thread_id, source_modified)` pair fed to export-status computation.
pub type ThreadExportSource = (String, Option<SystemTime>);

/// Returns `(thread_id, source_modified)` pairs for top-level threads for
/// export-status computation without writing the cache, or `None` when the
/// cache is missing or from another schema version.
///
/// Unchanged files are answered from the cache; only new/changed files read
/// their canonical meta line.
///
/// # Errors
/// Returns an error when the cache or the threads directory cannot be read.
pub fn export_status_sources() -> Result<Option<Vec<ThreadExportSource>>> {
    let path = db_path();
    if !path.exists() {
        return Ok(None);
    }
    let conn = Connection::open_with_flags(&path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    conn.execute_batch("PRAGMA busy_timeout=5000;")?;
    if read_meta(&conn, "schema_version")?.as_deref() != Some(SCHEMA_VERSION) {
        return Ok(None);
    }

    let mut cached: HashMap<String, (i64, i64, bool)> = HashMap::new();
    {
        let mut stmt =
            conn.prepare("SELECT thread_id, mtime_ns, size, origin_kind FROM thread_meta")?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                (
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, Option<String>>(3)?.is_none(),
                ),
            ))
        })?;
        for row in rows {
            let (thread_id, value) = row?;
            cached.insert(thread_id, value);
        }
    }

    let files = thread_persistence::list_thread_files(&config::paths::threads_dir())?;
    let mut sources = Vec::with_capacity(files.len());
    for file in &files {
        let mtime_ns = mtime_nanos(file.modified);
        let size = i64::try_from(file.size).unwrap_or(i64::MAX);
        match cached.get(&file.id) {
            Some(&(cached_mtime, cached_size, top_level))
                if cached_mtime == mtime_ns && cached_size == size =>
            {
                if top_level {
                    sources.push((file.id.clone(), file.modified));
                }
            }
            _ => {
                let thread = thread_persistence::thread_summary_from_file(file);
                if !thread.is_child_run() {
                    sources.push((thread.id, thread.modified));
                }
            }
        }
    }
    Ok(Some(sources))
}

/// Counts cached thread metadata rows for status display.
///
/// # Errors
/// Returns an error when the cache is missing or cannot be read.
pub fn meta_row_count() -> Result<usize> {
    let path = db_path();
    let conn = Connection::open_with_flags(&path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    conn.execute_batch("PRAGMA busy_timeout=5000;")?;
    let count: i64 = conn.query_row("SELECT COUNT(*) FROM thread_meta", [], |row| row.get(0))?;
    Ok(usize::try_from(count).unwrap_or(usize::MAX))
}

/// Incrementally syncs the cache from on-disk thread files.
///
/// Every thread file is enumerated with a cheap stat, but canonical JSONL is
/// read (meta line + full events for FTS/tool rows) only for new or
/// `(mtime,size)`-changed threads. Rows for deleted threads are removed and
/// changed threads mark their export dirty.
fn sync(conn: &Connection) -> Result<ThreadCacheSyncSummary> {
    let files = thread_persistence::list_thread_files(&config::paths::threads_dir())
        .context("enumerate thread files for native thread index")?;
    let mut summary = ThreadCacheSyncSummary {
        files_enumerated: files.len(),
        ..ThreadCacheSyncSummary::default()
    };

    let mut cached: HashMap<String, (i64, i64)> = HashMap::new();
    {
        let mut stmt = conn.prepare("SELECT thread_id, mtime_ns, size FROM thread_meta")?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                (row.get::<_, i64>(1)?, row.get::<_, i64>(2)?),
            ))
        })?;
        for row in rows {
            let (thread_id, token) = row?;
            cached.insert(thread_id, token);
        }
    }

    let tx = conn.unchecked_transaction()?;
    let mut seen = HashSet::with_capacity(files.len());
    for file in &files {
        seen.insert(file.id.clone());
        let mtime_ns = mtime_nanos(file.modified);
        let size = i64::try_from(file.size).unwrap_or(i64::MAX);
        if cached.get(&file.id) == Some(&(mtime_ns, size)) {
            continue;
        }
        summary.metas_read += 1;
        // Skip per-row DELETEs for brand-new threads: `thread_fts` deletes by
        // an UNINDEXED column are full-table scans, which made first builds
        // O(N²) over the corpus.
        let existed = cached.contains_key(&file.id);
        index_one_thread(&tx, file, mtime_ns, size, existed)?;
        summary.rows_upserted += 1;
    }
    for thread_id in cached.keys().filter(|id| !seen.contains(*id)) {
        delete_thread_rows(&tx, thread_id)?;
        summary.rows_removed += 1;
    }
    write_meta(&tx, "schema_version", SCHEMA_VERSION)?;
    tx.commit()?;
    Ok(summary)
}

/// Indexes one new/changed thread: metadata, FTS text, tool rows, dirty mark.
///
/// `existed` skips the per-row DELETE statements for threads that have no
/// prior rows (the FTS delete is a full-table scan).
fn index_one_thread(
    conn: &Connection,
    file: &thread_persistence::ThreadFileMeta,
    mtime_ns: i64,
    size: i64,
    existed: bool,
) -> Result<()> {
    let thread = thread_persistence::thread_summary_from_file(file);
    let events = thread_persistence::load_thread_events(&thread.id).unwrap_or_default();

    let modified_at = thread.modified.map(format_system_time);
    let activity_at = thread_persistence::latest_event_timestamp(&events)
        .map(|dt| dt.to_rfc3339_opts(SecondsFormat::Secs, true))
        .or_else(|| modified_at.clone());
    let preview = preview_from_events(&events, thread.title.as_deref());
    let text = searchable_text(&events);

    conn.execute(
        "INSERT INTO thread_meta(
            thread_id, mtime_ns, size, title, root_path, handoff_from, origin_kind,
            parent_thread_id, subagent_name, activity_at, modified_at, preview
         ) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
         ON CONFLICT(thread_id) DO UPDATE SET
           mtime_ns=excluded.mtime_ns, size=excluded.size, title=excluded.title,
           root_path=excluded.root_path, handoff_from=excluded.handoff_from,
           origin_kind=excluded.origin_kind, parent_thread_id=excluded.parent_thread_id,
           subagent_name=excluded.subagent_name, activity_at=excluded.activity_at,
           modified_at=excluded.modified_at, preview=excluded.preview",
        params![
            thread.id,
            mtime_ns,
            size,
            thread.title,
            thread.root_path,
            thread.handoff_from,
            thread.origin_kind,
            thread.parent_thread_id,
            thread.subagent_name,
            activity_at,
            modified_at,
            preview,
        ],
    )?;

    if existed {
        conn.execute("DELETE FROM thread_fts WHERE thread_id = ?1", [&thread.id])?;
    }
    conn.execute(
        "INSERT INTO thread_fts(thread_id, title, text) VALUES(?1, ?2, ?3)",
        params![thread.id, thread.title.as_deref().unwrap_or(""), text],
    )?;

    if existed {
        conn.execute("DELETE FROM thread_tool WHERE thread_id = ?1", [&thread.id])?;
    }
    {
        let mut insert_tool = conn.prepare_cached(
            "INSERT INTO thread_tool(
                thread_id, tool_use_id, tool_name, tool_ts, tool_date, status,
                args_summary, error_code, error_message, error_details
             ) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        )?;
        for row in tool_rows(&events) {
            insert_tool.execute(params![
                thread.id,
                row.tool_use_id,
                row.tool_name,
                row.tool_ts,
                row.tool_date,
                row.status,
                row.args_summary,
                row.error_code,
                row.error_message,
                row.error_details,
            ])?;
        }
    }

    conn.execute(
        "INSERT INTO thread_export_state(thread_id, source_mtime_ns, source_size, dirty)
         VALUES(?1, ?2, ?3, 1)
         ON CONFLICT(thread_id) DO UPDATE SET
           source_mtime_ns=excluded.source_mtime_ns,
           source_size=excluded.source_size,
           dirty=1",
        params![thread.id, mtime_ns, size],
    )?;
    Ok(())
}

fn delete_thread_rows(conn: &Connection, thread_id: &str) -> Result<()> {
    conn.execute("DELETE FROM thread_meta WHERE thread_id = ?1", [thread_id])?;
    conn.execute(
        "DELETE FROM thread_export_state WHERE thread_id = ?1",
        [thread_id],
    )?;
    conn.execute("DELETE FROM thread_fts WHERE thread_id = ?1", [thread_id])?;
    conn.execute("DELETE FROM thread_tool WHERE thread_id = ?1", [thread_id])?;
    Ok(())
}

#[derive(Debug)]
struct ExportCandidate {
    thread_id: String,
    source_mtime_ns: i64,
    source_size: i64,
    export_format_version: Option<String>,
    dirty: Option<i64>,
}

/// Exports thread transcripts selected by `threads.sqlite` dirty state.
///
/// A thread is exported when it is forced, dirty, missing its export file, has
/// no export state row, or was exported with a different format version. The
/// dirty flag is cleared only after re-statting the source and proving its
/// `(mtime,size)` token did not change during export.
fn export_threads_from_cache(
    conn: &Connection,
    force: bool,
) -> Result<thread_export::ThreadExportSummary> {
    let candidates: Vec<ExportCandidate> = {
        let mut stmt = conn.prepare(
            "SELECT m.thread_id, m.mtime_ns, m.size, s.export_format_version, s.dirty
             FROM thread_meta m
             LEFT JOIN thread_export_state s ON s.thread_id = m.thread_id
             WHERE m.origin_kind IS NULL",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(ExportCandidate {
                thread_id: row.get(0)?,
                source_mtime_ns: row.get(1)?,
                source_size: row.get(2)?,
                export_format_version: row.get(3)?,
                dirty: row.get(4)?,
            })
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?
    };

    let export_dir = config::paths::thread_exports_dir();
    let threads_dir = config::paths::threads_dir();
    let mut summary = thread_export::ThreadExportSummary::default();
    let mut thread_ids = HashSet::with_capacity(candidates.len());
    let mut upsert_state = conn.prepare_cached(
        "INSERT INTO thread_export_state(
            thread_id, source_mtime_ns, source_size, export_mtime_ns, export_size,
            export_format_version, dirty
         ) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7)
         ON CONFLICT(thread_id) DO UPDATE SET
           source_mtime_ns=excluded.source_mtime_ns, source_size=excluded.source_size,
           export_mtime_ns=excluded.export_mtime_ns, export_size=excluded.export_size,
           export_format_version=excluded.export_format_version, dirty=excluded.dirty",
    )?;

    for candidate in &candidates {
        thread_ids.insert(candidate.thread_id.clone());
        let export_path = export_dir.join(format!("{}.md", candidate.thread_id));
        let needs_export = force
            || candidate.dirty.unwrap_or(1) != 0
            || candidate.export_format_version.as_deref() != Some(EXPORT_FORMAT_VERSION)
            || !export_path.exists();
        if !needs_export {
            summary.skipped += 1;
            continue;
        }

        let Ok(export_path) = thread_export::export_thread(&candidate.thread_id) else {
            summary.failed += 1;
            continue;
        };
        summary.exported += 1;

        let source_meta =
            fs::metadata(threads_dir.join(format!("{}.jsonl", candidate.thread_id))).ok();
        let source_mtime_ns = mtime_nanos(source_meta.as_ref().and_then(|m| m.modified().ok()));
        let source_size = source_meta
            .as_ref()
            .map_or(0, |m| i64::try_from(m.len()).unwrap_or(i64::MAX));
        let source_unchanged =
            source_mtime_ns == candidate.source_mtime_ns && source_size == candidate.source_size;
        let export_meta = fs::metadata(&export_path).ok();
        upsert_state.execute(params![
            candidate.thread_id,
            candidate.source_mtime_ns,
            candidate.source_size,
            export_meta.as_ref().map(|m| mtime_nanos(m.modified().ok())),
            export_meta
                .as_ref()
                .map(|m| i64::try_from(m.len()).unwrap_or(i64::MAX)),
            EXPORT_FORMAT_VERSION,
            i64::from(!source_unchanged),
        ])?;
    }

    thread_export::remove_orphan_exports(&thread_ids, false, &mut summary)?;
    Ok(summary)
}

/// Returns thread ids whose title or user/assistant text matches the query:
/// OR of FTS token-prefix phrases, with a LIKE substring fallback when FTS
/// finds nothing.
fn matching_thread_ids(conn: &Connection, query: &str) -> Result<HashSet<String>> {
    let words: Vec<&str> = query.split_whitespace().filter(|w| !w.is_empty()).collect();
    if words.is_empty() {
        return Ok(HashSet::new());
    }

    let fts_query = words
        .iter()
        .map(|word| format!("\"{}\"*", word.replace('"', "\"\"")))
        .collect::<Vec<_>>()
        .join(" OR ");
    let mut ids: HashSet<String> = {
        let mut stmt =
            conn.prepare_cached("SELECT thread_id FROM thread_fts WHERE thread_fts MATCH ?1")?;
        let rows = stmt.query_map([&fts_query], |row| row.get::<_, String>(0));
        match rows {
            Ok(rows) => rows.collect::<rusqlite::Result<HashSet<_>>>()?,
            // Malformed FTS queries fall through to the LIKE path.
            Err(_) => HashSet::new(),
        }
    };

    if ids.is_empty() {
        let mut stmt = conn.prepare_cached(
            "SELECT thread_id FROM thread_fts
             WHERE title LIKE ?1 ESCAPE '\\' OR text LIKE ?1 ESCAPE '\\'",
        )?;
        for word in &words {
            let pattern = format!("%{}%", escape_like(word));
            let rows = stmt.query_map([&pattern], |row| row.get::<_, String>(0))?;
            for row in rows {
                ids.insert(row?);
            }
        }
    }
    Ok(ids)
}

#[derive(Debug)]
struct ToolRow {
    tool_use_id: String,
    tool_name: String,
    tool_ts: Option<String>,
    tool_date: Option<String>,
    status: String,
    args_summary: String,
    error_code: Option<String>,
    error_message: Option<String>,
    error_details: Option<String>,
}

fn tool_rows(events: &[ThreadEvent]) -> Vec<ToolRow> {
    let mut pending: HashMap<String, ToolRow> = HashMap::new();
    let mut order: Vec<String> = Vec::new();
    let mut rows = Vec::new();

    for event in events {
        match event {
            ThreadEvent::ToolUse {
                id,
                name,
                input,
                ts,
                ..
            } => {
                order.push(id.clone());
                pending.insert(
                    id.clone(),
                    ToolRow {
                        tool_use_id: id.clone(),
                        tool_name: name.clone(),
                        tool_ts: Some(ts.clone()),
                        tool_date: date_from_rfc3339(ts),
                        status: "pending".to_string(),
                        args_summary: thread_persistence::summarize_tool_args(input),
                        error_code: None,
                        error_message: None,
                        error_details: None,
                    },
                );
            }
            ThreadEvent::ToolResult {
                tool_use_id,
                output,
                ok,
                ..
            } => {
                let Some(mut row) = pending.remove(tool_use_id) else {
                    continue;
                };
                row.status = if *ok { "ok" } else { "failed" }.to_string();
                let (error_code, error_message, error_details) =
                    thread_persistence::extract_tool_error(output);
                row.error_code = error_code;
                row.error_message = error_message;
                row.error_details = error_details;
                rows.push(row);
            }
            _ => {}
        }
    }

    for id in order {
        if let Some(row) = pending.remove(&id) {
            rows.push(row);
        }
    }
    rows
}

fn searchable_text(events: &[ThreadEvent]) -> String {
    let mut text = String::new();
    for event in events {
        if let ThreadEvent::Message {
            role, text: body, ..
        } = event
            && (role == "user" || role == "assistant")
        {
            let trimmed = body.trim();
            if trimmed.is_empty() {
                continue;
            }
            if !text.is_empty() {
                text.push('\n');
            }
            text.push_str(trimmed);
        }
    }
    text
}

fn preview_from_events(events: &[ThreadEvent], title: Option<&str>) -> String {
    for event in events {
        if let ThreadEvent::Message { role, text, .. } = event
            && role == "assistant"
        {
            let trimmed = text.trim();
            if !trimmed.is_empty() {
                return thread_persistence::truncate_preview(trimmed);
            }
        }
    }
    title
        .map(thread_persistence::truncate_preview)
        .unwrap_or_default()
}

fn matches_date_filters(
    activity_at: Option<&str>,
    date: Option<NaiveDate>,
    date_start: Option<NaiveDate>,
    date_end: Option<NaiveDate>,
) -> bool {
    if date.is_none() && date_start.is_none() && date_end.is_none() {
        return true;
    }
    let Some(activity_date) = activity_at
        .and_then(|raw| DateTime::parse_from_rfc3339(raw).ok())
        .map(|dt| dt.with_timezone(&Utc).date_naive())
    else {
        return false;
    };
    if date.is_some_and(|date| activity_date != date) {
        return false;
    }
    if date_start.is_some_and(|start| activity_date < start) {
        return false;
    }
    if date_end.is_some_and(|end| activity_date > end) {
        return false;
    }
    true
}

fn date_from_rfc3339(raw: &str) -> Option<String> {
    DateTime::parse_from_rfc3339(raw)
        .ok()
        .map(|dt| dt.with_timezone(&Utc).date_naive().to_string())
}

fn open(path: &Path) -> Result<Connection> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).context("create cache directory")?;
    }
    match try_open(path) {
        Ok(conn) => Ok(conn),
        Err(err) if is_corruption(&err) => {
            let _ = fs::remove_file(path);
            let _ = fs::remove_file(path.with_extension("sqlite-wal"));
            let _ = fs::remove_file(path.with_extension("sqlite-shm"));
            try_open(path).context("recreate native thread cache")
        }
        Err(err) => Err(err),
    }
}

fn try_open(path: &Path) -> Result<Connection> {
    let conn = Connection::open(path)?;
    conn.execute_batch(
        "PRAGMA busy_timeout=5000; PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL;",
    )?;
    let integrity: String = conn.query_row("PRAGMA integrity_check(1)", [], |r| r.get(0))?;
    if integrity != "ok" {
        bail!("integrity check failed: {integrity}");
    }
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS cache_meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);",
    )?;
    if read_meta(&conn, "schema_version")?.as_deref() != Some(SCHEMA_VERSION) {
        conn.execute_batch(
            "DROP TABLE IF EXISTS thread_meta;
             DROP TABLE IF EXISTS thread_export_state;
             DROP TABLE IF EXISTS thread_fts;
             DROP TABLE IF EXISTS thread_tool;",
        )?;
    }
    conn.execute_batch(CREATE_SQL)?;
    Ok(conn)
}

fn is_corruption(err: &anyhow::Error) -> bool {
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

fn read_meta(conn: &Connection, key: &str) -> Result<Option<String>> {
    let exists: bool = conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='cache_meta'",
        [],
        |row| row.get::<_, i64>(0).map(|count| count > 0),
    )?;
    if !exists {
        return Ok(None);
    }
    conn.query_row("SELECT value FROM cache_meta WHERE key = ?1", [key], |r| {
        r.get::<_, String>(0)
    })
    .optional()
    .map_err(Into::into)
}

fn write_meta(conn: &Connection, key: &str, value: &str) -> Result<()> {
    conn.execute(
        "INSERT INTO cache_meta(key, value) VALUES(?1, ?2) ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        (key, value),
    )?;
    Ok(())
}

fn escape_like(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}

fn mtime_nanos(modified: Option<SystemTime>) -> i64 {
    modified
        .and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok())
        .map_or(0, |d| i64::try_from(d.as_nanos()).unwrap_or(i64::MAX))
}

fn system_time_from_nanos(nanos: i64) -> Option<SystemTime> {
    if nanos <= 0 {
        return None;
    }
    u64::try_from(nanos)
        .ok()
        .map(|nanos| SystemTime::UNIX_EPOCH + Duration::from_nanos(nanos))
}

fn format_system_time(time: SystemTime) -> String {
    let datetime: DateTime<Utc> = time.into();
    datetime.to_rfc3339_opts(SecondsFormat::Secs, true)
}
