//! TUI reducer (update function).
//!
//! All state mutations happen here. The runtime calls `update(app, event)`
//! and executes the returned effects.
//!
//! This is the single source of truth for how events modify state.

use crossterm::event::Event;

use crate::modes::tui::app::{AgentState, AppState, TuiState};
use crate::modes::tui::events::{ThreadUiEvent, UiEvent};
use crate::modes::tui::input::HandoffState;
use crate::modes::tui::overlays::{self, FilePickerState, Overlay};
use crate::modes::tui::shared::effects::UiEffect;
use crate::modes::tui::shared::internal::{ConfigMutation, StateMutation};
use crate::modes::tui::transcript::HistoryCell;
use crate::modes::tui::{auth, input, render, thread, transcript};

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
            let has_thread = app.tui.thread.thread_log.is_some();
            let (mut effects, mutations) = transcript::handle_agent_event(
                &mut app.tui.transcript,
                &mut app.tui.agent_state,
                has_thread,
                &agent_event,
            );
            apply_mutations(&mut app.tui, mutations);

            // Save usage to thread after each request completes.
            // A request is complete when we receive output tokens (MessageDelta with usage).
            // This ensures tool-use turns with multiple requests save all usage, not just the last.
            if let crate::core::events::AgentEvent::UsageUpdate { output_tokens, .. } = &agent_event
                && *output_tokens > 0
                && has_thread
            {
                // Save per-request delta values (not cumulative) for event-sourcing
                let usage = app.tui.thread.usage.turn_usage();
                effects.push(UiEffect::SaveThread {
                    event: crate::core::thread_log::ThreadEvent::usage(usage),
                });
                // Mark as saved to prevent duplicate saves on TurnComplete/Interrupted
                app.tui.thread.usage.mark_saved();
            }

            // Also save any unsaved usage on turn completion or interruption.
            // This handles the case where a request is interrupted before output tokens arrive -
            // we still want to save the input tokens that were consumed.
            if matches!(
                &agent_event,
                crate::core::events::AgentEvent::TurnComplete { .. }
                    | crate::core::events::AgentEvent::Interrupted
            ) && has_thread
                && app.tui.thread.usage.has_unsaved_usage()
            {
                let usage = app.tui.thread.usage.turn_usage();
                effects.push(UiEffect::SaveThread {
                    event: crate::core::thread_log::ThreadEvent::usage(usage),
                });
                app.tui.thread.usage.mark_saved();
            }

            effects
        }
        UiEvent::AgentSpawned { rx } => {
            app.tui.agent_state = AgentState::Waiting { rx };
            vec![]
        }
        UiEvent::LoginResult { req, result } => {
            if !app.tui.auth.login_request.finish_if_active(req) {
                return vec![];
            }

            let provider = match &app.overlay {
                Some(overlays::Overlay::Login(overlays::LoginState::Exchanging { provider })) => {
                    *provider
                }
                Some(overlays::Overlay::Login(overlays::LoginState::AwaitingCode {
                    provider,
                    ..
                })) => *provider,
                _ => crate::providers::provider_for_model(&app.tui.config.model),
            };
            let (mutations, overlay_action) =
                auth::handle_login_result(&mut app.tui.auth, result, provider);
            apply_mutations(&mut app.tui, mutations);

            match overlay_action {
                auth::LoginOverlayAction::Close => {
                    app.overlay = None;
                }
                auth::LoginOverlayAction::Reopen { error } => {
                    app.overlay = Some(overlays::Overlay::Login(overlays::LoginState::reopen(
                        provider, error,
                    )));
                }
            }
            vec![]
        }
        UiEvent::LoginCallbackResult(code) => {
            // Clear in-progress flag
            app.tui.auth.callback_in_progress = false;
            let mut effects = Vec::new();
            if let Some(overlays::Overlay::Login(login_state)) = &mut app.overlay {
                match login_state {
                    overlays::LoginState::AwaitingCode {
                        provider,
                        pkce_verifier,
                        error,
                        ..
                    } if *provider == crate::providers::ProviderKind::OpenAICodex => match code {
                        Some(code) => {
                            *error = None;
                            let verifier = pkce_verifier.clone();
                            let provider = *provider;
                            *login_state = overlays::LoginState::Exchanging { provider };
                            push_token_exchange(
                                &mut app.tui,
                                &mut effects,
                                provider,
                                code,
                                verifier,
                            );
                        }
                        None => {
                            *error =
                                Some("Local login timed out. Paste the code or URL.".to_string());
                        }
                    },
                    _ => {}
                }
            }
            effects
        }
        UiEvent::HandoffResult(result) => {
            let mutations = input::handle_handoff_result(&mut app.tui.input, result);
            apply_mutations(&mut app.tui, mutations);
            vec![]
        }
        UiEvent::HandoffGenerationStarted { goal, cancel } => {
            app.tui.input.handoff = HandoffState::Generating { goal, cancel };
            vec![]
        }
        UiEvent::HandoffThreadCreated { thread_log } => {
            let thread_path = thread_log.path().display().to_string();
            app.tui.thread.thread_log = Some(thread_log);
            app.tui
                .transcript
                .cells
                .push(HistoryCell::system(format!("Thread path: {}", thread_path)));
            vec![UiEffect::StartAgentTurn]
        }
        UiEvent::HandoffThreadCreateFailed { error } => {
            app.tui.transcript.cells.push(HistoryCell::system(format!(
                "Warning: Failed to create thread: {}",
                error
            )));
            vec![UiEffect::StartAgentTurn]
        }
        UiEvent::FileDiscoveryStarted { cancel } => {
            if let Some(overlays::Overlay::FilePicker(picker)) = &mut app.overlay {
                picker.discovery_cancel = Some(cancel);
            }
            vec![]
        }
        UiEvent::FilesDiscovered(files) => {
            if let Some(overlays::Overlay::FilePicker(picker)) = &mut app.overlay {
                picker.discovery_cancel = None;
            }
            overlays::handle_files_discovered(&mut app.overlay, files);
            vec![]
        }

        // Clipboard copy succeeded - show brief feedback in thread picker
        UiEvent::ClipboardCopied => {
            if let Some(overlays::Overlay::ThreadPicker(picker)) = &mut app.overlay {
                picker.copied_at = Some(std::time::Instant::now());
            }
            vec![]
        }

        // Direct bash execution events
        UiEvent::BashExecutionStarted {
            id,
            command,
            cancel,
        } => {
            app.tui.bash_running = Some((id.clone(), command.clone()));
            app.tui.bash_cancel = Some(cancel);

            // Create a running tool cell immediately (shows spinner)
            let input = serde_json::json!({ "command": command });
            let cell = HistoryCell::tool_running(&id, "bash", input);
            app.tui.transcript.cells.push(cell);

            // Don't add to messages yet - wait for result so we can send
            // a single user message with command + output
            vec![]
        }
        UiEvent::BashExecuted { id, result } => {
            // Get command from bash_running before clearing
            let command = app.tui.bash_running.as_ref().map(|(_, cmd)| cmd.clone());
            app.tui.bash_running = None;
            app.tui.bash_cancel = None;

            // Find the existing tool cell and set the result
            if let Some(cell) =
                app.tui.transcript.cells.iter_mut().find(
                    |c| matches!(c, HistoryCell::Tool { tool_use_id, .. } if tool_use_id == &id),
                )
            {
                cell.set_tool_result(result.clone());
            }

            // Persist to thread and add to messages for LLM context
            let mut effects = vec![];
            if let Some(cmd) = command
                && app.tui.thread.thread_log.is_some()
            {
                // Format as a user message describing what the user did
                // This makes it clear to the LLM that the USER ran the command
                let user_message = format!(
                    "[I executed a bash command]\n$ {}\n\nResult:\n{}",
                    cmd,
                    result.to_json_string()
                );

                // Save as user message event to thread log
                effects.push(UiEffect::SaveThread {
                    event: crate::core::thread_log::ThreadEvent::user_message(&user_message),
                });

                // Add user message for LLM context
                app.tui
                    .thread
                    .messages
                    .push(crate::providers::ChatMessage::user(&user_message));
            }

            effects
        }

        // Thread async result events - delegate to thread feature
        UiEvent::Thread(thread_event) => match thread_event {
            ThreadUiEvent::ListStarted => {
                app.tui.thread_ops.list_loading = true;
                vec![]
            }
            ThreadUiEvent::ListLoaded {
                threads,
                original_cells,
            } => {
                app.tui.thread_ops.list_loading = false;
                let (mut effects, mutations, overlay_action) =
                    thread::handle_thread_event(ThreadUiEvent::ListLoaded {
                        threads,
                        original_cells,
                    });
                apply_mutations(&mut app.tui, mutations);

                if let thread::ThreadOverlayAction::OpenThreadPicker {
                    threads,
                    original_cells,
                } = overlay_action
                    && app.overlay.is_none()
                {
                    let (state, overlay_effects) = overlays::ThreadPickerState::open(
                        threads,
                        original_cells,
                        &app.tui.agent_opts.root,
                    );
                    app.overlay = Some(overlays::Overlay::ThreadPicker(state));
                    effects.extend(overlay_effects);
                }

                effects
            }
            ThreadUiEvent::ListFailed { error } => {
                app.tui.thread_ops.list_loading = false;
                let (effects, mutations, _) =
                    thread::handle_thread_event(ThreadUiEvent::ListFailed { error });
                apply_mutations(&mut app.tui, mutations);
                effects
            }
            ThreadUiEvent::LoadStarted => {
                app.tui.thread_ops.load_loading = true;
                vec![]
            }
            ThreadUiEvent::Loaded {
                thread_id,
                cells,
                messages,
                history,
                thread_log,
                usage,
            } => {
                app.tui.thread_ops.load_loading = false;
                let (mut effects, mutations, overlay_action) =
                    thread::handle_thread_event(ThreadUiEvent::Loaded {
                        thread_id,
                        cells,
                        messages,
                        history,
                        thread_log,
                        usage,
                    });
                apply_mutations(&mut app.tui, mutations);

                if let thread::ThreadOverlayAction::OpenThreadPicker {
                    threads,
                    original_cells,
                } = overlay_action
                    && app.overlay.is_none()
                {
                    let (state, overlay_effects) = overlays::ThreadPickerState::open(
                        threads,
                        original_cells,
                        &app.tui.agent_opts.root,
                    );
                    app.overlay = Some(overlays::Overlay::ThreadPicker(state));
                    effects.extend(overlay_effects);
                }

                effects
            }
            ThreadUiEvent::LoadFailed { error } => {
                app.tui.thread_ops.load_loading = false;
                let (effects, mutations, _) =
                    thread::handle_thread_event(ThreadUiEvent::LoadFailed { error });
                apply_mutations(&mut app.tui, mutations);
                effects
            }
            ThreadUiEvent::PreviewStarted => {
                app.tui.thread_ops.preview_loading = true;
                vec![]
            }
            ThreadUiEvent::PreviewLoaded { req, cells } => {
                let allow = match app.overlay.as_mut() {
                    Some(overlays::Overlay::ThreadPicker(picker)) => {
                        picker.preview_request.finish_if_active(req)
                    }
                    _ => {
                        app.tui.thread_ops.preview_loading = false;
                        return vec![];
                    }
                };

                if !allow {
                    return vec![];
                }

                app.tui.thread_ops.preview_loading = false;
                let (mut effects, mutations, overlay_action) =
                    thread::handle_thread_event(ThreadUiEvent::PreviewLoaded { req, cells });
                apply_mutations(&mut app.tui, mutations);

                if let thread::ThreadOverlayAction::OpenThreadPicker {
                    threads,
                    original_cells,
                } = overlay_action
                    && app.overlay.is_none()
                {
                    let (state, overlay_effects) = overlays::ThreadPickerState::open(
                        threads,
                        original_cells,
                        &app.tui.agent_opts.root,
                    );
                    app.overlay = Some(overlays::Overlay::ThreadPicker(state));
                    effects.extend(overlay_effects);
                }

                effects
            }
            ThreadUiEvent::PreviewFailed { req } => {
                let allow = match app.overlay.as_mut() {
                    Some(overlays::Overlay::ThreadPicker(picker)) => {
                        picker.preview_request.finish_if_active(req)
                    }
                    _ => {
                        app.tui.thread_ops.preview_loading = false;
                        return vec![];
                    }
                };

                if !allow {
                    return vec![];
                }

                app.tui.thread_ops.preview_loading = false;
                let (effects, mutations, _) =
                    thread::handle_thread_event(ThreadUiEvent::PreviewFailed { req });
                apply_mutations(&mut app.tui, mutations);
                effects
            }
            ThreadUiEvent::CreateStarted => {
                app.tui.thread_ops.create_loading = true;
                vec![]
            }
            ThreadUiEvent::ForkStarted => {
                app.tui.thread_ops.fork_loading = true;
                vec![]
            }
            ThreadUiEvent::Created {
                thread_log,
                context_paths,
            } => {
                app.tui.thread_ops.create_loading = false;
                let (mut effects, mutations, overlay_action) =
                    thread::handle_thread_event(ThreadUiEvent::Created {
                        thread_log,
                        context_paths,
                    });
                apply_mutations(&mut app.tui, mutations);

                if let thread::ThreadOverlayAction::OpenThreadPicker {
                    threads,
                    original_cells,
                } = overlay_action
                    && app.overlay.is_none()
                {
                    let (state, overlay_effects) = overlays::ThreadPickerState::open(
                        threads,
                        original_cells,
                        &app.tui.agent_opts.root,
                    );
                    app.overlay = Some(overlays::Overlay::ThreadPicker(state));
                    effects.extend(overlay_effects);
                }

                effects
            }
            ThreadUiEvent::ForkedLoaded {
                thread_id,
                cells,
                messages,
                history,
                thread_log,
                usage,
                user_input,
                turn_number,
            } => {
                app.tui.thread_ops.fork_loading = false;
                let (mut effects, mutations, overlay_action) =
                    thread::handle_thread_event(ThreadUiEvent::ForkedLoaded {
                        thread_id,
                        cells,
                        messages,
                        history,
                        thread_log,
                        usage,
                        user_input,
                        turn_number,
                    });
                apply_mutations(&mut app.tui, mutations);

                if let thread::ThreadOverlayAction::OpenThreadPicker {
                    threads,
                    original_cells,
                } = overlay_action
                    && app.overlay.is_none()
                {
                    let (state, overlay_effects) = overlays::ThreadPickerState::open(
                        threads,
                        original_cells,
                        &app.tui.agent_opts.root,
                    );
                    app.overlay = Some(overlays::Overlay::ThreadPicker(state));
                    effects.extend(overlay_effects);
                }

                effects
            }
            ThreadUiEvent::CreateFailed { error } => {
                app.tui.thread_ops.create_loading = false;
                let (effects, mutations, _) =
                    thread::handle_thread_event(ThreadUiEvent::CreateFailed { error });
                apply_mutations(&mut app.tui, mutations);
                effects
            }
            ThreadUiEvent::ForkFailed { error } => {
                app.tui.thread_ops.fork_loading = false;
                let (effects, mutations, _) =
                    thread::handle_thread_event(ThreadUiEvent::ForkFailed { error });
                apply_mutations(&mut app.tui, mutations);
                effects
            }
            ThreadUiEvent::RenameStarted => {
                app.tui.thread_ops.rename_loading = true;
                vec![]
            }
            ThreadUiEvent::Renamed { thread_id, title } => {
                app.tui.thread_ops.rename_loading = false;
                let (mut effects, mutations, overlay_action) =
                    thread::handle_thread_event(ThreadUiEvent::Renamed { thread_id, title });
                apply_mutations(&mut app.tui, mutations);

                if let thread::ThreadOverlayAction::OpenThreadPicker {
                    threads,
                    original_cells,
                } = overlay_action
                    && app.overlay.is_none()
                {
                    let (state, overlay_effects) = overlays::ThreadPickerState::open(
                        threads,
                        original_cells,
                        &app.tui.agent_opts.root,
                    );
                    app.overlay = Some(overlays::Overlay::ThreadPicker(state));
                    effects.extend(overlay_effects);
                }

                effects
            }
            ThreadUiEvent::RenameFailed { error } => {
                app.tui.thread_ops.rename_loading = false;
                let (effects, mutations, _) =
                    thread::handle_thread_event(ThreadUiEvent::RenameFailed { error });
                apply_mutations(&mut app.tui, mutations);
                effects
            }
        },
    }
}

