//! Shared tool-detail body rendering.
//!
//! Builds the full view of a single tool call — status, pretty-printed args,
//! relayed child tools, and output (final result or live streaming deltas) —
//! as ratatui lines. What a result means (output by kind, exit code, timeout,
//! warnings) comes from the shared `zdx_types::tool_view`; this module only
//! styles it for the terminal. Used by the `zdx-tui` tool detail popup and the
//! `zdx-monitor` transcript overlay so both show identical content.
//!
//! Interactive concerns (selection, clipboard, scrolling, popup chrome) stay
//! with the callers.

use std::fmt::Write as _;

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use zdx_types::tool_view::{DiffOp, ToolBody, ToolView, ViewLimits, tool_output_view};

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

fn push_view_body(lines: &mut Vec<Line<'static>>, view: &ToolView) {
    let plain = Style::default().fg(Color::White);
    let dim = Style::default().fg(Color::DarkGray);
    let no_output = |lines: &mut Vec<Line<'static>>| {
        lines.push(Line::from(Span::styled("(no output)", dim)));
    };

    match &view.body {
        ToolBody::Empty => no_output(lines),
        ToolBody::Streams { streams } => {
            if streams.is_empty() {
                no_output(lines);
            }
            for stream in streams {
                let is_stderr = stream.name == "stderr";
                if is_stderr && streams.len() > 1 {
                    lines.push(Line::from(Span::styled("─── stderr ───", dim)));
                }
                let style = if is_stderr {
                    Style::default().fg(Color::LightRed)
                } else {
                    plain
                };
                push_text_lines(lines, &stream.text, style);
            }
        }
        ToolBody::Code { text, .. } | ToolBody::Text { text } => {
            push_text_lines(lines, text, plain);
        }
        ToolBody::Matches { files } => {
            if files.is_empty() {
                lines.push(Line::from(Span::styled("(no matches)", dim)));
            }
            for file in files {
                lines.push(Line::from(Span::styled(
                    file.path.clone(),
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                )));
                for row in &file.lines {
                    let text_style = if row.context { dim } else { plain };
                    lines.push(Line::from(vec![
                        Span::styled(format!("{:>6}  ", row.line), dim),
                        Span::styled(row.text.clone(), text_style),
                    ]));
                }
            }
        }
        ToolBody::Paths { entries } => {
            if entries.is_empty() {
                lines.push(Line::from(Span::styled("(nothing found)", dim)));
            }
            for entry in entries {
                let suffix = if entry.dir { "/" } else { "" };
                lines.push(Line::from(Span::styled(
                    format!("{}{suffix}", entry.path),
                    plain,
                )));
            }
        }
        ToolBody::Diff { lines: diff, .. } => {
            for row in diff {
                let (sign, style) = match row.op {
                    DiffOp::Add => ("+ ", Style::default().fg(Color::Green)),
                    DiffOp::Del => ("- ", Style::default().fg(Color::Red)),
                    DiffOp::Ctx => ("  ", dim),
                };
                lines.push(Line::from(Span::styled(
                    format!("{sign}{}", row.text),
                    style,
                )));
            }
        }
        ToolBody::Markdown { text } => {
            push_text_lines(lines, text, plain);
            if let Some(thread) = &view.thread_id {
                lines.push(Line::from(Span::styled("───", dim)));
                lines.push(Line::from(Span::styled(
                    format!("child_thread_id: {thread}"),
                    plain,
                )));
            }
        }
        ToolBody::Error {
            code,
            message,
            details,
        } => {
            lines.push(Line::from(Span::styled(
                format!("Error [{code}]: {message}"),
                Style::default().fg(Color::Red),
            )));
            for detail_line in details.as_deref().unwrap_or_default().lines() {
                lines.push(Line::from(Span::styled(format!("  {detail_line}"), dim)));
            }
        }
    }

    push_view_notes(lines, view);
}

