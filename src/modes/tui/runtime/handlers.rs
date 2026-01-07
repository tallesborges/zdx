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
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use tokio::sync::{mpsc, oneshot};

use crate::core::session::SessionEvent;
use crate::core::{interrupt, session};
use crate::modes::tui::app::TuiState;
use crate::modes::tui::events::{SessionUiEvent, UiEvent};
use crate::modes::tui::transcript::{HistoryCell, build_transcript_from_events};

// ============================================================================
// Session Handlers (Async - return receivers for runtime to poll)
// ============================================================================

/// Spawns async session list loading and returns the receiver.
///
/// Returns events via channel for the runtime to collect and dispatch to reducer.
pub fn spawn_session_list_load(original_cells: Vec<HistoryCell>) -> UiEvent {
    let (tx, rx) = mpsc::channel::<UiEvent>(1);

    tokio::spawn(async move {
        let event = tokio::task::spawn_blocking(move || match session::list_sessions() {
            Ok(sessions) if sessions.is_empty() => UiEvent::Session(SessionUiEvent::ListFailed {
                error: "No sessions found.".to_string(),
            }),
            Ok(sessions) => UiEvent::Session(SessionUiEvent::ListLoaded {
                sessions,
                original_cells,
            }),
            Err(e) => UiEvent::Session(SessionUiEvent::ListFailed {
                error: format!("Failed to load sessions: {}", e),
            }),
        })
        .await
        .unwrap_or_else(|e| {
            UiEvent::Session(SessionUiEvent::ListFailed {
                error: format!("Task failed: {}", e),
            })
        });

        let _ = tx.send(event).await;
    });

    UiEvent::Session(SessionUiEvent::ListStarted { rx })
}

/// Spawns async session loading (full switch) and returns the receiver.
///
/// Loads events, builds transcript cells, messages, and history.
pub fn spawn_session_load(session_id: String) -> UiEvent {
    let (tx, rx) = mpsc::channel::<UiEvent>(1);

    tokio::spawn(async move {
        let id = session_id.clone();
        let event = tokio::task::spawn_blocking(move || load_session_sync(&id))
            .await
            .unwrap_or_else(|e| {
                UiEvent::Session(SessionUiEvent::LoadFailed {
                    error: format!("Task failed: {}", e),
                })
            });

        let _ = tx.send(event).await;
    });

    UiEvent::Session(SessionUiEvent::LoadStarted { rx })
}

/// Synchronous session loading (runs in blocking task).
fn load_session_sync(session_id: &str) -> UiEvent {
    // Load session events (I/O)
    let events = match session::load_session(session_id) {
        Ok(events) => events,
        Err(e) => {
            return UiEvent::Session(SessionUiEvent::LoadFailed {
                error: format!("Failed to load session: {}", e),
            });
        }
    };

    // Extract usage from events before consuming them
    let usage = session::extract_usage_from_events(&events);

    // Build transcript cells from events
    let cells = build_transcript_from_events(&events);

    // Build API messages for conversation context
    let messages = session::events_to_messages(events);

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

    // Create or get the session handle for future appends
    let session = session::Session::with_id(session_id.to_string()).ok();

    UiEvent::Session(SessionUiEvent::Loaded {
        session_id: session_id.to_string(),
        cells,
        messages,
        history,
        session,
        usage,
    })
}

/// Spawns async session preview loading and returns the receiver.
///
/// Preview only loads transcript cells (not full session data).
pub fn spawn_session_preview(session_id: String) -> UiEvent {
    let (tx, rx) = mpsc::channel::<UiEvent>(1);

    tokio::spawn(async move {
        let event = tokio::task::spawn_blocking(move || {
            match session::load_session(&session_id) {
                Ok(events) => {
                    let cells = build_transcript_from_events(&events);
                    UiEvent::Session(SessionUiEvent::PreviewLoaded { cells })
                }
                Err(_) => {
                    // Silent failure for preview - errors shown on actual load
                    UiEvent::Session(SessionUiEvent::PreviewFailed)
                }
            }
        })
        .await
        .unwrap_or(UiEvent::Session(SessionUiEvent::PreviewFailed));

        let _ = tx.send(event).await;
    });

    UiEvent::Session(SessionUiEvent::PreviewStarted { rx })
}

