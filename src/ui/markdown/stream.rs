use super::parse::render_markdown;
use crate::ui::transcript::StyledLine;

/// Maximum bytes to buffer before forcing a commit, even without a newline.
/// This prevents very long paragraphs from delaying rendering indefinitely.
const MAX_BUFFER_BEFORE_FORCE_COMMIT: usize = 500;

/// Collects streaming markdown deltas and commits complete elements incrementally.
///
/// This collector buffers incoming markdown text and determines "commit points" where
/// content can be safely rendered. A commit point is typically a newline, but we avoid
/// committing in the middle of code blocks (where closing ``` hasn't arrived yet).
///
/// # Usage
///
/// ```ignore
/// let mut collector = MarkdownStreamCollector::new();
///
/// // During streaming - only committed (complete) content is rendered
/// collector.push_delta("# Hello\n");
/// collector.push_delta("Some text");
/// let lines = collector.render_committed(80);
///
/// // When finalized - use render_markdown() directly on the full content
/// ```
#[derive(Debug, Clone, Default)]
pub struct MarkdownStreamCollector {
    /// The accumulated raw markdown buffer.
    buffer: String,
}

impl MarkdownStreamCollector {
    /// Creates a new empty stream collector.
    pub fn new() -> Self {
        Self::default()
    }

    /// Appends a delta to the markdown buffer.
    pub fn push_delta(&mut self, delta: &str) {
        self.buffer.push_str(delta);
    }

    /// Renders committed content (up to the last safe commit point).
    ///
    /// A safe commit point is:
    /// - The last newline, if we're not inside an unclosed code fence
    /// - Content before an unclosed code fence (if there's prior content)
    /// - Or a forced commit after MAX_BUFFER_BEFORE_FORCE_COMMIT bytes without newline
    ///
    /// Returns the rendered styled lines for the committed portion.
    pub fn render_committed(&self, width: usize) -> Vec<StyledLine> {
        let commit_pos = self.find_commit_point();
        if commit_pos == 0 {
            return vec![];
        }

        let to_render = &self.buffer[..commit_pos];
        render_markdown(to_render, width)
    }

    /// Finds the safe commit point in the buffer.
    ///
    /// Returns the byte position up to which we can safely render markdown.
    /// Returns 0 if no safe commit point exists yet.
    fn find_commit_point(&self) -> usize {
        if self.buffer.is_empty() {
            return 0;
        }

        // Find all fence positions and determine safe commit point
        let fence_positions = self.find_fence_positions();

        if fence_positions.is_empty() {
            // No fences - commit up to last newline
            return self.find_last_newline_commit();
        }

        // Check if we're inside an unclosed code block (odd fence count)
        if fence_positions.len() % 2 == 1 {
            // We have an unclosed fence. Find safe commit point BEFORE it.
            let unclosed_fence_start = fence_positions[fence_positions.len() - 1];

            // If there are complete blocks before this, commit up to after the last complete block
            if fence_positions.len() >= 3 {
                // We have at least one complete block (pair) before the unclosed one
                let last_complete_close = fence_positions[fence_positions.len() - 2];
                if let Some(newline_pos) = self.buffer[last_complete_close..].find('\n') {
                    let commit_pos = last_complete_close + newline_pos + 1;
                    // Only commit if this is before the unclosed fence
                    if commit_pos <= unclosed_fence_start {
                        return commit_pos;
                    }
                }
            }

            // Find last newline before the unclosed fence
            if unclosed_fence_start > 0 {
                // Look for the start of the line containing the fence
                if let Some(line_start) = self.buffer[..unclosed_fence_start].rfind('\n') {
                    return line_start + 1;
                }
            }

            // Unclosed fence at start of buffer - can't commit anything
            return 0;
        }

        // Even fence count - all blocks are closed
        // Commit up to after the last closing fence (if terminated) or last newline
        if fence_positions.len() >= 2 {
            let last_close = fence_positions[fence_positions.len() - 1];
            if let Some(newline_pos) = self.buffer[last_close..].find('\n') {
                return last_close + newline_pos + 1;
            }
            // Closing fence not terminated yet - commit up to before it
            if let Some(line_start) = self.buffer[..last_close].rfind('\n') {
                return line_start + 1;
            }
        }

        // Fallback: commit up to last newline
        self.find_last_newline_commit()
    }

