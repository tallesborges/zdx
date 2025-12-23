//! Transcript model for TUI rendering.
//!
//! This module defines the transcript types that form the source of truth
//! for the TUI. The transcript is width-agnostic; wrapping happens at
//! display time for the current terminal width.
//!
//! See SPEC.md §9 for the contract.

use std::sync::atomic::{AtomicU64, Ordering};

use chrono::{DateTime, Utc};
use serde_json::Value;

use crate::core::events::ToolOutput;

/// Global counter for generating unique cell IDs.
static CELL_ID_COUNTER: AtomicU64 = AtomicU64::new(1);

/// Unique identifier for a transcript cell.
///
/// IDs are monotonically increasing and unique within a process.
/// Used for:
/// - Selection anchoring
/// - Scroll position tracking
/// - Event addressing (e.g., `AssistantDelta { cell_id }`)
/// - Wrap cache keying
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CellId(pub u64);

impl CellId {
    /// Generates a new unique cell ID.
    pub fn new() -> Self {
        CellId(CELL_ID_COUNTER.fetch_add(1, Ordering::Relaxed))
    }
}

impl Default for CellId {
    fn default() -> Self {
        Self::new()
    }
}

/// State of a tool invocation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolState {
    /// Tool is currently executing.
    Running,
    /// Tool completed successfully.
    Done,
    /// Tool failed with an error.
    Error,
    /// Tool was cancelled/interrupted by user.
    Cancelled,
}

/// Spinner frames for animated tool running indicator.
const SPINNER_FRAMES: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

/// A logical unit in the transcript.
///
/// Each cell represents a complete conceptual block:
/// - User input
/// - Assistant response (streaming or final)
/// - Tool invocation with result
/// - System/info banner
///
/// Every cell has a unique `id` for addressing and an optional `created_at`
/// timestamp for ordering/display.
#[derive(Debug, Clone, PartialEq)]
pub enum HistoryCell {
    /// User input message.
    User {
        id: CellId,
        created_at: DateTime<Utc>,
        content: String,
    },

    /// Assistant response.
    ///
    /// During streaming, `content` accumulates deltas.
    /// `is_streaming` indicates if more content is expected.
    Assistant {
        id: CellId,
        created_at: DateTime<Utc>,
        content: String,
        is_streaming: bool,
    },

    /// Tool invocation with state and optional result.
    Tool {
        id: CellId,
        created_at: DateTime<Utc>,
        tool_use_id: String,
        name: String,
        input: Value,
        state: ToolState,
        started_at: DateTime<Utc>,
        /// Some when tool has finished (Done or Error).
        result: Option<ToolOutput>,
    },

    /// System message or informational banner.
    System {
        id: CellId,
        created_at: DateTime<Utc>,
        content: String,
    },
}

impl HistoryCell {
    /// Returns the cell's unique ID.
    pub fn id(&self) -> CellId {
        match self {
            HistoryCell::User { id, .. } => *id,
            HistoryCell::Assistant { id, .. } => *id,
            HistoryCell::Tool { id, .. } => *id,
            HistoryCell::System { id, .. } => *id,
        }
    }

    /// Creates a new user cell.
    pub fn user(content: impl Into<String>) -> Self {
        HistoryCell::User {
            id: CellId::new(),
            created_at: Utc::now(),
            content: content.into(),
        }
    }

    /// Creates a new assistant cell (finalized, not streaming).
    pub fn assistant(content: impl Into<String>) -> Self {
        HistoryCell::Assistant {
            id: CellId::new(),
            created_at: Utc::now(),
            content: content.into(),
            is_streaming: false,
        }
    }

    /// Creates a new streaming assistant cell.
    pub fn assistant_streaming(content: impl Into<String>) -> Self {
        HistoryCell::Assistant {
            id: CellId::new(),
            created_at: Utc::now(),
            content: content.into(),
            is_streaming: true,
        }
    }

    /// Creates a new tool cell (running state).
    pub fn tool_running(
        tool_use_id: impl Into<String>,
        name: impl Into<String>,
        input: Value,
    ) -> Self {
        let now = Utc::now();
        HistoryCell::Tool {
            id: CellId::new(),
            created_at: now,
            tool_use_id: tool_use_id.into(),
            name: name.into(),
            input,
            state: ToolState::Running,
            started_at: now,
            result: None,
        }
    }

    /// Creates a system/info cell.
    pub fn system(content: impl Into<String>) -> Self {
        HistoryCell::System {
            id: CellId::new(),
            created_at: Utc::now(),
            content: content.into(),
        }
    }

