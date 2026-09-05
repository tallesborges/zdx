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
use serde::Serialize;

use crate::config::paths::thread_exports_dir;
use crate::core::thread_persistence::{self, ThreadEvent};

/// Version tag for the Markdown transcript export format. Bumping it makes the
/// thread index re-export every transcript without `--force`.
pub(crate) const EXPORT_FORMAT_VERSION: &str = "thread-md-v1";

/// Records which format produced the exports currently on disk.
///
/// Freshness of an individual export is a filesystem question (is the `.md`
/// newer than its `.jsonl`), so the only thing that needs recording is the
/// format the whole directory was written in. Keeping it as a file beside the
/// exports means no derived database can lose it, and a format bump still
/// re-exports everything exactly once.
fn format_stamp_path() -> PathBuf {
    thread_exports_dir().join(".export-format")
}

/// Whether the exports on disk were produced by [`EXPORT_FORMAT_VERSION`].
///
/// An unreadable or absent stamp reports `false`, which forces one full
/// re-export and then writes the stamp.
pub(crate) fn exports_match_current_format() -> bool {
    fs::read_to_string(format_stamp_path()).is_ok_and(|tag| tag.trim() == EXPORT_FORMAT_VERSION)
}

/// Records the current format tag after a completed (non-dry-run) export pass.
pub(crate) fn write_format_stamp() {
    let path = format_stamp_path();
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let _ = fs::write(path, EXPORT_FORMAT_VERSION);
}

/// Options for batch thread transcript export.
#[derive(Debug, Clone, Copy, Default)]
pub struct ThreadExportOptions {
    /// Regenerate exports even when they are up to date.
    pub force: bool,
    /// Report what would change without writing or removing files.
    pub dry_run: bool,
}

/// Counts from a batch thread transcript export.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
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
    // Full-reconcile path: raw file scan, never writes the thread cache (so
    // dry runs stay write-free).
    let threads = thread_persistence::list_threads_scan().context("list threads for export")?;
    let options = ThreadExportOptions {
        force: options.force || !exports_match_current_format(),
        ..options
    };
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

    remove_orphan_exports(&thread_ids, options.dry_run, &mut summary)?;

    if !options.dry_run && summary.failed == 0 {
        write_format_stamp();
    }

    Ok(summary)
}

/// Removes exports whose thread id is not in `thread_ids`, updating `summary`.
pub(crate) fn remove_orphan_exports(
    thread_ids: &HashSet<String>,
    dry_run: bool,
    summary: &mut ThreadExportSummary,
) -> Result<()> {
    let export_dir = thread_exports_dir();
    if !export_dir.exists() {
        return Ok(());
    }
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

        if !dry_run && fs::remove_file(&path).is_err() {
            summary.failed += 1;
            continue;
        }
        summary.removed += 1;
    }
    Ok(())
}

/// Reports freshness of exported thread transcripts without writing files.
///
/// # Errors
/// Returns an error if thread/export directory discovery fails.
pub fn thread_export_status() -> Result<ThreadExportStatus> {
    let threads =
        thread_persistence::list_threads_scan().context("list threads for export status")?;
    let sources: Vec<(String, Option<SystemTime>)> = threads
        .into_iter()
        .map(|thread| (thread.id, thread.modified))
        .collect();
    thread_export_status_for(&sources)
}

/// Reports export freshness for a pre-computed `(thread_id, source_modified)`
/// list, without reading canonical thread files.
///
/// # Errors
/// Returns an error if the export directory cannot be read.
pub fn thread_export_status_for(
    threads: &[(String, Option<SystemTime>)],
) -> Result<ThreadExportStatus> {
    let export_dir = thread_exports_dir();
    let mut status = ThreadExportStatus {
        source_threads: threads.len(),
        ..ThreadExportStatus::default()
    };
    let mut thread_ids = HashSet::with_capacity(threads.len());

    for (thread_id, source_modified) in threads {
        thread_ids.insert(thread_id.clone());
        status.latest_source_modified = max_time(status.latest_source_modified, *source_modified);

        let export_path = export_dir.join(format!("{thread_id}.md"));
        let Ok(metadata) = fs::metadata(&export_path) else {
            status.missing_exports += 1;
            continue;
        };
        status.exported_threads += 1;
        let export_modified = metadata.modified().ok();
        status.latest_export_modified = max_time(status.latest_export_modified, export_modified);
        if let (Some(source_modified), Some(export_modified)) = (source_modified, export_modified)
            && export_modified < *source_modified
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
pub(crate) enum ExportAction {
    Exported,
    Skipped,
}

/// Exports one thread unless its `.md` is already newer than its `.jsonl`.
///
/// This is the single freshness rule for both export paths: the answer comes
/// from the two files themselves, so it cannot drift from, or be lost with, a
/// derived database.
pub(crate) fn export_one_incremental(
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

    /// Wiping the derived thread index must not resurrect exports that are
    /// already current on disk. A rebuild used to hand the memory indexer
    /// thousands of stale-dirty threads because export bookkeeping lived in the
    /// file the rebuild deletes.
    #[test]
    fn rebuilding_the_thread_index_does_not_make_current_exports_stale() {
        let _home = crate::test_support::temp_zdx_home();

        let thread_id = format!("export-fresh-{}", uuid::Uuid::new_v4());
        let mut thread = thread_persistence::Thread::with_id(thread_id.clone()).unwrap();
        thread.append(&ThreadEvent::user_message("hello")).unwrap();

        // First pass writes the export and the format stamp.
        let first = crate::core::thread_index::sync_and_export(false).unwrap().1;
        assert_eq!(first.exported, 1, "first pass exports the thread");
        assert!(exports_match_current_format());

        // Delete the whole index file, exactly as a SCHEMA_VERSION bump does.
        crate::core::thread_index::reset_cache_for_test();
        let db = crate::core::thread_index::db_path();
        let _ = fs::remove_file(&db);
        let _ = fs::remove_file(db.with_extension("sqlite-wal"));
        let _ = fs::remove_file(db.with_extension("sqlite-shm"));

        let second = crate::core::thread_index::sync_and_export(false).unwrap().1;
        assert_eq!(
            second.exported, 0,
            "a rebuilt index must not re-export an unchanged transcript"
        );
        assert_eq!(second.skipped, 1);
    }

    /// A format bump is the one thing that still re-exports everything.
    #[test]
    fn a_missing_format_stamp_forces_one_full_re_export() {
        let _home = crate::test_support::temp_zdx_home();

        let thread_id = format!("export-format-{}", uuid::Uuid::new_v4());
        let mut thread = thread_persistence::Thread::with_id(thread_id).unwrap();
        thread.append(&ThreadEvent::user_message("hi")).unwrap();

        assert_eq!(
            export_threads_incremental(ThreadExportOptions::default())
                .unwrap()
                .exported,
            1
        );
        assert_eq!(
            export_threads_incremental(ThreadExportOptions::default())
                .unwrap()
                .skipped,
            1,
            "second pass is a no-op"
        );

        fs::write(format_stamp_path(), "thread-md-v0").unwrap();
        let after_bump = export_threads_incremental(ThreadExportOptions::default()).unwrap();
        assert_eq!(after_bump.exported, 1, "a format change re-exports");
        assert!(exports_match_current_format(), "stamp is rewritten");
    }

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
                duration_ms: None,
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
