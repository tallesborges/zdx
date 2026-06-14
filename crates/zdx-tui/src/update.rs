#![allow(clippy::too_many_lines)]
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
use crate::input::{HandoffState, PromptBuilderState};
use crate::mutations::{ConfigMutation, InputMutation, StateMutation, TranscriptMutation};
use crate::overlays::{self, FilePickerState, Overlay};
use crate::state::{AgentState, AppState, TabId, TabKind, TuiState};
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
            // Also coalesce background tab deltas
            for tab in &mut app.background_tabs {
                transcript::apply_pending_delta(&mut tab.transcript, &mut tab.agent_state);
            }
            // Refresh the terminal title for the active pane, deduped so it only
            // writes when the value actually changes (spinner frame, title, or
            // run state). This single path covers streaming animation, idle,
            // async title arrival, and tab switches.
            let mut effects = Vec::new();
            if let Some(value) = term_title_value(
                app.tui.config.notifications.osc,
                app.tui.agent_state.is_running(),
                app.tui.thread.title.as_deref(),
                app.tui.spinner_frame,
            ) && app.last_term_title.as_deref() != Some(value.as_str())
            {
                app.last_term_title = Some(value.clone());
                effects.push(UiEffect::SetTermTitle { value });
            }
            // Animate / refresh the cmux status pill for the active pane. Driven
            // from the tick loop (deduped against `last_cmux_status`) so it stays
            // correct across streaming, idle, async title arrival, and tab
            // switches. Each write spawns a `cmux` process, hence the slower
            // spinner cadence; a `None` result clears the pill.
            match cmux_pill_value(
                app.tui.config.notifications.cmux_status,
                app.tui.agent_state.is_running(),
                app.tui.last_turn_outcome,
                app.tui.thread.title.as_deref(),
                app.tui.spinner_frame,
            ) {
                Some(value) if app.last_cmux_status.as_deref() != Some(value.as_str()) => {
                    app.last_cmux_status = Some(value.clone());
                    effects.push(UiEffect::CmuxStatus { value });
                }
                None if app.last_cmux_status.is_some()
                    && app.tui.config.notifications.cmux_status =>
                {
                    app.last_cmux_status = None;
                    effects.push(UiEffect::CmuxStatusClear);
                }
                _ => {}
            }
            effects
        }
        UiEvent::Frame { width, height } => {
            let tab_bar_height = u16::from(app.tab_count() > 1);
            handle_frame(&mut app.tui, width, height, tab_bar_height);
            vec![]
        }
        UiEvent::Terminal(term_event) => handle_terminal_event(app, term_event),
        UiEvent::Agent(agent_event) => handle_agent_event(app, &agent_event),
        UiEvent::AgentSpawned {
            rx,
            cancel,
            thread_handle,
            messages,
        } => {
            // For btw tabs: apply thread state from first-send preparation
            if let Some(handle) = thread_handle {
                app.tui.mark_thread_running(handle.id.clone());
                app.tui.thread.thread_handle = Some(handle);
            } else if let Some(thread_id) = app
                .tui
                .thread
                .thread_handle
                .as_ref()
                .map(|thread| thread.id.clone())
            {
                app.tui.mark_thread_running(thread_id);
            }
            if let Some(msgs) = messages {
                app.tui.thread.messages = msgs;
            }
            app.tui.agent_state = AgentState::Waiting { rx, cancel };
            app.tui.transcript.activate_pending_user_cell();
            app.tui.status_line.start_turn();
            vec![]
        }
        UiEvent::BackgroundTabAgent { tab_id, event } => {
            // Suppress the ask_user_question marker on background tabs: it must
            // not pollute the tool-output cell, and the picker only opens for
            // the active tab (the pending question still answers via typing
            // once the user switches to that tab).
            if is_ask_user_marker(&event) {
                return vec![];
            }
            if let Some(tab) = app.background_tab_mut(tab_id) {
                let has_thread = tab.thread.thread_handle.is_some();
                let (_inner_effects, mutations) = transcript::handle_agent_event(
                    &mut tab.transcript,
                    &mut tab.agent_state,
                    has_thread,
                    &event,
                );
                apply_tab_mutations(tab, mutations);

                let mut effects = Vec::new();
                finalize_agent_event_for_tab(
                    tab,
                    &event,
                    input::TabContext::Background(tab_id),
                    &mut effects,
                );
                effects
            } else {
                vec![]
            }
        }
        UiEvent::BackgroundTabAgentSpawned {
            tab_id,
            rx,
            cancel,
            thread_handle,
            messages,
        } => {
            if let Some(tab) = app.background_tab_mut(tab_id) {
                if let Some(handle) = thread_handle {
                    tab.mark_thread_running(handle.id.clone());
                    tab.thread.thread_handle = Some(handle);
                } else if let Some(thread_id) = tab
                    .thread
                    .thread_handle
                    .as_ref()
                    .map(|thread| thread.id.clone())
                {
                    tab.mark_thread_running(thread_id);
                }
                if let Some(msgs) = messages {
                    tab.thread.messages = msgs;
                }
                tab.agent_state = AgentState::Waiting { rx, cancel };
                tab.transcript.activate_pending_user_cell();
                tab.status_line.start_turn();
            }
            vec![]
        }
        UiEvent::LoginResult { result } => handle_login_result_event(app, result),
        UiEvent::LoginCallbackResult(code) => handle_login_callback_result(app, code),
        UiEvent::TaskStarted { kind, started } => handle_task_started_event(app, kind, &started),
        UiEvent::TaskCompleted { kind, completed } => {
            let ok = {
                let state = app.tui.tasks.state_mut(kind);
                state.finish_if_active(completed.id)
            };
            if ok {
                update(app, *completed.result)
            } else {
                vec![]
            }
        }
        UiEvent::HandoffResult {
            next_message,
            result,
        } => {
            let mutations = input::handle_handoff_result(&mut app.tui.input, &next_message, result);
            apply_mutations(&mut app.tui, mutations);
            vec![]
        }
        UiEvent::PromptBuilderResult { intent, result } => {
            let mutations =
                input::handle_prompt_builder_result(&mut app.tui.input, &intent, result);
            apply_mutations(&mut app.tui, mutations);
            vec![]
        }
        UiEvent::FilesDiscovered(files) => {
            overlays::handle_files_discovered(&mut app.overlay, files);
            vec![]
        }

        UiEvent::Skill(skill_event) => handle_skill_event(app, skill_event),

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
        } => handle_bash_executed_event(app, &id, &command, &result),
        UiEvent::RootDisplayResolved {
            path,
            git_branch,
            display_path,
        } => {
            apply_mutations(
                &mut app.tui,
                vec![StateMutation::SetRootDisplay {
                    path,
                    git_branch,
                    display_path,
                }],
            );
            vec![]
        }
        UiEvent::SystemPromptRefreshed { result } => handle_system_prompt_refreshed(app, result),

        // Thread async result events - delegate to thread feature
        UiEvent::Thread(thread_event) => handle_thread_ui_event(app, thread_event),
        UiEvent::ImagePreviewDecoded { result } => {
            if let Some(overlays::Overlay::ImagePreview(state)) = &mut app.overlay {
                match result {
                    Ok(data) => state.set_image_data(data.base64_png, data.width, data.height),
                    Err(e) => state.set_error(e),
                }
            }
            vec![]
        }
        UiEvent::VoiceRecorded { result } => handle_voice_recorded(app, result),
        UiEvent::VoiceTranscribed { result } => handle_voice_transcribed(app, result),
        UiEvent::TldrResult { thread_id, result } => {
            if let Some(overlays::Overlay::Tldr(state)) = &mut app.overlay
                && state.thread_id == thread_id
            {
                match result {
                    Ok(text) => state.set_ready(text),
                    Err(message) => state.set_error(message),
                }
            }
            // If the overlay was closed or switched threads, drop the result silently.
            vec![]
        }
        UiEvent::ContextResult { result } => {
            if let Some(overlays::Overlay::Context(state)) = &mut app.overlay {
                match result {
                    Ok(report) => state.set_ready(report),
                    Err(message) => state.set_error(message),
                }
            }
            // If the overlay was closed in the meantime, drop the result silently.
            vec![]
        }
    }
}

fn handle_voice_recorded(
    app: &mut AppState,
    result: Result<crate::events::RecordedAudio, String>,
) -> Vec<UiEffect> {
    if app.tui.input.voice.discard_next_capture {
        app.tui.input.voice.mark_idle();
        return vec![];
    }

    match result {
        Ok(audio) => vec![UiEffect::StartVoiceTranscription { audio }],
        Err(error) => {
            let message = format!("Voice recording failed: {error}");
            app.tui.input.voice.mark_error(message.clone());
            apply_mutations(
                &mut app.tui,
                vec![StateMutation::Transcript(
                    TranscriptMutation::AppendSystemMessage(message),
                )],
            );
            vec![]
        }
    }
}

