//! TUI reducer (update function).
//!
//! All state mutations happen here. The runtime calls `update(app, event)`
//! and executes the returned effects.
//!
//! This is the single source of truth for how events modify state.

use crossterm::event::Event;

use crate::common::{TaskKind, TaskMeta};
use crate::effects::UiEffect;
use crate::events::{SkillUiEvent, ThreadUiEvent, UiEvent};
use crate::input::HandoffState;
use crate::mutations::{ConfigMutation, StateMutation, TranscriptMutation};
use crate::overlays::{self, FilePickerState, Overlay};
use crate::state::{AgentState, AppState, TuiState};
use crate::transcript::HistoryCell;
use crate::{auth, input, render, thread, transcript};

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
            // Apply pending streaming deltas each tick so final chunks render without input
            transcript::apply_pending_delta(&mut app.tui.transcript, &mut app.tui.agent_state);
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

            // Mark tool used for turn timing (only show "Worked for Xs" if tools were used)
            if matches!(
                &agent_event,
                zdx_core::core::events::AgentEvent::ToolRequested { .. }
            ) {
                app.tui.status_line.mark_tool_used();
            }

            // Save usage to thread after each request completes.
            // A request is complete when we receive output tokens (MessageDelta with usage).
            // This ensures tool-use turns with multiple requests save all usage, not just the last.
            if let zdx_core::core::events::AgentEvent::UsageUpdate { output_tokens, .. } =
                &agent_event
                && *output_tokens > 0
                && has_thread
            {
                // Save per-request delta values (not cumulative) for event-sourcing
                let usage = app.tui.thread.usage.turn_usage();
                effects.push(UiEffect::SaveThread {
                    event: zdx_core::core::thread_log::ThreadEvent::usage(usage),
                });
                // Mark as saved to prevent duplicate saves on TurnCompleted/Interrupted
                app.tui.thread.usage.mark_saved();
            }

            // Also save any unsaved usage on turn completion or interruption.
            // This handles the case where a request is interrupted before output tokens arrive -
            // we still want to save the input tokens that were consumed.
            if matches!(
                &agent_event,
                zdx_core::core::events::AgentEvent::TurnCompleted { .. }
                    | zdx_core::core::events::AgentEvent::Interrupted { .. }
            ) && has_thread
                && app.tui.thread.usage.has_unsaved_usage()
            {
                let usage = app.tui.thread.usage.turn_usage();
                effects.push(UiEffect::SaveThread {
                    event: zdx_core::core::thread_log::ThreadEvent::usage(usage),
                });
                app.tui.thread.usage.mark_saved();
            }

            let should_dequeue = matches!(
                &agent_event,
                zdx_core::core::events::AgentEvent::TurnCompleted { .. }
                    | zdx_core::core::events::AgentEvent::Interrupted { .. }
                    | zdx_core::core::events::AgentEvent::Error { .. }
            );

            // End turn timer and push timing cell when turn completes
            if should_dequeue && let Some((duration, tool_count)) = app.tui.status_line.end_turn() {
                // Only show timing for turns that ran for at least 1 second
                if duration.as_secs_f64() >= 1.0 {
                    app.tui
                        .transcript
                        .push_cell(HistoryCell::timing(duration, tool_count));
                }
            }

            if should_dequeue
                && !app.tui.agent_state.is_running()
                && !app.tui.tasks.state(TaskKind::Bash).is_running()
                && !app.tui.transcript.has_pending_user_cell()
                && let Some(text) = app.tui.input.pop_queued_prompt()
            {
                let thread_id = app.tui.thread.thread_log.as_ref().map(|log| log.id.clone());
                let should_suggest_title = thread_id.is_some()
                    && app.tui.thread.title.is_none()
                    && !app.tui.tasks.state(TaskKind::ThreadTitle).is_running();
                let (queue_effects, queue_mutations) =
                    input::build_send_effects(&text, thread_id, should_suggest_title);
                apply_mutations(&mut app.tui, queue_mutations);
                effects.extend(queue_effects);
            }

            effects
        }
        UiEvent::AgentSpawned { rx } => {
            app.tui.agent_state = AgentState::Waiting { rx };
            app.tui.transcript.activate_pending_user_cell();
            app.tui.status_line.start_turn();
            vec![]
        }
        UiEvent::LoginResult { result } => {
            let provider = match &app.overlay {
                Some(overlays::Overlay::Login(overlays::LoginState::Exchanging { provider })) => {
                    *provider
                }
                Some(overlays::Overlay::Login(overlays::LoginState::AwaitingCode {
                    provider,
                    ..
                })) => *provider,
                _ => zdx_core::providers::provider_for_model(&app.tui.config.model),
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
            let mut effects = Vec::new();
            if let Some(overlays::Overlay::Login(login_state)) = &mut app.overlay {
                match login_state {
                    overlays::LoginState::AwaitingCode {
                        provider,
                        pkce_verifier,
                        oauth_state,
                        redirect_uri,
                        error,
                        ..
                    } if matches!(
                        *provider,
                        zdx_core::providers::ProviderKind::ClaudeCli
                            | zdx_core::providers::ProviderKind::OpenAICodex
                            | zdx_core::providers::ProviderKind::GeminiCli
                    ) =>
                    {
                        match code {
                            Some(code) => {
                                *error = None;
                                let verifier = pkce_verifier.clone();
                                let provider = *provider;
                                let code =
                                    if provider == zdx_core::providers::ProviderKind::ClaudeCli {
                                        let state =
                                            oauth_state.clone().unwrap_or_else(|| verifier.clone());
                                        format!("{}#{}", code, state)
                                    } else {
                                        code
                                    };
                                let redirect_uri =
                                    if provider == zdx_core::providers::ProviderKind::ClaudeCli {
                                        redirect_uri.clone()
                                    } else {
                                        None
                                    };
                                *login_state = overlays::LoginState::Exchanging { provider };
                                push_token_exchange(
                                    &mut effects,
                                    provider,
                                    code,
                                    verifier,
                                    redirect_uri,
                                );
                            }
                            None => {
                                *error = Some(
                                    "Local login timed out. Paste the code or URL.".to_string(),
                                );
                            }
                        }
                    }
                    _ => {}
                }
            }
            effects
        }
        UiEvent::TaskStarted { kind, started } => {
            app.tui.tasks.state_mut(kind).on_started(&started);
            match kind {
                TaskKind::Handoff => {
                    if matches!(&started.meta, TaskMeta::Handoff { .. }) {
                        app.tui.input.handoff = HandoffState::Generating;
                    }
                }
                TaskKind::Bash => {
                    if let TaskMeta::Bash { id, command } = &started.meta {
                        // Create a running tool cell immediately (shows spinner)
                        let input = serde_json::json!({ "command": command });
                        let cell = HistoryCell::tool_running(id, "bash", input);
                        app.tui.transcript.push_cell(cell);
                    }
                }
                TaskKind::FileDiscovery
                | TaskKind::SkillsFetch
                | TaskKind::SkillInstall
                | TaskKind::ThreadList
                | TaskKind::ThreadLoad
                | TaskKind::ThreadRename
                | TaskKind::ThreadTitle
                | TaskKind::ThreadPreview
                | TaskKind::ThreadCreate
                | TaskKind::ThreadFork
                | TaskKind::LoginExchange
                | TaskKind::LoginCallback => {}
            }
            vec![]
        }
        UiEvent::TaskCompleted { kind, completed } => {
            let ok = {
                let state = app.tui.tasks.state_mut(kind);
                state.finish_if_active(completed.id)
            };
            if !ok {
                vec![]
            } else {
                update(app, *completed.result)
            }
        }
        UiEvent::HandoffResult { goal, result } => {
            let mutations = input::handle_handoff_result(&mut app.tui.input, goal, result);
            apply_mutations(&mut app.tui, mutations);
            vec![]
        }
        UiEvent::HandoffThreadCreated {
            thread_log,
            context_paths,
            prompt,
        } => {
            let (effects, mutations, _action) =
                thread::handle_thread_event(ThreadUiEvent::Created {
                    thread_log,
                    context_paths,
                    skills: Vec::new(), // Handoff creation doesn't currently track skills here
                });
            apply_mutations(&mut app.tui, mutations);
            app.tui.input.set_text(&prompt);
            effects
        }
        UiEvent::HandoffThreadCreateFailed { error } => {
            app.tui.transcript.push_cell(HistoryCell::system(format!(
                "Warning: Failed to create thread: {}",
                error
            )));
            vec![]
        }
        UiEvent::FilesDiscovered(files) => {
            overlays::handle_files_discovered(&mut app.overlay, files);
            vec![]
        }

        UiEvent::Skill(skill_event) => match skill_event {
            SkillUiEvent::ListLoaded { repo, skills } => {
                if let Some(overlays::Overlay::SkillPicker(picker)) = &mut app.overlay {
                    let items = skills
                        .into_iter()
                        .map(|skill| overlays::skill_picker::SkillItem {
                            name: skill.name,
                            path: skill.path,
                            description: skill.description,
                        })
                        .collect();
                    picker.set_skills(&repo, items);
                }
                vec![]
            }
            SkillUiEvent::ListFailed { repo, error } => {
                if let Some(overlays::Overlay::SkillPicker(picker)) = &mut app.overlay {
                    picker.set_error(&repo, format!("Failed to load skills: {}", error));
                }
                vec![]
            }
            SkillUiEvent::Installed { repo: _, skill } => {
                if let Some(overlays::Overlay::SkillPicker(picker)) = &mut app.overlay {
                    picker.set_installing(None);
                    picker.mark_installed(&skill);
                }
                app.overlay = None;
                apply_mutations(
                    &mut app.tui,
                    vec![StateMutation::Transcript(
                        TranscriptMutation::AppendSystemMessage(format!(
                            "Installed skill \"{}\". Restart ZDX to pick up new skills.",
                            skill
                        )),
                    )],
                );
                vec![]
            }
            SkillUiEvent::InstallFailed { repo, skill, error } => {
                if let Some(overlays::Overlay::SkillPicker(picker)) = &mut app.overlay {
                    picker.set_installing(None);
                    picker.set_error(&repo, format!("Install failed: {}", error));
                }
                apply_mutations(
                    &mut app.tui,
                    vec![StateMutation::Transcript(
                        TranscriptMutation::AppendSystemMessage(format!(
                            "Failed to install {}: {}",
                            skill, error
                        )),
                    )],
                );
                vec![]
            }
            SkillUiEvent::InstructionsLoaded {
                repo: _,
                skill_path,
                content,
            } => {
                if let Some(overlays::Overlay::SkillPicker(picker)) = &mut app.overlay {
                    picker.set_instructions(&skill_path, content);
                }
                vec![]
            }
            SkillUiEvent::InstructionsFailed {
                repo: _,
                skill_path,
                error,
            } => {
                if let Some(overlays::Overlay::SkillPicker(picker)) = &mut app.overlay {
                    picker.set_instructions_error(&skill_path, error);
                }
                vec![]
            }
        },

        // Clipboard copy succeeded - show brief feedback in thread picker
        UiEvent::ClipboardCopied => {
            if let Some(overlays::Overlay::ThreadPicker(picker)) = &mut app.overlay {
                picker.copied_at = Some(std::time::Instant::now());
            }
            vec![]
        }

        // Direct bash execution events
        UiEvent::BashExecuted {
            id,
            command,
            result,
        } => {
            // Find the existing tool cell and set the result
            app.tui.transcript.set_tool_result_for(&id, result.clone());

            // Persist to thread and add to messages for LLM context
            let mut effects = vec![];
            if app.tui.thread.thread_log.is_some() {
                // Format as a user message describing what the user did
                // This makes it clear to the LLM that the USER ran the command
                let user_message = format!(
                    "[I executed a bash command]\n$ {}\n\nResult:\n{}",
                    command,
                    result.to_json_string()
                );

                // Save as user message event to thread log
                effects.push(UiEffect::SaveThread {
                    event: zdx_core::core::thread_log::ThreadEvent::user_message(&user_message),
                });

                // Add user message for LLM context
                app.tui
                    .thread
                    .messages
                    .push(zdx_core::providers::ChatMessage::user(&user_message));
            }

            effects
        }

        // Thread async result events - delegate to thread feature
        UiEvent::Thread(thread_event) => match thread_event {
            ThreadUiEvent::ListLoaded {
                threads,
                original_cells,
                mode,
            } => {
                let (mut effects, mutations, overlay_action) =
                    thread::handle_thread_event(ThreadUiEvent::ListLoaded {
                        threads,
                        original_cells,
                        mode,
                    });
                apply_mutations(&mut app.tui, mutations);

                if let thread::ThreadOverlayAction::OpenThreadPicker {
                    threads,
                    original_cells,
                    mode,
                } = overlay_action
                    && app.overlay.is_none()
                {
                    let current_thread_id =
                        app.tui.thread.thread_log.as_ref().map(|log| log.id.clone());
                    let (state, overlay_effects) = overlays::ThreadPickerState::open(
                        threads,
                        original_cells,
                        &app.tui.agent_opts.root,
                        current_thread_id,
                        mode,
                    );
                    app.overlay = Some(overlays::Overlay::ThreadPicker(state));
                    effects.extend(overlay_effects);
                }

                effects
            }
            ThreadUiEvent::ListFailed { error } => {
                let (effects, mutations, _) =
                    thread::handle_thread_event(ThreadUiEvent::ListFailed { error });
                apply_mutations(&mut app.tui, mutations);
                effects
            }
            ThreadUiEvent::Loaded {
                thread_id,
                cells,
                messages,
                history,
                thread_log,
                title,
                usage,
            } => {
                let (mut effects, mutations, overlay_action) =
                    thread::handle_thread_event(ThreadUiEvent::Loaded {
                        thread_id,
                        cells,
                        messages,
                        history,
                        thread_log,
                        title,
                        usage,
                    });
                apply_mutations(&mut app.tui, mutations);

                if let thread::ThreadOverlayAction::OpenThreadPicker {
                    threads,
                    original_cells,
                    mode,
                } = overlay_action
                    && app.overlay.is_none()
                {
                    let current_thread_id =
                        app.tui.thread.thread_log.as_ref().map(|log| log.id.clone());
                    let (state, overlay_effects) = overlays::ThreadPickerState::open(
                        threads,
                        original_cells,
                        &app.tui.agent_opts.root,
                        current_thread_id,
                        mode,
                    );
                    app.overlay = Some(overlays::Overlay::ThreadPicker(state));
                    effects.extend(overlay_effects);
                }

                effects
            }
            ThreadUiEvent::LoadFailed { error } => {
                let (effects, mutations, _) =
                    thread::handle_thread_event(ThreadUiEvent::LoadFailed { error });
                apply_mutations(&mut app.tui, mutations);
                effects
            }
            ThreadUiEvent::PreviewLoaded { cells } => {
                let allow = matches!(
                    app.overlay.as_ref(),
                    Some(overlays::Overlay::ThreadPicker(_))
                );
                if !allow {
                    vec![]
                } else {
                    let (mut effects, mutations, overlay_action) =
                        thread::handle_thread_event(ThreadUiEvent::PreviewLoaded { cells });
                    apply_mutations(&mut app.tui, mutations);

                    if let thread::ThreadOverlayAction::OpenThreadPicker {
                        threads,
                        original_cells,
                        mode,
                    } = overlay_action
                        && app.overlay.is_none()
                    {
                        let current_thread_id =
                            app.tui.thread.thread_log.as_ref().map(|log| log.id.clone());
                        let (state, overlay_effects) = overlays::ThreadPickerState::open(
                            threads,
                            original_cells,
                            &app.tui.agent_opts.root,
                            current_thread_id,
                            mode,
                        );
                        app.overlay = Some(overlays::Overlay::ThreadPicker(state));
                        effects.extend(overlay_effects);
                    }

                    effects
                }
            }
            ThreadUiEvent::PreviewFailed => {
                let allow = matches!(
                    app.overlay.as_ref(),
                    Some(overlays::Overlay::ThreadPicker(_))
                );
                if !allow {
                    vec![]
                } else {
                    let (effects, mutations, _) =
                        thread::handle_thread_event(ThreadUiEvent::PreviewFailed);
                    apply_mutations(&mut app.tui, mutations);
                    effects
                }
            }
            ThreadUiEvent::Created {
                thread_log,
                context_paths,
                skills,
            } => {
                let (mut effects, mutations, overlay_action) =
                    thread::handle_thread_event(ThreadUiEvent::Created {
                        thread_log,
                        context_paths,
                        skills,
                    });
                apply_mutations(&mut app.tui, mutations);

                if let thread::ThreadOverlayAction::OpenThreadPicker {
                    threads,
                    original_cells,
                    mode,
                } = overlay_action
                    && app.overlay.is_none()
                {
                    let current_thread_id =
                        app.tui.thread.thread_log.as_ref().map(|log| log.id.clone());
                    let (state, overlay_effects) = overlays::ThreadPickerState::open(
                        threads,
                        original_cells,
                        &app.tui.agent_opts.root,
                        current_thread_id,
                        mode,
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
                    mode,
                } = overlay_action
                    && app.overlay.is_none()
                {
                    let current_thread_id =
                        app.tui.thread.thread_log.as_ref().map(|log| log.id.clone());
                    let (state, overlay_effects) = overlays::ThreadPickerState::open(
                        threads,
                        original_cells,
                        &app.tui.agent_opts.root,
                        current_thread_id,
                        mode,
                    );
                    app.overlay = Some(overlays::Overlay::ThreadPicker(state));
                    effects.extend(overlay_effects);
                }

                effects
            }
            ThreadUiEvent::CreateFailed { error } => {
                let (effects, mutations, _) =
                    thread::handle_thread_event(ThreadUiEvent::CreateFailed { error });
                apply_mutations(&mut app.tui, mutations);
                effects
            }
            ThreadUiEvent::ForkFailed { error } => {
                let (effects, mutations, _) =
                    thread::handle_thread_event(ThreadUiEvent::ForkFailed { error });
                apply_mutations(&mut app.tui, mutations);
                effects
            }
            ThreadUiEvent::Renamed { thread_id, title } => {
                let (mut effects, mutations, overlay_action) =
                    thread::handle_thread_event(ThreadUiEvent::Renamed { thread_id, title });
                apply_mutations(&mut app.tui, mutations);

                if let thread::ThreadOverlayAction::OpenThreadPicker {
                    threads,
                    original_cells,
                    mode,
                } = overlay_action
                    && app.overlay.is_none()
                {
                    let current_thread_id =
                        app.tui.thread.thread_log.as_ref().map(|log| log.id.clone());
                    let (state, overlay_effects) = overlays::ThreadPickerState::open(
                        threads,
                        original_cells,
                        &app.tui.agent_opts.root,
                        current_thread_id,
                        mode,
                    );
                    app.overlay = Some(overlays::Overlay::ThreadPicker(state));
                    effects.extend(overlay_effects);
                }

                effects
            }
            ThreadUiEvent::TitleSuggested { thread_id, title } => {
                let is_current = app
                    .tui
                    .thread
                    .thread_log
                    .as_ref()
                    .is_some_and(|log| log.id == thread_id);
                if !is_current {
                    vec![]
                } else {
                    let (effects, mutations, _) =
                        thread::handle_thread_event(ThreadUiEvent::TitleSuggested {
                            thread_id,
                            title,
                        });
                    apply_mutations(&mut app.tui, mutations);
                    effects
                }
            }
            ThreadUiEvent::RenameFailed { error } => {
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
            StateMutation::SetLastSkillRepo(repo) => {
                tui.last_skill_repo = Some(repo);
            }
            StateMutation::ToggleDebugStatus => {
                tui.show_debug_status = !tui.show_debug_status;
            }
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
    effects: &mut Vec<UiEffect>,
    provider: zdx_core::providers::ProviderKind,
    code: String,
    verifier: String,
    redirect_uri: Option<String>,
) {
    effects.push(UiEffect::SpawnTokenExchange {
        provider,
        code,
        verifier,
        redirect_uri,
    });
}

fn apply_overlay_update(app: &mut AppState, update: overlays::OverlayUpdate) -> Vec<UiEffect> {
    let mut effects = Vec::with_capacity(update.effects.len());
    for effect in update.effects {
        match effect {
            overlays::OverlayEffect::Ui(effect) => effects.push(effect),
        }
    }
    match update.transition {
        overlays::OverlayTransition::Stay => {}
        overlays::OverlayTransition::Close => {
            if matches!(app.overlay.as_ref(), Some(overlays::Overlay::FilePicker(_))) {
                effects.push(UiEffect::CancelTask {
                    kind: TaskKind::FileDiscovery,
                    token: None,
                });
            }
            if matches!(
                app.overlay.as_ref(),
                Some(overlays::Overlay::SkillPicker(_))
            ) {
                effects.push(UiEffect::CancelTask {
                    kind: TaskKind::SkillsFetch,
                    token: None,
                });
                effects.push(UiEffect::CancelTask {
                    kind: TaskKind::SkillInstall,
                    token: None,
                });
            }
            app.overlay = None;
        }
        overlays::OverlayTransition::Open(request) => {
            if matches!(app.overlay.as_ref(), Some(overlays::Overlay::FilePicker(_))) {
                effects.push(UiEffect::CancelTask {
                    kind: TaskKind::FileDiscovery,
                    token: None,
                });
            }
            if matches!(
                app.overlay.as_ref(),
                Some(overlays::Overlay::SkillPicker(_))
            ) {
                effects.push(UiEffect::CancelTask {
                    kind: TaskKind::SkillsFetch,
                    token: None,
                });
                effects.push(UiEffect::CancelTask {
                    kind: TaskKind::SkillInstall,
                    token: None,
                });
            }
            effects.extend(open_overlay_request(app, request));
        }
    }
    effects
}

fn open_overlay_request(app: &mut AppState, request: overlays::OverlayRequest) -> Vec<UiEffect> {
    match request {
        overlays::OverlayRequest::CommandPalette => {
            let provider = zdx_core::providers::provider_for_model(&app.tui.config.model);
            let (state, effects) =
                overlays::CommandPaletteState::open(provider, app.tui.config.model.clone());
            app.overlay = Some(overlays::Overlay::CommandPalette(state));
            effects
        }
        overlays::OverlayRequest::ModelPicker => {
            let (state, effects) =
                overlays::ModelPickerState::open(&app.tui.config.model, &app.tui.config.providers);
            app.overlay = Some(overlays::Overlay::ModelPicker(state));
            effects
        }
        overlays::OverlayRequest::SkillPicker => {
            let repos = app.tui.config.skills.skill_repositories.clone();
            let last_repo = app.tui.last_skill_repo.as_deref();
            let (state, effects) = overlays::SkillPickerState::open(repos, last_repo);
            if let Some(repo) = state.current_repo() {
                app.tui.last_skill_repo = Some(repo.to_string());
            }
            app.overlay = Some(overlays::Overlay::SkillPicker(state));
            effects
        }
        overlays::OverlayRequest::ThinkingPicker => {
            if !zdx_core::models::model_supports_reasoning(&app.tui.config.model) {
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
                app.tui.transcript.cells(),
                &app.tui.transcript.scroll,
                app.tui.transcript.scroll.mode.clone(),
            );
            app.overlay = Some(overlays::Overlay::Timeline(state));
            apply_mutations(&mut app.tui, mutations);
            effects
        }
        overlays::OverlayRequest::Rename => {
            if let Some(thread_log) = &app.tui.thread.thread_log {
                let (state, effects) = overlays::RenameState::open(
                    thread_log.id.clone(),
                    None, // Current title not readily available in ThreadLog
                );
                app.overlay = Some(overlays::Overlay::Rename(state));
                effects
            } else {
                vec![]
            }
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
    let previous_width = tui.transcript.terminal_size.0;

    // Update transcript layout with current terminal dimensions
    let viewport_height = render::calculate_transcript_height_with_state(tui, height);
    tui.transcript
        .update_layout((width, height), viewport_height);

    // Apply any pending streaming text deltas (coalescing)
    transcript::apply_pending_delta(&mut tui.transcript, &mut tui.agent_state);

    // Apply accumulated scroll delta from mouse events (coalescing)
    transcript::apply_scroll_delta(&mut tui.transcript);

    // Update cell line info for lazy rendering and scroll calculations
    let width_changed = previous_width != width;
    if width_changed || tui.transcript.scroll.cell_line_info.is_empty() {
        let cell_line_counts = render::calculate_cell_line_counts(tui, width as usize);
        tui.transcript
            .scroll
            .update_cell_line_info(cell_line_counts);
    }
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
            app.tui.transcript.invalidate_line_info();
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
        let text = app.tui.input.get_text();
        let should_switch = key.code == crossterm::event::KeyCode::Char('@')
            && text.as_bytes().get(picker.trigger_pos) == Some(&b'@')
            && text.as_bytes().get(picker.trigger_pos + 1) == Some(&b'@');

        if should_switch {
            let trigger_pos = picker.trigger_pos;
            app.overlay = None;
            return vec![
                UiEffect::CancelTask {
                    kind: TaskKind::FileDiscovery,
                    token: None,
                },
                UiEffect::OpenThreadPicker {
                    mode: crate::overlays::ThreadPickerMode::Insert { trigger_pos },
                },
            ];
        }

        if picker.update_from_input(&app.tui.input) {
            app.overlay = None;
            return vec![UiEffect::CancelTask {
                kind: TaskKind::FileDiscovery,
                token: None,
            }];
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
    let ctx = input::InputContext {
        agent_state: &app.tui.agent_state,
        tasks: &app.tui.tasks,
        thread_id,
        thread_title: app.tui.thread.title.as_deref(),
        model_id: &app.tui.config.model,
    };
    let (effects, mutations, overlay_request) =
        input::handle_main_key(&mut app.tui.input, &ctx, key);
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
    use std::path::PathBuf;

    use zdx_core::core::events::AgentEvent;

    use super::*;
    use crate::transcript::{HistoryCell, ScrollMode};

    #[test]
    fn test_scroll_to_top() {
        let config = zdx_core::config::Config::default();
        let mut app = AppState::new(config, PathBuf::new(), None, None);

        app.tui.transcript.scroll_to_top();

        assert!(matches!(
            app.tui.transcript.scroll.mode,
            ScrollMode::Anchored { offset: 0 }
        ));
    }

    #[test]
    fn test_scroll_to_bottom() {
        let config = zdx_core::config::Config::default();
        let mut app = AppState::new(config, PathBuf::new(), None, None);
        app.tui.transcript.scroll_to_top(); // Start from top

        app.tui.transcript.scroll_to_bottom();

        assert!(matches!(
            app.tui.transcript.scroll.mode,
            ScrollMode::FollowLatest
        ));
    }

    #[test]
    fn test_scroll_up_and_down() {
        let config = zdx_core::config::Config::default();
        let mut app = AppState::new(config, PathBuf::new(), None, None);
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
    fn test_apply_scroll_delta_with_acceleration() {
        let config = zdx_core::config::Config::default();
        let mut app = AppState::new(config, PathBuf::new(), None, None);
        app.tui.transcript.scroll.update_line_count(100);
        app.tui.transcript.viewport_height = 20;

        // First frame: multiple events accumulated, but acceleration starts at 1
        app.tui.transcript.scroll_accumulator.accumulate(-1);
        app.tui.transcript.scroll_accumulator.accumulate(-1);
        app.tui.transcript.scroll_accumulator.accumulate(-1);

        // Apply scrolls 1 line (acceleration starting point)
        transcript::apply_scroll_delta(&mut app.tui.transcript);

        // Should be anchored at offset 79 (80 - 1)
        assert!(matches!(
            app.tui.transcript.scroll.mode,
            ScrollMode::Anchored { offset: 79 }
        ));
    }

    #[test]
    fn test_queue_drains_on_turn_completed() {
        let config = zdx_core::config::Config::default();
        let mut app = AppState::new(config, PathBuf::new(), None, None);
        app.tui.input.enqueue_prompt("queued prompt".to_string());

        let effects = update(
            &mut app,
            UiEvent::Agent(AgentEvent::TurnCompleted {
                final_text: String::new(),
                messages: Vec::new(),
            }),
        );

        assert!(
            effects
                .iter()
                .any(|effect| matches!(effect, UiEffect::StartAgentTurn))
        );
        assert!(!app.tui.input.has_queued());
        let last_cell = app.tui.transcript.cells().last().expect("cell");
        assert!(matches!(
            last_cell,
            HistoryCell::User { content, .. } if content == "queued prompt"
        ));
    }
}
