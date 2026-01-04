//! Transcript display state.
//!
//! Manages scroll position, viewport dimensions, selection state, and
//! rendering cache for the transcript area.

use std::ops::Range;

use super::selection::{PositionMap, SelectionState};
use super::CellId;

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
}

/// Accumulator for mouse scroll deltas.
///
/// Coalesces rapid scroll events (especially from trackpads) into a single
/// scroll operation per frame, improving smoothness and reducing jitter.
///
/// Convention: positive delta = scroll down, negative delta = scroll up.
#[derive(Debug, Clone, Default)]
pub struct ScrollAccumulator {
    /// Accumulated scroll delta (positive = down, negative = up).
    pending_delta: i32,
}

impl ScrollAccumulator {
    /// Accumulates a scroll delta.
    ///
    /// Positive values scroll down, negative values scroll up.
    pub fn accumulate(&mut self, delta: i32) {
        self.pending_delta += delta;
    }

    /// Takes the accumulated delta, resetting it to zero.
    ///
    /// Returns the delta to apply (positive = down, negative = up).
    pub fn take_delta(&mut self) -> i32 {
        std::mem::take(&mut self.pending_delta)
    }

    /// Returns the current pending delta without consuming it.
    #[cfg(test)]
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
    /// Transcript cells (in-memory display).
    pub cells: Vec<super::HistoryCell>,

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
        }
    }
}

impl TranscriptState {
    /// Creates a new TranscriptState with default values.
    pub fn new() -> Self {
        Self::default()
    }

    /// Resets transcript to empty state (for /new, handoff submit).
    ///
    /// Clears cells, scroll, and wrap cache. Keeps viewport/terminal size.
    pub fn reset(&mut self) {
        self.cells.clear();
        self.scroll.reset();
        self.wrap_cache.clear();
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
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_scroll_accumulator_coalesces_and_resets() {
        let mut acc = ScrollAccumulator::default();

        // Accumulate mixed directions
        acc.accumulate(5); // down
        acc.accumulate(-3); // up
        acc.accumulate(1); // down
        assert_eq!(acc.peek_delta(), 3); // net: down 3

        // Take consumes and resets
        let delta = acc.take_delta();
        assert_eq!(delta, 3);
        assert_eq!(acc.take_delta(), 0); // Already taken
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
}