fn handle_voice_transcribed(
    app: &mut AppState,
    result: Result<Option<String>, String>,
) -> Vec<UiEffect> {
    match result {
        Ok(Some(text)) => {
            app.tui.input.voice.mark_idle();
            apply_mutations(
                &mut app.tui,
                vec![StateMutation::Input(InputMutation::InsertText(text))],
            );
        }
        Ok(None) => {
            app.tui.input.voice.mark_idle();
            apply_mutations(
                &mut app.tui,
                vec![StateMutation::Transcript(
                    TranscriptMutation::AppendSystemMessage(
                        "Voice transcription unavailable. Configure an OpenAI or Mistral transcription provider (or set ZDX_TRANSCRIPTION_MODEL).".to_string(),
                    ),
                )],
            );
        }
        Err(error) => {
            let message = format!("Voice transcription failed: {error}");
            app.tui.input.voice.mark_error(message.clone());
            apply_mutations(
                &mut app.tui,
                vec![StateMutation::Transcript(
                    TranscriptMutation::AppendSystemMessage(message),
                )],
            );
        }
    }
    vec![]
}

/// Handles `ask_user_question` lifecycle events for the active tab.
///
/// Returns `true` when the event was fully consumed (the `REGISTERED_MARKER`
/// delta) and must not reach the transcript. Close-on-complete and
/// close-on-turn-end run as side effects but let normal handling proceed.
fn intercept_ask_user_event(
    app: &mut AppState,
    agent_event: &zdx_engine::core::events::AgentEvent,
) -> bool {
    use zdx_engine::core::events::AgentEvent;

    match agent_event {
        AgentEvent::ToolOutputDelta { id, chunk }
            if chunk == zdx_engine::tools::ask_user_question::REGISTERED_MARKER =>
        {
            // Don't steal the keyboard: instead of auto-opening the picker,
            // show the question inline so the user can type or dictate an
            // answer. Ctrl+F opens the picker on demand.
            let thread_id = app.tui.thread.thread_handle.as_ref().map(|t| t.id.clone());
            if let Some(thread_id) = thread_id
                && let Some(view) = crate::ask_user::pending_view(&app.tui.ask_user_map, &thread_id)
                && view.tool_use_id == *id
            {
                app.tui
                    .transcript
                    .push_cell(crate::transcript::HistoryCell::system(format_question(
                        &view,
                    )));
            }
            true
        }
        AgentEvent::ToolCompleted { id, .. } => {
            close_question_picker_if(app, |p| p.tool_use_id() == id);
            false
        }
        AgentEvent::TurnFinished { .. } => {
            close_question_picker_if(app, |_| true);
            false
        }
        _ => false,
    }
}

/// Closes the question picker overlay when one is open and `pred` matches.
fn close_question_picker_if(
    app: &mut AppState,
    pred: impl FnOnce(&overlays::QuestionPickerState) -> bool,
) {
    if let Some(overlays::Overlay::QuestionPicker(p)) = app.overlay.as_ref()
        && pred(p)
    {
        app.overlay = None;
    }
}

/// Formats a pending question + options into an inline system cell body.
fn format_question(view: &crate::ask_user::QuestionView) -> String {
    use std::fmt::Write;
    let mut out = format!("❓ {}", view.question);
    for (idx, opt) in view.options.iter().enumerate() {
        if opt.description.trim().is_empty() {
            let _ = write!(out, "\n  {}. {}", idx + 1, opt.label);
        } else {
            let _ = write!(
                out,
                "\n  {}. {} — {}",
                idx + 1,
                opt.label,
                opt.description.trim()
            );
        }
    }
    out.push_str("\n(type or dictate your answer, or press Ctrl+F to pick)");
    out
}

/// Whether an event is the `ask_user_question` registration marker.
fn is_ask_user_marker(event: &zdx_engine::core::events::AgentEvent) -> bool {
    matches!(
        event,
        zdx_engine::core::events::AgentEvent::ToolOutputDelta { chunk, .. }
            if chunk == zdx_engine::tools::ask_user_question::REGISTERED_MARKER
    )
}

fn handle_agent_event(
    app: &mut AppState,
    agent_event: &zdx_engine::core::events::AgentEvent,
) -> Vec<UiEffect> {
    // ask_user_question lifecycle: open the picker once the question is
    // registered (REGISTERED_MARKER), and close it when the question
    // completes or the turn ends. The marker delta is suppressed so it never
    // pollutes the tool-output cell.
    if intercept_ask_user_event(app, agent_event) {
        return vec![];
    }

    let has_thread = app.tui.thread.thread_handle.is_some();
    let (mut effects, mutations) = transcript::handle_agent_event(
        &mut app.tui.transcript,
        &mut app.tui.agent_state,
        has_thread,
        agent_event,
    );
    apply_mutations(&mut app.tui, mutations);

    finalize_agent_event_for_tab(
        &mut app.tui,
        agent_event,
        input::TabContext::Active,
        &mut effects,
    );

    effects
}

/// Shared end-of-turn handling for both the active tab (`UiEvent::Agent`)
/// and background tabs (`UiEvent::BackgroundTabAgent`).
///
/// Marks tool usage on `ToolRequested`, marks the thread as finished on
/// `TurnFinished`, optionally pushes a timing cell, and drains the next
/// queued prompt for that tab when appropriate. Without this helper the
/// background-tab path silently dropped both the timing cell and any
/// queued prompts.
fn finalize_agent_event_for_tab(
    tui: &mut crate::state::TuiState,
    agent_event: &zdx_engine::core::events::AgentEvent,
    tab: input::TabContext,
    effects: &mut Vec<UiEffect>,
) {
    use zdx_engine::core::events::AgentEvent;

    if matches!(agent_event, AgentEvent::ToolRequested { .. }) {
        tui.status_line.mark_tool_used();
    }

    // cmux progress bar reflects only the active tab (one per pane). The status
    // pill is entirely tick-driven; here `TurnFinished` only records
    // `last_turn_outcome` so the next tick can render the idle pill.
    let cmux = tui.config.notifications.cmux_status && matches!(tab, input::TabContext::Active);
    if cmux
        && let AgentEvent::ToolCompleted { result, .. } = agent_event
        && let Some((value, label)) = todo_progress_from_output(result)
    {
        effects.push(UiEffect::CmuxProgress { value, label });
    }

    let should_dequeue = matches!(agent_event, AgentEvent::TurnFinished { .. });

    if should_dequeue
        && let Some(thread_id) = tui
            .thread
            .thread_handle
            .as_ref()
            .map(|thread| thread.id.clone())
    {
        tui.mark_thread_finished(&thread_id);
    }

    maybe_push_timing_cell_for_tab(tui, should_dequeue);
    let continues = maybe_send_next_queued_prompt_for_tab(tui, should_dequeue, tab, effects);

    if let AgentEvent::TurnFinished { status, .. } = agent_event {
        use zdx_engine::core::events::TurnStatus;

        use crate::state::TurnOutcome;

        // Remember the outcome so the idle cmux pill (driven from the tick loop)
        // can render it for this tab, including after a later tab switch.
        tui.last_turn_outcome = Some(match status {
            TurnStatus::Failed { .. } => TurnOutcome::Failed,
            TurnStatus::Completed | TurnStatus::Interrupted => TurnOutcome::Succeeded,
        });

        if !continues {
            if cmux {
                effects.push(UiEffect::CmuxProgressClear);
            }
            if let Some(ok) = turn_notification_outcome(status) {
                effects.push(UiEffect::NotifyTurnEnd { ok });
            }
        }
    }
}

/// Spinner glyphs for the animated terminal title and cmux status pill (circle
/// glyphs, matching the in-app spinner — braille renders unevenly across
/// terminal tab fonts).
const SPINNER_GLYPHS: &[&str] = &["◐", "◓", "◑", "◒"];

/// Render-frame divisor for the cmux status-pill spinner. Coarser than the
/// in-app spinner (`SPINNER_SPEED_DIVISOR`) because each cmux update spawns a
/// `cmux` subprocess, so we trade smoothness for far fewer spawns (~4/s vs
/// ~10/s).
const CMUX_SPINNER_SPEED_DIVISOR: usize = 16;

/// Current spinner glyph for a frame counter, advancing one glyph every
/// `divisor` ticks.
fn spinner_glyph(spinner_frame: usize, divisor: usize) -> &'static str {
    SPINNER_GLYPHS[(spinner_frame / divisor) % SPINNER_GLYPHS.len()]
}

/// Trims a title and returns `None` when it is empty, so callers can apply a
/// fallback or skip the segment.
fn non_empty_trimmed(text: Option<&str>) -> Option<&str> {
    text.map(str::trim).filter(|t| !t.is_empty())
}

/// Computes the terminal window/tab title for the active pane: the thread
/// `title` (or `zdx` before one exists), prefixed with an animated spinner
/// frame while a turn is `running`. Returns `None` when OSC integration is off,
/// leaving the terminal title untouched.
fn term_title_value(
    osc: bool,
    running: bool,
    title: Option<&str>,
    spinner_frame: usize,
) -> Option<String> {
    if !osc {
        return None;
    }
    let base = non_empty_trimmed(title).map_or_else(
        || "zdx".to_string(),
        |t| crate::common::truncate_with_ellipsis(t, 40),
    );
    if running {
        Some(format!(
            "{} {base}",
            spinner_glyph(spinner_frame, crate::transcript::SPINNER_SPEED_DIVISOR)
        ))
    } else {
        Some(base)
    }
}

