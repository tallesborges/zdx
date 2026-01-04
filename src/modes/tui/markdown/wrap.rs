use unicode_width::UnicodeWidthStr;

use crate::modes::tui::transcript::{Style, StyledLine, StyledSpan};

/// Options for wrapping styled spans with hanging indents.
#[derive(Debug, Clone, Default)]
pub struct WrapOptions {
    /// Maximum display width for lines.
    pub width: usize,
    /// Prefix spans for the first line (e.g., "- " for list bullet).
    pub first_prefix: Vec<StyledSpan>,
    /// Prefix spans for continuation lines (e.g., "  " for alignment).
    pub rest_prefix: Vec<StyledSpan>,
}

impl WrapOptions {
    /// Creates wrap options with just a width (no prefixes).
    pub fn new(width: usize) -> Self {
        Self {
            width,
            first_prefix: vec![],
            rest_prefix: vec![],
        }
    }
}

/// Calculates the display width of a slice of styled spans.
fn spans_display_width(spans: &[StyledSpan]) -> usize {
    spans.iter().map(|s| s.text.width()).sum()
}

/// Context for wrapping operations, reducing argument count for helper functions.
struct WrapContext<'a> {
    /// Completed lines.
    lines: Vec<StyledLine>,
    /// Spans for current line being built.
    current_line_spans: Vec<StyledSpan>,
    /// Display width of current line content.
    current_line_width: usize,
    /// Whether we're on the first line.
    is_first_line: bool,
    /// Available width for continuation lines.
    rest_width: usize,
    /// Prefix for first line.
    first_prefix: &'a [StyledSpan],
    /// Prefix for continuation lines.
    rest_prefix: &'a [StyledSpan],
}

impl<'a> WrapContext<'a> {
    fn new(opts: &'a WrapOptions, content_width_rest: usize) -> Self {
        Self {
            lines: Vec::new(),
            current_line_spans: Vec::new(),
            current_line_width: 0,
            is_first_line: true,
            rest_width: content_width_rest,
            first_prefix: &opts.first_prefix,
            rest_prefix: &opts.rest_prefix,
        }
    }

    /// Flush current line to lines vec.
    fn flush_line(&mut self) {
        let prefix = if self.is_first_line {
            self.first_prefix.to_vec()
        } else {
            self.rest_prefix.to_vec()
        };

        let mut final_spans = prefix;
        final_spans.append(&mut self.current_line_spans);
        self.lines.push(StyledLine { spans: final_spans });
        self.is_first_line = false;
    }

    /// Get current available width based on line position.
    fn current_avail(&self, first_line_width: usize) -> usize {
        if self.is_first_line {
            first_line_width
        } else {
            self.rest_width
        }
    }
}

/// Breaks a styled span into character-by-width fragments.
///
/// Used for inline code or when word boundaries aren't available.
fn break_span_by_width(span: &StyledSpan, max_width: usize) -> Vec<StyledSpan> {
    use unicode_width::UnicodeWidthChar;

    let mut parts = Vec::new();
    let mut current = String::new();
    let mut current_width: usize = 0;

    for ch in span.text.chars() {
        let ch_width = ch.width().unwrap_or(0);

        // Zero-width characters always stay with current fragment
        if ch_width == 0 {
            current.push(ch);
            continue;
        }

        // Check if adding this character would exceed width
        if current_width + ch_width > max_width && !current.is_empty() {
            parts.push(StyledSpan {
                text: std::mem::take(&mut current),
                style: span.style,
            });
            current_width = 0;
        }

        current.push(ch);
        current_width += ch_width;
    }

    if !current.is_empty() {
        parts.push(StyledSpan {
            text: current,
            style: span.style,
        });
    }

    if parts.is_empty() {
        parts.push(StyledSpan {
            text: String::new(),
            style: span.style,
        });
    }

    parts
}

/// Wraps styled spans while preserving styles across line breaks.
///
/// This is the critical function that enables markdown rendering:
/// - Wraps at word boundaries for normal text
/// - Preserves whitespace for inline code
/// - Handles hanging indents via `WrapOptions`
/// - Maintains style information across line breaks
pub fn wrap_styled_spans(spans: &[StyledSpan], opts: &WrapOptions) -> Vec<StyledLine> {
    if opts.width == 0 || spans.is_empty() {
        // Degenerate case: just return spans as single line
        let mut all_spans = opts.first_prefix.clone();
        all_spans.extend(spans.iter().cloned());
        return vec![StyledLine { spans: all_spans }];
    }

    // Calculate prefix widths
    let first_prefix_width = spans_display_width(&opts.first_prefix);
    let rest_prefix_width = spans_display_width(&opts.rest_prefix);

    // Available content width depends on which line we're on
    let content_width_first = opts.width.saturating_sub(first_prefix_width);
    let content_width_rest = opts.width.saturating_sub(rest_prefix_width);

    let mut ctx = WrapContext::new(opts, content_width_rest);

    for span in spans {
        // Check for hard breaks (newlines in span text)
        if span.text.contains('\n') {
            for (i, part) in span.text.split('\n').enumerate() {
                if i > 0 {
                    // Flush current line on newline
                    ctx.flush_line();
                    ctx.current_line_width = 0;
                }

                if !part.is_empty() {
                    let part_span = StyledSpan {
                        text: part.to_string(),
                        style: span.style,
                    };
                    process_span_impl(&part_span, &mut ctx, content_width_first);
                }
            }
            continue;
        }

        process_span_impl(span, &mut ctx, content_width_first);
    }

    // Flush remaining content
    if !ctx.current_line_spans.is_empty() {
        ctx.flush_line();
    }

    // Ensure at least one line with prefix
    if ctx.lines.is_empty() {
        ctx.lines.push(StyledLine {
            spans: opts.first_prefix.clone(),
        });
    }

    ctx.lines
}

