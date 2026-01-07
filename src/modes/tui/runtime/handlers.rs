//! Effect handlers for the TUI runtime.
//!
//! This module contains the implementation of side effects triggered by the reducer.
//! These functions perform I/O and spawn async tasks. They do NOT mutate state directly.
//!
//! State mutations happen in the reducer via events. Effect handlers either:
//! 1. Perform synchronous I/O and return results for the runtime to convert to events
//! 2. Spawn async tasks that send results via channels (runtime collects and converts to events)

use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use tokio::sync::{mpsc, oneshot};

use crate::core::thread_log::ThreadEvent;
use crate::core::{interrupt, thread_log};
use crate::modes::tui::app::TuiState;
use crate::modes::tui::events::{ThreadUiEvent, UiEvent};
use crate::modes::tui::transcript::{HistoryCell, build_transcript_from_events};

// ============================================================================
// Thread Handlers (Async - return receivers for runtime to poll)
// ============================================================================

/// Spawns async thread list loading and returns the receiver.
///
/// Returns events via channel for the runtime to collect and dispatch to reducer.
pub fn spawn_thread_list_load(original_cells: Vec<HistoryCell>) -> UiEvent {
    let (tx, rx) = mpsc::channel::<UiEvent>(1);

    tokio::spawn(async move {
        let event = tokio::task::spawn_blocking(move || match thread_log::list_threads() {
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
        });

        let _ = tx.send(event).await;
    });

    UiEvent::Thread(ThreadUiEvent::ListStarted { rx })
}

/// Spawns async thread loading (full switch) and returns the receiver.
///
/// Loads events, builds transcript cells, messages, and history.
pub fn spawn_thread_load(thread_id: String, root: PathBuf) -> UiEvent {
    let (tx, rx) = mpsc::channel::<UiEvent>(1);

    tokio::spawn(async move {
        let id = thread_id.clone();
        let root = root.clone();
        let event = tokio::task::spawn_blocking(move || load_thread_sync(&id, &root))
            .await
            .unwrap_or_else(|e| {
                UiEvent::Thread(ThreadUiEvent::LoadFailed {
                    error: format!("Task failed: {}", e),
                })
            });

        let _ = tx.send(event).await;
    });

    UiEvent::Thread(ThreadUiEvent::LoadStarted { rx })
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

/// Spawns async thread preview loading and returns the receiver.
///
/// Preview only loads transcript cells (not full thread data).
pub fn spawn_thread_preview(thread_id: String) -> UiEvent {
    let (tx, rx) = mpsc::channel::<UiEvent>(1);

    tokio::spawn(async move {
        let event = tokio::task::spawn_blocking(move || {
            match thread_log::load_thread_events(&thread_id) {
                Ok(events) => {
                    let cells = build_transcript_from_events(&events);
                    UiEvent::Thread(ThreadUiEvent::PreviewLoaded { cells })
                }
                Err(_) => {
                    // Silent failure for preview - errors shown on actual load
                    UiEvent::Thread(ThreadUiEvent::PreviewFailed)
                }
            }
        })
        .await
        .unwrap_or(UiEvent::Thread(ThreadUiEvent::PreviewFailed));

        let _ = tx.send(event).await;
    });

    UiEvent::Thread(ThreadUiEvent::PreviewStarted { rx })
}

/// Spawns async new thread creation and returns the receiver.
pub fn spawn_thread_create(config: crate::config::Config, root: PathBuf) -> UiEvent {
    let (tx, rx) = mpsc::channel::<UiEvent>(1);

    tokio::spawn(async move {
        let event =
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
                    match crate::core::context::build_effective_system_prompt_with_paths(
                        &config, &root,
                    ) {
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
            });

        let _ = tx.send(event).await;
    });

    UiEvent::Thread(ThreadUiEvent::CreateStarted { rx })
}

