//! Input feature reducer.
//!
//! Handles keyboard input, history navigation, and handoff state transitions.
//! All state mutations for input-related events happen here.

use crossterm::event::{KeyCode, KeyModifiers};

use super::state::{HandoffState, InputState};
use crate::core::session::SessionEvent;
use crate::modes::tui::app::AgentState;
use crate::modes::tui::overlays::Overlay;
use crate::modes::tui::shared::effects::UiEffect;
use crate::modes::tui::shared::internal::{SessionCommand, StateCommand, TranscriptCommand};
use crate::modes::tui::transcript::HistoryCell;
use crate::providers::anthropic::ChatMessage;

/// Handles paste events for input.
pub fn handle_paste(input: &mut InputState, overlay: &mut Option<Overlay>, text: &str) {
    if let Some(Overlay::Login(crate::modes::tui::overlays::LoginState::AwaitingCode {
        input,
        ..
    })) = overlay
    {
        input.push_str(text);
    } else {
        input.textarea.insert_str(text);
    }
}

/// Handles main key input when no overlay is active.
pub fn handle_main_key(
    input: &mut InputState,
    agent_state: &AgentState,
    session_id: Option<String>,
    key: crossterm::event::KeyEvent,
) -> (Vec<UiEffect>, Vec<StateCommand>) {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let shift = key.modifiers.contains(KeyModifiers::SHIFT);
    let alt = key.modifiers.contains(KeyModifiers::ALT);

    let (effects, commands) = match key.code {
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
            (vec![], vec![])
        }
        KeyCode::Char('/') if !ctrl && !shift && !alt => {
            if input.get_text().is_empty() {
                (
                    vec![UiEffect::OpenCommandPalette {
                        command_mode: false,
                    }],
                    vec![],
                )
            } else {
                input.textarea.input(key);
                (vec![], vec![])
            }
        }
        KeyCode::Char('p') if ctrl && !shift && !alt => (
            vec![UiEffect::OpenCommandPalette {
                command_mode: false,
            }],
            vec![],
        ),
        KeyCode::Char('t') if ctrl && !shift && !alt => {
            (vec![UiEffect::OpenThinkingPicker], vec![])
        }
        KeyCode::Char('c') if ctrl => {
            // Ctrl+C: interrupt agent, clear input, or quit
            if agent_state.is_running() {
                (vec![UiEffect::InterruptAgent], vec![])
            } else if !input.get_text().is_empty() {
                input.clear();
                (vec![], vec![])
            } else {
                (vec![UiEffect::Quit], vec![])
            }
        }
        KeyCode::Enter if !shift && !alt => {
            return submit_input(input, agent_state, session_id);
        }
        KeyCode::Char('j') if ctrl => {
            input.textarea.insert_newline();
            (vec![], vec![])
        }
        KeyCode::Esc => {
            if input.handoff.is_active() {
                // Cancel handoff mode
                input.handoff.cancel();
                input.clear();
                (vec![], vec![])
            } else if agent_state.is_running() {
                (vec![UiEffect::InterruptAgent], vec![])
            } else {
                input.clear();
                (vec![], vec![])
            }
        }
        KeyCode::PageUp => (
            vec![],
            vec![StateCommand::Transcript(TranscriptCommand::PageUp)],
        ),
        KeyCode::PageDown => (
            vec![],
            vec![StateCommand::Transcript(TranscriptCommand::PageDown)],
        ),
        KeyCode::Home if ctrl => (
            vec![],
            vec![StateCommand::Transcript(TranscriptCommand::ScrollToTop)],
        ),
        KeyCode::End if ctrl => (
            vec![],
            vec![StateCommand::Transcript(TranscriptCommand::ScrollToBottom)],
        ),
        KeyCode::Up if alt && !ctrl && !shift => {
            // Alt+Up: Move cursor to first line of input
            input.textarea.move_cursor(tui_textarea::CursorMove::Top);
            (vec![], vec![])
        }
        KeyCode::Down if alt && !ctrl && !shift => {
            // Alt+Down: Move cursor to last line of input
            input.textarea.move_cursor(tui_textarea::CursorMove::Bottom);
            (vec![], vec![])
        }
        KeyCode::Up if !ctrl && !shift && !alt => {
            if input.should_navigate_up() {
                input.navigate_up();
            } else {
                input.textarea.input(key);
            }
            (vec![], vec![])
        }
        KeyCode::Down if !ctrl && !shift && !alt => {
            if input.should_navigate_down() {
                input.navigate_down();
            } else {
                input.textarea.input(key);
            }
            (vec![], vec![])
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
                return (vec![UiEffect::OpenFilePicker { trigger_pos }], vec![]);
            }

            (vec![], vec![])
        }
    };

    (effects, commands)
}

