//! Input feature reducer.
//!
//! Handles keyboard input, history navigation, and handoff state transitions.
//! All state mutations for input-related events happen here.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers as CrosstermKeyModifiers};
use zdx_engine::agent_activity;
use zdx_engine::config::Config;
use zdx_engine::core::thread_persistence::ThreadEvent;
use zdx_engine::providers::ChatMessage;

use super::CursorMove;
use super::state::{
    HandoffState, InputState, LARGE_PASTE_CHAR_THRESHOLD, PendingImage, PendingPaste,
    PromptBuilderState,
};
use crate::common::{TaskKind, Tasks, sanitize_for_display};
use crate::effects::UiEffect;
use crate::mutations::{ConfigMutation, StateMutation, ThreadMutation, TranscriptMutation};
use crate::overlays::{LoginState, Overlay, OverlayRequest};
use crate::state::{AgentState, TabId, fast_mode_enabled_for_model, fast_mode_provider_for_model};
use crate::transcript::HistoryCell;

/// Result type for key handlers.
type KeyResult = (Vec<UiEffect>, Vec<StateMutation>, Option<OverlayRequest>);

/// Context for handling main key input.
///
/// Groups the contextual state needed to decide how to handle a key press,
/// avoiding excessive function parameters.
pub struct InputContext<'a> {
    pub agent_state: &'a AgentState,
    pub tasks: &'a Tasks,
    pub thread_id: Option<String>,
    pub thread_title: Option<&'a str>,
    pub config: &'a Config,
    pub model_id: &'a str,
    pub active_thread_ids: &'a std::collections::HashSet<String>,
}

const FAST_MODE_UNAVAILABLE_MSG: &str =
    "Fast mode is only available for OpenAI and OpenAI Codex models.";

/// Builds the shared fast-mode toggle effects and mutations for the active model.
///
/// # Errors
/// Returns an error message when the current model does not support fast mode.
pub fn build_fast_mode_toggle_actions(
    config: &Config,
    model_id: &str,
) -> Result<(Vec<UiEffect>, Vec<StateMutation>), &'static str> {
    let Some(provider) = fast_mode_provider_for_model(model_id) else {
        return Err(FAST_MODE_UNAVAILABLE_MSG);
    };

    let enabled = !fast_mode_enabled_for_model(config, model_id);
    let label = if enabled { "on" } else { "off" };
    let suffix = if enabled {
        " (service_tier: priority, 2× cost)"
    } else {
        ""
    };

    Ok((
        vec![UiEffect::PersistFastMode { enabled, provider }],
        vec![
            StateMutation::Config(ConfigMutation::SetFastMode { provider, enabled }),
            StateMutation::Transcript(TranscriptMutation::AppendSystemMessage(format!(
                "Fast mode {label}{suffix}"
            ))),
        ],
    ))
}

fn is_image_path(text: &str) -> bool {
    let trimmed = text.trim();
    if trimmed.contains('\n') {
        return false;
    }
    // URLs (http://, https://, file://, ...) are not local paths; let them
    // paste as plain text instead of routing to the file-based attach flow.
    if trimmed.contains("://") {
        return false;
    }
    // Unescape shell-escaped characters for extension detection
    let unescaped = trimmed.replace("\\ ", " ");
    let ext = std::path::Path::new(&unescaped)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("");
    matches!(
        ext.to_ascii_lowercase().as_str(),
        "png" | "jpg" | "jpeg" | "gif" | "webp"
    )
}

/// Handles paste events for input.
///
/// Sanitizes pasted text by stripping ANSI escapes and expanding tabs to spaces.
/// For large pastes (>1000 chars), inserts a placeholder and stores the original
/// content for expansion on submission.
pub fn handle_paste(
    input: &mut InputState,
    overlay: &mut Option<Overlay>,
    text: &str,
) -> Vec<UiEffect> {
    // Pasting into the prompt-builder review state implicitly accepts the
    // polished prompt — the paste then modifies the composer normally.
    if input.prompt_builder.is_ready() {
        input.prompt_builder = PromptBuilderState::Idle;
    }
    let sanitized = sanitize_for_display(text);
    if is_image_path(&sanitized) {
        let path = sanitized.trim();
        let path = path.trim_matches('\'').trim_matches('"');
        return vec![UiEffect::AttachImage {
            path: path.to_string(),
        }];
    }
    if let Some(Overlay::Login(LoginState::AwaitingCode { .. })) = overlay {
        // Ignore paste while waiting for OAuth callback.
        return vec![];
    }

    let char_count = sanitized.chars().count();
    if char_count > LARGE_PASTE_CHAR_THRESHOLD {
        // Large paste: create placeholder and store original content
        let id = input.next_paste_id();
        let placeholder = InputState::generate_placeholder(char_count, &id);
        input.pending_pastes.push(PendingPaste {
            id,
            placeholder: placeholder.clone(),
            content: sanitized.into_owned(),
        });
        input.textarea.insert_str(&placeholder);
    } else {
        // Small paste: insert directly
        input.textarea.insert_str(&sanitized);
    }

    // Sync pending pastes in case the paste replaced selected text containing a placeholder
    input.sync_pending_pastes();
    input.sync_pending_images();
    vec![]
}

/// Handles main key input when no overlay is active.
///
/// # Errors
/// Returns an error if the operation fails.
pub fn handle_main_key(input: &mut InputState, ctx: &InputContext<'_>, key: KeyEvent) -> KeyResult {
    let mods = Modifiers::from(&key);

    // While a sub-feature's async generation phase owns the composer
    // (handoff, prompt-builder), restrict input to control keys
    // (Esc/Ctrl+C/voice hotkey) and Enter (which shows the existing "press
    // Esc to cancel" advisory). Drop everything else silently so the user
    // cannot accidentally type into a composer that will be overwritten
    // when the result event arrives or Esc restores the captured base.
    if input.is_modal_generation_active() {
        if let Some(result) = handle_control_keys(input, ctx, key.code, &mods) {
            return result;
        }
        if let Some(result) = handle_submission(input, ctx, key.code, &mods) {
            return result;
        }
        return (vec![], vec![], None);
    }

    // Prompt-builder review state (`Ready`): any non-Esc keystroke implicitly
    // accepts the polished prompt by dropping to `Idle` so normal dispatch
    // takes over (Enter sends the prompt, edits modify it). Esc is preserved
    // so `handle_esc_modals` can route it to the reject path that restores
    // the original intent.
    if input.prompt_builder.is_ready() && key.code != KeyCode::Esc {
        input.prompt_builder = PromptBuilderState::Idle;
    }

    // Try each handler category in order; first match wins
    handle_line_editing(input, key.code, &mods)
        .or_else(|| handle_word_editing(input, key.code, &mods))
        .or_else(|| handle_navigation(input, key, &mods))
        .or_else(|| handle_control_keys(input, ctx, key.code, &mods))
        .or_else(|| handle_overlays(input, ctx.model_id, key.code, &mods))
        .or_else(|| handle_submission(input, ctx, key.code, &mods))
        .unwrap_or_else(|| handle_default_input(input, key))
}

/// Parsed key modifiers for cleaner pattern matching.
struct Modifiers(CrosstermKeyModifiers);

impl Modifiers {
    fn from(key: &KeyEvent) -> Self {
        Self(key.modifiers)
    }

    fn none(&self) -> bool {
        self.0.is_empty()
    }

    fn ctrl(&self) -> bool {
        self.0.contains(CrosstermKeyModifiers::CONTROL)
    }

    fn shift(&self) -> bool {
        self.0.contains(CrosstermKeyModifiers::SHIFT)
    }

