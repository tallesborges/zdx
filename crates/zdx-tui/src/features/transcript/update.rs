//! Transcript update handlers.
//!
//! Contains all state mutations for the transcript feature:
//! - Agent event handling (streaming text, tool use, etc.)
//! - Mouse events (scroll, selection)
//! - Delta coalescing (pending text, scroll)

use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
use zdx_core::core::events::AgentEvent;
use zdx_core::core::interrupt;
use zdx_core::core::thread_persistence::ThreadEvent;

use crate::effects::UiEffect;
use crate::mutations::{StateMutation, ThreadMutation};
use crate::state::AgentState;
use crate::transcript::{HistoryCell, TranscriptState};

/// Lines to scroll per mouse wheel tick.
const MOUSE_SCROLL_LINES: usize = 1;

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
    has_thread: bool,
    event: &AgentEvent,
) -> (Vec<UiEffect>, Vec<StateMutation>) {
    let mut mutations = Vec::new();
    let effects = match event {
        AgentEvent::AssistantDelta { text } => {
            handle_assistant_delta(transcript, agent_state, text);
            vec![]
        }
        AgentEvent::AssistantCompleted { .. } => {
            // Apply any pending delta before finalizing to ensure no content is lost.
            // This is critical because multiple events can be processed in one loop
            // iteration, and TurnCompleted may follow immediately after AssistantCompleted.
            apply_pending_delta(transcript, agent_state);

            if let AgentState::Streaming { cell_id, .. } = &agent_state {
                transcript.finalize_assistant_cell(*cell_id);
            }
            vec![]
        }
        AgentEvent::Error { message, .. } => {
            // Apply any pending delta before resetting agent state to preserve
            // partial content that was streamed before the error occurred.
            apply_pending_delta(transcript, agent_state);

            // Mark all running/streaming cells as errored (stops spinner, shows error state)
            transcript.mark_errored();

            transcript.push_cell(HistoryCell::system(format!("Error: {message}")));
            // Reset agent state - the turn is over due to the error
            *agent_state = AgentState::Idle;
            vec![]
        }
        AgentEvent::Interrupted { .. } => {
            handle_interrupted(transcript, agent_state);
            vec![]
        }
        AgentEvent::ToolRequested { id, name, input } => {
            let tool_cell = HistoryCell::tool_running(id, name, input.clone());
            let cell_id = tool_cell.id();
            transcript.push_cell(tool_cell);

            // Transition from Waiting to Streaming so UI shows activity
            if let AgentState::Waiting { .. } = agent_state {
                let old_state = std::mem::replace(agent_state, AgentState::Idle);
                if let AgentState::Waiting { rx } = old_state {
                    *agent_state = AgentState::Streaming {
                        rx,
                        cell_id,
                        pending_delta: String::new(),
                    };
                }
            }
            vec![]
        }
        AgentEvent::ToolInputCompleted { id, input, .. } => {
            transcript.set_tool_input_for(id, input.clone());
            vec![]
        }
        AgentEvent::ToolInputDelta { id, delta, .. } => {
            transcript.set_tool_input_delta_for(id, delta.clone());
            vec![]
        }
        AgentEvent::ToolStarted { .. } => vec![],
        AgentEvent::ToolCompleted { id, result } => {
            transcript.set_tool_result_for(id, result.clone());
            vec![]
        }
        AgentEvent::TurnCompleted {
            final_text,
            messages,
        } => {
            // Apply any pending delta before resetting agent state to ensure no
            // content is lost. This handles edge cases where AssistantCompleted wasn't
            // received or didn't have a chance to apply the delta.
            apply_pending_delta(transcript, agent_state);

            mutations.push(StateMutation::Thread(ThreadMutation::SetMessages(
                messages.clone(),
            )));
            *agent_state = AgentState::Idle;

            // Save assistant message to thread if enabled
            if !final_text.is_empty() && has_thread {
                vec![UiEffect::SaveThread {
                    event: ThreadEvent::assistant_message(final_text),
                }]
            } else {
                vec![]
            }
        }
        // Reasoning events - create or update thinking cells in transcript
        AgentEvent::ReasoningDelta { text } => {
            handle_thinking_delta(transcript, agent_state, text);
            vec![]
        }
        AgentEvent::ReasoningCompleted { block } => {
            transcript.finalize_last_thinking_cell(block.replay.clone());
            vec![]
        }
        AgentEvent::UsageUpdate {
            input_tokens,
            output_tokens,
            cache_read_input_tokens,
            cache_creation_input_tokens,
        } => {
            mutations.push(StateMutation::Thread(ThreadMutation::UpdateUsage {
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
            // For now, we only show final output in ToolCompleted
            vec![]
        }
    };

    (effects, mutations)
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
            transcript.push_cell(cell);

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
                .cells()
                .iter()
                .find(|c| c.id() == *cell_id)
                .is_none_or(|c| {
                    !matches!(
                        c,
                        HistoryCell::Assistant {
                            is_streaming: true,
                            ..
                        }
                    )
                });

            if needs_new_cell {
                let new_cell = HistoryCell::assistant_streaming("");
                let new_cell_id = new_cell.id();
                transcript.push_cell(new_cell);
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
            transcript.push_cell(cell);
            *agent_state = AgentState::Streaming {
                rx,
                cell_id,
                pending_delta: String::new(),
            };
        }
        return;
    }

    // When the API sends text → thinking → text (e.g., Anthropic with thinking_mode=auto
    // emitting an empty text block before thinking), an empty streaming assistant cell
    // may have been created prematurely. Remove it so the thinking cell takes the correct
    // position (before the response text that will arrive later).
    if let AgentState::Streaming {
        cell_id,
        pending_delta,
        ..
    } = agent_state
    {
        let is_empty_assistant = transcript
            .cells()
            .iter()
            .find(|c| c.id() == *cell_id)
            .is_some_and(|c| {
                matches!(
                    c,
                    HistoryCell::Assistant {
                        content,
                        is_streaming: true,
                        ..
                    } if content.is_empty() || content.trim().is_empty()
                )
            });

        if is_empty_assistant && pending_delta.trim().is_empty() {
            // Remove the empty assistant cell and discard whitespace-only pending delta.
            // Leave cell_id as-is: because the cell is gone, future text deltas in
            // handle_assistant_delta will detect needs_new_cell=true and create a new
            // assistant cell (which will correctly appear after the thinking cell).
            transcript.remove_cell_by_id(*cell_id);
            pending_delta.clear();
        }
    }

    // Find the last cell and check if it's a streaming thinking cell
    let should_create_new = transcript.cells().last().is_none_or(|cell| {
        !matches!(
            cell,
            HistoryCell::Thinking {
                is_streaming: true,
                ..
            }
        )
    });

    if should_create_new {
        // Create a new streaming thinking cell
        let cell = HistoryCell::thinking_streaming(text);
        transcript.push_cell(cell);
    } else {
        // Append to the existing streaming thinking cell
        transcript.append_thinking_delta_to_last(text);
    }
}

/// Handles interruption events.
fn handle_interrupted(transcript: &mut TranscriptState, agent_state: &mut AgentState) {
    // Apply any pending delta before resetting agent state to preserve
    // partial content that was streamed before the interruption.
    apply_pending_delta(transcript, agent_state);

    // Mark all running/streaming cells as cancelled
    transcript.mark_interrupted();

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
pub fn handle_mouse(
    transcript: &mut TranscriptState,
    mouse: MouseEvent,
    transcript_margin: u16,
) -> Option<crate::overlays::OverlayRequest> {
    match mouse.kind {
        MouseEventKind::ScrollUp => {
            // Accumulate negative delta (up = negative)
            transcript
                .scroll_accumulator
                .accumulate(-(MOUSE_SCROLL_LINES as i32));
            None
        }
        MouseEventKind::ScrollDown => {
            // Accumulate positive delta (down = positive)
            transcript
                .scroll_accumulator
                .accumulate(MOUSE_SCROLL_LINES as i32);
            None
        }
        MouseEventKind::Down(MouseButton::Left) => {
            // Start selection at clicked position
            if let Some((line, col)) =
                screen_to_transcript_pos(transcript, mouse.column, mouse.row, transcript_margin)
            {
                // Check for image indicator click
                if let Some(request) = check_image_click(transcript, line, col) {
                    return Some(request);
                }

                // If user clicks on tool output/args disclosure rows, toggle expand/collapse
                // and skip selection behavior.
                if transcript.toggle_tool_output_for_line(line)
                    || transcript.toggle_tool_args_for_line(line)
                {
                    return None;
                }

                if transcript.register_click(line, col) {
                    if !transcript.select_word_at(line, col) {
                        transcript.start_selection(line, col);
                    }
                } else {
                    transcript.start_selection(line, col);
                }
            }
            None
        }
        MouseEventKind::Drag(MouseButton::Left) => {
            // Extend selection while dragging
            if transcript.selection.is_selecting
                && let Some((line, col)) =
                    screen_to_transcript_pos(transcript, mouse.column, mouse.row, transcript_margin)
            {
                transcript.extend_selection(line, col);
            }
            None
        }
        MouseEventKind::Up(MouseButton::Left) => {
            // Finish selection and auto-copy if there's selected text
            transcript.finish_selection();
            if transcript.has_selection() {
                // Copy to clipboard and schedule visual clear
                let _ = transcript.copy_and_schedule_clear();
            }
            None
        }
        _ => None,
    }
}

/// Checks if a click on the given line is on an image placeholder and returns an overlay request.
fn check_image_click(
    transcript: &TranscriptState,
    line: usize,
    col: usize,
) -> Option<crate::overlays::OverlayRequest> {
    use crate::transcript::LineInteraction;

    let mapping = transcript.position_map.get_by_global_line(line)?;
    if !matches!(mapping.interaction, Some(LineInteraction::ImagePlaceholder)) {
        return None;
    }

    // Click on [Image N] in message text — find which placeholder was clicked
    let text = &mapping.text;
    let image_index = find_image_placeholder_at_col(text, col)?;

    let cell_idx = transcript.scroll.cell_index_for_line(line)?;
    let cell = transcript.cells().get(cell_idx)?;
    if let HistoryCell::User {
        image_paths,
        content,
        ..
    } = cell
    {
        // Map global image number to local index within this cell's content.
        // The cell's content may use non-sequential image IDs (e.g., [Image 3])
        // but image_paths is ordered by appearance. Find the ordinal position of
        // the clicked image number among all [Image N] placeholders in the content.
        let local_index = find_local_image_index(content, image_index)?;
        let path = image_paths.get(local_index)?;
        return Some(crate::overlays::OverlayRequest::ImagePreview {
            image_path: path.clone(),
            image_index,
        });
    }

    None
}

/// Maps a global image number (from `[Image N]`) to its ordinal position (0-indexed)
/// among all image placeholders in the cell's content text.
///
/// For example, if content is `"[Image 3] what's this?"`, image number 3 is at
/// local index 0 (the first placeholder in this cell).
fn find_local_image_index(content: &str, target_image_number: usize) -> Option<usize> {
    let mut search_start = 0;
    let mut ordinal = 0;

    loop {
        let bracket_pos = content[search_start..]
            .find("[Image ")
            .or_else(|| content[search_start..].find("[Image\u{00A0}"));
        let Some(bracket_pos) = bracket_pos else {
            break;
        };
        let abs_pos = search_start + bracket_pos;
        if let Some(close_offset) = content[abs_pos..].find(']') {
            let close_pos = abs_pos + close_offset + 1;
            let candidate = &content[abs_pos..close_pos];
            let after_image = &candidate["[Image".len()..];
            let inner = after_image
                .strip_prefix(' ')
                .or_else(|| after_image.strip_prefix('\u{00A0}'))
                .and_then(|s| s.strip_suffix(']'));
            if let Some(inner) = inner
                && !inner.is_empty()
                && inner.chars().all(|c| c.is_ascii_digit())
                && let Ok(n) = inner.parse::<usize>()
            {
                if n == target_image_number {
                    return Some(ordinal);
                }
                ordinal += 1;
            }
            search_start = close_pos;
        } else {
            break;
        }
    }
    None
}

/// Finds which `[Image N]` placeholder the column position falls within.
/// Returns the image number N (1-indexed) if found.
fn find_image_placeholder_at_col(text: &str, col: usize) -> Option<usize> {
    use unicode_segmentation::UnicodeSegmentation;

    let mut search_start = 0;

    // Match both regular space and non-breaking space (\u{00A0}) in "[Image N]"
    while search_start < text.len() {
        // Find next "[Image" followed by a space (regular or non-breaking)
        let bracket_pos = text[search_start..]
            .find("[Image ")
            .or_else(|| text[search_start..].find("[Image\u{00A0}"));
        let Some(bracket_pos) = bracket_pos else {
            break;
        };
        let abs_pos = search_start + bracket_pos;
        if let Some(close_offset) = text[abs_pos..].find(']') {
            let close_pos = abs_pos + close_offset + 1;
            let candidate = &text[abs_pos..close_pos];
            // Strip "[Image" + separator (space or NBSP, which can be multi-byte) + "]"
            let after_image = &candidate["[Image".len()..];
            let inner = after_image
                .strip_prefix(' ')
                .or_else(|| after_image.strip_prefix('\u{00A0}'))
                .and_then(|s| s.strip_suffix(']'));
            if let Some(inner) = inner
                && !inner.is_empty()
                && inner.chars().all(|c| c.is_ascii_digit())
            {
                // Count graphemes up to abs_pos
                let start_grapheme = text[..abs_pos].graphemes(true).count();
                let end_grapheme = text[..close_pos].graphemes(true).count();

                if col >= start_grapheme && col < end_grapheme {
                    return inner.parse().ok();
                }
            }
            search_start = close_pos;
        } else {
            break;
        }
    }
    None
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
        transcript.append_to_streaming_cell(*cell_id, pending_delta);
        pending_delta.clear();
    }
}

/// Applies any accumulated scroll delta from mouse events.
///
/// Called once per frame after all events are processed to coalesce
/// rapid scroll events (especially from trackpads) into a single scroll.
/// Uses scroll acceleration: slow for precision, faster for long scrolls.
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn find_image_placeholder_first() {
        let text = "You: [Image 1] [Image 2]";
        assert_eq!(find_image_placeholder_at_col(text, 5), Some(1));
        assert_eq!(find_image_placeholder_at_col(text, 13), Some(1));
    }

    #[test]
    fn find_image_placeholder_second() {
        let text = "You: [Image 1] [Image 2]";
        assert_eq!(find_image_placeholder_at_col(text, 15), Some(2));
        assert_eq!(find_image_placeholder_at_col(text, 23), Some(2));
    }

    #[test]
    fn find_image_placeholder_between() {
        let text = "You: [Image 1] [Image 2]";
        assert_eq!(find_image_placeholder_at_col(text, 14), None);
    }

    #[test]
    fn find_image_placeholder_bare() {
        let text = "[Image 1] [Image 2]";
        assert_eq!(find_image_placeholder_at_col(text, 0), Some(1));
        assert_eq!(find_image_placeholder_at_col(text, 10), Some(2));
    }

    #[test]
    fn find_image_placeholder_nbsp() {
        // Non-breaking space variant (used to prevent wrapping)
        let text = "[Image\u{00A0}1] [Image\u{00A0}2]";
        assert_eq!(find_image_placeholder_at_col(text, 0), Some(1));
        assert_eq!(find_image_placeholder_at_col(text, 10), Some(2));
    }

    #[test]
    fn local_image_index_sequential() {
        let content = "[Image 1] hello [Image 2]";
        assert_eq!(find_local_image_index(content, 1), Some(0));
        assert_eq!(find_local_image_index(content, 2), Some(1));
        assert_eq!(find_local_image_index(content, 3), None);
    }

    #[test]
    fn local_image_index_non_sequential() {
        // Second message has [Image 3] as its only image
        let content = "[Image 3] what's this?";
        assert_eq!(find_local_image_index(content, 3), Some(0));
        assert_eq!(find_local_image_index(content, 1), None);
    }

    #[test]
    fn local_image_index_nbsp() {
        let content = "[Image\u{00A0}5] test [Image\u{00A0}6]";
        assert_eq!(find_local_image_index(content, 5), Some(0));
        assert_eq!(find_local_image_index(content, 6), Some(1));
    }
}