    /// Find commit point based on last newline (for non-code-block content).
    fn find_last_newline_commit(&self) -> usize {
        if let Some(pos) = self.buffer.rfind('\n') {
            return pos + 1;
        }

        // No newline found - check if we should force commit due to buffer size
        if self.buffer.len() > MAX_BUFFER_BEFORE_FORCE_COMMIT {
            if let Some(pos) = self.buffer[..MAX_BUFFER_BEFORE_FORCE_COMMIT].rfind(' ') {
                return pos + 1;
            }
            return MAX_BUFFER_BEFORE_FORCE_COMMIT;
        }

        0
    }

    /// Finds all fence positions in the buffer.
    ///
    /// A fence is ``` or ~~~ at the start of a line (with up to 3 leading spaces).
    fn find_fence_positions(&self) -> Vec<usize> {
        let mut positions = Vec::new();
        let mut line_start = 0;

        for (i, ch) in self.buffer.char_indices() {
            if ch == '\n' {
                line_start = i + 1;
                continue;
            }

            // Check if we're at a potential fence start
            if i == line_start || (i > line_start && i - line_start <= 3) {
                // Allow up to 3 leading spaces
                let before_fence = &self.buffer[line_start..i];
                if before_fence.chars().all(|c| c == ' ') {
                    // Check for ``` or ~~~
                    let remaining = &self.buffer[i..];
                    if remaining.starts_with("```") || remaining.starts_with("~~~") {
                        positions.push(i);
                    }
                }
            }
        }

        positions
    }

    /// Checks if we're currently inside an unclosed code block.
    #[cfg(test)]
    pub fn is_in_code_block(&self) -> bool {
        self.find_fence_positions().len() % 2 == 1
    }
}