    fn alt(&self) -> bool {
        self.0.contains(CrosstermKeyModifiers::ALT)
    }

    fn super_key(&self) -> bool {
        self.0.contains(CrosstermKeyModifiers::SUPER)
    }

    fn only_ctrl(&self) -> bool {
        self.ctrl() && !self.shift() && !self.alt() && !self.super_key()
    }

    fn only_alt(&self) -> bool {
        self.alt() && !self.ctrl() && !self.shift() && !self.super_key()
    }

    fn only_super(&self) -> bool {
        self.super_key() && !self.ctrl() && !self.shift() && !self.alt()
    }
}

// =============================================================================
// Line editing: Ctrl+A, Ctrl+E, Ctrl+U, Ctrl+K
// =============================================================================

fn handle_line_editing(
    input: &mut InputState,
    code: KeyCode,
    mods: &Modifiers,
) -> Option<KeyResult> {
    match code {
        // Ctrl+A: move to beginning of line
        KeyCode::Char('a') if mods.only_ctrl() => {
            input.textarea.move_cursor(CursorMove::Head);
            Some((vec![], vec![], None))
        }
        // Ctrl+E: move to end of line
        KeyCode::Char('e') if mods.only_ctrl() => {
            input.textarea.move_cursor(CursorMove::End);
            Some((vec![], vec![], None))
        }
        // Ctrl+U: kill from cursor to beginning of line (unix line-kill)
        KeyCode::Char('u') if mods.only_ctrl() => {
            let (row, _) = input.textarea.cursor();
            let current_line = input
                .textarea
                .lines()
                .get(row)
                .map_or("", std::string::String::as_str);
            if current_line.is_empty() && row > 0 {
                // Line is empty, move to end of previous line and delete the newline
                input.textarea.move_cursor(CursorMove::Up);
                input.textarea.move_cursor(CursorMove::End);
                input.textarea.delete_next_char(); // delete the newline
            } else {
                // Clear from cursor to beginning of line
                input.textarea.move_cursor(CursorMove::Head);
                input.textarea.delete_line_by_end();
            }
            input.sync_pending_pastes();
            input.sync_pending_images();
            Some((vec![], vec![], None))
        }
        // Ctrl+K: kill from cursor to end of line
        KeyCode::Char('k') if mods.only_ctrl() => {
            input.textarea.delete_line_by_end();
            input.sync_pending_pastes();
            input.sync_pending_images();
            Some((vec![], vec![], None))
        }
        // Ctrl+J: insert newline (like Shift+Enter in some editors)
        KeyCode::Char('j') if mods.only_ctrl() => {
            input.textarea.insert_newline();
            Some((vec![], vec![], None))
        }
        _ => None,
    }
}

// =============================================================================
// Word editing: Ctrl+W, Alt+Backspace, Alt+f/b (word movement)
// =============================================================================

fn handle_word_editing(
    input: &mut InputState,
    code: KeyCode,
    mods: &Modifiers,
) -> Option<KeyResult> {
    match code {
        // Ctrl+W: delete word backward (common readline binding)
        KeyCode::Char('w') if mods.only_ctrl() => {
            input.reset_navigation();
            if !input.try_delete_placeholder_at_bracket(true) {
                input.textarea.delete_word_left();
                input.sync_pending_pastes();
                input.sync_pending_images();
            }
            Some((vec![], vec![], None))
        }
        // Alt+Backspace: delete word backward
        // (macOS sends this for Option+Delete)
        KeyCode::Backspace if mods.only_alt() => {
            input.reset_navigation();
            if !input.try_delete_placeholder_at_bracket(true) {
                input.textarea.delete_word_left();
                input.sync_pending_pastes();
                input.sync_pending_images();
            }
            Some((vec![], vec![], None))
        }
        // Alt+b or Alt+Left: move word backward
        // (macOS terminal sends Alt+b for Option+Left)
        KeyCode::Char('b') | KeyCode::Left if mods.only_alt() => {
            if !input.try_jump_over_placeholder(true) {
                input.textarea.move_word_left();
            }
            Some((vec![], vec![], None))
        }
        // Alt+f or Alt+Right: move word forward
        // (macOS terminal sends Alt+f for Option+Right)
        KeyCode::Char('f') | KeyCode::Right if mods.only_alt() => {
            if !input.try_jump_over_placeholder(false) {
                input.textarea.move_word_right();
            }
            Some((vec![], vec![], None))
        }
        _ => None,
    }
}

// =============================================================================
// Navigation: arrows, PageUp/Down, Home/End
// =============================================================================

fn handle_navigation(input: &mut InputState, key: KeyEvent, mods: &Modifiers) -> Option<KeyResult> {
    match key.code {
        // PageUp/PageDown: scroll transcript
        KeyCode::PageUp => Some((
            vec![],
            vec![StateMutation::Transcript(TranscriptMutation::PageUp)],
            None,
        )),
        KeyCode::PageDown => Some((
            vec![],
            vec![StateMutation::Transcript(TranscriptMutation::PageDown)],
            None,
        )),
        // Ctrl+Home: scroll to top
        KeyCode::Home if mods.ctrl() => Some((
            vec![],
            vec![StateMutation::Transcript(TranscriptMutation::ScrollToTop)],
            None,
        )),
        // Ctrl+End: scroll to bottom
        KeyCode::End if mods.ctrl() => Some((
            vec![],
            vec![StateMutation::Transcript(
                TranscriptMutation::ScrollToBottom,
            )],
            None,
        )),
        // Super+Left (Command+Left on macOS): move to beginning of line
        KeyCode::Left if mods.only_super() => {
            input.textarea.move_cursor(CursorMove::Head);
            Some((vec![], vec![], None))
        }
        // Super+Right (Command+Right on macOS): move to end of line
        KeyCode::Right if mods.only_super() => {
            input.textarea.move_cursor(CursorMove::End);
            Some((vec![], vec![], None))
        }
        // Alt+Up: move cursor to first line of input
        KeyCode::Up if mods.only_alt() => {
            input.textarea.move_cursor(CursorMove::Top);
            Some((vec![], vec![], None))
        }
        // Alt+Down: move cursor to last line of input
        KeyCode::Down if mods.only_alt() => {
            input.textarea.move_cursor(CursorMove::Bottom);
            Some((vec![], vec![], None))
        }
        // Up: history navigation or cursor movement
        KeyCode::Up if mods.none() => {
            if input.should_navigate_up() {
                input.navigate_up();
            } else {
                input.textarea.input(key);
                input.sync_pending_pastes();
                input.sync_pending_images();
                input.snap_to_placeholder_end();
            }
            Some((vec![], vec![], None))
        }
        // Down: history navigation or cursor movement
        KeyCode::Down if mods.none() => {
            if input.should_navigate_down() {
                input.navigate_down();
            } else {
                input.textarea.input(key);
                input.sync_pending_pastes();
                input.sync_pending_images();
                input.snap_to_placeholder_end();
            }
            Some((vec![], vec![], None))
        }
        // Left: character movement (with placeholder jumping)
        KeyCode::Left if mods.none() => {
            if !input.try_jump_over_placeholder(true) {
                input.textarea.input(key);
            }
            Some((vec![], vec![], None))
        }
        // Right: character movement (with placeholder jumping)
        KeyCode::Right if mods.none() => {
            if !input.try_jump_over_placeholder(false) {
                input.textarea.input(key);
            }
            Some((vec![], vec![], None))
        }
        _ => None,
    }
}

// =============================================================================
// Control keys: Ctrl+C, Escape
// =============================================================================

