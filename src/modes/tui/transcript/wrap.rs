use std::cell::RefCell;
use std::collections::HashMap;

use unicode_width::UnicodeWidthStr;

use super::cell::CellId;
use super::style::{Style, StyledLine, StyledSpan};

/// Cache for wrapped lines to avoid re-computing on every frame.
///
/// Keyed by `(CellId, width, content_len)` where `content_len` helps
/// invalidate entries when streaming content changes.
///
/// Uses interior mutability (`RefCell`) to allow caching during immutable
/// render passes.
#[derive(Debug, Default)]
pub struct WrapCache {
    /// Maps (cell_id, width, content_len) -> cached styled lines
    cache: RefCell<HashMap<(CellId, usize, usize), Vec<StyledLine>>>,
}

impl WrapCache {
    /// Creates a new empty cache.
    pub fn new() -> Self {
        Self {
            cache: RefCell::new(HashMap::new()),
        }
    }

    /// Clears all cached entries.
    ///
    /// Call this on terminal resize to invalidate width-dependent caches.
    pub fn clear(&self) {
        self.cache.borrow_mut().clear();
    }

    /// Gets cached lines for a cell, cloning if present.
    pub(crate) fn get(
        &self,
        cell_id: CellId,
        width: usize,
        content_len: usize,
    ) -> Option<Vec<StyledLine>> {
        self.cache
            .borrow()
            .get(&(cell_id, width, content_len))
            .cloned()
    }

    /// Stores wrapped lines in the cache.
    pub(crate) fn insert(
        &self,
        cell_id: CellId,
        width: usize,
        content_len: usize,
        lines: Vec<StyledLine>,
    ) {
        self.cache
            .borrow_mut()
            .insert((cell_id, width, content_len), lines);
    }

    /// Returns true if the cache is empty (test-only).
    #[cfg(test)]
    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.cache.borrow().is_empty()
    }
}

/// Renders content with a prefix, handling line wrapping.
///
/// The prefix appears on the first line; subsequent wrapped lines
/// are indented to align with the content start (or repeat the prefix
/// if `repeat_prefix` is true).
pub(crate) fn render_prefixed_content(
    prefix: &str,
    content: &str,
    width: usize,
    prefix_style: Style,
    content_style: Style,
    repeat_prefix: bool,
) -> Vec<StyledLine> {
    let mut lines = Vec::new();
    // Use display width for prefix
    let prefix_display_width = prefix.width();

    // Minimum usable width
    let min_width = prefix_display_width + 10;
    let effective_width = width.max(min_width);

    // Content width after prefix/indent
    let content_width = effective_width.saturating_sub(prefix_display_width);

    // Split content into paragraphs (preserve blank lines)
    let paragraphs: Vec<&str> = content.split('\n').collect();

    for paragraph in paragraphs {
        if paragraph.is_empty() {
            // Empty paragraph = blank line
            // Use prefix or indentation based on repeat_prefix and whether it's the first line
            let line_prefix = if repeat_prefix || lines.is_empty() {
                StyledSpan {
                    text: prefix.to_string(),
                    style: prefix_style,
                }
            } else {
                StyledSpan {
                    text: " ".repeat(prefix_display_width),
                    style: Style::Plain,
                }
            };
            lines.push(StyledLine {
                spans: vec![line_prefix],
            });
            continue;
        }

        // Wrap the paragraph
        let wrapped = wrap_text(paragraph, content_width);

        for wrapped_line in wrapped {
            let mut spans = Vec::new();

            if repeat_prefix {
                // Repeat the styled prefix on every line
                spans.push(StyledSpan {
                    text: prefix.to_string(),
                    style: prefix_style,
                });
            } else if lines.is_empty() {
                // First line gets the prefix
                spans.push(StyledSpan {
                    text: prefix.to_string(),
                    style: prefix_style,
                });
            } else {
                // Indent continuation lines (use display width for proper alignment)
                spans.push(StyledSpan {
                    text: " ".repeat(prefix_display_width),
                    style: Style::Plain,
                });
            }

            spans.push(StyledSpan {
                text: wrapped_line,
                style: content_style,
            });

            lines.push(StyledLine { spans });
        }
    }

    // Handle empty content
    if lines.is_empty() {
        lines.push(StyledLine {
            spans: vec![StyledSpan {
                text: prefix.to_string(),
                style: prefix_style,
            }],
        });
    }

    lines
}

/// Wraps text to fit within the given display width.
///
/// Uses unicode display width for proper handling of:
/// - CJK characters (double-width)
/// - Emoji
/// - Zero-width characters
///
/// Does not handle hyphenation.
pub(crate) fn wrap_text(text: &str, width: usize) -> Vec<String> {
    if width == 0 {
        return vec![text.to_string()];
    }

    let mut lines = Vec::new();
    let mut current_line = String::new();
    let mut current_width: usize = 0;

    for word in text.split_whitespace() {
        let word_width = word.width();

        if current_line.is_empty() {
            // First word on line
            if word_width > width {
                // Word is too long, force break by character
                let mut broken = wrap_chars(word, width);
                if let Some(last) = broken.pop() {
                    // All but last go to completed lines
                    lines.extend(broken);
                    // Last part becomes current line
                    current_width = last.width();
                    current_line = last;
                }
            } else {
                current_line = word.to_string();
                current_width = word_width;
            }
        } else if current_width + 1 + word_width <= width {
            // Word fits on current line (+ 1 for space)
            current_line.push(' ');
            current_line.push_str(word);
            current_width += 1 + word_width;
        } else {
            // Start new line
            lines.push(std::mem::take(&mut current_line));
            if word_width > width {
                // Word is too long, force break by character
                let mut broken = wrap_chars(word, width);
                if let Some(last) = broken.pop() {
                    lines.extend(broken);
                    current_width = last.width();
                    current_line = last;
                }
            } else {
                current_line = word.to_string();
                current_width = word_width;
            }
        }
    }

    if !current_line.is_empty() {
        lines.push(current_line);
    }

    // Handle empty input
    if lines.is_empty() {
        lines.push(String::new());
    }

    lines
}