/// Computes the cmux status-pill value for the active pane. While a turn runs it
/// animates a spinner glyph alongside the thread title (falling back to `zdx`
/// before a title exists); when idle it reflects the last turn's `outcome`
/// (bare title on success, `✗` on failure). Returns `None` when the cmux
/// integration is disabled or there is nothing to show (no turn yet, or a
/// completed turn with no title) — the caller clears the pill in that case.
/// This single path keeps the pill correct across streaming, idle, async title
/// arrival, and tab switches.
fn cmux_pill_value(
    cmux_enabled: bool,
    running: bool,
    outcome: Option<crate::state::TurnOutcome>,
    title: Option<&str>,
    spinner_frame: usize,
) -> Option<String> {
    use crate::state::TurnOutcome;

    if !cmux_enabled {
        return None;
    }
    if running {
        // While running, always show an identifiable pill — fall back to `zdx`
        // until the thread title is generated.
        let base = non_empty_trimmed(title).unwrap_or("zdx");
        return Some(cmux_status_value(
            spinner_glyph(spinner_frame, CMUX_SPINNER_SPEED_DIVISOR),
            Some(base),
        ));
    }
    let status = match outcome? {
        TurnOutcome::Succeeded => "",
        TurnOutcome::Failed => "✗",
    };
    let value = cmux_status_value(status, title);
    (!value.is_empty()).then_some(value)
}

/// Builds the cmux status-pill value: an optional status prefix (a spinner glyph
/// while running, `✗` on failure, or empty when complete) joined to the thread
/// title with ` · `. No `zdx` prefix — the pill is already per-instance in the
/// cmux sidebar.
fn cmux_status_value(status: &str, title: Option<&str>) -> String {
    let title = non_empty_trimmed(title).map(|t| crate::common::truncate_with_ellipsis(t, 32));
    match (status.is_empty(), title) {
        (true, Some(t)) => t,
        (true, None) => String::new(),
        (false, Some(t)) => format!("{status} · {t}"),
        (false, None) => status.to_string(),
    }
}

/// Extracts `(progress, label)` from a `todo_write` tool result for the cmux
/// progress bar. Returns `None` for any other tool. `progress` is
/// completed/total; `label` is the active todo's content or a `done/total`
/// fallback.
fn todo_progress_from_output(
    result: &zdx_engine::core::events::ToolOutput,
) -> Option<(f64, String)> {
    let data = result.data()?;
    let reminder = data.get("reminder").and_then(|r| r.as_str())?;
    if !reminder.contains("Todo_Write") {
        return None;
    }
    let counts = data.get("counts")?;
    let total = counts.get("total").and_then(serde_json::Value::as_u64)?;
    if total == 0 {
        return None;
    }
    let completed = counts
        .get("completed")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);
    let active_content = data
        .get("todos")
        .and_then(|t| t.as_array())
        .and_then(|todos| {
            todos
                .iter()
                .find(|todo| todo.get("status").and_then(|s| s.as_str()) == Some("in_progress"))
        })
        .and_then(|todo| todo.get("content").and_then(|c| c.as_str()));
    let label = active_content.map_or_else(
        || format!("{completed}/{total} done"),
        |content| crate::common::truncate_with_ellipsis(content, 40),
    );
    Some((completed as f64 / total as f64, label))
}

/// `Some(true)` completed, `Some(false)` failed, `None` interrupted (no
/// notification — the user is already present).
fn turn_notification_outcome(status: &zdx_engine::core::events::TurnStatus) -> Option<bool> {
    use zdx_engine::core::events::TurnStatus;
    match status {
        TurnStatus::Completed => Some(true),
        TurnStatus::Failed { .. } => Some(false),
        TurnStatus::Interrupted => None,
    }
}

fn maybe_push_timing_cell_for_tab(tui: &mut crate::state::TuiState, should_dequeue: bool) {
    if should_dequeue
        && let Some((duration, tool_count)) = tui.status_line.end_turn()
        && duration.as_secs_f64() >= 1.0
    {
        tui.transcript
            .push_cell(HistoryCell::timing(duration, tool_count));
    }
}

/// Drains the next queued prompt for `tab` when the turn just ended and the
/// tab is idle. Returns `true` when the turn effectively continues (a queued
/// prompt was dispatched or the tab is still busy), so the caller suppresses
/// the turn-end notification.
fn maybe_send_next_queued_prompt_for_tab(
    tui: &mut crate::state::TuiState,
    should_dequeue: bool,
    tab: input::TabContext,
    effects: &mut Vec<UiEffect>,
) -> bool {
    if !should_dequeue {
        return false;
    }

    if tui.agent_state.is_running()
        || tui.tasks.state(TaskKind::Bash).is_running()
        || tui.transcript.has_pending_user_cell()
    {
        return true;
    }

    let Some(text) = tui.input.pop_queued_prompt() else {
        return false;
    };

    let thread_id = tui.thread.thread_handle.as_ref().map(|log| log.id.clone());
    // Title suggestion only fires for the active tab — see
    // `build_send_effects_for_tab` for the rationale.
    let should_suggest_title = matches!(tab, input::TabContext::Active)
        && thread_id.is_some()
        && tui.thread.title.is_none()
        && !tui.tasks.state(TaskKind::ThreadTitle).is_running();
    let (queue_effects, queue_mutations) =
        input::build_send_effects_for_tab(&text, thread_id, should_suggest_title, vec![], tab);
    apply_tab_mutations(tui, queue_mutations);
    effects.extend(queue_effects);
    true
}

/// Applies cross-slice state mutations to a single `TuiState` (active or
/// background tab). Mirrors `apply_mutations` for `AppState`, but works on
/// any tab so background-tab queue draining can apply transcript/thread/
/// input changes to the right tab.
fn apply_tab_mutations(tui: &mut crate::state::TuiState, mutations: Vec<StateMutation>) {
    for mutation in mutations {
        match mutation {
            StateMutation::Transcript(m) => tui.transcript.apply(m),
            StateMutation::Thread(m) => tui.thread.apply(m),
            StateMutation::Input(m) => tui.input.apply(m),
            // Per-tab state: must apply to the owning tab, not just the active one.
            StateMutation::SetLastFollowups(items) => tui.last_followups = items,
            StateMutation::Auth(_)
            | StateMutation::Config(_)
            | StateMutation::SetRootDisplay { .. }
            | StateMutation::SetActiveThreadOverrides { .. }
            | StateMutation::SetSystemPrompt(_)
            | StateMutation::SetLastSkillRepo(_)
            | StateMutation::SetLoadedSkills(_)
            | StateMutation::ToggleDebugStatus => {
                // App-level mutations never originate from a queued-prompt
                // drain or transcript event; ignored here so the helper
                // can stay focused on per-tab slices.
            }
        }
    }
}

fn handle_login_result_event(app: &mut AppState, result: Result<(), String>) -> Vec<UiEffect> {
    let provider = match &app.overlay {
        Some(overlays::Overlay::Login(overlays::LoginState::Exchanging { provider })) => *provider,
        Some(overlays::Overlay::Login(overlays::LoginState::AwaitingCode { provider, .. })) => {
            *provider
        }
        _ => zdx_engine::providers::provider_for_model(&app.tui.config.model),
    };
    let (mutations, overlay_action) =
        auth::handle_login_result(&mut app.tui.auth, result, provider);
    apply_mutations(&mut app.tui, mutations);

    match overlay_action {
        auth::LoginOverlayAction::Close => app.overlay = None,
        auth::LoginOverlayAction::Reopen { error } => {
            app.overlay = Some(overlays::Overlay::Login(overlays::LoginState::reopen(
                provider, error,
            )));
        }
    }
    vec![]
}

fn handle_login_callback_result(app: &mut AppState, code: Option<String>) -> Vec<UiEffect> {
    let mut effects = Vec::new();
    if let Some(overlays::Overlay::Login(login_state)) = &mut app.overlay {
        process_login_callback(login_state, code, &mut effects);
    }
    effects
}

fn process_login_callback(
    login_state: &mut overlays::LoginState,
    code: Option<String>,
    effects: &mut Vec<UiEffect>,
) {
    if let overlays::LoginState::AwaitingCode {
        provider,
        pkce_verifier,
        oauth_state,
        redirect_uri,
        error,
        ..
    } = login_state
        && matches!(
            *provider,
            zdx_engine::providers::ProviderKind::ClaudeCli
                | zdx_engine::providers::ProviderKind::OpenAICodex
                | zdx_engine::providers::ProviderKind::GeminiCli
                | zdx_engine::providers::ProviderKind::GoogleAntigravity
        )
    {
        match code {
            Some(code) => {
                *error = None;
                let verifier = pkce_verifier.clone();
                let provider = *provider;
                let code = if provider == zdx_engine::providers::ProviderKind::ClaudeCli {
                    let state = oauth_state.clone().unwrap_or_else(|| verifier.clone());
                    format!("{code}#{state}")
                } else {
                    code
                };
                let redirect_uri = if provider == zdx_engine::providers::ProviderKind::ClaudeCli {
                    redirect_uri.clone()
                } else {
                    None
                };
                *login_state = overlays::LoginState::Exchanging { provider };
                push_token_exchange(effects, provider, code, verifier, redirect_uri);
            }
            None => {
                *error = Some("Local login timed out. Paste the code or URL.".to_string());
            }
        }
    }
}

