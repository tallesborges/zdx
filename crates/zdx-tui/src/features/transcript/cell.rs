#![allow(
    clippy::cast_precision_loss,
    clippy::match_same_arms,
    clippy::too_many_lines,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss
)]

use std::collections::VecDeque;
use std::fmt::Write;
use std::sync::atomic::{AtomicU64, Ordering};

use chrono::{DateTime, Utc};
use serde_json::Value;
use unicode_width::UnicodeWidthStr;
use zdx_core::core::events::ToolOutput;
use zdx_core::providers::ReplayToken;

use super::style::{Style, StyledLine, StyledSpan};
use super::wrap::{WrapCache, render_prefixed_content, wrap_chars, wrap_text};
use crate::common::{sanitize_for_display, truncate_with_ellipsis};

/// Formats a byte truncation warning with human-readable byte counts.
fn format_byte_truncation(stream: &str, total_bytes: u64) -> String {
    let size_str = if total_bytes >= 1024 * 1024 {
        format!("{:.1} MB", total_bytes as f64 / (1024.0 * 1024.0))
    } else if total_bytes >= 1024 {
        format!("{:.1} KB", total_bytes as f64 / 1024.0)
    } else {
        format!("{total_bytes} bytes")
    };
    format!("{stream} truncated: {size_str} total")
}

fn extract_u64(value: &Value) -> Option<u64> {
    match value {
        Value::Number(num) => num.as_u64(),
        Value::String(text) => text.parse::<u64>().ok(),
        _ => None,
    }
}

fn tool_input_delta<'a>(name: &str, input: &'a Value) -> Option<&'a str> {
    match name {
        "write" => input.get("content")?.as_str(),
        "edit" => input.get("new")?.as_str(),
        _ => None,
    }
}

fn value_as_trimmed_str<'a>(input: &'a Value, key: &str) -> Option<&'a str> {
    let value = input.get(key)?.as_str()?.trim();
    (!value.is_empty()).then_some(value)
}

fn value_as_string_list(input: &Value, key: &str) -> Vec<String> {
    match input.get(key) {
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .collect(),
        Some(Value::String(item)) => {
            let item = item.trim();
            if item.is_empty() {
                Vec::new()
            } else {
                vec![item.to_string()]
            }
        }
        _ => Vec::new(),
    }
}

fn format_compact_list(items: &[String], max_items: usize) -> String {
    let displayed: Vec<&str> = items
        .iter()
        .take(max_items)
        .map(std::string::String::as_str)
        .collect();
    let mut summary = displayed.join(", ");
    if items.len() > max_items {
        summary.push_str(", +");
        summary.push_str(&(items.len() - max_items).to_string());
        summary.push_str(" more");
    }
    summary
}

fn summarize_apply_patch_targets(patch: &str) -> Option<String> {
    const ADD_FILE_PREFIX: &str = "*** Add File: ";
    const DELETE_FILE_PREFIX: &str = "*** Delete File: ";
    const UPDATE_FILE_PREFIX: &str = "*** Update File: ";

    let mut targets = Vec::new();

    for line in patch.lines() {
        let line = line.trim();
        let target = if let Some(file_path) = line.strip_prefix(ADD_FILE_PREFIX) {
            let file_path = file_path.trim();
            (!file_path.is_empty()).then(|| format!("+{file_path}"))
        } else if let Some(file_path) = line.strip_prefix(DELETE_FILE_PREFIX) {
            let file_path = file_path.trim();
            (!file_path.is_empty()).then(|| format!("-{file_path}"))
        } else {
            line.strip_prefix(UPDATE_FILE_PREFIX).and_then(|file_path| {
                let file_path = file_path.trim();
                (!file_path.is_empty()).then(|| format!("~{file_path}"))
            })
        };

        if let Some(target) = target
            && !targets.contains(&target)
        {
            targets.push(target);
        }
    }

    if targets.is_empty() {
        None
    } else {
        Some(format_compact_list(&targets, 3))
    }
}

fn tool_display_text(name: &str, input: &Value) -> String {
    match name {
        "bash" => value_as_trimmed_str(input, "command")
            .map_or_else(|| "bash".to_string(), str::to_string),
        "read" | "write" | "edit" => value_as_trimmed_str(input, "path")
            .map_or_else(|| name.to_string(), |path| format!("{name} {path}")),
        "apply_patch" => value_as_trimmed_str(input, "patch")
            .and_then(summarize_apply_patch_targets)
            .map_or_else(|| name.to_string(), |targets| format!("{name} {targets}")),
        "web_search" => {
            let queries = value_as_string_list(input, "search_queries");
            if queries.is_empty() {
                value_as_trimmed_str(input, "objective").map_or_else(
                    || name.to_string(),
                    |objective| format!("{name} {}", truncate_with_ellipsis(objective, 72)),
                )
            } else {
                format!("{name} [{}]", format_compact_list(&queries, 3))
            }
        }
        "fetch_webpage" => value_as_trimmed_str(input, "url")
            .map_or_else(|| name.to_string(), |url| format!("{name} {url}")),
        "read_thread" => value_as_trimmed_str(input, "thread_id").map_or_else(
            || name.to_string(),
            |thread_id| format!("{name} {thread_id}"),
        ),
        "thread_search" => value_as_trimmed_str(input, "query").map_or_else(
            || name.to_string(),
            |query| format!("{name} {}", truncate_with_ellipsis(query, 72)),
        ),
        "invoke_subagent" => value_as_trimmed_str(input, "model")
            .map_or_else(|| name.to_string(), |model| format!("{name} model={model}")),
        _ => name.to_string(),
    }
}

