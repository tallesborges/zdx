//! Transcript display state.
//!
//! Manages scroll position, viewport dimensions, selection state, and
//! rendering cache for the transcript area.

use std::ops::Range;
use std::time::{Duration, Instant};

use unicode_segmentation::UnicodeSegmentation;

use super::selection::{PositionMap, SelectionState, VisualPosition};
use crate::mutations::TranscriptMutation;

const DOUBLE_CLICK_MAX_DELAY: Duration = Duration::from_millis(400);
const DOUBLE_CLICK_MAX_COLUMN_DELTA: usize = 1;

#[derive(Debug, Clone, Copy)]
struct ClickInfo {
    line: usize,
    column: usize,
    at: Instant,
}

/// Scroll mode for the transcript.
#[derive(Debug, Clone)]
pub enum ScrollMode {
    /// Auto-scroll to show latest content (bottom of transcript).
    FollowLatest,
    /// User scrolled manually; offset is line index from top.
    Anchored { offset: usize },
}

/// Line count info for a single cell.
///
/// Used for O(log n) visibility calculations in lazy rendering.
#[derive(Debug, Clone)]
pub struct CellLineInfo {
    /// Starting line index (cumulative offset from top).
    pub start_line: usize,
    /// Number of lines this cell produces (including trailing blank).
    pub line_count: usize,
}

/// Result of visible range calculation.
///
/// Contains both the cell range and the line offset within the first cell.
#[derive(Debug, Clone)]
pub struct VisibleRange {
    /// Range of cell indices that are visible.
    pub cell_range: Range<usize>,
    /// Line offset to skip within the first visible cell.
    pub first_cell_line_offset: usize,
    /// Total lines to skip before the visible range (for position map).
    pub lines_before: usize,
}

/// Scroll state for the transcript pane.
///
/// Encapsulates scroll mode, cached line count, and all scroll navigation logic.
/// This keeps scroll math in one place and simplifies the reducer.
#[derive(Debug, Clone)]
pub struct ScrollState {
    /// Current scroll mode (follow latest or anchored at offset).
    pub mode: ScrollMode,
    /// Cached total line count from last render (for scroll calculations).
    pub cached_line_count: usize,
    /// Line info per cell for O(log n) visibility calculations.
    /// Updated when cells are added/modified or terminal width changes.
    pub cell_line_info: Vec<CellLineInfo>,
}

impl Default for ScrollState {
    fn default() -> Self {
        Self {
            mode: ScrollMode::FollowLatest,
            cached_line_count: 0,
            cell_line_info: Vec::new(),
        }
    }
}

impl ScrollState {
    /// Creates a new `ScrollState` in follow mode.
    #[cfg(test)]
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns true if currently following output (auto-scroll).
    pub fn is_following(&self) -> bool {
        matches!(self.mode, ScrollMode::FollowLatest)
    }

    /// Returns the current scroll offset for rendering.
    ///
    /// In `FollowLatest` mode, calculates offset to show bottom of content.
    /// In Anchored mode, returns the stored offset (clamped to valid range).
    pub fn get_offset(&self, viewport_height: usize) -> usize {
        match &self.mode {
            ScrollMode::FollowLatest => self.cached_line_count.saturating_sub(viewport_height),
            ScrollMode::Anchored { offset } => {
                let max_offset = self.cached_line_count.saturating_sub(viewport_height);
                (*offset).min(max_offset)
            }
        }
    }

    /// Returns true if there's content below the current viewport.
    #[cfg(test)]
    pub fn has_content_below(&self, viewport_height: usize) -> bool {
        let offset = self.get_offset(viewport_height);
        offset + viewport_height < self.cached_line_count
    }

    /// Scrolls up by the given number of lines.
    pub fn scroll_up(&mut self, lines: usize, viewport_height: usize) {
        let current_offset = self.get_offset(viewport_height);
        let new_offset = current_offset.saturating_sub(lines);
        self.mode = ScrollMode::Anchored { offset: new_offset };
    }

    /// Scrolls down by the given number of lines.
    ///
    /// Transitions to `FollowLatest` mode when reaching the bottom.
    pub fn scroll_down(&mut self, lines: usize, viewport_height: usize) {
        if matches!(self.mode, ScrollMode::FollowLatest) {
            return; // Already at bottom
        }

        let current_offset = self.get_offset(viewport_height);
        let max_offset = self.cached_line_count.saturating_sub(viewport_height);
        let new_offset = (current_offset + lines).min(max_offset);

        if new_offset >= max_offset {
            self.mode = ScrollMode::FollowLatest;
        } else {
            self.mode = ScrollMode::Anchored { offset: new_offset };
        }
    }

    /// Scrolls to the top of the transcript.
    pub fn scroll_to_top(&mut self) {
        self.mode = ScrollMode::Anchored { offset: 0 };
    }

    /// Scrolls to the bottom of the transcript (enables follow mode).
    pub fn scroll_to_bottom(&mut self) {
        self.mode = ScrollMode::FollowLatest;
    }

    /// Scrolls up by one page.
    pub fn page_up(&mut self, viewport_height: usize) {
        self.scroll_up(viewport_height.max(1), viewport_height);
    }

    /// Scrolls down by one page.
    pub fn page_down(&mut self, viewport_height: usize) {
        self.scroll_down(viewport_height.max(1), viewport_height);
    }

