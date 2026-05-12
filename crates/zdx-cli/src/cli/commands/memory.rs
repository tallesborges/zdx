//! Memory command handlers.

use std::time::SystemTime;

use anyhow::{Context, Result};
use chrono::{DateTime, SecondsFormat, Utc};
use zdx_engine::config;
use zdx_engine::core::qmd::{self, QmdMemoryCollectionState};
use zdx_engine::core::thread_export::{self, ThreadExportOptions};

/// Input options for `zdx memory search`.
#[derive(Debug, Clone)]
pub struct SearchCommandOptions {
    pub query: String,
    pub limit: usize,
    pub strategy: String,
    pub source: Option<String>,
    pub intent: Option<String>,
    pub candidate_limit: Option<usize>,
    pub json: bool,
}

pub fn index(config: &config::Config) -> Result<()> {
    let export_summary = thread_export::export_threads_incremental(ThreadExportOptions::default())
        .context("export threads before qmd indexing")?;

    println!(
        "Thread exports: exported={}, skipped={}, removed={}, failed={}",
        export_summary.exported,
        export_summary.skipped,
        export_summary.removed,
        export_summary.failed
    );

    let index_summary = qmd::index_memory_collections(&config.qmd, &config.memory)
        .context("index memory with qmd")?;
    let installed = if index_summary.installed {
        " (installed)"
    } else {
        ""
    };
    println!(
        "qmd binary: {}{}",
        index_summary.binary_path.display(),
        installed
    );
    for collection in &index_summary.collections {
        let collection_action = if collection.collection_added {
            "created"
        } else {
            "updated"
        };
        println!(
            "qmd collection: {} {} at {}",
            collection_action,
            collection.name,
            collection.root_dir.display()
        );
    }
    println!("qmd index: updated and embedded ZDX memory collections");
    let last_successful_index_at =
        qmd::record_memory_index_success().context("record successful memory index run")?;
    println!("Last successful index: {last_successful_index_at}");

    Ok(())
}

pub fn status(config: &config::Config) -> Result<()> {
    let qmd_status =
        qmd::memory_status(&config.qmd, &config.memory).context("inspect qmd memory status")?;
    let export_status =
        thread_export::thread_export_status().context("inspect thread export status")?;

    println!(
        "Memory search readiness: {}",
        readiness(&qmd_status, &export_status)
    );

    println!("\nqmd binary:");
    if let Some(binary) = &qmd_status.binary {
        println!("  Found: yes");
        println!("  Command: {}", binary.command);
        println!("  Path: {}", binary.path.display());
        match (&binary.version, &binary.version_error) {
            (Some(version), _) => println!("  Version: {version}"),
            (None, Some(error)) => println!("  Version: unavailable ({error})"),
            (None, None) => println!("  Version: unavailable"),
        }
    } else {
        println!("  Found: no");
        println!("  Command: {}", config.qmd.command);
        println!("  Install: run `zdx memory index` or install qmd on PATH");
    }

    println!("\nThread exports:");
    println!("  Source threads: {}", export_status.source_threads);
    println!("  Exported transcripts: {}", export_status.exported_threads);
    println!("  Missing exports: {}", export_status.missing_exports);
    println!("  Stale exports: {}", export_status.stale_exports);
    println!("  Orphaned exports: {}", export_status.orphaned_exports);
    println!(
        "  Latest source update: {}",
        format_system_time(export_status.latest_source_modified)
    );
    println!(
        "  Latest export update: {}",
        format_system_time(export_status.latest_export_modified)
    );

    println!("\nqmd collections:");
    for collection in &qmd_status.collections {
        println!(
            "  {} ({}) — {}",
            collection.name,
            collection.source,
            collection_state_label(collection.state)
        );
        println!("    Path: {}", collection.expected_root_dir.display());
        println!("    Pattern: {}", collection.expected_pattern);
        if let Some(detail) = &collection.detail {
            println!("    Detail: {detail}");
        }
    }

    println!("\nLast successful index:");
    match &qmd_status.last_successful_index_at {
        Some(timestamp) => println!("  {timestamp}"),
        None => println!(
            "  Not recorded yet (run `zdx memory index`; status file: {})",
            qmd_status.status_path.display()
        ),
    }

    Ok(())
}

