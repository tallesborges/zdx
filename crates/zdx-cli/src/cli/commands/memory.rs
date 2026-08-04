//! Memory command handlers.

use anyhow::{Context, Result};
use zdx_engine::config;
use zdx_engine::core::native_memory::{
    self, MemorySearchStrategy, MemorySource, MemoryState, NativeMemoryIndexOptions,
    NativeMemorySearchOptions,
};

/// Input options for `zdx memory index`.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, Copy)]
pub struct IndexCommandOptions {
    pub force: bool,
    pub rebuild: bool,
    pub dry_run: bool,
    pub embed: bool,
    pub json: bool,
}

/// Input options for `zdx memory search`.
#[derive(Debug, Clone)]
pub struct SearchCommandOptions {
    pub query: String,
    pub limit: usize,
    pub strategy: Option<String>,
    pub source: Option<String>,
    pub intent: Option<String>,
    pub candidate_limit: Option<usize>,
    pub json: bool,
}

pub async fn index(config: &config::Config, options: IndexCommandOptions) -> Result<()> {
    let mut summary = native_memory::index_memory(
        &config.memory,
        NativeMemoryIndexOptions {
            force: options.force,
            rebuild: options.rebuild,
            dry_run: options.dry_run,
            embed: options.embed,
        },
    )
    .context("build native memory index")?;

    if options.embed {
        if options.dry_run {
            // Keep the config-aware preflight summary when the dry-run probe
            // cannot run (e.g. embeddings unconfigured or lexical index absent).
            if let Ok(embeddings) = native_memory::embed_memory(&config.memory, true).await {
                summary.embeddings = embeddings;
            }
        } else {
            summary.embeddings = native_memory::embed_memory(&config.memory, false)
                .await
                .context("embed native memory corpus")?;
        }
    }

    if options.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&summary)
                .context("serialize native memory index summary")?
        );
        return Ok(());
    }

    println!(
        "Thread cache: files={}, metas_read={}, upserted={}, removed={}",
        summary.thread_cache.files_enumerated,
        summary.thread_cache.metas_read,
        summary.thread_cache.rows_upserted,
        summary.thread_cache.rows_removed
    );
    println!(
        "Thread exports: exported={}, skipped={}, removed={}, failed={}",
        summary.thread_exports.exported,
        summary.thread_exports.skipped,
        summary.thread_exports.removed,
        summary.thread_exports.failed
    );
    println!(
        "Native memory: documents={}, removed={}, chunks={}, generation={}",
        summary.documents_indexed,
        summary.documents_removed,
        summary.chunks_indexed,
        summary.generation.map_or_else(
            || "dry-run".to_string(),
            |generation| generation.to_string()
        )
    );
    if options.embed || options.dry_run {
        println!(
            "Embeddings: state={}, chunks={}, pending={}, cached={}, estimated_tokens={}{}{}",
            state_label(&summary.embeddings.state),
            summary.embeddings.chunks,
            summary.embeddings.pending_inputs,
            summary.embeddings.cached_inputs,
            summary.embeddings.estimated_tokens,
            summary
                .embeddings
                .estimated_usd
                .map_or_else(String::new, |usd| format!(", estimated_usd={usd:.4}")),
            summary
                .embeddings
                .actual_tokens
                .map_or_else(String::new, |tokens| format!(", actual_tokens={tokens}")),
        );
        if let Some(detail) = &summary.embeddings.detail {
            println!("Embeddings detail: {detail}");
        }
    }
    for warning in &summary.warnings {
        println!("Warning: {warning}");
    }
    Ok(())
}

