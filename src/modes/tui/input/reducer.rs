//! Input feature reducer.
//!
//! Handles keyboard input, history navigation, and handoff state transitions.
//! All state mutations for input-related events happen here.

use crossterm::event::{KeyCode, KeyModifiers};

use super::state::HandoffState;
use crate::core::session::SessionEvent;
use crate::modes::tui::overlays::Overlay;
use crate::modes::tui::shared::effects::UiEffect;
use crate::modes::tui::state::{AgentState, AppState, TuiState};
use crate::modes::tui::transcript::HistoryCell;

/// Handles paste events for input.
pub fn handle_paste(app: &mut AppState, text: &str) {
    if let Some(Overlay::Login(crate::modes::tui::overlays::LoginState::AwaitingCode {
        ref mut input,
        ..
    })) = app.overlay
    {
        input.push_str(text);
    } else {
        app.tui.input.textarea.insert_str(text);
    }
}

/// Handles main key input when no overlay is active.
pub fn handle_main_key(app: &mut AppState, key: crossterm::event::KeyEvent) -> Vec<UiEffect> {
    let tui = &mut app.tui;
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let shift = key.modifiers.contains(KeyModifiers::SHIFT);
    let alt = key.modifiers.contains(KeyModifiers::ALT);

    match key.code {
        // Ctrl+U (or Command+Backspace on macOS): clear the current line
        KeyCode::Char('u') if ctrl && !shift && !alt => {
            let (row, _) = tui.input.textarea.cursor();
            let current_line = tui
                .input
                .textarea
                .lines()
                .get(row)
                .map(|s| s.as_str())
                .unwrap_or("");
            if current_line.is_empty() && row > 0 {
                // Line is empty, move to end of previous line and delete the newline
                tui.input.textarea.move_cursor(tui_textarea::CursorMove::Up);
                tui.input
                    .textarea
                    .move_cursor(tui_textarea::CursorMove::End);
                tui.input.textarea.delete_next_char(); // delete the newline
            } else {
                // Clear current line
                tui.input
                    .textarea
                    .move_cursor(tui_textarea::CursorMove::Head);
                tui.input.textarea.delete_line_by_end();
            }
            vec![]
        }
        KeyCode::Char('/') if !ctrl && !shift && !alt => {
            if tui.get_input_text().is_empty() {
                vec![UiEffect::OpenCommandPalette {
                    command_mode: false,
                }]
            } else {
                tui.input.textarea.input(key);
                vec![]
            }
        }
        KeyCode::Char('p') if ctrl && !shift && !alt => {
            vec![UiEffect::OpenCommandPalette {
                command_mode: false,
            }]
        }
        KeyCode::Char('t') if ctrl && !shift && !alt => {
            vec![UiEffect::OpenThinkingPicker]
        }
        KeyCode::Char('c') if ctrl => {
            // Ctrl+C: interrupt agent, clear input, or quit
            if tui.agent_state.is_running() {
                vec![UiEffect::InterruptAgent]
            } else if !tui.get_input_text().is_empty() {
                tui.clear_input();
                vec![]
            } else {
                vec![UiEffect::Quit]
            }
        }
        KeyCode::Enter if !shift && !alt => submit_input(app),
        KeyCode::Char('j') if ctrl => {
            tui.input.textarea.insert_newline();
            vec![]
        }
        KeyCode::Esc => {
            if tui.input.handoff.is_active() {
                // Cancel handoff mode
                tui.input.handoff.cancel();
                tui.clear_input();
                vec![]
            } else if tui.agent_state.is_running() {
                vec![UiEffect::InterruptAgent]
            } else {
                tui.clear_input();
                vec![]
            }
        }
        KeyCode::PageUp => {
            tui.transcript.page_up();
            vec![]
        }
        KeyCode::PageDown => {
            tui.transcript.page_down();
            vec![]
        }
        KeyCode::Home if ctrl => {
            tui.transcript.scroll_to_top();
            vec![]
        }
        KeyCode::End if ctrl => {
            tui.transcript.scroll_to_bottom();
            vec![]
        }
        KeyCode::Up if alt && !ctrl && !shift => {
            // Alt+Up: Move cursor to first line of input
            tui.input
                .textarea
                .move_cursor(tui_textarea::CursorMove::Top);
            vec![]
        }
        KeyCode::Down if alt && !ctrl && !shift => {
            // Alt+Down: Move cursor to last line of input
            tui.input
                .textarea
                .move_cursor(tui_textarea::CursorMove::Bottom);
            vec![]
        }
        KeyCode::Up if !ctrl && !shift && !alt => {
            if tui.input.should_navigate_up() {
                tui.input.navigate_up();
            } else {
                tui.input.textarea.input(key);
            }
            vec![]
        }
        KeyCode::Down if !ctrl && !shift && !alt => {
            if tui.input.should_navigate_down() {
                tui.input.navigate_down();
            } else {
                tui.input.textarea.input(key);
            }
            vec![]
        }
        _ => {
            tui.reset_history_navigation();
            tui.input.textarea.input(key);

            // Detect `@` trigger for file picker
            if key.code == KeyCode::Char('@') && !key.modifiers.contains(KeyModifiers::CONTROL) {
                // Find the position of the `@` we just typed
                // It's the cursor position minus 1 (since cursor is now after the `@`)
                let text = tui.get_input_text();
                let cursor_pos = {
                    let (row, col) = tui.input.textarea.cursor();
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
                return vec![UiEffect::OpenFilePicker { trigger_pos }];
            }

            vec![]
        }
    }
}

/// Handles input submission.
fn submit_input(app: &mut AppState) -> Vec<UiEffect> {
    let tui = &mut app.tui;

    if !matches!(tui.agent_state, AgentState::Idle) {
        return vec![];
    }

    // Block input during handoff generation (prevent state interleaving)
    if tui.input.handoff.is_generating() {
        tui.transcript.cells.push(HistoryCell::system(
            "Handoff generation in progress. Press Esc to cancel.",
        ));
        return vec![];
    }

    let text = tui.get_input_text();

    // Slash command: /rename <title>
    let trimmed = text.trim();
    if let Some(rest) = trimmed.strip_prefix("/rename") {
        let title = rest.trim();
        if title.is_empty() {
            tui.transcript
                .cells
                .push(HistoryCell::system("Usage: /rename <title>"));
            tui.clear_input();
            return vec![];
        }
        if let Some(session) = &tui.conversation.session {
            let session_id = session.id.clone();
            tui.clear_input();
            return vec![UiEffect::RenameSession {
                session_id,
                title: Some(title.to_string()),
            }];
        } else {
            tui.transcript
                .cells
                .push(HistoryCell::system("No active session to rename."));
            tui.clear_input();
            return vec![];
        }
    }

    // Check if we're submitting the handoff goal (to trigger generation)
    // This check must come before the empty check to show proper error
    if tui.input.handoff.is_pending() {
        if text.trim().is_empty() {
            // Show error for empty goal (spec requirement)
            tui.transcript
                .cells
                .push(HistoryCell::system("Handoff goal cannot be empty."));
            return vec![];
        }
        tui.clear_input();

        // Show status in transcript (state mutation happens in reducer)
        tui.transcript.cells.push(HistoryCell::system(format!(
            "Generating handoff for goal: \"{}\"...",
            text
        )));

        return vec![UiEffect::StartHandoff { goal: text }];
    }

    // Check if we're submitting the generated handoff prompt (to create new session)
    if tui.input.handoff.is_ready() {
        if text.trim().is_empty() {
            // Edge case: user cleared the generated prompt
            tui.transcript
                .cells
                .push(HistoryCell::system("Handoff prompt cannot be empty."));
            return vec![];
        }
        tui.input.handoff = HandoffState::Idle;
        tui.clear_input();

        // Clear state (like /new) then add user message
        tui.reset_conversation();
        tui.input.history.push(text.clone());
        tui.transcript.cells.push(HistoryCell::user(&text));
        tui.conversation
            .messages
            .push(crate::providers::anthropic::ChatMessage::user(&text));

        return vec![UiEffect::HandoffSubmit { prompt: text }];
    }

    // Normal message submission
    if text.trim().is_empty() {
        return vec![];
    }

    tui.input.history.push(text.clone());
    tui.reset_history_navigation();

    tui.transcript.cells.push(HistoryCell::user(&text));
    tui.conversation
        .messages
        .push(crate::providers::anthropic::ChatMessage::user(&text));

    let effects = if tui.conversation.session.is_some() {
        vec![
            UiEffect::SaveSession {
                event: SessionEvent::user_message(&text),
            },
            UiEffect::StartAgentTurn,
        ]
    } else {
        vec![UiEffect::StartAgentTurn]
    };

    tui.clear_input();
    effects
}

/// Handles the handoff generation result.
pub fn handle_handoff_result(tui: &mut TuiState, result: Result<String, String>) {
    // Extract goal from Generating state before transitioning
    let goal = if let HandoffState::Generating { goal, .. } = &tui.input.handoff {
        Some(goal.clone())
    } else {
        None
    };

    match result {
        Ok(generated_prompt) => {
            // Set the generated prompt in the input textarea
            tui.input.set_text(&generated_prompt);

            // Transition to Ready state
            tui.input.handoff = HandoffState::Ready;

            // Show success message
            tui.transcript.cells.push(HistoryCell::system(
                "Handoff ready. Edit and press Enter to start new session, or Esc to cancel.",
            ));
        }
        Err(error) => {
            // Show error message
            tui.transcript.cells.push(HistoryCell::system(format!(
                "Handoff generation failed: {}",
                error
            )));

            // Restore goal for retry (spec requirement)
            if let Some(goal) = goal {
                tui.input.set_text(&goal);
                tui.input.handoff = HandoffState::Pending;
                tui.transcript.cells.push(HistoryCell::system(
                    "Press Enter to retry, or Esc to cancel.",
                ));
            } else {
                tui.input.handoff = HandoffState::Idle;
            }
        }
    }
}
