//! Text utilities for TUI rendering.
//!
//! Shared text processing functions used across rendering paths.

use std::borrow::Cow;

use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

const VS16: char = '\u{FE0F}';

pub fn ratatui_text(s: &str) -> Cow<'_, str> {
    if s.contains(VS16) {
        Cow::Owned(s.replace(VS16, ""))
    } else {
        Cow::Borrowed(s)
    }
}

pub fn ratatui_width(s: &str) -> usize {
    ratatui_text(s).width()
}

/// Truncates text for Ratatui rendering, preserving source graphemes.
pub fn truncate_with_ellipsis(text: &str, max_width: usize) -> String {
    if ratatui_width(text) <= max_width {
        return text.to_string();
    }
    if max_width <= 1 {
        return "…".to_string();
    }

    let target = max_width - ratatui_width("…");
    let mut truncated = String::new();
    let mut width = 0;
    for grapheme in text.graphemes(true) {
        let grapheme_width = ratatui_width(grapheme);
        if width + grapheme_width > target {
            break;
        }
        truncated.push_str(grapheme);
        width += grapheme_width;
    }
    truncated.push('…');
    truncated
}

/// Truncates text from the start for Ratatui rendering, preserving source graphemes.
pub fn truncate_start_with_ellipsis(text: &str, max_width: usize) -> String {
    if ratatui_width(text) <= max_width {
        return text.to_string();
    }
    if max_width <= 1 {
        return "…".to_string();
    }

    let target = max_width - ratatui_width("…");
    let graphemes: Vec<&str> = text.graphemes(true).collect();
    let mut kept: Vec<&str> = Vec::new();
    let mut width = 0;

    for grapheme in graphemes.iter().rev() {
        let grapheme_width = ratatui_width(grapheme);
        if width + grapheme_width > target {
            break;
        }
        kept.push(*grapheme);
        width += grapheme_width;
    }

    let mut result = String::from("…");
    for grapheme in kept.iter().rev() {
        result.push_str(grapheme);
    }
    result
}

