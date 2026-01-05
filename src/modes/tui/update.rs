//! TUI reducer (update function).
//!
//! All state mutations happen here. The runtime calls `update(app, event)`
//! and executes the returned effects.
//!
//! This is the single source of truth for how events modify state.

use crossterm::event::Event;

use crate::modes::tui::events::UiEvent;
use crate::modes::tui::overlays;
use crate::modes::tui::shared::effects::UiEffect;
use crate::modes::tui::shared::internal::{
    AuthCommand, ConfigCommand, InputCommand, SessionCommand, StateCommand, TranscriptCommand,
};
use crate::modes::tui::app::{AgentState, AppState, TuiState};
use crate::modes::tui::input::HandoffState;
use crate::modes::tui::{auth, input, render, session, transcript};
use crate::modes::tui::{session::SessionUsage, transcript::TranscriptState};

/// The main reducer function.
///
/// Takes the current state and an event, mutates state, and returns effects
/// for the runtime to execute.
pub fn update(app: &mut AppState, event: UiEvent) -> Vec<UiEffect> {
    match event {
        UiEvent::Tick => {
            // Advance spinner animation
            app.tui.spinner_frame = app.tui.spinner_frame.wrapping_add(1);
            // Check if selection should be auto-cleared after copy
            app.tui.transcript.check_selection_timeout();
            vec![]
        }
        UiEvent::Frame { width, height } => {
            handle_frame(&mut app.tui, width, height);
            vec![]
        }
        UiEvent::Terminal(term_event) => handle_terminal_event(app, term_event),
        UiEvent::Agent(agent_event) => {
            let has_session = app.tui.conversation.session.is_some();
            let (effects, commands) = transcript::handle_agent_event(
                &mut app.tui.transcript,
                &mut app.tui.agent_state,
                has_session,
                &agent_event,
            );
            apply_state_commands(&mut app.tui, commands);
            effects
        }
        UiEvent::AgentSpawned { rx } => {
            app.tui.agent_state = AgentState::Waiting { rx };
            vec![]
        }
        UiEvent::LoginResult(result) => {
            let (commands, overlay_action) = auth::handle_login_result(&mut app.tui.auth, result);
            apply_state_commands(&mut app.tui, commands);

            match overlay_action {
                auth::LoginOverlayAction::Close => {
                    app.overlay = None;
                }
                auth::LoginOverlayAction::Reopen { error } => {
                    use crate::providers::oauth::anthropic;

                    let pkce = anthropic::generate_pkce();
                    let url = anthropic::build_auth_url(&pkce);
                    app.overlay = Some(overlays::Overlay::Login(
                        overlays::LoginState::AwaitingCode {
                            url,
                            pkce_verifier: pkce.verifier,
                            input: String::new(),
                            error: Some(error),
                        },
                    ));
                }
            }
            vec![]
        }
        UiEvent::LoginExchangeStarted { rx } => {
            app.tui.auth.login_rx = Some(rx);
            vec![]
        }
        UiEvent::HandoffResult(result) => {
            let commands = input::handle_handoff_result(&mut app.tui.input, result);
            apply_state_commands(&mut app.tui, commands);
            vec![]
        }
        UiEvent::HandoffGenerationStarted { goal, rx, cancel_tx } => {
            app.tui.input.handoff = HandoffState::Generating {
                goal,
                rx,
                cancel_tx,
            };
            vec![]
        }
        UiEvent::HandoffSessionCreated { session } => {
            let session_path = session.path().display().to_string();
            app.tui.conversation.session = Some(session);
            app.tui.transcript.cells.push(
                crate::modes::tui::transcript::HistoryCell::system(format!(
                    "Session path: {}",
                    session_path
                )),
            );
            vec![UiEffect::StartAgentTurn]
        }
        UiEvent::HandoffSessionCreateFailed { error } => {
            app.tui.transcript.cells.push(
                crate::modes::tui::transcript::HistoryCell::system(format!(
                    "Warning: Failed to create session: {}",
                    error
                )),
            );
            vec![UiEffect::StartAgentTurn]
        }
        UiEvent::FilesDiscovered(files) => {
            overlays::handle_files_discovered(&mut app.overlay, files);
            vec![]
        }

        // Session async result events - delegate to session feature
        UiEvent::Session(session_event) => {
            let (mut effects, commands, overlay_action) =
                session::handle_session_event(session_event);
            apply_state_commands(&mut app.tui, commands);

            if let session::SessionOverlayAction::OpenSessionPicker {
                sessions,
                original_cells,
            } = overlay_action
                && app.overlay.is_none()
            {
                let (state, overlay_effects) =
                    overlays::SessionPickerState::open(sessions, original_cells);
                app.overlay = Some(overlays::Overlay::SessionPicker(state));
                effects.extend(overlay_effects);
            }

            effects
        }
    }
}

