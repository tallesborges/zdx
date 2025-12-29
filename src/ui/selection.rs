//! Transcript selection and copy functionality.
//!
//! This module implements text selection within the transcript using grapheme
//! indices for proper Unicode handling (emoji, CJK, combining characters).
//!
//! ## Position Mapping
//!
//! Selection positions are tracked as `(visual_line, grapheme_column)` through
//! the `PositionMap` which is rebuilt each render.
//!
//! ## Copy Transport
//!
//! Text is copied using (in order of preference):
//! 1. OSC 52 - Terminal clipboard escape sequence (works over SSH)
//! 2. System clipboard via `arboard` crate

use std::cell::RefCell;
use std::io::Write;
use std::time::{Duration, Instant};

use unicode_segmentation::UnicodeSegmentation;

/// How long to keep the selection visible after copying (for visual feedback).
const SELECTION_CLEAR_DELAY: Duration = Duration::from_millis(300);

/// A position in the transcript identified by visual coordinates.
///
/// Uses grapheme indices for column to properly handle Unicode:
/// - "🇺🇸" is 1 grapheme (1 column), even though it's multiple code points
/// - "👨‍👩‍👧" is 1 grapheme (1 column), even though it's multiple code points
/// - "é" is 1 grapheme regardless of whether composed or decomposed
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VisualPosition {
    /// Line index from the top of the flattened transcript (0-indexed).
    pub line: usize,
    /// Grapheme column within the line (0-indexed).
    pub column: usize,
}

impl VisualPosition {
    /// Creates a new visual position.
    pub fn new(line: usize, column: usize) -> Self {
        Self { line, column }
    }
}

impl Ord for VisualPosition {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        (self.line, self.column).cmp(&(other.line, other.column))
    }
}

impl PartialOrd for VisualPosition {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

/// Maps visual lines to their source cells.
///
/// Built during rendering to enable selection position mapping.
#[derive(Debug, Clone)]
pub struct LineMapping {
    /// The rendered text content of this line (for display and selection).
    pub text: String,
}

/// Maps visual positions to cell text positions.
///
/// Rebuilt on each render to track which cell/text each visual line comes from.
/// Uses `RefCell` for interior mutability so it can be updated during immutable
/// render passes (same pattern as `WrapCache`).
#[derive(Debug, Default)]
pub struct PositionMap {
    /// Mapping for each visual line.
    lines: RefCell<Vec<LineMapping>>,
}

impl PositionMap {
    /// Creates an empty position map.
    pub fn new() -> Self {
        Self {
            lines: RefCell::new(Vec::new()),
        }
    }

    /// Clears the position map.
    pub fn clear(&self) {
        self.lines.borrow_mut().clear();
    }

    /// Adds a line mapping.
    pub fn push(&self, mapping: LineMapping) {
        self.lines.borrow_mut().push(mapping);
    }

    /// Returns the number of lines in the map.
    pub fn len(&self) -> usize {
        self.lines.borrow().len()
    }

    /// Gets a clone of the mapping for a visual line.
    pub fn get(&self, line: usize) -> Option<LineMapping> {
        self.lines.borrow().get(line).cloned()
    }

    /// Gets the text content for a range of visual lines.
    ///
    /// Useful for extracting selected text.
    pub fn get_text_range(&self, start: VisualPosition, end: VisualPosition) -> String {
        let (start, end) = if start < end {
            (start, end)
        } else {
            (end, start)
        };

        let lines = self.lines.borrow();
        if lines.is_empty() {
            return String::new();
        }

        let start_line = start.line.min(lines.len().saturating_sub(1));
        let end_line = end.line.min(lines.len().saturating_sub(1));

        let mut result = String::new();

        for line_idx in start_line..=end_line {
            let Some(mapping) = lines.get(line_idx) else {
                continue;
            };

            let graphemes: Vec<&str> = mapping.text.graphemes(true).collect();
            let line_len = graphemes.len();

            let col_start = if line_idx == start_line {
                start.column.min(line_len)
            } else {
                0
            };

            let col_end = if line_idx == end_line {
                end.column.min(line_len)
            } else {
                line_len
            };

            // Extract the selected portion of this line
            let selected: String = graphemes[col_start..col_end].join("");
            result.push_str(&selected);

            // Add newline between lines (not after the last line)
            if line_idx < end_line {
                result.push('\n');
            }
        }

        result
    }
}

/// Current selection state.
#[derive(Debug, Clone, Default)]
pub struct SelectionState {
    /// Selection anchor (where selection started). None if no selection.
    anchor: Option<VisualPosition>,
    /// Selection cursor (current end of selection). None if no selection.
    cursor: Option<VisualPosition>,
    /// Whether the user is actively selecting (mouse button held).
    pub is_selecting: bool,
    /// When to auto-clear the selection (for visual feedback after copy).
    clear_at: Option<Instant>,
}

impl SelectionState {
    /// Creates a new empty selection state.
    pub fn new() -> Self {
        Self::default()
    }