fn handle_control_keys(
    input: &mut InputState,
    ctx: &InputContext<'_>,
    code: KeyCode,
    mods: &Modifiers,
) -> Option<KeyResult> {
    match code {
        KeyCode::Char(' ') | KeyCode::Null if mods.only_ctrl() => Some(handle_voice_hotkey(input)),
        // Ctrl+C: interrupt agent, clear input, or quit app
        KeyCode::Char('c') if mods.ctrl() => {
            if ctx.agent_state.is_running() {
                Some((vec![UiEffect::InterruptAgent], vec![], None))
            } else if ctx.tasks.state(TaskKind::Bash).is_running() {
                Some((vec![UiEffect::InterruptBash], vec![], None))
            } else if !input.get_text().is_empty() {
                input.clear();
                Some((vec![], vec![], None))
            } else {
                Some((vec![UiEffect::Quit], vec![], None))
            }
        }
        // Escape: cancel current operation or clear input
        KeyCode::Esc => {
            if let Some(result) = handle_esc_voice(input) {
                return Some(result);
            }
            if let Some(result) = handle_esc_modals(input) {
                return Some(result);
            }
            if ctx.agent_state.is_running() {
                Some((vec![UiEffect::InterruptAgent], vec![], None))
            } else if ctx.tasks.state(TaskKind::Bash).is_running() {
                Some((
                    vec![UiEffect::CancelTask {
                        kind: TaskKind::Bash,
                        token: None,
                    }],
                    vec![],
                    None,
                ))
            } else {
                input.clear();
                Some((vec![], vec![], None))
            }
        }
        _ => None,
    }
}

/// Handles Esc while voice capture/transcription is active. Returns `Some`
/// when voice owns the input.
fn handle_esc_voice(input: &mut InputState) -> Option<KeyResult> {
    if input.voice.is_recording() {
        input.voice.discard_next_capture = true;
        return Some((vec![UiEffect::StopVoiceRecording], vec![], None));
    }
    if input.voice.is_transcribing() {
        input.voice.mark_idle();
        return Some((
            vec![UiEffect::CancelTask {
                kind: TaskKind::VoiceTranscribe,
                token: None,
            }],
            vec![],
            None,
        ));
    }
    None
}

/// Handles Esc for any modal flow that owns the composer. Generating-state
/// modals cancel the underlying task; non-generating modals just reset to
/// Idle. Returns `Some` when one of the modals matched.
fn handle_esc_modals(input: &mut InputState) -> Option<KeyResult> {
    if input.handoff.is_generating() {
        input.handoff = HandoffState::Idle;
        input.clear();
        return Some((
            vec![UiEffect::CancelTask {
                kind: TaskKind::Handoff,
                token: None,
            }],
            vec![],
            None,
        ));
    }
    if input.handoff.is_active() {
        input.handoff = HandoffState::Idle;
        input.clear();
        return Some((vec![], vec![], None));
    }
    if input.prompt_builder.is_generating() {
        let restored = match &input.prompt_builder {
            PromptBuilderState::Generating { intent } => Some(intent.clone()),
            _ => None,
        };
        input.prompt_builder = PromptBuilderState::Idle;
        if let Some(intent) = restored {
            input.set_text(&intent);
        } else {
            input.clear();
        }
        return Some((
            vec![UiEffect::CancelTask {
                kind: TaskKind::PromptBuilder,
                token: None,
            }],
            vec![],
            None,
        ));
    }
    if input.prompt_builder.is_ready() {
        // Reject the generated prompt: restore the original intent so the
        // user does not lose what they had typed. The generation task has
        // already completed, so there is nothing to cancel.
        let restored = match &input.prompt_builder {
            PromptBuilderState::Ready { intent } => Some(intent.clone()),
            _ => None,
        };
        input.prompt_builder = PromptBuilderState::Idle;
        if let Some(intent) = restored {
            input.set_text(&intent);
        } else {
            input.clear();
        }
        return Some((vec![], vec![], None));
    }
    if input.prompt_builder.is_active() {
        // Pending: composer already holds the typed intent. Preserve it so
        // the user keeps whatever they were drafting after Esc.
        input.prompt_builder = PromptBuilderState::Idle;
        return Some((vec![], vec![], None));
    }
    None
}

fn handle_voice_hotkey(input: &mut InputState) -> KeyResult {
    if input.voice.is_recording() {
        return (vec![UiEffect::StopVoiceRecording], vec![], None);
    }

    if input.voice.is_transcribing() {
        return (
            vec![],
            vec![StateMutation::Transcript(
                TranscriptMutation::AppendSystemMessage(
                    "Voice transcription already in progress.".to_string(),
                ),
            )],
            None,
        );
    }

    (vec![UiEffect::StartVoiceRecording], vec![], None)
}

// =============================================================================
// Overlays: command palette, model picker, thinking picker
// =============================================================================

fn handle_overlays(
    input: &mut InputState,
    model_id: &str,
    code: KeyCode,
    mods: &Modifiers,
) -> Option<KeyResult> {
    match code {
        // `/` when input is empty: open command palette
        KeyCode::Char('/') if mods.none() && input.get_text().is_empty() => {
            Some((vec![], vec![], Some(OverlayRequest::CommandPalette)))
        }
        // Ctrl+O: open command palette
        KeyCode::Char('o') if mods.only_ctrl() => {
            Some((vec![], vec![], Some(OverlayRequest::CommandPalette)))
        }
        // Ctrl+L: open model picker
        KeyCode::Char('l') if mods.only_ctrl() => {
            Some((vec![], vec![], Some(OverlayRequest::ModelPicker)))
        }
        // Ctrl+T: open thinking picker (if model supports reasoning)
        KeyCode::Char('t') if mods.only_ctrl() => {
            if zdx_engine::models::model_supports_reasoning(model_id) {
                Some((vec![], vec![], Some(OverlayRequest::ThinkingPicker)))
            } else {
                Some((vec![], vec![], None))
            }
        }
        // Ctrl+R: open thread TLDR/recap overlay
        KeyCode::Char('r') if mods.only_ctrl() => {
            Some((vec![], vec![], Some(OverlayRequest::Tldr)))
        }
        // Ctrl+B: open prompt builder (mirrors `/prompt-builder`).
        // Mirrors guards in `execute_prompt_builder` in the command palette.
        KeyCode::Char('b') if mods.only_ctrl() => {
            if input.handoff.is_active() {
                Some((
                    vec![],
                    vec![StateMutation::Transcript(
                        TranscriptMutation::AppendSystemMessage(
                            "Cancel the handoff before starting prompt-builder.".to_string(),
                        ),
                    )],
                    None,
                ))
            } else if input.prompt_builder.is_active() {
                Some((
                    vec![],
                    vec![StateMutation::Transcript(
                        TranscriptMutation::AppendSystemMessage(
                            "Prompt-builder is already active. Press Esc to cancel.".to_string(),
                        ),
                    )],
                    None,
                ))
            } else {
                input.prompt_builder = PromptBuilderState::Pending;
                Some((vec![], vec![], None))
            }
        }
        _ => None,
    }
}

// =============================================================================
// Submission: Enter key
// =============================================================================

fn handle_submission(
    input: &mut InputState,
    ctx: &InputContext<'_>,
    code: KeyCode,
    mods: &Modifiers,
) -> Option<KeyResult> {
    match code {
        KeyCode::Enter if !mods.shift() && !mods.alt() => Some(submit_input(
            input,
            ctx.agent_state,
            ctx.tasks,
            ctx.thread_id.clone(),
            ctx.thread_title,
            ctx.active_thread_ids,
            ctx.config,
            ctx.model_id,
        )),
        _ => None,
    }
}