/// Renders markdown content for streaming, returning only the committed portion.
///
/// This is a convenience function that creates a collector, pushes the content,
/// and returns the committed lines. For actual streaming, use `MarkdownStreamCollector`
/// directly to avoid re-parsing on each delta.
pub fn render_markdown_streaming(content: &str, width: usize) -> Vec<StyledLine> {
    let mut collector = MarkdownStreamCollector::new();
    collector.push_delta(content);
    collector.render_committed(width)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_stream_collector_empty() {
        let collector = MarkdownStreamCollector::new();
        let lines = collector.render_committed(80);
        assert!(lines.is_empty(), "Empty buffer should produce no lines");
    }

    #[test]
    fn test_stream_collector_no_newline() {
        let mut collector = MarkdownStreamCollector::new();
        collector.push_delta("Hello world");
        let lines = collector.render_committed(80);
        // No newline yet, so nothing committed (unless over buffer limit)
        assert!(lines.is_empty(), "No newline = nothing committed");
    }

    #[test]
    fn test_stream_collector_with_newline() {
        let mut collector = MarkdownStreamCollector::new();
        collector.push_delta("Hello\n");
        let lines = collector.render_committed(80);
        assert!(!lines.is_empty(), "Should commit line ending with newline");
    }

    #[test]
    fn test_stream_collector_multiple_lines() {
        let mut collector = MarkdownStreamCollector::new();
        collector.push_delta("Line 1\nLine 2\nLine 3");
        let lines = collector.render_committed(80);
        // Should commit up to after "Line 2\n"
        assert!(!lines.is_empty(), "Should commit completed lines");
        // Line 3 has no newline, so it won't be fully parsed in committed
    }

    #[test]
    fn test_stream_collector_code_block_not_committed() {
        let mut collector = MarkdownStreamCollector::new();
        collector.push_delta("```rust\nfn main() {\n");
        let lines = collector.render_committed(80);
        // Inside code block without closing fence - don't commit
        assert!(
            lines.is_empty(),
            "Should not commit inside unclosed code block"
        );
    }

    #[test]
    fn test_stream_collector_code_block_closed() {
        let mut collector = MarkdownStreamCollector::new();
        collector.push_delta("```rust\nfn main() {}\n```\n");
        let lines = collector.render_committed(80);
        // Code block is closed - should commit
        assert!(!lines.is_empty(), "Should commit when code block is closed");
    }

    #[test]
    fn test_stream_collector_incremental() {
        let mut collector = MarkdownStreamCollector::new();

        // Push some deltas
        collector.push_delta("# Title\n");
        let lines1 = collector.render_committed(80);
        assert!(!lines1.is_empty(), "Should have title line");

        collector.push_delta("\nParagraph text");
        let lines2 = collector.render_committed(80);
        // Now we have "# Title\n\nParagraph text" - should commit up to blank line
        assert!(!lines2.is_empty());
    }

    #[test]
    fn test_stream_collector_is_in_code_block() {
        let mut collector = MarkdownStreamCollector::new();

        collector.push_delta("normal text\n");
        assert!(!collector.is_in_code_block(), "Should not be in code block");

        collector.push_delta("```\ncode\n");
        assert!(collector.is_in_code_block(), "Should be in code block");

        collector.push_delta("```\n");
        assert!(
            !collector.is_in_code_block(),
            "Should not be in code block after close"
        );
    }

    #[test]
    fn test_stream_collector_inline_backticks_not_fence() {
        let mut collector = MarkdownStreamCollector::new();
        // Inline code uses single or double backticks, not triple
        collector.push_delta("Use `code` here\n");
        // Should NOT be in code block
        assert!(
            !collector.is_in_code_block(),
            "Inline backticks should not trigger code block"
        );
    }

    #[test]
    fn test_render_markdown_streaming() {
        // Test the convenience function
        let lines = render_markdown_streaming("# Hello\nWorld", 80);
        // Should commit the heading line only
        assert!(!lines.is_empty());
    }

    // ========================================================================
    // Edge case tests for fence detection
    // ========================================================================

    #[test]
    fn test_stream_collector_tilde_fence() {
        let mut collector = MarkdownStreamCollector::new();
        collector.push_delta("~~~\ncode\n");
        assert!(
            collector.is_in_code_block(),
            "Tilde fence should open block"
        );

        collector.push_delta("~~~\n");
        assert!(
            !collector.is_in_code_block(),
            "Tilde fence should close block"
        );
    }

    #[test]
    fn test_stream_collector_indented_fence() {
        let mut collector = MarkdownStreamCollector::new();
        // Up to 3 spaces before fence is allowed
        collector.push_delta("   ```\ncode\n");
        assert!(
            collector.is_in_code_block(),
            "Indented fence should open block"
        );

        collector.push_delta("   ```\n");
        assert!(
            !collector.is_in_code_block(),
            "Indented fence should close block"
        );
    }

    #[test]
    fn test_stream_collector_content_before_unclosed_fence() {
        let mut collector = MarkdownStreamCollector::new();
        // Content before the unclosed fence should be committed
        collector.push_delta("# Heading\n\nParagraph\n```rust\nfn main() {\n");

        let lines = collector.render_committed(80);
        // Should commit heading and paragraph, not the unclosed code block
        assert!(
            !lines.is_empty(),
            "Should commit content before unclosed fence"
        );

        // Verify the heading is in the output
        let text: String = lines
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| s.text.as_str()))
            .collect();
        assert!(text.contains("Heading"), "Should contain heading");
    }

    #[test]
    fn test_stream_collector_complete_block_then_unclosed() {
        let mut collector = MarkdownStreamCollector::new();
        // Complete block followed by unclosed block
        collector.push_delta("```\nfirst\n```\n\n```\nsecond\n");

        let lines = collector.render_committed(80);
        // Should commit the first complete block
        assert!(
            !lines.is_empty(),
            "Should commit complete block before unclosed"
        );
    }

    #[test]
    fn test_stream_collector_four_spaces_not_fence() {
        let mut collector = MarkdownStreamCollector::new();
        // 4 spaces means indented code block, not a fence
        collector.push_delta("    ```\ncode\n");
        assert!(
            !collector.is_in_code_block(),
            "4-space indent should not be a fence"
        );
    }
}