    /// Starts a new selection at the given position.
    pub fn start(&mut self, pos: VisualPosition) {
        self.anchor = Some(pos);
        self.cursor = Some(pos);
        self.is_selecting = true;
        self.clear_at = None; // Cancel any pending clear
    }

    /// Extends the selection to the given position.
    ///
    /// Does nothing if no selection is active.
    pub fn extend(&mut self, pos: VisualPosition) {
        if self.anchor.is_some() {
            self.cursor = Some(pos);
        }
    }

    /// Finishes selecting (mouse button released).
    pub fn finish(&mut self) {
        self.is_selecting = false;
    }

    /// Schedules the selection to be cleared after the standard delay.
    ///
    /// Call this after copying to provide visual feedback.
    pub fn schedule_clear(&mut self) {
        self.clear_at = Some(Instant::now() + SELECTION_CLEAR_DELAY);
    }

    /// Checks if the selection should be cleared and clears it if so.
    ///
    /// Returns true if the selection was cleared.
    pub fn check_and_clear(&mut self) -> bool {
        if let Some(clear_at) = self.clear_at
            && Instant::now() >= clear_at
        {
            self.clear();
            return true;
        }
        false
    }

    /// Clears the current selection.
    pub fn clear(&mut self) {
        self.anchor = None;
        self.cursor = None;
        self.is_selecting = false;
        self.clear_at = None;
    }

    /// Returns true if there's an active selection.
    pub fn has_selection(&self) -> bool {
        self.anchor.is_some() && self.cursor.is_some()
    }

    /// Returns the normalized selection range (start, end) in reading order.
    ///
    /// Returns `None` if no selection is active.
    pub fn get_range(&self) -> Option<(VisualPosition, VisualPosition)> {
        let anchor = self.anchor?;
        let cursor = self.cursor?;

        if anchor < cursor {
            Some((anchor, cursor))
        } else {
            Some((cursor, anchor))
        }
    }

    /// Returns whether the given visual line is (partially) selected.
    ///
    /// Also returns the column range within the line that's selected.
    pub fn line_selection(
        &self,
        line: usize,
        line_grapheme_count: usize,
    ) -> Option<(usize, usize)> {
        let (start, end) = self.get_range()?;

        // Line is completely before or after selection
        if line < start.line || line > end.line {
            return None;
        }

        // Calculate selection bounds within this line
        let start_col = if line == start.line { start.column } else { 0 };

        let end_col = if line == end.line {
            end.column
        } else {
            line_grapheme_count
        };

        // Clamp to line bounds
        let start_col = start_col.min(line_grapheme_count);
        let end_col = end_col.min(line_grapheme_count);

        if start_col == end_col {
            None // Empty selection on this line
        } else {
            Some((start_col, end_col))
        }
    }
}

/// Clipboard interface with multiple transport fallbacks.
pub struct Clipboard;

impl Clipboard {
    /// Copies text to the clipboard.
    ///
    /// Tries in order:
    /// 1. OSC 52 escape sequence (works over SSH)
    /// 2. System clipboard via arboard
    ///
    /// Returns `Ok(())` if any method succeeded.
    pub fn copy(text: &str) -> Result<(), ClipboardError> {
        // Try OSC 52 first (best for terminal apps, works over SSH)
        if Self::copy_osc52(text).is_ok() {
            return Ok(());
        }

        // Fall back to system clipboard
        Self::copy_system(text)
    }

    /// Copies text using OSC 52 escape sequence.
    ///
    /// This writes directly to stdout, which the terminal intercepts
    /// and copies to the system clipboard.
    fn copy_osc52(text: &str) -> Result<(), ClipboardError> {
        use base64::Engine;

        let encoded = base64::engine::general_purpose::STANDARD.encode(text);

        // OSC 52 format: ESC ] 52 ; c ; <base64-data> ESC \
        // - 'c' specifies the clipboard selection (system clipboard)
        let mut stdout = std::io::stdout();
        write!(stdout, "\x1b]52;c;{}\x1b\\", encoded)
            .map_err(|e| ClipboardError::Osc52(e.to_string()))?;
        stdout
            .flush()
            .map_err(|e| ClipboardError::Osc52(e.to_string()))?;

        Ok(())
    }

