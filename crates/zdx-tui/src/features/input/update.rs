//! Input feature reducer.
//!
//! Handles keyboard input, history navigation, and handoff state transitions.
//! All state mutations for input-related events happen here.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers as CrosstermKeyModifiers};
use zdx_core::core::thread_log::ThreadEvent;
use zdx_core::providers::ChatMessage;

use super::CursorMove;
use super::state::{HandoffState, InputState, LARGE_PASTE_CHAR_THRESHOLD, PendingPaste};
use crate::common::{TaskKind, sanitize_for_display};
use crate::effects::UiEffect;
use crate::mutations::{InputMutation, StateMutation, ThreadMutation, TranscriptMutation};
use crate::overlays::{LoginState, Overlay, OverlayRequest};
use crate::state::AgentState;
use crate::transcript::HistoryCell;

/// Result type for key handlers.
type KeyResult = (Vec<UiEffect>, Vec<StateMutation>, Option<OverlayRequest>);

/// Context for handling main key input.
///
/// Groups the contextual state needed to decide how to handle a key press,
/// avoiding excessive function parameters.
pub struct InputContext<'a> {
    pub agent_state: &'a AgentState,
    pub bash_running: bool,
    pub thread_id: Option<String>,
    pub thread_is_empty: bool,
    pub model_id: &'a str,
}

