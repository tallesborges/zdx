#![allow(
    clippy::cast_possible_truncation,
    clippy::struct_excessive_bools,
    clippy::match_same_arms
)]

use pulldown_cmark::{CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd};
use unicode_width::UnicodeWidthStr;

use super::wrap::{WrapOptions, wrap_styled_spans};
use crate::common::{sanitize_for_display, terminal_display_width, terminal_truncate};
use crate::transcript::{Style, StyledLine, StyledSpan};

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

    // Sanitize text for display (strips ANSI escapes, expands tabs)
    let text = sanitize_for_display(text);

    let mut options = Options::empty();
    options.insert(Options::ENABLE_TABLES);
    let parser = Parser::new_ext(&text, options);
    let mut renderer = MarkdownRenderer::new(width);

    for event in parser {
        renderer.process_event(event);
    }

    renderer.finish()
}

/// Simple table buffer for rendering markdown tables.
#[derive(Debug, Clone, Default)]
struct TableBuffer {
    /// Header row cells (plain text).
    header: Vec<String>,
    /// Data rows (plain text).
    rows: Vec<Vec<String>>,
    /// Current row being built.
    current_row: Vec<String>,
    /// Current cell text being collected.
    current_cell: String,
}

impl TableBuffer {
    fn new() -> Self {
        Self::default()
    }

    fn clear(&mut self) {
        self.header.clear();
        self.rows.clear();
        self.current_row.clear();
        self.current_cell.clear();
    }

    fn push_cell_text(&mut self, text: &str) {
        self.current_cell.push_str(text);
    }

    fn finish_cell(&mut self) {
        let cell = std::mem::take(&mut self.current_cell);
        self.current_row.push(cell);
    }

    fn finish_row(&mut self, is_header: bool) {
        let row = std::mem::take(&mut self.current_row);
        if is_header {
            self.header = row;
        } else {
            self.rows.push(row);
        }
    }

