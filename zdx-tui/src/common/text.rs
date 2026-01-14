//! Text utilities for TUI rendering.
//!
//! Shared text processing functions used across rendering paths.

use std::borrow::Cow;

use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

/// Truncates a string with ellipsis if it exceeds max_width (unicode-aware).
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
/// OpenAI's Codex CLI approach. This is a pragmatic "good enough" solution
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
}