const TOOL_ARG_SUMMARY_MAX_WIDTH: usize = 180;
const TOOL_ARG_INLINE_VALUE_MAX_WIDTH: usize = 72;
const TOOL_ARG_DETAIL_MAX_LINES: usize = 200;
const TOOL_OUTPUT_PREVIEW_MAX_LINES: usize = 5;
const TOOL_OUTPUT_DETAIL_MAX_LINES: usize = 200;
const TOOL_OUTPUT_SUMMARY_MAX_WIDTH: usize = 120;

#[derive(Debug, Clone, Copy)]
enum OutputStrategy {
    Head,
    Tail,
}

#[derive(Debug, Clone)]
struct ToolOutputPayload {
    text: String,
    strategy: OutputStrategy,
}

fn format_tool_arg_inline_value(value: &Value, max_width: usize) -> (String, bool) {
    let raw = serde_json::to_string(value).unwrap_or_else(|_| value.to_string());
    let truncated = raw.width() > max_width;
    (truncate_with_ellipsis(&raw, max_width), truncated)
}

fn sorted_object_entries(input: &Value) -> Option<Vec<(&str, &Value)>> {
    let Value::Object(map) = input else {
        return None;
    };

    let mut entries: Vec<(&str, &Value)> = map.iter().map(|(k, v)| (k.as_str(), v)).collect();
    entries.sort_unstable_by(|(left, _), (right, _)| left.cmp(right));
    Some(entries)
}

fn build_tool_args_summary(input: &Value, max_width: usize) -> (String, bool, bool) {
    let max_width = max_width.max(24);

    if let Some(entries) = sorted_object_entries(input) {
        if entries.is_empty() {
            return ("{}".to_string(), false, false);
        }

        let mut parts = Vec::with_capacity(entries.len());
        let mut has_nested = false;
        let mut value_truncated = false;

        for (key, value) in entries {
            has_nested |= matches!(value, Value::Object(_) | Value::Array(_));
            let (inline_value, truncated) =
                format_tool_arg_inline_value(value, TOOL_ARG_INLINE_VALUE_MAX_WIDTH);
            value_truncated |= truncated;
            parts.push(format!("{key}={inline_value}"));
        }

        let mut summary = String::new();
        let mut summary_truncated = value_truncated;
        for part in parts {
            let sep = if summary.is_empty() { "" } else { ", " };
            let next_width = summary.width() + sep.width() + part.width();
            if next_width <= max_width {
                summary.push_str(sep);
                summary.push_str(&part);
                continue;
            }

            summary_truncated = true;
            if summary.is_empty() {
                summary = truncate_with_ellipsis(&part, max_width);
            } else {
                summary = truncate_with_ellipsis(&format!("{summary}, …"), max_width);
            }
            break;
        }

        return (summary, summary_truncated, has_nested);
    }

    let raw = serde_json::to_string(input).unwrap_or_else(|_| input.to_string());
    let truncated = raw.width() > max_width;
    (
        truncate_with_ellipsis(&raw, max_width),
        truncated,
        matches!(input, Value::Object(_) | Value::Array(_)),
    )
}

fn should_show_tool_arg_details(input: &Value, summary_truncated: bool, has_nested: bool) -> bool {
    if summary_truncated || has_nested {
        return true;
    }

    match input {
        Value::Object(map) => map.len() > 3,
        Value::Array(items) => items.len() > 2,
        _ => false,
    }
}

fn should_render_tool_args(name: &str, input: &Value) -> bool {
    if name != "bash" {
        return true;
    }

    match input {
        Value::Object(map) => !(map.len() == 1 && map.contains_key("command")),
        _ => true,
    }
}