// =============================================================================
// Default input handling: character insertion, Tab, Backspace, Delete
// =============================================================================

fn handle_default_input(input: &mut InputState, key: KeyEvent) -> KeyResult {
    let mods = Modifiers::from(&key);

    match key.code {
        // Tab: insert spaces (tabs cause rendering issues)
        KeyCode::Tab => {
            input.textarea.insert_str("    ");
            (vec![], vec![], None)
        }
        // Backspace: delete character (with placeholder handling)
        KeyCode::Backspace => {
            input.reset_navigation();
            if !input.try_delete_placeholder_at_bracket(true) {
                input.textarea.input(key);
                input.sync_pending_pastes();
                input.sync_pending_images();
            }
            (vec![], vec![], None)
        }
        // Delete: delete forward (with placeholder handling)
        KeyCode::Delete => {
            input.reset_navigation();
            if !input.try_delete_placeholder_at_bracket(false) {
                input.textarea.input(key);
                input.sync_pending_pastes();
                input.sync_pending_images();
            }
            (vec![], vec![], None)
        }
        // Space: expand placeholder or insert normally
        KeyCode::Char(' ') if mods.none() => {
            if input.try_expand_placeholder_at_cursor() {
                return (vec![], vec![], None);
            }
            input.reset_navigation();
            input.textarea.input(key);
            input.sync_pending_pastes();
            input.sync_pending_images();
            (vec![], vec![], None)
        }
        // `/` when input is not empty: insert normally
        KeyCode::Char('/') if mods.none() => {
            input.textarea.input(key);
            input.sync_pending_pastes();
            input.sync_pending_images();
            (vec![], vec![], None)
        }
        // Default: insert character
        _ => {
            input.reset_navigation();
            input.textarea.input(key);
            input.sync_pending_pastes();
            input.sync_pending_images();

            // Detect `@` trigger for file picker or thread picker (reference insert)
            if key.code == KeyCode::Char('@')
                && !key.modifiers.contains(CrosstermKeyModifiers::CONTROL)
            {
                let trigger_pos = compute_at_trigger_position(input);
                let text = input.get_text();
                if trigger_pos > 0 && text.as_bytes().get(trigger_pos - 1) == Some(&b'@') {
                    return (
                        vec![UiEffect::OpenThreadPicker {
                            mode: crate::overlays::ThreadPickerMode::Insert {
                                trigger_pos: trigger_pos - 1,
                            },
                        }],
                        vec![],
                        None,
                    );
                }
                return (
                    vec![],
                    vec![],
                    Some(OverlayRequest::FilePicker { trigger_pos }),
                );
            }

            (vec![], vec![], None)
        }
    }
}

/// Computes the byte position of the `@` character just typed.
fn compute_at_trigger_position(input: &InputState) -> usize {
    let text = input.get_text();
    let (row, col) = input.textarea.cursor();
    let lines: Vec<&str> = text.lines().collect();
    let mut pos = 0;
    for (i, line) in lines.iter().enumerate() {
        if i < row {
            pos += line.len() + 1; // +1 for newline
        } else {
            pos += col;
            break;
        }
    }
    // trigger_pos is cursor - 1 since we just typed the `@`
    pos.saturating_sub(1)
}

// =============================================================================
// Input submission logic
// =============================================================================

/// Handles input submission.
#[allow(clippy::too_many_arguments)]
fn submit_input(
    input: &mut InputState,
    agent_state: &AgentState,
    tasks: &Tasks,
    thread_id: Option<String>,
    thread_title: Option<&str>,
    active_thread_ids: &std::collections::HashSet<String>,
    config: &Config,
    model_id: &str,
) -> KeyResult {
    // Block input during any modal generation. Each branch shows a hint
    // pointing at Esc as the cancel path and shares the early-return shape.
    if let Some(result) = block_submit_during_generation(input) {
        return result;
    }

    let text = input.get_text_with_pending();
    let trimmed = text.trim();

    let agent_running = agent_state.is_running();
    if agent_running {
        return handle_submit_while_agent_running(input, trimmed, &text);
    }

    let bash_running = tasks.state(TaskKind::Bash).is_running();
    if bash_running {
        return (vec![], vec![], None);
    }

    let thread_create_running = tasks.state(TaskKind::ThreadCreate).is_running();
    if thread_create_running {
        return (
            vec![],
            vec![StateMutation::Transcript(
                TranscriptMutation::AppendSystemMessage(
                    "Creating new thread. Wait for it to finish before sending.".to_string(),
                ),
            )],
            None,
        );
    }

    let title_task_running = tasks.state(TaskKind::ThreadTitle).is_running();
    let should_suggest_title = thread_id.is_some() && thread_title.is_none() && !title_task_running;

    if let Some(thread_id) = thread_id.as_deref()
        && (active_thread_ids.contains(thread_id) || thread_has_background_run(thread_id))
    {
        return (
            vec![],
            vec![StateMutation::Transcript(
                TranscriptMutation::AppendSystemMessage(
                    "This thread is still running in the background. Wait for it to finish before sending a new message.".to_string(),
                ),
            )],
            None,
        );
    }

    // Modal flows that own the composer (handoff, prompt-builder) must
    // claim the submission before slash/bash parsing. Otherwise an intent
    // that happens to start with `/fast` or `$cmd` would short-circuit the
    // modal flow and execute as a normal slash/bash command.
    if let Some(result) = handle_handoff_submission(input, trimmed, &text, thread_id.as_deref()) {
        return result;
    }

    if let Some(result) = handle_prompt_builder_submission(input, trimmed, &text) {
        return result;
    }

    // Try slash commands (/fast, etc.)
    if let Some(result) = handle_slash_commands(input, trimmed, config, model_id) {
        return result;
    }

    // Try bash commands
    if let Some((mut effects, mutations, overlay)) = handle_bash_commands(input, trimmed, &text) {
        if should_suggest_title && let Some(thread_id) = thread_id.as_ref() {
            effects.push(UiEffect::SuggestThreadTitle {
                thread_id: thread_id.clone(),
                message: text.clone(),
            });
        }
        return (effects, mutations, overlay);
    }

    // Normal message submission
    if trimmed.is_empty() {
        return (vec![], vec![], None);
    }

    let images = input.take_images();
    input.history.push(text.clone());
    input.reset_navigation();
    input.clear();
    let (effects, mutations) = build_send_effects(&text, thread_id, should_suggest_title, images);

    (effects, mutations, None)
}

/// Returns `Some(advisory)` if a modal flow's generation phase currently
/// owns the composer, blocking the user from submitting via Enter.
///
/// Centralizes the per-modal "press Esc to cancel" advisories so
/// `submit_input` stays small.
fn block_submit_during_generation(input: &InputState) -> Option<KeyResult> {
    let advisory = |message: &str| -> KeyResult {
        (
            vec![],
            vec![StateMutation::Transcript(
                TranscriptMutation::AppendSystemMessage(message.to_string()),
            )],
            None,
        )
    };
    if input.handoff.is_generating() {
        return Some(advisory(
            "Handoff generation in progress. Press Esc to cancel.",
        ));
    }
    if input.prompt_builder.is_generating() {
        return Some(advisory(
            "Prompt-builder generation in progress. Press Esc to cancel.",
        ));
    }
    None
}

