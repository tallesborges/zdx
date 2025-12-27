//! TUI reducer (update function).
//!
//! All state mutations happen here. The runtime calls `update(state, event)`
//! and executes the returned effects.
//!
//! This is the single source of truth for how events modify state.

use crossterm::event::{Event, KeyCode, KeyModifiers, MouseEventKind};

use crate::core::interrupt;
use crate::core::session::SessionEvent;
use crate::ui::effects::UiEffect;
use crate::ui::events::UiEvent;
use crate::ui::overlays::{
    LoginEvent, LoginState, handle_login_key, handle_login_result, handle_model_picker_key,
    handle_palette_key, handle_thinking_picker_key, open_command_palette, open_model_picker,
    open_thinking_picker,
};
use crate::ui::state::{AgentState, OverlayState, TuiState};
use crate::ui::transcript::{HistoryCell, ToolState};

/// Lines to scroll per mouse wheel tick.
/// Set to 1 for smooth scrolling on macOS trackpads (which generate many events
/// per gesture). Traditional mouse wheels work fine with single-line increments.
const MOUSE_SCROLL_LINES: usize = 1;

/// The main reducer function.
///
/// Takes the current state and an event, mutates state, and returns effects
/// for the runtime to execute.
pub fn update(state: &mut TuiState, event: UiEvent, viewport_height: usize) -> Vec<UiEffect> {
    match event {
        UiEvent::Tick => {
            // Advance spinner animation
            state.spinner_frame = state.spinner_frame.wrapping_add(1);
            vec![]
        }
        UiEvent::Terminal(term_event) => handle_terminal_event(state, term_event, viewport_height),
        UiEvent::Agent(agent_event) => handle_agent_event(state, &agent_event),
        UiEvent::LoginResult(result) => {
            handle_login_result(state, result);
            vec![]
        }
    }
}

// ============================================================================
// Terminal Event Handlers
// ============================================================================

fn handle_terminal_event(
    state: &mut TuiState,
    event: Event,
    viewport_height: usize,
) -> Vec<UiEffect> {
    match event {
        Event::Key(key) => handle_key(state, key, viewport_height),
        Event::Mouse(mouse) => {
            handle_mouse(state, mouse, viewport_height);
            vec![]
        }
        Event::Paste(text) => {
            handle_paste(state, &text);
            vec![]
        }
        Event::Resize(_, _) => {
            // Clear wrap cache on resize since line wrapping depends on width
            state.wrap_cache.clear();
            vec![]
        }
        _ => vec![],
    }
}

fn handle_paste(state: &mut TuiState, text: &str) {
    if let OverlayState::Login(LoginState::AwaitingCode { ref mut input, .. }) = state.overlay {
        input.push_str(text);
    } else {
        state.textarea.insert_str(text);
    }
}

fn handle_mouse(state: &mut TuiState, mouse: crossterm::event::MouseEvent, viewport_height: usize) {
    match mouse.kind {
        MouseEventKind::ScrollUp => {
            state.scroll.scroll_up(MOUSE_SCROLL_LINES, viewport_height);
        }
        MouseEventKind::ScrollDown => {
            state
                .scroll
                .scroll_down(MOUSE_SCROLL_LINES, viewport_height);
        }
        _ => {}
    }
}

fn handle_key(
    state: &mut TuiState,
    key: crossterm::event::KeyEvent,
    viewport_height: usize,
) -> Vec<UiEffect> {
    // Route by active overlay - single match, no cascade
    match &state.overlay {
        OverlayState::Login(_) => handle_login_key(state, key),
        OverlayState::CommandPalette(_) => handle_palette_key(state, key),
        OverlayState::ModelPicker(_) => handle_model_picker_key(state, key),
        OverlayState::ThinkingPicker(_) => handle_thinking_picker_key(state, key),
        OverlayState::None => handle_main_key(state, key, viewport_height),
    }
}

