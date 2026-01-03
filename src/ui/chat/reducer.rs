//! TUI reducer (update function).
//!
//! All state mutations happen here. The runtime calls `update(app, event)`
//! and executes the returned effects.
//!
//! This is the single source of truth for how events modify state.

use crossterm::event::{Event, KeyCode, KeyModifiers, MouseEventKind};

use crate::core::interrupt;
use crate::core::session::SessionEvent;
use crate::ui::chat::effects::UiEffect;
use crate::ui::chat::events::UiEvent;
use crate::ui::chat::overlays::{OverlayAction, OverlayState, handle_login_result};
use crate::ui::chat::state::{AgentState, AppState, HandoffState, TuiState};
use crate::ui::chat::view;
use crate::ui::transcript::{HistoryCell, ToolState};

/// Lines to scroll per mouse wheel tick.
const MOUSE_SCROLL_LINES: usize = 3;

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
            vec![]
        }
        UiEvent::Frame { width, height } => {
            handle_frame(&mut app.tui, width, height);
            vec![]
        }
        UiEvent::Terminal(term_event) => handle_terminal_event(app, term_event),
        UiEvent::Agent(agent_event) => handle_agent_event(&mut app.tui, &agent_event),
        UiEvent::LoginResult(result) => {
            handle_login_result(&mut app.tui, &mut app.overlay, result);
            vec![]
        }
        UiEvent::HandoffResult(result) => {
            handle_handoff_result(&mut app.tui, result);
            vec![]
        }
        UiEvent::FilesDiscovered(files) => {
            handle_files_discovered(&mut app.overlay, files);
            vec![]
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
    // Update transcript layout with current terminal dimensions
    let viewport_height = view::calculate_transcript_height_with_state(tui, height);
    tui.transcript
        .update_layout((width, height), viewport_height);

    // Apply any pending streaming text deltas (coalescing)
    apply_pending_delta(tui);

    // Apply accumulated scroll delta from mouse events (coalescing)
    apply_scroll_delta(tui);

    // Update cell line info for lazy rendering and scroll calculations
    let cell_line_counts = view::calculate_cell_line_counts(tui, width as usize);
    tui.transcript
        .scroll
        .update_cell_line_info(cell_line_counts);
}

// ============================================================================
// Terminal Event Handlers
// ============================================================================

fn handle_terminal_event(app: &mut AppState, event: Event) -> Vec<UiEffect> {
    match event {
        Event::Key(key) => handle_key(app, key),
        Event::Mouse(mouse) => {
            handle_mouse(&mut app.tui, mouse);
            vec![]
        }
        Event::Paste(text) => {
            handle_paste(app, &text);
            vec![]
        }
        Event::Resize(_, _) => {
            // Clear wrap cache on resize since line wrapping depends on width
            app.tui.transcript.wrap_cache.clear();
            vec![]
        }
        _ => vec![],
    }
}

fn handle_paste(app: &mut AppState, text: &str) {
    if let OverlayState::Login(crate::ui::chat::overlays::LoginState::AwaitingCode {
        ref mut input,
        ..
    }) = app.overlay
    {
        input.push_str(text);
    } else {
        app.tui.input.textarea.insert_str(text);
    }
}

fn handle_mouse(tui: &mut TuiState, mouse: crossterm::event::MouseEvent) {
    use crate::ui::chat::view::TRANSCRIPT_MARGIN;

    match mouse.kind {
        MouseEventKind::ScrollUp => {
            // Accumulate negative delta (up = negative)
            tui.transcript
                .scroll_accumulator
                .accumulate(-(MOUSE_SCROLL_LINES as i32));
        }
        MouseEventKind::ScrollDown => {
            // Accumulate positive delta (down = positive)
            tui.transcript
                .scroll_accumulator
                .accumulate(MOUSE_SCROLL_LINES as i32);
        }
        MouseEventKind::Down(crossterm::event::MouseButton::Left) => {
            // Start selection at clicked position
            if let Some((line, col)) =
                screen_to_transcript_pos(tui, mouse.column, mouse.row, TRANSCRIPT_MARGIN)
            {
                tui.transcript.start_selection(line, col);
            }
        }
        MouseEventKind::Drag(crossterm::event::MouseButton::Left) => {
            // Extend selection while dragging
            if tui.transcript.selection.is_selecting
                && let Some((line, col)) =
                    screen_to_transcript_pos(tui, mouse.column, mouse.row, TRANSCRIPT_MARGIN)
            {
                tui.transcript.extend_selection(line, col);
            }
        }
        MouseEventKind::Up(crossterm::event::MouseButton::Left) => {
            // Finish selection and auto-copy if there's selected text
            tui.transcript.finish_selection();
            if tui.transcript.has_selection() {
                // Copy to clipboard and schedule visual clear
                let _ = tui.transcript.copy_and_schedule_clear();
            }
        }
        _ => {}
    }
}

