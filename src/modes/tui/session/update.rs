//! Session feature reducer.
//!
//! Handles session-related state transitions: loading, switching, creating, renaming.

use std::path::PathBuf;

use crate::core::session::{Session, SessionSummary, Usage, short_session_id};
use crate::modes::tui::events::SessionUiEvent;
use crate::modes::tui::shared::effects::UiEffect;
use crate::modes::tui::shared::internal::{
    InputCommand, SessionCommand, StateCommand, TranscriptCommand,
};
use crate::modes::tui::transcript::HistoryCell;
use crate::providers::anthropic::ChatMessage;

/// Handles session UI events.
///
/// Dispatches to specific handlers based on event type.
/// Returns effects for the runtime to execute.
#[derive(Debug)]
pub enum SessionOverlayAction {
    OpenSessionPicker {
        sessions: Vec<SessionSummary>,
        original_cells: Vec<HistoryCell>,
    },
    None,
}

pub fn handle_session_event(
    event: SessionUiEvent,
) -> (Vec<UiEffect>, Vec<StateCommand>, SessionOverlayAction) {
    let mut commands = Vec::new();
    let mut overlay_action = SessionOverlayAction::None;
    let effects = match event {
        SessionUiEvent::ListStarted { .. }
        | SessionUiEvent::LoadStarted { .. }
        | SessionUiEvent::PreviewStarted { .. }
        | SessionUiEvent::CreateStarted { .. }
        | SessionUiEvent::ForkStarted { .. }
        | SessionUiEvent::RenameStarted { .. } => vec![],
        SessionUiEvent::ListLoaded {
            sessions,
            original_cells,
        } => {
            overlay_action = SessionOverlayAction::OpenSessionPicker {
                sessions,
                original_cells,
            };
            vec![]
        }
        SessionUiEvent::ListFailed { error } => {
            commands.push(StateCommand::Transcript(
                TranscriptCommand::AppendSystemMessage(error),
            ));
            vec![]
        }
        SessionUiEvent::Loaded {
            session_id,
            cells,
            messages,
            history,
            session,
            usage,
        } => {
            handle_session_loaded(
                session,
                &session_id,
                cells,
                messages,
                history,
                usage,
                &mut commands,
            );
            vec![]
        }
        SessionUiEvent::LoadFailed { error } => {
            commands.push(StateCommand::Transcript(
                TranscriptCommand::AppendSystemMessage(error),
            ));
            vec![]
        }
        SessionUiEvent::PreviewLoaded { cells } => {
            handle_session_preview_loaded(cells, &mut commands);
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
            handle_session_created(session, context_paths, &mut commands);
            vec![]
        }
        SessionUiEvent::ForkedLoaded {
            session_id: _,
            cells,
            messages,
            history,
            session,
            usage,
            user_input,
            turn_number,
        } => {
            handle_session_forked(
                session,
                cells,
                messages,
                history,
                usage,
                user_input,
                turn_number,
                &mut commands,
            );
            vec![]
        }
        SessionUiEvent::CreateFailed { error } => {
            commands.push(StateCommand::Transcript(
                TranscriptCommand::AppendSystemMessage(error),
            ));
            commands.push(StateCommand::Transcript(
                TranscriptCommand::AppendSystemMessage("Conversation cleared.".to_string()),
            ));
            vec![]
        }
        SessionUiEvent::ForkFailed { error } => {
            commands.push(StateCommand::Transcript(
                TranscriptCommand::AppendSystemMessage(error),
            ));
            vec![]
        }
        SessionUiEvent::Renamed { session_id, title } => {
            handle_session_renamed(&session_id, title, &mut commands);
            vec![]
        }
        SessionUiEvent::RenameFailed { error } => {
            commands.push(StateCommand::Transcript(
                TranscriptCommand::AppendSystemMessage(error),
            ));
            vec![]
        }
    };

    (effects, commands, overlay_action)
}