/// Handles input submission.
fn submit_input(
    input: &mut InputState,
    agent_state: &AgentState,
    session_id: Option<String>,
) -> (Vec<UiEffect>, Vec<StateCommand>) {
    if !matches!(agent_state, AgentState::Idle) {
        return (vec![], vec![]);
    }

    // Block input during handoff generation (prevent state interleaving)
    if input.handoff.is_generating() {
        return (
            vec![],
            vec![StateCommand::Transcript(
                TranscriptCommand::AppendSystemMessage(
                    "Handoff generation in progress. Press Esc to cancel.".to_string(),
                ),
            )],
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
                vec![StateCommand::Transcript(
                    TranscriptCommand::AppendSystemMessage("Usage: /rename <title>".to_string()),
                )],
            );
        }
        if let Some(session_id) = session_id {
            input.clear();
            return (
                vec![UiEffect::RenameSession {
                    session_id,
                    title: Some(title.to_string()),
                }],
                vec![],
            );
        } else {
            input.clear();
            return (
                vec![],
                vec![StateCommand::Transcript(
                    TranscriptCommand::AppendSystemMessage(
                        "No active session to rename.".to_string(),
                    ),
                )],
            );
        }
    }

    // Check if we're submitting the handoff goal (to trigger generation)
    // This check must come before the empty check to show proper error
    if input.handoff.is_pending() {
        if text.trim().is_empty() {
            // Show error for empty goal (spec requirement)
            return (
                vec![],
                vec![StateCommand::Transcript(
                    TranscriptCommand::AppendSystemMessage(
                        "Handoff goal cannot be empty.".to_string(),
                    ),
                )],
            );
        }
        input.clear();

        return (
            vec![UiEffect::StartHandoff { goal: text.clone() }],
            vec![StateCommand::Transcript(
                TranscriptCommand::AppendSystemMessage(format!(
                    "Generating handoff for goal: \"{}\"...",
                    text
                )),
            )],
        );
    }

    // Check if we're submitting the generated handoff prompt (to create new session)
    if input.handoff.is_ready() {
        if text.trim().is_empty() {
            // Edge case: user cleared the generated prompt
            return (
                vec![],
                vec![StateCommand::Transcript(
                    TranscriptCommand::AppendSystemMessage(
                        "Handoff prompt cannot be empty.".to_string(),
                    ),
                )],
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
                StateCommand::Transcript(TranscriptCommand::Clear),
                StateCommand::Session(SessionCommand::ClearMessages),
                StateCommand::Session(SessionCommand::ResetUsage),
                StateCommand::Transcript(TranscriptCommand::AppendCell(HistoryCell::user(&text))),
                StateCommand::Session(SessionCommand::AppendMessage(ChatMessage::user(&text))),
            ],
        );
    }

    // Normal message submission
    if text.trim().is_empty() {
        return (vec![], vec![]);
    }

    input.history.push(text.clone());
    input.reset_navigation();

    let effects = if session_id.is_some() {
        vec![
            UiEffect::SaveSession {
                event: SessionEvent::user_message(&text),
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
            StateCommand::Transcript(TranscriptCommand::AppendCell(HistoryCell::user(&text))),
            StateCommand::Session(SessionCommand::AppendMessage(ChatMessage::user(&text))),
        ],
    )
}

/// Handles the handoff generation result.
pub fn handle_handoff_result(
    input: &mut InputState,
    result: Result<String, String>,
) -> Vec<StateCommand> {
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

            vec![StateCommand::Transcript(
                TranscriptCommand::AppendSystemMessage(
                    "Handoff ready. Edit and press Enter to start new session, or Esc to cancel."
                        .to_string(),
                ),
            )]
        }
        Err(error) => {
            let mut commands = vec![StateCommand::Transcript(
                TranscriptCommand::AppendSystemMessage(format!(
                    "Handoff generation failed: {}",
                    error
                )),
            )];

            // Restore goal for retry (spec requirement)
            if let Some(goal) = goal {
                input.set_text(&goal);
                input.handoff = HandoffState::Pending;
                commands.push(StateCommand::Transcript(
                    TranscriptCommand::AppendSystemMessage(
                        "Press Enter to retry, or Esc to cancel.".to_string(),
                    ),
                ));
            } else {
                input.handoff = HandoffState::Idle;
            }

            commands
        }
    }
}