fn handle_task_started_event(
    app: &mut AppState,
    kind: TaskKind,
    started: &crate::common::TaskStarted,
) -> Vec<UiEffect> {
    app.tui.tasks.state_mut(kind).on_started(started);
    match kind {
        TaskKind::Handoff => {
            if matches!(&started.meta, TaskMeta::Handoff { .. }) {
                app.tui.input.handoff = HandoffState::Generating;
            }
        }
        TaskKind::PromptBuilder => {
            if let TaskMeta::PromptBuilder { intent } = &started.meta {
                app.tui.input.prompt_builder = PromptBuilderState::Generating {
                    intent: intent.clone(),
                };
            }
        }
        TaskKind::Bash => {
            if let TaskMeta::Bash { id, command } = &started.meta {
                let input = serde_json::json!({ "command": command });
                app.tui
                    .transcript
                    .push_cell(HistoryCell::tool_running(id, "bash", input));
            }
        }
        TaskKind::VoiceRecord => app.tui.input.voice.start_recording(),
        TaskKind::VoiceTranscribe => app.tui.input.voice.start_transcribing(),
        TaskKind::FileDiscovery
        | TaskKind::SkillsFetch
        | TaskKind::SkillInstall
        | TaskKind::ThreadList
        | TaskKind::ThreadLoad
        | TaskKind::ThreadRename
        | TaskKind::ThreadTitle
        | TaskKind::ThreadTldr
        | TaskKind::ContextAnalyze
        | TaskKind::ThreadPreview
        | TaskKind::ThreadCreate
        | TaskKind::ThreadFork
        | TaskKind::ThreadWorktree
        | TaskKind::LoginExchange
        | TaskKind::LoginCallback
        | TaskKind::ImageDecode => {}
    }
    vec![]
}

fn handle_skill_event(app: &mut AppState, skill_event: SkillUiEvent) -> Vec<UiEffect> {
    match skill_event {
        SkillUiEvent::ListLoaded { repo, skills } => {
            if let Some(overlays::Overlay::SkillPicker(picker)) = &mut app.overlay {
                let items = skills
                    .into_iter()
                    .map(|skill| overlays::skill_picker::SkillItem {
                        name: skill.name,
                        path: skill.path,
                        description: skill.description,
                        source: None,
                    })
                    .collect();
                picker.set_skills(&repo, items);
            }
        }
        SkillUiEvent::ListFailed { repo, error } => {
            if let Some(overlays::Overlay::SkillPicker(picker)) = &mut app.overlay {
                picker.set_error(&repo, format!("Failed to load skills: {error}"));
            }
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
                        "Installed skill \"{skill}\". Restart ZDX to pick up new skills."
                    )),
                )],
            );
        }
        SkillUiEvent::InstallFailed { repo, skill, error } => {
            if let Some(overlays::Overlay::SkillPicker(picker)) = &mut app.overlay {
                picker.set_installing(None);
                picker.set_error(&repo, format!("Install failed: {error}"));
            }
            apply_mutations(
                &mut app.tui,
                vec![StateMutation::Transcript(
                    TranscriptMutation::AppendSystemMessage(format!(
                        "Failed to install {skill}: {error}"
                    )),
                )],
            );
        }
        SkillUiEvent::InstructionsLoaded {
            repo: _,
            skill_path,
            content,
        } => {
            if let Some(overlays::Overlay::SkillPicker(picker)) = &mut app.overlay {
                picker.set_instructions(&skill_path, content);
            }
        }
        SkillUiEvent::InstructionsFailed {
            repo: _,
            skill_path,
            error,
        } => {
            if let Some(overlays::Overlay::SkillPicker(picker)) = &mut app.overlay {
                picker.set_instructions_error(&skill_path, error);
            }
        }
    }
    vec![]
}

fn handle_bash_executed_event(
    app: &mut AppState,
    id: &str,
    command: &str,
    result: &zdx_engine::core::events::ToolOutput,
) -> Vec<UiEffect> {
    app.tui.transcript.set_tool_result_for(id, result.clone());
    if app.tui.thread.thread_handle.is_none() {
        return vec![];
    }

    let user_message = format!(
        "[I executed a bash command]\n$ {}\n\nResult:\n{}",
        command,
        result.to_json_string()
    );
    app.tui
        .thread
        .messages
        .push(zdx_engine::providers::ChatMessage::user(&user_message));
    vec![UiEffect::SaveThread {
        event: zdx_engine::core::thread_persistence::ThreadEvent::user_message(&user_message),
    }]
}

fn handle_system_prompt_refreshed(
    app: &mut AppState,
    result: Result<Option<String>, String>,
) -> Vec<UiEffect> {
    let mutation = match result {
        Ok(prompt) => StateMutation::SetSystemPrompt(prompt),
        Err(error) => StateMutation::Transcript(TranscriptMutation::AppendSystemMessage(error)),
    };
    apply_mutations(&mut app.tui, vec![mutation]);
    vec![]
}

fn handle_thread_ui_event(app: &mut AppState, thread_event: ThreadUiEvent) -> Vec<UiEffect> {
    let preview_target = match app.overlay.as_ref() {
        Some(overlays::Overlay::ThreadPicker(picker)) => {
            picker.selected_thread().map(|thread| thread.id.clone())
        }
        _ => None,
    };
    match thread_event {
        ThreadUiEvent::PreviewLoaded { thread_id, .. }
            if preview_target.as_deref() != Some(thread_id.as_str()) =>
        {
            vec![]
        }
        ThreadUiEvent::PreviewFailed { thread_id }
            if preview_target.as_deref() != Some(thread_id.as_str()) =>
        {
            vec![]
        }
        ThreadUiEvent::TitleSuggested { thread_id, .. }
            if app
                .tui
                .thread
                .thread_handle
                .as_ref()
                .is_none_or(|log| log.id != thread_id) =>
        {
            vec![]
        }
        ThreadUiEvent::OpenAsTab {
            cells,
            messages,
            history,
            thread_handle,
            title,
            model_override,
            thinking_override,
            usage,
            user_input,
        } => {
            let tab_id = app.next_tab_id();
            let tab = create_thread_tab(
                tab_id,
                cells,
                messages,
                history,
                thread_handle,
                title.as_ref(),
                model_override.as_ref(),
                thinking_override,
                usage,
                user_input.as_deref(),
                &app.tui,
            );
            app.overlay = None; // Close thread picker / timeline
            app.push_tab(tab);
            vec![]
        }
        event => {
            let (mut effects, mutations, overlay_action) = thread::handle_thread_event(event);
            apply_mutations(&mut app.tui, mutations);
            maybe_open_thread_picker_overlay(app, overlay_action, &mut effects);
            effects
        }
    }
}

fn maybe_open_thread_picker_overlay(
    app: &mut AppState,
    overlay_action: thread::ThreadOverlayAction,
    effects: &mut Vec<UiEffect>,
) {
    if let thread::ThreadOverlayAction::OpenThreadPicker {
        threads,
        mut active_thread_ids,
        original_cells,
        mode,
    } = overlay_action
        && app.overlay.is_none()
    {
        active_thread_ids.extend(app.tui.snapshot_active_thread_ids());
        let current_thread_id = app
            .tui
            .thread
            .thread_handle
            .as_ref()
            .map(|log| log.id.clone());
        let (state, overlay_effects) = overlays::ThreadPickerState::open(
            threads,
            active_thread_ids,
            original_cells,
            &app.tui.agent_opts.root,
            current_thread_id,
            mode,
        );
        app.overlay = Some(overlays::Overlay::ThreadPicker(state));
        effects.extend(overlay_effects);
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
            StateMutation::Auth(mutation) => tui.auth.apply(&mutation),
            StateMutation::Config(mutation) => apply_config_mutation(tui, mutation),
            StateMutation::SetRootDisplay {
                path,
                git_branch,
                display_path,
            } => {
                tui.agent_opts.root = path;
                tui.git_branch = git_branch;
                tui.display_path = display_path;
            }
            StateMutation::SetActiveThreadOverrides {
                model_override,
                thinking_override,
            } => {
                tui.config.model = model_override.unwrap_or_else(|| tui.base_model.clone());
                tui.config.thinking_level = thinking_override.unwrap_or(tui.base_thinking_level);
            }
            StateMutation::SetSystemPrompt(system_prompt) => {
                tui.system_prompt = system_prompt;
            }
            StateMutation::SetLastSkillRepo(repo) => {
                tui.last_skill_repo = Some(repo);
            }
            StateMutation::SetLoadedSkills(skills) => {
                tui.loaded_skills = skills;
            }
            StateMutation::SetLastFollowups(items) => {
                tui.last_followups = items;
            }
            StateMutation::ToggleDebugStatus => {
                tui.show_debug_status = !tui.show_debug_status;
            }
        }
    }
}