/// Handles session list loaded - opens session picker overlay.
/// Handles session loaded - switches to the session.
fn handle_session_loaded(
    session: Option<Session>,
    session_id: &str,
    cells: Vec<HistoryCell>,
    messages: Vec<ChatMessage>,
    history: Vec<String>,
    usage: (Usage, Usage),
    commands: &mut Vec<StateCommand>,
) {
    let (cumulative, latest) = usage;
    commands.push(StateCommand::Transcript(TranscriptCommand::ReplaceCells(
        cells,
    )));
    commands.push(StateCommand::Transcript(TranscriptCommand::ResetScroll));
    commands.push(StateCommand::Transcript(TranscriptCommand::ClearWrapCache));
    commands.push(StateCommand::Session(SessionCommand::SetMessages(messages)));
    commands.push(StateCommand::Session(SessionCommand::SetSession(session)));
    commands.push(StateCommand::Session(SessionCommand::SetUsage {
        cumulative,
        latest,
    }));
    commands.push(StateCommand::Input(InputCommand::SetHistory(history)));

    // Show confirmation message
    let short_id = if session_id.len() > 8 {
        format!("{}…", &session_id[..8])
    } else {
        session_id.to_string()
    };
    commands.push(StateCommand::Transcript(
        TranscriptCommand::AppendSystemMessage(format!("Switched to session {}", short_id)),
    ));
}

/// Handles session preview loaded - shows transcript without full switch.
fn handle_session_preview_loaded(cells: Vec<HistoryCell>, commands: &mut Vec<StateCommand>) {
    commands.push(StateCommand::Transcript(TranscriptCommand::ReplaceCells(
        cells,
    )));
    commands.push(StateCommand::Transcript(TranscriptCommand::ResetScroll));
    commands.push(StateCommand::Transcript(TranscriptCommand::ClearWrapCache));
}

/// Handles session created - sets up the new session.
fn handle_session_created(
    session: Session,
    context_paths: Vec<PathBuf>,
    commands: &mut Vec<StateCommand>,
) {
    let session_path = session.path().display().to_string();
    commands.push(StateCommand::Session(SessionCommand::SetSession(Some(
        session,
    ))));

    // Show session path
    commands.push(StateCommand::Transcript(
        TranscriptCommand::AppendSystemMessage(format!("Session path: {}", session_path)),
    ));

    // Show loaded AGENTS.md files
    if !context_paths.is_empty() {
        let paths_list: Vec<String> = context_paths
            .iter()
            .map(|p| format!("  - {}", p.display()))
            .collect();
        let message = format!("Loaded AGENTS.md from:\n{}", paths_list.join("\n"));
        commands.push(StateCommand::Transcript(
            TranscriptCommand::AppendSystemMessage(message),
        ));
    }
}

#[allow(clippy::too_many_arguments)]
fn handle_session_forked(
    session: Session,
    cells: Vec<HistoryCell>,
    messages: Vec<ChatMessage>,
    history: Vec<String>,
    usage: (Usage, Usage),
    user_input: Option<String>,
    turn_number: usize,
    commands: &mut Vec<StateCommand>,
) {
    let (cumulative, latest) = usage;
    commands.push(StateCommand::Transcript(TranscriptCommand::ReplaceCells(
        cells,
    )));
    commands.push(StateCommand::Transcript(TranscriptCommand::ResetScroll));
    commands.push(StateCommand::Transcript(TranscriptCommand::ClearWrapCache));
    commands.push(StateCommand::Session(SessionCommand::SetMessages(messages)));
    commands.push(StateCommand::Session(SessionCommand::SetSession(Some(
        session,
    ))));
    commands.push(StateCommand::Session(SessionCommand::SetUsage {
        cumulative,
        latest,
    }));
    commands.push(StateCommand::Input(InputCommand::SetHistory(history)));
    commands.push(StateCommand::Input(InputCommand::Clear));
    if let Some(text) = user_input {
        commands.push(StateCommand::Input(InputCommand::SetText(text)));
    }
    commands.push(StateCommand::Transcript(
        TranscriptCommand::AppendSystemMessage(format!("Forked from turn {}.", turn_number)),
    ));
}

/// Handles session rename success.
fn handle_session_renamed(
    session_id: &str,
    title: Option<String>,
    commands: &mut Vec<StateCommand>,
) {
    let short_id = short_session_id(session_id);
    let display_title = title.unwrap_or_else(|| short_id.clone());
    commands.push(StateCommand::Transcript(
        TranscriptCommand::AppendSystemMessage(format!(
            "Session {} renamed to \"{}\".",
            short_id, display_title
        )),
    ));
}
