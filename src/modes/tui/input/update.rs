//! Input feature reducer.
//!
//! Handles keyboard input, history navigation, and handoff state transitions.
//! All state mutations for input-related events happen here.

use crossterm::event::{KeyCode, KeyModifiers};

use super::state::{HandoffState, InputState};
use crate::core::thread_log::ThreadEvent;
use crate::modes::tui::app::AgentState;
use crate::modes::tui::overlays::{LoginState, Overlay, OverlayRequest};
use crate::modes::tui::shared::effects::UiEffect;
use crate::modes::tui::shared::internal::{StateMutation, ThreadMutation, TranscriptMutation};
use crate::modes::tui::shared::sanitize_for_display;
use crate::modes::tui::transcript::HistoryCell;
use crate::providers::anthropic::ChatMessage;

/// Handles paste events for input.
///
/// Sanitizes pasted text by stripping ANSI escapes and expanding tabs to spaces.
pub fn handle_paste(input: &mut InputState, overlay: &mut Option<Overlay>, text: &str) {
    let sanitized = sanitize_for_display(text);
    if let Some(Overlay::Login(LoginState::AwaitingCode { input, .. })) = overlay {
        input.push_str(&sanitized);
    } else {
        input.textarea.insert_str(&sanitized);
    }
}

/// Handles main key input when no overlay is active.
pub fn handle_main_key(
    input: &mut InputState,
    agent_state: &AgentState,
    bash_running: bool,
    thread_id: Option<String>,
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
            (vec![], vec![], Some(OverlayRequest::ThinkingPicker))
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
            return submit_input(input, agent_state, bash_running, thread_id);
        }
        KeyCode::Char('j') if ctrl => {
            input.textarea.insert_newline();
            (vec![], vec![], None)
        }
        KeyCode::Esc => {
            if input.handoff.is_active() {
                // Cancel handoff mode
                input.handoff.cancel();
                input.clear();
                (vec![], vec![], None)
            } else if agent_state.is_running() {
                (vec![UiEffect::InterruptAgent], vec![], None)
            } else if bash_running {
                (vec![UiEffect::InterruptBash], vec![], None)
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
            }
            (vec![], vec![], None)
        }
        KeyCode::Down if !ctrl && !shift && !alt => {
            if input.should_navigate_down() {
                input.navigate_down();
            } else {
                input.textarea.input(key);
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
        _ => {
            input.reset_navigation();
            input.textarea.input(key);

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
) -> (Vec<UiEffect>, Vec<StateMutation>, Option<OverlayRequest>) {
    if !matches!(agent_state, AgentState::Idle) || bash_running {
        return (vec![], vec![], None);
    }

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

    let text = input.get_text();

    // Slash command: /rename <title>
    let trimmed = text.trim();
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
        if let Some(thread_id) = thread_id {
            input.clear();
            return (
                vec![UiEffect::RenameThread {
                    thread_id,
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
            vec![UiEffect::StartHandoff { goal: text.clone() }],
            vec![StateMutation::Transcript(
                TranscriptMutation::AppendSystemMessage(format!(
                    "Generating handoff for goal: \"{}\"...",
                    text
                )),
            )],
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
        input.clear();
        input.clear_history();
        input.history.push(text.clone());

        return (
            vec![UiEffect::HandoffSubmit {
                prompt: text.clone(),
            }],
            vec![
                StateMutation::Transcript(TranscriptMutation::Clear),
                StateMutation::Thread(ThreadMutation::ClearMessages),
                StateMutation::Thread(ThreadMutation::ResetUsage),
                StateMutation::Transcript(TranscriptMutation::AppendCell(HistoryCell::user(&text))),
                StateMutation::Thread(ThreadMutation::AppendMessage(ChatMessage::user(&text))),
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

    let effects = if thread_id.is_some() {
        vec![
            UiEffect::SaveThread {
                event: ThreadEvent::user_message(&text),
            },
            UiEffect::StartAgentTurn,
        ]
    } else {
        vec![UiEffect::StartAgentTurn]
    };

    input.clear();
    (
        effects,
        vec![
            StateMutation::Transcript(TranscriptMutation::AppendCell(HistoryCell::user(&text))),
            StateMutation::Thread(ThreadMutation::AppendMessage(ChatMessage::user(&text))),
        ],
        None,
    )
}

/// Handles the handoff generation result.
pub fn handle_handoff_result(
    input: &mut InputState,
    result: Result<String, String>,
) -> Vec<StateMutation> {
    // Extract goal from Generating state before transitioning
    let goal = if let HandoffState::Generating { goal, .. } = &input.handoff {
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

            vec![StateMutation::Transcript(
                TranscriptMutation::AppendSystemMessage(
                    "Handoff ready. Edit and press Enter to start new thread, or Esc to cancel."
                        .to_string(),
                ),
            )]
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
