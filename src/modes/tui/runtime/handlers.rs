//! Effect handlers for the TUI runtime.
//!
//! This module contains the implementation of side effects triggered by the reducer.
//! These functions perform I/O and async tasks. They do NOT mutate state directly.
//!
//! ## Pure Async Pattern
//!
//! Handlers are pure async functions that return `UiEvent`. The runtime uses
//! `spawn_effect` to spawn them and send results to the inbox. This keeps
//! handlers focused on business logic while the runtime handles spawning.
//!
//! ```ignore
//! // Handler: pure async, returns UiEvent
//! pub async fn thread_list_load(cells: Vec<HistoryCell>) -> UiEvent { ... }
//!
//! // Runtime: spawns and sends to inbox
//! self.spawn_effect(Some(started_event), || handler(args));
//! ```

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
use crate::modes::tui::shared::RequestId;
use crate::modes::tui::transcript::{HistoryCell, build_transcript_from_events};

// ============================================================================
// Inbox Types (used by runtime)
// ============================================================================

/// Sender for the runtime's event inbox.
pub type UiEventSender = mpsc::UnboundedSender<UiEvent>;

/// Receiver for the runtime's event inbox.
pub type UiEventReceiver = mpsc::UnboundedReceiver<UiEvent>;

// ============================================================================
// Thread Handlers (Pure Async)
// ============================================================================

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
pub async fn thread_create(config: crate::config::Config, root: PathBuf) -> UiEvent {
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
            match crate::core::context::build_effective_system_prompt_with_paths(&config, &root) {
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
// Auth Handlers (Pure Async)
// ============================================================================

/// Exchanges an OAuth code for credentials.
///
/// Pure async function - runtime spawns and sends result to inbox.
pub async fn token_exchange(
    provider: crate::providers::ProviderKind,
    code: String,
    verifier: String,
    redirect_uri: Option<String>,
    req: RequestId,
) -> UiEvent {
    use crate::providers::oauth::{anthropic, gemini_cli, openai_codex};

    let result = match provider {
        crate::providers::ProviderKind::Anthropic => {
            let pkce = anthropic::Pkce {
                verifier,
                challenge: String::new(),
            };
            let redirect_uri = match redirect_uri {
                Some(value) => value,
                None => {
                    return UiEvent::LoginResult {
                        req,
                        result: Err("Missing redirect URI for Anthropic OAuth.".to_string()),
                    };
                }
            };
            match anthropic::exchange_code(&code, &pkce, &redirect_uri).await {
                Ok(creds) => anthropic::save_credentials(&creds)
                    .map_err(|e| format!("Failed to save: {}", e)),
                Err(e) => Err(e.to_string()),
            }
        }
        crate::providers::ProviderKind::OpenAICodex => {
            let pkce = openai_codex::Pkce {
                verifier,
                challenge: String::new(),
            };
            match openai_codex::exchange_code(&code, &pkce).await {
                Ok(creds) => openai_codex::save_credentials(&creds)
                    .map_err(|e| format!("Failed to save: {}", e)),
                Err(e) => Err(e.to_string()),
            }
        }
        crate::providers::ProviderKind::GeminiCli => {
            let pkce = gemini_cli::Pkce {
                verifier,
                challenge: String::new(),
            };
            match gemini_cli::exchange_code(&code, &pkce).await {
                Ok(mut creds) => {
                    // Discover project ID after getting tokens
                    match gemini_cli::discover_project(&creds.access).await {
                        Ok(project_id) => {
                            creds.account_id = Some(project_id);
                            gemini_cli::save_credentials(&creds)
                                .map_err(|e| format!("Failed to save: {}", e))
                        }
                        Err(e) => Err(format!("Failed to discover project: {}", e)),
                    }
                }
                Err(e) => Err(e.to_string()),
            }
        }
        _ => Err("OAuth is not supported for this provider.".to_string()),
    };
    UiEvent::LoginResult { req, result }
}

/// Listens for a local OAuth callback.
///
/// Pure async function - runtime spawns and sends result to inbox.
pub async fn local_auth_callback(
    provider: crate::providers::ProviderKind,
    state: Option<String>,
    port: Option<u16>,
) -> UiEvent {
    let code = match provider {
        crate::providers::ProviderKind::Anthropic => {
            use crate::providers::oauth::anthropic;
            port.and_then(|port| {
                wait_for_local_code(port, anthropic::LOCAL_CALLBACK_PATH, state.as_deref())
            })
        }
        crate::providers::ProviderKind::OpenAICodex => {
            wait_for_local_code(1455, "/auth/callback", state.as_deref())
        }
        crate::providers::ProviderKind::GeminiCli => {
            wait_for_local_code(8085, "/oauth2callback", state.as_deref())
        }
        _ => None,
    };
    UiEvent::LoginCallbackResult(code)
}

fn wait_for_local_code(
    port: u16,
    callback_path: &str,
    expected_state: Option<&str>,
) -> Option<String> {
    let listener = match TcpListener::bind(format!("127.0.0.1:{}", port)) {
        Ok(listener) => listener,
        Err(_) => return None,
    };
    let _ = listener.set_nonblocking(true);

    let (tx, rx) = std::sync::mpsc::channel::<Option<String>>();
    let expected_state = expected_state.map(|s| s.to_string());
    let callback_path = callback_path.to_string();

    std::thread::spawn(move || {
        let start = std::time::Instant::now();
        loop {
            match listener.accept() {
                Ok((mut stream, _)) => {
                    let mut buffer = [0u8; 2048];
                    let _ = stream.read(&mut buffer);
                    let request = String::from_utf8_lossy(&buffer);
                    let code = extract_code_from_request(
                        &request,
                        &callback_path,
                        expected_state.as_deref(),
                    );
                    let response = match code.is_some() {
                        true => oauth_success_response(),
                        false => oauth_error_response(),
                    };
                    let _ = stream.write_all(response.as_bytes());
                    let _ = tx.send(code);
                    break;
                }
                Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                    if start.elapsed() > Duration::from_secs(120) {
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

    rx.recv_timeout(Duration::from_secs(120)).ok().flatten()
}

fn extract_code_from_request(
    request: &str,
    callback_path: &str,
    expected_state: Option<&str>,
) -> Option<String> {
    let mut lines = request.lines();
    let request_line = lines.next()?;
    let mut parts = request_line.split_whitespace();
    let _method = parts.next()?;
    let path = parts.next()?;

    let url = url::Url::parse(&format!("http://localhost{}", path)).ok()?;
    if url.path() != callback_path {
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

/// File discovery with cancellation support.
///
/// Returns (started_event, future) - started event contains cancel token,
/// future does the discovery work.
pub fn file_discovery(
    root: PathBuf,
) -> (
    UiEvent,
    impl std::future::Future<Output = UiEvent> + Send + 'static,
) {
    use crate::modes::tui::overlays::discover_files;

    let cancel = Arc::new(AtomicBool::new(false));
    let cancel_clone = cancel.clone();
    let started = UiEvent::FileDiscoveryStarted { cancel };

    let future = async move {
        let files = tokio::task::spawn_blocking(move || discover_files(&root, &cancel_clone))
            .await
            .unwrap_or_default();
        UiEvent::FilesDiscovered(files)
    };

    (started, future)
}

// ============================================================================
// Direct Bash Execution Handlers
// ============================================================================

/// Bash execution with cancellation support.
///
/// Returns (started_event, future) - started event contains cancel token,
/// future runs the command.
pub fn bash_execution(
    id: String,
    command: String,
    root: PathBuf,
) -> (
    UiEvent,
    impl std::future::Future<Output = UiEvent> + Send + 'static,
) {
    use crate::core::events::ToolOutput;
    use crate::tools::{ToolContext, bash};

    let (cancel_tx, mut cancel_rx) = oneshot::channel::<()>();
    let cmd = command.clone();
    let result_id = id.clone();

    let started = UiEvent::BashExecutionStarted {
        id,
        command,
        cancel: cancel_tx,
    };

    let future = async move {
        let ctx = ToolContext::with_timeout(root, None);
        let run_fut = bash::run(&cmd, &ctx, None);
        let result = tokio::select! {
            result = run_fut => result,
            _ = &mut cancel_rx => ToolOutput::canceled("Interrupted by user"),
        };
        UiEvent::BashExecuted {
            id: result_id,
            result,
        }
    };

    (started, future)
}