    /// Updates the cached line count.
    ///
    /// Call this after rendering to keep scroll calculations accurate.
    /// Note: For production use, prefer `patch_cell_line_info()` which
    /// updates both cell info and `cached_line_count`.
    #[cfg(test)]
    pub fn update_line_count(&mut self, line_count: usize) {
        self.cached_line_count = line_count;
    }

    /// Resets scroll state to follow mode (e.g., after clearing transcript).
    pub fn reset(&mut self) {
        self.mode = ScrollMode::FollowLatest;
        self.cached_line_count = 0;
        self.cell_line_info.clear();
    }

    /// Calculates which cells are visible in the current viewport.
    ///
    /// Returns `None` if `cell_line_info` is empty or not yet populated.
    /// Otherwise returns the range of cell indices to render and metadata
    /// for proper positioning.
    pub fn visible_range(&self, viewport_height: usize) -> Option<VisibleRange> {
        if self.cell_line_info.is_empty() {
            return None;
        }

        let scroll_offset = self.get_offset(viewport_height);
        let viewport_end = scroll_offset + viewport_height;

        // Binary search for first cell that overlaps with viewport
        // A cell overlaps if: cell.start_line + cell.line_count > scroll_offset
        let first_cell = self
            .cell_line_info
            .partition_point(|info| info.start_line + info.line_count <= scroll_offset);

        if first_cell >= self.cell_line_info.len() {
            // All cells are above viewport (shouldn't happen in practice)
            return None;
        }

        // Binary search for last cell that overlaps with viewport
        // A cell overlaps if: cell.start_line < viewport_end
        let last_cell = self
            .cell_line_info
            .partition_point(|info| info.start_line < viewport_end);

        // Calculate line offset within first visible cell
        let first_cell_info = &self.cell_line_info[first_cell];
        let first_cell_line_offset = scroll_offset.saturating_sub(first_cell_info.start_line);

        Some(VisibleRange {
            cell_range: first_cell..last_cell,
            first_cell_line_offset,
            lines_before: scroll_offset,
        })
    }

    /// Incrementally patches cell line info from `from_index` onward.
    ///
    /// Entries before `from_index` are kept as-is; entries at and after it are
    /// replaced by `line_counts` (which must cover exactly `cells[from_index..]`
    /// in order). Start lines are recomputed cumulatively from the last kept
    /// entry, and `cached_line_count` is updated.
    ///
    /// `from_index == 0` is equivalent to a full rebuild.
    pub fn patch_cell_line_info(&mut self, from_index: usize, line_counts: Vec<usize>) {
        let from_index = from_index.min(self.cell_line_info.len());
        self.cell_line_info.truncate(from_index);

        let mut cumulative_offset = self
            .cell_line_info
            .last()
            .map_or(0, |info| info.start_line + info.line_count);

        for line_count in line_counts {
            self.cell_line_info.push(CellLineInfo {
                start_line: cumulative_offset,
                line_count,
            });
            cumulative_offset += line_count;
        }

        self.cached_line_count = cumulative_offset;
    }
    pub fn cell_start_line(&self, cell_index: usize) -> Option<usize> {
        self.cell_line_info
            .get(cell_index)
            .map(|info| info.start_line)
    }

    /// Returns the cell index containing the given global line.
    pub fn cell_index_for_line(&self, line: usize) -> Option<usize> {
        if self.cell_line_info.is_empty() {
            return None;
        }

        let idx = self
            .cell_line_info
            .partition_point(|info| info.start_line + info.line_count <= line);

        (idx < self.cell_line_info.len()).then_some(idx)
    }
}

/// Transcript display state.
///
/// Encapsulates all state related to displaying the transcript: cells, scroll,
/// layout dimensions, selection, and rendering cache.
#[derive(Debug)]
pub struct TranscriptState {
    /// Transcript cells (private to enforce mutation API).
    cells: Vec<super::HistoryCell>,

    /// Scroll state (mode, offset, cached line count).
    pub scroll: ScrollState,

    /// Cache for wrapped line rendering.
    pub wrap_cache: super::WrapCache,

    /// Available height for transcript viewport.
    pub viewport_height: usize,

    /// Current terminal dimensions (width, height).
    pub terminal_size: (u16, u16),

    /// Selection state (anchor, cursor, active flag).
    pub selection: SelectionState,

    /// Position map for selection coordinate translation.
    /// Rebuilt each render to track visual line → cell/text mappings.
    pub position_map: PositionMap,

    /// Last click info for double-click detection.
    last_click: Option<ClickInfo>,

    /// User cell ID for the most recently appended prompt awaiting agent start.
    pending_user_cell_id: Option<super::CellId>,

    /// User cell ID for the currently running agent turn.
    active_user_cell_id: Option<super::CellId>,

    /// Smallest cell index whose line info needs recomputing before the next
    /// render. `None` means `cell_line_info` is up to date.
    ///
    /// Streaming appends only touch the changed cell, so this lets
    /// `handle_frame` patch line info incrementally instead of clearing and
    /// rebuilding the whole transcript every frame.
    line_info_dirty: Option<usize>,

    /// Cell id of the last model/preset switch notice, while it is still the
    /// trailing cell. Lets repeated switches replace it in place instead of
    /// stacking one banner per switch. Cleared implicitly once any other cell
    /// becomes the last cell.
    last_switch_cell_id: Option<super::CellId>,
}

