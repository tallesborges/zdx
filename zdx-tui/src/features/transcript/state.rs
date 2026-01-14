//! Transcript display state.
//!
//! Manages scroll position, viewport dimensions, selection state, and
//! rendering cache for the transcript area.

use std::ops::Range;
use std::time::{Duration, Instant};

use unicode_segmentation::UnicodeSegmentation;

use super::CellId;
use super::selection::{PositionMap, SelectionState};
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
    /// Unique cell ID (stored for debugging and future extensibility).
    #[allow(dead_code)]
    pub cell_id: CellId,
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
    /// Creates a new ScrollState in follow mode.
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
    /// In FollowLatest mode, calculates offset to show bottom of content.
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
    /// Transitions to FollowLatest mode when reaching the bottom.
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
    /// Note: For production use, prefer update_cell_line_info() which
    /// updates both cell info and cached_line_count.
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
    /// Returns `None` if cell_line_info is empty or not yet populated.
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

    /// Updates cell line info from rendered cells.
    ///
    /// Call this after rendering to update visibility calculations.
    /// The `line_counts` iterator should yield (cell_id, line_count) pairs
    /// in cell order. Also updates cached_line_count.
    pub fn update_cell_line_info<I>(&mut self, line_counts: I)
    where
        I: IntoIterator<Item = (CellId, usize)>,
    {
        self.cell_line_info.clear();
        let mut cumulative_offset = 0;

        for (cell_id, line_count) in line_counts {
            self.cell_line_info.push(CellLineInfo {
                cell_id,
                start_line: cumulative_offset,
                line_count,
            });
            cumulative_offset += line_count;
        }

        self.cached_line_count = cumulative_offset;
    }

    /// Returns the start line for a given cell index, if available.
    pub fn cell_start_line(&self, cell_index: usize) -> Option<usize> {
        self.cell_line_info
            .get(cell_index)
            .map(|info| info.start_line)
    }
}

/// Accumulator for mouse scroll deltas with acceleration.
///
/// Coalesces rapid scroll events (especially from trackpads) into a single
/// scroll operation per frame, improving smoothness and reducing jitter.
///
/// Features scroll acceleration: starts slow (1 line) for precision,
/// then speeds up if the user keeps scrolling in the same direction.
///
/// Convention: positive delta = scroll down, negative delta = scroll up.
#[derive(Debug, Clone, Default)]
pub struct ScrollAccumulator {
    /// Accumulated scroll delta (positive = down, negative = up).
    pending_delta: i32,
    /// Consecutive frames scrolling in the same direction.
    consecutive_frames: u8,
    /// Direction of last scroll (-1 = up, 0 = none, 1 = down).
    last_direction: i8,
}

impl ScrollAccumulator {
    /// Accumulates a scroll delta.
    ///
    /// Positive values scroll down, negative values scroll up.
    pub fn accumulate(&mut self, delta: i32) {
        self.pending_delta += delta;
    }

    /// Takes the accumulated delta and returns the lines to scroll.
    ///
    /// Implements acceleration: starts at 1 line, increases with consecutive
    /// frames scrolling in the same direction, resets on direction change.
    pub fn take_delta(&mut self) -> i32 {
        let raw_delta = std::mem::take(&mut self.pending_delta);
        if raw_delta == 0 {
            // No scroll this frame - reset acceleration
            self.consecutive_frames = 0;
            self.last_direction = 0;
            return 0;
        }

        let current_direction = raw_delta.signum() as i8;

        // Check if direction changed
        if current_direction != self.last_direction {
            self.consecutive_frames = 1;
            self.last_direction = current_direction;
        } else {
            self.consecutive_frames = self.consecutive_frames.saturating_add(1);
        }

        // Log2-based acceleration: smooth, unbounded growth.
        // Formula: 1 + floor(log2(max(1, consecutive_frames - 1)))
        // This gives: frames 1-2 → 1, frame 3+ → grows logarithmically.
        let multiplier = {
            let adjusted = self.consecutive_frames.saturating_sub(1).max(1) as f64;
            (1.0 + adjusted.log2()).floor() as u32
        };

        // Apply multiplier but cap at the raw delta magnitude
        let max_lines = raw_delta.unsigned_abs().max(1);
        let lines = multiplier.min(max_lines);

        // Return with correct sign
        if raw_delta < 0 {
            -(lines as i32)
        } else {
            lines as i32
        }
    }

    /// Returns the current pending delta without consuming it.
    pub fn peek_delta(&self) -> i32 {
        self.pending_delta
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

    /// Accumulator for mouse scroll deltas (coalesces events within a frame).
    pub scroll_accumulator: ScrollAccumulator,

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
}

impl Default for TranscriptState {
    fn default() -> Self {
        Self {
            cells: Vec::new(),
            scroll: ScrollState::default(),
            scroll_accumulator: ScrollAccumulator::default(),
            wrap_cache: super::WrapCache::new(),
            viewport_height: 20,
            terminal_size: (80, 24),
            selection: SelectionState::new(),
            position_map: PositionMap::new(),
            last_click: None,
            pending_user_cell_id: None,
            active_user_cell_id: None,
        }
    }
}

impl TranscriptState {
    /// Creates a new TranscriptState with default values.
    #[allow(dead_code)]
    pub fn new() -> Self {
        Self::default()
    }