/// Warnings, then plain long details (log paths).
fn push_view_notes(lines: &mut Vec<Line<'static>>, view: &ToolView) {
    let warn = Style::default().fg(Color::Yellow);
    for note in &view.notes {
        lines.push(Line::from(Span::styled(format!("⚠ {note}"), warn)));
    }
    let dim = Style::default().fg(Color::DarkGray);
    for info in &view.info {
        lines.push(Line::from(Span::styled(info.clone(), dim)));
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
        ToolState::Running => {
            let secs = chrono::Utc::now()
                .signed_duration_since(*started_at)
                .num_seconds()
                .max(0);
            if secs < 60 {
                format!("Running… ({secs}s)")
            } else {
                format!("Running… ({}m {}s)", secs / 60, secs % 60)
            }
        }
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
    // Timing comes from the cell, so the view gets no duration. The terminal
    // has no raw-output view, so the view keeps the whole output.
    let view = result
        .as_ref()
        .map(|res| tool_output_view(name, input, Some(res), None, ViewLimits::FULL));
    let mut status = vec![
        Span::styled(
            "Status: ",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(status_text, Style::default().fg(tool_state_color(state))),
    ];
    // Error and Cancelled already say so; facts add exit code, size, ranges.
    if *state == ToolState::Done
        && let Some(view) = &view
    {
        for fact in &view.status {
            let color = if fact.bad { Color::Red } else { Color::Gray };
            status.push(Span::styled(" · ", Style::default().fg(Color::DarkGray)));
            status.push(Span::styled(fact.text.clone(), Style::default().fg(color)));
        }
    }
    lines.push(Line::from(status));
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

    if let Some(view) = &view {
        push_view_body(&mut lines, view);
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

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};
    use zdx_types::ToolOutput;

    use super::*;

    fn body_text(name: &str, input: Value, output: ToolOutput) -> (String, String) {
        let mut cell = HistoryCell::tool_running("t1", name, input);
        cell.set_tool_result(output);
        let body = tool_detail_body(&cell);
        let text = |lines: &[Line<'static>]| {
            lines
                .iter()
                .map(|line| {
                    line.spans
                        .iter()
                        .map(|s| s.content.as_ref())
                        .collect::<String>()
                })
                .collect::<Vec<_>>()
                .join("\n")
        };
        (
            text(&body.lines[..1]),
            text(&body.lines[body.output_start..]),
        )
    }

    #[test]
    fn bash_output_is_the_text_and_facts_go_to_the_status() {
        let (status, output) = body_text(
            "bash",
            json!({"command": "grep -n x f"}),
            ToolOutput::success(json!({
                "stdout": "54: x\n", "stderr": "", "exit_code": 0, "timed_out": false,
                "stdout_truncated": false, "stdout_total_bytes": 6, "stderr_total_bytes": 0
            })),
        );
        assert!(status.ends_with(" · exit 0 · 6 B"), "{status}");
        assert_eq!(output, "54: x");
    }

    #[test]
    fn timed_out_bash_says_so() {
        let (status, output) = body_text(
            "bash",
            json!({"command": "sleep 9"}),
            ToolOutput::success(json!({
                "stdout": "partial\n", "stderr": "", "exit_code": -1, "timed_out": true,
                "stdout_truncated": true, "stdout_total_bytes": 2048, "stdout_file": "/tmp/out"
            })),
        );
        assert!(status.contains(" · timed out"), "{status}");
        assert!(!status.contains("exit"), "{status}");
        assert!(
            output.starts_with("partial\n⚠ The command timed out"),
            "{output}"
        );
        assert!(output.contains("Full stdout: /tmp/out"), "{output}");
    }

    #[test]
    fn one_line_edit_shows_its_diff() {
        let (status, output) = body_text(
            "edit",
            json!({"file_path": "a.rs", "old_string": "x = 1", "new_string": "x = 2"}),
            ToolOutput::success(json!({"replacements": 1})),
        );
        assert!(status.ends_with(" · 1 replacement"), "{status}");
        assert_eq!(output, "- x = 1\n+ x = 2");
    }

    #[test]
    fn failure_shows_code_message_and_details() {
        let (status, output) = body_text(
            "read",
            json!({"file_path": "x"}),
            ToolOutput::failure("not_found", "No such file", Some("looked in /a".into())),
        );
        assert_eq!(status, "Status: Error");
        assert_eq!(output, "Error [not_found]: No such file\n  looked in /a");
    }

    #[test]
    fn output_past_the_display_cap_is_shown_whole() {
        let big = format!(
            "{}\nlast line",
            "x".repeat(zdx_types::tool_view::MAX_VIEW_TEXT_BYTES)
        );
        let (_, output) = body_text(
            "bash",
            json!({"command": "yes"}),
            ToolOutput::success(json!({"stdout": big, "exit_code": 0})),
        );
        assert_eq!(output, big);
        assert!(!output.contains('⚠'));
    }

    #[test]
    fn background_output_shows_its_streams_not_json() {
        let (status, output) = body_text(
            "background_output",
            json!({"bg_id": "bg-1"}),
            ToolOutput::success(json!({
                "bg_id": "bg-1", "status": "exited", "exit_code": 0,
                "stdout": "built\n\n", "stderr": ""
            })),
        );
        assert!(status.ends_with(" · exited · exit 0"), "{status}");
        assert_eq!(output, "built\n");
    }

    #[test]
    fn overwritten_file_shows_its_content() {
        let (status, output) = body_text(
            "write",
            json!({"file_path": "a.txt", "content": "same\n"}),
            ToolOutput::success(json!({"created": false, "bytes": 5})),
        );
        assert!(status.ends_with(" · overwritten · 5 B"), "{status}");
        assert_eq!(output, "same");
    }

    #[test]
    fn background_start_shows_its_job_and_logs() {
        let (status, output) = body_text(
            "bash",
            json!({"command": "just ci", "background": true}),
            ToolOutput::success(json!({
                "bg_id": "bg-1", "pid": 5, "status": "running",
                "stdout_log": "/l/o", "stderr_log": "/l/e", "message": "Started"
            })),
        );
        assert!(
            status.ends_with(" · in background · bg-1 · pid 5"),
            "{status}"
        );
        assert_eq!(output, "(no output)\nLogs: stdout /l/o, stderr /l/e");
    }

    #[test]
    fn subagent_answer_shows_text_then_its_thread() {
        let (_, output) = body_text(
            "invoke_subagent",
            json!({"subagent": "explorer", "prompt": "look"}),
            ToolOutput::success(
                json!({ "result": "line one\nline two", "child_thread_id": "thread-1" }),
            ),
        );
        assert_eq!(output, "line one\nline two\n───\nchild_thread_id: thread-1");
    }
}