    /// Appends text to an assistant cell's content.
    ///
    /// Panics if called on a non-assistant cell.
    pub fn append_assistant_delta(&mut self, delta: &str) {
        match self {
            HistoryCell::Assistant { content, .. } => {
                content.push_str(delta);
            }
            _ => panic!("append_assistant_delta called on non-assistant cell"),
        }
    }

    /// Marks an assistant cell as finalized (no longer streaming).
    ///
    /// Panics if called on a non-assistant cell.
    pub fn finalize_assistant(&mut self) {
        match self {
            HistoryCell::Assistant { is_streaming, .. } => {
                *is_streaming = false;
            }
            _ => panic!("finalize_assistant called on non-assistant cell"),
        }
    }

    /// Sets the result on a tool cell and updates state to Done or Error.
    ///
    /// Panics if called on a non-tool cell.
    pub fn set_tool_result(&mut self, tool_result: ToolOutput) {
        match self {
            HistoryCell::Tool { state, result, .. } => {
                *state = if tool_result.is_ok() {
                    ToolState::Done
                } else {
                    ToolState::Error
                };
                *result = Some(tool_result);
            }
            _ => panic!("set_tool_result called on non-tool cell"),
        }
    }

    /// Marks a tool cell as cancelled (interrupted by user).
    ///
    /// Only affects cells that are still in Running state.
    pub fn mark_cancelled(&mut self) {
        if let HistoryCell::Tool { state, .. } = self
            && *state == ToolState::Running
        {
            *state = ToolState::Cancelled;
        }
    }