    /// Copies text using the system clipboard.
    fn copy_system(text: &str) -> Result<(), ClipboardError> {
        let mut clipboard =
            arboard::Clipboard::new().map_err(|e| ClipboardError::System(e.to_string()))?;

        clipboard
            .set_text(text)
            .map_err(|e| ClipboardError::System(e.to_string()))?;

        Ok(())
    }
}

/// Clipboard operation errors.
#[derive(Debug)]
pub enum ClipboardError {
    /// OSC 52 write failed.
    Osc52(String),
    /// System clipboard operation failed.
    System(String),
}

impl std::fmt::Display for ClipboardError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ClipboardError::Osc52(msg) => write!(f, "OSC 52 clipboard failed: {}", msg),
            ClipboardError::System(msg) => write!(f, "System clipboard failed: {}", msg),
        }
    }
}

impl std::error::Error for ClipboardError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_visual_position_ordering() {
        let a = VisualPosition::new(0, 5);
        let b = VisualPosition::new(0, 10);
        let c = VisualPosition::new(1, 0);

        assert!(a < b);
        assert!(b < c);
        assert!(a < c);
        assert!(!(b < a));
    }

    #[test]
    fn test_selection_state_start_and_extend() {
        let mut sel = SelectionState::new();
        assert!(!sel.has_selection());

        sel.start(VisualPosition::new(0, 5));
        assert!(sel.has_selection());
        assert!(sel.is_selecting);

        sel.extend(VisualPosition::new(1, 10));
        let (start, end) = sel.get_range().unwrap();
        assert_eq!(start, VisualPosition::new(0, 5));
        assert_eq!(end, VisualPosition::new(1, 10));

        sel.finish();
        assert!(!sel.is_selecting);
        assert!(sel.has_selection()); // Selection persists after finish
    }

    #[test]
    fn test_selection_state_reverse_order() {
        let mut sel = SelectionState::new();
        sel.start(VisualPosition::new(5, 10));
        sel.extend(VisualPosition::new(2, 5));

        let (start, end) = sel.get_range().unwrap();
        assert_eq!(start, VisualPosition::new(2, 5));
        assert_eq!(end, VisualPosition::new(5, 10));
    }

    #[test]
    fn test_selection_state_clear() {
        let mut sel = SelectionState::new();
        sel.start(VisualPosition::new(0, 0));
        sel.extend(VisualPosition::new(1, 5));
        assert!(sel.has_selection());

        sel.clear();
        assert!(!sel.has_selection());
        assert!(!sel.is_selecting);
    }

    #[test]
    fn test_line_selection() {
        let mut sel = SelectionState::new();
        sel.start(VisualPosition::new(1, 5));
        sel.extend(VisualPosition::new(3, 10));

        // Line 0: before selection
        assert!(sel.line_selection(0, 20).is_none());

        // Line 1: starts at column 5
        assert_eq!(sel.line_selection(1, 20), Some((5, 20)));

        // Line 2: fully selected
        assert_eq!(sel.line_selection(2, 15), Some((0, 15)));

        // Line 3: ends at column 10
        assert_eq!(sel.line_selection(3, 20), Some((0, 10)));

        // Line 4: after selection
        assert!(sel.line_selection(4, 20).is_none());
    }

    #[test]
    fn test_position_map_get_text_range() {
        let map = PositionMap::new();

        map.push(LineMapping {
            text: "Hello".to_string(),
        });
        map.push(LineMapping {
            text: "World!".to_string(),
        });

        // Select "llo\nWor"
        let text = map.get_text_range(VisualPosition::new(0, 2), VisualPosition::new(1, 3));
        assert_eq!(text, "llo\nWor");
    }

    #[test]
    fn test_position_map_unicode() {
        let map = PositionMap::new();

        // Text with emoji and CJK
        map.push(LineMapping {
            text: "🎉你好AB".to_string(), // 5 graphemes: 🎉, 你, 好, A, B
        });

        // Select graphemes 1-3 (你好A)
        let text = map.get_text_range(VisualPosition::new(0, 1), VisualPosition::new(0, 4));
        assert_eq!(text, "你好A");
    }
}
