use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, anyhow, bail};
use zdx_core::core::thread_persistence::ThreadEvent;
use zdx_core::core::{thread_persistence as thread_log, worktree};

use crate::events::{ThreadUiEvent, UiEvent};
use crate::transcript::{HistoryCell, build_transcript_from_events};

/// Loads the list of threads.
///
/// Pure async function - runtime spawns and sends result to inbox.
pub async fn thread_list_load(
    original_cells: Vec<HistoryCell>,
    mode: crate::overlays::ThreadPickerMode,
) -> UiEvent {
    tokio::task::spawn_blocking(move || match thread_log::list_threads() {
        Ok(threads) if threads.is_empty() => UiEvent::Thread(ThreadUiEvent::ListFailed {
            error: "No threads found.".to_string(),
        }),
        Ok(threads) => UiEvent::Thread(ThreadUiEvent::ListLoaded {
            threads,
            original_cells,
            mode,
        }),
        Err(e) => UiEvent::Thread(ThreadUiEvent::ListFailed {
            error: format!("Failed to load threads: {}", e),
        }),
    })
    .await
    .unwrap_or_else(|e| {
        UiEvent::Thread(ThreadUiEvent::ListFailed {
            error: format!("Task failed: {}", e),
        })
    })
}

/// Loads a thread by ID (full switch).
///
/// Pure async function - runtime spawns and sends result to inbox.
pub async fn thread_load(thread_id: String, root: PathBuf) -> UiEvent {
    tokio::task::spawn_blocking(move || load_thread_sync(&thread_id, &root))
        .await
        .unwrap_or_else(|e| {
            UiEvent::Thread(ThreadUiEvent::LoadFailed {
                error: format!("Task failed: {}", e),
            })
        })
}

/// Synchronous thread loading (runs in blocking task).
fn load_thread_sync(thread_id: &str, root: &Path) -> UiEvent {
    // Load thread events (I/O)
    let events = match thread_log::load_thread_events(thread_id) {
        Ok(events) => events,
        Err(e) => {
            return UiEvent::Thread(ThreadUiEvent::LoadFailed {
                error: format!("Failed to load thread: {}", e),
            });
        }
    };

    // Extract usage from events before consuming them
    let usage = thread_log::extract_usage_from_thread_events(&events);

    let title = thread_log::extract_title_from_events(&events);

    // Build transcript cells from events
    let cells = build_transcript_from_events(&events);

    let stored_root = thread_log::extract_root_path_from_events(&events);

    // Build API messages for thread context
    let messages = thread_log::thread_events_to_messages(events);

    // Build input history from user messages in transcript
    let history: Vec<String> = cells
        .iter()
        .filter_map(|cell| {
            if let HistoryCell::User { content, .. } = cell {
                Some(content.clone())
            } else {
                None
            }
        })
        .collect();

    // Create or get the thread handle for future appends
    let thread_log_handle = thread_log::ThreadLog::with_id(thread_id.to_string()).ok();

    // Backfill thread root for older threads that don't have one yet.
    if stored_root.is_none()
        && let Some(mut handle) = thread_log_handle.clone()
    {
        let _ = handle.set_root_path(root);
    }

    UiEvent::Thread(ThreadUiEvent::Loaded {
        thread_id: thread_id.to_string(),
        cells,
        messages,
        history,
        stored_root: stored_root.map(PathBuf::from),
        thread_log: thread_log_handle,
        title,
        usage,
    })
}

/// Ensures a worktree for the active thread and persists it as thread root.
pub async fn thread_ensure_worktree(thread_id: String, root: PathBuf) -> UiEvent {
    tokio::task::spawn_blocking(move || match worktree::ensure_worktree(&root, &thread_id) {
        Ok(path) => {
            if let Ok(mut thread_log) = thread_log::ThreadLog::with_id(thread_id) {
                let _ = thread_log.set_root_path(&path);
            }
            UiEvent::Thread(ThreadUiEvent::WorktreeReady { path })
        }
        Err(error) => UiEvent::Thread(ThreadUiEvent::WorktreeFailed {
            error: format!("Failed to enable worktree: {}", error),
        }),
    })
    .await
    .unwrap_or_else(|e| {
        UiEvent::Thread(ThreadUiEvent::WorktreeFailed {
            error: format!("Task failed: {}", e),
        })
    })
}