// ============================================================================
// StateCommand Dispatcher
// ============================================================================

fn apply_state_commands(tui: &mut TuiState, commands: Vec<StateCommand>) {
    for command in commands {
        match command {
            StateCommand::Transcript(command) => apply_transcript_command(&mut tui.transcript, command),
            StateCommand::Input(command) => apply_input_command(&mut tui.input, command),
            StateCommand::Session(command) => apply_session_command(&mut tui.conversation, command),
            StateCommand::Auth(command) => apply_auth_command(&mut tui.auth, command),
            StateCommand::Config(command) => apply_config_command(tui, command),
        }
    }
}

fn apply_transcript_command(transcript: &mut TranscriptState, command: TranscriptCommand) {
    match command {
        TranscriptCommand::AppendCell(cell) => transcript.cells.push(cell),
        TranscriptCommand::AppendSystemMessage(message) => {
            transcript.cells.push(crate::modes::tui::transcript::HistoryCell::system(message));
        }
        TranscriptCommand::Clear => transcript.reset(),
        TranscriptCommand::ReplaceCells(cells) => transcript.cells = cells,
        TranscriptCommand::ResetScroll => transcript.scroll.reset(),
        TranscriptCommand::ClearWrapCache => transcript.wrap_cache.clear(),
        TranscriptCommand::ScrollToTop => transcript.scroll_to_top(),
        TranscriptCommand::ScrollToBottom => transcript.scroll_to_bottom(),
        TranscriptCommand::PageUp => transcript.page_up(),
        TranscriptCommand::PageDown => transcript.page_down(),
    }
}

fn apply_input_command(input: &mut crate::modes::tui::input::InputState, command: InputCommand) {
    match command {
        InputCommand::Clear => input.clear(),
        InputCommand::SetText(text) => input.set_text(&text),
        InputCommand::InsertChar(ch) => {
            input.textarea.insert_char(ch);
        }
        InputCommand::SetTextAndCursor {
            text,
            cursor_row,
            cursor_col,
        } => {
            use tui_textarea::CursorMove;

            input.set_text(&text);
            input.textarea.move_cursor(CursorMove::Top);
            input.textarea.move_cursor(CursorMove::Head);
            for _ in 0..cursor_row {
                input.textarea.move_cursor(CursorMove::Down);
            }
            for _ in 0..cursor_col {
                input.textarea.move_cursor(CursorMove::Forward);
            }
        }
        InputCommand::SetHistory(history) => {
            input.history = history;
            input.reset_navigation();
        }
        InputCommand::ClearHistory => input.clear_history(),
        InputCommand::SetHandoffState(state) => {
            input.handoff.cancel();
            input.handoff = state;
        }
    }
}

fn apply_session_command(
    session: &mut crate::modes::tui::session::SessionState,
    command: SessionCommand,
) {
    match command {
        SessionCommand::ClearMessages => session.messages.clear(),
        SessionCommand::SetMessages(messages) => session.messages = messages,
        SessionCommand::AppendMessage(message) => session.messages.push(message),
        SessionCommand::SetSession(session_handle) => session.session = session_handle,
        SessionCommand::ResetUsage => session.usage = SessionUsage::new(),
        SessionCommand::UpdateUsage {
            input,
            output,
            cache_read,
            cache_write,
        } => session.usage.add(input, output, cache_read, cache_write),
    }
}

fn apply_auth_command(auth: &mut crate::modes::tui::auth::AuthState, command: AuthCommand) {
    match command {
        AuthCommand::RefreshStatus => auth.refresh(),
        AuthCommand::ClearLoginRx => auth.login_rx = None,
    }
}

fn apply_config_command(tui: &mut TuiState, command: ConfigCommand) {
    match command {
        ConfigCommand::SetModel(model) => tui.config.model = model,
        ConfigCommand::SetThinkingLevel(level) => tui.config.thinking_level = level,
    }
}

// ============================================================================
// Frame Handler (layout, delta coalescing, cell line info)
// ============================================================================

/// Handles per-frame state updates.
///
/// This consolidates all the "housekeeping" mutations that need to happen
/// each frame: layout updates, delta coalescing, and cell line info for
/// lazy rendering.
fn handle_frame(tui: &mut TuiState, width: u16, height: u16) {
    // Update transcript layout with current terminal dimensions
    let viewport_height = render::calculate_transcript_height_with_state(tui, height);
    tui.transcript
        .update_layout((width, height), viewport_height);

    // Apply any pending streaming text deltas (coalescing)
    transcript::apply_pending_delta(&mut tui.transcript, &mut tui.agent_state);

    // Apply accumulated scroll delta from mouse events (coalescing)
    transcript::apply_scroll_delta(&mut tui.transcript);

    // Update cell line info for lazy rendering and scroll calculations
    let cell_line_counts = render::calculate_cell_line_counts(tui, width as usize);
    tui.transcript
        .scroll
        .update_cell_line_info(cell_line_counts);
}

