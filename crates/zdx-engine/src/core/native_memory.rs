//! Native memory index over exported threads, Notes, and Calendar Markdown.
//!
//! Canonical sources stay on disk. This module owns only derived `SQLite` state
//! under `$ZDX_HOME/cache/memory.sqlite`.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::time::{Duration, SystemTime};

use anyhow::{Context, Result, bail};
use chrono::{DateTime, SecondsFormat, Utc};
use ignore::WalkBuilder;
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::config::{self, MemoryConfig};
use crate::core::thread_export::{self, ThreadExportOptions};
use crate::core::thread_index;

const SCHEMA_VERSION: &str = "2";
const DOCID_VERSION: &str = "v1";
const CHUNKER_VERSION: &str = "md-lines-v1";
const EXPORT_FORMAT_VERSION: &str = "thread-md-v1";
const MAX_CHUNK_BYTES: usize = 3_500;
const MEMORY_GET_MAX_BYTES: usize = 40_000;
const MEMORY_GET_MAX_LINES: usize = 1_200;
const LOCK_STALE_AFTER: Duration = Duration::from_mins(30);

const CREATE_META_SQL: &str =
    "CREATE TABLE IF NOT EXISTS cache_meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);";

const CREATE_DATA_SQL: &str = "\
CREATE TABLE IF NOT EXISTS document (
    docid TEXT PRIMARY KEY,
    source TEXT NOT NULL,
    file TEXT NOT NULL,
    relative_path TEXT NOT NULL,
    title TEXT,
    content TEXT NOT NULL,
    content_hash TEXT NOT NULL,
    source_mtime_ns INTEGER NOT NULL,
    source_size INTEGER NOT NULL,
    root_id TEXT NOT NULL,
    generation INTEGER NOT NULL,
    indexed_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_document_source ON document(source);