/// Resolves root-derived display fields for a new root.
pub fn resolve_root_display(path: PathBuf) -> UiEvent {
    UiEvent::RootDisplayResolved {
        git_branch: get_git_branch(&path),
        display_path: shorten_path(&path),
        path,
    }
}

/// Refreshes the effective system prompt for a new root.
pub fn refresh_system_prompt(config: zdx_core::config::Config, path: PathBuf) -> UiEvent {
    let result = zdx_core::core::context::build_effective_system_prompt_with_paths(&config, &path)
        .map(|context| context.prompt)
        .map_err(|error| format!("Failed to refresh system prompt: {}", error));

    UiEvent::SystemPromptRefreshed { result }
}

fn get_git_branch(root: &Path) -> Option<String> {
    let head_path = root.join(".git/HEAD");
    if let Ok(content) = std::fs::read_to_string(head_path)
        && let Some(branch) = content.strip_prefix("ref: refs/heads/")
    {
        return Some(branch.trim().to_string());
    }
    None
}

fn shorten_path(path: &Path) -> String {
    let path = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    if let Some(home) = dirs::home_dir()
        && let Ok(relative) = path.strip_prefix(&home)
    {
        let display = format!("~/{}", relative.display());
        return compact_path_segments(display, 5);
    }
    compact_path_segments(path.display().to_string(), 5)
}

fn compact_path_segments(path: String, keep_segments_each_side: usize) -> String {
    let has_leading_slash = path.starts_with('/');
    let segments: Vec<String> = path
        .split('/')
        .filter(|s| !s.is_empty())
        .map(|segment| compact_segment(segment, 5))
        .collect();

    if segments.len() <= keep_segments_each_side * 2 {
        let joined = segments.join("/");
        if has_leading_slash {
            return format!("/{}", joined);
        }
        return joined;
    }

    let mut compact: Vec<String> = Vec::with_capacity(keep_segments_each_side * 2 + 1);
    compact.extend_from_slice(&segments[..keep_segments_each_side]);
    compact.push("...".to_string());
    compact.extend_from_slice(&segments[segments.len() - keep_segments_each_side..]);

    let joined = compact.join("/");
    if has_leading_slash {
        format!("/{}", joined)
    } else {
        joined
    }
}

fn compact_segment(segment: &str, keep_chars_each_side: usize) -> String {
    let char_count = segment.chars().count();
    if char_count <= keep_chars_each_side * 2 + 3 {
        return segment.to_string();
    }

    let start: String = segment.chars().take(keep_chars_each_side).collect();
    let end: String = segment
        .chars()
        .skip(char_count.saturating_sub(keep_chars_each_side))
        .collect();
    format!("{}...{}", start, end)
}

pub fn resolve_project_root(root: &Path) -> anyhow::Result<PathBuf> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["rev-parse", "--path-format=absolute", "--git-common-dir"])
        .output()
        .context("git rev-parse --git-common-dir")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("git rev-parse failed: {}", stderr.trim());
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let trimmed = stdout.trim();
    if trimmed.is_empty() {
        bail!("git rev-parse returned empty git common dir");
    }

    let git_common_dir = PathBuf::from(trimmed);
    let project_root = git_common_dir.parent().ok_or_else(|| {
        anyhow!(
            "cannot derive project root from git common dir: {}",
            git_common_dir.display()
        )
    })?;

    Ok(project_root
        .canonicalize()
        .unwrap_or_else(|_| project_root.to_path_buf()))
}

/// Loads a thread preview.
///
/// Pure async function - runtime spawns and sends result to inbox.
pub async fn thread_preview(thread_id: String) -> UiEvent {
    tokio::task::spawn_blocking(move || match thread_log::load_thread_events(&thread_id) {
        Ok(events) => {
            let cells = build_transcript_from_events(&events);
            UiEvent::Thread(ThreadUiEvent::PreviewLoaded { cells })
        }
        Err(_) => {
            // Silent failure for preview - errors shown on actual load
            UiEvent::Thread(ThreadUiEvent::PreviewFailed)
        }
    })
    .await
    .unwrap_or(UiEvent::Thread(ThreadUiEvent::PreviewFailed))
}