// ============================================================================
// Terminal Event Handlers
// ============================================================================

fn handle_terminal_event(app: &mut AppState, event: Event) -> Vec<UiEffect> {
    match event {
        Event::Key(key) => handle_key(app, key),
        Event::Mouse(mouse) => {
            transcript::handle_mouse(&mut app.tui.transcript, mouse, render::TRANSCRIPT_MARGIN);
            vec![]
        }
        Event::Paste(text) => {
            input::handle_paste(&mut app.tui.input, &mut app.overlay, &text);
            vec![]
        }
        Event::Resize(_, _) => {
            // Clear wrap cache on resize since line wrapping depends on width
            app.tui.transcript.wrap_cache.clear();
            vec![]
        }
        _ => vec![],
    }
}

fn handle_key(app: &mut AppState, key: crossterm::event::KeyEvent) -> Vec<UiEffect> {
    if let Some(crate::modes::tui::overlays::Overlay::FilePicker(picker)) =
        app.overlay.as_mut()
        && crate::modes::tui::overlays::FilePickerState::should_route_input_key(key)
    {
        app.tui.input.textarea.input(key);
        if picker.update_from_input(&app.tui.input) {
            app.overlay = None;
        }
        return vec![];
    }

    // Try to dispatch to the active overlay
    let (effects, commands) = overlays::handle_overlay_key(&app.tui, &mut app.overlay, key);
    apply_state_commands(&mut app.tui, commands);
    if let Some(effects) = effects {
        return effects;
    }

    // No overlay active - delegate to input feature module
    let session_id = app
        .tui
        .conversation
        .session
        .as_ref()
        .map(|session| session.id.clone());
    let (effects, commands) =
        input::handle_main_key(&mut app.tui.input, &app.tui.agent_state, session_id, key);
    apply_state_commands(&mut app.tui, commands);
    effects
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::modes::tui::transcript::ScrollMode;

    #[test]
    fn test_scroll_to_top() {
        let config = crate::config::Config::default();
        let mut app = AppState::new(config, std::path::PathBuf::new(), None, None);

        app.tui.transcript.scroll_to_top();

        assert!(matches!(
            app.tui.transcript.scroll.mode,
            ScrollMode::Anchored { offset: 0 }
        ));
    }

    #[test]
    fn test_scroll_to_bottom() {
        let config = crate::config::Config::default();
        let mut app = AppState::new(config, std::path::PathBuf::new(), None, None);
        app.tui.transcript.scroll_to_top(); // Start from top

        app.tui.transcript.scroll_to_bottom();

        assert!(matches!(
            app.tui.transcript.scroll.mode,
            ScrollMode::FollowLatest
        ));
    }

    #[test]
    fn test_scroll_up_and_down() {
        let config = crate::config::Config::default();
        let mut app = AppState::new(config, std::path::PathBuf::new(), None, None);
        app.tui.transcript.scroll.update_line_count(100);

        // Start following, scroll up should anchor
        app.tui.transcript.scroll.scroll_up(5, 20);
        assert!(matches!(
            app.tui.transcript.scroll.mode,
            ScrollMode::Anchored { .. }
        ));

        // Scroll down should move towards bottom
        app.tui.transcript.scroll.scroll_down(100, 20);
        assert!(matches!(
            app.tui.transcript.scroll.mode,
            ScrollMode::FollowLatest
        ));
    }

    #[test]
    fn test_apply_scroll_delta_coalesces_events() {
        let config = crate::config::Config::default();
        let mut app = AppState::new(config, std::path::PathBuf::new(), None, None);
        app.tui.transcript.scroll.update_line_count(100);
        app.tui.transcript.viewport_height = 20;

        // Simulate multiple scroll up events (trackpad-like)
        app.tui.transcript.scroll_accumulator.accumulate(-1);
        app.tui.transcript.scroll_accumulator.accumulate(-1);
        app.tui.transcript.scroll_accumulator.accumulate(-1);

        // Apply should coalesce into single scroll of 3 lines
        transcript::apply_scroll_delta(&mut app.tui.transcript);

        // Should be anchored at offset 77 (80 - 3)
        assert!(matches!(
            app.tui.transcript.scroll.mode,
            ScrollMode::Anchored { offset: 77 }
        ));
    }
}
