//! TUI reducer (update function).
//!
//! All state mutations happen here. The runtime calls `update(state, event)`
//! and executes the returned effects.
//!
//! This is the single source of truth for how events modify state.

use crossterm::event::{Event, KeyCode, KeyModifiers, MouseEventKind};

use crate::core::interrupt;
use crate::core::session::SessionEvent;
use crate::ui::chat::effects::UiEffect;
use crate::ui::chat::events::UiEvent;
use crate::ui::chat::overlays::{
    LoginEvent, LoginState, handle_login_key, handle_login_result, handle_model_picker_key,
    handle_palette_key, handle_thinking_picker_key, open_command_palette, open_model_picker,
    open_thinking_picker,
};
use crate::ui::chat::state::{AgentState, OverlayState, TuiState};
use crate::ui::transcript::{HistoryCell, ToolState};

/// Lines to scroll per mouse wheel tick.
const MOUSE_SCROLL_LINES: usize = 3;

/// The main reducer function.
///
/// Takes the current state and an event, mutates state, and returns effects
/// for the runtime to execute.
pub fn update(state: &mut TuiState, event: UiEvent) -> Vec<UiEffect> {
    match event {
        UiEvent::Tick => {
            // Advance spinner animation
            state.spinner_frame = state.spinner_frame.wrapping_add(1);
            // Check if selection should be auto-cleared after copy
            state.transcript.check_selection_timeout();
            vec![]
        }
        UiEvent::Terminal(term_event) => handle_terminal_event(state, term_event),
        UiEvent::Agent(agent_event) => handle_agent_event(state, &agent_event),
        UiEvent::LoginResult(result) => {
            handle_login_result(state, result);
            vec![]
        }
        UiEvent::HandoffResult(result) => {
            handle_handoff_result(state, result);
            vec![]
        }
    }
}

// ============================================================================
// Terminal Event Handlers
// ============================================================================

fn handle_terminal_event(state: &mut TuiState, event: Event) -> Vec<UiEffect> {
    match event {
        Event::Key(key) => handle_key(state, key),
        Event::Mouse(mouse) => {
            handle_mouse(state, mouse);
            vec![]
        }
        Event::Paste(text) => {
            handle_paste(state, &text);
            vec![]
        }
        Event::Resize(_, _) => {
            // Clear wrap cache on resize since line wrapping depends on width
            state.transcript.wrap_cache.clear();
            vec![]
        }
        _ => vec![],
    }
}

fn handle_paste(state: &mut TuiState, text: &str) {
    if let OverlayState::Login(LoginState::AwaitingCode { ref mut input, .. }) = state.overlay {
        input.push_str(text);
    } else {
        state.input.textarea.insert_str(text);
    }
}

fn handle_mouse(state: &mut TuiState, mouse: crossterm::event::MouseEvent) {
    use crate::ui::chat::view::TRANSCRIPT_MARGIN;

    match mouse.kind {
        MouseEventKind::ScrollUp => {
            // Accumulate negative delta (up = negative)
            state
                .transcript
                .scroll_accumulator
                .accumulate(-(MOUSE_SCROLL_LINES as i32));
        }
        MouseEventKind::ScrollDown => {
            // Accumulate positive delta (down = positive)
            state
                .transcript
                .scroll_accumulator
                .accumulate(MOUSE_SCROLL_LINES as i32);
        }
        MouseEventKind::Down(crossterm::event::MouseButton::Left) => {
            // Start selection at clicked position
            if let Some((line, col)) =
                screen_to_transcript_pos(state, mouse.column, mouse.row, TRANSCRIPT_MARGIN)
            {
                state.transcript.start_selection(line, col);
            }
        }
        MouseEventKind::Drag(crossterm::event::MouseButton::Left) => {
            // Extend selection while dragging
            if state.transcript.selection.is_selecting
                && let Some((line, col)) =
                    screen_to_transcript_pos(state, mouse.column, mouse.row, TRANSCRIPT_MARGIN)
            {
                state.transcript.extend_selection(line, col);
            }
        }
        MouseEventKind::Up(crossterm::event::MouseButton::Left) => {
            // Finish selection and auto-copy if there's selected text
            state.transcript.finish_selection();
            if state.transcript.has_selection() {
                // Copy to clipboard and schedule visual clear
                let _ = state.transcript.copy_and_schedule_clear();
            }
        }
        _ => {}
    }
}

