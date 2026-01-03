//! Effect handlers for the TUI runtime.
//!
//! This module contains the implementation of side effects triggered by the reducer.
//! These functions perform I/O, spawn async tasks, and modify state.

use std::fmt::Display;

use tokio::sync::mpsc;

use crate::core::interrupt;
use crate::core::session::{self};
use crate::ui::chat::overlays;
use crate::ui::chat::state::{AgentState, SessionUsage, TuiState};
use crate::ui::chat::transcript_build::build_transcript_from_events;
use crate::ui::transcript::HistoryCell;

// ============================================================================
// Helper Methods
// ============================================================================

/// Helper to push a system message to the transcript.
pub fn push_system(tui: &mut TuiState, msg: impl Into<String>) {
    tui.transcript.cells.push(HistoryCell::system(msg));
}

/// Helper to push a warning message to the transcript.
pub fn push_warning(tui: &mut TuiState, prefix: &str, err: impl Display) {
    tui.transcript
        .cells
        .push(HistoryCell::system(format!("{}: {}", prefix, err)));
}

// ============================================================================
// Session Handlers
// ============================================================================

/// Loads transcript cells from a session.
///
/// Shared helper for `load_session` and `preview_session`.
fn load_transcript_cells(session_id: &str) -> Result<Vec<HistoryCell>, anyhow::Error> {
    let events = session::load_session(session_id)?;
    Ok(build_transcript_from_events(&events))
}

/// Opens the session picker overlay.
///
/// Loads the session list (I/O) and opens the overlay if sessions exist.
/// Shows an error message if no sessions are found or loading fails.
/// Also triggers a preview of the first session.
pub fn open_session_picker(tui: &mut TuiState, overlay: &mut Option<overlays::Overlay>) {
    // Don't open if another overlay is active
    if overlay.is_some() {
        return;
    }

    // Load sessions (I/O happens here in the effect handler, not reducer)
    match session::list_sessions() {
        Ok(sessions) if sessions.is_empty() => {
            push_system(tui, "No sessions found.");
        }
        Ok(sessions) => {
            if overlay.is_none() {
                let original_cells = tui.transcript.cells.clone();
                let (state, effects) = overlays::SessionPickerState::open(sessions, original_cells);
                *overlay = Some(state.into());

                // Execute preview effect immediately (within same I/O context)
                for effect in effects {
                    if let crate::ui::chat::effects::UiEffect::PreviewSession { session_id } =
                        effect
                    {
                        preview_session(tui, &session_id);
                    }
                }
            }
        }
        Err(e) => {
            push_warning(tui, "Failed to load sessions", e);
        }
    }
}

/// Loads a session by ID and switches to it.
///
/// This:
/// 1. Loads events from the session file
/// 2. Builds transcript cells from events
/// 3. Builds API messages for conversation context
/// 4. Resets all state facets with loaded data
pub fn load_session(tui: &mut TuiState, session_id: &str) {
    // Load session events (I/O)
    let events = match session::load_session(session_id) {
        Ok(events) => events,
        Err(e) => {
            push_warning(tui, "Failed to load session", e);
            return;
        }
    };

    // Build transcript cells from events
    let transcript_cells = build_transcript_from_events(&events);

    // Build API messages for conversation context
    let messages = session::events_to_messages(events);

    // Build input history from user messages in transcript
    let command_history: Vec<String> = transcript_cells
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
    let session_handle = match session::Session::with_id(session_id.to_string()) {
        Ok(s) => Some(s),
        Err(e) => {
            push_warning(tui, "Warning: Failed to open session for writing", e);
            None
        }
    };

    // Reset state facets with loaded data
    tui.transcript.cells = transcript_cells;
    tui.conversation.messages = messages;
    tui.conversation.session = session_handle;
    tui.conversation.usage = SessionUsage::new();
    tui.input.history = command_history;
    tui.transcript.scroll.reset();
    tui.transcript.wrap_cache.clear();

    // Show confirmation message
    let short_id = if session_id.len() > 8 {
        format!("{}…", &session_id[..8])
    } else {
        session_id.to_string()
    };
    push_system(tui, format!("Switched to session {}", short_id));
}

/// Previews a session (shows transcript without full switch).
///
/// Used during session picker navigation to show a live preview.
/// Only updates transcript cells and display state, not conversation
/// messages or session handle. The original cells are stored in the
/// picker state for restore on Esc.
pub fn preview_session(tui: &mut TuiState, session_id: &str) {
    // Load transcript cells, silently fail for preview - errors shown on actual load
    if let Ok(transcript_cells) = load_transcript_cells(session_id) {
        tui.transcript.cells = transcript_cells;
        tui.transcript.scroll.reset();
        tui.transcript.wrap_cache.clear();
    }
}

/// Creates a new session and shows context info in transcript.
///
/// Returns `Ok(())` if session was created successfully, `Err(())` if it failed.
/// On failure, an error message is already added to the transcript.
pub fn create_session_and_show_context(tui: &mut TuiState) -> Result<(), ()> {
    let new_session = match session::Session::new() {
        Ok(s) => s,
        Err(e) => {
            push_warning(tui, "Warning: Failed to create new session", e);
            return Err(());
        }
    };

    let new_path = new_session.path().display().to_string();
    tui.conversation.session = Some(new_session);

    // Show session path
    push_system(tui, format!("Session path: {}", new_path));

    // Show loaded AGENTS.md files
    let effective = match crate::core::context::build_effective_system_prompt_with_paths(
        &tui.config,
        &tui.agent_opts.root,
    ) {
        Ok(e) => e,
        Err(err) => {
            push_warning(tui, "Warning: Failed to load context", err);
            return Ok(()); // Session created, just context loading failed
        }
    };

    if !effective.loaded_agents_paths.is_empty() {
        let paths_list: Vec<String> = effective
            .loaded_agents_paths
            .iter()
            .map(|p| format!("  - {}", p.display()))
            .collect();
        let message = format!("Loaded AGENTS.md from:\n{}", paths_list.join("\n"));
        push_system(tui, message);
    }

    Ok(())
}

// ============================================================================
// Agent Handlers
// ============================================================================

/// Interrupts the running agent.
pub fn interrupt_agent(tui: &mut TuiState) {
    if tui.agent_state.is_running() {
        interrupt::trigger_ctrl_c();
    }
}

/// Spawns an agent turn.
pub fn spawn_agent_turn(tui: &mut TuiState) {
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

    tui.agent_state = AgentState::Waiting { rx: tui_rx };
}

// ============================================================================
// Auth Handlers
// ============================================================================

/// Spawns a token exchange task.
pub fn spawn_token_exchange(tui: &mut TuiState, code: &str, verifier: &str) {
    use crate::providers::oauth::anthropic;

    let code = code.to_string();
    let pkce_verifier = verifier.to_string();

    let (tx, rx) = mpsc::channel::<Result<(), String>>(1);
    tui.auth.login_rx = Some(rx);

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
}

// ============================================================================
// File Picker Handlers
// ============================================================================

/// Spawns async file discovery and returns the receiver.
///
/// The runtime owns the receiver (not state) for cleaner Elm-like separation.
pub fn spawn_file_discovery(root: &std::path::Path) -> mpsc::Receiver<Vec<std::path::PathBuf>> {
    use crate::ui::chat::overlays::discover_files;

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