/// Process a single span, handling word wrapping.
fn process_span_impl(span: &StyledSpan, ctx: &mut WrapContext, first_line_width: usize) {
    let is_code = matches!(span.style, Style::CodeInline | Style::CodeBlock);

    if is_code {
        process_code_span_impl(span, ctx, first_line_width);
    } else {
        process_text_span_impl(span, ctx, first_line_width);
    }
}

/// Process code span (preserve whitespace, break by character).
fn process_code_span_impl(span: &StyledSpan, ctx: &mut WrapContext, first_line_width: usize) {
    let span_width = span.text.width();
    let available_width = ctx.current_avail(first_line_width);

    if ctx.current_line_width + span_width <= available_width {
        // Fits on current line
        ctx.current_line_spans.push(span.clone());
        ctx.current_line_width += span_width;
    } else if span_width <= ctx.rest_width && ctx.current_line_width > 0 {
        // Doesn't fit but would fit on fresh line
        ctx.flush_line();
        ctx.current_line_width = 0;
        ctx.current_line_spans.push(span.clone());
        ctx.current_line_width = span_width;
    } else {
        // Need to break the span
        let remaining_width = available_width.saturating_sub(ctx.current_line_width);
        let fragments = break_span_by_width(span, remaining_width.max(1));

        for (i, frag) in fragments.into_iter().enumerate() {
            let frag_width = frag.text.width();
            let current_avail = ctx.current_avail(first_line_width);

            if i > 0 && ctx.current_line_width + frag_width > current_avail {
                ctx.flush_line();
                ctx.current_line_width = 0;
            }

            if !frag.text.is_empty() {
                ctx.current_line_spans.push(frag.clone());
                ctx.current_line_width += frag_width;
            }
        }
    }
}