    /// Creates a TranscriptState with pre-loaded cells.
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

    /// Resets transcript to empty state (for /new, handoff submit).
    ///
    /// Clears cells, scroll, and wrap cache. Keeps viewport/terminal size.
    pub fn reset(&mut self) {
        self.cells.clear();
        self.scroll.reset();
        self.wrap_cache.clear();
        self.pending_user_cell_id = None;
        self.active_user_cell_id = None;
    }

    /// Pushes a cell and invalidates line info.
    pub fn push_cell(&mut self, cell: super::HistoryCell) {
        if let super::HistoryCell::User { id, .. } = &cell {
            self.pending_user_cell_id = Some(*id);
        }
        self.cells.push(cell);
        self.scroll.cell_line_info.clear();
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

    /// Invalidates cell line info to force full rendering.
    pub fn invalidate_line_info(&mut self) {
        self.scroll.cell_line_info.clear();
    }

    // ========================================================================
    // Cell Mutation Methods (auto-invalidate line info)
    // ========================================================================

    /// Sets tool result for a cell by tool_use_id.
    pub fn set_tool_result_for(
        &mut self,
        tool_id: &str,
        result: zdx_core::core::events::ToolOutput,
    ) {
        if let Some(cell) = self.cells.iter_mut().find(
            |c| matches!(c, super::HistoryCell::Tool { tool_use_id, .. } if tool_use_id == tool_id),
        ) {
            cell.set_tool_result(result);
            self.invalidate_line_info();
        }
    }

    /// Sets tool input for a cell by tool_use_id.
    pub fn set_tool_input_for(&mut self, tool_id: &str, input: serde_json::Value) {
        if let Some(cell) = self.cells.iter_mut().find(
            |c| matches!(c, super::HistoryCell::Tool { tool_use_id, .. } if tool_use_id == tool_id),
        ) {
            cell.set_tool_input(input);
            self.invalidate_line_info();
        }
    }

    /// Finalizes an assistant cell by cell_id (streaming → complete).
    pub fn finalize_assistant_cell(&mut self, cell_id: super::CellId) {
        if let Some(cell) = self.cells.iter_mut().find(|c| c.id() == cell_id) {
            cell.finalize_assistant();
            self.invalidate_line_info();
        }
    }

    /// Appends delta to a streaming assistant cell by cell_id.
    pub fn append_to_streaming_cell(&mut self, cell_id: super::CellId, delta: &str) {
        if let Some(cell) = self.cells.iter_mut().find(|c| c.id() == cell_id) {
            cell.append_assistant_delta(delta);
            self.invalidate_line_info();
        }
    }

    /// Appends delta to the last thinking cell.
    pub fn append_thinking_delta_to_last(&mut self, delta: &str) {
        if let Some(cell) = self.cells.last_mut() {
            cell.append_thinking_delta(delta);
            self.invalidate_line_info();
        }
    }

    /// Finalizes the last streaming thinking cell.
    pub fn finalize_last_thinking_cell(
        &mut self,
        replay: Option<zdx_core::providers::ReplayToken>,
    ) {
        if let Some(cell) = self.cells.iter_mut().rev().find(|c| {
            matches!(
                c,
                super::HistoryCell::Thinking {
                    is_streaming: true,
                    ..
                }
            )
        }) {
            cell.finalize_thinking(replay);
            self.invalidate_line_info();
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

    /// Applies a cross-slice transcript mutation.
    pub fn apply(&mut self, mutation: TranscriptMutation) {
        match mutation {
            TranscriptMutation::AppendCell(cell) => {
                self.push_cell(cell);
            }
            TranscriptMutation::AppendSystemMessage(message) => {
                self.push_cell(super::HistoryCell::system(message));
            }
            TranscriptMutation::Clear => self.reset(),
            TranscriptMutation::ReplaceCells(cells) => {
                self.cells = cells;
                // Invalidate cell line info so visible_range() falls back to full render
                self.scroll.cell_line_info.clear();
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
        self.selection.start(VisualPosition::new(line, column));
    }

    /// Extends the selection to the given visual position.
    pub fn extend_selection(&mut self, line: usize, column: usize) {
        use super::selection::VisualPosition;
        self.selection.extend(VisualPosition::new(line, column));
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
        let is_double_click = self
            .last_click
            .filter(|last| {
                now.duration_since(last.at) <= DOUBLE_CLICK_MAX_DELAY
                    && last.line == line
                    && last.column.abs_diff(column) <= DOUBLE_CLICK_MAX_COLUMN_DELTA
            })
            .is_some();

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

        use super::selection::VisualPosition;
        self.selection.start(VisualPosition::new(line, start));
        self.selection.extend(VisualPosition::new(line, end));
        self.selection.finish();
        true
    }
}

fn is_word_grapheme(grapheme: &str) -> bool {
    grapheme.chars().any(|ch| ch.is_alphanumeric() || ch == '_')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_scroll_accumulator_acceleration() {
        let mut acc = ScrollAccumulator::default();

        // First frame: starts at 1 line
        acc.accumulate(5);
        assert_eq!(acc.take_delta(), 1);

        // Second frame same direction: still 1 line
        acc.accumulate(5);
        assert_eq!(acc.take_delta(), 1);

        // Third frame same direction: accelerates to 2
        acc.accumulate(5);
        assert_eq!(acc.take_delta(), 2);

        // Direction change resets acceleration
        acc.accumulate(-5);
        assert_eq!(acc.take_delta(), -1);

        // No scroll resets acceleration
        assert_eq!(acc.take_delta(), 0);
        acc.accumulate(5);
        assert_eq!(acc.take_delta(), 1); // Back to 1
    }

    // ========================================================================
    // Lazy Rendering Tests
    // ========================================================================

    fn make_test_cell_id(n: u64) -> CellId {
        CellId(n)
    }

    #[test]
    fn test_visible_range_empty_cell_info() {
        let scroll = ScrollState::new();
        assert!(scroll.visible_range(20).is_none());
    }

    #[test]
    fn test_visible_range_single_cell_fits() {
        let mut scroll = ScrollState::new();
        // Single cell with 10 lines
        scroll.update_cell_line_info(vec![(make_test_cell_id(1), 10)]);

        let visible = scroll.visible_range(20).expect("should have range");
        assert_eq!(visible.cell_range, 0..1);
        assert_eq!(visible.first_cell_line_offset, 0);
        assert_eq!(visible.lines_before, 0);
    }

    #[test]
    fn test_visible_range_multiple_cells_all_visible() {
        let mut scroll = ScrollState::new();
        // 3 cells with 5 lines each = 15 total, viewport 20
        scroll.update_cell_line_info(vec![
            (make_test_cell_id(1), 5),
            (make_test_cell_id(2), 5),
            (make_test_cell_id(3), 5),
        ]);

        let visible = scroll.visible_range(20).expect("should have range");
        assert_eq!(visible.cell_range, 0..3);
        assert_eq!(visible.first_cell_line_offset, 0);
    }

    #[test]
    fn test_visible_range_scrolled_to_middle() {
        let mut scroll = ScrollState::new();
        // 5 cells with 10 lines each = 50 total
        scroll.update_cell_line_info(vec![
            (make_test_cell_id(1), 10), // lines 0-9
            (make_test_cell_id(2), 10), // lines 10-19
            (make_test_cell_id(3), 10), // lines 20-29
            (make_test_cell_id(4), 10), // lines 30-39
            (make_test_cell_id(5), 10), // lines 40-49
        ]);

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
        scroll.update_cell_line_info(vec![
            (make_test_cell_id(1), 10),
            (make_test_cell_id(2), 10),
            (make_test_cell_id(3), 10),
            (make_test_cell_id(4), 10),
            (make_test_cell_id(5), 10),
        ]);

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
        scroll.update_cell_line_info(vec![
            (make_test_cell_id(1), 20), // lines 0-19
            (make_test_cell_id(2), 20), // lines 20-39
            (make_test_cell_id(3), 20), // lines 40-59
        ]);

        // Scroll to offset 5 with viewport 10
        scroll.mode = ScrollMode::Anchored { offset: 5 };

        let visible = scroll.visible_range(10).expect("should have range");
        // Offset 5 is in cell 0, viewport ends at 15 which is still in cell 0
        assert_eq!(visible.cell_range, 0..1);
        assert_eq!(visible.first_cell_line_offset, 5);
    }

    #[test]
    fn test_update_cell_line_info_updates_cached_line_count() {
        let mut scroll = ScrollState::new();
        scroll.update_cell_line_info(vec![
            (make_test_cell_id(1), 10),
            (make_test_cell_id(2), 15),
            (make_test_cell_id(3), 5),
        ]);

        assert_eq!(scroll.cached_line_count, 30);
        assert_eq!(scroll.cell_line_info.len(), 3);
        assert_eq!(scroll.cell_line_info[0].start_line, 0);
        assert_eq!(scroll.cell_line_info[1].start_line, 10);
        assert_eq!(scroll.cell_line_info[2].start_line, 25);
    }

    #[test]
    fn test_reset_clears_cell_line_info() {
        let mut scroll = ScrollState::new();
        scroll.update_cell_line_info(vec![(make_test_cell_id(1), 10), (make_test_cell_id(2), 10)]);

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