CREATE TABLE IF NOT EXISTS chunk (
    chunk_id TEXT PRIMARY KEY,
    docid TEXT NOT NULL,
    source TEXT NOT NULL,
    ordinal INTEGER NOT NULL,
    text TEXT NOT NULL,
    content_hash TEXT NOT NULL,
    FOREIGN KEY(docid) REFERENCES document(docid) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS idx_chunk_docid ON chunk(docid);
CREATE VIRTUAL TABLE IF NOT EXISTS chunk_fts USING fts5(
    chunk_id UNINDEXED,
    docid UNINDEXED,
    source UNINDEXED,
    title,
    path,
    text,
    tokenize = 'unicode61'
);
CREATE TABLE IF NOT EXISTS embedding_vector (
    input_hash TEXT NOT NULL,
    profile_fingerprint TEXT NOT NULL,
    dims INTEGER NOT NULL,
    vector BLOB NOT NULL,
    created_at TEXT NOT NULL,
    PRIMARY KEY (input_hash, profile_fingerprint)
);
CREATE TABLE IF NOT EXISTS chunk_vector (
    chunk_id TEXT PRIMARY KEY,
    input_hash TEXT NOT NULL,
    profile_fingerprint TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_chunk_vector_hash ON chunk_vector(input_hash, profile_fingerprint);";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemorySource {
    Thread,
    Note,
    Calendar,
}

impl MemorySource {
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Thread => "thread",
            Self::Note => "note",
            Self::Calendar => "calendar",
        }
    }

    fn from_label(value: &str) -> Option<Self> {
        match value {
            "thread" => Some(Self::Thread),
            "note" => Some(Self::Note),
            "calendar" => Some(Self::Calendar),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemorySearchStrategy {
    Keyword,
    Vector,
    Hybrid,
}

impl MemorySearchStrategy {
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Keyword => "keyword",
            Self::Vector => "vector",
            Self::Hybrid => "hybrid",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryState {
    NotConfigured,
    Unprobed,
    Missing,
    Building,
    Ready,
    Stale,
    Partial,
    Incompatible,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MemoryComponentStatus {
    pub state: MemoryState,
    pub path: String,
    pub detail: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct NativeMemoryStatus {
    pub backend: &'static str,
    pub readiness: MemoryState,
    pub thread_exports: ThreadExportsStatusJson,
    pub threads_sqlite: MemoryComponentStatus,
    pub memory_sqlite: MemorySqliteStatus,
    pub embeddings: MemoryComponentStatus,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ThreadExportsStatusJson {
    pub state: MemoryState,
    pub path: String,
    pub source_threads: usize,
    pub exported_threads: usize,
    pub missing_exports: usize,
    pub stale_exports: usize,
    pub orphaned_exports: usize,
    pub latest_source_modified: Option<String>,
    pub latest_export_modified: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MemorySqliteStatus {
    pub state: MemoryState,
    pub path: String,
    pub schema_version: Option<String>,
    pub generation: Option<i64>,
    pub documents: usize,
    pub chunks: usize,
    pub last_indexed_at: Option<String>,
    pub detail: Option<String>,
}

#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, Copy, Default)]
pub struct NativeMemoryIndexOptions {
    pub force: bool,
    pub rebuild: bool,
    pub dry_run: bool,
    pub embed: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct NativeMemoryIndexSummary {
    pub dry_run: bool,
    pub thread_cache: thread_index::ThreadCacheSyncSummary,
    pub thread_exports: thread_export::ThreadExportSummary,
    pub documents_indexed: usize,
    pub documents_removed: usize,
    pub chunks_indexed: usize,
    pub generation: Option<i64>,
    pub embeddings: EmbeddingIndexSummary,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct EmbeddingIndexSummary {
    pub state: MemoryState,
    pub dry_run: bool,
    pub provider: Option<String>,
    pub endpoint_host: Option<String>,
    pub model: Option<String>,
    pub sources: Vec<String>,
    pub chunks: usize,
    /// Unique embedding inputs that still need a provider call.
    pub pending_inputs: usize,
    /// Unique embedding inputs already covered by stored vectors.
    pub cached_inputs: usize,
    pub estimated_tokens: u64,
    pub estimated_usd: Option<f64>,
    /// Provider-reported tokens actually spent by this run (0 when the
    /// provider reports no usage).
    pub actual_tokens: Option<u64>,
    pub actual_usd: Option<f64>,
    pub detail: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct NativeMemorySearchOptions {
    pub query: String,
    pub limit: usize,
    pub strategy: Option<MemorySearchStrategy>,
    pub source: Option<MemorySource>,
    pub intent: Option<String>,
    pub candidate_limit: Option<usize>,
    pub exclude_thread_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct NativeMemorySearchOutput {
    pub results: Vec<NativeMemorySearchResult>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct NativeMemorySearchResult {
    pub docid: String,
    pub source: String,
    pub file: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    pub snippet: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub score: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct NativeMemoryGetOutput {
    pub docid: String,
    pub source: String,
    pub file: String,
    pub title: Option<String>,
    pub content: String,
    pub truncated: bool,
    pub next_start_byte: Option<usize>,
    pub byte_range: MemoryByteRange,
    pub content_hash: String,
    pub indexed_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MemoryByteRange {
    pub start: usize,
    pub end: usize,
    pub total: usize,
}

#[derive(Debug, Clone)]
struct SourceDocument {
    source: MemorySource,
    file: String,
    relative_path: String,
    title: Option<String>,
    content: String,
    mtime_ns: i64,
    size: i64,
    root_id: String,
}

#[derive(Debug, Clone)]
struct DocumentRow {
    docid: String,
    source: String,
    file: String,
    title: Option<String>,
    content: String,
    content_hash: String,
    indexed_at: String,
}

#[derive(Debug, Clone)]
struct ChunkHit {
    docid: String,
    source: String,
    file: String,
    title: Option<String>,
    text: String,
    score: f64,
    ordinal: i64,
}

/// Builds/refreshes the native memory index.
///
/// # Errors
/// Returns an error when canonical source discovery, export, or `SQLite` writes fail.
pub fn index_memory(
    memory_config: &MemoryConfig,
    options: NativeMemoryIndexOptions,
) -> Result<NativeMemoryIndexSummary> {
    let _lock = if options.dry_run {
        None
    } else {
        Some(IndexLock::acquire()?)
    };

    let (thread_cache, export_summary) = if options.dry_run {
        // Dry runs must not write caches, so export preflight uses the
        // full-reconcile path instead of the threads.sqlite dirty state.
        let export_summary = thread_export::export_threads_incremental(ThreadExportOptions {
            force: options.force || options.rebuild,
            dry_run: true,
        })
        .context("preflight thread transcript export")?;
        (
            thread_index::ThreadCacheSyncSummary::default(),
            export_summary,
        )
    } else {
        let (thread_cache, export_summary) =
            thread_index::sync_and_export(options.force || options.rebuild)?;
        tracing::debug!(
            files_enumerated = thread_cache.files_enumerated,
            metas_read = thread_cache.metas_read,
            rows_upserted = thread_cache.rows_upserted,
            rows_removed = thread_cache.rows_removed,
            exported = export_summary.exported,
            skipped = export_summary.skipped,
            "native memory thread cache sync"
        );
        (thread_cache, export_summary)
    };

    let docs = collect_source_documents(memory_config)?;
    let embedding_summary =
        embedding_preflight(memory_config, &docs, options.embed, options.dry_run);

    if options.dry_run {
        let chunks_indexed = docs
            .iter()
            .map(|doc| chunk_markdown(&doc.content).len())
            .sum();
        return Ok(NativeMemoryIndexSummary {
            dry_run: true,
            thread_cache,
            thread_exports: export_summary,
            documents_indexed: docs.len(),
            documents_removed: 0,
            chunks_indexed,
            generation: None,
            embeddings: embedding_summary,
            warnings: Vec::new(),
        });
    }

    let path = memory_db_path();
    let conn = open_cache(&path)?;
    ensure_schema(&conn, options.rebuild)?;
    let generation = next_generation(&conn)?;
    let indexed_at = now_rfc3339();
    let existing_docs = load_existing_documents(&conn)?;
    let mut current_docids = HashSet::new();
    let mut documents_written = 0usize;
    let mut chunks_indexed = 0usize;

    let tx = conn.unchecked_transaction()?;
    for doc in &docs {
        let docid = native_docid(doc.source, &doc.root_id, &doc.relative_path);
        current_docids.insert(docid.clone());
        // Skip unchanged documents by their source `(mtime,size)` token:
        // rewriting them would full-scan `chunk_fts` per doc (UNINDEXED
        // column deletes) and made unchanged re-index runs O(N²).
        let existed = existing_docs.get(&docid);
        if existed == Some(&(doc.mtime_ns, doc.size)) {
            continue;
        }
        let content_hash = sha256_hex(doc.content.as_bytes());
        replace_document(&tx, &docid, doc, &content_hash, generation, &indexed_at)?;
        let chunks = chunk_markdown(&doc.content);
        chunks_indexed += chunks.len();
        replace_chunks(&tx, &docid, doc, &content_hash, &chunks, existed.is_some())?;
        documents_written += 1;
    }
    let mut removed = 0usize;
    for docid in existing_docs.keys() {
        if !current_docids.contains(docid) {
            delete_document(&tx, docid)?;
            removed += 1;
        }
    }
    write_meta_tx(&tx, "schema_version", SCHEMA_VERSION)?;
    write_meta_tx(&tx, "chunker_version", CHUNKER_VERSION)?;
    write_meta_tx(&tx, "export_format_version", EXPORT_FORMAT_VERSION)?;
    write_meta_tx(&tx, "generation", &generation.to_string())?;
    write_meta_tx(&tx, "last_indexed_at", &indexed_at)?;
    tx.commit()?;

    Ok(NativeMemoryIndexSummary {
        dry_run: false,
        thread_cache,
        thread_exports: export_summary,
        documents_indexed: documents_written,
        documents_removed: removed,
        chunks_indexed,
        generation: Some(generation),
        embeddings: embedding_summary,
        warnings: Vec::new(),
    })
}

/// Reports native memory readiness without invoking qmd or mutating caches.
///
/// # Errors
/// Returns an error when thread export status cannot be inspected.
pub fn memory_status(memory_config: &MemoryConfig) -> Result<NativeMemoryStatus> {
    let export_status = thread_export_status_via_cache().context("inspect thread exports")?;
    let thread_exports = thread_exports_status_json(export_status);
    let memory_sqlite = memory_sqlite_status();
    let threads_sqlite = threads_sqlite_status();
    let embeddings = embeddings_status(memory_config);
    let mut warnings = Vec::new();
    if !memory_config.effective_notes_path().exists() {
        warnings.push(format!(
            "notes directory does not exist: {}",
            memory_config.effective_notes_path().display()
        ));
    }
    if !memory_config.effective_daily_path().exists() {
        warnings.push(format!(
            "calendar directory does not exist: {}",
            memory_config.effective_daily_path().display()
        ));
    }

    let readiness = readiness(&thread_exports, &memory_sqlite);
    Ok(NativeMemoryStatus {
        backend: "native-sqlite",
        readiness,
        thread_exports,
        threads_sqlite,
        memory_sqlite,
        embeddings,
        warnings,
    })
}

/// Searches native memory with any strategy.
///
/// Omitted/`keyword` strategies stay fully local. `vector`/`hybrid` require a
/// complete configured embedding profile and send the query (plus optional
/// intent) text to the configured embedding provider — corpus embedding is
/// never triggered from this path.
///
/// # Errors
/// Returns an error for missing/incomplete embedding configuration, missing
/// index, or provider/`SQLite` failures.
pub async fn search_memory_with_config(
    memory_config: &MemoryConfig,
    options: &NativeMemorySearchOptions,
) -> Result<NativeMemorySearchOutput> {
    match options.strategy.unwrap_or(MemorySearchStrategy::Keyword) {
        MemorySearchStrategy::Keyword => search_memory(options),
        strategy => semantic_search(memory_config, options, strategy).await,
    }
}

/// Searches the native lexical memory index.
///
/// # Errors
/// Returns an error for unsupported strategies, missing index, or `SQLite` failures.
pub fn search_memory(options: &NativeMemorySearchOptions) -> Result<NativeMemorySearchOutput> {
    let query = options.query.trim();
    if query.is_empty() {
        bail!("memory search query cannot be empty");
    }
    let strategy = options.strategy.unwrap_or(MemorySearchStrategy::Keyword);
    match strategy {
        MemorySearchStrategy::Keyword => {}
        MemorySearchStrategy::Vector | MemorySearchStrategy::Hybrid => bail!(
            "native {strategy} memory search requires a complete configured embedding profile; run `zdx memory status` and `zdx memory index --embed --dry-run` first",
            strategy = strategy.label()
        ),
    }

    let mut warnings = Vec::new();
    if options
        .intent
        .as_ref()
        .is_some_and(|s| !s.trim().is_empty())
    {
        warnings.push("intent is ignored for native keyword memory search; it is only used by vector/hybrid retrieval".to_string());
    }
    if options.candidate_limit.is_some() {
        warnings.push("candidate_limit is ignored for native keyword memory search".to_string());
    }

    let conn = open_existing_cache(&memory_db_path())?;
    let hits = lexical_hits(&conn, query, options.source, options.limit.max(1) * 8)?;
    let mut best: BTreeMap<String, ChunkHit> = BTreeMap::new();
    for hit in hits {
        if options.exclude_thread_id.as_ref().is_some_and(|excluded| {
            hit.source == MemorySource::Thread.label()
                && thread_id_from_export_file(&hit.file).as_deref() == Some(excluded.as_str())
        }) {
            continue;
        }
        best.entry(hit.docid.clone())
            .and_modify(|old| {
                let ordering = hit.score.total_cmp(&old.score);
                if ordering.is_gt() || (ordering.is_eq() && hit.ordinal < old.ordinal) {
                    *old = hit.clone();
                }
            })
            .or_insert(hit);
    }

    let mut ranked: Vec<_> = best.into_values().collect();
    ranked.sort_by(|a, b| {
        b.score
            .total_cmp(&a.score)
            .then(a.source.cmp(&b.source))
            .then(a.file.cmp(&b.file))
    });
    ranked.truncate(options.limit.max(1));

    Ok(NativeMemorySearchOutput {
        results: ranked
            .into_iter()
            .map(|hit| NativeMemorySearchResult {
                docid: hit.docid,
                source: hit.source,
                file: hit.file,
                title: hit.title,
                snippet: snippet(&hit.text, query),
                score: Some(hit.score),
            })
            .collect(),
        warnings,
    })
}

/// Vector/hybrid retrieval over the active complete embedding generation.
async fn semantic_search(
    memory_config: &MemoryConfig,
    options: &NativeMemorySearchOptions,
    strategy: MemorySearchStrategy,
) -> Result<NativeMemorySearchOutput> {
    let query = options.query.trim();
    if query.is_empty() {
        bail!("memory search query cannot be empty");
    }
    let Some(cfg) = memory_config.embeddings.as_ref() else {
        bail!(
            "native {} memory search requires [memory.embeddings] configuration; keyword search stays available",
            strategy.label()
        );
    };
    let profile = resolve_embedding_profile(cfg)?;
    let conn = open_existing_cache(&memory_db_path())?;
    let fingerprint_ok =
        read_meta(&conn, "embedding_fingerprint")?.as_deref() == Some(profile.fingerprint.as_str());
    let complete = read_meta(&conn, "embedding_complete")?.as_deref() == Some("1");
    if !fingerprint_ok || !complete {
        bail!(
            "native {} memory search requires complete embedding coverage for the current profile; run `zdx memory index --embed`",
            strategy.label()
        );
    }

    let mut warnings = vec![format!(
        "query text was sent to {} ({}) for embedding; vector/hybrid searches incur hosted cost",
        profile.provider_id, profile.model
    )];
    if let Some(source) = options.source
        && !profile.sources.contains(&source)
    {
        warnings.push(format!(
            "source '{}' is not in the [memory.embeddings] allowlist; vector results only cover embedded sources",
            source.label()
        ));
    }

    let api_key = profile.api_key()?;
    let mut query_input = query.to_string();
    if let Some(intent) = options
        .intent
        .as_deref()
        .map(str::trim)
        .filter(|intent| !intent.is_empty())
    {
        query_input.push_str("\n\ncontext: ");
        query_input.push_str(intent);
    }
    let response =
        crate::providers::embeddings::embed(&crate::providers::embeddings::EmbeddingsRequest {
            base_url: &profile.base_url,
            api_key: &api_key,
            model: &profile.model,
            dimensions: profile.dimensions,
            inputs: &[query_input],
        })
        .await
        .context("embed query text")?;
    let query_vector = normalize_vector(
        response
            .vectors
            .into_iter()
            .next()
            .context("embedding provider returned no query vector")?,
    );

    let limit = options.limit.max(1);
    let candidate_limit = options.candidate_limit.unwrap_or(50).max(limit);
    let vector_ranked = vector_doc_hits(
        &conn,
        &profile.fingerprint,
        &query_vector,
        options.source,
        options.exclude_thread_id.as_deref(),
        candidate_limit,
    )?;

    let ranked = match strategy {
        MemorySearchStrategy::Vector => vector_ranked,
        MemorySearchStrategy::Hybrid => {
            let lexical_ranked = keyword_doc_hits(
                &conn,
                query,
                options.source,
                options.exclude_thread_id.as_deref(),
                candidate_limit,
            )?;
            fuse_rrf(lexical_ranked, vector_ranked)
        }
        MemorySearchStrategy::Keyword => unreachable!("keyword handled by search_memory"),
    };

    let mut results: Vec<NativeMemorySearchResult> = ranked
        .into_iter()
        .map(|hit| NativeMemorySearchResult {
            docid: hit.docid,
            source: hit.source,
            file: hit.file,
            title: hit.title,
            snippet: snippet(&hit.text, query),
            score: Some(hit.score),
        })
        .collect();
    results.truncate(limit);
    Ok(NativeMemorySearchOutput { results, warnings })
}

/// Best chunk per document by cosine similarity, ranked descending.
fn vector_doc_hits(
    conn: &Connection,
    fingerprint: &str,
    query_vector: &[f32],
    source: Option<MemorySource>,
    exclude_thread_id: Option<&str>,
    take: usize,
) -> Result<Vec<ChunkHit>> {
    let source_filter = source.map(MemorySource::label);
    let sql = if source_filter.is_some() {
        "SELECT c.docid, d.source, d.file, d.title, c.text, c.ordinal, v.vector
         FROM chunk_vector cv
         JOIN embedding_vector v ON v.input_hash = cv.input_hash AND v.profile_fingerprint = ?1
         JOIN chunk c ON c.chunk_id = cv.chunk_id
         JOIN document d ON d.docid = c.docid
         WHERE cv.profile_fingerprint = ?1 AND d.source = ?2"
    } else {
        "SELECT c.docid, d.source, d.file, d.title, c.text, c.ordinal, v.vector
         FROM chunk_vector cv
         JOIN embedding_vector v ON v.input_hash = cv.input_hash AND v.profile_fingerprint = ?1
         JOIN chunk c ON c.chunk_id = cv.chunk_id
         JOIN document d ON d.docid = c.docid
         WHERE cv.profile_fingerprint = ?1 AND ?2 IS NULL"
    };
    let mut stmt = conn.prepare(sql)?;
    let rows = stmt.query_map(params![fingerprint, source_filter], |row| {
        Ok((
            ChunkHit {
                docid: row.get(0)?,
                source: row.get(1)?,
                file: row.get(2)?,
                title: row.get(3)?,
                text: row.get(4)?,
                score: 0.0,
                ordinal: row.get(5)?,
            },
            row.get::<_, Vec<u8>>(6)?,
        ))
    })?;

    let mut best: BTreeMap<String, ChunkHit> = BTreeMap::new();
    for row in rows {
        let (mut hit, blob) = row?;
        if excluded_thread_hit(&hit, exclude_thread_id) {
            continue;
        }
        hit.score = f64::from(dot(query_vector, &decode_vector(&blob)));
        best.entry(hit.docid.clone())
            .and_modify(|old| {
                let ordering = hit.score.total_cmp(&old.score);
                if ordering.is_gt() || (ordering.is_eq() && hit.ordinal < old.ordinal) {
                    *old = hit.clone();
                }
            })
            .or_insert(hit);
    }

    let mut ranked: Vec<_> = best.into_values().collect();
    ranked.sort_by(|a, b| {
        b.score
            .total_cmp(&a.score)
            .then(a.source.cmp(&b.source))
            .then(a.file.cmp(&b.file))
    });
    ranked.truncate(take);
    Ok(ranked)
}

/// Best chunk per document from lexical retrieval, ranked descending.
fn keyword_doc_hits(
    conn: &Connection,
    query: &str,
    source: Option<MemorySource>,
    exclude_thread_id: Option<&str>,
    take: usize,
) -> Result<Vec<ChunkHit>> {
    let hits = lexical_hits(conn, query, source, take * 8)?;
    let mut best: BTreeMap<String, ChunkHit> = BTreeMap::new();
    for hit in hits {
        if excluded_thread_hit(&hit, exclude_thread_id) {
            continue;
        }
        best.entry(hit.docid.clone())
            .and_modify(|old| {
                let ordering = hit.score.total_cmp(&old.score);
                if ordering.is_gt() || (ordering.is_eq() && hit.ordinal < old.ordinal) {
                    *old = hit.clone();
                }
            })
            .or_insert(hit);
    }
    let mut ranked: Vec<_> = best.into_values().collect();
    ranked.sort_by(|a, b| {
        b.score
            .total_cmp(&a.score)
            .then(a.source.cmp(&b.source))
            .then(a.file.cmp(&b.file))
    });
    ranked.truncate(take);
    Ok(ranked)
}

fn excluded_thread_hit(hit: &ChunkHit, exclude_thread_id: Option<&str>) -> bool {
    exclude_thread_id.is_some_and(|excluded| {
        hit.source == MemorySource::Thread.label()
            && thread_id_from_export_file(&hit.file).as_deref() == Some(excluded)
    })
}

/// Deterministic reciprocal-rank fusion of two ranked document lists.
fn fuse_rrf(lexical: Vec<ChunkHit>, vector: Vec<ChunkHit>) -> Vec<ChunkHit> {
    const RRF_K: f64 = 60.0;
    let mut fused: BTreeMap<String, ChunkHit> = BTreeMap::new();
    for list in [lexical, vector] {
        for (rank, mut hit) in list.into_iter().enumerate() {
            #[allow(clippy::cast_precision_loss)]
            let contribution = 1.0 / (RRF_K + rank as f64 + 1.0);
            if let Some(existing) = fused.get_mut(&hit.docid) {
                existing.score += contribution;
            } else {
                hit.score = contribution;
                fused.insert(hit.docid.clone(), hit);
            }
        }
    }
    let mut ranked: Vec<_> = fused.into_values().collect();
    ranked.sort_by(|a, b| {
        b.score
            .total_cmp(&a.score)
            .then(a.source.cmp(&b.source))
            .then(a.file.cmp(&b.file))
    });
    ranked
}

/// Reads a bounded indexed document snapshot by native docid, starting at
/// `start_byte` (clamped to a UTF-8 boundary) for continuation reads.
///
/// # Errors
/// Returns an error when the docid is unsupported/missing or `SQLite` fails.
pub fn get_memory_doc(docid: &str, start_byte: usize) -> Result<NativeMemoryGetOutput> {
    let docid = docid.trim();
    validate_native_docid(docid)?;
    let conn = open_existing_cache(&memory_db_path())?;
    let row = load_document(&conn, docid)?
        .with_context(|| format!("native memory docid not found in active index: {docid}"))?;
    let start = safe_boundary_floor(&row.content, start_byte.min(row.content.len()));
    let (content, end, truncated) = bounded_content(&row.content, start);
    Ok(NativeMemoryGetOutput {
        docid: row.docid,
        source: row.source,
        file: row.file,
        title: row.title,
        content,
        truncated,
        next_start_byte: truncated.then_some(end),
        byte_range: MemoryByteRange {
            start,
            end,
            total: row.content.len(),
        },
        content_hash: row.content_hash,
        indexed_at: row.indexed_at,
    })
}

fn readiness(
    thread_exports: &ThreadExportsStatusJson,
    memory_sqlite: &MemorySqliteStatus,
) -> MemoryState {
    if memory_sqlite.state == MemoryState::Incompatible || memory_sqlite.state == MemoryState::Error
    {
        return memory_sqlite.state.clone();
    }
    if memory_sqlite.state == MemoryState::Missing {
        return MemoryState::Missing;
    }
    if thread_exports.state == MemoryState::Stale {
        return MemoryState::Stale;
    }
    if memory_sqlite.documents == 0 {
        return MemoryState::Partial;
    }
    MemoryState::Ready
}

fn thread_exports_status_json(
    status: thread_export::ThreadExportStatus,
) -> ThreadExportsStatusJson {
    let state =
        if status.missing_exports > 0 || status.stale_exports > 0 || status.orphaned_exports > 0 {
            MemoryState::Stale
        } else if status.exported_threads == 0 {
            MemoryState::Missing
        } else {
            MemoryState::Ready
        };
    ThreadExportsStatusJson {
        state,
        path: config::paths::thread_exports_dir().display().to_string(),
        source_threads: status.source_threads,
        exported_threads: status.exported_threads,
        missing_exports: status.missing_exports,
        stale_exports: status.stale_exports,
        orphaned_exports: status.orphaned_exports,
        latest_source_modified: status.latest_source_modified.map(format_system_time),
        latest_export_modified: status.latest_export_modified.map(format_system_time),
    }
}

fn memory_sqlite_status() -> MemorySqliteStatus {
    let path = memory_db_path();
    if !path.exists() {
        return MemorySqliteStatus {
            state: MemoryState::Missing,
            path: path.display().to_string(),
            schema_version: None,
            generation: None,
            documents: 0,
            chunks: 0,
            last_indexed_at: None,
            detail: Some("run `zdx memory index`".to_string()),
        };
    }

    match open_existing_cache(&path).and_then(|conn| {
        let schema_version = read_meta(&conn, "schema_version")?;
        if schema_version.as_deref() != Some(SCHEMA_VERSION) {
            return Ok(MemorySqliteStatus {
                state: MemoryState::Incompatible,
                path: path.display().to_string(),
                schema_version,
                generation: None,
                documents: 0,
                chunks: 0,
                last_indexed_at: None,
                detail: Some("run `zdx memory index --rebuild`".to_string()),
            });
        }
        let documents = count_rows(&conn, "document")?;
        let chunks = count_rows(&conn, "chunk")?;
        let generation = read_meta(&conn, "generation")?.and_then(|v| v.parse::<i64>().ok());
        let last_indexed_at = read_meta(&conn, "last_indexed_at")?;
        Ok(MemorySqliteStatus {
            state: if documents == 0 {
                MemoryState::Partial
            } else {
                MemoryState::Ready
            },
            path: path.display().to_string(),
            schema_version: Some(SCHEMA_VERSION.to_string()),
            generation,
            documents,
            chunks,
            last_indexed_at,
            detail: None,
        })
    }) {
        Ok(status) => status,
        Err(err) => MemorySqliteStatus {
            state: MemoryState::Error,
            path: path.display().to_string(),
            schema_version: None,
            generation: None,
            documents: 0,
            chunks: 0,
            last_indexed_at: None,
            detail: Some(err.to_string()),
        },
    }
}

/// Embedding readiness: explicit configuration plus stored coverage state.
fn embeddings_status(memory_config: &MemoryConfig) -> MemoryComponentStatus {
    let path = memory_db_path();
    let Some(cfg) = memory_config.embeddings.as_ref() else {
        return MemoryComponentStatus {
            state: MemoryState::NotConfigured,
            path: path.display().to_string(),
            detail: Some(
                "hosted corpus embeddings require explicit [memory.embeddings] configuration and are never triggered by agent search"
                    .to_string(),
            ),
        };
    };
    let profile = match resolve_embedding_profile(cfg) {
        Ok(profile) => profile,
        Err(err) => {
            return MemoryComponentStatus {
                state: MemoryState::Error,
                path: path.display().to_string(),
                detail: Some(format!("invalid [memory.embeddings] config: {err}")),
            };
        }
    };
    if !path.exists() {
        return MemoryComponentStatus {
            state: MemoryState::Missing,
            path: path.display().to_string(),
            detail: Some("run `zdx memory index --embed`".to_string()),
        };
    }
    match open_existing_cache(&path).and_then(|conn| {
        let fingerprint_ok = read_meta(&conn, "embedding_fingerprint")?.as_deref()
            == Some(profile.fingerprint.as_str());
        let complete = read_meta(&conn, "embedding_complete")?.as_deref() == Some("1");
        Ok((fingerprint_ok, complete))
    }) {
        Ok((true, true)) => MemoryComponentStatus {
            state: MemoryState::Ready,
            path: path.display().to_string(),
            detail: Some(format!(
                "complete coverage for {}:{}",
                profile.provider_id, profile.model
            )),
        },
        Ok(_) => MemoryComponentStatus {
            state: MemoryState::Partial,
            path: path.display().to_string(),
            detail: Some(
                "embedding coverage incomplete or from another profile; run `zdx memory index --embed`"
                    .to_string(),
            ),
        },
        Err(err) => MemoryComponentStatus {
            state: MemoryState::Error,
            path: path.display().to_string(),
            detail: Some(err.to_string()),
        },
    }
}

fn threads_sqlite_status() -> MemoryComponentStatus {
    let path = thread_index::db_path();
    if !path.exists() {
        return MemoryComponentStatus {
            state: MemoryState::Missing,
            path: path.display().to_string(),
            detail: Some("run `zdx memory index`".to_string()),
        };
    }
    match thread_index::meta_row_count() {
        Ok(count) => MemoryComponentStatus {
            state: MemoryState::Ready,
            path: path.display().to_string(),
            detail: Some(format!("indexed thread metadata rows: {count}")),
        },
        Err(err) => MemoryComponentStatus {
            state: MemoryState::Error,
            path: path.display().to_string(),
            detail: Some(err.to_string()),
        },
    }
}

/// Reports export freshness via the thread index when possible (unchanged
/// threads answered without opening their JSONL files), falling back to the
/// full thread scan when the cache is missing or incompatible.
fn thread_export_status_via_cache() -> Result<thread_export::ThreadExportStatus> {
    match thread_index::export_status_sources() {
        Ok(Some(sources)) => thread_export::thread_export_status_for(&sources),
        Ok(None) | Err(_) => thread_export::thread_export_status(),
    }
}

fn collect_source_documents(memory_config: &MemoryConfig) -> Result<Vec<SourceDocument>> {
    let mut docs = Vec::new();
    collect_markdown_tree(
        &config::paths::thread_exports_dir(),
        MemorySource::Thread,
        &mut docs,
    )?;
    collect_markdown_tree(
        &memory_config.effective_notes_path(),
        MemorySource::Note,
        &mut docs,
    )?;
    collect_markdown_tree(
        &memory_config.effective_daily_path(),
        MemorySource::Calendar,
        &mut docs,
    )?;
    Ok(docs)
}

fn collect_markdown_tree(
    root: &Path,
    source: MemorySource,
    docs: &mut Vec<SourceDocument>,
) -> Result<()> {
    if !root.exists() {
        return Ok(());
    }
    let canonical_root = fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    let root_id = sha256_hex(canonical_root.to_string_lossy().as_bytes());
    let mut builder = WalkBuilder::new(root);
    builder.hidden(false).git_ignore(false).git_exclude(false);
    for entry in builder.build() {
        let entry = match entry {
            Ok(entry) => entry,
            Err(err) => {
                // Transient errors (e.g. EINTR) or unreadable entries must not
                // abort the whole index build; the file is picked up next run.
                tracing::warn!(root = %root.display(), error = %err, "skipping unreadable memory source entry");
                continue;
            }
        };
        let path = entry.path();
        if path == root || entry.file_type().is_some_and(|ft| ft.is_dir()) {
            continue;
        }
        if path.extension().is_none_or(|ext| ext != "md") {
            continue;
        }
        let relative_path = normalized_relative_path(root, path)?;
        if source != MemorySource::Thread && is_archive_or_trash_path(&relative_path) {
            continue;
        }
        let (metadata, content) = match fs::metadata(path)
            .and_then(|metadata| fs::read_to_string(path).map(|content| (metadata, content)))
        {
            Ok(pair) => pair,
            Err(err) => {
                tracing::warn!(path = %path.display(), error = %err, "skipping unreadable memory source file");
                continue;
            }
        };
        docs.push(SourceDocument {
            source,
            file: display_file(source, &relative_path),
            relative_path: relative_path.clone(),
            title: title_from_markdown(&content).or_else(|| title_from_path(&relative_path)),
            content,
            mtime_ns: mtime_nanos(metadata.modified().ok()),
            size: i64::try_from(metadata.len()).unwrap_or(i64::MAX),
            root_id: root_id.clone(),
        });
    }
    Ok(())
}

fn display_file(source: MemorySource, relative_path: &str) -> String {
    match source {
        MemorySource::Thread => format!("thread://{relative_path}"),
        MemorySource::Note => format!("note://{relative_path}"),
        MemorySource::Calendar => format!("calendar://{relative_path}"),
    }
}

fn normalized_relative_path(root: &Path, path: &Path) -> Result<String> {
    let relative = path
        .strip_prefix(root)
        .with_context(|| format!("{} is outside {}", path.display(), root.display()))?;
    let mut parts = Vec::new();
    for component in relative.components() {
        match component {
            Component::Normal(part) => parts.push(part.to_string_lossy().to_string()),
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                bail!("rejected escaping memory path: {}", path.display());
            }
        }
    }
    if parts.is_empty() {
        bail!("empty memory relative path: {}", path.display());
    }
    Ok(parts.join("/"))
}

fn is_archive_or_trash_path(path: &str) -> bool {
    path.split('/')
        .any(|component| component == "@Archive" || component == "@Trash")
}

fn title_from_markdown(content: &str) -> Option<String> {
    content.lines().find_map(|line| {
        line.strip_prefix("# ")
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(ToOwned::to_owned)
    })
}

fn title_from_path(path: &str) -> Option<String> {
    Path::new(path)
        .file_stem()
        .map(|stem| stem.to_string_lossy().to_string())
        .filter(|s| !s.is_empty())
}

fn chunk_markdown(content: &str) -> Vec<String> {
    let mut chunks = Vec::new();
    let mut current = String::new();
    for paragraph in content.split("\n\n") {
        let paragraph = paragraph.trim();
        if paragraph.is_empty() {
            continue;
        }
        push_bounded_text(&mut chunks, &mut current, paragraph);
    }
    if !current.trim().is_empty() {
        chunks.push(current.trim().to_string());
    }
    chunks
}

fn push_bounded_text(chunks: &mut Vec<String>, current: &mut String, text: &str) {
    if text.len() > MAX_CHUNK_BYTES {
        if !current.trim().is_empty() {
            chunks.push(current.trim().to_string());
            current.clear();
        }
        let mut start = 0;
        while start < text.len() {
            let end = safe_end(text, start, (start + MAX_CHUNK_BYTES).min(text.len()));
            chunks.push(text[start..end].trim().to_string());
            start = end;
        }
        return;
    }
    if current.len() + text.len() + 2 > MAX_CHUNK_BYTES && !current.trim().is_empty() {
        chunks.push(current.trim().to_string());
        current.clear();
    }
    if !current.is_empty() {
        current.push_str("\n\n");
    }
    current.push_str(text);
}

fn safe_end(text: &str, start: usize, mut end: usize) -> usize {
    while end > start && !text.is_char_boundary(end) {
        end -= 1;
    }
    end.max(start + 1).min(text.len())
}

fn lexical_hits(
    conn: &Connection,
    query: &str,
    source: Option<MemorySource>,
    overfetch: usize,
) -> Result<Vec<ChunkHit>> {
    let mut hits = fts_hits(conn, query, source, overfetch).unwrap_or_default();
    if hits.len() < overfetch {
        let mut seen: HashSet<String> = hits.iter().map(|hit| hit.docid.clone()).collect();
        for hit in like_hits(conn, query, source, overfetch)? {
            if seen.insert(hit.docid.clone()) {
                hits.push(hit);
            }
            if hits.len() >= overfetch {
                break;
            }
        }
    }
    Ok(hits)
}

fn fts_hits(
    conn: &Connection,
    query: &str,
    source: Option<MemorySource>,
    limit: usize,
) -> Result<Vec<ChunkHit>> {
    let fts_query = fts_query(query);
    let source_filter = source.map(MemorySource::label);
    let sql = if source_filter.is_some() {
        "SELECT f.docid, d.source, d.file, d.title, f.text, -bm25(chunk_fts) AS score, c.ordinal \
         FROM chunk_fts f JOIN document d ON d.docid = f.docid JOIN chunk c ON c.chunk_id = f.chunk_id \
         WHERE chunk_fts MATCH ?1 AND d.source = ?2 ORDER BY bm25(chunk_fts) ASC LIMIT ?3"
    } else {
        "SELECT f.docid, d.source, d.file, d.title, f.text, -bm25(chunk_fts) AS score, c.ordinal \
         FROM chunk_fts f JOIN document d ON d.docid = f.docid JOIN chunk c ON c.chunk_id = f.chunk_id \
         WHERE chunk_fts MATCH ?1 ORDER BY bm25(chunk_fts) ASC LIMIT ?3"
    };
    let mut stmt = conn.prepare(sql)?;
    let limit_i64 = i64::try_from(limit).unwrap_or(i64::MAX);
    let rows = if let Some(source_filter) = source_filter {
        stmt.query_map(
            params![fts_query, source_filter, limit_i64],
            chunk_hit_from_row,
        )?
        .collect::<rusqlite::Result<Vec<_>>>()?
    } else {
        stmt.query_map(
            params![fts_query, rusqlite::types::Null, limit_i64],
            chunk_hit_from_row,
        )?
        .collect::<rusqlite::Result<Vec<_>>>()?
    };
    Ok(rows)
}

fn like_hits(
    conn: &Connection,
    query: &str,
    source: Option<MemorySource>,
    limit: usize,
) -> Result<Vec<ChunkHit>> {
    let source_filter = source.map(MemorySource::label);
    let like = format!("%{}%", escape_like(query));
    let sql = if source_filter.is_some() {
        "SELECT c.docid, d.source, d.file, d.title, c.text, 0.1 AS score, c.ordinal \
         FROM chunk c JOIN document d ON d.docid = c.docid \
         WHERE d.source = ?1 AND (c.text LIKE ?2 ESCAPE '\\' OR d.file LIKE ?2 ESCAPE '\\' OR d.title LIKE ?2 ESCAPE '\\') \
         ORDER BY d.file ASC, c.ordinal ASC LIMIT ?3"
    } else {
        "SELECT c.docid, d.source, d.file, d.title, c.text, 0.1 AS score, c.ordinal \
         FROM chunk c JOIN document d ON d.docid = c.docid \
         WHERE c.text LIKE ?2 ESCAPE '\\' OR d.file LIKE ?2 ESCAPE '\\' OR d.title LIKE ?2 ESCAPE '\\' \
         ORDER BY d.file ASC, c.ordinal ASC LIMIT ?3"
    };
    let mut stmt = conn.prepare(sql)?;
    let limit_i64 = i64::try_from(limit).unwrap_or(i64::MAX);
    let rows = if let Some(source_filter) = source_filter {
        stmt.query_map(params![source_filter, like, limit_i64], chunk_hit_from_row)?
            .collect::<rusqlite::Result<Vec<_>>>()?
    } else {
        stmt.query_map(
            params![rusqlite::types::Null, like, limit_i64],
            chunk_hit_from_row,
        )?
        .collect::<rusqlite::Result<Vec<_>>>()?
    };
    Ok(rows)
}

fn chunk_hit_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<ChunkHit> {
    Ok(ChunkHit {
        docid: row.get(0)?,
        source: row.get(1)?,
        file: row.get(2)?,
        title: row.get(3)?,
        text: row.get(4)?,
        score: row.get(5)?,
        ordinal: row.get(6)?,
    })
}

fn fts_query(query: &str) -> String {
    query
        .split_whitespace()
        .filter(|term| !term.is_empty())
        .map(|term| format!("\"{}\"", term.replace('"', "\"\"")))
        .collect::<Vec<_>>()
        .join(" AND ")
}

fn escape_like(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}

fn snippet(text: &str, query: &str) -> String {
    let needle = query
        .split_whitespace()
        .next()
        .unwrap_or(query)
        .to_ascii_lowercase();
    let lower = text.to_ascii_lowercase();
    let start = lower.find(&needle).map_or(0, |idx| idx.saturating_sub(120));
    let end = (start + 320).min(text.len());
    let start = safe_boundary_floor(text, start);
    let end = safe_boundary_floor(text, end);
    let mut out = text[start..end]
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if start > 0 {
        out.insert(0, '…');
    }
    if end < text.len() {
        out.push('…');
    }
    out
}

fn safe_boundary_floor(text: &str, mut idx: usize) -> usize {
    while idx > 0 && !text.is_char_boundary(idx) {
        idx -= 1;
    }
    idx
}

fn bounded_content(content: &str, start: usize) -> (String, usize, bool) {
    let start = safe_boundary_floor(content, start.min(content.len()));
    let mut end = (start + MEMORY_GET_MAX_BYTES).min(content.len());
    end = safe_boundary_floor(content, end);
    let mut lines = 0usize;
    for (idx, ch) in content[start..end].char_indices() {
        if ch == '\n' {
            lines += 1;
            if lines >= MEMORY_GET_MAX_LINES {
                end = start + idx + ch.len_utf8();
                break;
            }
        }
    }
    (content[start..end].to_string(), end, end < content.len())
}

fn native_docid(source: MemorySource, root_id: &str, relative_path: &str) -> String {
    let digest = sha256_hex(format!("{root_id}:{}:{relative_path}", source.label()).as_bytes());
    format!(
        "zdxmem:{DOCID_VERSION}:{}:{}",
        source.label(),
        &digest[..16]
    )
}

fn validate_native_docid(docid: &str) -> Result<()> {
    if docid.starts_with('#') {
        bail!(
            "qmd docids are not supported by native memory; run `zdx memory search` to get a zdxmem:v1 docid"
        );
    }
    let parts: Vec<_> = docid.split(':').collect();
    if parts.len() != 4
        || parts[0] != "zdxmem"
        || parts[1] != DOCID_VERSION
        || MemorySource::from_label(parts[2]).is_none()
        || parts[3].len() != 16
        || !parts[3].chars().all(|ch| ch.is_ascii_hexdigit())
    {
        bail!("unsupported native memory docid; expected zdxmem:v1:<source>:<hex16>");
    }
    Ok(())
}

fn thread_id_from_export_file(file: &str) -> Option<String> {
    file.strip_prefix("thread://")
        .and_then(|path| path.strip_suffix(".md"))
        .map(ToOwned::to_owned)
}

fn replace_document(
    conn: &Connection,
    docid: &str,
    doc: &SourceDocument,
    content_hash: &str,
    generation: i64,
    indexed_at: &str,
) -> Result<()> {
    conn.execute(
        "INSERT INTO document(docid, source, file, relative_path, title, content, content_hash, source_mtime_ns, source_size, root_id, generation, indexed_at)
         VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
         ON CONFLICT(docid) DO UPDATE SET
           source=excluded.source, file=excluded.file, relative_path=excluded.relative_path,
           title=excluded.title, content=excluded.content, content_hash=excluded.content_hash,
           source_mtime_ns=excluded.source_mtime_ns, source_size=excluded.source_size,
           root_id=excluded.root_id, generation=excluded.generation, indexed_at=excluded.indexed_at",
        params![
            docid,
            doc.source.label(),
            doc.file,
            doc.relative_path,
            doc.title,
            doc.content,
            content_hash,
            doc.mtime_ns,
            doc.size,
            doc.root_id,
            generation,
            indexed_at,
        ],
    )?;
    Ok(())
}

fn replace_chunks(
    conn: &Connection,
    docid: &str,
    doc: &SourceDocument,
    content_hash: &str,
    chunks: &[String],
    existed: bool,
) -> Result<()> {
    if existed {
        conn.execute("DELETE FROM chunk WHERE docid = ?1", [docid])?;
        conn.execute("DELETE FROM chunk_fts WHERE docid = ?1", [docid])?;
    }
    let mut chunk_stmt = conn.prepare_cached(
        "INSERT INTO chunk(chunk_id, docid, source, ordinal, text, content_hash) VALUES(?1, ?2, ?3, ?4, ?5, ?6)",
    )?;
    let mut fts_stmt = conn.prepare_cached(
        "INSERT INTO chunk_fts(chunk_id, docid, source, title, path, text) VALUES(?1, ?2, ?3, ?4, ?5, ?6)",
    )?;
    for (idx, text) in chunks.iter().enumerate() {
        let chunk_id = format!("{docid}:{}", idx + 1);
        let ordinal = i64::try_from(idx).unwrap_or(i64::MAX);
        chunk_stmt.execute(params![
            chunk_id,
            docid,
            doc.source.label(),
            ordinal,
            text,
            content_hash
        ])?;
        fts_stmt.execute(params![
            chunk_id,
            docid,
            doc.source.label(),
            doc.title,
            doc.file,
            text,
        ])?;
    }
    Ok(())
}

fn delete_document(conn: &Connection, docid: &str) -> Result<()> {
    conn.execute("DELETE FROM chunk_fts WHERE docid = ?1", [docid])?;
    conn.execute("DELETE FROM chunk WHERE docid = ?1", [docid])?;
    conn.execute("DELETE FROM document WHERE docid = ?1", [docid])?;
    Ok(())
}

fn load_document(conn: &Connection, docid: &str) -> Result<Option<DocumentRow>> {
    conn.query_row(
        "SELECT docid, source, file, title, content, content_hash, indexed_at FROM document WHERE docid = ?1",
        [docid],
        |row| {
            Ok(DocumentRow {
                docid: row.get(0)?,
                source: row.get(1)?,
                file: row.get(2)?,
                title: row.get(3)?,
                content: row.get(4)?,
                content_hash: row.get(5)?,
                indexed_at: row.get(6)?,
            })
        },
    )
    .optional()
    .map_err(Into::into)
}

fn open_cache(path: &Path) -> Result<Connection> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).context("create cache directory")?;
    }
    match try_open_cache(path) {
        Ok(conn) => Ok(conn),
        Err(err) if is_cache_corruption(&err) => {
            let _ = fs::remove_file(path);
            let _ = fs::remove_file(path.with_extension("sqlite-wal"));
            let _ = fs::remove_file(path.with_extension("sqlite-shm"));
            try_open_cache(path).context("recreate native memory cache")
        }
        Err(err) => Err(err),
    }
}

fn open_existing_cache(path: &Path) -> Result<Connection> {
    if !path.exists() {
        bail!("native memory index is missing; run `zdx memory index`");
    }
    let conn = try_open_cache(path)?;
    let version = read_meta(&conn, "schema_version")?;
    if version.as_deref() != Some(SCHEMA_VERSION) {
        bail!("native memory index schema is incompatible; run `zdx memory index --rebuild`");
    }
    Ok(conn)
}

fn try_open_cache(path: &Path) -> Result<Connection> {
    let conn = Connection::open(path)?;
    conn.execute_batch("PRAGMA busy_timeout=5000; PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL; PRAGMA foreign_keys=ON;")?;
    // No `integrity_check` here: it scans every page, which costs seconds
    // once the index reaches a few hundred MB and every search pays it. The
    // schema statement below fails with a corruption code on a broken file,
    // which is what `is_cache_corruption` recovers from.
    conn.execute_batch(CREATE_META_SQL)?;
    Ok(conn)
}

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

fn ensure_schema(conn: &Connection, rebuild: bool) -> Result<()> {
    let version_ok = read_meta(conn, "schema_version")?.as_deref() == Some(SCHEMA_VERSION);
    if rebuild || !version_ok {
        conn.execute_batch(
            "DROP TABLE IF EXISTS chunk_fts;
             DROP TABLE IF EXISTS chunk;
             DROP TABLE IF EXISTS document;
             DROP TABLE IF EXISTS embedding_profile;
             DROP TABLE IF EXISTS embedding_vector;
             DROP TABLE IF EXISTS chunk_vector;",
        )?;
    }
    conn.execute_batch(CREATE_DATA_SQL)?;
    write_meta(conn, "schema_version", SCHEMA_VERSION)?;
    Ok(())
}

fn next_generation(conn: &Connection) -> Result<i64> {
    Ok(read_meta(conn, "generation")?
        .and_then(|value| value.parse::<i64>().ok())
        .unwrap_or(0)
        + 1)
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
        "INSERT INTO cache_meta(key, value) VALUES(?1, ?2) ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        (key, value),
    )?;
    Ok(())
}

fn write_meta_tx(conn: &Connection, key: &str, value: &str) -> Result<()> {
    write_meta(conn, key, value)
}

/// Existing document tokens: docid → `(source_mtime_ns, source_size)`.
fn load_existing_documents(conn: &Connection) -> Result<HashMap<String, (i64, i64)>> {
    let mut stmt = conn.prepare("SELECT docid, source_mtime_ns, source_size FROM document")?;
    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            (row.get::<_, i64>(1)?, row.get::<_, i64>(2)?),
        ))
    })?;
    rows.collect::<rusqlite::Result<HashMap<_, _>>>()
        .map_err(Into::into)
}

fn count_rows(conn: &Connection, table: &str) -> Result<usize> {
    let sql = format!("SELECT COUNT(*) FROM {table}");
    let count: i64 = conn.query_row(&sql, [], |row| row.get(0))?;
    Ok(usize::try_from(count).unwrap_or(usize::MAX))
}

fn memory_db_path() -> PathBuf {
    config::paths::zdx_home()
        .join("cache")
        .join("memory.sqlite")
}

fn lock_path() -> PathBuf {
    config::paths::zdx_home().join("cache").join("memory.lock")
}

struct IndexLock {
    path: PathBuf,
}

impl IndexLock {
    fn acquire() -> Result<Self> {
        let path = lock_path();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).context("create memory lock directory")?;
        }
        match fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&path)
        {
            Ok(_) => Ok(Self { path }),
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
                if is_stale_lock(&path) {
                    let _ = fs::remove_file(&path);
                    return Self::acquire();
                }
                bail!(
                    "native memory index is already building (lock: {})",
                    path.display()
                );
            }
            Err(err) => Err(err).context("create native memory lock"),
        }
    }
}

impl Drop for IndexLock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

fn is_stale_lock(path: &Path) -> bool {
    fs::metadata(path)
        .and_then(|metadata| metadata.modified())
        .ok()
        .and_then(|modified| modified.elapsed().ok())
        .is_some_and(|age| age > LOCK_STALE_AFTER)
}

fn embedding_preflight(
    memory_config: &MemoryConfig,
    docs: &[SourceDocument],
    requested: bool,
    dry_run: bool,
) -> EmbeddingIndexSummary {
    let profile = memory_config
        .embeddings
        .as_ref()
        .and_then(|cfg| resolve_embedding_profile(cfg).ok());
    let scoped_docs: Vec<&SourceDocument> = match &profile {
        Some(profile) => docs
            .iter()
            .filter(|doc| profile.sources.contains(&doc.source))
            .collect(),
        None => docs.iter().collect(),
    };
    let chunks = scoped_docs
        .iter()
        .map(|doc| chunk_markdown(&doc.content).len())
        .sum();
    let estimated_tokens: u64 = scoped_docs
        .iter()
        .map(|doc| estimate_tokens(&doc.content))
        .sum();
    let sources = profile.as_ref().map_or_else(
        || {
            scoped_docs
                .iter()
                .map(|doc| doc.source.label().to_string())
                .collect::<HashSet<_>>()
                .into_iter()
                .collect()
        },
        |profile| {
            profile
                .sources
                .iter()
                .map(|source| source.label().to_string())
                .collect()
        },
    );

    match profile {
        Some(profile) => EmbeddingIndexSummary {
            state: MemoryState::Unprobed,
            dry_run,
            provider: Some(profile.provider_id.clone()),
            endpoint_host: Some(profile.endpoint_host()),
            model: Some(profile.model.clone()),
            sources,
            chunks,
            pending_inputs: 0,
            cached_inputs: 0,
            estimated_tokens,
            estimated_usd: Some(profile.estimate_usd(estimated_tokens)),
            actual_tokens: None,
            actual_usd: None,
            detail: Some(if requested {
                "preflight only; hosted embedding runs report per-input pending/cached counts"
                    .to_string()
            } else {
                "embeddings configured but not requested; pass --embed to embed".to_string()
            }),
        },
        None => EmbeddingIndexSummary {
            state: MemoryState::NotConfigured,
            dry_run,
            provider: None,
            endpoint_host: None,
            model: None,
            sources,
            chunks,
            pending_inputs: 0,
            cached_inputs: 0,
            estimated_tokens,
            estimated_usd: None,
            actual_tokens: None,
            actual_usd: None,
            detail: Some(if requested {
                "hosted embeddings are not configured; set [memory.embeddings] (provider, model, sources, usd_per_million_tokens, max_run_tokens) to approve corpus upload"
            } else {
                "embeddings not requested"
            }
            .to_string()),
        },
    }
}

/// Resolved, validated hosted-embedding profile.
#[derive(Debug, Clone)]
struct ResolvedEmbeddingProfile {
    provider_id: String,
    model: String,
    base_url: String,
    api_key_env: &'static str,
    dimensions: Option<u32>,
    sources: Vec<MemorySource>,
    usd_per_million_tokens: f64,
    max_run_tokens: u64,
    /// Fingerprint over every input-affecting profile field; vectors are keyed
    /// by `(input_hash, fingerprint)` so profile changes re-embed cleanly.
    fingerprint: String,
}

impl ResolvedEmbeddingProfile {
    fn endpoint_host(&self) -> String {
        url::Url::parse(&self.base_url)
            .ok()
            .and_then(|url| url.host_str().map(ToOwned::to_owned))
            .unwrap_or_else(|| self.base_url.clone())
    }

    fn estimate_usd(&self, tokens: u64) -> f64 {
        #[allow(clippy::cast_precision_loss)]
        {
            tokens as f64 / 1_000_000.0 * self.usd_per_million_tokens
        }
    }

    fn api_key(&self) -> Result<String> {
        std::env::var(self.api_key_env).with_context(|| {
            format!(
                "missing {} for embedding provider '{}'",
                self.api_key_env, self.provider_id
            )
        })
    }
}

fn resolve_embedding_profile(
    cfg: &crate::config::MemoryEmbeddingsConfig,
) -> Result<ResolvedEmbeddingProfile> {
    let kind = crate::providers::provider_kind_from_id(&cfg.provider)
        .with_context(|| format!("unknown embedding provider '{}'", cfg.provider))?;
    let api_key_env = kind
        .api_key_env_var()
        .with_context(|| format!("provider '{}' has no API key env var", cfg.provider))?;
    let base_url = cfg
        .base_url
        .clone()
        .unwrap_or_else(|| kind.default_base_url().to_string());
    if cfg.model.trim().is_empty() {
        bail!("[memory.embeddings] model must not be blank");
    }
    let mut sources = Vec::new();
    for label in &cfg.sources {
        let source = MemorySource::from_label(label)
            .with_context(|| format!("invalid [memory.embeddings] source '{label}'"))?;
        if !sources.contains(&source) {
            sources.push(source);
        }
    }
    if sources.is_empty() {
        bail!("[memory.embeddings] sources must list at least one of thread, note, calendar");
    }
    if cfg.usd_per_million_tokens <= 0.0 {
        bail!("[memory.embeddings] usd_per_million_tokens must be positive");
    }
    if cfg.max_run_tokens == 0 {
        bail!("[memory.embeddings] max_run_tokens must be positive");
    }

    let fingerprint = sha256_hex(
        format!(
            "embed-v1|{}|{}|{}|{}|{}|input-text-v1",
            cfg.provider,
            base_url,
            cfg.model,
            cfg.dimensions.map_or_else(String::new, |d| d.to_string()),
            CHUNKER_VERSION,
        )
        .as_bytes(),
    );

    Ok(ResolvedEmbeddingProfile {
        provider_id: cfg.provider.clone(),
        model: cfg.model.clone(),
        base_url,
        api_key_env,
        dimensions: cfg.dimensions,
        sources,
        usd_per_million_tokens: cfg.usd_per_million_tokens,
        max_run_tokens: cfg.max_run_tokens,
        fingerprint,
    })
}

const EMBED_BATCH_INPUTS: usize = 64;
const EMBED_BATCH_BYTES: usize = 100_000;

/// Embeds pending memory chunks with the configured hosted profile.
///
/// Vectors are stored by `(input_hash, profile_fingerprint)`, so unchanged
/// inputs are never re-purchased and interrupted runs resume where they
/// stopped. Provider calls happen outside `SQLite` transactions; each completed
/// batch persists in a short transaction. Agent tool calls never reach this
/// function — only `zdx memory index --embed` does.
///
/// # Errors
/// Returns an error when the profile is missing/invalid, the lexical index is
/// absent, budgets would be exceeded, or provider calls fail.
pub async fn embed_memory(
    memory_config: &MemoryConfig,
    dry_run: bool,
) -> Result<EmbeddingIndexSummary> {
    let Some(cfg) = memory_config.embeddings.as_ref() else {
        bail!(
            "hosted embeddings are not configured; set [memory.embeddings] (provider, model, sources, usd_per_million_tokens, max_run_tokens) in config.toml to approve corpus upload"
        );
    };
    let profile = resolve_embedding_profile(cfg)?;

    let conn = open_existing_cache(&memory_db_path())
        .context("hosted embeddings require the lexical index; run `zdx memory index` first")?;

    // Collect allowlisted chunks and dedupe identical inputs by content hash.
    let source_labels: Vec<String> = profile
        .sources
        .iter()
        .map(|source| source.label().to_string())
        .collect();
    let (chunk_hashes, input_texts) = collect_embedding_inputs(&conn, &source_labels)?;

    let existing: HashSet<String> = {
        let mut stmt =
            conn.prepare("SELECT input_hash FROM embedding_vector WHERE profile_fingerprint = ?1")?;
        let rows = stmt.query_map([&profile.fingerprint], |row| row.get::<_, String>(0))?;
        rows.collect::<rusqlite::Result<HashSet<_>>>()?
    };

    let pending: Vec<(&String, &String)> = input_texts
        .iter()
        .filter(|(hash, _)| !existing.contains(*hash))
        .collect();
    let cached_inputs = input_texts.len() - pending.len();
    let estimated_tokens: u64 = pending.iter().map(|(_, text)| estimate_tokens(text)).sum();
    let estimated_usd = profile.estimate_usd(estimated_tokens);

    let over_budget = estimated_tokens > profile.max_run_tokens;

    let mut summary = EmbeddingIndexSummary {
        state: MemoryState::Partial,
        dry_run,
        provider: Some(profile.provider_id.clone()),
        endpoint_host: Some(profile.endpoint_host()),
        model: Some(profile.model.clone()),
        sources: source_labels.clone(),
        chunks: chunk_hashes.len(),
        pending_inputs: pending.len(),
        cached_inputs,
        estimated_tokens,
        estimated_usd: Some(estimated_usd),
        actual_tokens: None,
        actual_usd: None,
        detail: None,
    };

    if dry_run {
        summary.state = if pending.is_empty() {
            MemoryState::Ready
        } else {
            MemoryState::Partial
        };
        summary.detail = Some(if over_budget {
            format!(
                "over budget: estimated {estimated_tokens} tokens (~${estimated_usd:.4}) exceeds max_run_tokens={}; a non-dry-run would refuse to upload",
                profile.max_run_tokens
            )
        } else {
            format!(
                "dry run: no provider calls were made; a non-dry-run would embed {} inputs (pricing source: config usd_per_million_tokens={})",
                pending.len(),
                profile.usd_per_million_tokens
            )
        });
        return Ok(summary);
    }

    if over_budget {
        bail!(
            "refusing hosted embedding run: conservative estimate {estimated_tokens} tokens (~${estimated_usd:.4}) exceeds configured budget (max_run_tokens={}); raise the budget or narrow [memory.embeddings] sources",
            profile.max_run_tokens
        );
    }
    let api_key = profile.api_key()?;
    let actual_tokens = run_embedding_batches(&conn, &profile, &api_key, &pending).await?;
    let complete = finalize_embedding_run(
        &conn,
        &profile,
        &chunk_hashes,
        &source_labels,
        actual_tokens,
    )?;

    summary.state = if complete {
        MemoryState::Ready
    } else {
        MemoryState::Partial
    };
    summary.actual_tokens = Some(actual_tokens);
    summary.actual_usd = Some(profile.estimate_usd(actual_tokens));
    summary.detail = Some(format!(
        "embedded {} inputs, reused {} cached (pricing source: config usd_per_million_tokens={})",
        summary.pending_inputs, summary.cached_inputs, profile.usd_per_million_tokens
    ));
    Ok(summary)
}

/// Chunk `(chunk_id, input_hash)` pairs for the embedding scope.
type ChunkHashRows = Vec<(String, String)>;

/// Loads allowlisted chunk `(chunk_id, input_hash)` pairs plus the deduped
/// `input_hash -> text` map for embedding.
fn collect_embedding_inputs(
    conn: &Connection,
    source_labels: &[String],
) -> Result<(ChunkHashRows, BTreeMap<String, String>)> {
    let placeholders = vec!["?"; source_labels.len()].join(", ");
    let sql = format!(
        "SELECT chunk_id, text FROM chunk WHERE source IN ({placeholders}) ORDER BY chunk_id"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(rusqlite::params_from_iter(source_labels.iter()), |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;

    let mut input_texts: BTreeMap<String, String> = BTreeMap::new();
    let mut chunk_hashes: Vec<(String, String)> = Vec::new();
    for row in rows {
        let (chunk_id, text) = row?;
        let hash = sha256_hex(text.as_bytes());
        chunk_hashes.push((chunk_id, hash.clone()));
        input_texts.entry(hash).or_insert(text);
    }
    Ok((chunk_hashes, input_texts))
}

/// Calls the provider in bounded batches outside `SQLite` transactions and
/// persists each completed batch in a short transaction, so interrupted runs
/// resume without repurchasing stored vectors. Returns provider-reported
/// tokens.
async fn run_embedding_batches(
    conn: &Connection,
    profile: &ResolvedEmbeddingProfile,
    api_key: &str,
    pending: &[(&String, &String)],
) -> Result<u64> {
    let mut batches: Vec<Vec<(&String, &String)>> = Vec::new();
    let mut batch: Vec<(&String, &String)> = Vec::new();
    let mut batch_bytes = 0usize;
    for item in pending {
        if !batch.is_empty()
            && (batch.len() >= EMBED_BATCH_INPUTS || batch_bytes + item.1.len() > EMBED_BATCH_BYTES)
        {
            batches.push(std::mem::take(&mut batch));
            batch_bytes = 0;
        }
        batch_bytes += item.1.len();
        batch.push(*item);
    }
    if !batch.is_empty() {
        batches.push(batch);
    }

    let mut actual_tokens: u64 = 0;
    for batch in batches {
        let inputs: Vec<String> = batch.iter().map(|(_, text)| (*text).clone()).collect();
        let response = embed_batch_with_retry(profile, api_key, &inputs).await?;
        actual_tokens += response.prompt_tokens.unwrap_or(0);

        let created_at = now_rfc3339();
        let tx = conn.unchecked_transaction()?;
        {
            let mut stmt = tx.prepare_cached(
                "INSERT OR REPLACE INTO embedding_vector(input_hash, profile_fingerprint, dims, vector, created_at)
                 VALUES(?1, ?2, ?3, ?4, ?5)",
            )?;
            for ((hash, _), vector) in batch.iter().zip(response.vectors) {
                let normalized = normalize_vector(vector);
                stmt.execute(params![
                    hash,
                    profile.fingerprint,
                    i64::try_from(normalized.len()).unwrap_or(i64::MAX),
                    encode_vector(&normalized),
                    created_at,
                ])?;
            }
        }
        tx.commit()?;
    }
    Ok(actual_tokens)
}

/// One embeddings API batch with bounded retries on transient failures
/// (HTTP 5xx/429 and transport errors); non-transient errors abort the run,
/// which resumes later without repurchasing persisted batches.
async fn embed_batch_with_retry(
    profile: &ResolvedEmbeddingProfile,
    api_key: &str,
    inputs: &[String],
) -> Result<crate::providers::embeddings::EmbeddingsResponse> {
    const MAX_ATTEMPTS: u32 = 4;
    let mut delay = Duration::from_secs(2);
    let mut attempt = 1;
    loop {
        let result =
            crate::providers::embeddings::embed(&crate::providers::embeddings::EmbeddingsRequest {
                base_url: &profile.base_url,
                api_key,
                model: &profile.model,
                dimensions: profile.dimensions,
                inputs,
            })
            .await;
        match result {
            Ok(response) => return Ok(response),
            Err(err) if attempt < MAX_ATTEMPTS && is_transient_embed_error(&err) => {
                tracing::warn!(attempt, error = %err, "transient embeddings failure; retrying");
                tokio::time::sleep(delay).await;
                delay *= 2;
                attempt += 1;
            }
            Err(err) => return Err(err),
        }
    }
}

fn is_transient_embed_error(err: &anyhow::Error) -> bool {
    let message = err.to_string();
    ["with 500", "with 502", "with 503", "with 504", "with 429"]
        .iter()
        .any(|status| message.contains(status))
        || message.contains("send embeddings request")
}

/// Refreshes chunk-to-vector mappings, drops stale rows/orphan vectors, and/// records coverage state. Returns whether allowlisted coverage is complete.
fn finalize_embedding_run(
    conn: &Connection,
    profile: &ResolvedEmbeddingProfile,
    chunk_hashes: &[(String, String)],
    source_labels: &[String],
    actual_tokens: u64,
) -> Result<bool> {
    let tx = conn.unchecked_transaction()?;
    {
        let mut stmt = tx.prepare_cached(
            "INSERT OR REPLACE INTO chunk_vector(chunk_id, input_hash, profile_fingerprint) VALUES(?1, ?2, ?3)",
        )?;
        for (chunk_id, hash) in chunk_hashes {
            stmt.execute(params![chunk_id, hash, profile.fingerprint])?;
        }
    }
    tx.execute(
        "DELETE FROM chunk_vector WHERE chunk_id NOT IN (SELECT chunk_id FROM chunk)",
        [],
    )?;
    tx.execute(
        "DELETE FROM embedding_vector WHERE NOT EXISTS (
             SELECT 1 FROM chunk_vector cv
             WHERE cv.input_hash = embedding_vector.input_hash
               AND cv.profile_fingerprint = embedding_vector.profile_fingerprint
         )",
        [],
    )?;
    let covered: i64 = tx.query_row(
        "SELECT COUNT(*) FROM chunk_vector cv
         JOIN embedding_vector v ON v.input_hash = cv.input_hash AND v.profile_fingerprint = ?1
         WHERE cv.profile_fingerprint = ?1",
        [&profile.fingerprint],
        |row| row.get(0),
    )?;
    let complete = usize::try_from(covered).unwrap_or(0) >= chunk_hashes.len();
    write_meta(&tx, "embedding_fingerprint", &profile.fingerprint)?;
    write_meta(&tx, "embedding_complete", if complete { "1" } else { "0" })?;
    write_meta(&tx, "embedding_sources", &source_labels.join(","))?;
    write_meta(&tx, "embedding_last_run_at", &now_rfc3339())?;
    write_meta(
        &tx,
        "embedding_last_actual_tokens",
        &actual_tokens.to_string(),
    )?;
    tx.commit()?;
    Ok(complete)
}

fn estimate_tokens(text: &str) -> u64 {
    u64::try_from(text.len().div_ceil(4))
        .unwrap_or(u64::MAX)
        .max(1)
}

fn normalize_vector(mut vector: Vec<f32>) -> Vec<f32> {
    let norm = vector.iter().map(|v| v * v).sum::<f32>().sqrt();
    if norm > 0.0 {
        for value in &mut vector {
            *value /= norm;
        }
    }
    vector
}

fn encode_vector(vector: &[f32]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(vector.len() * 4);
    for value in vector {
        bytes.extend_from_slice(&value.to_le_bytes());
    }
    bytes
}

fn decode_vector(bytes: &[u8]) -> Vec<f32> {
    bytes
        .chunks_exact(4)
        .map(|chunk| f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]))
        .collect()
}