// ============================================================================
// StateMutation Dispatcher
// ============================================================================

fn apply_mutations(tui: &mut TuiState, mutations: Vec<StateMutation>) {
    for mutation in mutations {
        match mutation {
            StateMutation::Transcript(mutation) => tui.transcript.apply(mutation),
            StateMutation::Input(mutation) => tui.input.apply(mutation),
            StateMutation::Thread(mutation) => tui.thread.apply(mutation),
            StateMutation::Auth(mutation) => tui.auth.apply(mutation),
            StateMutation::Config(mutation) => apply_config_mutation(tui, mutation),
        }
    }
}

fn apply_config_mutation(tui: &mut TuiState, mutation: ConfigMutation) {
    match mutation {
        ConfigMutation::SetModel(model) => tui.config.model = model,
        ConfigMutation::SetThinkingLevel(level) => tui.config.thinking_level = level,
    }
}

fn push_token_exchange(
    tui: &mut TuiState,
    effects: &mut Vec<UiEffect>,
    provider: crate::providers::ProviderKind,
    code: String,
    verifier: String,
) {
    let req = tui.auth.login_request.begin();
    effects.push(UiEffect::SpawnTokenExchange {
        provider,
        code,
        verifier,
        req,
    });
}

fn apply_overlay_update(app: &mut AppState, update: overlays::OverlayUpdate) -> Vec<UiEffect> {
    let mut effects = Vec::with_capacity(update.effects.len());
    for effect in update.effects {
        match effect {
            overlays::OverlayEffect::StartTokenExchange {
                provider,
                code,
                verifier,
            } => push_token_exchange(&mut app.tui, &mut effects, provider, code, verifier),
            overlays::OverlayEffect::Ui(effect) => effects.push(effect),
        }
    }
    match update.transition {
        overlays::OverlayTransition::Stay => {}
        overlays::OverlayTransition::Close => {
            app.overlay = None;
        }
        overlays::OverlayTransition::Open(request) => {
            effects.extend(open_overlay_request(app, request));
        }
    }
    effects
}