/// Spawns async forked thread creation and returns the receiver.
pub fn spawn_forked_thread(
    events: Vec<ThreadEvent>,
    user_input: Option<String>,
    turn_number: usize,
    root: PathBuf,
) -> UiEvent {
    let (tx, rx) = mpsc::channel::<UiEvent>(1);

    tokio::spawn(async move {
        let event = tokio::task::spawn_blocking(move || {
            fork_thread_sync(events, user_input, turn_number, &root)
        })
        .await
        .unwrap_or_else(|e| {
            UiEvent::Thread(ThreadUiEvent::ForkFailed {
                error: format!("Task failed: {}", e),
            })
        });

        let _ = tx.send(event).await;
    });

    UiEvent::Thread(ThreadUiEvent::ForkStarted { rx })
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

/// Spawns async thread rename and returns the receiver.
pub fn spawn_thread_rename(thread_id: String, title: Option<String>) -> UiEvent {
    let (tx, rx) = mpsc::channel::<UiEvent>(1);

    tokio::spawn(async move {
        let id = thread_id.clone();
        let event = tokio::task::spawn_blocking(move || {
            match thread_log::set_thread_title(&id, title.clone()) {
                Ok(new_title) => UiEvent::Thread(ThreadUiEvent::Renamed {
                    thread_id: id,
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
        });

        let _ = tx.send(event).await;
    });

    UiEvent::Thread(ThreadUiEvent::RenameStarted { rx })
}

// ============================================================================
// Agent Handlers
// ============================================================================

/// Interrupts the running agent.
pub fn interrupt_agent(tui: &TuiState) {
    if tui.agent_state.is_running() {
        interrupt::trigger_ctrl_c();
    }
}

/// Spawns an agent turn.
pub fn spawn_agent_turn(tui: &TuiState) -> UiEvent {
    let (agent_tx, agent_rx) = crate::core::agent::create_event_channel();

    let messages = tui.thread.messages.clone();
    let config = tui.config.clone();
    let agent_opts = tui.agent_opts.clone();
    let system_prompt = tui.system_prompt.clone();

    let (tui_tx, tui_rx) = crate::core::agent::create_event_channel();

    if let Some(thread_log_handle) = tui.thread.thread_log.clone() {
        let (persist_tx, persist_rx) = crate::core::agent::create_event_channel();
        let _broadcaster =
            crate::core::agent::spawn_broadcaster(agent_rx, vec![tui_tx, persist_tx]);
        let _persist = thread_log::spawn_thread_persist_task(thread_log_handle, persist_rx);
    } else {
        let _broadcaster = crate::core::agent::spawn_broadcaster(agent_rx, vec![tui_tx]);
    }

    // Spawn the agent task - it will send TurnComplete when done
    tokio::spawn(async move {
        let _ = crate::core::agent::run_turn(
            messages,
            &config,
            &agent_opts,
            system_prompt.as_deref(),
            agent_tx,
        )
        .await;
    });

    UiEvent::AgentSpawned { rx: tui_rx }
}

// ============================================================================
// Auth Handlers
// ============================================================================

/// Spawns a token exchange task.
pub fn spawn_token_exchange(
    provider: crate::providers::ProviderKind,
    code: &str,
    verifier: &str,
) -> UiEvent {
    use crate::providers::oauth::{anthropic, openai_codex};

    let code = code.to_string();
    let pkce_verifier = verifier.to_string();

    let (tx, rx) = mpsc::channel::<Result<(), String>>(1);

    tokio::spawn(async move {
        let result = match provider {
            crate::providers::ProviderKind::Anthropic => {
                let pkce = anthropic::Pkce {
                    verifier: pkce_verifier,
                    challenge: String::new(),
                };
                match anthropic::exchange_code(&code, &pkce).await {
                    Ok(creds) => anthropic::save_credentials(&creds)
                        .map_err(|e| format!("Failed to save: {}", e)),
                    Err(e) => Err(e.to_string()),
                }
            }
            crate::providers::ProviderKind::OpenAICodex => {
                let pkce = openai_codex::Pkce {
                    verifier: pkce_verifier,
                    challenge: String::new(),
                };
                match openai_codex::exchange_code(&code, &pkce).await {
                    Ok(creds) => openai_codex::save_credentials(&creds)
                        .map_err(|e| format!("Failed to save: {}", e)),
                    Err(e) => Err(e.to_string()),
                }
            }
        };
        let _ = tx.send(result).await;
    });

    UiEvent::LoginExchangeStarted { rx }
}

/// Spawns a local OAuth callback listener (if supported).
pub fn spawn_local_auth_callback(
    provider: crate::providers::ProviderKind,
    state: Option<String>,
) -> UiEvent {
    let (tx, rx) = mpsc::channel::<Option<String>>(1);

    tokio::spawn(async move {
        let code = match provider {
            crate::providers::ProviderKind::OpenAICodex => wait_for_local_code(state.as_deref()),
            crate::providers::ProviderKind::Anthropic => None,
        };
        let _ = tx.send(code).await;
    });

    UiEvent::LoginCallbackStarted { rx }
}

fn wait_for_local_code(expected_state: Option<&str>) -> Option<String> {
    let listener = match TcpListener::bind("127.0.0.1:1455") {
        Ok(listener) => listener,
        Err(_) => return None,
    };
    let _ = listener.set_nonblocking(true);

    let (tx, rx) = std::sync::mpsc::channel::<Option<String>>();
    let expected_state = expected_state.map(|s| s.to_string());

    std::thread::spawn(move || {
        let start = std::time::Instant::now();
        loop {
            match listener.accept() {
                Ok((mut stream, _)) => {
                    let mut buffer = [0u8; 2048];
                    let _ = stream.read(&mut buffer);
                    let request = String::from_utf8_lossy(&buffer);
                    let code = extract_code_from_request(&request, expected_state.as_deref());
                    let response = match code.is_some() {
                        true => oauth_success_response(),
                        false => oauth_error_response(),
                    };
                    let _ = stream.write_all(response.as_bytes());
                    let _ = tx.send(code);
                    break;
                }
                Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                    if start.elapsed() > Duration::from_secs(60) {
                        let _ = tx.send(None);
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(100));
                }
                Err(_) => {
                    let _ = tx.send(None);
                    break;
                }
            }
        }
    });

    rx.recv_timeout(Duration::from_secs(60)).ok().flatten()
}

fn extract_code_from_request(request: &str, expected_state: Option<&str>) -> Option<String> {
    let mut lines = request.lines();
    let request_line = lines.next()?;
    let mut parts = request_line.split_whitespace();
    let _method = parts.next()?;
    let path = parts.next()?;

    let url = url::Url::parse(&format!("http://localhost{}", path)).ok()?;
    if url.path() != "/auth/callback" {
        return None;
    }
    if let Some(expected) = expected_state {
        let state = url
            .query_pairs()
            .find(|(k, _)| k == "state")
            .map(|(_, v)| v.to_string())?;
        if state != expected {
            return None;
        }
    }
    url.query_pairs()
        .find(|(k, _)| k == "code")
        .map(|(_, v)| v.to_string())
}

fn oauth_success_response() -> String {
    let body = "<html><body><h3>Login complete</h3><p>You can close this window.</p></body></html>";
    format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\n\r\n{}",
        body.len(),
        body
    )
}

fn oauth_error_response() -> String {
    let body = "<html><body><h3>Login failed</h3><p>Please return to the terminal and paste the code.</p></body></html>";
    format!(
        "HTTP/1.1 400 Bad Request\r\nContent-Type: text/html\r\nContent-Length: {}\r\n\r\n{}",
        body.len(),
        body
    )
}

// ============================================================================
// File Picker Handlers
// ============================================================================

/// Spawns async file discovery with cancellation support.
pub fn spawn_file_discovery(root: &std::path::Path) -> UiEvent {
    use crate::modes::tui::overlays::discover_files;

    let root = root.to_path_buf();

    let (tx, rx) = oneshot::channel::<Vec<std::path::PathBuf>>();
    let cancel = Arc::new(AtomicBool::new(false));
    let cancel_clone = cancel.clone();

    tokio::spawn(async move {
        let files = tokio::task::spawn_blocking(move || discover_files(&root, &cancel_clone))
            .await
            .unwrap_or_default();
        let _ = tx.send(files);
    });

    UiEvent::FileDiscoveryStarted { rx, cancel }
}

// ============================================================================
// Direct Bash Execution Handlers
// ============================================================================

/// Spawns async direct bash command execution (for `!` shortcut).
pub fn spawn_bash_execution(id: String, command: String, root: PathBuf) -> UiEvent {
    use crate::core::events::ToolOutput;
    use crate::tools::{ToolContext, bash};

    let (tx, rx) = oneshot::channel::<ToolOutput>();
    let (cancel_tx, mut cancel_rx) = oneshot::channel::<()>();
    let cmd = command.clone();

    tokio::spawn(async move {
        let ctx = ToolContext::with_timeout(root, None);
        let run_fut = bash::run(&cmd, &ctx, None);
        tokio::select! {
            result = run_fut => {
                let _ = tx.send(result);
            }
            _ = &mut cancel_rx => {
                let _ = tx.send(ToolOutput::canceled("Interrupted by user"));
            }
        }
    });

    UiEvent::BashExecutionStarted {
        id,
        command,
        rx,
        cancel: cancel_tx,
    }
}