    /// Renders this cell into display lines for the given width.
    ///
    /// This is the core rendering contract from SPEC.md §9:
    /// - Each cell can render display lines for a given width
    /// - Wrapping happens at display time for the current width
    ///
    /// The `spinner_frame` parameter controls which frame of the spinner animation
    /// to display for running tools (0-9). Callers should increment this at ~10Hz.
    pub fn display_lines(&self, width: usize, spinner_frame: usize) -> Vec<StyledLine> {
        match self {
            HistoryCell::User { content, .. } => {
                let prefix = "| ";
                render_prefixed_content(prefix, content, width, Style::UserPrefix, Style::User)
            }
            HistoryCell::Assistant {
                content,
                is_streaming,
                ..
            } => {
                let prefix = "";
                let mut lines = render_prefixed_content(
                    prefix,
                    content,
                    width,
                    Style::AssistantPrefix,
                    Style::Assistant,
                );

                // Add streaming indicator if still streaming
                if *is_streaming && !content.is_empty() {
                    // Append cursor to last line
                    if let Some(last) = lines.last_mut() {
                        last.spans.push(StyledSpan {
                            text: "▌".to_string(),
                            style: Style::StreamingCursor,
                        });
                    }
                }
                lines
            }
            HistoryCell::Tool {
                name,
                state,
                input,
                result,
                ..
            } => {
                let mut lines = Vec::new();

                // Determine prefix and command style based on state
                let (prefix, prefix_style, cmd_style, suffix) = match state {
                    ToolState::Running => {
                        let frame = SPINNER_FRAMES[spinner_frame % SPINNER_FRAMES.len()];
                        // Need to allocate since we're selecting from array
                        (
                            format!("{} ", frame),
                            Style::ToolRunning,
                            Style::ToolStatus,
                            None,
                        )
                    }
                    ToolState::Done => (
                        "$ ".to_string(),
                        Style::ToolSuccess,
                        Style::ToolStatus,
                        None,
                    ),
                    ToolState::Error => (
                        "$ ".to_string(),
                        Style::ToolError,
                        Style::ToolCancelled,
                        Some(" (failed)"),
                    ),
                    ToolState::Cancelled => (
                        "$ ".to_string(),
                        Style::ToolCancelled,
                        Style::ToolCancelled,
                        Some(" (cancelled)"),
                    ),
                };

                // Show command for bash tool, or tool name for others
                if name == "bash" {
                    if let Some(cmd) = input.get("command").and_then(|v| v.as_str()) {
                        let mut spans = vec![
                            StyledSpan {
                                text: prefix.clone(),
                                style: prefix_style,
                            },
                            StyledSpan {
                                text: cmd.to_string(),
                                style: cmd_style,
                            },
                        ];
                        if let Some(suf) = suffix {
                            spans.push(StyledSpan {
                                text: suf.to_string(),
                                style: Style::ToolError,
                            });
                        }
                        lines.push(StyledLine { spans });
                    }
                } else {
                    // For other tools, show tool name and key input
                    let input_preview = match name.as_str() {
                        "read" => input
                            .get("path")
                            .and_then(|v| v.as_str())
                            .map(|s| s.to_string()),
                        "write" => input
                            .get("path")
                            .and_then(|v| v.as_str())
                            .map(|s| s.to_string()),
                        "edit" => input
                            .get("path")
                            .and_then(|v| v.as_str())
                            .map(|s| s.to_string()),
                        _ => None,
                    };
                    let display = if let Some(preview) = input_preview {
                        format!("{}: {}", name, preview)
                    } else {
                        name.to_string()
                    };
                    let mut spans = vec![
                        StyledSpan {
                            text: prefix.clone(),
                            style: prefix_style,
                        },
                        StyledSpan {
                            text: display,
                            style: cmd_style,
                        },
                    ];
                    if let Some(suf) = suffix {
                        spans.push(StyledSpan {
                            text: suf.to_string(),
                            style: Style::ToolError,
                        });
                    }
                    lines.push(StyledLine { spans });
                }

                // Show truncated output preview when done
                if let Some(res) = result
                    && let Some(data) = res.data()
                {
                    // Show stdout for bash tool
                    if let Some(output) = data.get("stdout").and_then(|v| v.as_str()) {
                        let output_lines: Vec<&str> = output.lines().collect();
                        let max_preview_lines = 5;
                        let truncated = output_lines.len() > max_preview_lines;

                        for line in output_lines.iter().take(max_preview_lines) {
                            lines.push(StyledLine {
                                spans: vec![StyledSpan {
                                    text: (*line).to_string(),
                                    style: Style::ToolOutput,
                                }],
                            });
                        }

                        if truncated {
                            lines.push(StyledLine {
                                spans: vec![StyledSpan {
                                    text: format!(
                                        "[... {} more lines ...]",
                                        output_lines.len() - max_preview_lines
                                    ),
                                    style: Style::ToolBracket,
                                }],
                            });
                        }
                    }
                    // Show stderr if present and non-empty
                    if let Some(stderr) = data.get("stderr").and_then(|v| v.as_str())
                        && !stderr.is_empty()
                    {
                        lines.push(StyledLine {
                            spans: vec![StyledSpan {
                                text: format!("stderr: {}", stderr.lines().next().unwrap_or("")),
                                style: Style::ToolError,
                            }],
                        });
                    }
                }

                // Status line (only show when failed - spinner indicates running)
                if *state == ToolState::Error {
                    lines.push(StyledLine {
                        spans: vec![StyledSpan {
                            text: "Failed".to_string(),
                            style: Style::ToolError,
                        }],
                    });
                }

                lines
            }
            HistoryCell::System { content, .. } => {
                let prefix = "System: ";
                render_prefixed_content(prefix, content, width, Style::SystemPrefix, Style::System)
            }
        }
    }
}

/// A styled span of text (UI-agnostic).
///
/// This is a minimal representation that can be converted to
/// ratatui Span/Line types at render time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StyledSpan {
    pub text: String,
    pub style: Style,
}

/// A line of styled spans.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StyledLine {
    pub spans: Vec<StyledSpan>,
}

impl StyledLine {
    /// Creates an empty line.
    pub fn empty() -> Self {
        StyledLine { spans: vec![] }
    }
}

/// Semantic style identifiers (UI-agnostic).
///
/// These are translated to actual terminal styles by the renderer.
/// This keeps the transcript module free of terminal dependencies.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Style {
    /// No styling.
    Plain,
    /// User message prefix ("| ").
    UserPrefix,
    /// User message content (italic).
    User,
    /// Assistant message prefix (none).
    AssistantPrefix,
    /// Assistant message content.
    Assistant,
    /// Streaming cursor indicator.
    StreamingCursor,
    /// System message prefix.
    SystemPrefix,
    /// System message content.
    System,
    /// Tool bracket/decoration.
    ToolBracket,
    /// Tool status text.
    ToolStatus,
    /// Tool error status.
    ToolError,
    /// Tool running spinner.
    ToolRunning,
    /// Tool success prefix (green $).
    ToolSuccess,
    /// Tool cancelled command (strikethrough).
    ToolCancelled,
    /// Tool output (stdout from bash, etc).
    ToolOutput,
}

