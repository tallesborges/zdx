//! Transcript model for TUI rendering.
//!
//! This module defines the transcript types that form the source of truth
//! for the TUI. The transcript is width-agnostic; wrapping happens at
//! display time for the current terminal width.
//!
//! See SPEC.md §9 for the contract.

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};

use chrono::{DateTime, Utc};
use serde_json::Value;
use unicode_width::UnicodeWidthStr;

use crate::core::events::ToolOutput;

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
    fn get(&self, cell_id: CellId, width: usize, content_len: usize) -> Option<Vec<StyledLine>> {
        self.cache
            .borrow()
            .get(&(cell_id, width, content_len))
            .cloned()
    }

    /// Stores wrapped lines in the cache.
    fn insert(&self, cell_id: CellId, width: usize, content_len: usize, lines: Vec<StyledLine>) {
        self.cache
            .borrow_mut()
            .insert((cell_id, width, content_len), lines);
    }
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
    /// During streaming, `content` accumulates deltas and `signature` is None.
    /// When finalized, `signature` is set (required for API continuity).
    /// `is_interrupted` indicates if streaming was cancelled by user.
    Thinking {
        id: CellId,
        created_at: DateTime<Utc>,
        content: String,
        /// Cryptographic signature from the API (None while streaming).
        signature: Option<String>,
        is_streaming: bool,
        is_interrupted: bool,
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
            signature: None,
            is_streaming: true,
            is_interrupted: false,
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

    /// Finalizes a thinking cell with its signature.
    ///
    /// Panics if called on a non-thinking cell.
    pub fn finalize_thinking(&mut self, sig: String) {
        match self {
            HistoryCell::Thinking {
                is_streaming,
                signature,
                ..
            } => {
                *is_streaming = false;
                *signature = Some(sig);
            }
            _ => panic!("finalize_thinking called on non-thinking cell"),
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
                let prefix = "| ";
                let mut lines =
                    render_prefixed_content(prefix, content, width, Style::UserPrefix, Style::User);

                // Append interrupted indicator to last line if request was cancelled
                if *is_interrupted {
                    if let Some(last) = lines.last_mut() {
                        last.spans.push(StyledSpan {
                            text: " (interrupted)".to_string(),
                            style: Style::Interrupted,
                        });
                    }
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
                use crate::ui::markdown::render_markdown;

                let mut lines = render_markdown(content, width);

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

                // Append interrupted indicator to last line
                if *is_interrupted {
                    if let Some(last) = lines.last_mut() {
                        last.spans.push(StyledSpan {
                            text: " (interrupted)".to_string(),
                            style: Style::Interrupted,
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
                        Some(" (interrupted)"),
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
                                style: Style::Interrupted,
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
                            style: Style::Interrupted,
                        });
                    }
                    lines.push(StyledLine { spans });
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
                        lines.push(StyledLine {
                            spans: vec![StyledSpan {
                                text: (*line).to_string(),
                                style: Style::ToolOutput,
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
                if *is_interrupted {
                    if let Some(last) = lines.last_mut() {
                        last.spans.push(StyledSpan {
                            text: " (interrupted)".to_string(),
                            style: Style::Interrupted,
                        });
                    }
                }
                lines
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
    /// Note: Not currently used since markdown rendering handles assistant output.
    #[allow(dead_code)]
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
    /// Tool cancelled/interrupted command style.
    ToolCancelled,
    /// Tool output (stdout from bash, etc).
    ToolOutput,
    /// Interrupted suffix indicator (dim).
    Interrupted,
    /// Thinking block prefix ("Thinking: ").
    ThinkingPrefix,
    /// Thinking block content (dim/italic).
    Thinking,

    // Markdown styles
    /// Inline code (`code`).
    CodeInline,
    /// Fenced code block content.
    CodeBlock,
    /// Emphasized text (*italic*).
    Emphasis,
    /// Strong text (**bold**).
    Strong,
    /// Heading level 1 (# Heading).
    H1,
    /// Heading level 2 (## Heading).
    H2,
    /// Heading level 3+ (### Heading).
    H3,
    /// Link text.
    Link,
    /// Blockquote content.
    BlockQuote,
    /// List bullet marker.
    ListBullet,
    /// List number marker.
    ListNumber,
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
    // Use display width for prefix
    let prefix_display_width = prefix.width();

    // Minimum usable width
    let min_width = prefix_display_width + 10;
    let effective_width = width.max(min_width);

    // Content width after prefix/indent
    let content_width = effective_width.saturating_sub(prefix_display_width);

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
fn wrap_text(text: &str, width: usize) -> Vec<String> {
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
                let mut broken = break_word_by_width(word, width);
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
                let mut broken = break_word_by_width(word, width);
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

/// Breaks a word into parts that fit within the given display width.
///
/// Used when a single word is wider than the available width.
/// Breaks at character boundaries, respecting display width.
fn break_word_by_width(word: &str, width: usize) -> Vec<String> {
    use unicode_width::UnicodeWidthChar;

    let mut parts = Vec::new();
    let mut current = String::new();
    let mut current_width: usize = 0;

    for ch in word.chars() {
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
        cell.finalize_thinking("signature123".to_string());
        let lines = cell.display_lines(80, 0);

        // Should NOT have streaming cursor
        let last_line = lines.last().unwrap();
        let last_span = last_line.spans.last().unwrap();
        assert_ne!(last_span.text, "▌");

        // Verify signature is stored
        match &cell {
            HistoryCell::Thinking {
                is_streaming,
                signature,
                ..
            } => {
                assert!(!*is_streaming);
                assert_eq!(signature.as_deref(), Some("signature123"));
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
    fn test_break_word_by_width_cjk() {
        // Breaking a long CJK word
        let parts = break_word_by_width("你好世界很长", 4);
        // Each part should be at most 4 columns (2 CJK chars)
        assert_eq!(parts.len(), 3);
        assert_eq!(parts[0], "你好");
        assert_eq!(parts[1], "世界");
        assert_eq!(parts[2], "很长");
    }

    #[test]
    fn test_break_word_by_width_emoji() {
        let parts = break_word_by_width("🎉🎊🎁🎄", 4);
        // Each emoji is 2 columns, so 2 per line
        assert_eq!(parts.len(), 2);
        assert_eq!(parts[0], "🎉🎊");
        assert_eq!(parts[1], "🎁🎄");
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
        // User prefix "| " is 2 bytes and 2 columns
        // Continuation lines should align correctly
        let cell = HistoryCell::user("First line\nSecond line");
        let lines = cell.display_lines(80, 0);

        assert_eq!(lines.len(), 2);
        // First line has "| " prefix
        assert_eq!(lines[0].spans[0].text, "| ");
        // Second line has 2-space indent for alignment
        assert_eq!(lines[1].spans[0].text, "  ");
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
        thinking.finalize_thinking("sig".to_string());
        assert!(thinking.is_cacheable());
    }
}