fn apply_config_mutation(tui: &mut TuiState, mutation: ConfigMutation) {
    match mutation {
        ConfigMutation::SetModel(model) => {
            tui.base_model.clone_from(&model);
            tui.config.model = model;
        }
        ConfigMutation::SetThinkingLevel(level) => {
            tui.base_thinking_level = level;
            tui.config.thinking_level = level;
        }
        ConfigMutation::SetFastMode { provider, enabled } => {
            tui.config.set_fast_mode_for_provider(provider, enabled);
        }
    }
}

fn push_token_exchange(
    effects: &mut Vec<UiEffect>,
    provider: zdx_engine::providers::ProviderKind,
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
    let mut effects = update.effects;
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
            effects.extend(open_overlay_request(app, &request));
        }
    }
    effects
}

fn open_overlay_request(app: &mut AppState, request: &overlays::OverlayRequest) -> Vec<UiEffect> {
    match request {
        overlays::OverlayRequest::CommandPalette => {
            let state = overlays::CommandPaletteState::open(
                app.tui.config.model.clone(),
                app.custom_commands.clone(),
            );
            app.overlay = Some(overlays::Overlay::CommandPalette(state));
            vec![]
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
            let loaded = app.tui.loaded_skills.clone();
            let (state, effects) = overlays::SkillPickerState::open(loaded, repos, last_repo);
            if let Some(repo) = state.current_repo() {
                app.tui.last_skill_repo = Some(repo.to_string());
            }
            app.overlay = Some(overlays::Overlay::SkillPicker(state));
            effects
        }
        overlays::OverlayRequest::ThinkingPicker => {
            if !zdx_engine::models::model_supports_reasoning(&app.tui.config.model) {
                return vec![];
            }
            let (state, effects) =
                overlays::ThinkingPickerState::open(app.tui.config.thinking_level);
            app.overlay = Some(overlays::Overlay::ThinkingPicker(state));
            effects
        }
        overlays::OverlayRequest::NewTab => {
            let tab_id = app.next_tab_id();
            let tab = create_main_tab(tab_id, &app.tui);
            app.overlay = None;
            app.push_tab(tab);
            // Persist new tab from the start: create a thread immediately so
            // messages sent in this tab are saved to disk. Without this, a
            // `Main` tab keeps its conversation in memory only and loses it on
            // close (see `spawn_agent_turn`'s no-handle branch and
            // `build_send_effects_for_tab`, which only emit `SaveThread` when
            // a handle exists).
            vec![UiEffect::CreateNewThread]
        }
        overlays::OverlayRequest::Btw => {
            let base_messages = build_btw_base_messages(&app.tui);
            let tab_id = app.next_tab_id();
            let btw_tab = create_btw_tab(tab_id, base_messages, &app.tui);
            app.overlay = None; // Close command palette before switching tabs
            app.push_tab(btw_tab);
            vec![]
        }
        overlays::OverlayRequest::Login => {
            let (state, effects) = overlays::LoginState::open(&app.tui);
            app.overlay = Some(overlays::Overlay::Login(state));
            effects
        }
        overlays::OverlayRequest::FilePicker { trigger_pos } => {
            let (state, effects) = overlays::FilePickerState::open(*trigger_pos);
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
            if let Some(thread_handle) = &app.tui.thread.thread_handle {
                let (state, effects) = overlays::RenameState::open(
                    thread_handle.id.clone(),
                    None, // Current title not readily available in Thread
                );
                app.overlay = Some(overlays::Overlay::Rename(state));
                effects
            } else {
                vec![]
            }
        }
        overlays::OverlayRequest::Tldr => {
            // Requires an active thread persisted on disk so the engine can
            // load events. Without a handle, surface a system message instead
            // of opening an empty overlay.
            let Some(thread_handle) = &app.tui.thread.thread_handle else {
                app.tui
                    .transcript
                    .push_cell(crate::transcript::HistoryCell::system(
                        "TLDR requires an active thread.".to_string(),
                    ));
                return vec![];
            };
            let thread_id = thread_handle.id.clone();
            // If a previous TLDR task is still in flight (e.g. user closed
            // overlay while it ran), drop it so this new request takes over.
            let mut effects = Vec::new();
            if app.tui.tasks.state(TaskKind::ThreadTldr).is_running() {
                effects.push(UiEffect::CancelTask {
                    kind: TaskKind::ThreadTldr,
                    token: app.tui.tasks.state(TaskKind::ThreadTldr).cancel.clone(),
                });
            }
            app.overlay = Some(overlays::Overlay::Tldr(overlays::TldrState::open(
                thread_id.clone(),
            )));
            effects.push(UiEffect::GenerateTldr { thread_id });
            effects
        }
        overlays::OverlayRequest::Context => {
            // Cancel any in-flight context-analyze task so this new request
            // takes over (e.g. user closed overlay mid-analysis and reopened).
            let mut effects = Vec::new();
            if app.tui.tasks.state(TaskKind::ContextAnalyze).is_running() {
                effects.push(UiEffect::CancelTask {
                    kind: TaskKind::ContextAnalyze,
                    token: app.tui.tasks.state(TaskKind::ContextAnalyze).cancel.clone(),
                });
            }
            // Default to the instant heuristic; user presses `r` in the
            // overlay to refine to the exact Anthropic count_tokens path.
            // `refine_supported` controls whether the `[r]` hint is shown.
            let refine_supported =
                crate::runtime::refine_supported(&app.tui.config.model, &app.tui.config);
            app.overlay = Some(overlays::Overlay::Context(overlays::ContextState::open(
                refine_supported,
            )));
            effects.push(UiEffect::AnalyzeContext {
                mode: crate::runtime::AnalysisMode::Heuristic,
            });
            effects
        }
        overlays::OverlayRequest::ImagePreview {
            image_path,
            image_index,
        } => {
            let state = overlays::ImagePreviewState::open(image_path, *image_index);
            app.overlay = Some(overlays::Overlay::ImagePreview(state));
            vec![UiEffect::DecodeImagePreview {
                image_path: image_path.clone(),
            }]
        }
        overlays::OverlayRequest::ToolDetail { tool_use_id } => {
            let state = overlays::ToolDetailState::open(tool_use_id.clone());
            app.overlay = Some(overlays::Overlay::ToolDetail(state));
            vec![]
        }
    }
}

fn build_btw_base_messages(tui: &TuiState) -> Vec<zdx_engine::providers::ChatMessage> {
    let mut messages = tui.thread.messages.clone();
    let has_in_flight_turn = tui.agent_state.is_running() || tui.transcript.has_pending_user_cell();
    if has_in_flight_turn
        && messages
            .last()
            .is_some_and(|message| message.role == "user")
    {
        messages.pop();
    }
    messages
}

/// Cycles to the next or previous tab.
///
/// `direction` is +1 for next, -1 for previous. Wraps around.
pub fn cycle_tab(app: &mut AppState, direction: i32) {
    if app.background_tabs.is_empty() {
        return;
    }

    // Build ordered list of all tab IDs: active first, then background
    let mut all_ids: Vec<TabId> = Vec::with_capacity(app.tab_count());
    all_ids.push(app.tui.tab_id);
    for tab in &app.background_tabs {
        all_ids.push(tab.tab_id);
    }

    // Find current position and compute target
    let current_pos = 0usize; // Active tab is always at position 0 in our view
    let len = all_ids.len() as i32;
    let target_pos = ((current_pos as i32 + direction).rem_euclid(len)) as usize;

    if target_pos != current_pos {
        app.switch_to_tab(all_ids[target_pos]);
    }
}

/// Creates a new btw tab forked from the current tab's conversation.
fn create_btw_tab(
    tab_id: TabId,
    base_messages: Vec<zdx_engine::providers::ChatMessage>,
    parent: &TuiState,
) -> TuiState {
    use crate::input::InputState;
    use crate::thread::ThreadState;
    use crate::transcript::TranscriptState;

    // Build transcript cells from the forked context so the user can see
    // the conversation they branched from.
    let cells = TuiState::build_transcript_from_history(&base_messages);
    let transcript = TranscriptState::with_cells(cells);

    let (ask_user_map, tool_config) = crate::state::build_ask_user_tooling();
    let mut agent_opts = parent.agent_opts.clone();
    agent_opts.tool_config = tool_config;

    TuiState {
        tab_id,
        tab_kind: TabKind::Btw { base_messages },
        should_quit: false,
        input: InputState::new(),
        transcript,
        thread: ThreadState::new(),
        task_seq: crate::common::TaskSeq::default(),
        tasks: crate::common::Tasks::default(),
        auth: crate::auth::AuthState::new(),
        config: parent.config.clone(),
        base_model: parent.base_model.clone(),
        base_thinking_level: parent.base_thinking_level,
        last_skill_repo: parent.last_skill_repo.clone(),
        loaded_skills: parent.loaded_skills.clone(),
        agent_opts,
        system_prompt: parent.system_prompt.clone(),
        agent_state: AgentState::Idle,
        last_turn_outcome: None,
        spinner_frame: 0,
        git_branch: parent.git_branch.clone(),
        display_path: parent.display_path.clone(),
        status_line: crate::statusline::StatusLineAccumulator::new(),
        show_debug_status: false,
        input_area: std::cell::Cell::new(ratatui::layout::Rect::default()),
        transcript_area: std::cell::Cell::new(ratatui::layout::Rect::default()),
        optimistic_active_threads: std::collections::HashMap::new(),
        ask_user_map,
        last_followups: Vec::new(),
    }
}