/// Creates a new thread.
///
/// Pure async function - runtime spawns and sends result to inbox.
pub async fn thread_create(config: zdx_core::config::Config, root: PathBuf) -> UiEvent {
    tokio::task::spawn_blocking(move || {
        let thread_log_handle = match thread_log::ThreadLog::new_with_root(&root) {
            Ok(thread_log_handle) => thread_log_handle,
            Err(e) => {
                return UiEvent::Thread(ThreadUiEvent::CreateFailed {
                    error: format!("Failed to create thread: {}", e),
                });
            }
        };

        // Load AGENTS.md paths and skills
        let context =
            zdx_core::core::context::build_effective_system_prompt_with_paths(&config, &root)
                .unwrap_or_default();

        UiEvent::Thread(ThreadUiEvent::Created {
            thread_log: thread_log_handle,
            context_paths: context.loaded_agents_paths,
            skills: context.loaded_skills,
        })
    })
    .await
    .unwrap_or_else(|e| {
        UiEvent::Thread(ThreadUiEvent::CreateFailed {
            error: format!("Task failed: {}", e),
        })
    })
}

/// Forks a thread at a specific point.
///
/// Pure async function - runtime spawns and sends result to inbox.
pub async fn thread_fork(
    events: Vec<ThreadEvent>,
    user_input: Option<String>,
    turn_number: usize,
    root: PathBuf,
) -> UiEvent {
    tokio::task::spawn_blocking(move || fork_thread_sync(events, user_input, turn_number, &root))
        .await
        .unwrap_or_else(|e| {
            UiEvent::Thread(ThreadUiEvent::ForkFailed {
                error: format!("Task failed: {}", e),
            })
        })
}

fn fork_thread_sync(
    events: Vec<ThreadEvent>,
    user_input: Option<String>,
    turn_number: usize,
    root: &Path,
) -> UiEvent {
    let mut thread_log_handle = match thread_log::ThreadLog::new_with_root(root) {
        Ok(thread_log_handle) => thread_log_handle,
        Err(e) => {
            return UiEvent::Thread(ThreadUiEvent::ForkFailed {
                error: format!("Failed to create thread: {}", e),
            });
        }
    };

    for event in &events {
        if let Err(e) = thread_log_handle.append(event) {
            return UiEvent::Thread(ThreadUiEvent::ForkFailed {
                error: format!("Failed to write thread: {}", e),
            });
        }
    }

    let usage = thread_log::extract_usage_from_thread_events(&events);
    let cells = build_transcript_from_events(&events);
    let messages = thread_log::thread_events_to_messages(events);
    let history: Vec<String> = cells
        .iter()
        .filter_map(|cell| {
            if let HistoryCell::User { content, .. } = cell {
                Some(content.clone())
            } else {
                None
            }
        })
        .collect();

    UiEvent::Thread(ThreadUiEvent::ForkedLoaded {
        thread_id: thread_log_handle.id.clone(),
        cells,
        messages,
        history,
        thread_log: thread_log_handle,
        usage,
        user_input,
        turn_number,
    })
}

/// Renames a thread.
///
/// Pure async function - runtime spawns and sends result to inbox.
pub async fn thread_rename(thread_id: String, title: Option<String>) -> UiEvent {
    tokio::task::spawn_blocking(move || {
        match thread_log::set_thread_title(&thread_id, title.clone()) {
            Ok(new_title) => UiEvent::Thread(ThreadUiEvent::Renamed {
                thread_id,
                title: new_title,
            }),
            Err(e) => UiEvent::Thread(ThreadUiEvent::RenameFailed {
                error: format!("Failed to rename thread: {}", e),
            }),
        }
    })
    .await
    .unwrap_or_else(|e| {
        UiEvent::Thread(ThreadUiEvent::RenameFailed {
            error: format!("Task failed: {}", e),
        })
    })
}