impl Default for TranscriptState {
    fn default() -> Self {
        Self {
            cells: Vec::new(),
            scroll: ScrollState::default(),
            wrap_cache: super::WrapCache::new(),
            viewport_height: 20,
            terminal_size: (80, 24),
            selection: SelectionState::new(),
            position_map: PositionMap::new(),
            last_click: None,
            pending_user_cell_id: None,
            active_user_cell_id: None,
            // Force an initial full build on the first frame.
            line_info_dirty: Some(0),
            last_switch_cell_id: None,
        }
    }
}

impl TranscriptState {
    /// Creates a `TranscriptState` with pre-loaded cells.
    pub fn with_cells(cells: Vec<super::HistoryCell>) -> Self {
        Self {
            cells,
            ..Self::default()
        }
    }

    /// Read-only access to cells.
    pub fn cells(&self) -> &[super::HistoryCell] {
        &self.cells
    }

    /// Returns the number of cells that belong to committed/stable history.
    ///
    /// When a prompt is pending or actively running, this excludes that turn and
    /// all cells that followed it so side features can fork from stable context.
    pub fn stable_prefix_len(&self) -> usize {
        let in_flight_id = self.active_user_cell_id.or(self.pending_user_cell_id);
        if let Some(in_flight_id) = in_flight_id
            && let Some(index) = self.cells.iter().position(|cell| cell.id() == in_flight_id)
        {
            return index;
        }
        self.cells.len()
    }

    /// Resets transcript to empty state (for /new, handoff submit).
    ///
    /// Clears cells, scroll, and wrap cache. Keeps viewport/terminal size.
    pub fn reset(&mut self) {
        self.cells.clear();
        self.scroll.reset();
        self.wrap_cache.clear();
        self.pending_user_cell_id = None;
        self.active_user_cell_id = None;
        self.invalidate_line_info();
    }

    /// Removes a cell by its ID.
    ///
    /// Used when a prematurely created cell (e.g., empty streaming assistant)
    /// needs to be removed to maintain correct cell ordering. Returns true
    /// if a cell was removed.
    pub fn remove_cell_by_id(&mut self, id: super::CellId) -> bool {
        if let Some(index) = self.cells.iter().position(|c| c.id() == id) {
            self.cells.remove(index);
            // Everything from the removed slot shifts up.
            self.mark_line_info_dirty_from(index);
            true
        } else {
            false
        }
    }

    /// Pushes a cell and marks line info dirty from the new cell.
    pub fn push_cell(&mut self, cell: super::HistoryCell) {
        if let super::HistoryCell::User { id, .. } = &cell {
            self.pending_user_cell_id = Some(*id);
        }
        self.cells.push(cell);
        self.mark_line_info_dirty_from(self.cells.len() - 1);
    }

    /// Activates the pending user cell for the current turn.
    pub fn activate_pending_user_cell(&mut self) {
        if let Some(id) = self.pending_user_cell_id.take() {
            self.active_user_cell_id = Some(id);
        }
    }

    /// Returns true if a prompt is pending agent start.
    pub fn has_pending_user_cell(&self) -> bool {
        self.pending_user_cell_id.is_some()
    }

    /// Invalidates all cell line info, forcing a full rebuild next frame.
    ///
    /// Prefer `mark_line_info_dirty_from` when the changed cell index is known,
    /// so streaming stays incremental.
    pub fn invalidate_line_info(&mut self) {
        self.mark_line_info_dirty_from(0);
    }

    /// Marks line info dirty starting at `index`, keeping the earliest pending
    /// dirty index so multiple mutations within a frame coalesce correctly.
    fn mark_line_info_dirty_from(&mut self, index: usize) {
        self.line_info_dirty = Some(match self.line_info_dirty {
            Some(existing) => existing.min(index),
            None => index,
        });
    }

    /// Takes the pending line-info dirty range, clearing it.
    ///
    /// Returns `Some(from_index)` when line info needs recomputing from that
    /// cell index, or `None` when it is already up to date.
    pub fn take_line_info_dirty(&mut self) -> Option<usize> {
        self.line_info_dirty.take()
    }

    // ========================================================================
    // Cell Mutation Methods (auto-invalidate line info)
    // ========================================================================

    /// Sets tool result for a cell by `tool_use_id`.
    pub fn set_tool_result_for(
        &mut self,
        tool_id: &str,
        result: zdx_engine::core::events::ToolOutput,
    ) {
        if let Some(index) = self.cells.iter().position(
            |c| matches!(c, super::HistoryCell::Tool { tool_use_id, .. } if tool_use_id == tool_id),
        ) {
            self.cells[index].set_tool_result(result);
            self.mark_line_info_dirty_from(index);
        }
    }

    /// Sets tool input for a cell by `tool_use_id`.
    pub fn set_tool_input_for(&mut self, tool_id: &str, input: serde_json::Value) {
        if let Some(index) = self.cells.iter().position(
            |c| matches!(c, super::HistoryCell::Tool { tool_use_id, .. } if tool_use_id == tool_id),
        ) {
            self.cells[index].set_tool_input(input);
            self.mark_line_info_dirty_from(index);
        }
    }

