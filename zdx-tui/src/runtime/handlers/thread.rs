use std::path::{Path, PathBuf};

use zdx_core::core::thread_log;
use zdx_core::core::thread_log::ThreadEvent;

use crate::common::RequestId;
use crate::events::{ThreadUiEvent, UiEvent};
use crate::transcript::{HistoryCell, build_transcript_from_events};

/// Loads the list of threads.
///
/// Pure async function - runtime spawns and sends result to inbox.
pub async fn thread_list_load(original_cells: Vec<HistoryCell>) -> UiEvent {
    tokio::task::spawn_blocking(move || match thread_log::list_threads() {
        Ok(threads) if threads.is_empty() => UiEvent::Thread(ThreadUiEvent::ListFailed {
            error: "No threads found.".to_string(),
        }),
        Ok(threads) => UiEvent::Thread(ThreadUiEvent::ListLoaded {
            threads,
            original_cells,
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

    // Auto-relink root path if it differs (best effort).
    if let Some(stored_root) = stored_root {
        let current_root = root
            .canonicalize()
            .unwrap_or_else(|_| root.to_path_buf())
            .display()
            .to_string();
        if stored_root != current_root
            && let Some(mut handle) = thread_log_handle.clone()
        {
            let _ = handle.set_root_path(root);
        }
    } else if let Some(mut handle) = thread_log_handle.clone() {
        let _ = handle.set_root_path(root);
    }

    UiEvent::Thread(ThreadUiEvent::Loaded {
        thread_id: thread_id.to_string(),
        cells,
        messages,
        history,
        thread_log: thread_log_handle,
        usage,
    })
}

/// Loads a thread preview.
///
/// Pure async function - runtime spawns and sends result to inbox.
pub async fn thread_preview(thread_id: String, req: RequestId) -> UiEvent {
    tokio::task::spawn_blocking(move || match thread_log::load_thread_events(&thread_id) {
        Ok(events) => {
            let cells = build_transcript_from_events(&events);
            UiEvent::Thread(ThreadUiEvent::PreviewLoaded { req, cells })
        }
        Err(_) => {
            // Silent failure for preview - errors shown on actual load
            UiEvent::Thread(ThreadUiEvent::PreviewFailed { req })
        }
    })
    .await
    .unwrap_or(UiEvent::Thread(ThreadUiEvent::PreviewFailed { req }))
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

        // Load AGENTS.md paths
        let context_paths =
            match zdx_core::core::context::build_effective_system_prompt_with_paths(&config, &root)
            {
                Ok(effective) => effective.loaded_agents_paths,
                Err(_) => Vec::new(), // Context loading failed, but thread was created
            };

        UiEvent::Thread(ThreadUiEvent::Created {
            thread_log: thread_log_handle,
            context_paths,
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
