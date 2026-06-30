//! Thread transcript exports.
//!
//! Markdown exports are derived from canonical JSONL thread files and are
//! disposable search documents.

use std::collections::HashSet;
use std::fs::{self, File};
use std::io::Write;
use std::path::PathBuf;
use std::time::SystemTime;

use anyhow::{Context, Result};

use crate::config::paths::thread_exports_dir;
use crate::core::thread_persistence::{self, ThreadEvent};

/// Options for batch thread transcript export.
#[derive(Debug, Clone, Copy, Default)]
pub struct ThreadExportOptions {
    /// Regenerate exports even when they are up to date.
    pub force: bool,
    /// Report what would change without writing or removing files.
    pub dry_run: bool,
}

/// Counts from a batch thread transcript export.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ThreadExportSummary {
    pub exported: usize,
    pub skipped: usize,
    pub removed: usize,
    pub failed: usize,
}

/// Diagnostic state for exported thread transcripts.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ThreadExportStatus {
    pub source_threads: usize,
    pub exported_threads: usize,
    pub missing_exports: usize,
    pub stale_exports: usize,
    pub orphaned_exports: usize,
    pub latest_source_modified: Option<SystemTime>,
    pub latest_export_modified: Option<SystemTime>,
}

/// Incrementally exports all saved threads and removes stale thread exports.
///
/// # Errors
/// Returns an error if thread/export directory discovery fails.
pub fn export_threads_incremental(options: ThreadExportOptions) -> Result<ThreadExportSummary> {
    let threads = thread_persistence::list_threads().context("list threads for export")?;
    let export_dir = thread_exports_dir();
    let mut summary = ThreadExportSummary::default();
    let mut thread_ids = HashSet::with_capacity(threads.len());

    for thread in threads {
        thread_ids.insert(thread.id.clone());
        match export_one_incremental(&thread.id, thread.modified, options) {
            Ok(ExportAction::Exported) => summary.exported += 1,
            Ok(ExportAction::Skipped) => summary.skipped += 1,
            Err(_) => summary.failed += 1,
        }
    }

    if export_dir.exists() {
        for entry in fs::read_dir(&export_dir).context("read thread exports directory")? {
            let entry = entry.context("read thread export entry")?;
            let path = entry.path();
            if path.extension().is_none_or(|ext| ext != "md") {
                continue;
            }

            let Some(thread_id) = path
                .file_stem()
                .map(|stem| stem.to_string_lossy().to_string())
            else {
                continue;
            };
            if thread_ids.contains(&thread_id) {
                continue;
            }

            if !options.dry_run && fs::remove_file(&path).is_err() {
                summary.failed += 1;
                continue;
            }
            summary.removed += 1;
        }
    }

    Ok(summary)
}

/// Reports freshness of exported thread transcripts without writing files.
///
/// # Errors
/// Returns an error if thread/export directory discovery fails.
pub fn thread_export_status() -> Result<ThreadExportStatus> {
    let threads = thread_persistence::list_threads().context("list threads for export status")?;
    let export_dir = thread_exports_dir();
    let mut status = ThreadExportStatus {
        source_threads: threads.len(),
        ..ThreadExportStatus::default()
    };
    let mut thread_ids = HashSet::with_capacity(threads.len());

    for thread in threads {
        thread_ids.insert(thread.id.clone());
        status.latest_source_modified = max_time(status.latest_source_modified, thread.modified);

        let export_path = export_dir.join(format!("{}.md", thread.id));
        let Ok(metadata) = fs::metadata(&export_path) else {
            status.missing_exports += 1;
            continue;
        };
        status.exported_threads += 1;
        let export_modified = metadata.modified().ok();
        status.latest_export_modified = max_time(status.latest_export_modified, export_modified);
        if let (Some(source_modified), Some(export_modified)) = (thread.modified, export_modified)
            && export_modified < source_modified
        {
            status.stale_exports += 1;
        }
    }

    if export_dir.exists() {
        for entry in fs::read_dir(&export_dir).context("read thread exports directory")? {
            let entry = entry.context("read thread export entry")?;
            let path = entry.path();
            if path.extension().is_none_or(|ext| ext != "md") {
                continue;
            }
            let Some(thread_id) = path
                .file_stem()
                .map(|stem| stem.to_string_lossy().to_string())
            else {
                continue;
            };
            if !thread_ids.contains(&thread_id) {
                status.orphaned_exports += 1;
            }
        }
    }

    Ok(status)
}