/// Converts screen coordinates to transcript position (line index, grapheme column).
///
/// Returns `None` if the position is outside the transcript area.
fn screen_to_transcript_pos(
    state: &TuiState,
    screen_x: u16,
    screen_y: u16,
    margin: u16,
) -> Option<(usize, usize)> {
    use unicode_segmentation::UnicodeSegmentation;
    use unicode_width::UnicodeWidthStr;

    // Check if position is within transcript area horizontally
    if screen_x < margin {
        return None;
    }

    let content_x = (screen_x - margin) as usize;

    // Check if position is within transcript area vertically
    // The transcript area is at the top, from y=0 to y=viewport_height-1
    let viewport_height = state.transcript.viewport_height;
    if screen_y as usize >= viewport_height {
        return None; // Click is in input or status area, not transcript
    }

    // Get scroll offset and line counts
    let scroll_offset = state.transcript.scroll.get_offset(viewport_height);
    let position_map_len = state.transcript.position_map.len();
    let cached_total = state.transcript.scroll.cached_line_count;

    if position_map_len == 0 {
        return None; // No content to select
    }

    // Detect if lazy rendering was used:
    // In lazy mode, position_map.len() < cached_line_count
    // In full mode, position_map.len() == cached_line_count (or close to it)
    let is_lazy_mode = position_map_len < cached_total && cached_total > 0;

    // Calculate visible content lines for bottom-align padding
    let visible_content_lines = if is_lazy_mode {
        position_map_len // Lazy mode: position_map contains exactly visible lines
    } else {
        cached_total
            .saturating_sub(scroll_offset)
            .min(viewport_height)
    };
    let padding = viewport_height.saturating_sub(visible_content_lines);

    // Adjust screen_y for bottom-align padding
    let screen_y = screen_y as usize;
    if screen_y < padding {
        return None; // Click is in padding area, not content
    }
    let content_y = screen_y - padding;

    // Convert to absolute transcript line index (for selection state)
    let absolute_line = scroll_offset + content_y;

    // Get the line mapping - indexing differs based on rendering mode
    let mapping = if is_lazy_mode {
        // Lazy mode: position_map is indexed 0 to visible_count-1
        state.transcript.position_map.get(content_y)?
    } else {
        // Full mode: position_map is indexed by global line number
        state.transcript.position_map.get(absolute_line)?
    };

    // Convert display column (x position) to grapheme index
    // We need to count graphemes until we've accumulated enough display width
    let mut accumulated_width = 0usize;
    let mut grapheme_idx = 0usize;

    for grapheme in mapping.text.graphemes(true) {
        let grapheme_width = grapheme.width();
        if accumulated_width + grapheme_width > content_x {
            break;
        }
        accumulated_width += grapheme_width;
        grapheme_idx += 1;
    }

    Some((absolute_line, grapheme_idx))
}

fn handle_key(state: &mut TuiState, key: crossterm::event::KeyEvent) -> Vec<UiEffect> {
    // Route by active overlay - single match, no cascade
    match &state.overlay {
        OverlayState::Login(_) => handle_login_key(state, key),
        OverlayState::CommandPalette(_) => handle_palette_key(state, key),
        OverlayState::ModelPicker(_) => handle_model_picker_key(state, key),
        OverlayState::ThinkingPicker(_) => handle_thinking_picker_key(state, key),
        OverlayState::None => handle_main_key(state, key),
    }
}

