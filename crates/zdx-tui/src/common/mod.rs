//! Shared leaf types for TUI features.
//!
//! Contains types with no feature dependencies (clipboard, tasks, text helpers).
//! These types are shared across all feature modules.
//!
//! IMPORTANT: This module must NOT import `UiEvent` or feature-specific state
//! to avoid circular dependencies.

pub mod clipboard;
pub mod commands;
pub mod notify;
pub mod scrollbar;
pub mod task;

pub use clipboard::Clipboard;
pub use scrollbar::Scrollbar;
pub use task::{TaskCompleted, TaskId, TaskKind, TaskMeta, TaskSeq, TaskStarted, Tasks};
// Text helpers now live in the shared `zdx-transcript` crate; re-export them
// here so existing `crate::common::…` call sites keep working.
pub use zdx_transcript::text::{
    ratatui_text, ratatui_width, sanitize_for_display, truncate_start_with_ellipsis,
    truncate_with_ellipsis,
};

/// Maps a horizontal display-column offset to a grapheme index within `text`.
///
/// Walks graphemes accumulating display width until `content_x` is reached,
/// returning the grapheme index at that column (clamped to the line end).
/// Shared by transcript and overlay selection so screen→text mapping stays
/// consistent.
pub fn grapheme_col_at_width(text: &str, content_x: usize) -> usize {
    use unicode_segmentation::UnicodeSegmentation;

    let mut accumulated = 0usize;
    let mut grapheme_idx = 0usize;
    for grapheme in text.graphemes(true) {
        let grapheme_width = ratatui_width(grapheme);
        if accumulated + grapheme_width > content_x {
            break;
        }
        accumulated += grapheme_width;
        grapheme_idx += 1;
    }
    grapheme_idx
}
