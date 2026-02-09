//! Custom scrollbar widget with stable thumb size.
//!
//! ratatui's built-in Scrollbar rounds `thumb_start` and `thumb_end` separately,
//! which causes the thumb size to fluctuate with scroll position. This custom
//! implementation computes a fixed thumb length and positions it manually.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::widgets::Widget;

/// Symbol for the thumb (scrollable indicator).
const THUMB_SYMBOL: &str = "█";
/// Symbol for the track (background).
const TRACK_SYMBOL: &str = "│";

/// A scrollbar with stable thumb size that reaches the bottom when fully scrolled.
///
/// Unlike ratatui's Scrollbar, this implementation:
/// - Computes thumb length once (no size fluctuation during scrolling)
/// - Positions thumb so it reaches exactly the bottom at max scroll
///
/// Implements the `Widget` trait for use with `frame.render_widget()`.
#[derive(Debug, Clone)]
pub struct Scrollbar {
    /// Total number of lines in content.
    total_lines: usize,
    /// Number of visible lines in viewport.
    viewport_height: usize,
    /// Current scroll offset (0 = top).
    scroll_offset: usize,
}

impl Scrollbar {
    /// Creates a new scrollbar.
    ///
    /// # Arguments
    /// * `total_lines` - Total number of lines in the scrollable content
    /// * `viewport_height` - Number of visible lines
    /// * `scroll_offset` - Current scroll position (0 = top)
    pub fn new(total_lines: usize, viewport_height: usize, scroll_offset: usize) -> Self {
        Self {
            total_lines,
            viewport_height,
            scroll_offset,
        }
    }

    /// Returns true if the scrollbar should be displayed.
    ///
    /// Only shows when there's content to scroll.
    fn should_display(&self) -> bool {
        self.total_lines > self.viewport_height
    }
}

impl Widget for Scrollbar {
    fn render(self, area: Rect, buf: &mut Buffer) {
        if !self.should_display() {
            return;
        }

        let max_scroll = self.total_lines.saturating_sub(self.viewport_height);
        let track_len = area.height as usize;
        let viewport_len = self.viewport_height.min(track_len);

        if track_len == 0 || max_scroll == 0 {
            return;
        }

        // Compute fixed thumb length using formula that matches ratatui's size at top position.
        // Formula: round(track_len * viewport_len / (total_lines - 1 + viewport_len))
        let denom = self
            .total_lines
            .saturating_sub(1)
            .saturating_add(viewport_len);
        let thumb_len = if denom > 0 {
            let numerator = track_len as u64 * viewport_len as u64;
            let rounded = (numerator + (denom as u64 / 2)) / denom as u64;
            (rounded as usize).clamp(1, track_len)
        } else {
            track_len
        };

        // Position thumb so it reaches bottom exactly when fully scrolled.
        // Formula: scroll_offset * (track_len - thumb_len) / max_scroll
        let available = track_len.saturating_sub(thumb_len);
        let thumb_start =
            ((self.scroll_offset as u64 * available as u64) / max_scroll as u64) as usize;

        // Render scrollbar on the right edge of the area
        let x = area.x + area.width.saturating_sub(1);
        for (idx, y) in (area.y..area.y + area.height).enumerate() {
            let symbol = if idx >= thumb_start && idx < thumb_start + thumb_len {
                THUMB_SYMBOL
            } else {
                TRACK_SYMBOL
            };
            buf.set_string(x, y, symbol, ratatui::style::Style::default());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_should_display_when_content_exceeds_viewport() {
        let scrollbar = Scrollbar::new(100, 20, 0);
        assert!(scrollbar.should_display());
    }

    #[test]
    fn test_should_not_display_when_content_fits() {
        let scrollbar = Scrollbar::new(10, 20, 0);
        assert!(!scrollbar.should_display());
    }

    #[test]
    fn test_should_not_display_when_equal() {
        let scrollbar = Scrollbar::new(20, 20, 0);
        assert!(!scrollbar.should_display());
    }
}
