//! Shared tool-detail body rendering.
//!
//! Builds the full view of a single tool call — status, pretty-printed args,
//! relayed child tools, and output (final result or live streaming deltas) —
//! as ratatui lines. Used by the `zdx-tui` tool detail popup and the
//! `zdx-monitor` transcript overlay so both show identical content.
//!
//! Interactive concerns (selection, clipboard, scrolling, popup chrome) stay
//! with the callers.

use std::fmt::Write as _;

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use serde_json::Value;

use crate::cell::{ChildToolState, HistoryCell, SPINNER_FRAMES, ToolState};

/// Border/status color for a tool state.
pub fn tool_state_color(state: &ToolState) -> Color {
    match state {
        ToolState::Running => Color::Cyan,
        ToolState::Done => Color::Green,
        ToolState::Error => Color::Red,
        ToolState::Cancelled => Color::Yellow,
    }
}

/// Status glyph for a tool state. `spinner_index` selects the spinner frame for
/// running tools (callers that render statically pass `0`).
pub fn tool_state_glyph(state: &ToolState, spinner_index: usize) -> &'static str {
    match state {
        ToolState::Running => SPINNER_FRAMES[spinner_index % SPINNER_FRAMES.len()],
        ToolState::Done => "✓",
        ToolState::Error => "✗",
        ToolState::Cancelled => "⊘",
    }
}

/// A tool's detail body plus the index of its first output line.
pub struct ToolDetailBody {
    /// Styled body lines (unwrapped; callers wrap to their own width).
    pub lines: Vec<Line<'static>>,
    /// Index of the first line after the `─── Output ───` header, so callers
    /// can slice just the output section (e.g. for a copy action).
    pub output_start: usize,
}

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

/// Builds human-readable output text for a tool result payload.
fn tool_output_text(name: &str, data: &Value) -> String {
    // Try stdout/stderr extraction (bash and other tools that produce it)
    let stdout = data.get("stdout").and_then(|v| v.as_str()).unwrap_or("");
    let stderr = data.get("stderr").and_then(|v| v.as_str()).unwrap_or("");
    if !stdout.is_empty() || !stderr.is_empty() {
        let mut text = String::new();
        if !stdout.is_empty() {
            text.push_str(stdout);
        }
        if !stderr.is_empty() {
            if !text.is_empty() && !text.ends_with('\n') {
                text.push('\n');
            }
            text.push_str(stderr);
        }
        // Append metadata fields when present
        let metadata_keys = [
            "exit_code",
            "timed_out",
            "stdout_file",
            "stderr_file",
            "stdout_truncated",
            "stderr_truncated",
        ];
        let mut has_meta = false;
        for key in metadata_keys {
            if let Some(val) = data.get(key) {
                if !has_meta {
                    if !text.ends_with('\n') {
                        text.push('\n');
                    }
                    text.push_str("───\n");
                    has_meta = true;
                }
                let _ = writeln!(text, "{key}: {val}");
            }
        }
        return text;
    }

    // For read tool: show file content directly
    if name == "read"
        && let Some(content) = data.get("content").and_then(Value::as_str)
    {
        return content.to_string();
    }

    if let Some(text) = data.as_str() {
        return text.to_string();
    }

    serde_json::to_string_pretty(data).unwrap_or_else(|_| data.to_string())
}

fn section_header(text: &'static str) -> Line<'static> {
    Line::from(Span::styled(
        text,
        Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD),
    ))
}

fn push_text_lines(lines: &mut Vec<Line<'static>>, text: &str, style: Style) {
    for line in text.lines() {
        lines.push(Line::from(Span::styled(line.to_string(), style)));
    }
}

fn push_truncation_warnings(lines: &mut Vec<Line<'static>>, data: &Value) {
    let flag = |key: &str| {
        data.get(key)
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false)
    };
    let count = |key: &str| {
        data.get(key)
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0)
    };
    let warn = Style::default().fg(Color::Yellow);

    if flag("stdout_truncated") {
        let text = format_byte_truncation("stdout", count("stdout_total_bytes"));
        lines.push(Line::from(Span::styled(format!("⚠ {text}"), warn)));
    }
    if flag("stderr_truncated") {
        let text = format_byte_truncation("stderr", count("stderr_total_bytes"));
        lines.push(Line::from(Span::styled(format!("⚠ {text}"), warn)));
    }
    if flag("truncated") {
        let total = count("total_lines");
        let shown = count("lines_shown");
        lines.push(Line::from(Span::styled(
            format!("⚠ file truncated: showing {shown} of {total} lines"),
            warn,
        )));
    }
}