/// Breaks a string into parts that fit within the given display width.
///
/// Used for "hard wrapping" (e.g., code blocks, long words, tool output)
/// where whitespace preservation and exact width are more important than
/// word boundaries.
///
/// Breaks at character boundaries, respecting display width.
///
/// Note: Callers should expand tabs to spaces before calling this function.
/// Tab characters have variable terminal width (to next tab stop), but
/// `unicode_width` returns `None` (0) for them. Pre-expanding ensures
/// consistent width calculation.
pub(crate) fn wrap_chars(text: &str, width: usize) -> Vec<String> {
    use unicode_width::UnicodeWidthChar;

    let mut parts = Vec::new();
    let mut current = String::new();
    let mut current_width: usize = 0;

    for ch in text.chars() {
        let ch_width = ch.width().unwrap_or(0);

        // Handle zero-width characters (always add to current)
        if ch_width == 0 {
            current.push(ch);
            continue;
        }

        // Check if adding this character would exceed width
        if current_width + ch_width > width && !current.is_empty() {
            parts.push(current);
            current = String::new();
            current_width = 0;
        }

        current.push(ch);
        current_width += ch_width;
    }

    if !current.is_empty() {
        parts.push(current);
    }

    // Ensure we return at least one empty part for empty input
    if parts.is_empty() {
        parts.push(String::new());
    }

    parts
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_wrap_text_basic() {
        let wrapped = wrap_text("hello world", 20);
        assert_eq!(wrapped, vec!["hello world"]);
    }

    #[test]
    fn test_wrap_text_split() {
        let wrapped = wrap_text("hello world", 8);
        assert_eq!(wrapped, vec!["hello", "world"]);
    }

    #[test]
    fn test_wrap_text_long_word() {
        let wrapped = wrap_text("supercalifragilistic", 10);
        assert_eq!(wrapped, vec!["supercalif", "ragilistic"]);
    }

    // ========================================================================
    // Unicode width tests (Phase 2a)
    // ========================================================================

    #[test]
    fn test_wrap_text_cjk_double_width() {
        // CJK characters are double-width
        // "你好世界" = 4 characters, 8 display columns
        let wrapped = wrap_text("你好世界", 6);
        // Should wrap after 3 CJK chars (6 columns), leaving 1 char on second line
        assert_eq!(wrapped.len(), 2);
        assert_eq!(wrapped[0], "你好世");
        assert_eq!(wrapped[1], "界");
    }

    #[test]
    fn test_wrap_text_emoji() {
        // Emoji are typically double-width
        // "🎉🎊🎁" = 3 emoji, 6 display columns
        let wrapped = wrap_text("🎉🎊🎁", 4);
        // Should wrap after 2 emoji (4 columns)
        assert_eq!(wrapped.len(), 2);
        assert_eq!(wrapped[0], "🎉🎊");
        assert_eq!(wrapped[1], "🎁");
    }

    #[test]
    fn test_wrap_text_mixed_ascii_cjk() {
        // Mix of ASCII (1-width) and CJK (2-width)
        // "Hi你好" = 2 + 4 = 6 display columns
        let wrapped = wrap_text("Hi你好", 5);
        // "Hi你" = 4 columns, "好" = 2 columns
        assert_eq!(wrapped.len(), 2);
        assert_eq!(wrapped[0], "Hi你");
        assert_eq!(wrapped[1], "好");
    }

    #[test]
    fn test_wrap_text_preserves_words_with_unicode() {
        // Word wrapping should work with unicode
        let wrapped = wrap_text("Hello 你好 World", 10);
        // "Hello" (5) fits, "你好" (4) fits, "World" (5) fits
        assert_eq!(wrapped.len(), 2);
        assert_eq!(wrapped[0], "Hello 你好");
        assert_eq!(wrapped[1], "World");
    }

    #[test]
    fn test_wrap_chars_cjk() {
        // Breaking a long CJK word
        let parts = wrap_chars("你好世界很长", 4);
        // Each part should be at most 4 columns (2 CJK chars)
        assert_eq!(parts.len(), 3);
        assert_eq!(parts[0], "你好");
        assert_eq!(parts[1], "世界");
        assert_eq!(parts[2], "很长");
    }

    #[test]
    fn test_wrap_chars_emoji() {
        let parts = wrap_chars("🎉🎊🎁🎄", 4);
        // Each emoji is 2 columns, so 2 per line
        assert_eq!(parts.len(), 2);
        assert_eq!(parts[0], "🎉🎊");
        assert_eq!(parts[1], "🎁🎄");
    }
}