fn handle_main_key(
    state: &mut TuiState,
    key: crossterm::event::KeyEvent,
    viewport_height: usize,
) -> Vec<UiEffect> {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let shift = key.modifiers.contains(KeyModifiers::SHIFT);
    let alt = key.modifiers.contains(KeyModifiers::ALT);

    match key.code {
        // Ctrl+U (or Command+Backspace on macOS): clear the current line
        KeyCode::Char('u') if ctrl && !shift && !alt => {
            let (row, _) = state.textarea.cursor();
            let current_line = state
                .textarea
                .lines()
                .get(row)
                .map(|s| s.as_str())
                .unwrap_or("");
            if current_line.is_empty() && row > 0 {
                // Line is empty, move to end of previous line and delete the newline
                state.textarea.move_cursor(tui_textarea::CursorMove::Up);
                state.textarea.move_cursor(tui_textarea::CursorMove::End);
                state.textarea.delete_next_char(); // delete the newline
            } else {
                // Clear current line
                state.textarea.move_cursor(tui_textarea::CursorMove::Head);
                state.textarea.delete_line_by_end();
            }
            vec![]
        }
        KeyCode::Char('/') if !ctrl && !shift && !alt => {
            if state.get_input_text().is_empty() {
                open_command_palette(state, false);
            } else {
                state.textarea.input(key);
            }
            vec![]
        }
        KeyCode::Char('p') if ctrl && !shift && !alt => {
            open_command_palette(state, false);
            vec![]
        }
        KeyCode::Char('t') if ctrl && !shift && !alt => {
            open_thinking_picker(state);
            vec![]
        }
        KeyCode::Char('c') if ctrl => {
            if state.agent_state.is_running() {
                vec![UiEffect::InterruptAgent]
            } else if !state.get_input_text().is_empty() {
                state.clear_input();
                vec![]
            } else {
                vec![UiEffect::Quit]
            }
        }
        KeyCode::Enter if !shift && !alt => submit_input(state),
        KeyCode::Char('j') if ctrl => {
            state.textarea.insert_newline();
            vec![]
        }
        KeyCode::Esc => {
            if state.agent_state.is_running() {
                vec![UiEffect::InterruptAgent]
            } else {
                state.clear_input();
                vec![]
            }
        }
        KeyCode::PageUp => {
            state.scroll.page_up(viewport_height);
            vec![]
        }
        KeyCode::PageDown => {
            state.scroll.page_down(viewport_height);
            vec![]
        }
        KeyCode::Home if ctrl => {
            state.scroll.scroll_to_top();
            vec![]
        }
        KeyCode::End if ctrl => {
            state.scroll.scroll_to_bottom();
            vec![]
        }
        KeyCode::Up if alt && !ctrl && !shift => {
            // Alt+Up: Move cursor to first line of input
            state.textarea.move_cursor(tui_textarea::CursorMove::Top);
            vec![]
        }
        KeyCode::Down if alt && !ctrl && !shift => {
            // Alt+Down: Move cursor to last line of input
            state.textarea.move_cursor(tui_textarea::CursorMove::Bottom);
            vec![]
        }
        KeyCode::Up if !ctrl && !shift && !alt => {
            if should_navigate_history_up(state) {
                navigate_history_up(state);
            } else {
                state.textarea.input(key);
            }
            vec![]
        }
        KeyCode::Down if !ctrl && !shift && !alt => {
            if should_navigate_history_down(state) {
                navigate_history_down(state);
            } else {
                state.textarea.input(key);
            }
            vec![]
        }
        _ => {
            state.reset_history_navigation();
            state.textarea.input(key);
            vec![]
        }
    }
}

// ============================================================================
// Submit / Agent
// ============================================================================

