use super::parse::render_markdown;
use crate::transcript::StyledLine;

/// Maximum bytes to buffer before forcing a commit, even without a newline.
/// This prevents very long paragraphs from delaying rendering indefinitely.
const MAX_BUFFER_BEFORE_FORCE_COMMIT: usize = 500;

/// Collects streaming markdown deltas and commits complete elements incrementally.
///
/// This collector buffers incoming markdown text and determines "commit points" where
/// content can be safely rendered. A commit point is typically a newline, but we avoid
/// committing in the middle of code blocks (where closing `` ``` `` hasn't arrived yet).
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
#[cfg(test)]
pub struct MarkdownStreamCollector {
    /// The accumulated raw markdown buffer.
    buffer: String,
}

#[cfg(test)]
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
    /// Returns the rendered styled lines for the committed portion.
    pub fn render_committed(&self, width: usize) -> Vec<StyledLine> {
        let commit_pos = commit_point(&self.buffer);
        if commit_pos == 0 {
            return vec![];
        }
        render_markdown(&self.buffer[..commit_pos], width)
    }

    /// Checks if we're currently inside an unclosed code block.
    #[cfg(test)]
    pub fn is_in_code_block(&self) -> bool {
        fence_positions(&self.buffer).len() % 2 == 1
    }
}

/// Length of the committed (stable) markdown prefix of `content`.
///
/// The streaming renderer only renders `content[..committed_len]`, and that
/// prefix is byte-stable until a new commit point appears. Callers use this to
/// key cached output so deltas that don't advance a commit point skip
/// re-parsing markdown every frame.
pub(crate) fn committed_len(content: &str) -> usize {
    commit_point(content)
}

/// Finds the safe commit point in `buffer`: the byte position up to which
/// markdown can be rendered. Returns 0 if nothing can be committed yet.
///
/// A safe commit point is the last newline (when not inside an unclosed code
/// fence), the content before an unclosed fence, or a forced commit after
/// `MAX_BUFFER_BEFORE_FORCE_COMMIT` bytes without a newline.
fn commit_point(buffer: &str) -> usize {
    if buffer.is_empty() {
        return 0;
    }

    let fences = fence_positions(buffer);

    if fences.is_empty() {
        // No fences - commit up to last newline
        return last_newline_commit(buffer);
    }

    // Check if we're inside an unclosed code block (odd fence count)
    if fences.len() % 2 == 1 {
        // We have an unclosed fence. Find safe commit point BEFORE it.
        let unclosed_fence_start = fences[fences.len() - 1];

        // If there are complete blocks before this, commit up to after the last complete block
        if fences.len() >= 3 {
            let last_complete_close = fences[fences.len() - 2];
            if let Some(newline_pos) = buffer[last_complete_close..].find('\n') {
                let commit_pos = last_complete_close + newline_pos + 1;
                if commit_pos <= unclosed_fence_start {
                    return commit_pos;
                }
            }
        }

        // Find last newline before the unclosed fence
        if unclosed_fence_start > 0
            && let Some(line_start) = buffer[..unclosed_fence_start].rfind('\n')
        {
            return line_start + 1;
        }

        // Unclosed fence at start of buffer - can't commit anything
        return 0;
    }

    // Even fence count - all blocks are closed.
    if fences.len() >= 2 {
        let last_close = fences[fences.len() - 1];
        if let Some(newline_pos) = buffer[last_close..].find('\n') {
            return last_close + newline_pos + 1;
        }
        // Closing fence not terminated yet - commit up to before it
        if let Some(line_start) = buffer[..last_close].rfind('\n') {
            return line_start + 1;
        }
    }

    // Fallback: commit up to last newline
    last_newline_commit(buffer)
}

/// Commit point based on the last newline (for non-code-block content).
fn last_newline_commit(buffer: &str) -> usize {
    if let Some(pos) = buffer.rfind('\n') {
        return pos + 1;
    }

    // No newline found - force commit if the buffer is getting long.
    if buffer.len() > MAX_BUFFER_BEFORE_FORCE_COMMIT {
        if let Some(pos) = buffer[..MAX_BUFFER_BEFORE_FORCE_COMMIT].rfind(' ') {
            return pos + 1;
        }
        return MAX_BUFFER_BEFORE_FORCE_COMMIT;
    }

    0
}

/// Finds all fence positions in `buffer`.
///
/// A fence starts with three backticks or `~~~` at the start of a line
/// (with up to 3 leading spaces).
fn fence_positions(buffer: &str) -> Vec<usize> {
    let mut positions = Vec::new();
    let mut line_start = 0;

    for (i, ch) in buffer.char_indices() {
        if ch == '\n' {
            line_start = i + 1;
            continue;
        }

        // Check if we're at a potential fence start (allow up to 3 leading spaces)
        if i == line_start || (i > line_start && i - line_start <= 3) {
            let before_fence = &buffer[line_start..i];
            if before_fence.chars().all(|c| c == ' ') {
                let remaining = &buffer[i..];
                if remaining.starts_with("```") || remaining.starts_with("~~~") {
                    positions.push(i);
                }
            }
        }
    }

    positions
}

/// Renders markdown content for streaming, returning only the committed portion.
///
/// Renders `content[..commit_point]` — complete lines/blocks only — so partial
/// trailing content isn't shown mid-parse.
pub fn render_markdown_streaming(content: &str, width: usize) -> Vec<StyledLine> {
    let commit_pos = commit_point(content);
    if commit_pos == 0 {
        return vec![];
    }
    render_markdown(&content[..commit_pos], width)
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