fn render_tool_arg_detail_rows(
    input: &Value,
    width: usize,
    max_rows: usize,
) -> (Vec<String>, usize) {
    let pretty = serde_json::to_string_pretty(input).unwrap_or_else(|_| input.to_string());
    let mut rows = Vec::new();

    for line in pretty.lines() {
        let safe_line = sanitize_for_display(line);
        let wrapped: Vec<String> = if safe_line.width() > width {
            wrap_chars(&safe_line, width)
        } else {
            vec![safe_line.into_owned()]
        };
        rows.extend(wrapped);
    }

    let total_rows = rows.len();
    if rows.len() > max_rows {
        rows.truncate(max_rows);
    }

    (rows, total_rows)
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

fn head_rendered_rows(text: &str, width: usize, max_rows: usize) -> (Vec<String>, usize) {
    let mut total_rows = 0;
    let mut rows = Vec::new();

    for line in text.lines() {
        let safe_line = sanitize_for_display(line);
        let wrapped: Vec<String> = if safe_line.width() > width {
            wrap_chars(&safe_line, width)
        } else {
            vec![safe_line.into_owned()]
        };

        for row in wrapped {
            total_rows += 1;
            if rows.len() < max_rows {
                rows.push(row);
            }
        }
    }

    (rows, total_rows)
}

fn render_tool_output_rows(
    text: &str,
    width: usize,
    max_rows: usize,
    strategy: OutputStrategy,
) -> (Vec<String>, usize) {
    match strategy {
        OutputStrategy::Head => head_rendered_rows(text, width, max_rows),
        OutputStrategy::Tail => tail_rendered_rows(text, width, max_rows),
    }
}

fn format_output_label(_total_rows: usize) -> String {
    "─ output ─ ".to_string()
}

fn extract_stdout_stderr(data: &Value) -> Option<String> {
    let stdout = data.get("stdout").and_then(Value::as_str).unwrap_or("");
    let stderr = data.get("stderr").and_then(Value::as_str).unwrap_or("");

    if stdout.is_empty() && stderr.is_empty() {
        return None;
    }

    let mut combined = String::new();
    if !stdout.is_empty() {
        combined.push_str(stdout);
        if !stdout.ends_with('\n') && !stderr.is_empty() {
            combined.push('\n');
        }
    }
    if !stderr.is_empty() {
        combined.push_str(stderr);
    }

    Some(combined)
}

fn summarize_apply_patch_output(data: &Value) -> Option<String> {
    let applied = data.get("applied")?.as_array()?;
    if applied.is_empty() {
        return None;
    }

    let mut lines = Vec::new();
    for op in applied {
        let op_type = op.get("op").and_then(Value::as_str).unwrap_or("op");
        let path = op
            .get("path")
            .and_then(Value::as_str)
            .unwrap_or("<unknown>");
        let move_path = op.get("move_path").and_then(Value::as_str);
        let chunks = op.get("chunks").and_then(Value::as_u64);
        let bytes = op.get("bytes").and_then(Value::as_u64);

        let line = match op_type {
            "add" => bytes.map_or_else(
                || format!("• add {path}"),
                |bytes| format!("• add {path} ({bytes} bytes)"),
            ),
            "delete" => format!("• delete {path}"),
            "update" => {
                let mut line = format!("• update {path}");
                if let Some(move_path) = move_path {
                    let _ = write!(line, " → {move_path}");
                }
                if chunks.is_some() || bytes.is_some() {
                    line.push_str(" (");
                    if let Some(chunks) = chunks {
                        let _ = write!(line, "{chunks} chunks");
                    }
                    if let Some(bytes) = bytes {
                        if chunks.is_some() {
                            line.push_str(", ");
                        }
                        let _ = write!(line, "{bytes} bytes");
                    }
                    line.push(')');
                }
                line
            }
            _ => format!("• {op_type} {path}"),
        };

        lines.push(line);
    }

    Some(lines.join("\n"))
}

fn summarize_write_edit_output(data: &Value) -> Option<String> {
    let mut lines = Vec::new();

    if let Some(path) = data.get("path").and_then(Value::as_str) {
        lines.push(format!("path: {path}"));
    }
    if let Some(bytes) = data.get("bytes").and_then(Value::as_u64) {
        lines.push(format!("bytes: {bytes}"));
    }
    if let Some(created) = data.get("created").and_then(Value::as_bool) {
        lines.push(format!("created: {created}"));
    }
    if let Some(replacements) = data.get("replacements").and_then(Value::as_u64) {
        lines.push(format!("replacements: {replacements}"));
    }

    if lines.is_empty() {
        None
    } else {
        Some(lines.join("\n"))
    }
}

fn summarize_image_output(data: &Value) -> Option<String> {
    let image_type = data.get("type").and_then(Value::as_str)?;
    if image_type != "image" {
        return None;
    }

    let mime = data
        .get("mime_type")
        .and_then(Value::as_str)
        .unwrap_or("image");
    let bytes = data.get("bytes").and_then(Value::as_u64);
    let path = data.get("path").and_then(Value::as_str);

    let mut line = format!("image: {mime}");
    if let Some(bytes) = bytes {
        let _ = write!(line, " ({bytes} bytes)");
    }
    if let Some(path) = path {
        let _ = write!(line, " — {path}");
    }

    Some(line)
}

fn summarize_results_output(data: &Value) -> Option<String> {
    if let Some(results) = data.get("results").and_then(Value::as_array) {
        let mut lines = Vec::new();

        if let Some(search_id) = data.get("search_id").and_then(Value::as_str) {
            lines.push(format!("search_id: {search_id}"));
        }
        if let Some(extract_id) = data.get("extract_id").and_then(Value::as_str) {
            lines.push(format!("extract_id: {extract_id}"));
        }
        if let Some(warnings) = data.get("warnings").and_then(Value::as_str) {
            let warnings = warnings.trim();
            if !warnings.is_empty() {
                lines.push(format!(
                    "warnings: {}",
                    truncate_with_ellipsis(warnings, TOOL_OUTPUT_SUMMARY_MAX_WIDTH)
                ));
            }
        }

        lines.extend(format_result_lines(results));
        return if lines.is_empty() {
            None
        } else {
            Some(lines.join("\n"))
        };
    }

    if let Some(results) = data.as_array() {
        let lines = format_result_lines(results);
        return if lines.is_empty() {
            None
        } else {
            Some(lines.join("\n"))
        };
    }

    None
}

fn format_result_lines(results: &[Value]) -> Vec<String> {
    let mut lines = Vec::new();

    for result in results {
        let Some(obj) = result.as_object() else {
            if let Some(text) = result.as_str() {
                lines.push(text.to_string());
            }
            continue;
        };

        let title = obj
            .get("title")
            .and_then(Value::as_str)
            .filter(|s| !s.trim().is_empty());
        let thread_id = obj
            .get("thread_id")
            .and_then(Value::as_str)
            .filter(|s| !s.trim().is_empty());
        let url = obj
            .get("url")
            .and_then(Value::as_str)
            .filter(|s| !s.trim().is_empty());

        let label = title.or(thread_id).unwrap_or("result");
        let mut line = format!(
            "• {}",
            truncate_with_ellipsis(label.trim(), TOOL_OUTPUT_SUMMARY_MAX_WIDTH)
        );
        if let Some(url) = url {
            line.push_str(" — ");
            line.push_str(&truncate_with_ellipsis(
                url.trim(),
                TOOL_OUTPUT_SUMMARY_MAX_WIDTH,
            ));
        }
        lines.push(line);

        if let Some(preview) = obj
            .get("preview")
            .and_then(Value::as_str)
            .filter(|s| !s.trim().is_empty())
        {
            lines.push(format!(
                "  {}",
                truncate_with_ellipsis(preview.trim(), TOOL_OUTPUT_SUMMARY_MAX_WIDTH)
            ));
            continue;
        }

        if let Some(excerpts) = obj.get("excerpts").and_then(Value::as_array)
            && let Some(excerpt) = excerpts
                .iter()
                .filter_map(Value::as_str)
                .map(str::trim)
                .find(|text| !text.is_empty())
        {
            lines.push(format!(
                "  {}",
                truncate_with_ellipsis(excerpt, TOOL_OUTPUT_SUMMARY_MAX_WIDTH)
            ));
            continue;
        }

        if let Some(full_content) = obj
            .get("full_content")
            .and_then(Value::as_str)
            .filter(|s| !s.trim().is_empty())
        {
            let snippet = full_content
                .lines()
                .map(str::trim)
                .find(|line| !line.is_empty())
                .unwrap_or(full_content.trim());
            if !snippet.is_empty() {
                lines.push(format!(
                    "  {}",
                    truncate_with_ellipsis(snippet, TOOL_OUTPUT_SUMMARY_MAX_WIDTH)
                ));
            }
        }
    }

    lines
}

fn build_tool_output_payload(name: &str, data: &Value) -> Option<ToolOutputPayload> {
    if let Some(text) = extract_stdout_stderr(data) {
        return Some(ToolOutputPayload {
            text,
            strategy: OutputStrategy::Tail,
        });
    }

    if name == "read"
        && let Some(content) = data.get("content").and_then(Value::as_str)
        && !content.is_empty()
    {
        return Some(ToolOutputPayload {
            text: content.to_string(),
            strategy: OutputStrategy::Head,
        });
    }

    if let Some(text) = data.as_str().filter(|text| !text.trim().is_empty()) {
        return Some(ToolOutputPayload {
            text: text.to_string(),
            strategy: OutputStrategy::Head,
        });
    }

    if let Some(summary) = summarize_image_output(data)
        .or_else(|| summarize_apply_patch_output(data))
        .or_else(|| summarize_write_edit_output(data))
        .or_else(|| summarize_results_output(data))
    {
        return Some(ToolOutputPayload {
            text: summary,
            strategy: OutputStrategy::Head,
        });
    }

    let pretty = serde_json::to_string_pretty(data).unwrap_or_else(|_| data.to_string());
    let pretty = pretty.trim();
    if pretty.is_empty() {
        None
    } else {
        Some(ToolOutputPayload {
            text: pretty.to_string(),
            strategy: OutputStrategy::Head,
        })
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
        input_delta: Option<String>,
        tool_args_expanded: bool,
        tool_output_expanded: bool,
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
            tool_args_expanded: false,
            tool_output_expanded: false,
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
    /// # Panics
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
    /// # Panics
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
    /// # Panics
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
    /// # Panics
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
    /// Used when `ToolInputCompleted` arrives with the complete input after
    /// `ToolRequested` created the cell with empty input.
    ///
    /// # Panics
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
    /// # Panics
    /// Panics if called on a non-tool cell.
    pub fn set_tool_input_delta(&mut self, delta: String) {
        match self {
            HistoryCell::Tool { input_delta, .. } => {
                *input_delta = Some(delta);
            }
            _ => panic!("set_tool_input_delta called on non-tool cell"),
        }
    }

    /// Toggles expanded/collapsed tool args details.
    ///
    /// Returns true if toggled, false for non-tool cells.
    pub fn toggle_tool_args_expanded(&mut self) -> bool {
        match self {
            HistoryCell::Tool {
                tool_args_expanded, ..
            } => {
                *tool_args_expanded = !*tool_args_expanded;
                true
            }
            _ => false,
        }
    }

    /// Toggles expanded/collapsed tool output details.
    ///
    /// Returns true if toggled, false for non-tool cells.
    pub fn toggle_tool_output_expanded(&mut self) -> bool {
        match self {
            HistoryCell::Tool {
                tool_output_expanded,
                ..
            } => {
                *tool_output_expanded = !*tool_output_expanded;
                true
            }
            _ => false,
        }
    }

    /// Sets the result on a tool cell and updates state to Done or Error.
    ///
    /// # Panics
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

    /// Marks a cell as errored due to stream/network error.
    ///
    /// For tools: sets state to Error (only affects Running state).
    /// For assistant/thinking: stops streaming without marking as user-interrupted.
    pub fn mark_errored(&mut self) {
        match self {
            HistoryCell::Tool { state, .. } if *state == ToolState::Running => {
                *state = ToolState::Error;
            }
            HistoryCell::Assistant { is_streaming, .. } if *is_streaming => {
                *is_streaming = false;
            }
            HistoryCell::Thinking { is_streaming, .. } if *is_streaming => {
                *is_streaming = false;
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
                tool_args_expanded,
                tool_output_expanded,
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
                            format!("{frame} "),
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

                let prefix_width = prefix.width();
                let content_width = width.saturating_sub(prefix_width).max(10);

                // Show command for bash and compact context for other tools.
                let display_text = tool_display_text(name, input);

                // Wrap the command/tool text
                let wrapped = wrap_text(&display_text, content_width);

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

                if should_render_tool_args(name, input) {
                    let args_summary_max =
                        TOOL_ARG_SUMMARY_MAX_WIDTH.min(content_width.saturating_mul(3));
                    let (args_summary, args_summary_truncated, args_has_nested) =
                        build_tool_args_summary(input, args_summary_max);
                    for row in wrap_text(&format!("args: {args_summary}"), content_width) {
                        lines.push(StyledLine {
                            spans: vec![
                                StyledSpan {
                                    text: " ".repeat(prefix_width),
                                    style: Style::Plain,
                                },
                                StyledSpan {
                                    text: row,
                                    style: Style::ToolOutput,
                                },
                            ],
                        });
                    }

                    if should_show_tool_arg_details(input, args_summary_truncated, args_has_nested)
                    {
                        let disclosure = if *tool_args_expanded { '▼' } else { '▶' };
                        lines.push(StyledLine {
                            spans: vec![
                                StyledSpan {
                                    text: " ".repeat(prefix_width),
                                    style: Style::Plain,
                                },
                                StyledSpan {
                                    text: "args (json) ".to_string(),
                                    style: Style::ToolOutput,
                                },
                                StyledSpan {
                                    text: disclosure.to_string(),
                                    style: Style::ToolBracket,
                                },
                            ],
                        });

                        if *tool_args_expanded {
                            let detail_width = content_width.saturating_sub(2).max(8);
                            let (detail_rows, total_rows) = render_tool_arg_detail_rows(
                                input,
                                detail_width,
                                TOOL_ARG_DETAIL_MAX_LINES,
                            );

                            for row in detail_rows {
                                lines.push(StyledLine {
                                    spans: vec![
                                        StyledSpan {
                                            text: " ".repeat(prefix_width),
                                            style: Style::Plain,
                                        },
                                        StyledSpan {
                                            text: "│ ".to_string(),
                                            style: Style::ToolBracket,
                                        },
                                        StyledSpan {
                                            text: row,
                                            style: Style::ToolOutput,
                                        },
                                    ],
                                });
                            }

                            if total_rows > TOOL_ARG_DETAIL_MAX_LINES {
                                lines.push(StyledLine {
                                    spans: vec![
                                        StyledSpan {
                                            text: " ".repeat(prefix_width),
                                            style: Style::Plain,
                                        },
                                        StyledSpan {
                                            text: "╰ ".to_string(),
                                            style: Style::ToolBracket,
                                        },
                                        StyledSpan {
                                            text: format!(
                                                "{} more arg lines ...",
                                                total_rows - TOOL_ARG_DETAIL_MAX_LINES
                                            ),
                                            style: Style::ToolBracket,
                                        },
                                    ],
                                });
                            }
                        }
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

                // Render output preview and truncation warnings when done.
                if let Some(res) = result
                    && let Some(data) = res.data()
                {
                    if let Some(output) = build_tool_output_payload(name, data) {
                        let max_rows = if *tool_output_expanded {
                            TOOL_OUTPUT_DETAIL_MAX_LINES
                        } else {
                            TOOL_OUTPUT_PREVIEW_MAX_LINES
                        };
                        let (rows, total_rows) =
                            render_tool_output_rows(&output.text, width, max_rows, output.strategy);
                        let row_count = rows.len();
                        let truncated = total_rows > row_count;

                        if !rows.is_empty() || truncated {
                            let disclosure = if *tool_output_expanded { '▼' } else { '▶' };
                            lines.push(StyledLine {
                                spans: vec![
                                    StyledSpan {
                                        text: format_output_label(total_rows),
                                        style: Style::ToolBracket,
                                    },
                                    StyledSpan {
                                        text: disclosure.to_string(),
                                        style: Style::ToolBracket,
                                    },
                                ],
                            });

                            if truncated {
                                lines.push(StyledLine {
                                    spans: vec![StyledSpan {
                                        text: format!(
                                            "[{} more lines ...]",
                                            total_rows - row_count
                                        ),
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

                    // Show tool-level truncation warnings (when the tool itself truncated output)
                    let mut truncation_warnings = Vec::new();

                    // Check for Bash tool truncation (stdout_truncated, stderr_truncated)
                    let stdout_truncated = data
                        .get("stdout_truncated")
                        .and_then(serde_json::Value::as_bool)
                        .unwrap_or(false);
                    let stderr_truncated = data
                        .get("stderr_truncated")
                        .and_then(serde_json::Value::as_bool)
                        .unwrap_or(false);

                    if stdout_truncated {
                        let total = data
                            .get("stdout_total_bytes")
                            .and_then(serde_json::Value::as_u64)
                            .unwrap_or(0);
                        truncation_warnings.push(format_byte_truncation("stdout", total));
                    }
                    if stderr_truncated {
                        let total = data
                            .get("stderr_total_bytes")
                            .and_then(serde_json::Value::as_u64)
                            .unwrap_or(0);
                        truncation_warnings.push(format_byte_truncation("stderr", total));
                    }

                    // Check for Read tool truncation (truncated, total_lines, lines_shown)
                    let file_truncated = data
                        .get("truncated")
                        .and_then(serde_json::Value::as_bool)
                        .unwrap_or(false);
                    if file_truncated {
                        let byte_limited = data
                            .get("byte_limited")
                            .and_then(serde_json::Value::as_bool)
                            .unwrap_or(false);
                        let read_has_explicit_limit =
                            name == "read" && input.get("limit").and_then(extract_u64).is_some();
                        let should_warn =
                            name != "read" || byte_limited || !read_has_explicit_limit;

                        if should_warn {
                            let total_lines = data
                                .get("total_lines")
                                .and_then(serde_json::Value::as_u64)
                                .unwrap_or(0);
                            let lines_shown = data
                                .get("lines_shown")
                                .and_then(serde_json::Value::as_u64)
                                .unwrap_or(0);
                            truncation_warnings.push(format!(
                                "file truncated: showing {lines_shown} of {total_lines} lines"
                            ));
                        }
                    }

                    // Display truncation warnings
                    for warning in truncation_warnings {
                        lines.push(StyledLine {
                            spans: vec![StyledSpan {
                                text: format!("[⚠ {warning}]"),
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
                                    text: format!("Error [{code}]: {message}"),
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
                                                text: format!("  {line}"),
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
                // Trim trailing whitespace for finalized thinking blocks to avoid extra vertical space.
                // Keep raw content for streaming to preserve cursor position on newlines.
                let display_content = if *is_streaming {
                    content.as_str()
                } else {
                    content.trim_end()
                };

                let mut lines = render_prefixed_content(
                    prefix,
                    display_content,
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
                    format!("{mins}m{remaining_secs:.1}s")
                } else {
                    format!("{secs:.1}s")
                };

                // Format tool count
                let tool_str = if *tool_count == 1 {
                    "1 tool".to_string()
                } else {
                    format!("{tool_count} tools")
                };

                let message = format!("{tool_str} · {duration_str}");

                // Build centered separator line: ─── 3 tools · 3.5s ───
                let text_with_padding = format!(" {message} ");
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
                content.len() + usize::from(*is_interrupted)
            }
            HistoryCell::Assistant {
                content,
                is_interrupted,
                ..
            } => {
                // Include is_interrupted in discriminator
                content.len() + usize::from(*is_interrupted)
            }
            HistoryCell::Tool {
                result,
                tool_args_expanded,
                tool_output_expanded,
                ..
            } => {
                // Include expanded/collapsed state so toggles invalidate cache.
                usize::from(result.is_some())
                    + usize::from(*tool_args_expanded)
                    + usize::from(*tool_output_expanded)
            }
            HistoryCell::System { content, .. } => content.len(),
            HistoryCell::Thinking {
                content,
                is_interrupted,
                ..
            } => {
                // Include is_interrupted in discriminator
                content.len() + usize::from(*is_interrupted)
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
    fn test_tool_args_summary_shows_param_names_and_values() {
        let cell = HistoryCell::tool_running(
            "123",
            "read",
            serde_json::json!({
                "path": "test.txt",
                "offset": 10,
                "limit": 25
            }),
        );
        let lines = cell.display_lines(80, 0);
        let all_text: String = lines
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| s.text.as_str()))
            .collect();

        assert!(all_text.contains("args:"));
        assert!(all_text.contains("path=\"test.txt\""));
        assert!(all_text.contains("offset=10"));
        assert!(all_text.contains("limit=25"));
    }

    #[test]
    fn test_apply_patch_display_shows_target_files() {
        let patch = "*** Begin Patch\n*** Update File: src/main.rs\n@@\n-old\n+new\n*** Add File: docs/notes.md\n+hello\n*** End Patch";
        let cell =
            HistoryCell::tool_running("123", "apply_patch", serde_json::json!({"patch": patch}));

        let all_text: String = cell
            .display_lines(140, 0)
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| s.text.as_str()))
            .collect();

        assert!(all_text.contains("apply_patch"));
        assert!(all_text.contains("src/main.rs"));
        assert!(all_text.contains("docs/notes.md"));
    }

    #[test]
    fn test_web_search_display_shows_queries() {
        let cell = HistoryCell::tool_running(
            "123",
            "web_search",
            serde_json::json!({
                "objective": "Find docs",
                "search_queries": ["ratatui style guide", "rust tui styling"]
            }),
        );

        let all_text: String = cell
            .display_lines(140, 0)
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| s.text.as_str()))
            .collect();

        assert!(all_text.contains("web_search"));
        assert!(all_text.contains('['));
        assert!(all_text.contains(']'));
        assert!(all_text.contains("ratatui style guide"));
        assert!(all_text.contains("rust tui styling"));
    }

    #[test]
    fn test_fetch_webpage_display_shows_url() {
        let cell = HistoryCell::tool_running(
            "123",
            "fetch_webpage",
            serde_json::json!({
                "url": "https://example.com/docs",
                "objective": "extract API section"
            }),
        );

        let all_text: String = cell
            .display_lines(140, 0)
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| s.text.as_str()))
            .collect();

        assert!(all_text.contains("fetch_webpage"));
        assert!(all_text.contains("https://example.com/docs"));
    }

    #[test]
    fn test_thread_search_display_shows_query() {
        let cell = HistoryCell::tool_running(
            "123",
            "thread_search",
            serde_json::json!({
                "query": "memory system last week",
                "date_start": "2026-02-01",
                "date_end": "2026-02-07",
                "limit": 10
            }),
        );

        let all_text: String = cell
            .display_lines(140, 0)
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| s.text.as_str()))
            .collect();

        assert!(all_text.contains("thread_search"));
        assert!(all_text.contains("memory system last week"));
    }

    #[test]
    fn test_tool_args_complex_values_show_json_preview() {
        let mut cell = HistoryCell::tool_running(
            "123",
            "apply_patch",
            serde_json::json!({
                "patch": "*** Begin Patch\n*** End Patch",
                "metadata": {
                    "author": "zdx",
                    "reviewers": ["alice", "bob"]
                }
            }),
        );
        cell.toggle_tool_args_expanded();

        let lines = cell.display_lines(80, 0);
        let all_text: String = lines
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| s.text.as_str()))
            .collect();

        assert!(all_text.contains("args (json) ▼"));
        assert!(all_text.contains("\"metadata\""));
        assert!(all_text.contains("\"reviewers\""));
    }

    #[test]
    fn test_tool_args_json_collapsed_by_default() {
        let cell = HistoryCell::tool_running(
            "123",
            "apply_patch",
            serde_json::json!({
                "patch": "*** Begin Patch\n*** End Patch",
                "metadata": {"author": "zdx"}
            }),
        );

        let all_text: String = cell
            .display_lines(80, 0)
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| s.text.as_str()))
            .collect();

        assert!(all_text.contains("args (json) ▶"));
        assert!(!all_text.contains("\"metadata\""));
    }

    #[test]
    fn test_tool_args_json_expand_toggle_shows_details() {
        let mut cell = HistoryCell::tool_running(
            "123",
            "apply_patch",
            serde_json::json!({
                "patch": "*** Begin Patch\n*** End Patch",
                "metadata": {"author": "zdx", "reviewers": ["alice"]}
            }),
        );

        assert!(cell.toggle_tool_args_expanded());

        let all_text: String = cell
            .display_lines(80, 0)
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| s.text.as_str()))
            .collect();

        assert!(all_text.contains("args (json) ▼"));
        assert!(all_text.contains("\"metadata\""));
        assert!(all_text.contains("\"reviewers\""));
    }

    #[test]
    fn test_bash_command_only_hides_args_section() {
        let cell =
            HistoryCell::tool_running("123", "bash", serde_json::json!({"command": "echo hi"}));

        let all_text: String = cell
            .display_lines(80, 0)
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| s.text.as_str()))
            .collect();

        assert!(all_text.contains("echo hi"));
        assert!(!all_text.contains("args:"));
        assert!(!all_text.contains("args (json)"));
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
            eprintln!("Line {i}: {texts:?}");
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
    fn test_thinking_trailing_newlines() {
        // Streaming: should preserve trailing newlines (cursor positioning)
        let cell_streaming = HistoryCell::thinking_streaming("Text\n\n");
        let lines_streaming = cell_streaming.display_lines(80, 0);

        // 3 lines: "Thinking: Text", "", ""
        assert_eq!(
            lines_streaming.len(),
            3,
            "Streaming should preserve trailing newlines"
        );

        // Finalized: should trim trailing newlines
        let mut cell_final = HistoryCell::thinking_streaming("Text\n\n");
        cell_final.finalize_thinking(None);
        let lines_final = cell_final.display_lines(80, 0);

        // 1 line: "Thinking: Text"
        assert_eq!(
            lines_final.len(),
            1,
            "Finalized should trim trailing newlines"
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
            eprintln!("Line {i}: {texts:?}");
        }

        // Should have 5 lines: Para1, blank, Para2, blank, Para3
        assert_eq!(lines.len(), 5, "Expected 5 lines");

        // Only first line should have "Thinking:" prefix
        assert_eq!(lines[0].spans[0].text, "Thinking: ");

        // All other lines (including blank lines) should have indentation
        for (i, _) in lines.iter().enumerate().skip(1) {
            assert_eq!(
                lines[i].spans[0].text, "          ",
                "Line {i} should be indented, not prefixed"
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
                "Line {i} width {line_width} exceeds limit {width}"
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
            format_byte_truncation("stdout", 51_200),
            "stdout truncated: 50.0 KB total"
        );
        assert_eq!(
            format_byte_truncation("stderr", 1_048_576),
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
            "stdout_total_bytes": 102_400,
            "stderr_total_bytes": 1_048_576
        })));

        let lines = cell.display_lines(80, 0);
        let all_text: String = lines
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| s.text.as_str()))
            .collect();

        // Should show truncation warnings with sizes
        assert!(
            all_text.contains("stdout truncated"),
            "Expected stdout truncation warning, got: {all_text}"
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
            "Expected file truncation warning, got: {all_text}"
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

    #[test]
    fn test_mark_errored_tool() {
        let mut cell =
            HistoryCell::tool_running("123", "read", serde_json::json!({"path": "test.txt"}));

        // Should be Running initially
        match &cell {
            HistoryCell::Tool { state, .. } => assert_eq!(*state, ToolState::Running),
            _ => panic!("Expected tool cell"),
        }

        cell.mark_errored();

        // Should be Error after mark_errored
        match &cell {
            HistoryCell::Tool { state, .. } => assert_eq!(*state, ToolState::Error),
            _ => panic!("Expected tool cell"),
        }
    }

    #[test]
    fn test_mark_errored_assistant_streaming() {
        let mut cell = HistoryCell::assistant_streaming("Partial response...");

        // Should be streaming initially
        match &cell {
            HistoryCell::Assistant { is_streaming, .. } => assert!(*is_streaming),
            _ => panic!("Expected assistant cell"),
        }

        cell.mark_errored();

        // Should not be streaming after mark_errored, and NOT marked as interrupted
        match &cell {
            HistoryCell::Assistant {
                is_streaming,
                is_interrupted,
                ..
            } => {
                assert!(!*is_streaming);
                assert!(!*is_interrupted); // Error != user interruption
            }
            _ => panic!("Expected assistant cell"),
        }
    }

    #[test]
    fn test_mark_errored_thinking_streaming() {
        let mut cell = HistoryCell::thinking_streaming("Partial thinking...");

        // Should be streaming initially
        match &cell {
            HistoryCell::Thinking { is_streaming, .. } => assert!(*is_streaming),
            _ => panic!("Expected thinking cell"),
        }

        cell.mark_errored();

        // Should not be streaming after mark_errored, and NOT marked as interrupted
        match &cell {
            HistoryCell::Thinking {
                is_streaming,
                is_interrupted,
                ..
            } => {
                assert!(!*is_streaming);
                assert!(!*is_interrupted); // Error != user interruption
            }
            _ => panic!("Expected thinking cell"),
        }
    }

    #[test]
    fn test_mark_errored_does_not_affect_completed() {
        // Completed tool should not change
        let mut tool_cell =
            HistoryCell::tool_running("123", "read", serde_json::json!({"path": "test.txt"}));
        tool_cell.set_tool_result(ToolOutput::success(serde_json::json!({"ok": true})));

        match &tool_cell {
            HistoryCell::Tool { state, .. } => assert_eq!(*state, ToolState::Done),
            _ => panic!("Expected tool cell"),
        }

        tool_cell.mark_errored();

        // Should still be Done (not Error)
        match &tool_cell {
            HistoryCell::Tool { state, .. } => assert_eq!(*state, ToolState::Done),
            _ => panic!("Expected tool cell"),
        }

        // Finalized assistant should not change
        let mut assistant_cell = HistoryCell::assistant("Complete response");
        match &assistant_cell {
            HistoryCell::Assistant { is_streaming, .. } => assert!(!*is_streaming),
            _ => panic!("Expected assistant cell"),
        }

        assistant_cell.mark_errored();

        // Should still not be streaming (no change)
        match &assistant_cell {
            HistoryCell::Assistant { is_streaming, .. } => assert!(!*is_streaming),
            _ => panic!("Expected assistant cell"),
        }
    }
}