fn handle_submit_while_agent_running(
    input: &mut InputState,
    trimmed: &str,
    text: &str,
) -> KeyResult {
    if trimmed.is_empty() {
        return (vec![], vec![], None);
    }
    if input.handoff.is_active() {
        return (
            vec![],
            vec![StateMutation::Transcript(
                TranscriptMutation::AppendSystemMessage(
                    "Finish or cancel handoff before queueing.".to_string(),
                ),
            )],
            None,
        );
    }
    if input.prompt_builder.is_active() {
        return (
            vec![],
            vec![StateMutation::Transcript(
                TranscriptMutation::AppendSystemMessage(
                    "Finish or cancel prompt-builder before queueing.".to_string(),
                ),
            )],
            None,
        );
    }
    if trimmed.starts_with('$') {
        return (
            vec![],
            vec![StateMutation::Transcript(
                TranscriptMutation::AppendSystemMessage(
                    "Bash commands ($<command>) can't be queued while streaming.".to_string(),
                ),
            )],
            None,
        );
    }

    input.history.push(text.to_string());
    input.reset_navigation();
    input.enqueue_prompt(text.to_string());
    input.clear();
    (vec![], vec![], None)
}

fn handle_slash_commands(
    input: &mut InputState,
    trimmed: &str,
    config: &Config,
    model_id: &str,
) -> Option<KeyResult> {
    let rest = trimmed.strip_prefix("/fast")?;
    let arg = rest.trim();

    input.clear();

    if !arg.is_empty() {
        return Some((
            vec![],
            vec![StateMutation::Transcript(
                TranscriptMutation::AppendSystemMessage("Usage: /fast".to_string()),
            )],
            None,
        ));
    }

    let (effects, mutations) = match build_fast_mode_toggle_actions(config, model_id) {
        Ok(actions) => actions,
        Err(message) => {
            return Some((
                vec![],
                vec![StateMutation::Transcript(
                    TranscriptMutation::AppendSystemMessage(message.to_string()),
                )],
                None,
            ));
        }
    };

    Some((effects, mutations, None))
}

fn handle_bash_commands(input: &mut InputState, trimmed: &str, text: &str) -> Option<KeyResult> {
    if let Some(command) = trimmed.strip_prefix('$') {
        let command = command.trim();
        if command.is_empty() {
            input.clear();
            return Some((
                vec![],
                vec![StateMutation::Transcript(
                    TranscriptMutation::AppendSystemMessage(
                        "Usage: $<command> (e.g., $ls -la)".to_string(),
                    ),
                )],
                None,
            ));
        }
        input.history.push(text.to_string());
        input.reset_navigation();
        input.clear();
        return Some((
            vec![UiEffect::ExecuteBash {
                command: command.to_string(),
            }],
            vec![],
            None,
        ));
    }

    None
}

fn thread_has_background_run(thread_id: &str) -> bool {
    agent_activity::list_active()
        .into_iter()
        .filter_map(|run| run.thread_id)
        .any(|id| id == thread_id)
}

fn handle_handoff_submission(
    input: &mut InputState,
    trimmed: &str,
    text: &str,
    thread_id: Option<&str>,
) -> Option<KeyResult> {
    // Submitting handoff goal (to trigger generation)
    if input.handoff.is_pending() {
        if trimmed.is_empty() {
            return Some((
                vec![],
                vec![StateMutation::Transcript(
                    TranscriptMutation::AppendSystemMessage(
                        "Handoff goal cannot be empty.".to_string(),
                    ),
                )],
                None,
            ));
        }
        input.clear();
        return Some((
            vec![UiEffect::StartHandoff {
                goal: text.to_string(),
            }],
            vec![],
            None,
        ));
    }

    // Submitting generated handoff prompt (to create new thread in a new tab)
    //
    // The new thread is opened in a fresh background tab, so the source thread
    // stays intact in its current tab. The reducer only resets modal state and
    // clears the textarea on the source tab; `update.rs` intercepts
    // `HandoffSubmit` and pushes the new tab before the runtime spawns
    // `thread_create`, so `ThreadUiEvent::Created` populates the new tab.
    if input.handoff.is_ready() {
        if trimmed.is_empty() {
            return Some((
                vec![],
                vec![StateMutation::Transcript(
                    TranscriptMutation::AppendSystemMessage(
                        "Handoff prompt cannot be empty.".to_string(),
                    ),
                )],
                None,
            ));
        }
        input.handoff = HandoffState::Idle;
        input.clear();
        return Some((
            vec![UiEffect::HandoffSubmit {
                prompt: text.to_string(),
                handoff_from: thread_id.map(std::string::ToString::to_string),
            }],
            vec![],
            None,
        ));
    }

    None
}

/// Handles `Enter` while the prompt-builder is in `Pending` state.
///
/// Captures the typed intent, clears the input, and emits the
/// `StartPromptBuilder` effect. Returns `None` when prompt-builder is not in
/// pending state so the caller falls through to normal submission handling.
fn handle_prompt_builder_submission(
    input: &mut InputState,
    trimmed: &str,
    text: &str,
) -> Option<KeyResult> {
    if !input.prompt_builder.is_pending() {
        return None;
    }

    if trimmed.is_empty() {
        return Some((
            vec![],
            vec![StateMutation::Transcript(
                TranscriptMutation::AppendSystemMessage(
                    "Prompt-builder intent cannot be empty.".to_string(),
                ),
            )],
            None,
        ));
    }

    // Stash the intent inside the Generating state so Esc can restore it,
    // then clear the composer for the "generating prompt..." view.
    input.prompt_builder = PromptBuilderState::Generating {
        intent: text.to_string(),
    };
    input.clear();
    Some((
        vec![UiEffect::StartPromptBuilder {
            intent: text.to_string(),
        }],
        vec![],
        None,
    ))
}

/// Tab targeting for `build_send_effects`.
///
/// The active tab uses the standard `StartAgentTurn` / `SaveThread`
/// effects which mutate `app.tui` directly. Background tabs need their
/// own variants so the runtime spawns the agent and persists messages
/// against the correct `TuiState` (the queued prompt would otherwise be
/// routed to whichever tab happens to be visible).
#[derive(Debug, Clone, Copy)]
pub enum TabContext {
    /// Effects target the active tab (`app.tui`).
    Active,
    /// Effects target a specific background tab.
    Background(TabId),
}

pub fn build_send_effects(
    text: &str,
    thread_id: Option<String>,
    should_suggest_title: bool,
    images: Vec<PendingImage>,
) -> (Vec<UiEffect>, Vec<StateMutation>) {
    build_send_effects_for_tab(
        text,
        thread_id,
        should_suggest_title,
        images,
        TabContext::Active,
    )
}

