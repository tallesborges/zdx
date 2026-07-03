//! Transcript update handlers.
//!
//! Contains all state mutations for the transcript feature:
//! - Agent event handling (streaming text, tool use, etc.)
//! - Mouse events (scroll, selection)
//! - Delta coalescing (pending text, scroll)

use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::Rect;
use zdx_engine::core::events::{AgentEvent, TurnStatus};
use zdx_engine::core::interrupt;

use super::reasoning::reasoning_display_text;
use crate::effects::UiEffect;
use crate::mutations::{StateMutation, ThreadMutation};
use crate::state::AgentState;
use crate::transcript::{HistoryCell, LineInteraction, TranscriptState};

/// Lines to scroll per mouse wheel tick.
const MOUSE_SCROLL_LINES: usize = 3;

// ============================================================================
// Agent Event Handler
// ============================================================================

/// Handles agent events that affect the transcript.
///
/// This is the main entry point for agent events. It updates transcript cells
/// based on streaming text, tool use, thinking, and turn completion.
#[allow(clippy::too_many_lines)]
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
            // iteration, and TurnFinished may follow immediately after AssistantCompleted.
            apply_pending_delta(transcript, agent_state);

            if let AgentState::Streaming { cell_id, .. } = &agent_state {
                let cell_id = *cell_id;
                let followups = transcript.finalize_assistant_cell_extracting_followups(cell_id);
                if !followups.is_empty() {
                    transcript.push_cell(HistoryCell::system(format_followups(&followups)));
                    mutations.push(StateMutation::SetLastFollowups(followups));
                }
            }
            vec![]
        }
        AgentEvent::Error { message, .. } => {
            transcript.push_cell(HistoryCell::system(format!("Error: {message}")));
            vec![]
        }
        AgentEvent::Notice { message, .. } => {
            // Non-fatal informational notice (e.g. refusal,
            // context window exceeded). Render as a system cell
            // without the "Error:" prefix.
            transcript.push_cell(HistoryCell::system(format!("⚠ {message}")));
            vec![]
        }
        AgentEvent::ProviderRetry {
            message,
            attempt,
            max_retries,
            delay_ms,
            ..
        } => {
            // Non-fatal retry notice: render as a plain system cell so users
            // see the backoff without an "Error:" prefix.
            let delay_secs = *delay_ms as f64 / 1000.0;
            transcript.push_cell(HistoryCell::system(format!(
                "⟳ Provider error, retrying in {delay_secs:.1}s (attempt {attempt}/{max_retries}): {message}"
            )));
            vec![]
        }
        AgentEvent::ToolRequested { id, name, input } => {
            let tool_cell = HistoryCell::tool_running(id, name, input.clone());
            let cell_id = tool_cell.id();
            transcript.push_cell(tool_cell);

            // Transition from Waiting to Streaming so UI shows activity
            if let AgentState::Waiting { .. } = agent_state {
                let old_state = std::mem::replace(agent_state, AgentState::Idle);
                if let AgentState::Waiting { rx, cancel } = old_state {
                    *agent_state = AgentState::Streaming {
                        rx,
                        cancel,
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
        AgentEvent::TurnFinished {
            status,
            final_text,
            messages,
            ..
        } => {
            // Apply any pending delta before resetting agent state to ensure no
            // content is lost. This handles edge cases where AssistantCompleted wasn't
            // received or didn't have a chance to apply the delta.
            apply_pending_delta(transcript, agent_state);
            match status {
                TurnStatus::Completed => {
                    let orphaned_tool_count = transcript.cancel_orphaned_running_tools();
                    if orphaned_tool_count > 0 {
                        tracing::warn!(
                            count = orphaned_tool_count,
                            "TurnFinished::Completed received with running tool cells; marking them cancelled"
                        );
                    }
                    mutations.push(StateMutation::Thread(ThreadMutation::SetMessages(
                        messages.clone(),
                    )));
                    *agent_state = AgentState::Idle;
                    let _ = (final_text, has_thread);
                    vec![]
                }
                TurnStatus::Interrupted => {
                    if !messages.is_empty() {
                        mutations.push(StateMutation::Thread(ThreadMutation::SetMessages(
                            messages.clone(),
                        )));
                    }
                    transcript.mark_interrupted();
                    interrupt::reset();
                    *agent_state = AgentState::Idle;
                    vec![]
                }
                TurnStatus::Failed { message, .. } => {
                    // Preserve committed messages so manual 'continue' resumes from the
                    // correct state (with tool results from the failed attempt intact).
                    if !messages.is_empty() {
                        mutations.push(StateMutation::Thread(ThreadMutation::SetMessages(
                            messages.clone(),
                        )));
                    }
                    transcript.mark_errored();
                    transcript.push_cell(HistoryCell::system(format!("Error: {message}")));
                    *agent_state = AgentState::Idle;
                    vec![]
                }
            }
        }
        // Reasoning events - create or update thinking cells in transcript
        AgentEvent::ReasoningDelta { text } => {
            handle_thinking_delta(transcript, agent_state, text);
            vec![]
        }
        AgentEvent::ReasoningCompleted { block } => {
            handle_reasoning_completed(transcript, block);
            vec![]
        }
        AgentEvent::UsageUpdate {
            input_tokens,
            output_tokens,
            cache_read_input_tokens,
            cache_creation_input_tokens,
            ..
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
            // A new turn supersedes the previous reply's suggestions.
            mutations.push(StateMutation::SetLastFollowups(Vec::new()));
            vec![]
        }
        AgentEvent::ToolOutputDelta { id, chunk } => {
            transcript.append_tool_output_delta_for(id, chunk);
            vec![]
        }
        AgentEvent::TurnCheckpoint { .. } => {
            // Non-terminal incremental snapshot used by persistence to flush
            // messages between tool turns. The TUI gets live state from
            // streaming events and does not need to react to checkpoints.
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
            if let AgentState::Waiting { rx, cancel } = old_state {
                *agent_state = AgentState::Streaming {
                    rx,
                    cancel,
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
        if let AgentState::Waiting { rx, cancel } = old_state {
            let cell = HistoryCell::thinking_streaming(text);
            let cell_id = cell.id();
            transcript.push_cell(cell);
            *agent_state = AgentState::Streaming {
                rx,
                cancel,
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

/// Finalizes the current streaming thinking cell, or creates a finalized one
/// from the completed reasoning block when no streaming thinking cell exists.
///
/// When no streaming thinking cell exists, uses `reasoning_display_text` to
/// pick the visible content: visible text if present, otherwise the
/// `[redacted reasoning]` placeholder for Anthropic `redacted_thinking`
/// blocks. Reasoning blocks with no text and no redacted replay are skipped.
fn handle_reasoning_completed(
    transcript: &mut TranscriptState,
    block: &zdx_engine::providers::ReasoningBlock,
) {
    if transcript.finalize_last_thinking_cell(block.replay.clone()) {
        return;
    }

    if let Some(display) = reasoning_display_text(block.text.as_deref(), block.replay.as_ref()) {
        let mut cell = HistoryCell::thinking_streaming(display);
        cell.finalize_thinking(block.replay.clone());
        transcript.push_cell(cell);
    }
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
    transcript_area: Rect,
) -> Option<crate::overlays::OverlayRequest> {
    match mouse.kind {
        MouseEventKind::ScrollUp => {
            if !contains_point(transcript_area, mouse.column, mouse.row) {
                return None;
            }
            transcript.scroll_up(MOUSE_SCROLL_LINES);
            None
        }
        MouseEventKind::ScrollDown => {
            if !contains_point(transcript_area, mouse.column, mouse.row) {
                return None;
            }
            transcript.scroll_down(MOUSE_SCROLL_LINES);
            None
        }
        MouseEventKind::Down(MouseButton::Left) => {
            // Start selection at clicked position
            if let Some((line, col)) =
                screen_to_transcript_pos(transcript, mouse.column, mouse.row, transcript_area)
            {
                // Check for image indicator click
                if let Some(request) = check_image_click(transcript, line, col) {
                    return Some(request);
                }

                // If user clicks on tool header, open tool detail popup
                if let Some(mapping) = transcript.position_map.get_by_global_line(line)
                    && matches!(mapping.interaction, Some(LineInteraction::OpenToolDetail))
                    && let Some(cell_idx) = transcript.scroll.cell_index_for_line(line)
                    && let Some(HistoryCell::Tool { tool_use_id, .. }) =
                        transcript.cells().get(cell_idx)
                {
                    return Some(crate::overlays::OverlayRequest::ToolDetail {
                        tool_use_id: tool_use_id.clone(),
                    });
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
                    screen_to_transcript_pos(transcript, mouse.column, mouse.row, transcript_area)
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

fn contains_point(area: Rect, x: u16, y: u16) -> bool {
    x >= area.x
        && x < area.x.saturating_add(area.width)
        && y >= area.y
        && y < area.y.saturating_add(area.height)
}

/// Checks if a click on the given line is on an image placeholder and returns an overlay request.
fn check_image_click(
    transcript: &TranscriptState,
    line: usize,
    col: usize,
) -> Option<crate::overlays::OverlayRequest> {
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
    transcript_area: Rect,
) -> Option<(usize, usize)> {
    use unicode_segmentation::UnicodeSegmentation;

    use crate::common::ratatui_width;

    if !contains_point(transcript_area, screen_x, screen_y) {
        return None; // Click is in input or status area, not transcript
    }

    let content_x = (screen_x - transcript_area.x) as usize;
    let screen_y = (screen_y - transcript_area.y) as usize;

    let viewport_height = transcript.viewport_height;

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
        let grapheme_width = ratatui_width(grapheme);
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

/// Formats follow-up suggestions into a system cell body.
fn format_followups(items: &[String]) -> String {
    use std::fmt::Write;
    let mut out = String::from("💡 Suggested next steps (Ctrl+F to pick):");
    for (idx, item) in items.iter().enumerate() {
        let _ = write!(out, "\n  {}. {item}", idx + 1);
    }
    out
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use zdx_engine::core::events::ToolOutput;
    use zdx_engine::providers::{ReasoningBlock, ReplayToken};

    use super::*;
    use crate::transcript::{LineMapping, ToolState};

    fn transcript_with_lines(lines: &[&str], viewport_height: usize) -> TranscriptState {
        let mut transcript = TranscriptState::default();
        transcript.update_layout((80, 24), viewport_height);
        transcript.scroll.cached_line_count = lines.len();
        transcript.position_map.clear();
        for line in lines {
            transcript
                .position_map
                .push(LineMapping::new((*line).to_string(), None));
        }
        transcript
    }

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
    fn screen_to_transcript_pos_accounts_for_y_offset() {
        let transcript = transcript_with_lines(&["first", "second"], 2);
        let transcript_area = Rect::new(1, 1, 78, 2);

        assert_eq!(
            screen_to_transcript_pos(&transcript, 1, 1, transcript_area),
            Some((0, 0))
        );
        assert_eq!(
            screen_to_transcript_pos(&transcript, 1, 0, transcript_area),
            None
        );
        assert_eq!(
            screen_to_transcript_pos(&transcript, 1, 2, transcript_area),
            Some((1, 0))
        );
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

    #[test]
    fn completed_turn_cancels_running_tool_without_result() {
        let mut transcript = TranscriptState::default();
        let mut agent_state = AgentState::Idle;

        handle_agent_event(
            &mut transcript,
            &mut agent_state,
            true,
            &AgentEvent::ToolRequested {
                id: "tool-1".to_string(),
                name: "apply_patch".to_string(),
                input: json!({"patch": "*** Begin Patch\n*** Update File: src/main.rs\n*** End Patch"}),
            },
        );
        handle_agent_event(
            &mut transcript,
            &mut agent_state,
            true,
            &AgentEvent::TurnFinished {
                status: TurnStatus::Completed,
                final_text: String::new(),
                messages: Vec::new(),
                prior_message_count: 0,
            },
        );

        match transcript.cells().last() {
            Some(HistoryCell::Tool { state, result, .. }) => {
                assert_eq!(*state, ToolState::Cancelled);
                assert_eq!(
                    result.as_ref(),
                    Some(&ToolOutput::canceled(
                        "Tool result was not received before the turn completed"
                    ))
                );
            }
            _ => panic!("Expected tool cell"),
        }
        assert!(matches!(agent_state, AgentState::Idle));
    }

    #[test]
    fn completed_turn_preserves_completed_tool_result() {
        let mut transcript = TranscriptState::default();
        let mut agent_state = AgentState::Idle;
        let expected_result = ToolOutput::success(json!({"content": "ok"}));

        handle_agent_event(
            &mut transcript,
            &mut agent_state,
            true,
            &AgentEvent::ToolRequested {
                id: "tool-1".to_string(),
                name: "read".to_string(),
                input: json!({"file_path": "test.txt"}),
            },
        );
        handle_agent_event(
            &mut transcript,
            &mut agent_state,
            true,
            &AgentEvent::ToolCompleted {
                id: "tool-1".to_string(),
                result: expected_result.clone(),
            },
        );
        handle_agent_event(
            &mut transcript,
            &mut agent_state,
            true,
            &AgentEvent::TurnFinished {
                status: TurnStatus::Completed,
                final_text: String::new(),
                messages: Vec::new(),
                prior_message_count: 0,
            },
        );

        match transcript.cells().last() {
            Some(HistoryCell::Tool { state, result, .. }) => {
                assert_eq!(*state, ToolState::Done);
                assert_eq!(result.as_ref(), Some(&expected_result));
            }
            _ => panic!("Expected tool cell"),
        }
    }

    #[test]
    fn reasoning_completed_creates_finalized_thinking_cell_without_live_delta() {
        let mut transcript = TranscriptState::default();
        let replay = ReplayToken::Anthropic {
            signature: "sig-123".to_string(),
        };

        handle_reasoning_completed(
            &mut transcript,
            &ReasoningBlock {
                text: Some("Condensed reasoning summary".to_string()),
                replay: Some(replay.clone()),
            },
        );

        assert_eq!(transcript.cells().len(), 1);
        assert!(matches!(
            transcript.cells().last(),
            Some(HistoryCell::Thinking {
                content,
                replay: Some(cell_replay),
                is_streaming: false,
                is_interrupted: false,
                ..
            }) if content == "Condensed reasoning summary" && cell_replay == &replay
        ));
    }

    #[test]
    fn reasoning_completed_redacted_creates_placeholder_cell() {
        // A `redacted_thinking` block completes with no prior `ReasoningDelta`
        // (nothing was streamed) and must still appear as a visible cell
        // anchored to the redacted replay token.
        let mut transcript = TranscriptState::default();
        let replay = ReplayToken::AnthropicRedacted {
            data: "blob".to_string(),
        };

        handle_reasoning_completed(
            &mut transcript,
            &ReasoningBlock {
                text: None,
                replay: Some(replay.clone()),
            },
        );

        assert_eq!(transcript.cells().len(), 1);
        assert!(matches!(
            transcript.cells().last(),
            Some(HistoryCell::Thinking {
                content,
                replay: Some(cell_replay),
                is_streaming: false,
                is_interrupted: false,
                ..
            }) if content == "[redacted reasoning]" && cell_replay == &replay
        ));
    }

    #[test]
    fn reasoning_completed_preserves_streamed_text_under_redacted_replay() {
        // Edge case #1: a streaming thinking cell already accumulated visible
        // text, and the completion event carries a redacted replay token.
        // The cell must be finalized with the redacted replay but KEEP the
        // streamed text — we must not overwrite visible content with the
        // placeholder.
        let mut transcript = TranscriptState::default();
        transcript.push_cell(HistoryCell::thinking_streaming("already streamed"));

        let replay = ReplayToken::AnthropicRedacted {
            data: "blob".to_string(),
        };

        handle_reasoning_completed(
            &mut transcript,
            &ReasoningBlock {
                text: None,
                replay: Some(replay.clone()),
            },
        );

        assert_eq!(transcript.cells().len(), 1);
        assert!(matches!(
            transcript.cells().last(),
            Some(HistoryCell::Thinking {
                content,
                replay: Some(cell_replay),
                is_streaming: false,
                ..
            }) if content == "already streamed" && cell_replay == &replay
        ));
    }

    #[test]
    fn reasoning_completed_skips_when_no_text_and_no_redacted_replay() {
        // Preserves previous behavior: a completion with no visible text and
        // no redacted replay token produces no cell.
        let mut transcript = TranscriptState::default();

        handle_reasoning_completed(
            &mut transcript,
            &ReasoningBlock {
                text: None,
                replay: None,
            },
        );

        assert!(transcript.cells().is_empty());
    }
}
