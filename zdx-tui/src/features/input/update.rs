//! Input feature reducer.
//!
//! Handles keyboard input, history navigation, and handoff state transitions.
//! All state mutations for input-related events happen here.

use crossterm::event::{KeyCode, KeyModifiers};
use zdx_core::core::thread_log::ThreadEvent;
use zdx_core::providers::ChatMessage;

use super::state::{HandoffState, InputState, LARGE_PASTE_CHAR_THRESHOLD, PendingPaste};
use crate::common::{TaskKind, sanitize_for_display};
use crate::effects::UiEffect;
use crate::mutations::{InputMutation, StateMutation, ThreadMutation, TranscriptMutation};
use crate::overlays::{LoginState, Overlay, OverlayRequest};
use crate::state::AgentState;
use crate::transcript::HistoryCell;

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
#[allow(clippy::too_many_arguments)]
pub fn handle_main_key(
    input: &mut InputState,
    agent_state: &AgentState,
    bash_running: bool,
    thread_id: Option<String>,
    thread_is_empty: bool,
    rename_loading: bool,
    model_id: &str,
    key: crossterm::event::KeyEvent,
) -> (Vec<UiEffect>, Vec<StateMutation>, Option<OverlayRequest>) {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let shift = key.modifiers.contains(KeyModifiers::SHIFT);
    let alt = key.modifiers.contains(KeyModifiers::ALT);

    let (effects, mutations, overlay_request) = match key.code {
        // Ctrl+U (or Command+Backspace on macOS): clear the current line
        KeyCode::Char('u') if ctrl && !shift && !alt => {
            let (row, _) = input.textarea.cursor();
            let current_line = input
                .textarea
                .lines()
                .get(row)
                .map(|s| s.as_str())
                .unwrap_or("");
            if current_line.is_empty() && row > 0 {
                // Line is empty, move to end of previous line and delete the newline
                input.textarea.move_cursor(tui_textarea::CursorMove::Up);
                input.textarea.move_cursor(tui_textarea::CursorMove::End);
                input.textarea.delete_next_char(); // delete the newline
            } else {
                // Clear current line
                input.textarea.move_cursor(tui_textarea::CursorMove::Head);
                input.textarea.delete_line_by_end();
            }
            input.sync_pending_pastes();
            (vec![], vec![], None)
        }
        KeyCode::Char('/') if !ctrl && !shift && !alt => {
            if input.get_text().is_empty() {
                (
                    vec![],
                    vec![],
                    Some(OverlayRequest::CommandPalette {
                        command_mode: false,
                    }),
                )
            } else {
                input.textarea.input(key);
                input.sync_pending_pastes();
                (vec![], vec![], None)
            }
        }
        KeyCode::Char('p') if ctrl && !shift && !alt => (
            vec![],
            vec![],
            Some(OverlayRequest::CommandPalette {
                command_mode: false,
            }),
        ),
        KeyCode::Char('t') if ctrl && !shift && !alt => {
            if zdx_core::models::model_supports_reasoning(model_id) {
                (vec![], vec![], Some(OverlayRequest::ThinkingPicker))
            } else {
                (vec![], vec![], None)
            }
        }
        KeyCode::Char('c') if ctrl => {
            // Ctrl+C: interrupt agent, clear input, or quit
            if agent_state.is_running() {
                (vec![UiEffect::InterruptAgent], vec![], None)
            } else if bash_running {
                (vec![UiEffect::InterruptBash], vec![], None)
            } else if !input.get_text().is_empty() {
                input.clear();
                (vec![], vec![], None)
            } else {
                (vec![UiEffect::Quit], vec![], None)
            }
        }
        KeyCode::Enter if !shift && !alt => {
            return submit_input(
                input,
                agent_state,
                bash_running,
                thread_id,
                thread_is_empty,
                rename_loading,
            );
        }
        KeyCode::Char('j') if ctrl => {
            input.textarea.insert_newline();
            (vec![], vec![], None)
        }
        KeyCode::Esc => {
            if input.handoff.is_generating() {
                input.handoff = HandoffState::Idle;
                input.clear();
                (
                    vec![UiEffect::CancelTask {
                        kind: TaskKind::Handoff,
                        token: None,
                    }],
                    vec![],
                    None,
                )
            } else if input.handoff.is_active() {
                // Cancel handoff mode
                input.handoff = HandoffState::Idle;
                input.clear();
                (vec![], vec![], None)
            } else if agent_state.is_running() {
                (vec![UiEffect::InterruptAgent], vec![], None)
            } else if bash_running {
                (
                    vec![UiEffect::CancelTask {
                        kind: TaskKind::Bash,
                        token: None,
                    }],
                    vec![],
                    None,
                )
            } else {
                input.clear();
                (vec![], vec![], None)
            }
        }
        KeyCode::PageUp => (
            vec![],
            vec![StateMutation::Transcript(TranscriptMutation::PageUp)],
            None,
        ),
        KeyCode::PageDown => (
            vec![],
            vec![StateMutation::Transcript(TranscriptMutation::PageDown)],
            None,
        ),
        KeyCode::Home if ctrl => (
            vec![],
            vec![StateMutation::Transcript(TranscriptMutation::ScrollToTop)],
            None,
        ),
        KeyCode::End if ctrl => (
            vec![],
            vec![StateMutation::Transcript(
                TranscriptMutation::ScrollToBottom,
            )],
            None,
        ),
        KeyCode::Up if alt && !ctrl && !shift => {
            // Alt+Up: Move cursor to first line of input
            input.textarea.move_cursor(tui_textarea::CursorMove::Top);
            (vec![], vec![], None)
        }
        KeyCode::Down if alt && !ctrl && !shift => {
            // Alt+Down: Move cursor to last line of input
            input.textarea.move_cursor(tui_textarea::CursorMove::Bottom);
            (vec![], vec![], None)
        }
        KeyCode::Up if !ctrl && !shift && !alt => {
            if input.should_navigate_up() {
                input.navigate_up();
            } else {
                input.textarea.input(key);
                input.sync_pending_pastes();
                // Snap cursor to placeholder end if it landed inside one
                input.snap_to_placeholder_end();
            }
            (vec![], vec![], None)
        }
        KeyCode::Down if !ctrl && !shift && !alt => {
            if input.should_navigate_down() {
                input.navigate_down();
            } else {
                input.textarea.input(key);
                input.sync_pending_pastes();
                // Snap cursor to placeholder end if it landed inside one
                input.snap_to_placeholder_end();
            }
            (vec![], vec![], None)
        }
        KeyCode::Tab => {
            // Convert tabs to spaces for consistent width calculation.
            // Tabs cause rendering issues because unicode_width treats them as 0-width,
            // but terminals render them as variable-width (to next tab stop).
            input.textarea.insert_str("    ");
            (vec![], vec![], None)
        }
        KeyCode::Backspace => {
            input.reset_navigation();
            // If deleting the closing `]` of a placeholder, remove the entire placeholder
            if !input.try_delete_placeholder_at_bracket(true) {
                input.textarea.input(key);
                input.sync_pending_pastes();
            }
            (vec![], vec![], None)
        }
        KeyCode::Delete => {
            input.reset_navigation();
            // If deleting the closing `]` of a placeholder, remove the entire placeholder
            if !input.try_delete_placeholder_at_bracket(false) {
                input.textarea.input(key);
                input.sync_pending_pastes();
            }
            (vec![], vec![], None)
        }
        KeyCode::Left if !ctrl && !shift && !alt => {
            // Jump over placeholder if cursor is inside one, otherwise move normally
            if !input.try_jump_over_placeholder(true) {
                input.textarea.input(key);
            }
            (vec![], vec![], None)
        }
        KeyCode::Right if !ctrl && !shift && !alt => {
            // Jump over placeholder if cursor is inside one, otherwise move normally
            if !input.try_jump_over_placeholder(false) {
                input.textarea.input(key);
            }
            (vec![], vec![], None)
        }
        KeyCode::Char(' ') if !ctrl && !shift && !alt => {
            // If cursor is inside a placeholder, expand it to original content
            if input.try_expand_placeholder_at_cursor() {
                return (vec![], vec![], None);
            }
            // Otherwise, insert space normally
            input.reset_navigation();
            input.textarea.input(key);
            input.sync_pending_pastes();
            (vec![], vec![], None)
        }
        _ => {
            input.reset_navigation();
            input.textarea.input(key);
            input.sync_pending_pastes();

            // Detect `@` trigger for file picker
            if key.code == KeyCode::Char('@') && !key.modifiers.contains(KeyModifiers::CONTROL) {
                // Find the position of the `@` we just typed
                // It's the cursor position minus 1 (since cursor is now after the `@`)
                let text = input.get_text();
                let cursor_pos = {
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
                    pos
                };
                // trigger_pos is the byte position of `@` (cursor - 1 since we just typed it)
                let trigger_pos = cursor_pos.saturating_sub(1);
                return (
                    vec![],
                    vec![],
                    Some(OverlayRequest::FilePicker { trigger_pos }),
                );
            }

            (vec![], vec![], None)
        }
    };

    (effects, mutations, overlay_request)
}