pub fn status(config: &config::Config, json: bool) -> Result<()> {
    let status =
        native_memory::memory_status(&config.memory).context("inspect native memory status")?;
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&status).context("serialize native memory status")?
        );
        return Ok(());
    }

    println!(
        "Memory search readiness: {}",
        state_label(&status.readiness)
    );
    println!("Backend: {}", status.backend);

    println!("\nThread exports:");
    println!("  State: {}", state_label(&status.thread_exports.state));
    println!("  Path: {}", status.thread_exports.path);
    println!("  Source threads: {}", status.thread_exports.source_threads);
    println!(
        "  Exported transcripts: {}",
        status.thread_exports.exported_threads
    );
    println!(
        "  Missing exports: {}",
        status.thread_exports.missing_exports
    );
    println!("  Stale exports: {}", status.thread_exports.stale_exports);
    println!(
        "  Orphaned exports: {}",
        status.thread_exports.orphaned_exports
    );
    println!(
        "  Latest source update: {}",
        status
            .thread_exports
            .latest_source_modified
            .as_deref()
            .unwrap_or("unknown")
    );
    println!(
        "  Latest export update: {}",
        status
            .thread_exports
            .latest_export_modified
            .as_deref()
            .unwrap_or("unknown")
    );

    println!("\nthreads.sqlite:");
    println!("  State: {}", state_label(&status.threads_sqlite.state));
    println!("  Path: {}", status.threads_sqlite.path);
    if let Some(detail) = &status.threads_sqlite.detail {
        println!("  Detail: {detail}");
    }

    println!("\nmemory.sqlite:");
    println!("  State: {}", state_label(&status.memory_sqlite.state));
    println!("  Path: {}", status.memory_sqlite.path);
    println!(
        "  Schema version: {}",
        status
            .memory_sqlite
            .schema_version
            .as_deref()
            .unwrap_or("unknown")
    );
    println!(
        "  Generation: {}",
        status
            .memory_sqlite
            .generation
            .map_or_else(|| "unknown".to_string(), |value| value.to_string())
    );
    println!("  Documents: {}", status.memory_sqlite.documents);
    println!("  Chunks: {}", status.memory_sqlite.chunks);
    println!(
        "  Last indexed: {}",
        status
            .memory_sqlite
            .last_indexed_at
            .as_deref()
            .unwrap_or("never")
    );
    if let Some(detail) = &status.memory_sqlite.detail {
        println!("  Detail: {detail}");
    }

    println!("\nEmbeddings:");
    println!("  State: {}", state_label(&status.embeddings.state));
    if let Some(detail) = &status.embeddings.detail {
        println!("  Detail: {detail}");
    }
    for warning in &status.warnings {
        println!("Warning: {warning}");
    }

    Ok(())
}

pub async fn search(options: &SearchCommandOptions, config: &config::Config) -> Result<()> {
    let query = options.query.trim().to_string();
    if query.is_empty() {
        anyhow::bail!("query is required");
    }

    let output = native_memory::search_memory_with_config(
        &config.memory,
        &NativeMemorySearchOptions {
            query,
            limit: options.limit.max(1),
            strategy: options
                .strategy
                .as_deref()
                .map(parse_search_strategy)
                .transpose()?,
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
    .await
    .context("search native memory")?;

    if options.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&output)
                .context("serialize native memory search results")?
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

pub fn get(docid: &str, start_byte: usize, json: bool) -> Result<()> {
    let output =
        native_memory::get_memory_doc(docid, start_byte).context("read native memory document")?;
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&output).context("serialize native memory get result")?
        );
        return Ok(());
    }

    println!("Docid: {}", output.docid);
    println!("Source: {}", output.source);
    println!("File: {}", output.file);
    if let Some(title) = &output.title {
        println!("Title: {title}");
    }
    println!(
        "Range: {}..{} of {}{}",
        output.byte_range.start,
        output.byte_range.end,
        output.byte_range.total,
        if output.truncated { " (truncated)" } else { "" }
    );
    if let Some(next) = output.next_start_byte {
        println!("Next start byte: {next}");
    }
    println!("\n{}", output.content);
    Ok(())
}

fn parse_search_strategy(value: &str) -> Result<MemorySearchStrategy> {
    match value {
        "keyword" => Ok(MemorySearchStrategy::Keyword),
        "vector" => Ok(MemorySearchStrategy::Vector),
        "hybrid" => Ok(MemorySearchStrategy::Hybrid),
        _ => anyhow::bail!("invalid memory search strategy '{value}'"),
    }
}

fn parse_search_source(value: &str) -> Result<MemorySource> {
    match value {
        "thread" => Ok(MemorySource::Thread),
        "note" => Ok(MemorySource::Note),
        "calendar" => Ok(MemorySource::Calendar),
        _ => anyhow::bail!("invalid memory search source '{value}'"),
    }
}

fn state_label(state: &MemoryState) -> &'static str {
    match state {
        MemoryState::NotConfigured => "not_configured",
        MemoryState::Unprobed => "unprobed",
        MemoryState::Missing => "missing",
        MemoryState::Building => "building",
        MemoryState::Ready => "ready",
        MemoryState::Stale => "stale",
        MemoryState::Partial => "partial",
        MemoryState::Incompatible => "incompatible",
        MemoryState::Error => "error",
    }
}