/// Converts screen coordinates to transcript position (line index, grapheme column).
///
/// Returns `None` if the position is outside the transcript area.
fn screen_to_transcript_pos(
    tui: &TuiState,
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
    let viewport_height = tui.transcript.viewport_height;
    if screen_y as usize >= viewport_height {
        return None; // Click is in input or status area, not transcript
    }

    // Get scroll offset and line counts
    let scroll_offset = tui.transcript.scroll.get_offset(viewport_height);
    let position_map_len = tui.transcript.position_map.len();
    let cached_total = tui.transcript.scroll.cached_line_count;

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
        tui.transcript.position_map.get(content_y)?
    } else {
        // Full mode: position_map is indexed by global line number
        tui.transcript.position_map.get(absolute_line)?
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

fn handle_key(app: &mut AppState, key: crossterm::event::KeyEvent) -> Vec<UiEffect> {
    // Try to dispatch to the active overlay
    match app.overlay.handle_key(&mut app.tui, key) {
        None => handle_main_key(app, key), // No overlay active
        Some(None) => vec![],              // Overlay handled it, continue
        Some(Some(action)) => process_overlay_action(app, action), // Overlay action
    }
}

/// Processes an OverlayAction returned by an overlay's handle_key.
fn process_overlay_action(app: &mut AppState, action: OverlayAction) -> Vec<UiEffect> {
    match action {
        OverlayAction::Close(effects) => {
            app.overlay = OverlayState::None;
            effects
        }
        OverlayAction::Transition { new_state, effects } => {
            app.overlay = new_state;
            effects
        }
        OverlayAction::Effects(effects) => effects,
    }
}

fn handle_main_key(app: &mut AppState, key: crossterm::event::KeyEvent) -> Vec<UiEffect> {
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
            if should_navigate_history_up(tui) {
                navigate_history_up(tui);
            } else {
                tui.input.textarea.input(key);
            }
            vec![]
        }
        KeyCode::Down if !ctrl && !shift && !alt => {
            if should_navigate_history_down(tui) {
                navigate_history_down(tui);
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

// ============================================================================
// Submit / Agent
// ============================================================================

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

// ============================================================================
// History Navigation
// ============================================================================

fn should_navigate_history_up(tui: &TuiState) -> bool {
    tui.input.should_navigate_up()
}

fn should_navigate_history_down(tui: &TuiState) -> bool {
    tui.input.should_navigate_down()
}

fn navigate_history_up(tui: &mut TuiState) {
    tui.input.navigate_up();
}

fn navigate_history_down(tui: &mut TuiState) {
    tui.input.navigate_down();
}

// ============================================================================
// Handoff Result Handler
// ============================================================================

/// Handles the handoff generation result.
fn handle_handoff_result(tui: &mut TuiState, result: Result<String, String>) {
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

// ============================================================================
// File Picker Handler
// ============================================================================

/// Handles the file discovery result.
fn handle_files_discovered(overlay: &mut OverlayState, files: Vec<std::path::PathBuf>) {
    if let Some(picker) = overlay.as_file_picker_mut() {
        picker.set_files(files);
    }
}

// ============================================================================
// Agent Event Handlers
// ============================================================================

pub fn handle_agent_event(
    tui: &mut TuiState,
    event: &crate::core::events::AgentEvent,
) -> Vec<UiEffect> {
    use crate::core::events::AgentEvent;

    match event {
        AgentEvent::AssistantDelta { text } => {
            match &mut tui.agent_state {
                AgentState::Waiting { .. } => {
                    // Create streaming cell and transition to Streaming state
                    let cell = HistoryCell::assistant_streaming("");
                    let cell_id = cell.id();
                    tui.transcript.cells.push(cell);

                    let old_state = std::mem::replace(&mut tui.agent_state, AgentState::Idle);
                    if let AgentState::Waiting { rx } = old_state {
                        tui.agent_state = AgentState::Streaming {
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
                    let needs_new_cell = tui
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
                        tui.transcript.cells.push(new_cell);
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
            apply_pending_delta(tui);

            if let AgentState::Streaming { cell_id, .. } = &tui.agent_state
                && let Some(cell) = tui.transcript.cells.iter_mut().find(|c| c.id() == *cell_id)
            {
                cell.finalize_assistant();
            }
            vec![]
        }
        AgentEvent::Error { message, .. } => {
            // Apply any pending delta before resetting agent state to preserve
            // partial content that was streamed before the error occurred.
            apply_pending_delta(tui);

            tui.transcript
                .cells
                .push(HistoryCell::system(format!("Error: {}", message)));
            // Reset agent state - the turn is over due to the error
            tui.agent_state = AgentState::Idle;
            vec![]
        }
        AgentEvent::Interrupted => {
            // Apply any pending delta before resetting agent state to preserve
            // partial content that was streamed before the interruption.
            apply_pending_delta(tui);

            // Mark any running tools or streaming cells as cancelled
            let mut any_marked = false;
            for cell in &mut tui.transcript.cells {
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
                && let Some(last_user) = tui
                    .transcript
                    .cells
                    .iter_mut()
                    .rev()
                    .find(|c| matches!(c, HistoryCell::User { .. }))
            {
                last_user.mark_request_interrupted();
            }

            interrupt::reset();
            tui.agent_state = AgentState::Idle;
            vec![]
        }
        AgentEvent::ToolRequested { id, name, input } => {
            let tool_cell = HistoryCell::tool_running(id, name, input.clone());
            tui.transcript.cells.push(tool_cell);
            vec![]
        }
        AgentEvent::ToolInputReady { id, input, .. } => {
            // Update the existing tool cell with the complete input
            if let Some(cell) =
                tui.transcript.cells.iter_mut().find(
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
                tui.transcript.cells.iter_mut().find(
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
            apply_pending_delta(tui);

            // Turn completed - update messages and reset agent state
            tui.conversation.messages = messages.clone();
            tui.agent_state = AgentState::Idle;

            // Save assistant message to session if enabled
            if !final_text.is_empty() && tui.conversation.session.is_some() {
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
            if let AgentState::Waiting { .. } = &tui.agent_state {
                let old_state = std::mem::replace(&mut tui.agent_state, AgentState::Idle);
                if let AgentState::Waiting { rx } = old_state {
                    let cell = HistoryCell::thinking_streaming(text);
                    let cell_id = cell.id();
                    tui.transcript.cells.push(cell);
                    tui.agent_state = AgentState::Streaming {
                        rx,
                        cell_id,
                        pending_delta: String::new(),
                    };
                }
                return vec![];
            }

            // Find the last cell and check if it's a streaming thinking cell
            let should_create_new = tui
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
                tui.transcript.cells.push(cell);
            } else {
                // Append to the existing streaming thinking cell
                if let Some(cell) = tui.transcript.cells.last_mut() {
                    cell.append_thinking_delta(text);
                }
            }
            vec![]
        }
        AgentEvent::ThinkingComplete { signature, .. } => {
            // Find the last streaming thinking cell and finalize it
            if let Some(cell) = tui.transcript.cells.iter_mut().rev().find(|c| {
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
            tui.conversation.usage.add(
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
pub fn apply_pending_delta(tui: &mut TuiState) {
    if let AgentState::Streaming {
        cell_id,
        pending_delta,
        ..
    } = &mut tui.agent_state
        && !pending_delta.is_empty()
    {
        if let Some(cell) = tui.transcript.cells.iter_mut().find(|c| c.id() == *cell_id) {
            cell.append_assistant_delta(pending_delta);
        }
        pending_delta.clear();
    }
}

/// Applies any accumulated scroll delta from mouse events.
///
/// Called once per frame after all events are processed to coalesce
/// rapid scroll events (especially from trackpads) into a single scroll.
pub fn apply_scroll_delta(tui: &mut TuiState) {
    let delta = tui.transcript.scroll_accumulator.take_delta();
    if delta == 0 {
        return;
    }

    let lines = delta.unsigned_abs() as usize;
    if delta < 0 {
        tui.transcript.scroll_up(lines);
    } else {
        tui.transcript.scroll_down(lines);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::chat::state::ScrollMode;

    #[test]
    fn test_scroll_to_top() {
        let config = crate::config::Config::default();
        let mut app = AppState::new(config, std::path::PathBuf::new(), None, None);

        app.tui.transcript.scroll_to_top();

        assert!(matches!(
            app.tui.transcript.scroll.mode,
            ScrollMode::Anchored { offset: 0 }
        ));
    }

    #[test]
    fn test_scroll_to_bottom() {
        let config = crate::config::Config::default();
        let mut app = AppState::new(config, std::path::PathBuf::new(), None, None);
        app.tui.transcript.scroll_to_top(); // Start from top

        app.tui.transcript.scroll_to_bottom();

        assert!(matches!(
            app.tui.transcript.scroll.mode,
            ScrollMode::FollowLatest
        ));
    }

    #[test]
    fn test_scroll_up_and_down() {
        let config = crate::config::Config::default();
        let mut app = AppState::new(config, std::path::PathBuf::new(), None, None);
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
    fn test_apply_scroll_delta_coalesces_events() {
        let config = crate::config::Config::default();
        let mut app = AppState::new(config, std::path::PathBuf::new(), None, None);
        app.tui.transcript.scroll.update_line_count(100);
        app.tui.transcript.viewport_height = 20;

        // Simulate multiple scroll up events (trackpad-like)
        app.tui.transcript.scroll_accumulator.accumulate(-1);
        app.tui.transcript.scroll_accumulator.accumulate(-1);
        app.tui.transcript.scroll_accumulator.accumulate(-1);

        // Apply should coalesce into single scroll of 3 lines
        super::apply_scroll_delta(&mut app.tui);

        // Should be anchored at offset 77 (80 - 3)
        assert!(matches!(
            app.tui.transcript.scroll.mode,
            ScrollMode::Anchored { offset: 77 }
        ));
    }
}