    /// Sets tool input preview for a cell by `tool_use_id`.
    pub fn set_tool_input_delta_for(&mut self, tool_id: &str, delta: String) {
        if let Some(index) = self.cells.iter().position(
            |c| matches!(c, super::HistoryCell::Tool { tool_use_id, .. } if tool_use_id == tool_id),
        ) {
            self.cells[index].set_tool_input_delta(delta);
            self.mark_line_info_dirty_from(index);
        }
    }

    /// Appends a chunk of streaming tool output for a cell by `tool_use_id`.
    ///
    /// Only appends if the cell is in `ToolState::Running` — silently ignores
    /// deltas for tools that have already completed, errored, or been cancelled.
    pub fn append_tool_output_delta_for(&mut self, tool_id: &str, chunk: &str) {
        if let Some(index) = self.cells.iter().position(
            |c| matches!(c, super::HistoryCell::Tool { tool_use_id, state, .. } if tool_use_id == tool_id && *state == super::ToolState::Running),
        ) {
            self.cells[index].apply_tool_output_delta(chunk);
            self.mark_line_info_dirty_from(index);
        }
    }

    /// Finalizes an assistant cell by `cell_id` (streaming → complete).
    pub fn finalize_assistant_cell(&mut self, cell_id: super::CellId) {
        if let Some(index) = self.cells.iter().position(|c| c.id() == cell_id) {
            self.cells[index].finalize_assistant();
            self.mark_line_info_dirty_from(index);
        }
    }

    /// Finalizes an assistant cell, stripping any `<followups>` block from its
    /// visible text and returning the extracted suggestions.
    pub fn finalize_assistant_cell_extracting_followups(
        &mut self,
        cell_id: super::CellId,
    ) -> Vec<String> {
        let mut items = Vec::new();
        if let Some(index) = self.cells.iter().position(|c| c.id() == cell_id) {
            items = self.cells[index].strip_followups();
            self.cells[index].finalize_assistant();
            self.mark_line_info_dirty_from(index);
        }
        items
    }

    /// Appends delta to a streaming assistant cell by `cell_id`.
    pub fn append_to_streaming_cell(&mut self, cell_id: super::CellId, delta: &str) {
        if let Some(index) = self.cells.iter().position(|c| c.id() == cell_id) {
            self.cells[index].append_assistant_delta(delta);
            self.mark_line_info_dirty_from(index);
        }
    }

    /// Appends delta to the last thinking cell.
    pub fn append_thinking_delta_to_last(&mut self, delta: &str) {
        if let Some((index, cell)) = self.cells.iter_mut().enumerate().next_back() {
            cell.append_thinking_delta(delta);
            self.mark_line_info_dirty_from(index);
        }
    }

    /// Finalizes the last streaming thinking cell.
    pub fn finalize_last_thinking_cell(
        &mut self,
        replay: Option<zdx_engine::providers::ReplayToken>,
    ) -> bool {
        if let Some(index) = self.cells.iter().rposition(|c| {
            matches!(
                c,
                super::HistoryCell::Thinking {
                    is_streaming: true,
                    ..
                }
            )
        }) {
            self.cells[index].finalize_thinking(replay);
            self.mark_line_info_dirty_from(index);
            true
        } else {
            false
        }
    }

    /// Marks all running/streaming cells as cancelled.
    pub fn mark_interrupted(&mut self) {
        let mut any_marked = false;
        for cell in &mut self.cells {
            let was_active = matches!(
                cell,
                super::HistoryCell::Assistant {
                    is_streaming: true,
                    ..
                } | super::HistoryCell::Thinking {
                    is_streaming: true,
                    ..
                } | super::HistoryCell::Tool {
                    state: super::ToolState::Running,
                    ..
                }
            );
            cell.mark_cancelled();
            if was_active {
                any_marked = true;
            }
        }

        // If no streaming/running cells were marked, mark the last user cell
        if !any_marked && let Some(active_id) = self.active_user_cell_id {
            if let Some(active_user) = self.cells.iter_mut().find(|c| c.id() == active_id) {
                active_user.mark_request_interrupted();
            } else if let Some(last_user) = self
                .cells
                .iter_mut()
                .rev()
                .find(|c| matches!(c, super::HistoryCell::User { .. }))
            {
                last_user.mark_request_interrupted();
            }
        } else if !any_marked
            && let Some(last_user) = self
                .cells
                .iter_mut()
                .rev()
                .find(|c| matches!(c, super::HistoryCell::User { .. }))
        {
            last_user.mark_request_interrupted();
        }

        self.active_user_cell_id = None;
        self.invalidate_line_info();
    }

    /// Cancels orphaned running tool cells left at turn completion.
    pub(super) fn cancel_orphaned_running_tools(&mut self) -> usize {
        let mut cancelled = 0;
        for cell in &mut self.cells {
            if matches!(
                cell,
                super::HistoryCell::Tool {
                    state: super::ToolState::Running,
                    ..
                }
            ) {
                cell.set_tool_result(zdx_engine::core::events::ToolOutput::canceled(
                    "Tool result was not received before the turn completed",
                ));
                cancelled += 1;
            }
        }

        if cancelled > 0 {
            self.invalidate_line_info();
        }

        cancelled
    }

