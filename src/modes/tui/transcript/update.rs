//! Transcript update handlers.
//!
//! Contains all state mutations for the transcript feature:
//! - Agent event handling (streaming text, tool use, etc.)
//! - Mouse events (scroll, selection)
//! - Delta coalescing (pending text, scroll)

use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};

use crate::core::events::AgentEvent;
use crate::core::interrupt;
use crate::core::session::SessionEvent;
use crate::modes::tui::app::AgentState;
use crate::modes::tui::shared::effects::UiEffect;
use crate::modes::tui::shared::internal::{SessionCommand, StateCommand};
use crate::modes::tui::transcript::{HistoryCell, ToolState, TranscriptState};

/// Lines to scroll per mouse wheel tick.
const MOUSE_SCROLL_LINES: usize = 3;

// ============================================================================
// Agent Event Handler
// ============================================================================

/// Handles agent events that affect the transcript.
///
/// This is the main entry point for agent events. It updates transcript cells
/// based on streaming text, tool use, thinking, and turn completion.
pub fn handle_agent_event(
    transcript: &mut TranscriptState,
    agent_state: &mut AgentState,
    has_session: bool,
    event: &AgentEvent,
) -> (Vec<UiEffect>, Vec<StateCommand>) {
    let mut commands = Vec::new();
    let effects =
        match event {
            AgentEvent::AssistantDelta { text } => {
                handle_assistant_delta(transcript, agent_state, text);
                vec![]
            }
            AgentEvent::AssistantComplete { .. } => {
                // Apply any pending delta before finalizing to ensure no content is lost.
                // This is critical because multiple events can be processed in one loop
                // iteration, and TurnComplete may follow immediately after AssistantComplete.
                apply_pending_delta(transcript, agent_state);

                if let AgentState::Streaming { cell_id, .. } = &agent_state
                    && let Some(cell) = transcript.cells.iter_mut().find(|c| c.id() == *cell_id)
                {
                    cell.finalize_assistant();
                }
                vec![]
            }
            AgentEvent::Error { message, .. } => {
                // Apply any pending delta before resetting agent state to preserve
                // partial content that was streamed before the error occurred.
                apply_pending_delta(transcript, agent_state);

                transcript
                    .cells
                    .push(HistoryCell::system(format!("Error: {}", message)));
                // Reset agent state - the turn is over due to the error
                *agent_state = AgentState::Idle;
                vec![]
            }
            AgentEvent::Interrupted => {
                handle_interrupted(transcript, agent_state);
                vec![]
            }
            AgentEvent::ToolRequested { id, name, input } => {
                let tool_cell = HistoryCell::tool_running(id, name, input.clone());
                transcript.cells.push(tool_cell);
                vec![]
            }
            AgentEvent::ToolInputReady { id, input, .. } => {
                // Update the existing tool cell with the complete input
                if let Some(cell) = transcript.cells.iter_mut().find(
                    |c| matches!(c, HistoryCell::Tool { tool_use_id, .. } if *tool_use_id == *id),
                ) {
                    cell.set_tool_input(input.clone());
                }
                vec![]
            }
            AgentEvent::ToolStarted { .. } => vec![],
            AgentEvent::ToolFinished { id, result } => {
                if let Some(cell) = transcript.cells.iter_mut().find(
                    |c| matches!(c, HistoryCell::Tool { tool_use_id, .. } if *tool_use_id == *id),
                ) {
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
                apply_pending_delta(transcript, agent_state);

                commands.push(StateCommand::Session(SessionCommand::SetMessages(
                    messages.clone(),
                )));
                *agent_state = AgentState::Idle;

                // Save assistant message to session if enabled
                if !final_text.is_empty() && has_session {
                    vec![UiEffect::SaveSession {
                        event: SessionEvent::assistant_message(final_text),
                    }]
                } else {
                    vec![]
                }
            }
            // Thinking events - create or update thinking cells in transcript
            AgentEvent::ThinkingDelta { text } => {
                handle_thinking_delta(transcript, agent_state, text);
                vec![]
            }
            AgentEvent::ThinkingComplete { signature, .. } => {
                // Find the last streaming thinking cell and finalize it
                if let Some(cell) = transcript.cells.iter_mut().rev().find(|c| {
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
                commands.push(StateCommand::Session(SessionCommand::UpdateUsage {
                    input: *input_tokens,
                    output: *output_tokens,
                    cache_read: *cache_read_input_tokens,
                    cache_write: *cache_creation_input_tokens,
                }));
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
        };

    (effects, commands)
}

// ============================================================================
// Private Agent Event Helpers
// ============================================================================

/// Handles assistant text delta events.
fn handle_assistant_delta(
    transcript: &mut TranscriptState,
    agent_state: &mut AgentState,
    text: &str,
) {
    match agent_state {
        AgentState::Waiting { .. } => {
            // Create streaming cell and transition to Streaming state
            let cell = HistoryCell::assistant_streaming("");
            let cell_id = cell.id();
            transcript.cells.push(cell);

            let old_state = std::mem::replace(agent_state, AgentState::Idle);
            if let AgentState::Waiting { rx } = old_state {
                *agent_state = AgentState::Streaming {
                    rx,
                    cell_id,
                    pending_delta: text.to_string(),
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
            let needs_new_cell = transcript
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
                transcript.cells.push(new_cell);
                *cell_id = new_cell_id;
                pending_delta.clear();
                pending_delta.push_str(text);
            } else {
                pending_delta.push_str(text);
            }
        }
        AgentState::Idle => {}
    }
}

/// Handles thinking text delta events.
fn handle_thinking_delta(
    transcript: &mut TranscriptState,
    agent_state: &mut AgentState,
    text: &str,
) {
    // Transition from Waiting to Streaming
    if let AgentState::Waiting { .. } = agent_state {
        let old_state = std::mem::replace(agent_state, AgentState::Idle);
        if let AgentState::Waiting { rx } = old_state {
            let cell = HistoryCell::thinking_streaming(text);
            let cell_id = cell.id();
            transcript.cells.push(cell);
            *agent_state = AgentState::Streaming {
                rx,
                cell_id,
                pending_delta: String::new(),
            };
        }
        return;
    }

    // Find the last cell and check if it's a streaming thinking cell
    let should_create_new = transcript
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
        transcript.cells.push(cell);
    } else {
        // Append to the existing streaming thinking cell
        if let Some(cell) = transcript.cells.last_mut() {
            cell.append_thinking_delta(text);
        }
    }
}

/// Handles interruption events.
fn handle_interrupted(transcript: &mut TranscriptState, agent_state: &mut AgentState) {
    // Apply any pending delta before resetting agent state to preserve
    // partial content that was streamed before the interruption.
    apply_pending_delta(transcript, agent_state);

    // Mark any running tools or streaming cells as cancelled
    let mut any_marked = false;
    for cell in &mut transcript.cells {
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
        && let Some(last_user) = transcript
            .cells
            .iter_mut()
            .rev()
            .find(|c| matches!(c, HistoryCell::User { .. }))
    {
        last_user.mark_request_interrupted();
    }

    interrupt::reset();
    *agent_state = AgentState::Idle;
}

// ============================================================================
// Mouse Event Handler
// ============================================================================

/// Handles mouse events for the transcript area.
///
/// Supports:
/// - Scroll wheel (up/down) with delta accumulation
/// - Click-and-drag selection with auto-copy on release
pub fn handle_mouse(transcript: &mut TranscriptState, mouse: MouseEvent, transcript_margin: u16) {
    match mouse.kind {
        MouseEventKind::ScrollUp => {
            // Accumulate negative delta (up = negative)
            transcript
                .scroll_accumulator
                .accumulate(-(MOUSE_SCROLL_LINES as i32));
        }
        MouseEventKind::ScrollDown => {
            // Accumulate positive delta (down = positive)
            transcript
                .scroll_accumulator
                .accumulate(MOUSE_SCROLL_LINES as i32);
        }
        MouseEventKind::Down(MouseButton::Left) => {
            // Start selection at clicked position
            if let Some((line, col)) =
                screen_to_transcript_pos(transcript, mouse.column, mouse.row, transcript_margin)
            {
                transcript.start_selection(line, col);
            }
        }
        MouseEventKind::Drag(MouseButton::Left) => {
            // Extend selection while dragging
            if transcript.selection.is_selecting
                && let Some((line, col)) =
                    screen_to_transcript_pos(transcript, mouse.column, mouse.row, transcript_margin)
            {
                transcript.extend_selection(line, col);
            }
        }
        MouseEventKind::Up(MouseButton::Left) => {
            // Finish selection and auto-copy if there's selected text
            transcript.finish_selection();
            if transcript.has_selection() {
                // Copy to clipboard and schedule visual clear
                let _ = transcript.copy_and_schedule_clear();
            }
        }
        _ => {}
    }
}

/// Converts screen coordinates to transcript position (line index, grapheme column).
///
/// Returns `None` if the position is outside the transcript area.
pub fn screen_to_transcript_pos(
    transcript: &TranscriptState,
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
    let viewport_height = transcript.viewport_height;
    if screen_y as usize >= viewport_height {
        return None; // Click is in input or status area, not transcript
    }

    // Get scroll offset and line counts
    let scroll_offset = transcript.scroll.get_offset(viewport_height);
    let position_map_len = transcript.position_map.len();
    let cached_total = transcript.scroll.cached_line_count;

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
        transcript.position_map.get(content_y)?
    } else {
        // Full mode: position_map is indexed by global line number
        transcript.position_map.get(absolute_line)?
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

// ============================================================================
// Delta Coalescing
// ============================================================================

/// Applies any pending delta to the streaming cell (coalescing).
pub fn apply_pending_delta(transcript: &mut TranscriptState, agent_state: &mut AgentState) {
    if let AgentState::Streaming {
        cell_id,
        pending_delta,
        ..
    } = agent_state
        && !pending_delta.is_empty()
    {
        if let Some(cell) = transcript.cells.iter_mut().find(|c| c.id() == *cell_id) {
            cell.append_assistant_delta(pending_delta);
        }
        pending_delta.clear();
    }
}

/// Applies any accumulated scroll delta from mouse events.
///
/// Called once per frame after all events are processed to coalesce
/// rapid scroll events (especially from trackpads) into a single scroll.
pub fn apply_scroll_delta(transcript: &mut TranscriptState) {
    let delta = transcript.scroll_accumulator.take_delta();
    if delta == 0 {
        return;
    }

    let lines = delta.unsigned_abs() as usize;
    if delta < 0 {
        transcript.scroll_up(lines);
    } else {
        transcript.scroll_down(lines);
    }
}