fn submit_input(state: &mut TuiState) -> Vec<UiEffect> {
    if !matches!(state.agent_state, AgentState::Idle) {
        return vec![];
    }

    let text = state.get_input_text();
    if text.trim().is_empty() {
        return vec![];
    }

    state.command_history.push(text.clone());
    state.reset_history_navigation();

    state.transcript.push(HistoryCell::user(&text));
    state
        .messages
        .push(crate::providers::anthropic::ChatMessage::user(&text));

    let effects = if state.session.is_some() {
        vec![
            UiEffect::SaveSession {
                event: SessionEvent::user_message(&text),
            },
            UiEffect::StartAgentTurn,
        ]
    } else {
        vec![UiEffect::StartAgentTurn]
    };

    state.clear_input();
    effects
}

// ============================================================================
// History Navigation
// ============================================================================

fn should_navigate_history_up(state: &TuiState) -> bool {
    if state.command_history.is_empty() {
        return false;
    }
    if state.history_index.is_some() {
        return true;
    }
    if state.get_input_text().is_empty() {
        return true;
    }
    let (row, _col) = state.textarea.cursor();
    row == 0
}

fn should_navigate_history_down(state: &TuiState) -> bool {
    if state.history_index.is_none() {
        return false;
    }
    let (row, _col) = state.textarea.cursor();
    let line_count = state.textarea.lines().len();
    row >= line_count.saturating_sub(1)
}

fn navigate_history_up(state: &mut TuiState) {
    if state.command_history.is_empty() {
        return;
    }

    if state.history_index.is_none() {
        let current = state.get_input_text();
        state.input_draft = Some(current);
        state.history_index = Some(state.command_history.len() - 1);
    } else if let Some(idx) = state.history_index
        && idx > 0
    {
        state.history_index = Some(idx - 1);
    }

    if let Some(idx) = state.history_index
        && let Some(entry) = state.command_history.get(idx).cloned()
    {
        state.set_input_text(&entry);
    }
}

fn navigate_history_down(state: &mut TuiState) {
    let Some(idx) = state.history_index else {
        return;
    };

    if idx + 1 < state.command_history.len() {
        state.history_index = Some(idx + 1);
        if let Some(entry) = state.command_history.get(idx + 1).cloned() {
            state.set_input_text(&entry);
        }
    } else {
        let draft = state.input_draft.take().unwrap_or_default();
        state.history_index = None;
        state.set_input_text(&draft);
    }
}

// ============================================================================
// Command Execution (dispatched from palette via ExecuteCommand effect)
// ============================================================================

/// Executes a slash command by name.
///
/// Called by the runtime when processing `UiEffect::ExecuteCommand`.
pub fn execute_command(state: &mut TuiState, cmd_name: &str) -> Vec<UiEffect> {
    use crate::ui::overlays::login::update_login;

    match cmd_name {
        "config" => vec![UiEffect::OpenConfig],
        "login" => update_login(state, LoginEvent::LoginRequested),
        "logout" => {
            execute_logout(state);
            vec![]
        }
        "model" => {
            open_model_picker(state);
            vec![]
        }
        "thinking" => {
            open_thinking_picker(state);
            vec![]
        }
        "new" => execute_new(state),
        "quit" => execute_quit(state),
        _ => vec![],
    }
}

// ============================================================================
// Slash Commands
// ============================================================================

fn execute_new(state: &mut TuiState) -> Vec<UiEffect> {
    if state.agent_state.is_running() {
        state
            .transcript
            .push(HistoryCell::system("Cannot clear while streaming."));
        return vec![];
    }

    state.transcript.clear();
    state.messages.clear();
    state.command_history.clear();
    state.scroll.reset();
    state.usage = crate::ui::state::SessionUsage::new();
    state.wrap_cache.clear();

    if state.session.is_some() {
        vec![UiEffect::CreateNewSession]
    } else {
        state
            .transcript
            .push(HistoryCell::system("Conversation cleared."));
        vec![]
    }
}