    /// Marks all running/streaming cells as errored due to stream/network error.
    ///
    /// Unlike `mark_interrupted`, this doesn't mark user cells since the error
    /// wasn't caused by user cancellation.
    pub fn mark_errored(&mut self) {
        for cell in &mut self.cells {
            cell.mark_errored();
        }
        self.active_user_cell_id = None;
        self.invalidate_line_info();
    }

    /// Applies a cross-slice transcript mutation.
    pub fn apply(&mut self, mutation: TranscriptMutation) {
        match mutation {
            TranscriptMutation::AppendCell(cell) => {
                self.push_cell(*cell);
            }
            TranscriptMutation::AppendSystemMessage(message) => {
                self.push_cell(super::HistoryCell::system(message));
            }
            TranscriptMutation::AppendOrReplaceSwitchNotice(message) => {
                let coalesce = self
                    .last_switch_cell_id
                    .is_some_and(|id| self.cells.last().is_some_and(|c| c.id() == id));
                if coalesce
                    && let Some(super::HistoryCell::System {
                        content,
                        created_at,
                        ..
                    }) = self.cells.last_mut()
                {
                    *content = message;
                    *created_at = chrono::Utc::now();
                    let last = self.cells.len() - 1;
                    self.mark_line_info_dirty_from(last);
                } else {
                    let cell = super::HistoryCell::system(message);
                    let id = cell.id();
                    self.push_cell(cell);
                    self.last_switch_cell_id = Some(id);
                }
            }
            TranscriptMutation::Clear => self.reset(),
            TranscriptMutation::ReplaceCells(cells) => {
                self.cells = cells;
                // Full rebuild: cell identities changed entirely.
                self.invalidate_line_info();
                self.pending_user_cell_id = None;
                self.active_user_cell_id = None;
            }
            TranscriptMutation::ResetScroll => self.scroll.reset(),
            TranscriptMutation::ClearWrapCache => self.wrap_cache.clear(),
            TranscriptMutation::SetScrollOffset { offset } => self.set_scroll_offset(offset),
            TranscriptMutation::SetScrollMode(mode) => self.set_scroll_mode(mode),
            TranscriptMutation::ScrollToTop => self.scroll_to_top(),
            TranscriptMutation::ScrollToBottom => self.scroll_to_bottom(),
            TranscriptMutation::PageUp => self.page_up(),
            TranscriptMutation::PageDown => self.page_down(),
        }
    }

    /// Scrolls up by the given number of lines.
    pub fn scroll_up(&mut self, lines: usize) {
        self.scroll.scroll_up(lines, self.viewport_height);
    }

    /// Scrolls down by the given number of lines.
    pub fn scroll_down(&mut self, lines: usize) {
        self.scroll.scroll_down(lines, self.viewport_height);
    }

    /// Scrolls up by one page.
    pub fn page_up(&mut self) {
        self.scroll.page_up(self.viewport_height);
    }

    /// Scrolls down by one page.
    pub fn page_down(&mut self) {
        self.scroll.page_down(self.viewport_height);
    }

    /// Scrolls to the top of the transcript.
    pub fn scroll_to_top(&mut self) {
        self.scroll.scroll_to_top();
    }

    /// Scrolls to the bottom of the transcript.
    pub fn scroll_to_bottom(&mut self) {
        self.scroll.scroll_to_bottom();
    }

    /// Sets scroll to an anchored offset.
    pub fn set_scroll_offset(&mut self, offset: usize) {
        self.scroll.mode = ScrollMode::Anchored { offset };
    }

    /// Restores scroll mode explicitly (anchored or follow-latest).
    pub fn set_scroll_mode(&mut self, mode: ScrollMode) {
        self.scroll.mode = mode;
    }

    /// Updates layout dimensions based on terminal size and input height.
    pub fn update_layout(&mut self, terminal_size: (u16, u16), viewport_height: usize) {
        self.terminal_size = terminal_size;
        self.viewport_height = viewport_height;
    }

    /// Returns true if text is currently selected.
    pub fn has_selection(&self) -> bool {
        self.selection.has_selection()
    }

    /// Starts a new selection at the given visual position.
    pub fn start_selection(&mut self, line: usize, column: usize) {
        use super::selection::VisualPosition;
        self.selection.start(VisualPosition::new(
            line,
            self.clamp_selection_column(line, column),
        ));
    }

    /// Extends the selection to the given visual position.
    pub fn extend_selection(&mut self, line: usize, column: usize) {
        use super::selection::VisualPosition;
        self.selection.extend(VisualPosition::new(
            line,
            self.clamp_selection_column(line, column),
        ));
    }

    /// Finishes an active selection (mouse button released).
    pub fn finish_selection(&mut self) {
        self.selection.finish();
    }

    /// Gets the selected text using the position map.
    pub fn get_selected_text(&self) -> Option<String> {
        let (start, end) = self.selection.get_range()?;
        Some(self.position_map.get_text_range(start, end))
    }

    /// Copies the selected text to the clipboard and schedules clear.
    ///
    /// Returns `Ok(())` if successful, or an error message if failed.
    ///
    /// # Errors
    /// Returns an error if the operation fails.
    pub fn copy_and_schedule_clear(&mut self) -> Result<(), String> {
        let text = self.get_selected_text().ok_or("No selection")?;
        if text.is_empty() {
            return Err("Selection is empty".to_string());
        }
        super::selection::Clipboard::copy(&text).map_err(|e| e.to_string())?;
        self.selection.schedule_clear();
        Ok(())
    }

