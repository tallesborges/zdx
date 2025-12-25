//! Markdown parsing and rendering for assistant responses.
//!
//! This module provides:
//! - `render_markdown()`: Parse markdown text into styled lines
//! - `wrap_styled_spans()`: Wrap styled spans while preserving styles across line breaks
//!
//! Uses pulldown-cmark for parsing. Falls back to plain text if parsing fails.

use pulldown_cmark::{CodeBlockKind, Event, HeadingLevel, Parser, Tag, TagEnd};
use unicode_width::UnicodeWidthStr;

use super::transcript::{Style, StyledLine, StyledSpan};

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

    /// Creates wrap options with hanging indent for list items.
    #[allow(dead_code)]
    pub fn with_list_indent(width: usize, bullet: &str, indent: usize) -> Self {
        Self {
            width,
            first_prefix: vec![StyledSpan {
                text: bullet.to_string(),
                style: Style::ListBullet,
            }],
            rest_prefix: vec![StyledSpan {
                text: " ".repeat(indent),
                style: Style::Plain,
            }],
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

/// Renders markdown text into styled lines.
///
/// This is the main entry point for markdown rendering:
/// - Parses markdown using pulldown-cmark
/// - Converts events to styled spans
/// - Wraps at the given width
///
/// Falls back to plain text rendering if parsing fails.
pub fn render_markdown(text: &str, width: usize) -> Vec<StyledLine> {
    if text.is_empty() {
        return vec![StyledLine { spans: vec![] }];
    }

    let parser = Parser::new(text);
    let mut renderer = MarkdownRenderer::new(width);

    for event in parser {
        renderer.process_event(event);
    }

    renderer.finish()
}

/// Internal state for markdown rendering.
struct MarkdownRenderer {
    width: usize,
    lines: Vec<StyledLine>,
    /// Current paragraph/block spans being collected.
    current_spans: Vec<StyledSpan>,
    /// Style stack for nested inline styles.
    style_stack: Vec<Style>,
    /// Are we inside a code block?
    in_code_block: bool,
    /// Language identifier for current code block (e.g., "rust", "python").
    code_block_lang: Option<String>,
    /// Current list nesting and state.
    list_stack: Vec<ListState>,
    /// Are we inside a blockquote?
    in_blockquote: bool,
    /// Current heading level (None if not in heading).
    current_heading: Option<HeadingLevel>,
}

#[derive(Debug, Clone)]
struct ListState {
    /// None for unordered, Some(n) for ordered starting at n.
    ordered: Option<u64>,
    /// Current item number (for ordered lists).
    current_item: u64,
}

impl MarkdownRenderer {
    fn new(width: usize) -> Self {
        Self {
            width,
            lines: Vec::new(),
            current_spans: Vec::new(),
            style_stack: vec![Style::Assistant],
            in_code_block: false,
            code_block_lang: None,
            list_stack: Vec::new(),
            in_blockquote: false,
            current_heading: None,
        }
    }

    fn current_style(&self) -> Style {
        self.style_stack.last().copied().unwrap_or(Style::Assistant)
    }

    fn push_style(&mut self, style: Style) {
        self.style_stack.push(style);
    }

    fn pop_style(&mut self) {
        if self.style_stack.len() > 1 {
            self.style_stack.pop();
        }
    }

    fn process_event(&mut self, event: Event) {
        match event {
            Event::Start(tag) => self.start_tag(tag),
            Event::End(tag) => self.end_tag(tag),
            Event::Text(text) => self.add_text(&text),
            Event::Code(code) => self.add_inline_code(&code),
            Event::SoftBreak => self.add_soft_break(),
            Event::HardBreak => self.add_hard_break(),
            Event::Html(_) => {
                // Skip HTML to avoid terminal injection
            }
            Event::InlineHtml(_) => {
                // Skip inline HTML
            }
            Event::FootnoteReference(_) => {
                // Skip footnotes for now
            }
            Event::TaskListMarker(checked) => {
                // Render task list marker
                let marker = if checked { "[x] " } else { "[ ] " };
                self.current_spans.push(StyledSpan {
                    text: marker.to_string(),
                    style: Style::ListBullet,
                });
            }
            Event::Rule => {
                // Horizontal rule - flush and add separator
                self.flush_paragraph();
                self.lines.push(StyledLine {
                    spans: vec![StyledSpan {
                        text: "─".repeat(self.width.min(40)),
                        style: Style::Plain,
                    }],
                });
            }
            Event::InlineMath(_) | Event::DisplayMath(_) => {
                // Math not supported yet, render as-is
            }
        }
    }

    fn start_tag(&mut self, tag: Tag) {
        match tag {
            Tag::Paragraph => {
                // Paragraphs are implicit containers
            }
            Tag::Heading { level, .. } => {
                self.current_heading = Some(level);
                let style = match level {
                    HeadingLevel::H1 => Style::H1,
                    HeadingLevel::H2 => Style::H2,
                    _ => Style::H3,
                };
                self.push_style(style);
            }
            Tag::CodeBlock(kind) => {
                self.flush_paragraph();
                self.in_code_block = true;
                // Extract language from fenced code blocks
                self.code_block_lang = match kind {
                    CodeBlockKind::Fenced(lang) if !lang.is_empty() => Some(lang.to_string()),
                    _ => None,
                };
                self.push_style(Style::CodeBlock);
            }
            Tag::List(start) => {
                self.flush_paragraph();
                self.list_stack.push(ListState {
                    ordered: start,
                    current_item: start.unwrap_or(1),
                });
            }
            Tag::Item => {
                self.flush_paragraph();
            }
            Tag::BlockQuote(_) => {
                self.flush_paragraph();
                self.in_blockquote = true;
                self.push_style(Style::BlockQuote);
            }
            Tag::Emphasis => {
                self.push_style(Style::Emphasis);
            }
            Tag::Strong => {
                self.push_style(Style::Strong);
            }
            Tag::Strikethrough => {
                // Use plain for strikethrough (terminal support varies)
                self.push_style(Style::Plain);
            }
            Tag::Link { .. } => {
                self.push_style(Style::Link);
                // TODO: Store URL for later to show after link text
            }
            Tag::Image { .. } => {
                // Images not supported in terminal
            }
            Tag::Table(_) | Tag::TableHead | Tag::TableRow | Tag::TableCell => {
                // Tables not implemented yet
            }
            Tag::FootnoteDefinition(_) => {
                // Footnotes not supported
            }
            Tag::MetadataBlock(_) => {
                // Metadata not relevant for display
            }
            Tag::HtmlBlock => {
                // Not implemented
            }
            Tag::DefinitionList | Tag::DefinitionListTitle | Tag::DefinitionListDefinition => {
                // Definition lists not implemented yet
            }
            Tag::Superscript => {
                // Render superscript as plain text (terminal support limited)
                self.push_style(Style::Plain);
            }
            Tag::Subscript => {
                // Render subscript as plain text (terminal support limited)
                self.push_style(Style::Plain);
            }
        }
    }

    fn end_tag(&mut self, tag: TagEnd) {
        match tag {
            TagEnd::Paragraph => {
                self.flush_paragraph();
                // Add blank line after paragraph, but not inside list items
                if self.list_stack.is_empty() {
                    self.lines.push(StyledLine::empty());
                }
            }
            TagEnd::Heading(_) => {
                self.flush_paragraph();
                self.pop_style();
                self.current_heading = None;
                // Add blank line after heading
                self.lines.push(StyledLine::empty());
            }
            TagEnd::CodeBlock => {
                self.flush_code_block();
                self.in_code_block = false;
                self.pop_style();
                // Add blank line after code block
                self.lines.push(StyledLine::empty());
            }
            TagEnd::List(_) => {
                self.list_stack.pop();
                if self.list_stack.is_empty() {
                    // Add blank line after top-level list
                    self.lines.push(StyledLine::empty());
                }
            }
            TagEnd::Item => {
                self.flush_list_item();
                // Increment item counter for ordered lists
                if let Some(list) = self.list_stack.last_mut() {
                    list.current_item += 1;
                }
            }
            TagEnd::BlockQuote(_) => {
                self.flush_paragraph();
                self.in_blockquote = false;
                self.pop_style();
            }
            TagEnd::Emphasis | TagEnd::Strong | TagEnd::Strikethrough | TagEnd::Link => {
                self.pop_style();
            }
            _ => {}
        }
    }

    fn add_text(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }

        let style = self.current_style();

        if self.in_code_block {
            // Code blocks: preserve exact text including newlines
            self.current_spans.push(StyledSpan {
                text: text.to_string(),
                style,
            });
        } else {
            // Normal text
            self.current_spans.push(StyledSpan {
                text: text.to_string(),
                style,
            });
        }
    }

    fn add_inline_code(&mut self, code: &str) {
        self.current_spans.push(StyledSpan {
            text: code.to_string(),
            style: Style::CodeInline,
        });
    }

    fn add_soft_break(&mut self) {
        // Soft break becomes a space
        self.current_spans.push(StyledSpan {
            text: " ".to_string(),
            style: self.current_style(),
        });
    }

    fn add_hard_break(&mut self) {
        // Hard break forces a new line within the current block
        self.current_spans.push(StyledSpan {
            text: "\n".to_string(),
            style: self.current_style(),
        });
    }

    fn flush_paragraph(&mut self) {
        if self.current_spans.is_empty() {
            return;
        }

        let spans = std::mem::take(&mut self.current_spans);
        let opts = WrapOptions::new(self.width);
        let wrapped = wrap_styled_spans(&spans, &opts);
        self.lines.extend(wrapped);
    }

    fn flush_code_block(&mut self) {
        if self.current_spans.is_empty() {
            return;
        }

        // Code blocks: emit each line as-is without wrapping
        let spans = std::mem::take(&mut self.current_spans);

        // Collect all text and split by newlines
        let full_text: String = spans.iter().map(|s| s.text.as_str()).collect();

        // Opening fence with optional language (subtle)
        let fence_text = match &self.code_block_lang {
            Some(lang) => format!("```{}", lang),
            None => "```".to_string(),
        };
        self.lines.push(StyledLine {
            spans: vec![StyledSpan {
                text: fence_text,
                style: Style::CodeFence,
            }],
        });

        // Trim trailing newline to avoid empty line before closing fence
        let trimmed = full_text.trim_end_matches('\n');

        for line in trimmed.split('\n') {
            // Add indent for visual separation
            self.lines.push(StyledLine {
                spans: vec![
                    StyledSpan {
                        text: "  ".to_string(),
                        style: Style::Plain,
                    },
                    StyledSpan {
                        text: line.to_string(),
                        style: Style::CodeBlock,
                    },
                ],
            });
        }

        // Closing fence (subtle)
        self.lines.push(StyledLine {
            spans: vec![StyledSpan {
                text: "```".to_string(),
                style: Style::CodeFence,
            }],
        });

        // Clear the language for next code block
        self.code_block_lang = None;
    }

    fn flush_list_item(&mut self) {
        if self.current_spans.is_empty() {
            return;
        }

        let spans = std::mem::take(&mut self.current_spans);

        // Determine list marker and indentation
        let (marker, marker_style) = if let Some(list) = self.list_stack.last() {
            if list.ordered.is_some() {
                (format!("{}. ", list.current_item), Style::ListNumber)
            } else {
                ("• ".to_string(), Style::ListBullet)
            }
        } else {
            ("• ".to_string(), Style::ListBullet)
        };

        let indent_level = self.list_stack.len().saturating_sub(1);
        let base_indent = "  ".repeat(indent_level);
        let marker_width = marker.width();

        let opts = WrapOptions {
            width: self.width,
            first_prefix: vec![
                StyledSpan {
                    text: base_indent.clone(),
                    style: Style::Plain,
                },
                StyledSpan {
                    text: marker,
                    style: marker_style,
                },
            ],
            rest_prefix: vec![StyledSpan {
                text: format!("{}{}", base_indent, " ".repeat(marker_width)),
                style: Style::Plain,
            }],
        };

        let wrapped = wrap_styled_spans(&spans, &opts);
        self.lines.extend(wrapped);
    }

    fn finish(mut self) -> Vec<StyledLine> {
        // Flush any remaining content
        if !self.current_spans.is_empty() {
            if self.in_code_block {
                self.flush_code_block();
            } else {
                self.flush_paragraph();
            }
        }

        // Remove trailing empty lines
        while self.lines.last().is_some_and(|l| l.spans.is_empty()) {
            self.lines.pop();
        }

        if self.lines.is_empty() {
            self.lines.push(StyledLine { spans: vec![] });
        }

        self.lines
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ========================================================================
    // wrap_styled_spans tests
    // ========================================================================

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

    // ========================================================================
    // render_markdown tests
    // ========================================================================

    #[test]
    fn test_inline_code() {
        let lines = render_markdown("Use `code` here", 80);

        // Should have CodeInline style
        let has_code_inline = lines
            .iter()
            .any(|l| l.spans.iter().any(|s| s.style == Style::CodeInline));
        assert!(has_code_inline);
    }

    #[test]
    fn test_inline_code_preserves_surrounding_spaces() {
        let lines = render_markdown("word `code` word", 80);

        // Combine all text
        let combined: String = lines
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| s.text.as_str()))
            .collect();

        // Should have spaces around the code
        assert!(
            combined.contains("word ") && combined.contains(" word"),
            "Expected spaces around inline code, got: {:?}",
            combined
        );
    }

    #[test]
    fn test_bold_italic() {
        let lines = render_markdown("**bold** and *italic*", 80);

        let has_strong = lines
            .iter()
            .any(|l| l.spans.iter().any(|s| s.style == Style::Strong));
        let has_emphasis = lines
            .iter()
            .any(|l| l.spans.iter().any(|s| s.style == Style::Emphasis));

        assert!(has_strong, "Should have Strong style");
        assert!(has_emphasis, "Should have Emphasis style");
    }

    #[test]
    fn test_code_block_no_wrap() {
        let md = "```\nfn main() {\n    println!(\"hello\");\n}\n```";
        let lines = render_markdown(md, 20);

        // Code block lines should have CodeBlock style
        let code_lines: Vec<_> = lines
            .iter()
            .filter(|l| l.spans.iter().any(|s| s.style == Style::CodeBlock))
            .collect();

        assert!(!code_lines.is_empty());
        // Should preserve indentation
        let has_indent = code_lines
            .iter()
            .any(|l| l.spans.iter().any(|s| s.text.contains("    ")));
        assert!(has_indent, "Code block should preserve indentation");
    }

    #[test]
    fn test_heading_styles() {
        let lines = render_markdown("# H1\n\n## H2\n\n### H3", 80);

        let has_h1 = lines
            .iter()
            .any(|l| l.spans.iter().any(|s| s.style == Style::H1));
        let has_h2 = lines
            .iter()
            .any(|l| l.spans.iter().any(|s| s.style == Style::H2));
        let has_h3 = lines
            .iter()
            .any(|l| l.spans.iter().any(|s| s.style == Style::H3));

        assert!(has_h1, "Should have H1 style");
        assert!(has_h2, "Should have H2 style");
        assert!(has_h3, "Should have H3 style");
    }

    #[test]
    fn test_list_indent() {
        let lines = render_markdown("- item 1\n- item 2", 80);

        // Should have list bullets
        let has_bullet = lines
            .iter()
            .any(|l| l.spans.iter().any(|s| s.style == Style::ListBullet));
        assert!(has_bullet, "Should have list bullets");
    }

    #[test]
    fn test_ordered_list() {
        let lines = render_markdown("1. first\n2. second", 80);

        // Should have list numbers
        let has_number = lines
            .iter()
            .any(|l| l.spans.iter().any(|s| s.style == Style::ListNumber));
        assert!(has_number, "Should have list numbers");
    }

    #[test]
    fn test_fallback_to_plain() {
        // Plain text should work fine
        let lines = render_markdown("Just plain text without any markdown", 80);

        assert!(!lines.is_empty());
        // Should have Assistant style (default)
        let has_assistant = lines
            .iter()
            .any(|l| l.spans.iter().any(|s| s.style == Style::Assistant));
        assert!(has_assistant);
    }

    #[test]
    fn test_empty_input() {
        let lines = render_markdown("", 80);
        assert_eq!(lines.len(), 1);
    }

    #[test]
    fn test_soft_hard_breaks() {
        // Soft break (single newline in paragraph) becomes space
        let md = "line1\nline2";
        let lines = render_markdown(md, 80);

        // Should be rendered as single paragraph
        // (pulldown-cmark treats single newline as soft break)
        assert!(!lines.is_empty());
    }
}
