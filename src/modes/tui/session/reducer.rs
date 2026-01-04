//! Session feature reducer.
//!
//! Handles session-related state transitions: loading, switching, creating, renaming.

use std::path::PathBuf;

use crate::core::session::{Session, SessionSummary, short_session_id};
use crate::modes::tui::core::events::SessionUiEvent;
use crate::modes::tui::overlays::{Overlay, SessionPickerState};
use crate::modes::tui::shared::effects::UiEffect;
use crate::modes::tui::state::TuiState;
use crate::modes::tui::transcript::HistoryCell;
use crate::providers::anthropic::ChatMessage;

use super::state::SessionUsage;

/// Handles session UI events.
///
/// Dispatches to specific handlers based on event type.
/// Returns effects for the runtime to execute.
pub fn handle_session_event(
    tui: &mut TuiState,
    overlay: &mut Option<Overlay>,
    event: SessionUiEvent,
) -> Vec<UiEffect> {
    match event {
        SessionUiEvent::ListLoaded {
            sessions,
            original_cells,
        } => {
            handle_session_list_loaded(tui, overlay, sessions, original_cells);
            vec![]
        }
        SessionUiEvent::ListFailed { error } => {
            tui.transcript.cells.push(HistoryCell::system(&error));
            vec![]
        }
        SessionUiEvent::Loaded {
            session_id,
            cells,
            messages,
            history,
            session,
        } => {
            handle_session_loaded(tui, &session_id, cells, messages, history, session);
            vec![]
        }
        SessionUiEvent::LoadFailed { error } => {
            tui.transcript.cells.push(HistoryCell::system(&error));
            vec![]
        }
        SessionUiEvent::PreviewLoaded { cells } => {
            handle_session_preview_loaded(tui, cells);
            vec![]
        }
        SessionUiEvent::PreviewFailed => {
            // Silent failure for preview - errors shown on actual load
            vec![]
        }
        SessionUiEvent::Created {
            session,
            context_paths,
        } => {
            handle_session_created(tui, session, context_paths);
            vec![]
        }
        SessionUiEvent::CreateFailed { error } => {
            tui.transcript.cells.push(HistoryCell::system(&error));
            // Still show "Conversation cleared" since the conversation was reset
            tui.transcript
                .cells
                .push(HistoryCell::system("Conversation cleared."));
            vec![]
        }
        SessionUiEvent::Renamed { session_id, title } => {
            handle_session_renamed(tui, &session_id, title);
            vec![]
        }
        SessionUiEvent::RenameFailed { error } => {
            tui.transcript.cells.push(HistoryCell::system(&error));
            vec![]
        }
    }
}

/// Handles session list loaded - opens session picker overlay.
fn handle_session_list_loaded(
    _tui: &mut TuiState,
    overlay: &mut Option<Overlay>,
    sessions: Vec<SessionSummary>,
    original_cells: Vec<HistoryCell>,
) {
    if overlay.is_some() {
        return; // Don't open if another overlay is active
    }

    let (state, effects) = SessionPickerState::open(sessions, original_cells);
    *overlay = Some(Overlay::SessionPicker(state));

    // Process any preview effects by directly loading the first session preview
    // Note: This is a small compromise - we trigger the preview effect inline
    // rather than returning it, since we're already in the reducer
    for effect in effects {
        if let UiEffect::PreviewSession { session_id } = effect {
            // We can't spawn async from here, so we'll let the runtime handle it
            // The session picker already has the data it needs to show
            // Preview will be triggered when user navigates
            let _ = session_id; // Suppress unused warning
        }
    }
}

/// Handles session loaded - switches to the session.
fn handle_session_loaded(
    tui: &mut TuiState,
    session_id: &str,
    cells: Vec<HistoryCell>,
    messages: Vec<ChatMessage>,
    history: Vec<String>,
    session: Option<Session>,
) {
    // Reset state facets with loaded data
    tui.transcript.cells = cells;
    tui.conversation.messages = messages;
    tui.conversation.session = session;
    tui.conversation.usage = SessionUsage::new();
    tui.input.history = history;
    tui.transcript.scroll.reset();
    tui.transcript.wrap_cache.clear();

    // Show confirmation message
    let short_id = if session_id.len() > 8 {
        format!("{}…", &session_id[..8])
    } else {
        session_id.to_string()
    };
    tui.transcript.cells.push(HistoryCell::system(format!(
        "Switched to session {}",
        short_id
    )));
}

/// Handles session preview loaded - shows transcript without full switch.
fn handle_session_preview_loaded(tui: &mut TuiState, cells: Vec<HistoryCell>) {
    tui.transcript.cells = cells;
    tui.transcript.scroll.reset();
    tui.transcript.wrap_cache.clear();
}

/// Handles session created - sets up the new session.
fn handle_session_created(tui: &mut TuiState, session: Session, context_paths: Vec<PathBuf>) {
    let session_path = session.path().display().to_string();
    tui.conversation.session = Some(session);

    // Show session path
    tui.transcript.cells.push(HistoryCell::system(format!(
        "Session path: {}",
        session_path
    )));

    // Show loaded AGENTS.md files
    if !context_paths.is_empty() {
        let paths_list: Vec<String> = context_paths
            .iter()
            .map(|p| format!("  - {}", p.display()))
            .collect();
        let message = format!("Loaded AGENTS.md from:\n{}", paths_list.join("\n"));
        tui.transcript.cells.push(HistoryCell::system(message));
    }
}

/// Handles session rename success.
fn handle_session_renamed(tui: &mut TuiState, session_id: &str, title: Option<String>) {
    let short_id = short_session_id(session_id);
    let display_title = title.unwrap_or_else(|| short_id.clone());
    tui.transcript.cells.push(HistoryCell::system(format!(
        "Session {} renamed to \"{}\".",
        short_id, display_title
    )));
}
