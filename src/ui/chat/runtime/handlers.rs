//! Effect handlers for the TUI runtime.
//!
//! This module contains the implementation of side effects triggered by the reducer.
//! These functions perform I/O, spawn async tasks, and modify state.

use std::fmt::Display;

use tokio::sync::mpsc;

use crate::core::interrupt;
use crate::core::session::{self};
use crate::ui::chat::overlays::SessionPickerState;
use crate::ui::chat::state::{AgentState, OverlayState, SessionUsage, TuiState};
use crate::ui::chat::transcript_build::build_transcript_from_events;
use crate::ui::transcript::HistoryCell;

// ============================================================================
// Helper Methods
// ============================================================================

/// Helper to push a system message to the transcript.
pub fn push_system(state: &mut TuiState, msg: impl Into<String>) {
    state.transcript.cells.push(HistoryCell::system(msg));
}

/// Helper to push a warning message to the transcript.
pub fn push_warning(state: &mut TuiState, prefix: &str, err: impl Display) {
    state
        .transcript
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
pub fn open_session_picker(state: &mut TuiState) {
    // Don't open if another overlay is active
    if !matches!(state.overlay, OverlayState::None) {
        return;
    }

    // Load sessions (I/O happens here in the effect handler, not reducer)
    match session::list_sessions() {
        Ok(sessions) if sessions.is_empty() => {
            push_system(state, "No sessions found.");
        }
        Ok(sessions) => {
            // Snapshot current transcript cells for restore on Esc
            let original_cells = state.transcript.cells.clone();

            // Get the first session ID for initial preview
            let first_session_id = sessions.first().map(|s| s.id.clone());

            state.overlay =
                OverlayState::SessionPicker(SessionPickerState::new(sessions, original_cells));

            // Trigger initial preview for the first session
            if let Some(session_id) = first_session_id {
                preview_session(state, &session_id);
            }
        }
        Err(e) => {
            push_warning(state, "Failed to load sessions", e);
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
pub fn load_session(state: &mut TuiState, session_id: &str) {
    // Load session events (I/O)
    let events = match session::load_session(session_id) {
        Ok(events) => events,
        Err(e) => {
            push_warning(state, "Failed to load session", e);
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
            push_warning(state, "Warning: Failed to open session for writing", e);
            None
        }
    };

    // Reset state facets with loaded data
    state.transcript.cells = transcript_cells;
    state.conversation.messages = messages;
    state.conversation.session = session_handle;
    state.conversation.usage = SessionUsage::new();
    state.input.history = command_history;
    state.transcript.scroll.reset();
    state.transcript.wrap_cache.clear();

    // Show confirmation message
    let short_id = if session_id.len() > 8 {
        format!("{}…", &session_id[..8])
    } else {
        session_id.to_string()
    };
    push_system(state, format!("Switched to session {}", short_id));
}

/// Previews a session (shows transcript without full switch).
///
/// Used during session picker navigation to show a live preview.
/// Only updates transcript cells and display state, not conversation
/// messages or session handle. The original cells are stored in the
/// picker state for restore on Esc.
pub fn preview_session(state: &mut TuiState, session_id: &str) {
    // Only preview if session picker is open
    if !matches!(state.overlay, OverlayState::SessionPicker(_)) {
        return;
    }

    // Load transcript cells, silently fail for preview - errors shown on actual load
    if let Ok(transcript_cells) = load_transcript_cells(session_id) {
        state.transcript.cells = transcript_cells;
        state.transcript.scroll.reset();
        state.transcript.wrap_cache.clear();
    }
}

/// Creates a new session and shows context info in transcript.
///
/// Returns `Ok(())` if session was created successfully, `Err(())` if it failed.
/// On failure, an error message is already added to the transcript.
pub fn create_session_and_show_context(state: &mut TuiState) -> Result<(), ()> {
    let new_session = match session::Session::new() {
        Ok(s) => s,
        Err(e) => {
            push_warning(state, "Warning: Failed to create new session", e);
            return Err(());
        }
    };

    let new_path = new_session.path().display().to_string();
    state.conversation.session = Some(new_session);

    // Show session path
    push_system(state, format!("Session path: {}", new_path));

    // Show loaded AGENTS.md files
    let effective = match crate::core::context::build_effective_system_prompt_with_paths(
        &state.config,
        &state.agent_opts.root,
    ) {
        Ok(e) => e,
        Err(err) => {
            push_warning(state, "Warning: Failed to load context", err);
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
        push_system(state, message);
    }

    Ok(())
}

// ============================================================================
// Agent Handlers
// ============================================================================

/// Interrupts the running agent.
pub fn interrupt_agent(state: &mut TuiState) {
    if state.agent_state.is_running() {
        interrupt::trigger_ctrl_c();
    }
}

/// Spawns an agent turn.
pub fn spawn_agent_turn(state: &mut TuiState) {
    let (agent_tx, agent_rx) = crate::core::agent::create_event_channel();

    let messages = state.conversation.messages.clone();
    let config = state.config.clone();
    let agent_opts = state.agent_opts.clone();
    let system_prompt = state.system_prompt.clone();

    let (tui_tx, tui_rx) = crate::core::agent::create_event_channel();

    if let Some(sess) = state.conversation.session.clone() {
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

    state.agent_state = AgentState::Waiting { rx: tui_rx };
}

// ============================================================================
// Auth Handlers
// ============================================================================

/// Spawns a token exchange task.
pub fn spawn_token_exchange(state: &mut TuiState, code: &str, verifier: &str) {
    use crate::providers::oauth::anthropic;

    let code = code.to_string();
    let pkce_verifier = verifier.to_string();

    let (tx, rx) = mpsc::channel::<Result<(), String>>(1);
    state.auth.login_rx = Some(rx);

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
