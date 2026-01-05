//! Effect handlers for the TUI runtime.
//!
//! This module contains the implementation of side effects triggered by the reducer.
//! These functions perform I/O and spawn async tasks. They do NOT mutate state directly.
//!
//! State mutations happen in the reducer via events. Effect handlers either:
//! 1. Perform synchronous I/O and return results for the runtime to convert to events
//! 2. Spawn async tasks that send results via channels (runtime collects and converts to events)

use std::path::PathBuf;

use tokio::sync::mpsc;

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
pub fn spawn_session_list_load(original_cells: Vec<HistoryCell>) -> mpsc::Receiver<UiEvent> {
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

    rx
}

/// Spawns async session loading (full switch) and returns the receiver.
///
/// Loads events, builds transcript cells, messages, and history.
pub fn spawn_session_load(session_id: String) -> mpsc::Receiver<UiEvent> {
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

    rx
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
pub fn spawn_session_preview(session_id: String) -> mpsc::Receiver<UiEvent> {
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

    rx
}

/// Spawns async new session creation and returns the receiver.
pub fn spawn_session_create(
    config: crate::config::Config,
    root: PathBuf,
) -> mpsc::Receiver<UiEvent> {
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

    rx
}

/// Spawns async session rename and returns the receiver.
pub fn spawn_session_rename(session_id: String, title: Option<String>) -> mpsc::Receiver<UiEvent> {
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

    rx
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
        let _fanout = crate::core::agent::spawn_fanout_task(agent_rx, vec![tui_tx, persist_tx]);
        let _persist = session::spawn_persist_task(sess, persist_rx);
    } else {
        let _fanout = crate::core::agent::spawn_fanout_task(agent_rx, vec![tui_tx]);
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
pub fn spawn_token_exchange(code: &str, verifier: &str) -> UiEvent {
    use crate::providers::oauth::anthropic;

    let code = code.to_string();
    let pkce_verifier = verifier.to_string();

    let (tx, rx) = mpsc::channel::<Result<(), String>>(1);

    tokio::spawn(async move {
        let pkce = anthropic::Pkce {
            verifier: pkce_verifier,
            challenge: String::new(),
        };
        let result = match anthropic::exchange_code(&code, &pkce).await {
            Ok(creds) => {
                anthropic::save_credentials(&creds).map_err(|e| format!("Failed to save: {}", e))
            }
            Err(e) => Err(e.to_string()),
        };
        let _ = tx.send(result).await;
    });

    UiEvent::LoginExchangeStarted { rx }
}

// ============================================================================
// File Picker Handlers
// ============================================================================

/// Spawns async file discovery and returns the receiver.
///
/// The runtime owns the receiver (not state) for cleaner Elm-like separation.
pub fn spawn_file_discovery(root: &std::path::Path) -> mpsc::Receiver<Vec<std::path::PathBuf>> {
    use crate::modes::tui::overlays::discover_files;

    let root = root.to_path_buf();

    let (tx, rx) = mpsc::channel::<Vec<std::path::PathBuf>>(1);

    // Use spawn_blocking since file walking is CPU/IO bound
    tokio::spawn(async move {
        let files = tokio::task::spawn_blocking(move || discover_files(&root))
            .await
            .unwrap_or_default();
        let _ = tx.send(files).await;
    });

    rx
}