fn execute_logout(state: &mut TuiState) {
    use crate::providers::oauth::anthropic;

    match anthropic::clear_credentials() {
        Ok(true) => {
            state.refresh_auth_type();
            state
                .transcript
                .push(HistoryCell::system("Logged out from Anthropic OAuth."));
        }
        Ok(false) => {
            state
                .transcript
                .push(HistoryCell::system("No OAuth credentials to clear."));
        }
        Err(e) => {
            state
                .transcript
                .push(HistoryCell::system(format!("Logout failed: {}", e)));
        }
    }
}

fn execute_quit(state: &mut TuiState) -> Vec<UiEffect> {
    if state.agent_state.is_running() {
        vec![UiEffect::InterruptAgent, UiEffect::Quit]
    } else {
        vec![UiEffect::Quit]
    }
}

// ============================================================================
// Agent Event Handlers
// ============================================================================

pub fn handle_agent_event(
    state: &mut TuiState,
    event: &crate::core::events::AgentEvent,
) -> Vec<UiEffect> {
    use crate::core::events::AgentEvent;

    match event {
        AgentEvent::AssistantDelta { text } => {
            match &mut state.agent_state {
                AgentState::Waiting { .. } => {
                    // Create streaming cell and transition to Streaming state
                    let cell = HistoryCell::assistant_streaming("");
                    let cell_id = cell.id();
                    state.transcript.push(cell);

                    let old_state = std::mem::replace(&mut state.agent_state, AgentState::Idle);
                    if let AgentState::Waiting { rx } = old_state {
                        state.agent_state = AgentState::Streaming {
                            rx,
                            cell_id,
                            pending_delta: text.clone(),
                        };
                    }
                }
                AgentState::Streaming {
                    cell_id,
                    pending_delta,
                    ..
                } => {
                    // Check if we need a new assistant cell:
                    // - current cell is finalized assistant, or
                    // - current cell is not an assistant (e.g., thinking cell)
                    let needs_new_cell = state
                        .transcript
                        .iter()
                        .find(|c| c.id() == *cell_id)
                        .map(|c| {
                            !matches!(
                                c,
                                HistoryCell::Assistant {
                                    is_streaming: true,
                                    ..
                                }
                            )
                        })
                        .unwrap_or(true);

                    if needs_new_cell {
                        let new_cell = HistoryCell::assistant_streaming("");
                        let new_cell_id = new_cell.id();
                        state.transcript.push(new_cell);
                        *cell_id = new_cell_id;
                        pending_delta.clear();
                        pending_delta.push_str(text);
                    } else {
                        pending_delta.push_str(text);
                    }
                }
                AgentState::Idle => {}
            }
            vec![]
        }
        AgentEvent::AssistantComplete { .. } => {
            // Apply any pending delta before finalizing to ensure no content is lost.
            // This is critical because multiple events can be processed in one loop
            // iteration, and TurnComplete may follow immediately after AssistantComplete.
            apply_pending_delta(state);

            if let AgentState::Streaming { cell_id, .. } = &state.agent_state
                && let Some(cell) = state.transcript.iter_mut().find(|c| c.id() == *cell_id)
            {
                cell.finalize_assistant();
            }
            vec![]
        }
        AgentEvent::Error { message, .. } => {
            // Apply any pending delta before resetting agent state to preserve
            // partial content that was streamed before the error occurred.
            apply_pending_delta(state);

            state
                .transcript
                .push(HistoryCell::system(format!("Error: {}", message)));
            // Reset agent state - the turn is over due to the error
            state.agent_state = AgentState::Idle;
            vec![]
        }
        AgentEvent::Interrupted => {
            // Apply any pending delta before resetting agent state to preserve
            // partial content that was streamed before the interruption.
            apply_pending_delta(state);

            // Mark any running tools or streaming cells as cancelled
            let mut any_marked = false;
            for cell in &mut state.transcript {
                let was_active = matches!(
                    cell,
                    HistoryCell::Assistant {
                        is_streaming: true,
                        ..
                    } | HistoryCell::Thinking {
                        is_streaming: true,
                        ..
                    } | HistoryCell::Tool {
                        state: ToolState::Running,
                        ..
                    }
                );
                cell.mark_cancelled();
                if was_active {
                    any_marked = true;
                }
            }

            // If no streaming/running cells were marked, mark the last user cell
            // (this means we interrupted before any response was generated)
            if !any_marked
                && let Some(last_user) = state
                    .transcript
                    .iter_mut()
                    .rev()
                    .find(|c| matches!(c, HistoryCell::User { .. }))
            {
                last_user.mark_request_interrupted();
            }

            interrupt::reset();
            state.agent_state = AgentState::Idle;
            vec![]
        }
        AgentEvent::ToolRequested { id, name, input } => {
            let tool_cell = HistoryCell::tool_running(id, name, input.clone());
            state.transcript.push(tool_cell);
            vec![]
        }
        AgentEvent::ToolStarted { .. } => vec![],
        AgentEvent::ToolFinished { id, result } => {
            if let Some(cell) = state
                .transcript
                .iter_mut()
                .find(|c| matches!(c, HistoryCell::Tool { tool_use_id, .. } if tool_use_id == id))
            {
                cell.set_tool_result(result.clone());
            }
            vec![]
        }
        AgentEvent::TurnComplete {
            final_text,
            messages,
        } => {
            // Apply any pending delta before resetting agent state to ensure no
            // content is lost. This handles edge cases where AssistantComplete wasn't
            // received or didn't have a chance to apply the delta.
            apply_pending_delta(state);

            // Turn completed - update messages and reset agent state
            state.messages = messages.clone();
            state.agent_state = AgentState::Idle;

            // Save assistant message to session if enabled
            if !final_text.is_empty() && state.session.is_some() {
                vec![UiEffect::SaveSession {
                    event: SessionEvent::assistant_message(final_text),
                }]
            } else {
                vec![]
            }
        }
        // Thinking events - create or update thinking cells in transcript
        AgentEvent::ThinkingDelta { text } => {
            // Transition from Waiting to Streaming
            if let AgentState::Waiting { .. } = &state.agent_state {
                let old_state = std::mem::replace(&mut state.agent_state, AgentState::Idle);
                if let AgentState::Waiting { rx } = old_state {
                    let cell = HistoryCell::thinking_streaming(text);
                    let cell_id = cell.id();
                    state.transcript.push(cell);
                    state.agent_state = AgentState::Streaming {
                        rx,
                        cell_id,
                        pending_delta: String::new(),
                    };
                }
                return vec![];
            }

            // Find the last cell and check if it's a streaming thinking cell
            let should_create_new = state
                .transcript
                .last()
                .map(|cell| {
                    !matches!(
                        cell,
                        HistoryCell::Thinking {
                            is_streaming: true,
                            ..
                        }
                    )
                })
                .unwrap_or(true);

            if should_create_new {
                // Create a new streaming thinking cell
                let cell = HistoryCell::thinking_streaming(text);
                state.transcript.push(cell);
            } else {
                // Append to the existing streaming thinking cell
                if let Some(cell) = state.transcript.last_mut() {
                    cell.append_thinking_delta(text);
                }
            }
            vec![]
        }
        AgentEvent::ThinkingComplete { signature, .. } => {
            // Find the last streaming thinking cell and finalize it
            if let Some(cell) = state.transcript.iter_mut().rev().find(|c| {
                matches!(
                    c,
                    HistoryCell::Thinking {
                        is_streaming: true,
                        ..
                    }
                )
            }) {
                cell.finalize_thinking(signature.clone());
            }
            vec![]
        }
        AgentEvent::UsageUpdate {
            input_tokens,
            output_tokens,
            cache_read_input_tokens,
            cache_creation_input_tokens,
        } => {
            // Accumulate usage for session-wide tracking
            state.usage.add(
                *input_tokens,
                *output_tokens,
                *cache_read_input_tokens,
                *cache_creation_input_tokens,
            );
            vec![]
        }
        AgentEvent::TurnStarted => {
            // Turn start - no UI updates needed yet
            vec![]
        }
        AgentEvent::ToolOutputDelta { .. } => {
            // TODO: Update tool cell with streaming output
            // For now, we only show final output in ToolFinished
            vec![]
        }
    }
}