/// Process normal text span (word boundaries, collapse whitespace).
fn process_text_span_impl(span: &StyledSpan, ctx: &mut WrapContext, first_line_width: usize) {
    // Check for leading/trailing whitespace before splitting
    let has_leading_space = span.text.starts_with(|c: char| c.is_whitespace());
    let has_trailing_space = span.text.ends_with(|c: char| c.is_whitespace());

    // Split into words (this collapses whitespace)
    let words: Vec<&str> = span.text.split_whitespace().collect();

    if words.is_empty() {
        // Only whitespace - add a single space if we have content
        if !ctx.current_line_spans.is_empty() {
            let space_span = StyledSpan {
                text: " ".to_string(),
                style: span.style,
            };
            let current_avail = ctx.current_avail(first_line_width);
            if ctx.current_line_width < current_avail {
                ctx.current_line_spans.push(space_span);
                ctx.current_line_width += 1;
            }
        }
        return;
    }

    // Add leading space if original text had it and we have prior content
    if has_leading_space && !ctx.current_line_spans.is_empty() {
        let current_avail = ctx.current_avail(first_line_width);
        if ctx.current_line_width < current_avail {
            ctx.current_line_spans.push(StyledSpan {
                text: " ".to_string(),
                style: span.style,
            });
            ctx.current_line_width += 1;
        }
    }

    for (i, word) in words.iter().enumerate() {
        let word_width = word.width();
        let current_avail = ctx.current_avail(first_line_width);

        // Add space before word (except first word - leading space handled above)
        if i > 0 {
            // Check if space + word fits
            if ctx.current_line_width + 1 + word_width <= current_avail {
                ctx.current_line_spans.push(StyledSpan {
                    text: " ".to_string(),
                    style: span.style,
                });
                ctx.current_line_width += 1;
            } else {
                // Word doesn't fit, start new line
                ctx.flush_line();
                ctx.current_line_width = 0;
            }
        }

        let current_avail = ctx.current_avail(first_line_width);

        // Now add the word
        if word_width <= current_avail.saturating_sub(ctx.current_line_width) {
            ctx.current_line_spans.push(StyledSpan {
                text: (*word).to_string(),
                style: span.style,
            });
            ctx.current_line_width += word_width;
        } else if word_width <= ctx.rest_width && ctx.current_line_width > 0 {
            // Word fits on fresh line
            ctx.flush_line();
            ctx.current_line_width = 0;
            ctx.current_line_spans.push(StyledSpan {
                text: (*word).to_string(),
                style: span.style,
            });
            ctx.current_line_width = word_width;
        } else {
            // Word is too long, need to break it
            if ctx.current_line_width > 0 {
                ctx.flush_line();
                ctx.current_line_width = 0;
            }

            let word_span = StyledSpan {
                text: (*word).to_string(),
                style: span.style,
            };
            let break_width = ctx.current_avail(first_line_width);
            let fragments = break_span_by_width(&word_span, break_width);

            for frag in fragments {
                let frag_width = frag.text.width();
                let current_avail = ctx.current_avail(first_line_width);
                if ctx.current_line_width + frag_width > current_avail && ctx.current_line_width > 0
                {
                    ctx.flush_line();
                    ctx.current_line_width = 0;
                }
                if !frag.text.is_empty() {
                    ctx.current_line_spans.push(frag);
                    ctx.current_line_width += frag_width;
                }
            }
        }
    }

    // Add trailing space if original text had it
    if has_trailing_space {
        let current_avail = ctx.current_avail(first_line_width);
        if ctx.current_line_width < current_avail {
            ctx.current_line_spans.push(StyledSpan {
                text: " ".to_string(),
                style: span.style,
            });
            ctx.current_line_width += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_wrap_styled_spans_basic() {
        let spans = vec![StyledSpan {
            text: "hello world".to_string(),
            style: Style::Assistant,
        }];
        let opts = WrapOptions::new(20);
        let lines = wrap_styled_spans(&spans, &opts);

        assert_eq!(lines.len(), 1);
        // Text is split into words but style is preserved
        let combined: String = lines[0].spans.iter().map(|s| s.text.as_str()).collect();
        assert_eq!(combined, "hello world");
        // All spans should have same style
        assert!(lines[0].spans.iter().all(|s| s.style == Style::Assistant));
    }

    #[test]
    fn test_wrap_styled_spans_split() {
        let spans = vec![StyledSpan {
            text: "hello world".to_string(),
            style: Style::Assistant,
        }];
        let opts = WrapOptions::new(8);
        let lines = wrap_styled_spans(&spans, &opts);

        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].spans[0].text, "hello");
        assert_eq!(lines[1].spans[0].text, "world");
    }

    #[test]
    fn test_wrap_styled_spans_mid_span_break() {
        let spans = vec![
            StyledSpan {
                text: "hello ".to_string(),
                style: Style::Assistant,
            },
            StyledSpan {
                text: "world".to_string(),
                style: Style::Strong,
            },
        ];
        let opts = WrapOptions::new(8);
        let lines = wrap_styled_spans(&spans, &opts);

        // "hello" fits on first line, "world" on second
        assert_eq!(lines.len(), 2);
        // First line should have "hello"
        // Second line should have "world" with Strong style preserved
        let last_line = &lines[1];
        assert!(last_line.spans.iter().any(|s| s.style == Style::Strong));
    }

    #[test]
    fn test_wrap_styled_spans_inline_code_whitespace() {
        // Inline code should preserve spaces
        let spans = vec![StyledSpan {
            text: "foo  bar".to_string(), // double space
            style: Style::CodeInline,
        }];
        let opts = WrapOptions::new(20);
        let lines = wrap_styled_spans(&spans, &opts);

        // Should preserve the double space
        assert_eq!(lines[0].spans[0].text, "foo  bar");
    }

    #[test]
    fn test_wrap_styled_spans_hard_break() {
        let spans = vec![StyledSpan {
            text: "line1\nline2".to_string(),
            style: Style::Assistant,
        }];
        let opts = WrapOptions::new(20);
        let lines = wrap_styled_spans(&spans, &opts);

        assert_eq!(lines.len(), 2);
    }

    #[test]
    fn test_wrap_styled_spans_hanging_indent() {
        let spans = vec![StyledSpan {
            text: "this is a longer text that should wrap".to_string(),
            style: Style::Assistant,
        }];
        let opts = WrapOptions {
            width: 20,
            first_prefix: vec![StyledSpan {
                text: "• ".to_string(),
                style: Style::ListBullet,
            }],
            rest_prefix: vec![StyledSpan {
                text: "  ".to_string(),
                style: Style::Plain,
            }],
        };
        let lines = wrap_styled_spans(&spans, &opts);

        // First line should start with bullet
        assert_eq!(lines[0].spans[0].text, "• ");
        // Continuation lines should have indent
        if lines.len() > 1 {
            assert_eq!(lines[1].spans[0].text, "  ");
        }
    }
}