/// Sanitizes a line for display by removing ANSI escapes and expanding tabs.
///
/// This combines common sanitization steps needed for tool output and other
/// external text that may contain control characters.
///
/// ### Tab Expansion
/// Tabs cause rendering issues because `unicode_width` returns `None` (treated as 0)
/// for control characters, but terminals render tabs as variable-width spaces
/// (to the next tab stop, typically every 8 columns).
///
/// This function uses a fixed 4-space expansion for simplicity, matching
/// `OpenAI`'s Codex CLI approach. This is a pragmatic "good enough" solution
/// that works correctly for the common case of tabs at line start, with minor
/// inaccuracy for mid-line tabs.
///
/// # Arguments
/// * `s` - The string to sanitize
///
/// # Returns
/// A `Cow<str>` - borrowed if no sanitization needed, owned if changes were made.
pub fn sanitize_for_display(s: &str) -> Cow<'_, str> {
    // Only allocate if we actually need to make changes
    if s.contains('\x1b') || s.contains('\t') {
        // Strip ANSI escape codes (remove \x1b to break sequences)
        // and expand tabs to spaces
        Cow::Owned(s.replace('\x1b', "").replace('\t', "    "))
    } else {
        Cow::Borrowed(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_truncate_with_ellipsis_short() {
        assert_eq!(truncate_with_ellipsis("hello", 10), "hello");
    }

    #[test]
    fn test_truncate_with_ellipsis_exact() {
        assert_eq!(truncate_with_ellipsis("hello", 5), "hello");
    }

    #[test]
    fn test_truncate_with_ellipsis_truncated() {
        assert_eq!(truncate_with_ellipsis("hello world", 8), "hello w…");
    }

    #[test]
    fn test_truncate_with_ellipsis_very_short() {
        assert_eq!(truncate_with_ellipsis("hello", 1), "…");
    }

    #[test]
    fn test_sanitize_for_display_ansi_and_tabs() {
        let result = sanitize_for_display("\x1b[31mred\x1b[0m\ttext");
        assert_eq!(result, "[31mred[0m    text");
    }

    #[test]
    fn test_sanitize_for_display_clean() {
        let result = sanitize_for_display("clean text");
        assert_eq!(result, "clean text");
    }

    #[test]
    fn test_sanitize_for_display_only_tabs() {
        let result = sanitize_for_display("hello\tworld");
        assert!(matches!(result, Cow::Owned(_)));
        assert_eq!(result, "hello    world");
    }

    #[test]
    fn test_truncate_with_ellipsis_wide_emoji() {
        // Emoji like 🎉 takes 2 terminal columns
        // "hello 🎉" = 5 + 1 + 2 = 8 columns
        let text = "hello 🎉 world";
        // With max_width=10, we should fit "hello 🎉" (8 cols) + ellipsis (1)
        let result = truncate_with_ellipsis(text, 10);
        assert_eq!(result, "hello 🎉 …");
    }

    #[test]
    fn test_truncate_with_ellipsis_wide_cjk() {
        // CJK characters take 2 terminal columns each
        // "中文" = 4 columns, "test" = 4 columns
        let text = "中文test";
        // With max_width=6, we should fit "中文t" (5 cols) + ellipsis (1)
        let result = truncate_with_ellipsis(text, 6);
        assert_eq!(result, "中文t…");
    }

    #[test]
    fn test_truncate_with_ellipsis_mixed_width() {
        // Mix of narrow (1 col) and wide (2 col) characters
        let text = "a中b文c";
        // Width: 1 + 2 + 1 + 2 + 1 = 7 columns
        assert_eq!(truncate_with_ellipsis(text, 7), "a中b文c");
        assert_eq!(truncate_with_ellipsis(text, 6), "a中b…");
        assert_eq!(truncate_with_ellipsis(text, 5), "a中b…");
        assert_eq!(truncate_with_ellipsis(text, 4), "a中…");
    }

    #[test]
    fn test_truncate_start_with_ellipsis_short() {
        assert_eq!(truncate_start_with_ellipsis("hello", 10), "hello");
    }

    #[test]
    fn test_truncate_start_with_ellipsis_exact() {
        assert_eq!(truncate_start_with_ellipsis("hello", 5), "hello");
    }

    #[test]
    fn test_truncate_start_with_ellipsis_truncated() {
        // "hello world" truncated from start with max 8
        // available = 7 (8 - 1 for ellipsis), "o world" = 7 chars fits
        assert_eq!(truncate_start_with_ellipsis("hello world", 8), "…o world");
    }

    #[test]
    fn test_truncate_start_with_ellipsis_very_short() {
        assert_eq!(truncate_start_with_ellipsis("hello", 1), "…");
    }

    #[test]
    fn test_truncate_start_with_ellipsis_wide_cjk() {
        // "test中文" = 4 + 4 = 8 columns
        let text = "test中文";
        // With max_width=6, available = 5, "t中文" = 1 + 4 = 5, fits
        let result = truncate_start_with_ellipsis(text, 6);
        assert_eq!(result, "…t中文");
    }

    #[test]
    fn test_ratatui_text_strips_vs16_only_for_rendering() {
        assert_eq!(ratatui_text("⚠️ Warn"), "⚠ Warn");
        assert_eq!(ratatui_text("👩‍🚀"), "👩‍🚀");
    }

    #[test]
    fn test_ratatui_width_matches_render_text() {
        assert_eq!(ratatui_width("⚠️⚠️"), ratatui_width("⚠⚠"));
        assert_eq!(ratatui_width("hello"), 5);
        assert_eq!(ratatui_width("✅"), 2);
    }

    #[test]
    fn test_truncate_with_ellipsis_preserves_graphemes() {
        for text in ["⚠️⚠️ab", "👩‍🚀👩‍🚀ab", "👍🏽👍🏽ab", "ééab"]
        {
            let truncated = truncate_with_ellipsis(text, 3);
            assert!(truncated.ends_with('…'));
            assert!(text.starts_with(truncated.trim_end_matches('…')));
        }

        assert_eq!(truncate_with_ellipsis("⚠️ab", 3), "⚠️ab");
        assert_eq!(truncate_with_ellipsis("⚠️ab", 2), "⚠️…");
    }

    #[test]
    fn test_truncate_start_with_ellipsis_mixed_width() {
        // Mix of narrow (1 col) and wide (2 col) characters
        let text = "a中b文c";
        // Width: 1 + 2 + 1 + 2 + 1 = 7 columns
        assert_eq!(truncate_start_with_ellipsis(text, 7), "a中b文c");
        // max=6, available=5: "b文c" = 1 + 2 + 1 = 4, fits
        assert_eq!(truncate_start_with_ellipsis(text, 6), "…b文c");
        // max=5, available=4: "b文c" = 1 + 2 + 1 = 4, fits
        assert_eq!(truncate_start_with_ellipsis(text, 5), "…b文c");
        // max=4, available=3: "文c" = 2 + 1 = 3, fits; "b文c" = 4, doesn't fit
        assert_eq!(truncate_start_with_ellipsis(text, 4), "…文c");
    }
}
