//! Transcript display state.
//!
//! Manages scroll position, viewport dimensions, and rendering cache for
//! the transcript area.

/// Scroll mode for the transcript.
#[derive(Debug, Clone)]
pub enum ScrollMode {
    /// Auto-scroll to show latest content (bottom of transcript).
    FollowLatest,
    /// User scrolled manually; offset is line index from top.
    Anchored { offset: usize },
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
}

impl Default for ScrollState {
    fn default() -> Self {
        Self {
            mode: ScrollMode::FollowLatest,
            cached_line_count: 0,
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
    pub fn update_line_count(&mut self, line_count: usize) {
        self.cached_line_count = line_count;
    }

    /// Resets scroll state to follow mode (e.g., after clearing transcript).
    pub fn reset(&mut self) {
        self.mode = ScrollMode::FollowLatest;
        self.cached_line_count = 0;
    }
}

/// Transcript display state.
///
/// Encapsulates all state related to displaying the transcript: cells, scroll,
/// layout dimensions, and rendering cache.
#[derive(Debug)]
pub struct TranscriptState {
    /// Transcript cells (in-memory display).
    pub cells: Vec<crate::ui::transcript::HistoryCell>,

    /// Scroll state (mode, offset, cached line count).
    pub scroll: ScrollState,

    /// Cache for wrapped line rendering.
    pub wrap_cache: crate::ui::transcript::WrapCache,

    /// Available height for transcript viewport.
    pub viewport_height: usize,

    /// Current terminal dimensions (width, height).
    pub terminal_size: (u16, u16),
}

impl Default for TranscriptState {
    fn default() -> Self {
        Self {
            cells: Vec::new(),
            scroll: ScrollState::default(),
            wrap_cache: crate::ui::transcript::WrapCache::new(),
            viewport_height: 20,
            terminal_size: (80, 24),
        }
    }
}

impl TranscriptState {
    /// Creates a new TranscriptState with default values.
    pub fn new() -> Self {
        Self::default()
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
}