    /// Render the table using comfy-table and return plain text lines.
    ///
    /// Uses `terminal_display_width` for column sizing to correctly handle
    /// emoji with variation selectors (e.g. ⚠️) that `unicode-width` under-counts.
    fn render(&self, max_width: usize) -> Vec<String> {
        let all_rows: Vec<&Vec<String>> = if self.header.is_empty() {
            self.rows.iter().collect()
        } else {
            std::iter::once(&self.header)
                .chain(self.rows.iter())
                .collect()
        };

        if all_rows.is_empty() {
            return Vec::new();
        }

        let num_cols = all_rows.iter().map(|r| r.len()).max().unwrap_or(0);
        if num_cols == 0 {
            return Vec::new();
        }

        // Calculate the max terminal display width for each column.
        let mut col_widths: Vec<usize> = vec![0; num_cols];
        for row in &all_rows {
            for (i, cell) in row.iter().enumerate() {
                col_widths[i] = col_widths[i].max(terminal_display_width(cell));
            }
        }

        // Ensure minimum column width of 3 for readability.
        for w in &mut col_widths {
            if *w < 3 {
                *w = 3;
            }
        }

        // Shrink columns if total width exceeds max_width.
        // Overhead: "| " prefix per col + " |" suffix = 2*num_cols + 1 + (num_cols-1) for separators
        // Actually: "| col1 | col2 |" → each col has "| " prefix (2) and last col has " |" suffix (2)
        // Total overhead = 1 (leading |) + num_cols * 3 (space + content + space) - content + trailing |
        // Let's just compute: borders = num_cols * 3 + 1 (for "| c | c |" pattern)
        let border_overhead = num_cols * 3 + 1;
        let content_budget = max_width.saturating_sub(border_overhead);
        let total_content: usize = col_widths.iter().sum();

        if total_content > content_budget && content_budget > 0 {
            // Proportionally shrink columns
            let scale = content_budget as f64 / total_content as f64;
            for w in &mut col_widths {
                *w = ((*w as f64 * scale).floor() as usize).max(1);
            }
        }

        let mut lines = Vec::new();

        // Helper: build a separator line like "+------+------+"
        let separator: String = {
            let mut s = String::from("+");
            for &w in &col_widths {
                s.push_str(&"-".repeat(w + 2));
                s.push('+');
            }
            s
        };

        // Helper: build a header separator line like "+=======+=======+"
        let header_separator: String = {
            let mut s = String::from("+");
            for &w in &col_widths {
                s.push_str(&"=".repeat(w + 2));
                s.push('+');
            }
            s
        };

        // Top border
        lines.push(separator.clone());

        for (row_idx, row) in all_rows.iter().enumerate() {
            // Build row line: "| cell1 | cell2 |"
            let mut line = String::from("|");
            for (col_idx, col_w) in col_widths.iter().enumerate() {
                let cell = row.get(col_idx).map_or("", String::as_str);
                // Truncate cell content if it exceeds the column width
                let cell = terminal_truncate(cell, *col_w);
                let cell_width = terminal_display_width(&cell);
                let padding = col_w.saturating_sub(cell_width);
                line.push(' ');
                line.push_str(&cell);
                // Pad with spaces to fill the column
                for _ in 0..padding {
                    line.push(' ');
                }
                line.push_str(" |");
            }
            lines.push(line);

            // After header row, use header separator; after other rows, normal separator
            if row_idx == 0 && !self.header.is_empty() {
                lines.push(header_separator.clone());
            } else {
                lines.push(separator.clone());
            }
        }

        lines
    }
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
    /// Are we inside a table?
    in_table: bool,
    /// Are we in the table header row?
    in_table_head: bool,
    /// Buffer for table content.
    table_buffer: TableBuffer,
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
            in_table: false,
            in_table_head: false,
            table_buffer: TableBuffer::new(),
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
            }
            Tag::Image { .. } => {
                // Images not supported in terminal
            }
            Tag::Table(_) => {
                self.flush_paragraph();
                self.in_table = true;
                self.table_buffer.clear();
            }
            Tag::TableHead => {
                self.in_table_head = true;
            }
            Tag::TableRow => {
                // Row will be built via cells
            }
            Tag::TableCell => {
                self.table_buffer.current_cell.clear();
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
                self.push_style(Style::Plain);
            }
            Tag::Subscript => {
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
            TagEnd::Table => {
                self.flush_table();
                self.in_table = false;
                // Add blank line after table
                self.lines.push(StyledLine::empty());
            }
            TagEnd::TableHead => {
                self.table_buffer.finish_row(true);
                self.in_table_head = false;
            }
            TagEnd::TableRow => {
                if !self.in_table_head {
                    self.table_buffer.finish_row(false);
                }
            }
            TagEnd::TableCell => {
                self.table_buffer.finish_cell();
            }
            _ => {}
        }
    }

    fn add_text(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }

        // When inside a table, collect plain text for the current cell
        if self.in_table {
            // Normalize newlines to spaces
            let text = text.replace('\n', " ");
            self.table_buffer.push_cell_text(&text);
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
        // Inline code in tables
        if self.in_table {
            let code = code.replace('\n', " ");
            self.table_buffer.push_cell_text(&format!("`{code}`"));
            return;
        }

        self.current_spans.push(StyledSpan {
            text: code.to_string(),
            style: Style::CodeInline,
        });
    }

    fn add_soft_break(&mut self) {
        if self.in_table {
            self.table_buffer.push_cell_text(" ");
            return;
        }

        // Soft break becomes a space
        self.current_spans.push(StyledSpan {
            text: " ".to_string(),
            style: self.current_style(),
        });
    }

    fn add_hard_break(&mut self) {
        if self.in_table {
            self.table_buffer.push_cell_text(" ");
            return;
        }

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
            Some(lang) => format!("```{lang}"),
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

    fn flush_table(&mut self) {
        // Render table manually, then convert to StyledLines
        let table_lines = self.table_buffer.render(self.width);

        for line in table_lines {
            self.lines.push(StyledLine {
                spans: vec![StyledSpan {
                    text: line,
                    style: Style::Plain,
                }],
            });
        }

        self.table_buffer.clear();
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
            "Expected spaces around inline code, got: {combined:?}"
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
    fn test_table_renders() {
        let md = "| A | B |\n|---|---|\n| 1 | 2 |";
        let lines = render_markdown(md, 80);

        // Should have at least header + separator + data row
        assert!(lines.len() >= 3, "Table should render multiple lines");

        // Combine all text
        let combined: String = lines
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| s.text.as_str()))
            .collect::<Vec<_>>()
            .join("\n");

        // Should contain the cell content
        assert!(combined.contains('A'), "Table should contain header A");
        assert!(combined.contains('B'), "Table should contain header B");
        assert!(combined.contains('1'), "Table should contain cell 1");
        assert!(combined.contains('2'), "Table should contain cell 2");
    }

    #[test]
    fn test_table_with_emoji_alignment() {
        // Regression test: emojis with VS16 (like ⚠️) must not break table alignment.
        let md = "| Fix | Verdict |\n|---|---|\n| #1 | ✅ Ship |\n| #2 | ⚠️ Needs change |";
        let lines = render_markdown(md, 80);

        // Extract table lines (non-empty, with pipe characters)
        let table_lines: Vec<&str> = lines
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| s.text.as_str()))
            .filter(|s| s.contains('|'))
            .collect();

        // All border/content lines should have the same display width
        let widths: Vec<usize> = table_lines
            .iter()
            .map(|l| terminal_display_width(l))
            .collect();

        assert!(
            !widths.is_empty(),
            "Table should have lines with pipe chars"
        );
        let first = widths[0];
        for (i, &w) in widths.iter().enumerate() {
            assert_eq!(
                w, first,
                "Line {i} has display width {w}, expected {first}.\nLine: {:?}",
                table_lines[i]
            );
        }
    }

    #[test]
    fn test_table_long_content_fits_max_width() {
        // Table with very long cell content must not exceed the given width.
        let md = "| Col | Description |\n|---|---|\n| A | This is a very long description that should be truncated to fit within the table width |";
        let max_width = 50;
        let lines = render_markdown(md, max_width);

        for styled_line in &lines {
            for span in &styled_line.spans {
                let w = terminal_display_width(&span.text);
                assert!(
                    w <= max_width,
                    "Line exceeds max_width ({w} > {max_width}): {:?}",
                    span.text
                );
            }
        }
    }
}