/// Handles input submission.
fn submit_input(
    input: &mut InputState,
    agent_state: &AgentState,
    bash_running: bool,
    thread_id: Option<String>,
    thread_is_empty: bool,
    rename_loading: bool,
) -> (Vec<UiEffect>, Vec<StateMutation>, Option<OverlayRequest>) {
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
        if trimmed.starts_with('/') || trimmed.starts_with('!') {
            return (
                vec![],
                vec![StateMutation::Transcript(
                    TranscriptMutation::AppendSystemMessage(
                        "Commands can't be queued while streaming.".to_string(),
                    ),
                )],
                None,
            );
        }

        input.history.push(text.clone());
        input.reset_navigation();
        input.enqueue_prompt(text.clone());
        input.clear();
        return (vec![], vec![], None);
    }

    if bash_running {
        return (vec![], vec![], None);
    }

    // Slash command: /rename <title>
    if let Some(rest) = trimmed.strip_prefix("/rename") {
        let title = rest.trim();
        if title.is_empty() {
            input.clear();
            return (
                vec![],
                vec![StateMutation::Transcript(
                    TranscriptMutation::AppendSystemMessage("Usage: /rename <title>".to_string()),
                )],
                None,
            );
        }
        if rename_loading {
            input.clear();
            return (vec![], vec![], None);
        }
        if let Some(thread_id) = thread_id.as_ref() {
            input.clear();
            return (
                vec![UiEffect::RenameThread {
                    task: None,
                    thread_id: thread_id.clone(),
                    title: Some(title.to_string()),
                }],
                vec![],
                None,
            );
        } else {
            input.clear();
            return (
                vec![],
                vec![StateMutation::Transcript(
                    TranscriptMutation::AppendSystemMessage(
                        "No active thread to rename.".to_string(),
                    ),
                )],
                None,
            );
        }
    }

    // Bang command: !<command> - execute bash directly
    if let Some(command) = trimmed.strip_prefix('!') {
        let command = command.trim();
        if command.is_empty() {
            input.clear();
            return (
                vec![],
                vec![StateMutation::Transcript(
                    TranscriptMutation::AppendSystemMessage(
                        "Usage: !<command> (e.g., !ls -la)".to_string(),
                    ),
                )],
                None,
            );
        }
        input.history.push(text.clone());
        input.reset_navigation();
        input.clear();
        return (
            vec![UiEffect::ExecuteBash {
                task: None,
                command: command.to_string(),
            }],
            vec![],
            None,
        );
    }

    // Check if we're submitting the handoff goal (to trigger generation)
    // This check must come before the empty check to show proper error
    if input.handoff.is_pending() {
        if text.trim().is_empty() {
            // Show error for empty goal (spec requirement)
            return (
                vec![],
                vec![StateMutation::Transcript(
                    TranscriptMutation::AppendSystemMessage(
                        "Handoff goal cannot be empty.".to_string(),
                    ),
                )],
                None,
            );
        }
        input.clear();

        return (
            vec![UiEffect::StartHandoff {
                task: None,
                goal: text.clone(),
            }],
            vec![],
            None,
        );
    }

    // Check if we're submitting the generated handoff prompt (to create new thread)
    if input.handoff.is_ready() {
        if text.trim().is_empty() {
            // Edge case: user cleared the generated prompt
            return (
                vec![],
                vec![StateMutation::Transcript(
                    TranscriptMutation::AppendSystemMessage(
                        "Handoff prompt cannot be empty.".to_string(),
                    ),
                )],
                None,
            );
        }
        input.handoff = HandoffState::Idle;
        input.clear_history();

        return (
            vec![UiEffect::HandoffSubmit {
                prompt: text.clone(),
            }],
            vec![
                StateMutation::Transcript(TranscriptMutation::Clear),
                StateMutation::Thread(ThreadMutation::ClearMessages),
                StateMutation::Thread(ThreadMutation::ResetUsage),
                StateMutation::Input(InputMutation::ClearQueue),
            ],
            None,
        );
    }

    // Normal message submission
    if text.trim().is_empty() {
        return (vec![], vec![], None);
    }

    input.history.push(text.clone());
    input.reset_navigation();
    input.clear();
    let (effects, mutations) = build_send_effects(&text, thread_id, thread_is_empty);

    (effects, mutations, None)
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