pub fn search(options: &SearchCommandOptions, config: &config::Config) -> Result<()> {
    let query = options.query.trim().to_string();
    if query.is_empty() {
        anyhow::bail!("query is required");
    }

    let export_summary = thread_export::export_threads_incremental(ThreadExportOptions::default())
        .context("export threads before qmd search")?;

    let mut output = qmd::search_memory_collections(
        &config.qmd,
        &config.memory,
        &qmd::QmdMemorySearchOptions {
            query,
            limit: options.limit.max(1),
            strategy: parse_search_strategy(&options.strategy)?,
            source: options
                .source
                .as_deref()
                .map(parse_search_source)
                .transpose()?,
            intent: options
                .intent
                .as_ref()
                .map(|intent| intent.trim().to_string())
                .filter(|intent| !intent.is_empty()),
            candidate_limit: options.candidate_limit.map(|limit| limit.max(1)),
            exclude_thread_id: None,
        },
    )
    .context("search memory with qmd")?;

    if export_summary.exported > 0 || export_summary.removed > 0 || export_summary.failed > 0 {
        output.warnings.insert(
            0,
            format!(
                "thread exports changed before search (exported={}, removed={}, failed={}); run `zdx memory index` to refresh qmd if results look stale",
                export_summary.exported, export_summary.removed, export_summary.failed
            ),
        );
    }

    if options.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&output).context("serialize qmd memory search results")?
        );
        return Ok(());
    }

    for warning in &output.warnings {
        println!("Warning: {warning}");
    }

    if output.results.is_empty() {
        println!("No memory results found matching the query.");
        return Ok(());
    }

    for result in output.results {
        let title = result.title.as_deref().unwrap_or(&result.file);
        match result.score {
            Some(score) => println!("[{}] {}  score={score:.3}", result.docid, title),
            None => println!("[{}] {}", result.docid, title),
        }
        if result.title.is_some() {
            println!("  File: {}", result.file);
        }
        if !result.snippet.is_empty() {
            println!("  Snippet: {}", result.snippet);
        }
        println!();
    }

    Ok(())
}

fn parse_search_strategy(value: &str) -> Result<qmd::QmdMemorySearchStrategy> {
    match value {
        "keyword" => Ok(qmd::QmdMemorySearchStrategy::Keyword),
        "vector" => Ok(qmd::QmdMemorySearchStrategy::Vector),
        "hybrid" => Ok(qmd::QmdMemorySearchStrategy::Hybrid),
        _ => anyhow::bail!("invalid memory search strategy '{value}'"),
    }
}

fn parse_search_source(value: &str) -> Result<qmd::QmdMemorySearchSource> {
    match value {
        "thread" => Ok(qmd::QmdMemorySearchSource::Thread),
        "note" => Ok(qmd::QmdMemorySearchSource::Note),
        "calendar" => Ok(qmd::QmdMemorySearchSource::Calendar),
        _ => anyhow::bail!("invalid memory search source '{value}'"),
    }
}

fn readiness(
    qmd_status: &qmd::QmdMemoryStatus,
    export_status: &thread_export::ThreadExportStatus,
) -> &'static str {
    if qmd_status.binary.is_none() {
        return "unavailable (qmd binary not found)";
    }
    if qmd_status.last_successful_index_at.is_none() {
        return "needs `zdx memory index` (no successful index recorded)";
    }
    if qmd_status
        .collections
        .iter()
        .any(|collection| collection.state != QmdMemoryCollectionState::Ready)
    {
        return "needs `zdx memory index` (collection issue)";
    }
    if export_status.missing_exports > 0
        || export_status.stale_exports > 0
        || export_status.orphaned_exports > 0
    {
        return "needs `zdx memory index` (thread exports changed)";
    }
    "ready"
}

fn collection_state_label(state: QmdMemoryCollectionState) -> &'static str {
    match state {
        QmdMemoryCollectionState::Ready => "ready",
        QmdMemoryCollectionState::Missing => "missing",
        QmdMemoryCollectionState::Mismatch => "mismatch",
        QmdMemoryCollectionState::Unavailable => "unavailable",
        QmdMemoryCollectionState::Error => "error",
    }
}

fn format_system_time(time: Option<SystemTime>) -> String {
    match time {
        Some(time) => {
            let datetime: DateTime<Utc> = time.into();
            datetime.to_rfc3339_opts(SecondsFormat::Secs, true)
        }
        None => "unknown".to_string(),
    }
}