fn create_main_tab(tab_id: TabId, parent: &TuiState) -> TuiState {
    let mut config = parent.config.clone();
    config.model.clone_from(&parent.base_model);
    config.thinking_level = parent.base_thinking_level;

    let mut tab = TuiState::with_history(
        tab_id,
        TabKind::Main,
        config,
        parent.agent_opts.root.clone(),
        parent.system_prompt.clone(),
        None,
        Vec::new(),
    );
    tab.last_skill_repo.clone_from(&parent.last_skill_repo);
    tab.show_debug_status = parent.show_debug_status;
    tab
}

/// Creates a tab from a loaded or forked thread.
#[allow(clippy::too_many_arguments)]
fn create_thread_tab(
    tab_id: TabId,
    cells: Vec<HistoryCell>,
    messages: Vec<zdx_engine::providers::ChatMessage>,
    history: Vec<String>,
    thread_handle: zdx_engine::core::thread_persistence::Thread,
    title: Option<&String>,
    model_override: Option<&String>,
    thinking_override: Option<zdx_engine::config::ThinkingLevel>,
    usage: (
        zdx_engine::core::thread_persistence::Usage,
        zdx_engine::core::thread_persistence::Usage,
    ),
    user_input: Option<&str>,
    parent: &TuiState,
) -> TuiState {
    use crate::input::InputState;
    use crate::thread::ThreadState;
    use crate::transcript::TranscriptState;

    let transcript = TranscriptState::with_cells(cells);
    let mut thread = ThreadState::with_thread(Some(thread_handle), messages);
    thread.title.clone_from(&title.cloned());
    thread.model_override = model_override.cloned();
    thread.thinking_override = thinking_override;
    thread.usage.restore(usage.0, usage.1);

    let mut config = parent.config.clone();
    if let Some(model) = model_override {
        config.model.clone_from(model);
    }
    if let Some(thinking) = thinking_override {
        config.thinking_level = thinking;
    }

    let mut input = InputState::new();
    input.history = history;
    if let Some(text) = user_input {
        input.set_text(text);
    }

    let (ask_user_map, tool_config) = crate::state::build_ask_user_tooling();
    let mut agent_opts = parent.agent_opts.clone();
    agent_opts.tool_config = tool_config;

    TuiState {
        tab_id,
        tab_kind: TabKind::Thread {
            title: title.cloned(),
            thread_id: thread
                .thread_handle
                .as_ref()
                .map(|t| t.id.clone())
                .unwrap_or_default(),
        },
        should_quit: false,
        input,
        transcript,
        thread,
        task_seq: crate::common::TaskSeq::default(),
        tasks: crate::common::Tasks::default(),
        auth: crate::auth::AuthState::new(),
        config,
        base_model: parent.base_model.clone(),
        base_thinking_level: parent.base_thinking_level,
        last_skill_repo: parent.last_skill_repo.clone(),
        loaded_skills: parent.loaded_skills.clone(),
        agent_opts,
        system_prompt: parent.system_prompt.clone(),
        agent_state: AgentState::Idle,
        last_turn_outcome: None,
        spinner_frame: 0,
        git_branch: parent.git_branch.clone(),
        display_path: parent.display_path.clone(),
        status_line: crate::statusline::StatusLineAccumulator::new(),
        show_debug_status: false,
        input_area: std::cell::Cell::new(ratatui::layout::Rect::default()),
        transcript_area: std::cell::Cell::new(ratatui::layout::Rect::default()),
        optimistic_active_threads: std::collections::HashMap::new(),
        ask_user_map,
        last_followups: Vec::new(),
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
fn handle_frame(tui: &mut TuiState, width: u16, height: u16, tab_bar_height: u16) {
    let previous_width = tui.transcript.terminal_size.0;

    // Update transcript layout with current terminal dimensions
    let viewport_height =
        render::calculate_transcript_height_with_state(tui, height, tab_bar_height);
    tui.transcript
        .update_layout((width, height), viewport_height);

    // Apply any pending streaming text deltas (coalescing)
    transcript::apply_pending_delta(&mut tui.transcript, &mut tui.agent_state);

    // Apply accumulated scroll delta from mouse events (coalescing)
    transcript::apply_scroll_delta(&mut tui.transcript);

    // Update cell line info for lazy rendering and scroll calculations.
    // Width changes invalidate every wrapped line; otherwise patch only the
    // cells marked dirty since the last frame (usually just the streaming cell).
    let width_changed = previous_width != width;
    let dirty = tui.transcript.take_line_info_dirty();
    let rebuild_from = if width_changed { Some(0) } else { dirty };
    if let Some(from) = rebuild_from {
        let cell_line_counts = render::calculate_cell_line_counts(tui, width as usize, from);
        tui.transcript
            .scroll
            .patch_cell_line_info(from, cell_line_counts);
    }
}

// ============================================================================
// Terminal Event Handlers
// ============================================================================

fn handle_terminal_event(app: &mut AppState, event: Event) -> Vec<UiEffect> {
    match event {
        Event::Key(key) => handle_key(app, key),
        Event::Mouse(mouse) => {
            if let Some(overlays::Overlay::Context(state)) = &mut app.overlay {
                match mouse.kind {
                    crossterm::event::MouseEventKind::ScrollUp => {
                        state.scroll_up(3);
                    }
                    crossterm::event::MouseEventKind::ScrollDown => {
                        state.scroll_down(3);
                    }
                    _ => {}
                }
                return vec![];
            }

            if let Some(overlays::Overlay::ToolDetail(state)) = &mut app.overlay {
                // ToolDetail overlay consumes all mouse events while open
                match mouse.kind {
                    crossterm::event::MouseEventKind::ScrollUp => {
                        state.scroll_up(3);
                    }
                    crossterm::event::MouseEventKind::ScrollDown => {
                        state.scroll_down(3);
                    }
                    _ => {}
                }
                return vec![];
            }

            // Check if click is in the input area first
            let input_area = app.tui.input_area.get();
            if mouse.row >= input_area.y
                && mouse.row < input_area.y + input_area.height
                && mouse.column >= input_area.x
                && mouse.column < input_area.x + input_area.width
            {
                if let Some(request) = input::handle_mouse(&app.tui.input, mouse, input_area) {
                    return open_overlay_request(app, &request);
                }
                return vec![];
            }

            if let Some(request) = transcript::handle_mouse(
                &mut app.tui.transcript,
                mouse,
                app.tui.transcript_area.get(),
            ) {
                open_overlay_request(app, &request)
            } else {
                vec![]
            }
        }
        Event::Paste(text) => input::handle_paste(&mut app.tui.input, &mut app.overlay, &text),
        Event::FocusGained => {
            app.is_focused = true;
            vec![]
        }
        Event::FocusLost => {
            app.is_focused = false;
            vec![]
        }
        Event::Resize(_, _) => {
            // Clear wrap cache on resize since line wrapping depends on width
            app.tui.transcript.wrap_cache.clear();
            app.tui.transcript.invalidate_line_info();
            vec![]
        }
    }
}

fn handle_key(app: &mut AppState, key: crossterm::event::KeyEvent) -> Vec<UiEffect> {
    use crossterm::event::{KeyCode, KeyModifiers};

    // Tab navigation — handled before overlays so it always works
    if app.tab_count() > 1 {
        match (key.modifiers, key.code) {
            // Ctrl+PageDown: next tab
            (m, KeyCode::PageDown) if m.contains(KeyModifiers::CONTROL) => {
                cycle_tab(app, 1);
                return vec![];
            }
            // Ctrl+PageUp: previous tab
            (m, KeyCode::PageUp) if m.contains(KeyModifiers::CONTROL) => {
                cycle_tab(app, -1);
                return vec![];
            }
            _ => {}
        }
    }

    // Ctrl+W: close current tab when idle and input is empty.
    // Otherwise keep the normal readline-style delete-word behavior in input handling.
    if app.overlay.is_none()
        && key.code == KeyCode::Char('w')
        && key.modifiers.contains(KeyModifiers::CONTROL)
        && app.tab_count() > 1
        && app.tui.input.get_text().is_empty()
    {
        return vec![UiEffect::CloseCurrentTab];
    }

    // Ctrl+F: open a picker on demand — the pending question picker when a
    // question is waiting, otherwise the follow-up suggestion picker. Keeps
    // the keyboard free for typing/dictation by default.
    if app.overlay.is_none()
        && key.code == KeyCode::Char('f')
        && key.modifiers.contains(KeyModifiers::CONTROL)
        && app.tui.input.get_text().is_empty()
    {
        let thread_id = app.tui.thread.thread_handle.as_ref().map(|t| t.id.clone());
        if let Some(tid) = thread_id.clone()
            && let Some(view) = crate::ask_user::pending_view(&app.tui.ask_user_map, &tid)
        {
            app.overlay = Some(Overlay::QuestionPicker(
                overlays::QuestionPickerState::open(tid, view),
            ));
            return vec![];
        }
        if !app.tui.agent_state.is_running() && !app.tui.last_followups.is_empty() {
            let items = app.tui.last_followups.clone();
            app.overlay = Some(Overlay::FollowupPicker(
                overlays::FollowupPickerState::open(thread_id, items),
            ));
            return vec![];
        }
    }

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
        .thread_handle
        .as_ref()
        .map(|thread_handle| thread_handle.id.clone());
    let active_thread_ids = app.tui.snapshot_active_thread_ids();
    let pending_question = thread_id
        .as_deref()
        .is_some_and(|id| crate::ask_user::has_pending(&app.tui.ask_user_map, id));
    let ctx = input::InputContext {
        agent_state: &app.tui.agent_state,
        tasks: &app.tui.tasks,
        thread_id,
        thread_title: app.tui.thread.title.as_deref(),
        config: &app.tui.config,
        model_id: &app.tui.config.model,
        active_thread_ids: &active_thread_ids,
        root: app.tui.agent_opts.root.as_path(),
        pending_question,
    };
    let (effects, mutations, overlay_request) =
        input::handle_main_key(&mut app.tui.input, &ctx, key);
    apply_mutations(&mut app.tui, mutations);

    // Handoff submission opens the new thread in a fresh background tab so
    // the source thread stays intact in its current tab. Push the new tab
    // here, before the runtime processes the `HandoffSubmit` effect — that
    // handler spawns `thread_create`, and `ThreadUiEvent::Created` populates
    // whichever tab is active when it arrives (which will be the new tab).
    if effects
        .iter()
        .any(|effect| matches!(effect, UiEffect::HandoffSubmit { .. }))
    {
        let tab_id = app.next_tab_id();
        let tab = create_main_tab(tab_id, &app.tui);
        app.push_tab(tab);
    }

    if let Some(request) = overlay_request
        && app.overlay.is_none()
    {
        let mut overlay_effects = open_overlay_request(app, &request);
        overlay_effects.extend(effects);
        return overlay_effects;
    }

    effects
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    use zdx_engine::core::events::AgentEvent;
    use zdx_engine::core::thread_persistence::Thread;
    use zdx_engine::skills::{Skill, SkillSource};

    use super::*;
    use crate::transcript::{HistoryCell, ScrollMode};

    fn unique_thread_id(prefix: &str) -> String {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        format!("{prefix}-{nanos}")
    }

    #[test]
    fn todo_progress_extracts_ratio_and_active_label() {
        let output = zdx_engine::core::events::ToolOutput::success(serde_json::json!({
            "todos": [
                { "id": "todo-1", "content": "Inspect bug", "status": "completed" },
                { "id": "todo-2", "content": "Implement fix", "status": "in_progress" },
                { "id": "todo-3", "content": "Add tests", "status": "pending" },
            ],
            "counts": { "total": 3, "pending": 1, "in_progress": 1, "completed": 1, "abandoned": 0 },
            "summary": "Active: todo-2 (Implement fix). Remaining todos: 2.",
            "reminder": "Todo list updated. Continue using Todo_Write to mark items in_progress before starting and completed as soon as you finish — keep exactly one todo in_progress while work remains.",
        }));

        let (value, label) = todo_progress_from_output(&output).expect("todo progress");
        assert!((value - 1.0 / 3.0).abs() < f64::EPSILON);
        assert_eq!(label, "Implement fix");
    }

    #[test]
    fn todo_progress_ignores_non_todo_output() {
        let output = zdx_engine::core::events::ToolOutput::success(serde_json::json!({
            "file_path": "src/lib.rs",
        }));
        assert!(todo_progress_from_output(&output).is_none());
    }

    #[test]
    fn cmux_status_value_uses_dot_separator() {
        // Glyph status + title.
        assert_eq!(
            cmux_status_value("✗", Some("fix auth bug")),
            "✗ · fix auth bug"
        );
        // Empty status (completed) → bare title.
        assert_eq!(cmux_status_value("", Some("fix auth bug")), "fix auth bug");
        // Empty status + no title → empty pill.
        assert_eq!(cmux_status_value("", None), "");
        // Glyph + no title → glyph only.
        assert_eq!(cmux_status_value("✗", None), "✗");
        // Whitespace-only title is treated as absent.
        assert_eq!(cmux_status_value("✗", Some("   ")), "✗");
    }

    #[test]
    fn term_title_value_animates_while_running() {
        use crate::transcript::SPINNER_SPEED_DIVISOR;

        // OSC disabled → leave the terminal title untouched.
        assert_eq!(term_title_value(false, true, Some("Fix bug"), 0), None);
        // Idle → bare thread title.
        assert_eq!(
            term_title_value(true, false, Some("Fix bug"), 0),
            Some("Fix bug".to_string())
        );
        // Running → spinner-prefixed title; frame 0 is the first glyph.
        assert_eq!(
            term_title_value(true, true, Some("Fix bug"), 0),
            Some("◐ Fix bug".to_string())
        );
        // Spinner advances once per `SPINNER_SPEED_DIVISOR` ticks.
        assert_eq!(
            term_title_value(true, true, Some("Fix bug"), SPINNER_SPEED_DIVISOR),
            Some("◓ Fix bug".to_string())
        );
        // No title yet (or whitespace) → falls back to `zdx`.
        assert_eq!(
            term_title_value(true, false, None, 0),
            Some("zdx".to_string())
        );
        assert_eq!(
            term_title_value(true, true, Some("   "), 0),
            Some("◐ zdx".to_string())
        );
    }

    #[test]
    fn cmux_pill_value_covers_running_and_idle() {
        use crate::state::TurnOutcome;

        // Disabled → no pill.
        assert_eq!(cmux_pill_value(false, true, None, Some("Fix bug"), 0), None);
        // Running → spinner-prefixed pill.
        assert_eq!(
            cmux_pill_value(true, true, None, Some("Fix bug"), 0),
            Some("◐ · Fix bug".to_string())
        );
        // Spinner advances once per `CMUX_SPINNER_SPEED_DIVISOR` ticks.
        assert_eq!(
            cmux_pill_value(
                true,
                true,
                None,
                Some("Fix bug"),
                CMUX_SPINNER_SPEED_DIVISOR
            ),
            Some("◓ · Fix bug".to_string())
        );
        // Running with no title → spinner + `zdx` fallback.
        assert_eq!(
            cmux_pill_value(true, true, None, None, 0),
            Some("◐ · zdx".to_string())
        );
        // Idle + succeeded → bare title.
        assert_eq!(
            cmux_pill_value(
                true,
                false,
                Some(TurnOutcome::Succeeded),
                Some("Fix bug"),
                0
            ),
            Some("Fix bug".to_string())
        );
        // Idle + failed → error glyph + title.
        assert_eq!(
            cmux_pill_value(true, false, Some(TurnOutcome::Failed), Some("Fix bug"), 0),
            Some("✗ · Fix bug".to_string())
        );
        // Idle + succeeded but no title yet → clear (None).
        assert_eq!(
            cmux_pill_value(true, false, Some(TurnOutcome::Succeeded), None, 0),
            None
        );
        // Idle + no finished turn → clear (None).
        assert_eq!(cmux_pill_value(true, false, None, Some("Fix bug"), 0), None);
    }

    #[test]
    fn test_scroll_to_top() {
        let config = zdx_engine::config::Config::default();
        let mut app = AppState::new(config, PathBuf::new(), None, None);

        app.tui.transcript.scroll_to_top();

        assert!(matches!(
            app.tui.transcript.scroll.mode,
            ScrollMode::Anchored { offset: 0 }
        ));
    }

    #[test]
    fn test_scroll_to_bottom() {
        let config = zdx_engine::config::Config::default();
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
        let config = zdx_engine::config::Config::default();
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
        let config = zdx_engine::config::Config::default();
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
    fn test_queue_drains_on_turn_finished() {
        let config = zdx_engine::config::Config::default();
        let mut app = AppState::new(config, PathBuf::new(), None, None);
        app.tui.input.enqueue_prompt("queued prompt".to_string());

        let effects = update(
            &mut app,
            UiEvent::Agent(AgentEvent::TurnFinished {
                status: zdx_engine::core::events::TurnStatus::Completed,
                final_text: String::new(),
                messages: Vec::new(),
                prior_message_count: 0,
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

    /// Regression: a `TurnFinished` for a background tab must drain that
    /// tab's queue and emit `StartAgentTurnInBackgroundTab` for it,
    /// instead of leaving the queued prompt stranded. See the bug
    /// where switching tabs while an agent was running left the queued
    /// prompt visible in the `Queued` panel forever once the
    /// background turn ended.
    #[test]
    fn background_tab_turn_finished_drains_queue() {
        let config = zdx_engine::config::Config::default();
        let mut app = AppState::new(config, PathBuf::new(), None, None);

        // Push a fresh tab to background. After this, `app.tui` is a new
        // empty active tab and the original tab lives in `background_tabs`
        // with its own `input.queued`.
        let new_tab_id = app.next_tab_id();
        let new_tab = create_main_tab(new_tab_id, &app.tui);
        app.push_tab(new_tab);
        let background_tab_id = app
            .background_tabs
            .first()
            .map(|t| t.tab_id)
            .expect("expected one background tab");

        // Enqueue a prompt on the background tab (mirrors what
        // `handle_submit_while_agent_running` would have produced before
        // the user switched tabs).
        app.background_tab_mut(background_tab_id)
            .expect("background tab")
            .input
            .enqueue_prompt("queued background prompt".to_string());

        let effects = update(
            &mut app,
            UiEvent::BackgroundTabAgent {
                tab_id: background_tab_id,
                event: AgentEvent::TurnFinished {
                    status: zdx_engine::core::events::TurnStatus::Completed,
                    final_text: String::new(),
                    messages: Vec::new(),
                    prior_message_count: 0,
                },
            },
        );

        // Background-targeted start effect, not the active-tab variant.
        assert!(
            effects.iter().any(|effect| matches!(
                effect,
                UiEffect::StartAgentTurnInBackgroundTab { tab_id } if *tab_id == background_tab_id
            )),
            "expected StartAgentTurnInBackgroundTab effect, got {effects:?}"
        );
        assert!(
            !effects
                .iter()
                .any(|effect| matches!(effect, UiEffect::StartAgentTurn)),
            "must not emit active-tab StartAgentTurn for a background tab drain"
        );

        // The queued prompt must be popped from the background tab's queue,
        // not the active tab's.
        let bg_tab = app
            .background_tabs
            .iter()
            .find(|t| t.tab_id == background_tab_id)
            .expect("background tab");
        assert!(!bg_tab.input.has_queued());
        assert!(app.tui.input.queued.is_empty());

        // The queued user cell lands on the background tab's transcript.
        let last_cell = bg_tab.transcript.cells().last().expect("cell");
        assert!(matches!(
            last_cell,
            HistoryCell::User { content, .. } if content == "queued background prompt"
        ));
        assert!(
            !app.tui
                .transcript
                .cells()
                .iter()
                .any(|c| matches!(c, HistoryCell::User { content, .. } if content == "queued background prompt")),
            "queued prompt must not leak into the active tab's transcript"
        );
    }

    #[test]
    fn test_thread_created_matches_startup_messages_and_prefills_initial_input() {
        let config = zdx_engine::config::Config::default();
        let mut app = AppState::new(config, PathBuf::new(), None, None);
        let zdx_home = std::env::temp_dir().join(unique_thread_id("zdx-tui-update-tests"));
        std::fs::create_dir_all(&zdx_home).unwrap();
        unsafe {
            std::env::set_var("ZDX_HOME", &zdx_home);
        }
        let thread_handle = Thread::with_id(unique_thread_id("handoff-created")).unwrap();
        let thread_path = thread_handle.path().display().to_string();
        let prompt = "Continue the work from the previous thread.".to_string();

        let effects = update(
            &mut app,
            UiEvent::Thread(ThreadUiEvent::Created {
                thread_handle,
                context_paths: vec![PathBuf::from("/tmp/project/AGENTS.md")],
                skills: vec![Skill {
                    name: "ship-first-plan".to_string(),
                    description: "Create ship-first plans".to_string(),
                    file_path: PathBuf::from("/tmp/skills/ship-first-plan/SKILL.md"),
                    base_dir: PathBuf::from("/tmp/skills/ship-first-plan"),
                    source: SkillSource::BuiltIn,
                }],
                initial_input: Some(prompt.clone()),
            }),
        );

        assert!(effects.is_empty());
        assert_eq!(app.tui.input.get_text(), prompt);

        let cells = app.tui.transcript.cells();
        assert_eq!(cells.len(), 3);
        assert!(matches!(
            &cells[0],
            HistoryCell::System { content, .. } if content == &format!("Thread path: {thread_path}")
        ));
        assert!(matches!(
            &cells[1],
            HistoryCell::System { content, .. }
                if content == "Project context files available from:\n  - /tmp/project/AGENTS.md"
        ));
        assert!(matches!(
            &cells[2],
            HistoryCell::System { content, .. }
                if content == "Loaded skills:\n  - ship-first-plan (builtin)"
        ));
    }

    #[test]
    fn test_create_failed_keeps_no_thread_state() {
        let config = zdx_engine::config::Config::default();
        let mut app = AppState::new(config, PathBuf::new(), None, None);

        let effects = update(
            &mut app,
            UiEvent::Thread(ThreadUiEvent::CreateFailed {
                error: "Failed to create thread: boom".to_string(),
            }),
        );

        assert!(effects.is_empty());
        assert!(app.tui.thread.thread_handle.is_none());
        let cells = app.tui.transcript.cells();
        assert_eq!(cells.len(), 2);
        assert!(matches!(
            &cells[0],
            HistoryCell::System { content, .. } if content == "Failed to create thread: boom"
        ));
        assert!(matches!(
            &cells[1],
            HistoryCell::System { content, .. } if content == "Thread cleared."
        ));
    }

    /// Regression: submitting a handoff in `Ready` state must open the new
    /// thread in a fresh background tab and leave the source tab untouched.
    /// See `crates/zdx-tui/src/features/input/update.rs::handle_handoff_submission`.
    #[test]
    fn handoff_submit_opens_new_tab_and_preserves_source_tab() {
        use crossterm::event::{Event as CrosstermEvent, KeyCode, KeyEvent, KeyModifiers};

        use crate::input::HandoffState;
        use crate::transcript::TranscriptState;

        let config = zdx_engine::config::Config::default();
        let mut app = AppState::new(config, PathBuf::new(), None, None);

        let zdx_home = std::env::temp_dir().join(unique_thread_id("zdx-tui-handoff-tab-tests"));
        std::fs::create_dir_all(&zdx_home).unwrap();
        unsafe {
            std::env::set_var("ZDX_HOME", &zdx_home);
        }

        // Seed the active (source) tab: thread handle, transcript, history,
        // and a generated handoff prompt sitting in the textarea in `Ready`
        // state — i.e. the user has just finished generation and is about to
        // press Enter.
        let source_thread_id = unique_thread_id("handoff-source");
        let source_thread_handle = Thread::with_id(source_thread_id.clone()).unwrap();
        app.tui.thread.thread_handle = Some(source_thread_handle);
        app.tui.transcript = TranscriptState::with_cells(vec![HistoryCell::user(
            "original message in source thread",
        )]);
        app.tui.input.history = vec!["earlier prompt".to_string()];
        app.tui.input.handoff = HandoffState::Ready;
        app.tui.input.set_text("Generated handoff prompt body.");
        let source_tab_id = app.tui.tab_id;

        let effects = update(
            &mut app,
            UiEvent::Terminal(CrosstermEvent::Key(KeyEvent::new(
                KeyCode::Enter,
                KeyModifiers::NONE,
            ))),
        );

        // The submission emits a HandoffSubmit effect with the prompt and source thread ID.
        let submit = effects
            .iter()
            .find_map(|effect| match effect {
                UiEffect::HandoffSubmit {
                    prompt,
                    handoff_from,
                } => Some((prompt.clone(), handoff_from.clone())),
                _ => None,
            })
            .expect("expected HandoffSubmit effect");
        assert_eq!(submit.0, "Generated handoff prompt body.");
        assert_eq!(submit.1.as_deref(), Some(source_thread_id.as_str()));

        // The source tab moved to background; a fresh tab is now active.
        assert_eq!(app.background_tabs.len(), 1);
        assert_ne!(app.tui.tab_id, source_tab_id);

        // Source tab still has its original thread, transcript, and history.
        let source = app
            .background_tabs
            .iter()
            .find(|t| t.tab_id == source_tab_id)
            .expect("source tab should be in background");
        assert_eq!(
            source
                .thread
                .thread_handle
                .as_ref()
                .map(|t| t.id.clone())
                .as_deref(),
            Some(source_thread_id.as_str()),
            "source tab must retain its thread handle"
        );
        assert_eq!(
            source.transcript.cells().len(),
            1,
            "source tab must retain its transcript"
        );
        assert_eq!(
            source.input.history,
            vec!["earlier prompt".to_string()],
            "source tab must retain its input history"
        );

        // The new active tab is fresh: no thread handle, empty transcript,
        // empty input, handoff state idle.
        assert!(app.tui.thread.thread_handle.is_none());
        assert!(app.tui.transcript.cells().is_empty());
        assert!(app.tui.input.get_text().is_empty());
        assert!(matches!(app.tui.input.handoff, HandoffState::Idle));
    }
}
