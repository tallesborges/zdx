//! TUI reducer (update function).
//!
//! All state mutations happen here. The runtime calls `update(app, event)`
//! and executes the returned effects.
//!
//! This is the single source of truth for how events modify state.

use crossterm::event::Event;

use crate::modes::tui::app::{AgentState, AppState, TuiState};
use crate::modes::tui::events::{SessionUiEvent, UiEvent};
use crate::modes::tui::input::HandoffState;
use crate::modes::tui::overlays::{self, FilePickerState, Overlay};
use crate::modes::tui::shared::effects::UiEffect;
use crate::modes::tui::shared::internal::{ConfigCommand, StateCommand};
use crate::modes::tui::transcript::HistoryCell;
use crate::modes::tui::{auth, input, render, session, transcript};

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
            let (mut effects, commands) = transcript::handle_agent_event(
                &mut app.tui.transcript,
                &mut app.tui.agent_state,
                has_session,
                &agent_event,
            );
            apply_state_commands(&mut app.tui, commands);

            // Save usage to session after each request completes.
            // A request is complete when we receive output tokens (MessageDelta with usage).
            // This ensures tool-use turns with multiple requests save all usage, not just the last.
            if let crate::core::events::AgentEvent::UsageUpdate { output_tokens, .. } = &agent_event
                && *output_tokens > 0
                && has_session
            {
                // Save per-request delta values (not cumulative) for event-sourcing
                let usage = app.tui.conversation.usage.turn_usage();
                effects.push(UiEffect::SaveSession {
                    event: crate::core::session::SessionEvent::usage(usage),
                });
                // Mark as saved to prevent duplicate saves on TurnComplete/Interrupted
                app.tui.conversation.usage.mark_saved();
            }

            // Also save any unsaved usage on turn completion or interruption.
            // This handles the case where a request is interrupted before output tokens arrive -
            // we still want to save the input tokens that were consumed.
            if matches!(
                &agent_event,
                crate::core::events::AgentEvent::TurnComplete { .. }
                    | crate::core::events::AgentEvent::Interrupted
            ) && has_session
                && app.tui.conversation.usage.has_unsaved_usage()
            {
                let usage = app.tui.conversation.usage.turn_usage();
                effects.push(UiEffect::SaveSession {
                    event: crate::core::session::SessionEvent::usage(usage),
                });
                app.tui.conversation.usage.mark_saved();
            }

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
        UiEvent::HandoffGenerationStarted {
            goal,
            rx,
            cancel_tx,
        } => {
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
            app.tui.transcript.cells.push(HistoryCell::system(format!(
                "Session path: {}",
                session_path
            )));
            vec![UiEffect::StartAgentTurn]
        }
        UiEvent::HandoffSessionCreateFailed { error } => {
            app.tui.transcript.cells.push(HistoryCell::system(format!(
                "Warning: Failed to create session: {}",
                error
            )));
            vec![UiEffect::StartAgentTurn]
        }
        UiEvent::FileDiscoveryStarted { rx, cancel } => {
            if let Some(overlays::Overlay::FilePicker(picker)) = &mut app.overlay {
                picker.discovery_rx = Some(rx);
                picker.discovery_cancel = Some(cancel);
            }
            vec![]
        }
        UiEvent::FilesDiscovered(files) => {
            if let Some(overlays::Overlay::FilePicker(picker)) = &mut app.overlay {
                picker.discovery_rx = None;
            }
            overlays::handle_files_discovered(&mut app.overlay, files);
            vec![]
        }

        // Clipboard copy succeeded - show brief feedback in session picker
        UiEvent::ClipboardCopied => {
            if let Some(overlays::Overlay::SessionPicker(picker)) = &mut app.overlay {
                picker.copied_at = Some(std::time::Instant::now());
            }
            vec![]
        }

        // Session async result events - delegate to session feature
        UiEvent::Session(session_event) => match session_event {
            SessionUiEvent::ListStarted { rx } => {
                app.tui.session_ops.list_rx = Some(rx);
                vec![]
            }
            SessionUiEvent::ListLoaded {
                sessions,
                original_cells,
            } => {
                app.tui.session_ops.list_rx = None;
                let (mut effects, commands, overlay_action) =
                    session::handle_session_event(SessionUiEvent::ListLoaded {
                        sessions,
                        original_cells,
                    });
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
            SessionUiEvent::ListFailed { error } => {
                app.tui.session_ops.list_rx = None;
                let (effects, commands, _) =
                    session::handle_session_event(SessionUiEvent::ListFailed { error });
                apply_state_commands(&mut app.tui, commands);
                effects
            }
            SessionUiEvent::LoadStarted { rx } => {
                app.tui.session_ops.load_rx = Some(rx);
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
                app.tui.session_ops.load_rx = None;
                let (mut effects, commands, overlay_action) =
                    session::handle_session_event(SessionUiEvent::Loaded {
                        session_id,
                        cells,
                        messages,
                        history,
                        session,
                        usage,
                    });
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
            SessionUiEvent::LoadFailed { error } => {
                app.tui.session_ops.load_rx = None;
                let (effects, commands, _) =
                    session::handle_session_event(SessionUiEvent::LoadFailed { error });
                apply_state_commands(&mut app.tui, commands);
                effects
            }
            SessionUiEvent::PreviewStarted { rx } => {
                app.tui.session_ops.preview_rx = Some(rx);
                vec![]
            }
            SessionUiEvent::PreviewLoaded { cells } => {
                app.tui.session_ops.preview_rx = None;
                let (mut effects, commands, overlay_action) =
                    session::handle_session_event(SessionUiEvent::PreviewLoaded { cells });
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
            SessionUiEvent::PreviewFailed => {
                app.tui.session_ops.preview_rx = None;
                let (effects, commands, _) =
                    session::handle_session_event(SessionUiEvent::PreviewFailed);
                apply_state_commands(&mut app.tui, commands);
                effects
            }
            SessionUiEvent::CreateStarted { rx } => {
                app.tui.session_ops.create_rx = Some(rx);
                vec![]
            }
            SessionUiEvent::Created {
                session,
                context_paths,
            } => {
                app.tui.session_ops.create_rx = None;
                let (mut effects, commands, overlay_action) =
                    session::handle_session_event(SessionUiEvent::Created {
                        session,
                        context_paths,
                    });
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
            SessionUiEvent::CreateFailed { error } => {
                app.tui.session_ops.create_rx = None;
                let (effects, commands, _) =
                    session::handle_session_event(SessionUiEvent::CreateFailed { error });
                apply_state_commands(&mut app.tui, commands);
                effects
            }
            SessionUiEvent::RenameStarted { rx } => {
                app.tui.session_ops.rename_rx = Some(rx);
                vec![]
            }
            SessionUiEvent::Renamed { session_id, title } => {
                app.tui.session_ops.rename_rx = None;
                let (mut effects, commands, overlay_action) =
                    session::handle_session_event(SessionUiEvent::Renamed { session_id, title });
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
            SessionUiEvent::RenameFailed { error } => {
                app.tui.session_ops.rename_rx = None;
                let (effects, commands, _) =
                    session::handle_session_event(SessionUiEvent::RenameFailed { error });
                apply_state_commands(&mut app.tui, commands);
                effects
            }
        },
    }
}

// ============================================================================
// StateCommand Dispatcher
// ============================================================================

fn apply_state_commands(tui: &mut TuiState, commands: Vec<StateCommand>) {
    for command in commands {
        match command {
            StateCommand::Transcript(command) => tui.transcript.apply(command),
            StateCommand::Input(command) => tui.input.apply(command),
            StateCommand::Session(command) => tui.conversation.apply(command),
            StateCommand::Auth(command) => tui.auth.apply(command),
            StateCommand::Config(command) => apply_config_command(tui, command),
        }
    }
}

fn apply_config_command(tui: &mut TuiState, command: ConfigCommand) {
    match command {
        ConfigCommand::SetModel(model) => tui.config.model = model,
        ConfigCommand::SetThinkingLevel(level) => tui.config.thinking_level = level,
    }
}

fn apply_overlay_action(app: &mut AppState, action: overlays::OverlayAction) -> Vec<UiEffect> {
    match action {
        overlays::OverlayAction::Close(effects) => {
            app.overlay = None;
            effects
        }
        overlays::OverlayAction::Effects(effects) => effects,
        overlays::OverlayAction::Open(request) => open_overlay_request(app, request),
    }
}

fn open_overlay_request(app: &mut AppState, request: overlays::OverlayRequest) -> Vec<UiEffect> {
    match request {
        overlays::OverlayRequest::CommandPalette { command_mode } => {
            let (state, effects) = overlays::CommandPaletteState::open(command_mode);
            app.overlay = Some(overlays::Overlay::CommandPalette(state));
            effects
        }
        overlays::OverlayRequest::ModelPicker => {
            let (state, effects) = overlays::ModelPickerState::open(&app.tui.config.model);
            app.overlay = Some(overlays::Overlay::ModelPicker(state));
            effects
        }
        overlays::OverlayRequest::ThinkingPicker => {
            let (state, effects) =
                overlays::ThinkingPickerState::open(app.tui.config.thinking_level);
            app.overlay = Some(overlays::Overlay::ThinkingPicker(state));
            effects
        }
        overlays::OverlayRequest::Login => {
            let (state, effects) = overlays::LoginState::open();
            app.overlay = Some(overlays::Overlay::Login(state));
            effects
        }
        overlays::OverlayRequest::FilePicker { trigger_pos } => {
            let (state, effects) = overlays::FilePickerState::open(trigger_pos);
            app.overlay = Some(overlays::Overlay::FilePicker(state));
            effects
        }
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
    if let Some(Overlay::FilePicker(picker)) = app.overlay.as_mut()
        && FilePickerState::should_route_input_key(key)
    {
        app.tui.input.textarea.input(key);
        if picker.update_from_input(&app.tui.input) {
            app.overlay = None;
        }
        return vec![];
    }

    // Try to dispatch to the active overlay
    let (action, commands) = overlays::handle_overlay_key(&app.tui, &mut app.overlay, key);
    apply_state_commands(&mut app.tui, commands);
    if let Some(action) = action {
        return apply_overlay_action(app, action);
    }

    // If overlay is active but returned no action, it still consumed the key
    if app.overlay.is_some() {
        return vec![];
    }

    // No overlay active - delegate to input feature module
    let session_id = app
        .tui
        .conversation
        .session
        .as_ref()
        .map(|session| session.id.clone());
    let (effects, commands, overlay_request) =
        input::handle_main_key(&mut app.tui.input, &app.tui.agent_state, session_id, key);
    apply_state_commands(&mut app.tui, commands);
    if let Some(request) = overlay_request
        && app.overlay.is_none()
    {
        let mut overlay_effects = open_overlay_request(app, request);
        overlay_effects.extend(effects);
        return overlay_effects;
    }

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