fn open_overlay_request(app: &mut AppState, request: overlays::OverlayRequest) -> Vec<UiEffect> {
    match request {
        overlays::OverlayRequest::CommandPalette { command_mode } => {
            let provider = crate::providers::provider_for_model(&app.tui.config.model);
            let (state, effects) = overlays::CommandPaletteState::open(
                command_mode,
                provider,
                app.tui.config.model.clone(),
            );
            app.overlay = Some(overlays::Overlay::CommandPalette(state));
            effects
        }
        overlays::OverlayRequest::ModelPicker => {
            let (state, effects) = overlays::ModelPickerState::open(&app.tui.config.model);
            app.overlay = Some(overlays::Overlay::ModelPicker(state));
            effects
        }
        overlays::OverlayRequest::ThinkingPicker => {
            if !crate::models::model_supports_reasoning(&app.tui.config.model) {
                return vec![];
            }
            let (state, effects) =
                overlays::ThinkingPickerState::open(app.tui.config.thinking_level);
            app.overlay = Some(overlays::Overlay::ThinkingPicker(state));
            effects
        }
        overlays::OverlayRequest::Login => {
            let (state, effects) = overlays::LoginState::open(&app.tui);
            app.overlay = Some(overlays::Overlay::Login(state));
            effects
        }
        overlays::OverlayRequest::FilePicker { trigger_pos } => {
            let (state, effects) = overlays::FilePickerState::open(trigger_pos);
            app.overlay = Some(overlays::Overlay::FilePicker(state));
            effects
        }
        overlays::OverlayRequest::Timeline => {
            let (state, effects, mutations) = overlays::TimelineState::open(
                &app.tui.transcript.cells,
                &app.tui.transcript.scroll,
                app.tui.transcript.scroll.mode.clone(),
            );
            app.overlay = Some(overlays::Overlay::Timeline(state));
            apply_mutations(&mut app.tui, mutations);
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
    if let Some(mut update) = overlays::handle_overlay_key(&app.tui, &mut app.overlay, key) {
        apply_mutations(&mut app.tui, std::mem::take(&mut update.mutations));
        return apply_overlay_update(app, update);
    }

    // No overlay active - delegate to input feature module
    let thread_id = app
        .tui
        .thread
        .thread_log
        .as_ref()
        .map(|thread_log| thread_log.id.clone());
    let (effects, mutations, overlay_request) = input::handle_main_key(
        &mut app.tui.input,
        &app.tui.agent_state,
        app.tui.bash_running.is_some(),
        thread_id,
        &app.tui.config.model,
        key,
    );
    apply_mutations(&mut app.tui, mutations);
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
