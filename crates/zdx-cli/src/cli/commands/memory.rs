//! Memory command handlers.

use anyhow::{Context, Result};
use zdx_engine::config;
use zdx_engine::core::qmd;
use zdx_engine::core::thread_export::{self, ThreadExportOptions};

/// Input options for `zdx memory search`.
#[derive(Debug, Clone)]
pub struct SearchCommandOptions {
    pub query: String,
    pub limit: usize,
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

    Ok(())
}

pub fn search(options: SearchCommandOptions, config: &config::Config) -> Result<()> {
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
