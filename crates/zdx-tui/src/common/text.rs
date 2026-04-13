//! Text utilities for TUI rendering.
//!
//! Shared text processing functions used across rendering paths.

use std::borrow::Cow;

use unicode_segmentation::UnicodeSegmentation;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

/// Calculates the terminal display width of a string, operating on grapheme
/// clusters for correct handling of multi-codepoint emoji sequences (ZWJ
/// sequences like 👩‍🚀, skin-tone modifiers, flag sequences, VS16).
///
/// `UnicodeWidthStr::width()` handles most sequences correctly, but terminals
/// render characters followed by U+FE0F (VS16) as 2-cell-wide emoji even when
/// `unicode-width` reports them as 1. This function corrects for that.
pub fn terminal_display_width(s: &str) -> usize {
    s.graphemes(true).map(grapheme_width).sum()
}

/// Returns the terminal display width of a single grapheme cluster.
///
/// For graphemes containing VS16 (U+FE0F) whose `UnicodeWidthStr` width is < 2,
/// we return 2 because terminals render VS16-qualified emoji at full emoji width.
fn grapheme_width(g: &str) -> usize {
    let w = g.width();
    // If the grapheme contains VS16 and unicode-width under-reports it, correct to 2.
    if w < 2 && g.contains('\u{FE0F}') {
        2
    } else {
        w
    }
}

/// Truncates a string to fit within `max_width` terminal columns, operating on
/// grapheme clusters. Appends `…` when truncated.
pub fn terminal_truncate(s: &str, max_width: usize) -> String {
    if terminal_display_width(s) <= max_width {
        return s.to_string();
    }
    if max_width <= 1 {
        return "…".to_string();
    }

    let target = max_width - 1; // reserve 1 column for ellipsis
    let mut result = String::new();
    let mut width = 0;

    for g in s.graphemes(true) {
        let gw = grapheme_width(g);
        if width + gw > target {
            break;
        }
        result.push_str(g);
        width += gw;
    }

    result.push('…');
    result
}

/// Truncates a string with ellipsis if it exceeds `max_width` (unicode-aware).
///
/// Uses unicode width for accurate terminal column calculation, handling
/// wide characters (CJK, emoji) correctly.
///
/// # Arguments
/// * `text` - The string to truncate
/// * `max_width` - Maximum display width in terminal columns
///
/// # Returns
/// The original string if it fits, or a truncated version ending with `…`
pub fn truncate_with_ellipsis(text: &str, max_width: usize) -> String {
    if text.width() <= max_width {
        return text.to_string();
    }
    if max_width <= 1 {
        return "…".to_string();
    }
    let mut truncated = String::new();
    for ch in text.chars() {
        let next_width = truncated.width() + ch.width().unwrap_or(0);
        if next_width + 1 > max_width {
            break;
        }
        truncated.push(ch);
    }
    truncated.push('…');
    truncated
}

/// Truncates a string from the start with ellipsis if it exceeds `max_width` (unicode-aware).
///
/// Shows the end of the string with `…` prefix when truncated.
/// Uses unicode width for accurate terminal column calculation.
///
/// # Arguments
/// * `text` - The string to truncate
/// * `max_width` - Maximum display width in terminal columns
///
/// # Returns
/// The original string if it fits, or a truncated version starting with `…`
pub fn truncate_start_with_ellipsis(text: &str, max_width: usize) -> String {
    if text.width() <= max_width {
        return text.to_string();
    }
    if max_width <= 1 {
        return "…".to_string();
    }

    // Collect chars with their widths from the end
    let chars: Vec<char> = text.chars().collect();
    let mut result_chars: Vec<char> = Vec::new();
    let mut current_width = 0;
    let available_width = max_width - 1; // Reserve 1 for ellipsis

    // Iterate from the end
    for &ch in chars.iter().rev() {
        let ch_width = ch.width().unwrap_or(0);
        if current_width + ch_width > available_width {
            break;
        }
        result_chars.push(ch);
        current_width += ch_width;
    }

    // Reverse to get correct order and prepend ellipsis
    result_chars.reverse();
    let mut result = String::from("…");
    for ch in result_chars {
        result.push(ch);
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
    fn test_terminal_display_width_plain() {
        assert_eq!(terminal_display_width("hello"), 5);
        assert_eq!(terminal_display_width(""), 0);
    }

    #[test]
    fn test_terminal_display_width_wide_emoji() {
        // ✅ (U+2705) is East Asian Width=W → 2 cells, no VS16 needed
        assert_eq!(terminal_display_width("✅"), 2);
    }

    #[test]
    fn test_terminal_display_width_vs16_emoji() {
        // ⚠️ = U+26A0 + U+FE0F: base char is narrow (1) but VS16 forces emoji (2)
        assert_eq!(terminal_display_width("⚠️"), 2);
        // Without VS16: just the base character
        assert_eq!(terminal_display_width("⚠"), 1);
    }

    #[test]
    fn test_terminal_display_width_mixed() {
        // "✅ Ship" = 2 + 1 + 4 = 7
        assert_eq!(terminal_display_width("✅ Ship"), 7);
        // "⚠️ Warn" = 2 + 1 + 4 = 7
        assert_eq!(terminal_display_width("⚠️ Warn"), 7);
    }

    #[test]
    fn test_terminal_display_width_zwj_emoji() {
        // ZWJ sequences should be treated as single grapheme clusters.
        // 👩‍🚀 = U+1F469 U+200D U+1F680 (woman astronaut)
        let astronaut = "👩\u{200D}🚀";
        let w = terminal_display_width(astronaut);
        // Most terminals render ZWJ emoji as 2 cells
        assert_eq!(w, 2);
    }

    #[test]
    fn test_terminal_truncate_grapheme_safe() {
        // Truncation must not split a grapheme cluster.
        // "⚠️ab" = 2 + 1 + 1 = 4 display width
        let s = "⚠️ab";
        assert_eq!(terminal_display_width(s), 4);
        // Truncate to 4 → fits entirely
        assert_eq!(terminal_truncate(s, 4), "⚠️ab");
        // Truncate to 3 → "⚠️" (2) + "…" (1) = 3
        assert_eq!(terminal_truncate(s, 3), "⚠️…");
        // Truncate to 2 → only "…" fits if the emoji (2) + ellipsis (1) > 2
        // target = 1, emoji width 2 > 1 → skip emoji, result = "…"
        // Actually "a" (1) fits target=1, but emoji comes first...
        // "⚠️" is the first grapheme (width 2), target=1, doesn't fit → "…"
        assert_eq!(terminal_truncate(s, 2), "…");
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