/// Spawns async new session creation and returns the receiver.
pub fn spawn_session_create(config: crate::config::Config, root: PathBuf) -> UiEvent {
    let (tx, rx) = mpsc::channel::<UiEvent>(1);

    tokio::spawn(async move {
        let event =
            tokio::task::spawn_blocking(move || {
                let session = match session::Session::new() {
                    Ok(s) => s,
                    Err(e) => {
                        return UiEvent::Session(SessionUiEvent::CreateFailed {
                            error: format!("Failed to create session: {}", e),
                        });
                    }
                };

                // Load AGENTS.md paths
                let context_paths =
                    match crate::core::context::build_effective_system_prompt_with_paths(
                        &config, &root,
                    ) {
                        Ok(effective) => effective.loaded_agents_paths,
                        Err(_) => Vec::new(), // Context loading failed, but session was created
                    };

                UiEvent::Session(SessionUiEvent::Created {
                    session,
                    context_paths,
                })
            })
            .await
            .unwrap_or_else(|e| {
                UiEvent::Session(SessionUiEvent::CreateFailed {
                    error: format!("Task failed: {}", e),
                })
            });

        let _ = tx.send(event).await;
    });

    UiEvent::Session(SessionUiEvent::CreateStarted { rx })
}

/// Spawns async forked session creation and returns the receiver.
pub fn spawn_forked_session(
    events: Vec<SessionEvent>,
    user_input: Option<String>,
    turn_number: usize,
) -> UiEvent {
    let (tx, rx) = mpsc::channel::<UiEvent>(1);

    tokio::spawn(async move {
        let event =
            tokio::task::spawn_blocking(move || fork_session_sync(events, user_input, turn_number))
                .await
                .unwrap_or_else(|e| {
                    UiEvent::Session(SessionUiEvent::ForkFailed {
                        error: format!("Task failed: {}", e),
                    })
                });

        let _ = tx.send(event).await;
    });

    UiEvent::Session(SessionUiEvent::ForkStarted { rx })
}

fn fork_session_sync(
    events: Vec<SessionEvent>,
    user_input: Option<String>,
    turn_number: usize,
) -> UiEvent {
    let mut session = match session::Session::new() {
        Ok(session) => session,
        Err(e) => {
            return UiEvent::Session(SessionUiEvent::ForkFailed {
                error: format!("Failed to create session: {}", e),
            });
        }
    };

    for event in &events {
        if let Err(e) = session.append(event) {
            return UiEvent::Session(SessionUiEvent::ForkFailed {
                error: format!("Failed to write session: {}", e),
            });
        }
    }

    let usage = session::extract_usage_from_events(&events);
    let cells = build_transcript_from_events(&events);
    let messages = session::events_to_messages(events);
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

    UiEvent::Session(SessionUiEvent::ForkedLoaded {
        session_id: session.id.clone(),
        cells,
        messages,
        history,
        session,
        usage,
        user_input,
        turn_number,
    })
}

/// Spawns async session rename and returns the receiver.
pub fn spawn_session_rename(session_id: String, title: Option<String>) -> UiEvent {
    let (tx, rx) = mpsc::channel::<UiEvent>(1);

    tokio::spawn(async move {
        let id = session_id.clone();
        let event = tokio::task::spawn_blocking(move || {
            match session::set_session_title(&id, title.clone()) {
                Ok(new_title) => UiEvent::Session(SessionUiEvent::Renamed {
                    session_id: id,
                    title: new_title,
                }),
                Err(e) => UiEvent::Session(SessionUiEvent::RenameFailed {
                    error: format!("Failed to rename session: {}", e),
                }),
            }
        })
        .await
        .unwrap_or_else(|e| {
            UiEvent::Session(SessionUiEvent::RenameFailed {
                error: format!("Task failed: {}", e),
            })
        });

        let _ = tx.send(event).await;
    });

    UiEvent::Session(SessionUiEvent::RenameStarted { rx })
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

    let messages = tui.conversation.messages.clone();
    let config = tui.config.clone();
    let agent_opts = tui.agent_opts.clone();
    let system_prompt = tui.system_prompt.clone();

    let (tui_tx, tui_rx) = crate::core::agent::create_event_channel();

    if let Some(sess) = tui.conversation.session.clone() {
        let (persist_tx, persist_rx) = crate::core::agent::create_event_channel();
        let _broadcaster =
            crate::core::agent::spawn_broadcaster(agent_rx, vec![tui_tx, persist_tx]);
        let _persist = session::spawn_persist_task(sess, persist_rx);
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