/// Renders content with a prefix, handling line wrapping.
///
/// The prefix appears on the first line; subsequent wrapped lines
/// are indented to align with the content start.
fn render_prefixed_content(
    prefix: &str,
    content: &str,
    width: usize,
    prefix_style: Style,
    content_style: Style,
) -> Vec<StyledLine> {
    let mut lines = Vec::new();
    let prefix_len = prefix.len();

    // Minimum usable width
    let min_width = prefix_len + 10;
    let effective_width = width.max(min_width);

    // Content width after prefix/indent
    let content_width = effective_width.saturating_sub(prefix_len);

    // Split content into paragraphs (preserve blank lines)
    let paragraphs: Vec<&str> = content.split('\n').collect();

    let mut is_first_line = true;

    for paragraph in paragraphs {
        if paragraph.is_empty() {
            // Empty paragraph = blank line (with indent for continuation)
            if is_first_line {
                lines.push(StyledLine {
                    spans: vec![StyledSpan {
                        text: prefix.to_string(),
                        style: prefix_style,
                    }],
                });
                is_first_line = false;
            } else {
                lines.push(StyledLine::empty());
            }
            continue;
        }

        // Wrap the paragraph
        let wrapped = wrap_text(paragraph, content_width);

        for wrapped_line in wrapped {
            let mut spans = Vec::new();

            if is_first_line {
                spans.push(StyledSpan {
                    text: prefix.to_string(),
                    style: prefix_style,
                });
                is_first_line = false;
            } else {
                // Indent continuation lines
                spans.push(StyledSpan {
                    text: " ".repeat(prefix_len),
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

/// Wraps text to fit within the given width.
///
/// Simple word-wrap implementation. Does not handle:
/// - Unicode grapheme clusters (uses byte length)
/// - Wide characters (CJK, emoji)
/// - Hyphenation
///
/// TODO: Use textwrap or unicode-width for proper wrapping.
fn wrap_text(text: &str, width: usize) -> Vec<String> {
    if width == 0 {
        return vec![text.to_string()];
    }

    let mut lines = Vec::new();
    let mut current_line = String::new();

    for word in text.split_whitespace() {
        if current_line.is_empty() {
            // First word on line
            if word.len() > width {
                // Word is too long, force break
                let mut remaining = word;
                while remaining.len() > width {
                    lines.push(remaining[..width].to_string());
                    remaining = &remaining[width..];
                }
                if !remaining.is_empty() {
                    current_line = remaining.to_string();
                }
            } else {
                current_line = word.to_string();
            }
        } else if current_line.len() + 1 + word.len() <= width {
            // Word fits on current line
            current_line.push(' ');
            current_line.push_str(word);
        } else {
            // Start new line
            lines.push(current_line);
            if word.len() > width {
                // Word is too long, force break
                let mut remaining = word;
                while remaining.len() > width {
                    lines.push(remaining[..width].to_string());
                    remaining = &remaining[width..];
                }
                current_line = remaining.to_string();
            } else {
                current_line = word.to_string();
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cell_id_unique() {
        let id1 = CellId::new();
        let id2 = CellId::new();
        let id3 = CellId::new();

        assert_ne!(id1, id2);
        assert_ne!(id2, id3);
        assert!(id1.0 < id2.0);
        assert!(id2.0 < id3.0);
    }

    #[test]
    fn test_cell_has_id() {
        let cell = HistoryCell::user("test");
        let id = cell.id();

        // ID should be valid
        assert!(id.0 > 0);
    }

    #[test]
    fn test_user_cell_display() {
        let cell = HistoryCell::user("Hello, world!");
        let lines = cell.display_lines(80, 0);

        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].spans.len(), 2);
        assert_eq!(lines[0].spans[0].text, "| ");
        assert_eq!(lines[0].spans[1].text, "Hello, world!");
    }

    #[test]
    fn test_user_cell_wrapping() {
        let cell = HistoryCell::user("This is a longer message that should wrap");
        let lines = cell.display_lines(25, 0);

        // "| " is 2 bytes, leaving 23 for content
        assert!(lines.len() > 1, "Should wrap to multiple lines");

        // First line has prefix
        assert_eq!(lines[0].spans[0].text, "| ");

        // Continuation lines have indent (2 spaces)
        assert_eq!(lines[1].spans[0].text, "  ");
    }

    #[test]
    fn test_assistant_streaming() {
        let cell = HistoryCell::assistant_streaming("Thinking...");
        let lines = cell.display_lines(80, 0);

        // Should have streaming cursor
        let last_line = lines.last().unwrap();
        let last_span = last_line.spans.last().unwrap();
        assert_eq!(last_span.text, "▌");
        assert_eq!(last_span.style, Style::StreamingCursor);
    }

    #[test]
    fn test_assistant_final() {
        let mut cell = HistoryCell::assistant_streaming("Done!");
        cell.finalize_assistant();
        let lines = cell.display_lines(80, 0);

        // Should NOT have streaming cursor
        let last_line = lines.last().unwrap();
        let last_span = last_line.spans.last().unwrap();
        assert_ne!(last_span.text, "▌");
    }

    #[test]
    fn test_tool_running() {
        let cell =
            HistoryCell::tool_running("123", "read", serde_json::json!({"path": "test.txt"}));
        let lines = cell.display_lines(80, 0);

        // Should have tool info line (spinner indicates running, no separate status line)
        assert!(!lines.is_empty());
        // First line should show spinner + tool name/path
        let first_line: String = lines[0].spans.iter().map(|s| s.text.as_str()).collect();
        assert!(first_line.contains("read") || first_line.contains("test.txt"));
        // Should have spinner prefix (first frame is ⠋)
        assert!(first_line.starts_with("⠋"));

        // State should be Running
        match cell {
            HistoryCell::Tool { state, .. } => assert_eq!(state, ToolState::Running),
            _ => panic!("Expected tool cell"),
        }
    }

    #[test]
    fn test_tool_success() {
        let mut cell =
            HistoryCell::tool_running("123", "read", serde_json::json!({"path": "test.txt"}));
        cell.set_tool_result(ToolOutput::success(
            serde_json::json!({"content": "file data"}),
        ));

        let lines = cell.display_lines(80, 0);
        // Should have at least the tool info line (no "Done" status line)
        assert!(!lines.is_empty());
        // Should NOT have "Running..." anymore
        let all_text: String = lines
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| s.text.as_str()))
            .collect();
        assert!(!all_text.contains("Running"));

        // State should be Done
        match cell {
            HistoryCell::Tool { state, .. } => assert_eq!(state, ToolState::Done),
            _ => panic!("Expected tool cell"),
        }
    }

    #[test]
    fn test_tool_failure() {
        let mut cell =
            HistoryCell::tool_running("123", "read", serde_json::json!({"path": "test.txt"}));
        cell.set_tool_result(ToolOutput::failure("not_found", "File not found"));

        let lines = cell.display_lines(80, 0);
        // Last line should show "Failed"
        let last_line: String = lines
            .last()
            .unwrap()
            .spans
            .iter()
            .map(|s| s.text.as_str())
            .collect();
        assert!(last_line.contains("Failed"));

        // State should be Error
        match cell {
            HistoryCell::Tool { state, .. } => assert_eq!(state, ToolState::Error),
            _ => panic!("Expected tool cell"),
        }
    }

    #[test]
    fn test_system_cell() {
        let cell = HistoryCell::system("Welcome to ZDX!");
        let lines = cell.display_lines(80, 0);

        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].spans[0].text, "System: ");
    }

    #[test]
    fn test_multiline_content() {
        let cell = HistoryCell::user("Line 1\nLine 2\nLine 3");
        let lines = cell.display_lines(80, 0);

        assert_eq!(lines.len(), 3);
        // First line has prefix
        assert_eq!(lines[0].spans[0].text, "| ");
        // Other lines have indent (2 spaces)
        assert_eq!(lines[1].spans[0].text, "  ");
        assert_eq!(lines[2].spans[0].text, "  ");
    }

    #[test]
    fn test_empty_content() {
        let cell = HistoryCell::user("");
        let lines = cell.display_lines(80, 0);

        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].spans[0].text, "| ");
    }

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

    #[test]
    fn test_append_assistant_delta() {
        let mut cell = HistoryCell::assistant_streaming("");
        cell.append_assistant_delta("Hello");
        cell.append_assistant_delta(" world");

        match &cell {
            HistoryCell::Assistant {
                content,
                is_streaming,
                ..
            } => {
                assert_eq!(content, "Hello world");
                assert!(*is_streaming);
            }
            _ => panic!("Expected assistant cell"),
        }
    }

    #[test]
    fn test_finalize_assistant() {
        let mut cell = HistoryCell::assistant_streaming("Done");
        cell.finalize_assistant();

        match &cell {
            HistoryCell::Assistant { is_streaming, .. } => {
                assert!(!*is_streaming);
            }
            _ => panic!("Expected assistant cell"),
        }
    }
}