    /// Checks if the selection timeout has passed and clears if so.
    ///
    /// Returns true if the selection was cleared.
    pub fn check_selection_timeout(&mut self) -> bool {
        self.selection.check_and_clear()
    }

    /// Records a click and returns true if it qualifies as a double-click.
    pub fn register_click(&mut self, line: usize, column: usize) -> bool {
        let now = Instant::now();
        let is_double_click = self.last_click.as_ref().is_some_and(|last| {
            now.duration_since(last.at) <= DOUBLE_CLICK_MAX_DELAY
                && last.line == line
                && last.column.abs_diff(column) <= DOUBLE_CLICK_MAX_COLUMN_DELTA
        });

        if is_double_click {
            self.last_click = None;
        } else {
            self.last_click = Some(ClickInfo {
                line,
                column,
                at: now,
            });
        }

        is_double_click
    }

    /// Selects the word at the given visual position.
    ///
    /// Returns false if no word was found at the position.
    pub fn select_word_at(&mut self, line: usize, column: usize) -> bool {
        let Some(mapping) = self.position_map.get_by_global_line(line) else {
            return false;
        };

        let graphemes: Vec<&str> = mapping.text.graphemes(true).collect();
        if graphemes.is_empty() {
            return false;
        }

        let mut idx = column;
        if idx >= graphemes.len() {
            idx = graphemes.len().saturating_sub(1);
        }

        if !is_word_grapheme(graphemes[idx]) {
            return false;
        }

        let mut start = idx;
        while start > 0 && is_word_grapheme(graphemes[start - 1]) {
            start -= 1;
        }

        let mut end = idx + 1;
        while end < graphemes.len() && is_word_grapheme(graphemes[end]) {
            end += 1;
        }

        self.selection.start(VisualPosition::new(line, start));
        self.selection.extend(VisualPosition::new(line, end));
        self.selection.finish();
        true
    }

    fn clamp_selection_column(&self, line: usize, column: usize) -> usize {
        self.position_map
            .get_by_global_line(line)
            .map_or(column, |mapping| {
                let prefix = usize::from(mapping.text.starts_with("│ ")) * 2;
                column.max(prefix)
            })
    }
}

