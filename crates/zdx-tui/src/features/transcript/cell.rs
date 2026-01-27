use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};

use chrono::{DateTime, Utc};
use serde_json::Value;
use unicode_width::UnicodeWidthStr;
use zdx_core::core::events::ToolOutput;
use zdx_core::providers::ReplayToken;

use super::style::{Style, StyledLine, StyledSpan};
use super::wrap::{WrapCache, render_prefixed_content, wrap_chars, wrap_text};
use crate::common::sanitize_for_display;

/// Formats a byte truncation warning with human-readable byte counts.
fn format_byte_truncation(stream: &str, total_bytes: u64) -> String {
    let size_str = if total_bytes >= 1024 * 1024 {
        format!("{:.1} MB", total_bytes as f64 / (1024.0 * 1024.0))
    } else if total_bytes >= 1024 {
        format!("{:.1} KB", total_bytes as f64 / 1024.0)
    } else {
        format!("{} bytes", total_bytes)
    };
    format!("{} truncated: {} total", stream, size_str)
}

fn extract_u64(value: &Value) -> Option<u64> {
    match value {
        Value::Number(num) => num.as_u64(),
        Value::String(text) => text.parse::<u64>().ok(),
        _ => None,
    }
}

fn format_read_preview(input: &Value) -> Option<String> {
    let path = input.get("path")?.as_str()?;
    let mut params = Vec::new();

    if let Some(offset) = input.get("offset").and_then(extract_u64) {
        params.push(format!("offset={}", offset));
    }
    if let Some(limit) = input.get("limit").and_then(extract_u64) {
        params.push(format!("limit={}", limit));
    }

    if params.is_empty() {
        Some(path.to_string())
    } else {
        Some(format!("{} ({})", path, params.join(", ")))
    }
}

fn tool_input_delta<'a>(name: &str, input: &'a Value) -> Option<&'a str> {
    match name {
        "write" => input.get("content")?.as_str(),
        "edit" => input.get("new")?.as_str(),
        _ => None,
    }
}