pub fn build_send_effects_for_tab(
    text: &str,
    thread_id: Option<String>,
    should_suggest_title: bool,
    images: Vec<PendingImage>,
    tab: TabContext,
) -> (Vec<UiEffect>, Vec<StateMutation>) {
    let user_event = ThreadEvent::user_message(text);
    let mut effects: Vec<UiEffect> = match tab {
        TabContext::Active => {
            if thread_id.is_some() {
                vec![
                    UiEffect::SaveThread { event: user_event },
                    UiEffect::StartAgentTurn,
                ]
            } else {
                vec![UiEffect::StartAgentTurn]
            }
        }
        TabContext::Background(tab_id) => {
            if thread_id.is_some() {
                vec![
                    UiEffect::SaveThreadInBackgroundTab {
                        tab_id,
                        event: user_event,
                    },
                    UiEffect::StartAgentTurnInBackgroundTab { tab_id },
                ]
            } else {
                vec![UiEffect::StartAgentTurnInBackgroundTab { tab_id }]
            }
        }
    };

    let image_pairs: Vec<(String, String, Option<String>)> = images
        .into_iter()
        .map(|img| (img.mime_type, img.data, img.source_path))
        .collect();

    let image_paths: Vec<String> = image_pairs
        .iter()
        .filter_map(|(_, _, path)| path.clone())
        .collect();

    let (cell, message) = if image_paths.is_empty() {
        (HistoryCell::user(text), ChatMessage::user(text))
    } else {
        (
            HistoryCell::user_with_images(text, image_paths),
            ChatMessage::user_with_images(text, &image_pairs),
        )
    };

    let mutations = vec![
        StateMutation::Transcript(TranscriptMutation::AppendCell(Box::new(cell))),
        StateMutation::Thread(ThreadMutation::AppendMessage(message)),
    ];

    // Title suggestion is intentionally only emitted for the active tab.
    // Background tabs are queue-drain only: their first turn (which is
    // when titles are normally suggested) always happened on the active
    // tab, so by the time a background drain fires the title task either
    // already completed or is still in flight on the original active tab.
    if matches!(tab, TabContext::Active)
        && should_suggest_title
        && let Some(thread_id) = thread_id
    {
        effects.push(UiEffect::SuggestThreadTitle {
            thread_id,
            message: text.to_string(),
        });
    }

    (effects, mutations)
}

/// Handles the handoff generation result.
///
/// # Errors
/// Returns an error if the operation fails.
pub fn handle_handoff_result(
    input: &mut InputState,
    goal: &str,
    result: Result<String, String>,
) -> Vec<StateMutation> {
    let was_generating = input.handoff.is_generating();

    match result {
        Ok(generated_prompt) => {
            // Set the generated prompt in the input textarea
            input.set_text(&generated_prompt);

            // Transition to Ready state
            input.handoff = HandoffState::Ready;

            vec![]
        }
        Err(error) => {
            let mut mutations = vec![StateMutation::Transcript(
                TranscriptMutation::AppendSystemMessage(format!(
                    "Handoff generation failed: {error}"
                )),
            )];

            // Restore goal for retry (spec requirement)
            if was_generating {
                input.set_text(goal);
                input.handoff = HandoffState::Pending;
                mutations.push(StateMutation::Transcript(
                    TranscriptMutation::AppendSystemMessage(
                        "Press Enter to retry, or Esc to cancel.".to_string(),
                    ),
                ));
            } else {
                input.handoff = HandoffState::Idle;
            }

            mutations
        }
    }
}

/// Handles the prompt-builder generation result.
///
/// On success the generated prompt is dropped into the composer and the
/// builder state returns to `Idle` so the user can edit and send it as a
/// normal message. On failure the user's intent is restored to the composer
/// and the builder returns to `Pending` so they can retry without retyping.
pub fn handle_prompt_builder_result(
    input: &mut InputState,
    intent: &str,
    result: Result<String, String>,
) -> Vec<StateMutation> {
    let was_generating = input.prompt_builder.is_generating();

    match result {
        Ok(generated_prompt) => {
            input.set_text(&generated_prompt);
            // Stay in `Ready` so the user can review the polished prompt.
            // Esc reverts to the original intent; any other key implicitly
            // accepts and drops to `Idle` (handled in `handle_main_key`).
            input.prompt_builder = PromptBuilderState::Ready {
                intent: intent.to_string(),
            };
            vec![]
        }
        Err(error) => {
            let mut mutations = vec![StateMutation::Transcript(
                TranscriptMutation::AppendSystemMessage(format!(
                    "Prompt-builder generation failed: {error}"
                )),
            )];

            if was_generating {
                input.set_text(intent);
                input.prompt_builder = PromptBuilderState::Pending;
                mutations.push(StateMutation::Transcript(
                    TranscriptMutation::AppendSystemMessage(
                        "Press Enter to retry, or Esc to cancel.".to_string(),
                    ),
                ));
            } else {
                input.prompt_builder = PromptBuilderState::Idle;
            }

            mutations
        }
    }
}

// =============================================================================
// Mouse event handling (image placeholder clicks)
// =============================================================================