fn is_word_grapheme(grapheme: &str) -> bool {
    grapheme.chars().any(|ch| ch.is_alphanumeric() || ch == '_')
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transcript::{HistoryCell, LineMapping};

    fn last_content(state: &TranscriptState) -> Option<&str> {
        match state.cells().last() {
            Some(HistoryCell::System { content, .. }) => Some(content.as_str()),
            _ => None,
        }
    }

    #[test]
    fn switch_notice_coalesces_until_a_message_breaks_the_chain() {
        let mut state = TranscriptState::default();
        for alias in ["Balanced", "Smart", "Fast"] {
            state.apply(TranscriptMutation::AppendOrReplaceSwitchNotice(format!(
                "Switched to {alias}"
            )));
        }
        // Three consecutive switches collapse into one banner with the latest text.
        assert_eq!(state.cells().len(), 1);
        assert_eq!(last_content(&state), Some("Switched to Fast"));

        // A real message breaks the chain, so the next switch appends fresh.
        state.apply(TranscriptMutation::AppendCell(Box::new(HistoryCell::user(
            "hello",
        ))));
        state.apply(TranscriptMutation::AppendOrReplaceSwitchNotice(
            "Switched to Reason".to_string(),
        ));
        assert_eq!(state.cells().len(), 3);
        assert_eq!(last_content(&state), Some("Switched to Reason"));

        // Further switches now coalesce onto the new banner.
        state.apply(TranscriptMutation::AppendOrReplaceSwitchNotice(
            "Switched to Balanced".to_string(),
        ));
        assert_eq!(state.cells().len(), 3);
        assert_eq!(last_content(&state), Some("Switched to Balanced"));
    }

    // ========================================================================
    // Lazy Rendering Tests
    // ========================================================================

    #[test]
    fn test_visible_range_empty_cell_info() {
        let scroll = ScrollState::new();
        assert!(scroll.visible_range(20).is_none());
    }

    #[test]
    fn test_visible_range_single_cell_fits() {
        let mut scroll = ScrollState::new();
        // Single cell with 10 lines
        scroll.patch_cell_line_info(0, vec![10]);

        let visible = scroll.visible_range(20).expect("should have range");
        assert_eq!(visible.cell_range, 0..1);
        assert_eq!(visible.first_cell_line_offset, 0);
        assert_eq!(visible.lines_before, 0);
    }

    #[test]
    fn test_visible_range_multiple_cells_all_visible() {
        let mut scroll = ScrollState::new();
        // 3 cells with 5 lines each = 15 total, viewport 20
        scroll.patch_cell_line_info(0, vec![5, 5, 5]);

        let visible = scroll.visible_range(20).expect("should have range");
        assert_eq!(visible.cell_range, 0..3);
        assert_eq!(visible.first_cell_line_offset, 0);
    }

    #[test]
    fn test_visible_range_scrolled_to_middle() {
        let mut scroll = ScrollState::new();
        // 5 cells with 10 lines each = 50 total
        scroll.patch_cell_line_info(0, vec![10, 10, 10, 10, 10]);

        // Scroll to offset 15 with viewport 20
        scroll.mode = ScrollMode::Anchored { offset: 15 };

        let visible = scroll.visible_range(20).expect("should have range");
        // Offset 15 is in cell 1 (lines 10-19), viewport ends at 35 which is in cell 3
        assert_eq!(visible.cell_range, 1..4);
        assert_eq!(visible.first_cell_line_offset, 5); // 15 - 10 = 5
        assert_eq!(visible.lines_before, 15);
    }

    #[test]
    fn test_visible_range_follow_mode() {
        let mut scroll = ScrollState::new();
        // 5 cells with 10 lines each = 50 total, viewport 20
        scroll.patch_cell_line_info(0, vec![10, 10, 10, 10, 10]);

        // Follow mode should show bottom (offset = 50 - 20 = 30)
        let visible = scroll.visible_range(20).expect("should have range");
        // Offset 30 is in cell 3 (lines 20-29), viewport ends at 50
        assert_eq!(visible.cell_range, 3..5);
        assert_eq!(visible.first_cell_line_offset, 0); // 30 - 30 = 0
        assert_eq!(visible.lines_before, 30);
    }

    #[test]
    fn test_visible_range_partial_first_cell() {
        let mut scroll = ScrollState::new();
        // 3 cells with 20 lines each = 60 total
        scroll.patch_cell_line_info(0, vec![20, 20, 20]);

        // Scroll to offset 5 with viewport 10
        scroll.mode = ScrollMode::Anchored { offset: 5 };

        let visible = scroll.visible_range(10).expect("should have range");
        // Offset 5 is in cell 0, viewport ends at 15 which is still in cell 0
        assert_eq!(visible.cell_range, 0..1);
        assert_eq!(visible.first_cell_line_offset, 5);
    }

    #[test]
    fn test_patch_cell_line_info_updates_cached_line_count() {
        let mut scroll = ScrollState::new();
        scroll.patch_cell_line_info(0, vec![10, 15, 5]);

        assert_eq!(scroll.cached_line_count, 30);
        assert_eq!(scroll.cell_line_info.len(), 3);
        assert_eq!(scroll.cell_line_info[0].start_line, 0);
        assert_eq!(scroll.cell_line_info[1].start_line, 10);
        assert_eq!(scroll.cell_line_info[2].start_line, 25);
    }

    #[test]
    fn test_patch_cell_line_info_incremental_keeps_prefix_and_reflows() {
        let mut scroll = ScrollState::new();
        scroll.patch_cell_line_info(0, vec![10, 15, 5]);

        // Simulate the streaming last cell growing (15 -> 20) plus an appended
        // cell, patched incrementally from index 2.
        scroll.patch_cell_line_info(2, vec![20, 4]);

        assert_eq!(scroll.cell_line_info.len(), 4);
        // Prefix entries are untouched.
        assert_eq!(scroll.cell_line_info[0].start_line, 0);
        assert_eq!(scroll.cell_line_info[1].start_line, 10);
        // Patched entries reflow from the last kept offset (10 + 15 = 25).
        assert_eq!(scroll.cell_line_info[2].start_line, 25);
        assert_eq!(scroll.cell_line_info[2].line_count, 20);
        assert_eq!(scroll.cell_line_info[3].start_line, 45);
        assert_eq!(scroll.cached_line_count, 49);
    }

    #[test]
    fn test_patch_cell_line_info_from_zero_is_full_rebuild() {
        let mut scroll = ScrollState::new();
        scroll.patch_cell_line_info(0, vec![10, 10]);

        scroll.patch_cell_line_info(0, vec![7]);

        assert_eq!(scroll.cell_line_info.len(), 1);
        assert_eq!(scroll.cell_line_info[0].start_line, 0);
        assert_eq!(scroll.cached_line_count, 7);
    }

    #[test]
    fn test_cell_index_for_line() {
        let mut scroll = ScrollState::new();
        scroll.patch_cell_line_info(0, vec![3, 2, 4]);

        assert_eq!(scroll.cell_index_for_line(0), Some(0));
        assert_eq!(scroll.cell_index_for_line(2), Some(0));
        assert_eq!(scroll.cell_index_for_line(3), Some(1));
        assert_eq!(scroll.cell_index_for_line(4), Some(1));
        assert_eq!(scroll.cell_index_for_line(5), Some(2));
        assert_eq!(scroll.cell_index_for_line(8), Some(2));
        assert_eq!(scroll.cell_index_for_line(9), None);
    }

    #[test]
    fn test_selection_clamps_non_selectable_prefix() {
        let mut transcript = TranscriptState::with_cells(vec![]);
        transcript.position_map.clear();
        transcript
            .position_map
            .push(LineMapping::new("│ hello".to_string(), None));

        transcript.start_selection(0, 0);
        transcript.extend_selection(0, 7);
        transcript.finish_selection();

        assert_eq!(transcript.get_selected_text().as_deref(), Some("hello"));
    }

    #[test]
    fn test_reset_clears_cell_line_info() {
        let mut scroll = ScrollState::new();
        scroll.patch_cell_line_info(0, vec![10, 10]);

        scroll.reset();

        assert!(scroll.cell_line_info.is_empty());
        assert_eq!(scroll.cached_line_count, 0);
        assert!(scroll.is_following());
    }

    // ========================================================================
    // ScrollState Tests (moved from state/mod.rs)
    // ========================================================================

    #[test]
    fn test_scroll_state_default() {
        let scroll = ScrollState::default();
        assert!(matches!(scroll.mode, ScrollMode::FollowLatest));
        assert_eq!(scroll.cached_line_count, 0);
        assert!(scroll.is_following());
    }

    #[test]
    fn test_scroll_state_get_offset_follow_mode() {
        let mut scroll = ScrollState::new();
        scroll.update_line_count(100);

        // In follow mode, offset should show the bottom
        let offset = scroll.get_offset(20);
        assert_eq!(offset, 80); // 100 - 20 = 80
    }

    #[test]
    fn test_scroll_state_get_offset_anchored_mode() {
        let mut scroll = ScrollState::new();
        scroll.update_line_count(100);
        scroll.mode = ScrollMode::Anchored { offset: 30 };

        let offset = scroll.get_offset(20);
        assert_eq!(offset, 30);
    }

    #[test]
    fn test_scroll_state_get_offset_clamps_to_max() {
        let mut scroll = ScrollState::new();
        scroll.update_line_count(100);
        scroll.mode = ScrollMode::Anchored { offset: 95 }; // Too close to bottom

        let offset = scroll.get_offset(20);
        assert_eq!(offset, 80); // max_offset = 100 - 20 = 80
    }

    #[test]
    fn test_scroll_state_scroll_up_from_follow() {
        let mut scroll = ScrollState::new();
        scroll.update_line_count(100);

        scroll.scroll_up(5, 20);

        // Should anchor at line 75 (80 - 5)
        assert!(matches!(scroll.mode, ScrollMode::Anchored { offset: 75 }));
    }

    #[test]
    fn test_scroll_state_scroll_up_clamped_to_zero() {
        let mut scroll = ScrollState::new();
        scroll.update_line_count(100);
        scroll.mode = ScrollMode::Anchored { offset: 3 };

        scroll.scroll_up(10, 20); // Would go negative

        assert!(matches!(scroll.mode, ScrollMode::Anchored { offset: 0 }));
    }

    #[test]
    fn test_scroll_state_scroll_down_to_bottom() {
        let mut scroll = ScrollState::new();
        scroll.update_line_count(100);
        scroll.mode = ScrollMode::Anchored { offset: 75 };

        scroll.scroll_down(10, 20); // Would exceed max

        // Should transition to FollowLatest
        assert!(matches!(scroll.mode, ScrollMode::FollowLatest));
    }

    #[test]
    fn test_scroll_state_scroll_down_partial() {
        let mut scroll = ScrollState::new();
        scroll.update_line_count(100);
        scroll.mode = ScrollMode::Anchored { offset: 50 };

        scroll.scroll_down(10, 20);

        assert!(matches!(scroll.mode, ScrollMode::Anchored { offset: 60 }));
    }

    #[test]
    fn test_scroll_state_scroll_down_noop_when_following() {
        let mut scroll = ScrollState::new();
        scroll.update_line_count(100);
        assert!(scroll.is_following());

        scroll.scroll_down(10, 20);

        // Should still be following
        assert!(scroll.is_following());
    }

    #[test]
    fn test_scroll_state_scroll_to_top() {
        let mut scroll = ScrollState::new();
        scroll.update_line_count(100);

        scroll.scroll_to_top();

        assert!(matches!(scroll.mode, ScrollMode::Anchored { offset: 0 }));
    }

    #[test]
    fn test_scroll_state_scroll_to_bottom() {
        let mut scroll = ScrollState::new();
        scroll.update_line_count(100);
        scroll.mode = ScrollMode::Anchored { offset: 30 };

        scroll.scroll_to_bottom();

        assert!(matches!(scroll.mode, ScrollMode::FollowLatest));
    }

    #[test]
    fn test_scroll_state_page_up() {
        let mut scroll = ScrollState::new();
        scroll.update_line_count(100);
        // Start at bottom (follow mode, offset = 80)

        scroll.page_up(20);

        // Should move up by viewport_height (20), so 80 - 20 = 60
        assert!(matches!(scroll.mode, ScrollMode::Anchored { offset: 60 }));
    }

    #[test]
    fn test_scroll_state_page_down() {
        let mut scroll = ScrollState::new();
        scroll.update_line_count(100);
        scroll.mode = ScrollMode::Anchored { offset: 40 };

        scroll.page_down(20);

        assert!(matches!(scroll.mode, ScrollMode::Anchored { offset: 60 }));
    }

    #[test]
    fn test_scroll_state_has_content_below() {
        let mut scroll = ScrollState::new();
        scroll.update_line_count(100);

        // At top, should have content below
        scroll.mode = ScrollMode::Anchored { offset: 0 };
        assert!(scroll.has_content_below(20));

        // At bottom, should not have content below
        scroll.scroll_to_bottom();
        assert!(!scroll.has_content_below(20));
    }

    #[test]
    fn test_scroll_state_reset() {
        let mut scroll = ScrollState::new();
        scroll.update_line_count(100);
        scroll.mode = ScrollMode::Anchored { offset: 50 };

        scroll.reset();

        assert!(matches!(scroll.mode, ScrollMode::FollowLatest));
        assert_eq!(scroll.cached_line_count, 0);
    }
}
