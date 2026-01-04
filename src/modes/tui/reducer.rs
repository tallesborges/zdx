//! TUI reducer (update function).
//!
//! All state mutations happen here. The runtime calls `update(app, event)`
//! and executes the returned effects.
//!
//! This is the single source of truth for how events modify state.

use crossterm::event::{Event, MouseEventKind};

use crate::core::interrupt;
use crate::core::session::SessionEvent;
use crate::modes::tui::core::events::UiEvent;
use crate::modes::tui::overlays::{Overlay, OverlayAction, handle_login_result};
use crate::modes::tui::shared::effects::UiEffect;
use crate::modes::tui::state::{AgentState, AppState, TuiState};
use crate::modes::tui::transcript::{HistoryCell, ToolState};
use crate::modes::tui::{input, session, view};

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
            input::handle_handoff_result(&mut app.tui, result);
            vec![]
        }
        UiEvent::FilesDiscovered(files) => {
            handle_files_discovered(&mut app.overlay, files);
            vec![]
        }

        // Session async result events - delegate to session feature
        UiEvent::Session(session_event) => {
            session::handle_session_event(&mut app.tui, &mut app.overlay, session_event)
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
            input::handle_paste(app, &text);
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

fn handle_mouse(tui: &mut TuiState, mouse: crossterm::event::MouseEvent) {
    use crate::modes::tui::view::TRANSCRIPT_MARGIN;

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
    if let Some(overlay) = app.overlay.as_mut() {
        return match overlay.handle_key(&mut app.tui, key) {
            None => vec![], // Overlay handled it, continue
            Some(OverlayAction::Close(effects)) => {
                app.overlay = None;
                effects
            }
            Some(OverlayAction::Effects(effects)) => effects,
        };
    }

    // No overlay active - delegate to input feature module
    input::handle_main_key(app, key)
}

// ============================================================================
// File Picker Handler
// ============================================================================

/// Handles the file discovery result.
fn handle_files_discovered(overlay: &mut Option<Overlay>, files: Vec<std::path::PathBuf>) {
    if let Some(picker) = overlay.as_mut().and_then(|o| o.as_file_picker_mut()) {
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
    use crate::modes::tui::state::ScrollMode;

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