fn handle_main_key(state: &mut TuiState, key: crossterm::event::KeyEvent) -> Vec<UiEffect> {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let shift = key.modifiers.contains(KeyModifiers::SHIFT);
    let alt = key.modifiers.contains(KeyModifiers::ALT);

    match key.code {
        // Ctrl+U (or Command+Backspace on macOS): clear the current line
        KeyCode::Char('u') if ctrl && !shift && !alt => {
            let (row, _) = state.input.textarea.cursor();
            let current_line = state
                .input
                .textarea
                .lines()
                .get(row)
                .map(|s| s.as_str())
                .unwrap_or("");
            if current_line.is_empty() && row > 0 {
                // Line is empty, move to end of previous line and delete the newline
                state
                    .input
                    .textarea
                    .move_cursor(tui_textarea::CursorMove::Up);
                state
                    .input
                    .textarea
                    .move_cursor(tui_textarea::CursorMove::End);
                state.input.textarea.delete_next_char(); // delete the newline
            } else {
                // Clear current line
                state
                    .input
                    .textarea
                    .move_cursor(tui_textarea::CursorMove::Head);
                state.input.textarea.delete_line_by_end();
            }
            vec![]
        }
        KeyCode::Char('/') if !ctrl && !shift && !alt => {
            if state.get_input_text().is_empty() {
                open_command_palette(state, false);
            } else {
                state.input.textarea.input(key);
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
            // Ctrl+C: interrupt agent, clear input, or quit
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
            state.input.textarea.insert_newline();
            vec![]
        }
        KeyCode::Esc => {
            if state.input.handoff_pending
                || state.input.handoff_generating
                || state.input.handoff_ready
            {
                // Cancel handoff mode
                state.input.handoff_pending = false;
                state.input.handoff_generating = false;
                state.input.handoff_ready = false;
                state.input.handoff_rx = None;
                state.input.handoff_goal = None;
                state.clear_input();
                vec![]
            } else if state.agent_state.is_running() {
                vec![UiEffect::InterruptAgent]
            } else {
                state.clear_input();
                vec![]
            }
        }
        KeyCode::PageUp => {
            state.transcript.page_up();
            vec![]
        }
        KeyCode::PageDown => {
            state.transcript.page_down();
            vec![]
        }
        KeyCode::Home if ctrl => {
            state.transcript.scroll_to_top();
            vec![]
        }
        KeyCode::End if ctrl => {
            state.transcript.scroll_to_bottom();
            vec![]
        }
        KeyCode::Up if alt && !ctrl && !shift => {
            // Alt+Up: Move cursor to first line of input
            state
                .input
                .textarea
                .move_cursor(tui_textarea::CursorMove::Top);
            vec![]
        }
        KeyCode::Down if alt && !ctrl && !shift => {
            // Alt+Down: Move cursor to last line of input
            state
                .input
                .textarea
                .move_cursor(tui_textarea::CursorMove::Bottom);
            vec![]
        }
        KeyCode::Up if !ctrl && !shift && !alt => {
            if should_navigate_history_up(state) {
                navigate_history_up(state);
            } else {
                state.input.textarea.input(key);
            }
            vec![]
        }
        KeyCode::Down if !ctrl && !shift && !alt => {
            if should_navigate_history_down(state) {
                navigate_history_down(state);
            } else {
                state.input.textarea.input(key);
            }
            vec![]
        }
        _ => {
            state.reset_history_navigation();
            state.input.textarea.input(key);
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

    // Check if we're submitting the handoff goal (to trigger generation)
    // This check must come before the empty check to show proper error
    if state.input.handoff_pending {
        if text.trim().is_empty() {
            // Show error for empty goal (spec requirement)
            state
                .transcript
                .cells
                .push(HistoryCell::system("Handoff goal cannot be empty."));
            return vec![];
        }
        state.input.handoff_pending = false;
        state.input.handoff_goal = Some(text.clone()); // Store for retry on failure
        state.clear_input();
        return vec![UiEffect::StartHandoff { goal: text }];
    }

    // Check if we're submitting the generated handoff prompt (to create new session)
    if state.input.handoff_ready {
        if text.trim().is_empty() {
            // Edge case: user cleared the generated prompt
            state
                .transcript
                .cells
                .push(HistoryCell::system("Handoff prompt cannot be empty."));
            return vec![];
        }
        state.input.handoff_ready = false;
        state.input.handoff_goal = None; // Clear goal on successful submit
        state.clear_input();
        return vec![UiEffect::HandoffSubmit { prompt: text }];
    }

    // Normal message submission
    if text.trim().is_empty() {
        return vec![];
    }

    state.input.history.push(text.clone());
    state.reset_history_navigation();

    state.transcript.cells.push(HistoryCell::user(&text));
    state
        .conversation
        .messages
        .push(crate::providers::anthropic::ChatMessage::user(&text));

    let effects = if state.conversation.session.is_some() {
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
    state.input.should_navigate_up()
}

fn should_navigate_history_down(state: &TuiState) -> bool {
    state.input.should_navigate_down()
}

fn navigate_history_up(state: &mut TuiState) {
    state.input.navigate_up();
}

fn navigate_history_down(state: &mut TuiState) {
    state.input.navigate_down();
}

// ============================================================================
// Command Execution (dispatched from palette via ExecuteCommand effect)
// ============================================================================

/// Executes a slash command by name.
///
/// Called by the runtime when processing `UiEffect::ExecuteCommand`.
pub fn execute_command(state: &mut TuiState, cmd_name: &str) -> Vec<UiEffect> {
    use crate::ui::chat::overlays::login::update_login;

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
        "handoff" => execute_handoff(state),
        "new" => execute_new(state),
        "quit" => execute_quit(state),
        _ => vec![],
    }
}

// ============================================================================
// Slash Commands
// ============================================================================

fn execute_handoff(state: &mut TuiState) -> Vec<UiEffect> {
    // Check if we have an active session
    if state.conversation.session.is_none() {
        state
            .transcript
            .cells
            .push(HistoryCell::system("Handoff requires an active session."));
        return vec![];
    }

    // Clear any stale handoff state from previous attempts
    // This includes canceling any in-flight generation
    state.input.handoff_ready = false;
    state.input.handoff_generating = false;
    state.input.handoff_rx = None;
    state.input.handoff_goal = None;
    state.clear_input();

    // Set handoff mode - next submit will trigger handoff
    state.input.handoff_pending = true;
    vec![]
}

fn execute_new(state: &mut TuiState) -> Vec<UiEffect> {
    if state.agent_state.is_running() {
        state
            .transcript
            .cells
            .push(HistoryCell::system("Cannot clear while streaming."));
        return vec![];
    }

    state.transcript.cells.clear();
    state.conversation.messages.clear();
    state.input.history.clear();
    state.transcript.scroll.reset();
    state.conversation.usage = crate::ui::chat::state::SessionUsage::new();
    state.transcript.wrap_cache.clear();

    if state.conversation.session.is_some() {
        vec![UiEffect::CreateNewSession]
    } else {
        state
            .transcript
            .cells
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
                .cells
                .push(HistoryCell::system("Logged out from Anthropic OAuth."));
        }
        Ok(false) => {
            state
                .transcript
                .cells
                .push(HistoryCell::system("No OAuth credentials to clear."));
        }
        Err(e) => {
            state
                .transcript
                .cells
                .push(HistoryCell::system(format!("Logout failed: {}", e)));
        }
    }
}

// ============================================================================
// Handoff Result Handler
// ============================================================================

/// Handles the handoff generation result.
fn handle_handoff_result(state: &mut TuiState, result: Result<String, String>) {
    // Clear generating state
    state.input.handoff_generating = false;
    state.input.handoff_rx = None;

    match result {
        Ok(generated_prompt) => {
            // Set the generated prompt in the input textarea
            state.input.set_text(&generated_prompt);

            // Mark handoff as ready - next submit will create new session
            state.input.handoff_ready = true;

            // Clear the stored goal (no longer needed)
            state.input.handoff_goal = None;

            // Show success message
            state.transcript.cells.push(HistoryCell::system(
                "Handoff ready. Edit and press Enter to start new session, or Esc to cancel.",
            ));
        }
        Err(error) => {
            // Show error message
            state.transcript.cells.push(HistoryCell::system(format!(
                "Handoff generation failed: {}",
                error
            )));

            // Restore goal for retry (spec requirement)
            if let Some(goal) = state.input.handoff_goal.clone() {
                state.input.set_text(&goal);
                state.input.handoff_pending = true;
                state.transcript.cells.push(HistoryCell::system(
                    "Press Enter to retry, or Esc to cancel.",
                ));
            }
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
                    state.transcript.cells.push(cell);

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
                        .cells
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
                        state.transcript.cells.push(new_cell);
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
                && let Some(cell) = state
                    .transcript
                    .cells
                    .iter_mut()
                    .find(|c| c.id() == *cell_id)
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
                .cells
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
            for cell in &mut state.transcript.cells {
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
                    .cells
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
            state.transcript.cells.push(tool_cell);
            vec![]
        }
        AgentEvent::ToolInputReady { id, input, .. } => {
            // Update the existing tool cell with the complete input
            if let Some(cell) =
                state.transcript.cells.iter_mut().find(
                    |c| matches!(c, HistoryCell::Tool { tool_use_id, .. } if *tool_use_id == *id),
                )
            {
                cell.set_tool_input(input.clone());
            }
            vec![]
        }
        AgentEvent::ToolStarted { .. } => vec![],
        AgentEvent::ToolFinished { id, result } => {
            if let Some(cell) =
                state.transcript.cells.iter_mut().find(
                    |c| matches!(c, HistoryCell::Tool { tool_use_id, .. } if *tool_use_id == *id),
                )
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
            state.conversation.messages = messages.clone();
            state.agent_state = AgentState::Idle;

            // Save assistant message to session if enabled
            if !final_text.is_empty() && state.conversation.session.is_some() {
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
                    state.transcript.cells.push(cell);
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
                .cells
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
                state.transcript.cells.push(cell);
            } else {
                // Append to the existing streaming thinking cell
                if let Some(cell) = state.transcript.cells.last_mut() {
                    cell.append_thinking_delta(text);
                }
            }
            vec![]
        }
        AgentEvent::ThinkingComplete { signature, .. } => {
            // Find the last streaming thinking cell and finalize it
            if let Some(cell) = state.transcript.cells.iter_mut().rev().find(|c| {
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
            state.conversation.usage.add(
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
        if let Some(cell) = state
            .transcript
            .cells
            .iter_mut()
            .find(|c| c.id() == *cell_id)
        {
            cell.append_assistant_delta(pending_delta);
        }
        pending_delta.clear();
    }
}

/// Applies any accumulated scroll delta from mouse events.
///
/// Called once per frame after all events are processed to coalesce
/// rapid scroll events (especially from trackpads) into a single scroll.
pub fn apply_scroll_delta(state: &mut TuiState) {
    let delta = state.transcript.scroll_accumulator.take_delta();
    if delta == 0 {
        return;
    }

    let lines = delta.unsigned_abs() as usize;
    if delta < 0 {
        state.transcript.scroll_up(lines);
    } else {
        state.transcript.scroll_down(lines);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::chat::state::ScrollMode;

    #[test]
    fn test_scroll_to_top() {
        let config = crate::config::Config::default();
        let mut state = TuiState::new(config, std::path::PathBuf::new(), None, None);

        state.transcript.scroll_to_top();

        assert!(matches!(
            state.transcript.scroll.mode,
            ScrollMode::Anchored { offset: 0 }
        ));
    }

    #[test]
    fn test_scroll_to_bottom() {
        let config = crate::config::Config::default();
        let mut state = TuiState::new(config, std::path::PathBuf::new(), None, None);
        state.transcript.scroll_to_top(); // Start from top

        state.transcript.scroll_to_bottom();

        assert!(matches!(
            state.transcript.scroll.mode,
            ScrollMode::FollowLatest
        ));
    }

    #[test]
    fn test_scroll_up_and_down() {
        let config = crate::config::Config::default();
        let mut state = TuiState::new(config, std::path::PathBuf::new(), None, None);
        state.transcript.scroll.update_line_count(100);

        // Start following, scroll up should anchor
        state.transcript.scroll.scroll_up(5, 20);
        assert!(matches!(
            state.transcript.scroll.mode,
            ScrollMode::Anchored { .. }
        ));

        // Scroll down should move towards bottom
        state.transcript.scroll.scroll_down(100, 20);
        assert!(matches!(
            state.transcript.scroll.mode,
            ScrollMode::FollowLatest
        ));
    }

    #[test]
    fn test_apply_scroll_delta_coalesces_events() {
        let config = crate::config::Config::default();
        let mut state = TuiState::new(config, std::path::PathBuf::new(), None, None);
        state.transcript.scroll.update_line_count(100);
        state.transcript.viewport_height = 20;

        // Simulate multiple scroll up events (trackpad-like)
        state.transcript.scroll_accumulator.accumulate(-1);
        state.transcript.scroll_accumulator.accumulate(-1);
        state.transcript.scroll_accumulator.accumulate(-1);

        // Apply should coalesce into single scroll of 3 lines
        super::apply_scroll_delta(&mut state);

        // Should be anchored at offset 77 (80 - 3)
        assert!(matches!(
            state.transcript.scroll.mode,
            ScrollMode::Anchored { offset: 77 }
        ));
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
        state.transcript.cells.push(HistoryCell::user("test"));
        state
            .conversation
            .messages
            .push(crate::providers::anthropic::ChatMessage::user("test"));
        state.input.history.push("test".to_string());
        state.conversation.usage.add(100, 50, 200, 25);

        // Trigger cache population by rendering (simulate)
        let _lines =
            state.transcript.cells[0].display_lines_cached(80, 0, &state.transcript.wrap_cache);
        assert!(!state.transcript.wrap_cache.is_empty());

        // Execute clear
        let effects = execute_new(&mut state);

        // Verify everything is cleared
        assert!(state.transcript.cells.is_empty() || state.transcript.cells.len() == 1); // May have "Conversation cleared." message
        assert!(state.conversation.messages.is_empty());
        assert!(state.input.history.is_empty());
        assert!(state.transcript.wrap_cache.is_empty());
        assert!(state.transcript.scroll.is_following());
        assert_eq!(state.conversation.usage.input_tokens, 0);
        assert_eq!(state.conversation.usage.output_tokens, 0);

        // Verify it returns no effects when no session is active
        assert_eq!(effects.len(), 0);
    }
}