fn dot(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

fn mtime_nanos(modified: Option<SystemTime>) -> i64 {
    modified
        .and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok())
        .map_or(0, |d| i64::try_from(d.as_nanos()).unwrap_or(i64::MAX))
}

fn format_system_time(time: SystemTime) -> String {
    let datetime: DateTime<Utc> = time.into();
    datetime.to_rfc3339_opts(SecondsFormat::Secs, true)
}

fn now_rfc3339() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true)
}

fn sha256_hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_docids_are_versioned_and_disjoint() {
        let docid = native_docid(MemorySource::Note, "root", "Folder/Note.md");
        assert!(docid.starts_with("zdxmem:v1:note:"));
        validate_native_docid(&docid).unwrap();
        assert!(
            validate_native_docid("#abc123")
                .unwrap_err()
                .to_string()
                .contains("qmd docids")
        );
    }

    #[test]
    fn chunking_bounds_long_text() {
        let content = "a".repeat(MAX_CHUNK_BYTES * 2 + 10);
        let chunks = chunk_markdown(&content);
        assert!(chunks.len() >= 2);
        assert!(chunks.iter().all(|chunk| chunk.len() <= MAX_CHUNK_BYTES));
    }

    #[test]
    fn memory_get_bounds_lines_and_bytes() {
        use std::fmt::Write as _;

        let mut content = String::new();
        for idx in 0..2_000 {
            writeln!(&mut content, "line {idx}").unwrap();
        }
        let (bounded, end, truncated) = bounded_content(&content, 0);
        assert!(truncated);
        assert!(end < content.len());
        assert!(bounded.lines().count() <= MEMORY_GET_MAX_LINES);
    }

    #[test]
    fn archive_and_trash_are_excluded() {
        assert!(is_archive_or_trash_path("Foo/@Archive/Bar.md"));
        assert!(is_archive_or_trash_path("@Trash/Bar.md"));
        assert!(!is_archive_or_trash_path("Foo/Bar.md"));
    }
}