/// Builds the detail body (status, args, child tools, output) for a tool cell.
///
/// Non-tool cells yield an empty body.
#[allow(clippy::too_many_lines)]
pub fn tool_detail_body(cell: &HistoryCell) -> ToolDetailBody {
    let HistoryCell::Tool {
        name,
        state,
        input,
        result,
        started_at,
        completed_at,
        input_delta,
        output_delta,
        child_tools,
        ..
    } = cell
    else {
        return ToolDetailBody {
            lines: Vec::new(),
            output_start: 0,
        };
    };

    let mut lines: Vec<Line<'static>> = Vec::new();

    let status_text = match state {
        ToolState::Running => "Running…".to_string(),
        ToolState::Done => match completed_at {
            Some(completed) => {
                let elapsed = completed.signed_duration_since(*started_at);
                format!("Done ({:.1}s)", elapsed.num_milliseconds() as f64 / 1000.0)
            }
            None => "Done".to_string(),
        },
        ToolState::Error => "Error".to_string(),
        ToolState::Cancelled => "Cancelled".to_string(),
    };
    lines.push(Line::from(vec![
        Span::styled(
            "Status: ",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(status_text, Style::default().fg(tool_state_color(state))),
    ]));
    lines.push(Line::from(""));

    lines.push(section_header("─── Args ───"));
    let pretty_args = serde_json::to_string_pretty(input).unwrap_or_else(|_| input.to_string());
    push_text_lines(
        &mut lines,
        &pretty_args,
        Style::default().fg(Color::DarkGray),
    );
    lines.push(Line::from(""));

    if !child_tools.is_empty() {
        lines.push(section_header("─── Child tools ───"));
        for entry in child_tools {
            let (glyph, color) = match entry.state {
                ChildToolState::Running => ("⟳", Color::Cyan),
                ChildToolState::Done => ("✓", Color::Green),
                ChildToolState::Error => ("✗", Color::Red),
            };
            let mut text = format!("{glyph} {}", entry.name);
            if let Some(arg) = entry.key_arg.as_deref().filter(|arg| !arg.is_empty()) {
                let _ = write!(text, "  {arg}");
            }
            lines.push(Line::from(Span::styled(text, Style::default().fg(color))));
        }
        lines.push(Line::from(""));
    }

    lines.push(section_header("─── Output ───"));
    let output_start = lines.len();

    if let Some(res) = result {
        if let Some(data) = res.data() {
            let output_text = tool_output_text(name, data);
            push_text_lines(&mut lines, &output_text, Style::default().fg(Color::White));
            push_truncation_warnings(&mut lines, data);
        }

        if let Some((code, message, details)) = res.error_info() {
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                format!("Error [{code}]: {message}"),
                Style::default().fg(Color::Red),
            )));
            if let Some(detail_text) = details {
                for detail_line in detail_text.lines() {
                    lines.push(Line::from(Span::styled(
                        format!("  {detail_line}"),
                        Style::default().fg(Color::DarkGray),
                    )));
                }
            }
        }
    } else if *state == ToolState::Running {
        // Streaming output first, then the partial input JSON, then a placeholder.
        if let Some(delta) = output_delta.as_deref().filter(|d| !d.is_empty()) {
            push_text_lines(&mut lines, delta, Style::default().fg(Color::White));
        } else if let Some(delta) = input_delta.as_deref().filter(|d| !d.is_empty()) {
            push_text_lines(&mut lines, delta, Style::default().fg(Color::Cyan));
        } else {
            lines.push(Line::from(Span::styled(
                "Waiting for output…",
                Style::default()
                    .fg(Color::DarkGray)
                    .add_modifier(Modifier::ITALIC),
            )));
        }
    } else if let Some(delta) = output_delta.as_deref().filter(|d| !d.is_empty()) {
        // Preserved partial output for cancelled/errored tools.
        push_text_lines(&mut lines, delta, Style::default().fg(Color::DarkGray));
    } else {
        lines.push(Line::from(Span::styled(
            "(no output)",
            Style::default().fg(Color::DarkGray),
        )));
    }

    ToolDetailBody {
        lines,
        output_start,
    }
}