fn tail_rendered_rows(text: &str, width: usize, max_rows: usize) -> (Vec<String>, usize) {
    let mut total_rows = 0;
    let mut tail: VecDeque<String> = VecDeque::with_capacity(max_rows);

    for line in text.lines() {
        let safe_line = sanitize_for_display(line);
        let wrapped: Vec<String> = if safe_line.width() > width {
            wrap_chars(&safe_line, width)
        } else {
            vec![safe_line.into_owned()]
        };

        for row in wrapped {
            total_rows += 1;
            if tail.len() == max_rows {
                tail.pop_front();
            }
            tail.push_back(row);
        }
    }

    (tail.into_iter().collect(), total_rows)
}

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
/// Spinner frames using circle characters for better terminal compatibility.
/// Braille dots (⠋⠙⠹) may not render correctly in all terminals/fonts.
const SPINNER_FRAMES: &[&str] = &["◐", "◓", "◑", "◒"];

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
    /// `is_interrupted` indicates if the request was cancelled before any response.
    User {
        id: CellId,
        created_at: DateTime<Utc>,
        content: String,
        is_interrupted: bool,
    },

    /// Assistant response.
    ///
    /// During streaming, `content` accumulates deltas.
    /// `is_streaming` indicates if more content is expected.
    /// `is_interrupted` indicates if streaming was cancelled by user.
    Assistant {
        id: CellId,
        created_at: DateTime<Utc>,
        content: String,
        is_streaming: bool,
        is_interrupted: bool,
    },

    /// Tool invocation with state and optional result.
    Tool {
        id: CellId,
        created_at: DateTime<Utc>,
        tool_use_id: String,
        name: String,
        input: Value,
        input_delta: Option<String>,
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

    /// Thinking block (extended thinking from the model).
    ///
    /// During streaming, `content` accumulates deltas and `replay` is None.
    /// When finalized, `replay` may be set for provider-specific continuity.
    /// `is_interrupted` indicates if streaming was cancelled by user.
    Thinking {
        id: CellId,
        created_at: DateTime<Utc>,
        content: String,
        /// Provider-specific replay token (None while streaming).
        replay: Option<ReplayToken>,
        is_streaming: bool,
        is_interrupted: bool,
    },

    /// Timing/duration cell (shows tool execution time).
    ///
    /// Displayed after a tool completes to show how long it took,
    /// similar to Codex's "Worked for Xs" indicator.
    Timing {
        id: CellId,
        created_at: DateTime<Utc>,
        duration: std::time::Duration,
        tool_count: usize,
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
            HistoryCell::Thinking { id, .. } => *id,
            HistoryCell::Timing { id, .. } => *id,
        }
    }

    /// Creates a new user cell.
    pub fn user(content: impl Into<String>) -> Self {
        HistoryCell::User {
            id: CellId::new(),
            created_at: Utc::now(),
            content: content.into(),
            is_interrupted: false,
        }
    }

    /// Creates a new assistant cell (finalized, not streaming).
    pub fn assistant(content: impl Into<String>) -> Self {
        HistoryCell::Assistant {
            id: CellId::new(),
            created_at: Utc::now(),
            content: content.into(),
            is_streaming: false,
            is_interrupted: false,
        }
    }

    /// Creates a new streaming assistant cell.
    pub fn assistant_streaming(content: impl Into<String>) -> Self {
        HistoryCell::Assistant {
            id: CellId::new(),
            created_at: Utc::now(),
            content: content.into(),
            is_streaming: true,
            is_interrupted: false,
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
            input_delta: None,
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

    /// Creates a new streaming thinking cell.
    pub fn thinking_streaming(content: impl Into<String>) -> Self {
        HistoryCell::Thinking {
            id: CellId::new(),
            created_at: Utc::now(),
            content: content.into(),
            replay: None,
            is_streaming: true,
            is_interrupted: false,
        }
    }

    /// Creates a timing cell showing turn/execution duration.
    pub fn timing(duration: std::time::Duration, tool_count: usize) -> Self {
        HistoryCell::Timing {
            id: CellId::new(),
            created_at: Utc::now(),
            duration,
            tool_count,
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

    /// Appends text to a thinking cell's content.
    ///
    /// Panics if called on a non-thinking cell.
    pub fn append_thinking_delta(&mut self, delta: &str) {
        match self {
            HistoryCell::Thinking { content, .. } => {
                content.push_str(delta);
            }
            _ => panic!("append_thinking_delta called on non-thinking cell"),
        }
    }

    /// Finalizes a thinking cell with its replay token (if any).
    ///
    /// Panics if called on a non-thinking cell.
    pub fn finalize_thinking(&mut self, replay: Option<ReplayToken>) {
        match self {
            HistoryCell::Thinking {
                is_streaming,
                replay: replay_slot,
                ..
            } => {
                *is_streaming = false;
                *replay_slot = replay;
            }
            _ => panic!("finalize_thinking called on non-thinking cell"),
        }
    }

    /// Updates the input on a tool cell.
    ///
    /// Used when ToolInputCompleted arrives with the complete input after
    /// ToolRequested created the cell with empty input.
    ///
    /// Panics if called on a non-tool cell.
    pub fn set_tool_input(&mut self, new_input: serde_json::Value) {
        match self {
            HistoryCell::Tool {
                input, input_delta, ..
            } => {
                *input = new_input;
                *input_delta = None;
            }
            _ => panic!("set_tool_input called on non-tool cell"),
        }
    }

    /// Updates the streaming input preview on a tool cell.
    ///
    /// Used for tool input streaming before JSON is complete.
    ///
    /// Panics if called on a non-tool cell.
    pub fn set_tool_input_delta(&mut self, delta: String) {
        match self {
            HistoryCell::Tool { input_delta, .. } => {
                *input_delta = Some(delta);
            }
            _ => panic!("set_tool_input_delta called on non-tool cell"),
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
                } else if matches!(tool_result, ToolOutput::Canceled { .. }) {
                    ToolState::Cancelled
                } else {
                    ToolState::Error
                };
                *result = Some(tool_result);
            }
            _ => panic!("set_tool_result called on non-tool cell"),
        }
    }

    /// Marks a cell as cancelled/interrupted by user.
    ///
    /// For tools: only affects cells still in Running state.
    /// For assistant/thinking: only affects cells still streaming.
    pub fn mark_cancelled(&mut self) {
        match self {
            HistoryCell::Tool { state, .. } if *state == ToolState::Running => {
                *state = ToolState::Cancelled;
            }
            HistoryCell::Assistant {
                is_streaming,
                is_interrupted,
                ..
            } if *is_streaming => {
                *is_streaming = false;
                *is_interrupted = true;
            }
            HistoryCell::Thinking {
                is_streaming,
                is_interrupted,
                ..
            } if *is_streaming => {
                *is_streaming = false;
                *is_interrupted = true;
            }
            _ => {}
        }
    }

    /// Marks a user cell as interrupted (request cancelled before any response).
    ///
    /// Only affects User cells.
    pub fn mark_request_interrupted(&mut self) {
        if let HistoryCell::User { is_interrupted, .. } = self {
            *is_interrupted = true;
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
            HistoryCell::User {
                content,
                is_interrupted,
                ..
            } => {
                let prefix = "│ ";
                let mut lines = render_prefixed_content(
                    prefix,
                    content,
                    width,
                    Style::UserPrefix,
                    Style::User,
                    true,
                );

                // Append interrupted indicator to last line if request was cancelled
                if *is_interrupted && let Some(last) = lines.last_mut() {
                    last.spans.push(StyledSpan {
                        text: " (interrupted)".to_string(),
                        style: Style::Interrupted,
                    });
                }
                lines
            }
            HistoryCell::Assistant {
                content,
                is_streaming,
                is_interrupted,
                ..
            } => {
                // Use markdown rendering for assistant responses
                // For streaming: render only committed lines (complete elements)
                // For finalized: render everything
                use crate::markdown::{render_markdown, render_markdown_streaming};

                let mut lines = if *is_streaming {
                    // Streaming: only render committed content (complete lines/blocks)
                    let committed = render_markdown_streaming(content, width);
                    if committed.is_empty() && !content.is_empty() {
                        // No committed content yet, but we have text - show a placeholder line
                        // with just the cursor (content is buffered waiting for commit point)
                        vec![StyledLine { spans: vec![] }]
                    } else {
                        committed
                    }
                } else {
                    // Finalized: render all content
                    render_markdown(content, width)
                };

                // Add streaming indicator if still streaming
                if *is_streaming && !content.is_empty() {
                    // Append cursor to last line (or first line if we created a placeholder)
                    if let Some(last) = lines.last_mut() {
                        last.spans.push(StyledSpan {
                            text: "▌".to_string(),
                            style: Style::StreamingCursor,
                        });
                    }
                }

                // Append interrupted indicator to last line
                if *is_interrupted && let Some(last) = lines.last_mut() {
                    last.spans.push(StyledSpan {
                        text: " (interrupted)".to_string(),
                        style: Style::Interrupted,
                    });
                }
                lines
            }
            HistoryCell::Tool {
                name,
                state,
                input,
                input_delta,
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
                        Some(" (interrupted)"),
                    ),
                };

                // Show command for bash tool, or tool name for others
                let display_text = if name == "bash" {
                    input
                        .get("command")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string())
                } else {
                    // For other tools, show tool name and key input
                    let input_preview = match name.as_str() {
                        "read" => format_read_preview(input),
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
                    Some(if let Some(preview) = input_preview {
                        format!("{}: {}", name, preview)
                    } else {
                        name.to_string()
                    })
                };

                if let Some(text) = display_text {
                    // Calculate available width for content (after prefix)
                    let prefix_width = prefix.width();
                    let content_width = width.saturating_sub(prefix_width).max(10);

                    // Wrap the command/tool text
                    let wrapped = wrap_text(&text, content_width);

                    for (i, wrapped_line) in wrapped.into_iter().enumerate() {
                        let mut spans = Vec::new();

                        if i == 0 {
                            // First line gets the prefix
                            spans.push(StyledSpan {
                                text: prefix.clone(),
                                style: prefix_style,
                            });
                        } else {
                            // Continuation lines get indent
                            spans.push(StyledSpan {
                                text: " ".repeat(prefix_width),
                                style: Style::Plain,
                            });
                        }

                        spans.push(StyledSpan {
                            text: wrapped_line,
                            style: cmd_style,
                        });

                        lines.push(StyledLine { spans });
                    }

                    // Add suffix to last line if present
                    if let Some(suf) = suffix
                        && let Some(last) = lines.last_mut()
                    {
                        last.spans.push(StyledSpan {
                            text: suf.to_string(),
                            style: Style::Interrupted,
                        });
                    }
                }

                let delta_text = input_delta
                    .as_deref()
                    .or_else(|| tool_input_delta(name, input));
                if let Some(delta_text) = delta_text {
                    let max_preview_rows = 7;
                    let (rows, total_rows) =
                        tail_rendered_rows(delta_text, width, max_preview_rows);
                    if !rows.is_empty() {
                        let truncated = total_rows > rows.len();
                        let label = match name.as_str() {
                            "write" => "write delta",
                            "edit" => "edit delta",
                            _ => "delta",
                        };

                        lines.push(StyledLine {
                            spans: vec![StyledSpan {
                                text: format!("[{}: last {} rows]", label, rows.len()),
                                style: Style::ToolBracket,
                            }],
                        });

                        if truncated {
                            lines.push(StyledLine {
                                spans: vec![StyledSpan {
                                    text: format!("[{} more rows ...]", total_rows - rows.len()),
                                    style: Style::ToolBracket,
                                }],
                            });
                        }

                        for row in rows {
                            lines.push(StyledLine {
                                spans: vec![StyledSpan {
                                    text: row,
                                    style: Style::ToolOutput,
                                }],
                            });
                        }
                    }
                }

                // Show truncated output preview when done (combine stdout + stderr)
                if let Some(res) = result
                    && let Some(data) = res.data()
                {
                    let stdout = data.get("stdout").and_then(|v| v.as_str()).unwrap_or("");
                    let stderr = data.get("stderr").and_then(|v| v.as_str()).unwrap_or("");
                    let all_lines: Vec<&str> = stdout.lines().chain(stderr.lines()).collect();
                    let max_preview_lines = 5;
                    let truncated = all_lines.len() > max_preview_lines;

                    // Show truncation indicator first (before the last lines)
                    if truncated {
                        lines.push(StyledLine {
                            spans: vec![StyledSpan {
                                text: format!(
                                    "[{} more lines ...]",
                                    all_lines.len() - max_preview_lines
                                ),
                                style: Style::ToolBracket,
                            }],
                        });
                    }

                    // Show the last N lines (most recent output is usually most relevant)
                    let skip_count = all_lines.len().saturating_sub(max_preview_lines);
                    for line in all_lines.iter().skip(skip_count) {
                        // Sanitize line for display (strips ANSI escapes, expands tabs)
                        let safe_line = sanitize_for_display(line);

                        // Check if line needs wrapping
                        let wrapped: Vec<String> = if safe_line.width() > width {
                            wrap_chars(&safe_line, width)
                        } else {
                            vec![safe_line.into_owned()]
                        };

                        for wrapped_line in wrapped {
                            lines.push(StyledLine {
                                spans: vec![StyledSpan {
                                    text: wrapped_line,
                                    style: Style::ToolOutput,
                                }],
                            });
                        }
                    }

                    // Show tool-level truncation warnings (when the tool itself truncated output)
                    let mut truncation_warnings = Vec::new();

                    // Check for Bash tool truncation (stdout_truncated, stderr_truncated)
                    let stdout_truncated = data
                        .get("stdout_truncated")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false);
                    let stderr_truncated = data
                        .get("stderr_truncated")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false);

                    if stdout_truncated {
                        let total = data
                            .get("stdout_total_bytes")
                            .and_then(|v| v.as_u64())
                            .unwrap_or(0);
                        truncation_warnings.push(format_byte_truncation("stdout", total));
                    }
                    if stderr_truncated {
                        let total = data
                            .get("stderr_total_bytes")
                            .and_then(|v| v.as_u64())
                            .unwrap_or(0);
                        truncation_warnings.push(format_byte_truncation("stderr", total));
                    }

                    // Check for Read tool truncation (truncated, total_lines, lines_shown)
                    let file_truncated = data
                        .get("truncated")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false);
                    if file_truncated {
                        let byte_limited = data
                            .get("byte_limited")
                            .and_then(|v| v.as_bool())
                            .unwrap_or(false);
                        let read_has_explicit_limit =
                            name == "read" && input.get("limit").and_then(extract_u64).is_some();
                        let should_warn =
                            name != "read" || byte_limited || !read_has_explicit_limit;

                        if should_warn {
                            let total_lines = data
                                .get("total_lines")
                                .and_then(|v| v.as_u64())
                                .unwrap_or(0);
                            let lines_shown = data
                                .get("lines_shown")
                                .and_then(|v| v.as_u64())
                                .unwrap_or(0);
                            truncation_warnings.push(format!(
                                "file truncated: showing {} of {} lines",
                                lines_shown, total_lines
                            ));
                        }
                    }

                    // Display truncation warnings
                    for warning in truncation_warnings {
                        lines.push(StyledLine {
                            spans: vec![StyledSpan {
                                text: format!("[⚠ {}]", warning),
                                style: Style::ToolTruncation,
                            }],
                        });
                    }
                }

                // Status line (only show when failed - spinner indicates running)
                if *state == ToolState::Error {
                    if let Some(res) = result {
                        if let Some((code, message, details)) = res.error_info() {
                            // Show error code and message
                            lines.push(StyledLine {
                                spans: vec![StyledSpan {
                                    text: format!("Error [{}]: {}", code, message),
                                    style: Style::ToolError,
                                }],
                            });

                            // Show additional details if available
                            if let Some(detail_text) = details {
                                // Split details by newlines and wrap each line
                                for detail_line in detail_text.lines() {
                                    let wrapped = wrap_text(detail_line, width.saturating_sub(2));
                                    for line in wrapped {
                                        lines.push(StyledLine {
                                            spans: vec![StyledSpan {
                                                text: format!("  {}", line),
                                                style: Style::ToolOutput,
                                            }],
                                        });
                                    }
                                }
                            }
                        }
                    } else {
                        // Fallback if result is somehow missing
                        lines.push(StyledLine {
                            spans: vec![StyledSpan {
                                text: "Failed".to_string(),
                                style: Style::ToolError,
                            }],
                        });
                    }
                }

                lines
            }
            HistoryCell::System { content, .. } => {
                let prefix = "System: ";
                render_prefixed_content(
                    prefix,
                    content,
                    width,
                    Style::SystemPrefix,
                    Style::System,
                    false,
                )
            }
            HistoryCell::Thinking {
                content,
                is_streaming,
                is_interrupted,
                ..
            } => {
                let prefix = "Thinking: ";
                let mut lines = render_prefixed_content(
                    prefix,
                    content,
                    width,
                    Style::ThinkingPrefix,
                    Style::Thinking,
                    false,
                );

                // Add streaming indicator if still streaming
                if *is_streaming
                    && !content.is_empty()
                    && let Some(last) = lines.last_mut()
                {
                    last.spans.push(StyledSpan {
                        text: "▌".to_string(),
                        style: Style::StreamingCursor,
                    });
                }

                // Append interrupted indicator to last line
                if *is_interrupted && let Some(last) = lines.last_mut() {
                    last.spans.push(StyledSpan {
                        text: " (interrupted)".to_string(),
                        style: Style::Interrupted,
                    });
                }
                lines
            }
            HistoryCell::Timing {
                duration,
                tool_count,
                ..
            } => {
                // Format duration for display
                let secs = duration.as_secs_f64();
                let duration_str = if secs >= 60.0 {
                    let mins = (secs / 60.0).floor() as u64;
                    let remaining_secs = secs % 60.0;
                    format!("{}m{:.1}s", mins, remaining_secs)
                } else {
                    format!("{:.1}s", secs)
                };

                // Format tool count
                let tool_str = if *tool_count == 1 {
                    "1 tool".to_string()
                } else {
                    format!("{} tools", tool_count)
                };

                let message = format!("{} · {}", tool_str, duration_str);

                // Build centered separator line: ─── 3 tools · 3.5s ───
                let text_with_padding = format!(" {} ", message);
                let text_width = text_with_padding.chars().count();
                let remaining = width.saturating_sub(text_width);
                let left_dashes = remaining / 2;
                let right_dashes = remaining - left_dashes;

                let line = format!(
                    "{}{}{}",
                    "─".repeat(left_dashes),
                    text_with_padding,
                    "─".repeat(right_dashes)
                );

                vec![StyledLine {
                    spans: vec![StyledSpan {
                        text: line,
                        style: Style::Timing,
                    }],
                }]
            }
        }
    }

    /// Returns whether this cell's display output can be cached.
    ///
    /// Cells with dynamic content (streaming, running tools with spinners)
    /// should not be cached since they change every frame.
    pub fn is_cacheable(&self) -> bool {
        match self {
            HistoryCell::User { .. } => true,
            HistoryCell::Assistant { is_streaming, .. } => !*is_streaming,
            HistoryCell::Tool { state, .. } => *state != ToolState::Running,
            HistoryCell::System { .. } => true,
            HistoryCell::Thinking { is_streaming, .. } => !*is_streaming,
            HistoryCell::Timing { .. } => true,
        }
    }

    /// Returns a discriminator for cache key computation.
    ///
    /// This is used to invalidate cache entries when content or state changes.
    /// The value must change when the rendered output would change.
    pub fn content_len(&self) -> usize {
        match self {
            HistoryCell::User {
                content,
                is_interrupted,
                ..
            } => {
                // Include is_interrupted in discriminator to invalidate cache when marked
                content.len() + if *is_interrupted { 1 } else { 0 }
            }
            HistoryCell::Assistant {
                content,
                is_interrupted,
                ..
            } => {
                // Include is_interrupted in discriminator
                content.len() + if *is_interrupted { 1 } else { 0 }
            }
            HistoryCell::Tool { result, .. } => {
                // Use result presence as cache discriminator
                if result.is_some() { 1 } else { 0 }
            }
            HistoryCell::System { content, .. } => content.len(),
            HistoryCell::Thinking {
                content,
                is_interrupted,
                ..
            } => {
                // Include is_interrupted in discriminator
                content.len() + if *is_interrupted { 1 } else { 0 }
            }
            HistoryCell::Timing { duration, .. } => {
                // Duration doesn't change, use millis as discriminator
                duration.as_millis() as usize
            }
        }
    }

    /// Renders this cell into display lines, using cache when possible.
    ///
    /// This is the preferred method for rendering in the TUI loop.
    /// It caches the output for static cells to avoid recomputation.
    pub fn display_lines_cached(
        &self,
        width: usize,
        spinner_frame: usize,
        cache: &WrapCache,
    ) -> Vec<StyledLine> {
        // Skip cache for dynamic cells
        if !self.is_cacheable() {
            return self.display_lines(width, spinner_frame);
        }

        let cell_id = self.id();
        let content_len = self.content_len();

        // Check cache
        if let Some(cached) = cache.get(cell_id, width, content_len) {
            return cached;
        }

        // Compute and cache
        let lines = self.display_lines(width, spinner_frame);
        cache.insert(cell_id, width, content_len, lines.clone());
        lines
    }
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
        assert_eq!(lines[0].spans[0].text, "│ ");
        assert_eq!(lines[0].spans[1].text, "Hello, world!");
    }

    #[test]
    fn test_user_cell_wrapping() {
        let cell = HistoryCell::user("This is a longer message that should wrap");
        let lines = cell.display_lines(25, 0);

        // "│ " is 1 char (3 bytes) + space = 2 display columns, leaving 23 for content
        assert!(lines.len() > 1, "Should wrap to multiple lines");

        // First line has prefix
        assert_eq!(lines[0].spans[0].text, "│ ");

        // Continuation lines also have the prefix (not just spaces)
        assert_eq!(lines[1].spans[0].text, "│ ");
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
        // Should have spinner prefix (first frame is ◐)
        assert!(first_line.starts_with("◐"));

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
            HistoryCell::tool_running("123", "read", serde_json::json!({ "path": "test.txt" }));
        cell.set_tool_result(ToolOutput::failure("not_found", "File not found", None));

        let lines = cell.display_lines(80, 0);
        // Should have error line with code and message
        let all_text: String = lines
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| s.text.as_str()))
            .collect();

        // New format: "Error [not_found]: File not found"
        assert!(all_text.contains("Error"));
        assert!(all_text.contains("not_found"));
        assert!(all_text.contains("File not found"));

        // State should be Error
        match cell {
            HistoryCell::Tool { state, .. } => assert_eq!(state, ToolState::Error),
            _ => panic!("Expected tool cell"),
        }
    }

    #[test]
    fn test_tool_canceled() {
        let mut cell =
            HistoryCell::tool_running("123", "bash", serde_json::json!({"command": "sleep 10"}));
        cell.set_tool_result(ToolOutput::canceled("Interrupted by user"));

        let lines = cell.display_lines(80, 0);
        // Should show "(interrupted)" suffix, not "Failed"
        let all_text: String = lines
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| s.text.as_str()))
            .collect();
        assert!(all_text.contains("(interrupted)"));
        assert!(!all_text.contains("Failed"));

        // State should be Cancelled, not Error
        match cell {
            HistoryCell::Tool { state, .. } => assert_eq!(state, ToolState::Cancelled),
            _ => panic!("Expected tool cell"),
        }
    }

    #[test]
    fn test_bash_command_wrapping() {
        // Long bash command that exceeds 30 columns
        let long_cmd = "cd /Users/test/project && gemini \"Review the implementation\" -m model";
        let cell =
            HistoryCell::tool_running("123", "bash", serde_json::json!({"command": long_cmd}));
        let lines = cell.display_lines(30, 0);

        // Should wrap to multiple lines
        assert!(
            lines.len() > 1,
            "Long bash command should wrap, got {} lines",
            lines.len()
        );

        // First line should have spinner prefix
        assert_eq!(lines[0].spans[0].text, "◐ ");
        assert_eq!(lines[0].spans[0].style, Style::ToolRunning);

        // Continuation lines should have indent (2 spaces to match prefix width)
        assert_eq!(lines[1].spans[0].text, "  ");
        assert_eq!(lines[1].spans[0].style, Style::Plain);

        // All content should be present when joined
        let all_content: String = lines
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| s.text.as_str()))
            .collect();
        assert!(
            all_content.contains("cd"),
            "Should contain start of command"
        );
        assert!(
            all_content.contains("model"),
            "Should contain end of command"
        );
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
        // All lines have the vertical bar prefix
        assert_eq!(lines[0].spans[0].text, "│ ");
        assert_eq!(lines[1].spans[0].text, "│ ");
        assert_eq!(lines[2].spans[0].text, "│ ");
    }

    #[test]
    fn test_empty_content() {
        let cell = HistoryCell::user("");
        let lines = cell.display_lines(80, 0);

        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].spans[0].text, "│ ");
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

    #[test]
    fn test_thinking_streaming() {
        let cell = HistoryCell::thinking_streaming("Analyzing...");
        let lines = cell.display_lines(80, 0);

        // Should have thinking prefix
        assert!(!lines.is_empty());
        assert_eq!(lines[0].spans[0].text, "Thinking: ");
        assert_eq!(lines[0].spans[0].style, Style::ThinkingPrefix);

        // Should have streaming cursor
        let last_line = lines.last().unwrap();
        let last_span = last_line.spans.last().unwrap();
        assert_eq!(last_span.text, "▌");
        assert_eq!(last_span.style, Style::StreamingCursor);
    }

    #[test]
    fn test_thinking_final() {
        let mut cell = HistoryCell::thinking_streaming("Done thinking.");
        cell.finalize_thinking(Some(ReplayToken::Anthropic {
            signature: "signature123".to_string(),
        }));
        let lines = cell.display_lines(80, 0);

        // Should NOT have streaming cursor
        let last_line = lines.last().unwrap();
        let last_span = last_line.spans.last().unwrap();
        assert_ne!(last_span.text, "▌");

        // Verify signature is stored
        match &cell {
            HistoryCell::Thinking {
                is_streaming,
                replay,
                ..
            } => {
                assert!(!*is_streaming);
                assert!(matches!(
                    replay,
                    Some(ReplayToken::Anthropic { signature })
                        if signature == "signature123"
                ));
            }
            _ => panic!("Expected thinking cell"),
        }
    }

    #[test]
    fn test_append_thinking_delta() {
        let mut cell = HistoryCell::thinking_streaming("");
        cell.append_thinking_delta("First ");
        cell.append_thinking_delta("second");

        match &cell {
            HistoryCell::Thinking {
                content,
                is_streaming,
                ..
            } => {
                assert_eq!(content, "First second");
                assert!(*is_streaming);
            }
            _ => panic!("Expected thinking cell"),
        }
    }

    #[test]
    fn test_thinking_cell_content_style() {
        let cell = HistoryCell::thinking_streaming("Deep analysis");
        let lines = cell.display_lines(80, 0);

        // Content should use Thinking style (dim/italic)
        assert!(lines[0].spans.len() >= 2);
        assert_eq!(lines[0].spans[1].style, Style::Thinking);
    }

    #[test]
    fn test_thinking_prefix_width() {
        // The thinking prefix "Thinking: " is 10 characters
        // This test ensures the prefix width is calculated correctly
        let cell = HistoryCell::thinking_streaming("x");
        let lines = cell.display_lines(20, 0);

        // Should have prefix + content on first line
        assert!(!lines.is_empty());
        assert_eq!(lines[0].spans[0].text, "Thinking: ");
    }

    #[test]
    fn test_user_prefix_alignment_with_unicode() {
        // User prefix "│ " is 4 bytes (3 for │ + 1 for space) and 2 display columns
        // All lines should have the prefix (not just indentation)
        let cell = HistoryCell::user("First line\nSecond line");
        let lines = cell.display_lines(80, 0);

        assert_eq!(lines.len(), 2);
        // All lines have "│ " prefix
        assert_eq!(lines[0].spans[0].text, "│ ");
        assert_eq!(lines[1].spans[0].text, "│ ");
    }

    // ========================================================================
    // Wrap cache tests
    // ========================================================================

    #[test]
    fn test_wrap_cache_basic() {
        let cache = WrapCache::new();
        let cell = HistoryCell::user("Hello world");

        // First call should compute and cache
        let lines1 = cell.display_lines_cached(80, 0, &cache);
        // Second call should return cached
        let lines2 = cell.display_lines_cached(80, 0, &cache);

        assert_eq!(lines1, lines2);
    }

    #[test]
    fn test_wrap_cache_different_widths() {
        let cache = WrapCache::new();
        let cell = HistoryCell::user("Hello world this is a test");

        // Different widths should cache separately
        let lines_wide = cell.display_lines_cached(80, 0, &cache);
        let lines_narrow = cell.display_lines_cached(20, 0, &cache);

        // Narrow should have more lines due to wrapping
        assert!(lines_narrow.len() > lines_wide.len());
    }

    #[test]
    fn test_wrap_cache_streaming_not_cached() {
        let cache = WrapCache::new();
        let cell = HistoryCell::assistant_streaming("Still typing...");

        // Streaming cells should not be cached (is_cacheable returns false)
        assert!(!cell.is_cacheable());

        // Should still work, just not cached
        let lines = cell.display_lines_cached(80, 0, &cache);
        assert!(!lines.is_empty());
    }

    #[test]
    fn test_wrap_cache_finalized_cached() {
        let cache = WrapCache::new();
        let mut cell = HistoryCell::assistant_streaming("Done");
        cell.finalize_assistant();

        // Finalized cells should be cacheable
        assert!(cell.is_cacheable());

        let lines1 = cell.display_lines_cached(80, 0, &cache);
        let lines2 = cell.display_lines_cached(80, 0, &cache);
        assert_eq!(lines1, lines2);
    }

    #[test]
    fn test_wrap_cache_clear() {
        let cache = WrapCache::new();
        let cell = HistoryCell::user("Hello");

        // Populate cache
        let _ = cell.display_lines_cached(80, 0, &cache);

        // Clear should remove all entries
        cache.clear();

        // Cache should be empty (we can't directly check, but behavior should be correct)
        // This mainly tests that clear() doesn't panic
    }

    #[test]
    fn test_is_cacheable() {
        // User cells are always cacheable
        assert!(HistoryCell::user("test").is_cacheable());

        // System cells are always cacheable
        assert!(HistoryCell::system("test").is_cacheable());

        // Streaming assistant is not cacheable
        assert!(!HistoryCell::assistant_streaming("test").is_cacheable());

        // Finalized assistant is cacheable
        let mut assistant = HistoryCell::assistant_streaming("test");
        assistant.finalize_assistant();
        assert!(assistant.is_cacheable());

        // Running tool is not cacheable (has spinner)
        assert!(!HistoryCell::tool_running("id", "bash", serde_json::json!({})).is_cacheable());

        // Completed tool is cacheable
        let mut tool = HistoryCell::tool_running("id", "bash", serde_json::json!({}));
        tool.set_tool_result(ToolOutput::success(serde_json::json!({})));
        assert!(tool.is_cacheable());

        // Streaming thinking is not cacheable
        assert!(!HistoryCell::thinking_streaming("test").is_cacheable());

        // Finalized thinking is cacheable
        let mut thinking = HistoryCell::thinking_streaming("test");
        thinking.finalize_thinking(Some(ReplayToken::Anthropic {
            signature: "sig".to_string(),
        }));
        assert!(thinking.is_cacheable());
    }

    #[test]
    fn test_thinking_multiline_prefix_behavior() {
        let cell = HistoryCell::thinking_streaming("Line 1\nLine 2\nLine 3");
        let lines = cell.display_lines(80, 0);

        // Should have 3 content lines
        assert_eq!(lines.len(), 3, "Expected 3 lines");

        // Debug: print what we actually get
        for (i, line) in lines.iter().enumerate() {
            let texts: Vec<&str> = line.spans.iter().map(|s| s.text.as_str()).collect();
            eprintln!("Line {}: {:?}", i, texts);
        }

        // First line should have "Thinking:" prefix
        assert_eq!(lines[0].spans[0].text, "Thinking: ");

        // Second and third lines should have spaces (indentation), NOT the prefix
        // "Thinking: " is 10 characters
        assert_eq!(
            lines[1].spans[0].text, "          ",
            "Second line should be indented, not prefixed"
        );
        assert_eq!(
            lines[2].spans[0].text, "          ",
            "Third line should be indented, not prefixed"
        );
    }

    #[test]
    fn test_thinking_with_blank_lines() {
        // Test thinking with blank lines between paragraphs
        let cell = HistoryCell::thinking_streaming("Para 1\n\nPara 2\n\nPara 3");
        let lines = cell.display_lines(80, 0);

        eprintln!("\n=== Thinking with blank lines ===");
        for (i, line) in lines.iter().enumerate() {
            let texts: Vec<&str> = line.spans.iter().map(|s| s.text.as_str()).collect();
            eprintln!("Line {}: {:?}", i, texts);
        }

        // Should have 5 lines: Para1, blank, Para2, blank, Para3
        assert_eq!(lines.len(), 5, "Expected 5 lines");

        // Only first line should have "Thinking:" prefix
        assert_eq!(lines[0].spans[0].text, "Thinking: ");

        // All other lines (including blank lines) should have indentation
        for (i, _) in lines.iter().enumerate().skip(1) {
            assert_eq!(
                lines[i].spans[0].text, "          ",
                "Line {} should be indented, not prefixed",
                i
            );
        }
    }

    #[test]
    fn test_tool_output_wrapping_correctness() {
        // Create a tool cell with a very long output line
        let long_line = "a".repeat(100); // 100 chars
        let mut cell =
            HistoryCell::tool_running("1", "bash", serde_json::json!({"command": "echo long"}));

        cell.set_tool_result(ToolOutput::success(serde_json::json!({
            "stdout": long_line,
            "stderr": ""
        })));

        // Request display with narrow width (e.g., 20 chars)
        let width = 20;
        let lines = cell.display_lines(width, 0);

        // Verify that no line exceeds the display width
        for (i, line) in lines.iter().enumerate() {
            let line_text: String = line.spans.iter().map(|s| s.text.as_str()).collect();
            let line_width = line_text.width();

            assert!(
                line_width <= width,
                "Line {} width {} exceeds limit {}",
                i,
                line_width,
                width
            );
        }
    }

    // ========================================================================
    // Truncation warning display tests
    // ========================================================================

    #[test]
    fn test_format_byte_truncation_sizes() {
        assert_eq!(
            format_byte_truncation("stdout", 512),
            "stdout truncated: 512 bytes total"
        );
        assert_eq!(
            format_byte_truncation("stdout", 51200),
            "stdout truncated: 50.0 KB total"
        );
        assert_eq!(
            format_byte_truncation("stderr", 1048576),
            "stderr truncated: 1.0 MB total"
        );
    }

    #[test]
    fn test_tool_bash_truncation_warnings_displayed() {
        let mut cell =
            HistoryCell::tool_running("1", "bash", serde_json::json!({"command": "cat bigfile"}));

        // Simulate a bash tool result with truncated stdout + stderr
        cell.set_tool_result(ToolOutput::success(serde_json::json!({
            "stdout": "truncated output...",
            "stderr": "error output...",
            "exit_code": 1,
            "timed_out": false,
            "stdout_truncated": true,
            "stderr_truncated": true,
            "stdout_total_bytes": 102400,
            "stderr_total_bytes": 1048576
        })));

        let lines = cell.display_lines(80, 0);
        let all_text: String = lines
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| s.text.as_str()))
            .collect();

        // Should show truncation warnings with sizes
        assert!(
            all_text.contains("stdout truncated"),
            "Expected stdout truncation warning, got: {}",
            all_text
        );
        assert!(
            all_text.contains("100.0 KB total"),
            "Expected stdout size info"
        );
        assert!(
            all_text.contains("stderr truncated"),
            "Expected stderr truncation warning"
        );
        assert!(
            all_text.contains("1.0 MB total"),
            "Expected stderr size info"
        );
    }

    #[test]
    fn test_tool_read_truncation_warning_displayed() {
        let mut cell =
            HistoryCell::tool_running("1", "read", serde_json::json!({"path": "large.txt"}));

        // Simulate a read tool result with truncated file
        cell.set_tool_result(ToolOutput::success(serde_json::json!({
            "path": "large.txt",
            "content": "first 2000 lines...",
            "offset": 1,
            "lines_shown": 2000,
            "total_lines": 5000,
            "truncated": true
        })));

        let lines = cell.display_lines(80, 0);
        let all_text: String = lines
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| s.text.as_str()))
            .collect();

        // Should show file truncation warning
        assert!(
            all_text.contains("file truncated"),
            "Expected file truncation warning, got: {}",
            all_text
        );
        assert!(
            all_text.contains("showing 2000 of 5000 lines"),
            "Expected line counts"
        );
    }

    #[test]
    fn test_tool_read_explicit_limit_no_truncation_warning() {
        let mut cell = HistoryCell::tool_running(
            "1",
            "read",
            serde_json::json!({"path": "large.txt", "limit": 240}),
        );

        cell.set_tool_result(ToolOutput::success(serde_json::json!({
            "path": "large.txt",
            "content": "first 240 lines...",
            "offset": 1,
            "lines_shown": 240,
            "total_lines": 342,
            "truncated": true,
            "byte_limited": false
        })));

        let lines = cell.display_lines(80, 0);
        let all_text: String = lines
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| s.text.as_str()))
            .collect();

        assert!(
            !all_text.contains("file truncated"),
            "Should not show file truncation warning when limit is explicit"
        );
    }

    #[test]
    fn test_tool_no_truncation_no_warning() {
        let mut cell =
            HistoryCell::tool_running("1", "bash", serde_json::json!({"command": "echo hi"}));

        // Simulate bash result without truncation
        cell.set_tool_result(ToolOutput::success(serde_json::json!({
            "stdout": "hi\n",
            "stderr": "",
            "exit_code": 0,
            "timed_out": false,
            "stdout_truncated": false,
            "stderr_truncated": false,
            "stdout_total_bytes": 3,
            "stderr_total_bytes": 0
        })));

        let lines = cell.display_lines(80, 0);
        let all_text: String = lines
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| s.text.as_str()))
            .collect();

        // Should NOT show any truncation warning
        assert!(
            !all_text.contains("truncated"),
            "Should not show truncation warning for non-truncated output"
        );
    }

    #[test]
    fn test_truncation_warning_style() {
        let mut cell =
            HistoryCell::tool_running("1", "bash", serde_json::json!({"command": "big"}));

        cell.set_tool_result(ToolOutput::success(serde_json::json!({
            "stdout": "x",
            "stderr": "",
            "exit_code": 0,
            "timed_out": false,
            "stdout_truncated": true,
            "stderr_truncated": false,
            "stdout_total_bytes": 51200,
            "stderr_total_bytes": 0
        })));

        let lines = cell.display_lines(80, 0);

        // Find the line with the truncation warning
        let truncation_line = lines
            .iter()
            .find(|l| l.spans.iter().any(|s| s.text.contains("stdout truncated")));

        assert!(
            truncation_line.is_some(),
            "Should have truncation warning line"
        );

        // Verify the style is ToolTruncation
        let span = truncation_line
            .unwrap()
            .spans
            .iter()
            .find(|s| s.text.contains("stdout truncated"))
            .unwrap();
        assert_eq!(span.style, Style::ToolTruncation);
    }

    #[test]
    fn test_timing_cell_display() {
        use std::time::Duration;

        // Test sub-minute duration with 1 tool
        let cell = HistoryCell::timing(Duration::from_secs_f64(3.5), 1);
        let lines = cell.display_lines(40, 0);

        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].spans.len(), 1);
        // Should contain the text centered with dashes
        assert!(lines[0].spans[0].text.contains("1 tool"));
        assert!(lines[0].spans[0].text.contains("3.5s"));
        assert!(lines[0].spans[0].text.starts_with("─"));
        assert!(lines[0].spans[0].text.ends_with("─"));
        assert_eq!(lines[0].spans[0].style, Style::Timing);
    }

    #[test]
    fn test_timing_cell_display_multiple_tools() {
        use std::time::Duration;

        // Test with multiple tools
        let cell = HistoryCell::timing(Duration::from_secs_f64(5.0), 3);
        let lines = cell.display_lines(40, 0);

        assert_eq!(lines.len(), 1);
        assert!(lines[0].spans[0].text.contains("3 tools"));
        assert!(lines[0].spans[0].text.contains("5.0s"));
    }

    #[test]
    fn test_timing_cell_display_minutes() {
        use std::time::Duration;

        // Test duration over a minute
        let cell = HistoryCell::timing(Duration::from_secs_f64(125.3), 2);
        let lines = cell.display_lines(40, 0);

        assert_eq!(lines.len(), 1);
        assert!(lines[0].spans[0].text.contains("2m5.3s"));
        assert!(lines[0].spans[0].text.contains("2 tools"));
    }

    #[test]
    fn test_timing_cell_cacheable() {
        use std::time::Duration;

        let cell = HistoryCell::timing(Duration::from_secs(5), 1);
        assert!(cell.is_cacheable());
    }
}