/// Exports one saved thread to `$ZDX_HOME/exports/threads/<thread_id>.md`.
///
/// # Errors
/// Returns an error if the canonical thread cannot be loaded or the export cannot be written.
pub fn export_thread(thread_id: &str) -> Result<PathBuf> {
    let events = thread_persistence::load_thread_events(thread_id)
        .with_context(|| format!("load thread '{thread_id}' for export"))?;
    let markdown = format_transcript_markdown(thread_id, &events);
    write_thread_export(thread_id, &markdown)
}

/// Formats thread events as the MVP Markdown transcript export.
#[must_use]
pub fn format_transcript_markdown(thread_id: &str, events: &[ThreadEvent]) -> String {
    let mut output = format!("# Thread {thread_id}\n\n");

    for event in events {
        let ThreadEvent::Message { role, text, .. } = event else {
            continue;
        };

        let label = match role.as_str() {
            "user" => "User",
            "assistant" => "Assistant",
            _ => continue,
        };

        let text = collapse_whitespace(text);
        if text.is_empty() {
            continue;
        }

        output.push_str(label);
        output.push_str(": ");
        output.push_str(&text);
        output.push('\n');
    }

    output
}

fn write_thread_export(thread_id: &str, markdown: &str) -> Result<PathBuf> {
    let dir = thread_exports_dir();
    fs::create_dir_all(&dir).context("create thread exports directory")?;

    let path = dir.join(format!("{thread_id}.md"));
    let temp_path = path.with_extension("md.tmp");

    let mut file = File::create(&temp_path).context("create temp thread export")?;
    file.write_all(markdown.as_bytes())
        .context("write temp thread export")?;
    file.sync_all().context("sync temp thread export")?;
    fs::rename(&temp_path, &path).context("replace thread export")?;

    Ok(path)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ExportAction {
    Exported,
    Skipped,
}

fn export_one_incremental(
    thread_id: &str,
    source_modified: Option<std::time::SystemTime>,
    options: ThreadExportOptions,
) -> Result<ExportAction> {
    let export_path = thread_exports_dir().join(format!("{thread_id}.md"));

    if !options.force
        && let Some(source_modified) = source_modified
        && let Ok(export_metadata) = fs::metadata(&export_path)
        && let Ok(export_modified) = export_metadata.modified()
        && export_modified >= source_modified
    {
        return Ok(ExportAction::Skipped);
    }

    if !options.dry_run {
        export_thread(thread_id)?;
    }
    Ok(ExportAction::Exported)
}

fn collapse_whitespace(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn max_time(left: Option<SystemTime>, right: Option<SystemTime>) -> Option<SystemTime> {
    match (left, right) {
        (Some(left), Some(right)) => Some(left.max(right)),
        (Some(left), None) => Some(left),
        (None, Some(right)) => Some(right),
        (None, None) => None,
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn formats_user_and_assistant_messages_only() {
        let events = vec![
            ThreadEvent::meta_with_root(None),
            ThreadEvent::user_message("hello\n\tthere"),
            ThreadEvent::ToolUse {
                id: "tool-1".to_string(),
                name: "read".to_string(),
                input: json!({ "file_path": "notes.md" }),
                id_origin: zdx_types::IdOrigin::Synthesized,
                replay: None,
                ts: "2026-05-10T00:00:00Z".to_string(),
            },
            ThreadEvent::ToolResult {
                tool_use_id: "tool-1".to_string(),
                output: json!({ "content": "noise" }),
                ok: true,
                ts: "2026-05-10T00:00:00Z".to_string(),
            },
            ThreadEvent::assistant_message("answer   with\nspaces"),
        ];

        assert_eq!(
            format_transcript_markdown("abc123", &events),
            "# Thread abc123\n\nUser: hello there\nAssistant: answer with spaces\n"
        );
    }

    #[test]
    fn skips_empty_collapsed_messages() {
        let events = vec![
            ThreadEvent::user_message(" \n\t "),
            ThreadEvent::assistant_message("done"),
        ];

        assert_eq!(
            format_transcript_markdown("thread-1", &events),
            "# Thread thread-1\n\nAssistant: done\n"
        );
    }
}