/// Applies any pending delta to the streaming cell (coalescing).
pub fn apply_pending_delta(state: &mut TuiState) {
    if let AgentState::Streaming {
        cell_id,
        pending_delta,
        ..
    } = &mut state.agent_state
        && !pending_delta.is_empty()
    {
        if let Some(cell) = state.transcript.iter_mut().find(|c| c.id() == *cell_id) {
            cell.append_assistant_delta(pending_delta);
        }
        pending_delta.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::state::ScrollMode;

    #[test]
    fn test_scroll_to_top() {
        let config = crate::config::Config::default();
        let mut state = TuiState::new(config, std::path::PathBuf::new(), None, None);

        state.scroll.scroll_to_top();

        assert!(matches!(
            state.scroll.mode,
            ScrollMode::Anchored { offset: 0 }
        ));
    }

    #[test]
    fn test_scroll_to_bottom() {
        let config = crate::config::Config::default();
        let mut state = TuiState::new(config, std::path::PathBuf::new(), None, None);
        state.scroll.scroll_to_top(); // Start from top

        state.scroll.scroll_to_bottom();

        assert!(matches!(state.scroll.mode, ScrollMode::FollowLatest));
    }

    #[test]
    fn test_scroll_up_and_down() {
        let config = crate::config::Config::default();
        let mut state = TuiState::new(config, std::path::PathBuf::new(), None, None);
        state.scroll.update_line_count(100);

        // Start following, scroll up should anchor
        state.scroll.scroll_up(5, 20);
        assert!(matches!(state.scroll.mode, ScrollMode::Anchored { .. }));

        // Scroll down should move towards bottom
        state.scroll.scroll_down(100, 20);
        assert!(matches!(state.scroll.mode, ScrollMode::FollowLatest));
    }

    #[test]
    fn test_execute_quit_when_idle() {
        let config = crate::config::Config::default();
        let mut state = TuiState::new(config, std::path::PathBuf::new(), None, None);

        let effects = execute_quit(&mut state);

        assert_eq!(effects.len(), 1);
        assert!(matches!(effects[0], UiEffect::Quit));
    }

    #[test]
    fn test_execute_new_clears_state_and_wrap_cache() {
        let config = crate::config::Config::default();
        let mut state = TuiState::new(config, std::path::PathBuf::new(), None, None);

        // Populate some state
        state.transcript.push(HistoryCell::user("test"));
        state
            .messages
            .push(crate::providers::anthropic::ChatMessage::user("test"));
        state.command_history.push("test".to_string());
        state.usage.add(100, 50, 200, 25);

        // Trigger cache population by rendering (simulate)
        let _lines = state.transcript[0].display_lines_cached(80, 0, &state.wrap_cache);
        assert!(!state.wrap_cache.is_empty());

        // Execute clear
        let effects = execute_new(&mut state);

        // Verify everything is cleared
        assert!(state.transcript.is_empty() || state.transcript.len() == 1); // May have "Conversation cleared." message
        assert!(state.messages.is_empty());
        assert!(state.command_history.is_empty());
        assert!(state.wrap_cache.is_empty());
        assert!(state.scroll.is_following());
        assert_eq!(state.usage.input_tokens, 0);
        assert_eq!(state.usage.output_tokens, 0);

        // Verify it returns no effects when no session is active
        assert_eq!(effects.len(), 0);
    }
}