/// Handles mouse clicks in the input area.
///
/// Detects clicks on `[Image #N]` placeholders and opens image preview.
/// Returns `None` for non-placeholder clicks (let default behavior proceed).
pub fn handle_mouse(
    input: &InputState,
    mouse: crossterm::event::MouseEvent,
    area: ratatui::layout::Rect,
) -> Option<crate::overlays::OverlayRequest> {
    use crossterm::event::{MouseButton, MouseEventKind};
    use unicode_width::UnicodeWidthStr;

    if !matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left)) {
        return None;
    }

    if input.pending_images.is_empty() {
        return None;
    }

    // Inner area: block has Borders::ALL so offset by 1 on each side
    let inner_x = area.x + 1;
    let inner_y = area.y + 1;
    let inner_w = area.width.saturating_sub(2) as usize;
    let inner_h = area.height.saturating_sub(2) as usize;

    if mouse.column < inner_x || mouse.row < inner_y || inner_w == 0 || inner_h == 0 {
        return None;
    }

    let click_col = (mouse.column - inner_x) as usize;
    let click_row = (mouse.row - inner_y) as usize;

    // Build visual rows tracking placeholder hit regions (display-width based).
    // This mirrors wrap_textarea's character-by-character wrapping.
    let (cursor_line, _) = input.textarea.cursor();

    // Collect placeholder info: (placeholder_text, image_index)
    let placeholders: Vec<(&str, usize)> = input
        .pending_images
        .iter()
        .enumerate()
        .map(|(i, img)| (img.placeholder.as_str(), i))
        .collect();

    // Each visual row stores: Vec<(display_col_start, display_col_end, image_index)>
    let mut rows: Vec<Vec<(usize, usize, usize)>> = Vec::new();
    let mut cursor_visual_row = 0usize;

    for (line_idx, logical_line) in input.textarea.lines().iter().enumerate() {
        if logical_line.is_empty() {
            rows.push(vec![]);
            if line_idx == cursor_line {
                cursor_visual_row = rows.len() - 1;
            }
            continue;
        }

        // Find all placeholder byte ranges in this line
        let mut ph_hits: Vec<(usize, usize, usize)> = Vec::new(); // (byte_start, byte_end, img_idx)
        for &(ph_text, img_idx) in &placeholders {
            let mut search = 0;
            while let Some(pos) = logical_line[search..].find(ph_text) {
                let abs = search + pos;
                ph_hits.push((abs, abs + ph_text.len(), img_idx));
                search = abs + 1;
            }
        }
        ph_hits.sort_by_key(|(s, _, _)| *s);

        // Walk character by character, tracking display width and active placeholder
        let mut display_w = 0usize;
        let mut row_hits: Vec<(usize, usize, usize)> = Vec::new();
        let mut active: Option<(usize, usize, usize)> = None; // (display_start, byte_end, img_idx)

        for (byte_off, ch) in logical_line.char_indices() {
            let ch_w = ch.to_string().width();

            // Line wrap
            if display_w + ch_w > inner_w && display_w > 0 {
                // Truncate any active placeholder spanning across wrap boundary
                active = None;
                rows.push(std::mem::take(&mut row_hits));
                display_w = 0;
            }

            // Check if entering a placeholder
            if active.is_none()
                && let Some(&(_, end, idx)) = ph_hits.iter().find(|(s, _, _)| *s == byte_off)
            {
                active = Some((display_w, end, idx));
            }

            display_w += ch_w;

            // Check if leaving a placeholder
            if let Some((start, end, idx)) = active
                && byte_off + ch.len_utf8() >= end
            {
                row_hits.push((start, display_w, idx));
                active = None;
            }
        }

        rows.push(row_hits);
        if line_idx == cursor_line {
            cursor_visual_row = rows.len() - 1;
        }
    }

    // Compute scroll offset (mirrors render_input logic)
    let total_rows = rows.len();
    let scroll_offset = if total_rows <= inner_h {
        0
    } else {
        let ideal = inner_h / 2;
        if cursor_visual_row < ideal {
            0
        } else if cursor_visual_row >= total_rows.saturating_sub(ideal) {
            total_rows.saturating_sub(inner_h)
        } else {
            cursor_visual_row.saturating_sub(ideal)
        }
    };

    let target_row = scroll_offset + click_row;
    let hits = rows.get(target_row)?;

    for &(col_start, col_end, img_idx) in hits {
        if click_col >= col_start && click_col < col_end {
            let img = input.pending_images.get(img_idx)?;
            let path = img.source_path.as_ref()?;
            return Some(crate::overlays::OverlayRequest::ImagePreview {
                image_path: path.clone(),
                image_index: img_idx + 1,
            });
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    use super::*;
    use crate::common::{TaskId, Tasks};
    use crate::state::AgentState;

    #[test]
    fn submit_is_blocked_while_thread_create_is_running() {
        let mut input = InputState::default();
        input.set_text("hello");
        let mut tasks = Tasks::default();
        tasks.state_mut(TaskKind::ThreadCreate).active = Some(TaskId(1));
        let active_thread_ids = std::collections::HashSet::new();
        let config = Config::default();
        let ctx = InputContext {
            agent_state: &AgentState::Idle,
            tasks: &tasks,
            thread_id: Some("thread-123".to_string()),
            thread_title: None,
            config: &config,
            model_id: &config.model,
            active_thread_ids: &active_thread_ids,
        };

        let (effects, mutations, overlay) = handle_main_key(
            &mut input,
            &ctx,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        );

        assert!(effects.is_empty());
        assert!(overlay.is_none());
        assert_eq!(input.get_text(), "hello");
        assert!(mutations.iter().any(|mutation| matches!(
            mutation,
            StateMutation::Transcript(TranscriptMutation::AppendSystemMessage(message))
                if message == "Creating new thread. Wait for it to finish before sending."
        )));
    }

    fn make_idle_ctx<'a>(
        tasks: &'a Tasks,
        active_thread_ids: &'a std::collections::HashSet<String>,
        config: &'a Config,
    ) -> InputContext<'a> {
        InputContext {
            agent_state: &AgentState::Idle,
            tasks,
            thread_id: None,
            thread_title: None,
            config,
            model_id: &config.model,
            active_thread_ids,
        }
    }

    #[test]
    fn prompt_builder_pending_submission_emits_start_effect() {
        let mut input = InputState::default();
        input.prompt_builder = PromptBuilderState::Pending;
        input.set_text("make me a bug investigation loop with Oracle");
        let tasks = Tasks::default();
        let active_thread_ids = std::collections::HashSet::new();
        let config = Config::default();
        let ctx = make_idle_ctx(&tasks, &active_thread_ids, &config);

        let (effects, mutations, overlay) = handle_main_key(
            &mut input,
            &ctx,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        );

        assert!(overlay.is_none());
        assert!(mutations.is_empty());
        assert_eq!(effects.len(), 1);
        match &effects[0] {
            UiEffect::StartPromptBuilder { intent } => {
                assert_eq!(intent, "make me a bug investigation loop with Oracle");
            }
            other => panic!("expected StartPromptBuilder, got {other:?}"),
        }
        // Composer is cleared until the result arrives.
        assert!(input.get_text().is_empty());
    }

    #[test]
    fn prompt_builder_pending_intent_starting_with_slash_is_not_treated_as_slash_command() {
        // Modal flows must claim Enter even when the typed intent happens to
        // collide with normal slash/bash syntax — otherwise `/fast` typed as
        // an intent would silently toggle fast mode.
        let mut input = InputState::default();
        input.prompt_builder = PromptBuilderState::Pending;
        input.set_text("/fast");
        let tasks = Tasks::default();
        let active_thread_ids = std::collections::HashSet::new();
        let config = Config::default();
        let ctx = make_idle_ctx(&tasks, &active_thread_ids, &config);

        let (effects, _mutations, overlay) = handle_main_key(
            &mut input,
            &ctx,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        );

        assert!(overlay.is_none());
        assert_eq!(effects.len(), 1);
        assert!(
            matches!(&effects[0], UiEffect::StartPromptBuilder { intent } if intent == "/fast"),
            "modal submission must take precedence over slash command parsing"
        );
    }

    #[test]
    fn prompt_builder_empty_pending_submission_keeps_state() {
        let mut input = InputState::default();
        input.prompt_builder = PromptBuilderState::Pending;
        let tasks = Tasks::default();
        let active_thread_ids = std::collections::HashSet::new();
        let config = Config::default();
        let ctx = make_idle_ctx(&tasks, &active_thread_ids, &config);

        let (effects, mutations, overlay) = handle_main_key(
            &mut input,
            &ctx,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        );

        assert!(overlay.is_none());
        assert!(effects.is_empty());
        assert!(input.prompt_builder.is_pending());
        assert!(mutations.iter().any(|mutation| matches!(
            mutation,
            StateMutation::Transcript(TranscriptMutation::AppendSystemMessage(message))
                if message.to_lowercase().contains("intent")
        )));
    }

    #[test]
    fn prompt_builder_result_inserts_prompt_into_composer_and_enters_review() {
        let mut input = InputState::default();
        input.prompt_builder = PromptBuilderState::Generating {
            intent: "the original intent".to_string(),
        };

        let mutations = handle_prompt_builder_result(
            &mut input,
            "the original intent",
            Ok("the polished prompt".to_string()),
        );

        assert!(mutations.is_empty());
        assert_eq!(input.get_text(), "the polished prompt");
        // Success now lands in `Ready` so the user can review the polished
        // prompt before it implicitly becomes a normal composer message.
        match &input.prompt_builder {
            PromptBuilderState::Ready { intent } => {
                assert_eq!(intent, "the original intent");
            }
            other => panic!("expected Ready, got {other:?}"),
        }
    }

    #[test]
    fn prompt_builder_failure_restores_intent_and_returns_to_pending() {
        let mut input = InputState::default();
        input.prompt_builder = PromptBuilderState::Generating {
            intent: "the original intent".to_string(),
        };

        let mutations = handle_prompt_builder_result(
            &mut input,
            "the original intent",
            Err("subagent timed out".to_string()),
        );

        assert_eq!(input.get_text(), "the original intent");
        assert!(matches!(input.prompt_builder, PromptBuilderState::Pending));
        assert!(mutations.iter().any(|m| matches!(
            m,
            StateMutation::Transcript(TranscriptMutation::AppendSystemMessage(text))
                if text.to_lowercase().contains("prompt-builder")
        )));
    }

    #[test]
    fn typing_is_dropped_silently_during_modal_generation() {
        // Regression: while a modal generation phase owns the composer the
        // user must not be able to type into it. Otherwise the result event
        // (or Esc) overwrites the typed text with the captured intent.
        let mut input = InputState::default();
        input.prompt_builder = PromptBuilderState::Generating {
            intent: "captured intent".to_string(),
        };
        let tasks = Tasks::default();
        let active_thread_ids = std::collections::HashSet::new();
        let config = Config::default();
        let ctx = make_idle_ctx(&tasks, &active_thread_ids, &config);

        // Typing a printable character must not mutate the composer.
        let (effects, mutations, overlay) = handle_main_key(
            &mut input,
            &ctx,
            KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE),
        );
        assert!(effects.is_empty());
        assert!(mutations.is_empty());
        assert!(overlay.is_none());
        assert!(input.get_text().is_empty());

        // Backspace likewise.
        handle_main_key(
            &mut input,
            &ctx,
            KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE),
        );
        assert!(input.get_text().is_empty());
    }

    #[test]
    fn esc_during_modal_generation_still_cancels_after_typing_guard() {
        // Sanity-check that the typing guard does not break Esc — the guard
        // explicitly delegates Esc to `handle_control_keys` so the cancel
        // path remains reachable.
        let mut input = InputState::default();
        input.prompt_builder = PromptBuilderState::Generating {
            intent: "captured intent".to_string(),
        };
        let tasks = Tasks::default();
        let active_thread_ids = std::collections::HashSet::new();
        let config = Config::default();
        let ctx = make_idle_ctx(&tasks, &active_thread_ids, &config);

        let (effects, _mutations, _overlay) = handle_main_key(
            &mut input,
            &ctx,
            KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
        );

        assert!(matches!(input.prompt_builder, PromptBuilderState::Idle));
        // Cancellation effect is emitted.
        assert!(effects.iter().any(|e| matches!(
            e,
            UiEffect::CancelTask {
                kind: TaskKind::PromptBuilder,
                ..
            }
        )));
    }

    #[test]
    fn esc_during_prompt_builder_pending_keeps_typed_intent() {
        // Esc while typing the intent must not wipe the composer — the user
        // should be able to recover whatever they had drafted.
        let mut input = InputState::default();
        input.prompt_builder = PromptBuilderState::Pending;
        input.set_text("half-written intent");
        let tasks = Tasks::default();
        let active_thread_ids = std::collections::HashSet::new();
        let config = Config::default();
        let ctx = make_idle_ctx(&tasks, &active_thread_ids, &config);

        let (effects, mutations, _overlay) = handle_main_key(
            &mut input,
            &ctx,
            KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
        );

        assert!(matches!(input.prompt_builder, PromptBuilderState::Idle));
        assert_eq!(input.get_text(), "half-written intent");
        assert!(effects.is_empty());
        assert!(mutations.is_empty());
    }

    #[test]
    fn esc_during_prompt_builder_generating_restores_intent() {
        // Esc while generation is in flight must restore the captured intent
        // back into the composer so the user does not lose their prompt.
        let mut input = InputState::default();
        input.prompt_builder = PromptBuilderState::Generating {
            intent: "the captured intent".to_string(),
        };
        let tasks = Tasks::default();
        let active_thread_ids = std::collections::HashSet::new();
        let config = Config::default();
        let ctx = make_idle_ctx(&tasks, &active_thread_ids, &config);

        let (effects, _mutations, _overlay) = handle_main_key(
            &mut input,
            &ctx,
            KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
        );

        assert!(matches!(input.prompt_builder, PromptBuilderState::Idle));
        assert_eq!(input.get_text(), "the captured intent");
        assert!(effects.iter().any(|e| matches!(
            e,
            UiEffect::CancelTask {
                kind: TaskKind::PromptBuilder,
                ..
            }
        )));
    }

    #[test]
    fn esc_during_prompt_builder_ready_restores_intent_and_emits_no_effect() {
        // Reject the polished prompt: the original intent should come back
        // into the composer and no cancellation effect should fire (the
        // generation task already completed).
        let mut input = InputState::default();
        input.prompt_builder = PromptBuilderState::Ready {
            intent: "the original intent".to_string(),
        };
        input.set_text("the polished prompt");
        let tasks = Tasks::default();
        let active_thread_ids = std::collections::HashSet::new();
        let config = Config::default();
        let ctx = make_idle_ctx(&tasks, &active_thread_ids, &config);

        let (effects, mutations, _overlay) = handle_main_key(
            &mut input,
            &ctx,
            KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
        );

        assert!(matches!(input.prompt_builder, PromptBuilderState::Idle));
        assert_eq!(input.get_text(), "the original intent");
        assert!(effects.is_empty());
        assert!(mutations.is_empty());
    }

    #[test]
    fn typing_in_prompt_builder_ready_implicitly_accepts_and_edits_composer() {
        // Any non-Esc keystroke must drop `Ready` to `Idle` and then flow
        // through normal dispatch so the user can edit the polished prompt
        // without a separate confirmation step.
        let mut input = InputState::default();
        input.prompt_builder = PromptBuilderState::Ready {
            intent: "the original intent".to_string(),
        };
        input.set_text("polished");
        let tasks = Tasks::default();
        let active_thread_ids = std::collections::HashSet::new();
        let config = Config::default();
        let ctx = make_idle_ctx(&tasks, &active_thread_ids, &config);

        let (_effects, _mutations, _overlay) = handle_main_key(
            &mut input,
            &ctx,
            KeyEvent::new(KeyCode::Char('!'), KeyModifiers::NONE),
        );

        assert!(matches!(input.prompt_builder, PromptBuilderState::Idle));
        // The keystroke was processed against the polished text — exact
        // cursor placement is implementation-dependent, but the new char
        // must be present and the polished text must still be there.
        let text = input.get_text();
        assert!(
            text.contains('!'),
            "expected '!' to be inserted, got {text:?}"
        );
        assert!(text.contains("polished"));
    }

    #[test]
    fn enter_in_prompt_builder_ready_accepts_and_falls_through_to_send() {
        // Enter on the polished prompt should accept (drop to Idle) and then
        // be handled like a normal submission — emitting `StartAgentTurn`.
        let mut input = InputState::default();
        input.prompt_builder = PromptBuilderState::Ready {
            intent: "the original intent".to_string(),
        };
        input.set_text("the polished prompt");
        let tasks = Tasks::default();
        let active_thread_ids = std::collections::HashSet::new();
        let config = Config::default();
        let ctx = make_idle_ctx(&tasks, &active_thread_ids, &config);

        let (effects, _mutations, _overlay) = handle_main_key(
            &mut input,
            &ctx,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        );

        assert!(matches!(input.prompt_builder, PromptBuilderState::Idle));
        // Submission path emits a `StartAgentTurn` effect with the polished
        // prompt as the user message.
        let started_turn = effects
            .iter()
            .any(|e| matches!(e, UiEffect::StartAgentTurn));
        assert!(
            started_turn,
            "expected StartAgentTurn after accepting Ready; got {effects:?}"
        );
    }

    #[test]
    fn is_image_path_accepts_local_image_paths() {
        assert!(is_image_path("/tmp/photo.png"));
        assert!(is_image_path("./screenshot.JPG"));
        assert!(is_image_path("~/Downloads/animation.gif"));
        assert!(is_image_path("/tmp/with\\ space.webp"));
    }

    #[test]
    fn is_image_path_rejects_urls() {
        // Pasted URLs should fall through to plain-text insertion rather than
        // being routed to the local-file attach flow.
        assert!(!is_image_path("https://example.com/photo.png"));
        assert!(!is_image_path("http://example.com/a.jpg"));
        assert!(!is_image_path("file:///tmp/photo.png"));
        assert!(!is_image_path("  https://example.com/photo.png  "));
    }

    #[test]
    fn is_image_path_rejects_non_image_extensions_and_multiline() {
        assert!(!is_image_path("/tmp/notes.txt"));
        assert!(!is_image_path("/tmp/photo.png\nmore"));
    }
}