/// Handles paste events for input.
///
/// Sanitizes pasted text by stripping ANSI escapes and expanding tabs to spaces.
/// For large pastes (>1000 chars), inserts a placeholder and stores the original
/// content for expansion on submission.
pub fn handle_paste(input: &mut InputState, overlay: &mut Option<Overlay>, text: &str) {
    let sanitized = sanitize_for_display(text);
    if let Some(Overlay::Login(LoginState::AwaitingCode { .. })) = overlay {
        // Ignore paste while waiting for OAuth callback.
        return;
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
}

/// Handles main key input when no overlay is active.
pub fn handle_main_key(input: &mut InputState, ctx: &InputContext<'_>, key: KeyEvent) -> KeyResult {
    let mods = Modifiers::from(&key);

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
struct Modifiers {
    ctrl: bool,
    shift: bool,
    alt: bool,
    super_key: bool,
}

impl Modifiers {
    fn from(key: &KeyEvent) -> Self {
        Self {
            ctrl: key.modifiers.contains(CrosstermKeyModifiers::CONTROL),
            shift: key.modifiers.contains(CrosstermKeyModifiers::SHIFT),
            alt: key.modifiers.contains(CrosstermKeyModifiers::ALT),
            super_key: key.modifiers.contains(CrosstermKeyModifiers::SUPER),
        }
    }

    fn none(&self) -> bool {
        !self.ctrl && !self.shift && !self.alt && !self.super_key
    }

    fn only_ctrl(&self) -> bool {
        self.ctrl && !self.shift && !self.alt && !self.super_key
    }

    fn only_alt(&self) -> bool {
        self.alt && !self.ctrl && !self.shift && !self.super_key
    }

    fn only_super(&self) -> bool {
        self.super_key && !self.ctrl && !self.shift && !self.alt
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
                .map(|s| s.as_str())
                .unwrap_or("");
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
            Some((vec![], vec![], None))
        }
        // Ctrl+K: kill from cursor to end of line
        KeyCode::Char('k') if mods.only_ctrl() => {
            input.textarea.delete_line_by_end();
            input.sync_pending_pastes();
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
        KeyCode::Home if mods.ctrl => Some((
            vec![],
            vec![StateMutation::Transcript(TranscriptMutation::ScrollToTop)],
            None,
        )),
        // Ctrl+End: scroll to bottom
        KeyCode::End if mods.ctrl => Some((
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
        // Ctrl+C: interrupt agent, clear input, or quit
        KeyCode::Char('c') if mods.ctrl => {
            if ctx.agent_state.is_running() {
                Some((vec![UiEffect::InterruptAgent], vec![], None))
            } else if ctx.bash_running {
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
            if input.handoff.is_generating() {
                input.handoff = HandoffState::Idle;
                input.clear();
                Some((
                    vec![UiEffect::CancelTask {
                        kind: TaskKind::Handoff,
                        token: None,
                    }],
                    vec![],
                    None,
                ))
            } else if input.handoff.is_active() {
                input.handoff = HandoffState::Idle;
                input.clear();
                Some((vec![], vec![], None))
            } else if ctx.agent_state.is_running() {
                Some((vec![UiEffect::InterruptAgent], vec![], None))
            } else if ctx.bash_running {
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

// =============================================================================
// Overlays: command palette, thinking picker
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
        // Ctrl+P: open command palette
        KeyCode::Char('p') if mods.only_ctrl() => {
            Some((vec![], vec![], Some(OverlayRequest::CommandPalette)))
        }
        // Ctrl+T: open thinking picker (if model supports reasoning)
        KeyCode::Char('t') if mods.only_ctrl() => {
            if zdx_core::models::model_supports_reasoning(model_id) {
                Some((vec![], vec![], Some(OverlayRequest::ThinkingPicker)))
            } else {
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
        KeyCode::Enter if !mods.shift && !mods.alt => Some(submit_input(
            input,
            ctx.agent_state,
            ctx.bash_running,
            ctx.thread_id.clone(),
            ctx.thread_is_empty,
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
            }
            (vec![], vec![], None)
        }
        // Delete: delete forward (with placeholder handling)
        KeyCode::Delete => {
            input.reset_navigation();
            if !input.try_delete_placeholder_at_bracket(false) {
                input.textarea.input(key);
                input.sync_pending_pastes();
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
            (vec![], vec![], None)
        }
        // `/` when input is not empty: insert normally
        KeyCode::Char('/') if mods.none() => {
            input.textarea.input(key);
            input.sync_pending_pastes();
            (vec![], vec![], None)
        }
        // Default: insert character
        _ => {
            input.reset_navigation();
            input.textarea.input(key);
            input.sync_pending_pastes();

            // Detect `@` trigger for file picker or thread picker (reference insert)
            if key.code == KeyCode::Char('@')
                && !key.modifiers.contains(CrosstermKeyModifiers::CONTROL)
            {
                let trigger_pos = compute_at_trigger_position(input);
                let text = input.get_text();
                if trigger_pos > 0 && text.as_bytes().get(trigger_pos - 1) == Some(&b'@') {
                    return (
                        vec![UiEffect::OpenThreadPicker {
                            task: None,
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
fn submit_input(
    input: &mut InputState,
    agent_state: &AgentState,
    bash_running: bool,
    thread_id: Option<String>,
    thread_is_empty: bool,
) -> KeyResult {
    // Block input during handoff generation (prevent state interleaving)
    if input.handoff.is_generating() {
        return (
            vec![],
            vec![StateMutation::Transcript(
                TranscriptMutation::AppendSystemMessage(
                    "Handoff generation in progress. Press Esc to cancel.".to_string(),
                ),
            )],
            None,
        );
    }

    let text = input.get_text_with_pending();
    let trimmed = text.trim();

    let agent_running = agent_state.is_running();
    if agent_running {
        return handle_submit_while_agent_running(input, trimmed, &text);
    }

    if bash_running {
        return (vec![], vec![], None);
    }

    // Try bash commands
    if let Some(result) = handle_bash_commands(input, trimmed, &text) {
        return result;
    }

    // Try handoff submissions
    if let Some(result) = handle_handoff_submission(input, trimmed, &text, &thread_id) {
        return result;
    }

    // Normal message submission
    if trimmed.is_empty() {
        return (vec![], vec![], None);
    }

    input.history.push(text.clone());
    input.reset_navigation();
    input.clear();
    let (effects, mutations) = build_send_effects(&text, thread_id, thread_is_empty);

    (effects, mutations, None)
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
                task: None,
                command: command.to_string(),
            }],
            vec![],
            None,
        ));
    }

    None
}

fn handle_handoff_submission(
    input: &mut InputState,
    trimmed: &str,
    text: &str,
    thread_id: &Option<String>,
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
                task: None,
                goal: text.to_string(),
            }],
            vec![],
            None,
        ));
    }

    // Submitting generated handoff prompt (to create new thread)
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
        input.clear_history();
        return Some((
            vec![UiEffect::HandoffSubmit {
                prompt: text.to_string(),
                handoff_from: thread_id.clone(),
            }],
            vec![
                StateMutation::Transcript(TranscriptMutation::Clear),
                StateMutation::Thread(ThreadMutation::ClearMessages),
                StateMutation::Thread(ThreadMutation::ResetUsage),
                StateMutation::Input(InputMutation::ClearQueue),
            ],
            None,
        ));
    }

    None
}

pub fn build_send_effects(
    text: &str,
    thread_id: Option<String>,
    thread_is_empty: bool,
) -> (Vec<UiEffect>, Vec<StateMutation>) {
    let mut effects = if thread_id.is_some() {
        vec![
            UiEffect::SaveThread {
                event: ThreadEvent::user_message(text),
            },
            UiEffect::StartAgentTurn,
        ]
    } else {
        vec![UiEffect::StartAgentTurn]
    };

    let mutations = vec![
        StateMutation::Transcript(TranscriptMutation::AppendCell(HistoryCell::user(text))),
        StateMutation::Thread(ThreadMutation::AppendMessage(ChatMessage::user(text))),
    ];

    if thread_is_empty && let Some(thread_id) = thread_id {
        effects.push(UiEffect::SuggestThreadTitle {
            thread_id,
            message: text.to_string(),
        });
    }

    (effects, mutations)
}

/// Handles the handoff generation result.
pub fn handle_handoff_result(
    input: &mut InputState,
    result: Result<String, String>,
) -> Vec<StateMutation> {
    // Extract goal from Generating state before transitioning
    let goal = if let HandoffState::Generating { goal } = &input.handoff {
        Some(goal.clone())
    } else {
        None
    };

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
                    "Handoff generation failed: {}",
                    error
                )),
            )];

            // Restore goal for retry (spec requirement)
            if let Some(goal) = goal {
                input.set_text(&goal);
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
