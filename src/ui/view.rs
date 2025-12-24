//! Pure view/render functions for the TUI.
//!
//! This module contains all rendering logic. Functions here:
//! - Take `&TuiState` by immutable reference
//! - Draw to a ratatui Frame
//! - Never mutate state or return effects
//!
//! The separation from TuiRuntime eliminates borrow-checker conflicts
//! that previously required cloning state for rendering.

use ratatui::{
    Frame,
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Wrap},
};

use crate::ui::overlays::{
    render_command_palette, render_login_overlay, render_model_picker, render_thinking_picker,
};
use crate::ui::state::{AuthType, EngineState, OverlayState, TuiState};
use crate::ui::transcript::{Style as TranscriptStyle, StyledLine};

/// Height of the input area (lines, including borders).
const INPUT_HEIGHT: u16 = 5;

/// Height of status line below input.
const STATUS_HEIGHT: u16 = 1;

/// Horizontal margin for the transcript area (left and right).
const TRANSCRIPT_MARGIN: u16 = 1;

/// Spinner frames for status line animation.
const SPINNER_FRAMES: &[&str] = &["◐", "◓", "◑", "◒"];

/// Spinner speed divisor (render frames per spinner frame).
/// At 30fps render rate, 3 gives ~10fps spinner animation.
const SPINNER_SPEED_DIVISOR: usize = 3;

/// Renders the entire TUI to the frame.
///
/// This is a pure render function - it only reads state and draws to frame.
/// No mutations, no side effects.
pub fn view(state: &TuiState, frame: &mut Frame) {
    let area = frame.area();

    // Get terminal size for transcript rendering (account for margins)
    let transcript_width = area.width.saturating_sub(TRANSCRIPT_MARGIN * 2) as usize;

    // Calculate transcript pane height (no header now)
    let transcript_height = area.height.saturating_sub(INPUT_HEIGHT + STATUS_HEIGHT) as usize;

    // Pre-render transcript lines
    let all_lines = render_transcript(state, transcript_width);
    let total_lines = all_lines.len();

    // Use ScrollState for offset calculation (uses cached line count)
    let scroll_offset = {
        let max_offset = total_lines.saturating_sub(transcript_height);
        if state.scroll.is_following() {
            total_lines.saturating_sub(transcript_height)
        } else {
            state.scroll.get_offset(transcript_height).min(max_offset)
        }
    };

    // Slice visible lines
    let visible_end = (scroll_offset + transcript_height).min(total_lines);
    let content_lines: Vec<Line<'static>> = all_lines
        .into_iter()
        .skip(scroll_offset)
        .take(visible_end - scroll_offset)
        .collect();

    // Bottom-align: add padding at top when content doesn't fill the screen
    let visible_lines: Vec<Line<'static>> = if content_lines.len() < transcript_height {
        let padding_count = transcript_height - content_lines.len();
        let mut padded = vec![Line::default(); padding_count];
        padded.extend(content_lines);
        padded
    } else {
        content_lines
    };

    // Create layout: transcript, input, status
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(1),                // Transcript
            Constraint::Length(INPUT_HEIGHT),  // Input
            Constraint::Length(STATUS_HEIGHT), // Status line
        ])
        .split(area);

    // Transcript area with horizontal margins
    let transcript = Paragraph::new(visible_lines)
        .wrap(Wrap { trim: false })
        .block(Block::default().borders(Borders::NONE));
    let transcript_area = Rect {
        x: chunks[0].x + TRANSCRIPT_MARGIN,
        y: chunks[0].y,
        width: chunks[0].width.saturating_sub(TRANSCRIPT_MARGIN * 2),
        height: chunks[0].height,
    };
    frame.render_widget(transcript, transcript_area);

    // Input area with model on top-left border and path on bottom-right
    render_input(state, frame, chunks[1]);

    // Status line below input
    render_status_line(state, frame, chunks[2]);

    // Render overlay (last, so it appears on top)
    match &state.overlay {
        OverlayState::CommandPalette(palette) => {
            render_command_palette(frame, palette, area, chunks[1].y);
        }
        OverlayState::ModelPicker(picker) => {
            render_model_picker(frame, picker, area, chunks[1].y);
        }
        OverlayState::ThinkingPicker(picker) => {
            render_thinking_picker(frame, picker, area, chunks[1].y);
        }
        OverlayState::Login(login_state) => {
            render_login_overlay(frame, login_state, area);
        }
        OverlayState::None => {}
    }
}

/// Renders the input area with model info on top border and path on bottom border.
fn render_input(state: &TuiState, frame: &mut Frame, area: Rect) {
    use crate::config::ThinkingLevel;

    // Build top-left title: model name + auth type + thinking level
    let auth_indicator = match state.auth_type {
        AuthType::OAuth => " (oauth)",
        AuthType::ApiKey => " (api-key)",
        AuthType::None => "",
    };

    // Build title spans: model + auth in normal style, thinking in dim style
    let base_style = Style::default().fg(Color::DarkGray);
    let thinking_style = Style::default()
        .fg(Color::DarkGray)
        .add_modifier(Modifier::DIM);

    let mut title_spans = vec![Span::styled(
        format!(" {}{}", state.config.model, auth_indicator),
        base_style,
    )];

    // Add thinking indicator with dim style (only when enabled)
    if state.config.thinking_level != ThinkingLevel::Off {
        title_spans.push(Span::styled(
            format!(" [💭{}]", state.config.thinking_level.display_name()),
            thinking_style,
        ));
    }

    title_spans.push(Span::styled(" ", base_style));

    // Build bottom-right title: path and git branch
    let bottom_title = if let Some(ref branch) = state.git_branch {
        format!(" {} ({}) ", state.display_path, branch)
    } else {
        format!(" {} ", state.display_path)
    };

    // Create a custom textarea rendering with our border titles
    // We need to render the textarea content inside our custom block
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::DarkGray))
        .title(Line::from(title_spans))
        .title_bottom(
            Line::from(Span::styled(
                bottom_title,
                Style::default().fg(Color::DarkGray),
            ))
            .alignment(Alignment::Right),
        );

    // Get cursor position for rendering
    let cursor_line = state.textarea.cursor().0;
    let cursor_col = state.textarea.cursor().1;

    // We need to render the textarea directly but with a modified block
    // Clone the textarea widget's visual appearance
    let mut styled_lines: Vec<Line> = Vec::new();
    for (i, line) in state.textarea.lines().iter().enumerate() {
        if i == cursor_line {
            // Add cursor indicator on the current line
            let mut spans = Vec::new();
            let chars: Vec<char> = line.chars().collect();
            if cursor_col < chars.len() {
                spans.push(Span::raw(chars[..cursor_col].iter().collect::<String>()));
                spans.push(Span::styled(
                    chars[cursor_col].to_string(),
                    Style::default().add_modifier(Modifier::REVERSED),
                ));
                spans.push(Span::raw(
                    chars[cursor_col + 1..].iter().collect::<String>(),
                ));
            } else {
                spans.push(Span::raw(line.clone()));
                spans.push(Span::styled(
                    " ",
                    Style::default().add_modifier(Modifier::REVERSED),
                ));
            }
            styled_lines.push(Line::from(spans));
        } else {
            styled_lines.push(Line::from(line.as_str()));
        }
    }

    // If no lines, show cursor
    if styled_lines.is_empty() {
        styled_lines.push(Line::from(Span::styled(
            " ",
            Style::default().add_modifier(Modifier::REVERSED),
        )));
    }

    let input_paragraph = Paragraph::new(styled_lines).block(block);
    frame.render_widget(input_paragraph, area);
}

/// Renders the status line below the input.
fn render_status_line(state: &TuiState, frame: &mut Frame, area: Rect) {
    let spinner_idx = (state.spinner_frame / SPINNER_SPEED_DIVISOR) % SPINNER_FRAMES.len();
    let spinner = SPINNER_FRAMES[spinner_idx];

    let spans: Vec<Span> = match &state.engine_state {
        EngineState::Idle => {
            // Show helpful shortcuts when idle
            vec![
                Span::styled("Ctrl+P", Style::default().fg(Color::DarkGray)),
                Span::raw(" commands  "),
                Span::styled("Ctrl+C", Style::default().fg(Color::DarkGray)),
                Span::raw(" quit"),
            ]
        }
        EngineState::Waiting { .. } => {
            vec![
                Span::styled(spinner, Style::default().fg(Color::Yellow)),
                Span::raw(" "),
                Span::styled("Waiting...", Style::default().fg(Color::Yellow)),
                Span::raw("  "),
                Span::styled("Esc", Style::default().fg(Color::DarkGray)),
                Span::raw(" to cancel"),
            ]
        }
        EngineState::Streaming { .. } => {
            vec![
                Span::styled(spinner, Style::default().fg(Color::Cyan)),
                Span::raw(" "),
                Span::styled("Streaming...", Style::default().fg(Color::Cyan)),
                Span::raw("  "),
                Span::styled("Esc", Style::default().fg(Color::DarkGray)),
                Span::raw(" to cancel"),
            ]
        }
    };

    let status = Paragraph::new(Line::from(spans)).alignment(Alignment::Left);
    frame.render_widget(status, area);
}

/// Renders the transcript into ratatui Lines.
fn render_transcript(state: &TuiState, width: usize) -> Vec<Line<'static>> {
    let mut lines = Vec::new();

    for cell in &state.transcript {
        let styled_lines = cell.display_lines_cached(
            width,
            state.spinner_frame / SPINNER_SPEED_DIVISOR,
            &state.wrap_cache,
        );
        for styled_line in styled_lines {
            lines.push(convert_styled_line(styled_line));
        }
        // Add blank line between cells
        lines.push(Line::default());
    }

    lines
}

/// Converts a transcript StyledLine to a ratatui Line.
fn convert_styled_line(styled_line: StyledLine) -> Line<'static> {
    let spans: Vec<Span<'static>> = styled_line
        .spans
        .into_iter()
        .map(|s| {
            let style = convert_style(s.style);
            Span::styled(s.text, style)
        })
        .collect();
    Line::from(spans)
}

/// Converts a transcript Style to a ratatui Style.
fn convert_style(style: TranscriptStyle) -> Style {
    match style {
        TranscriptStyle::Plain => Style::default(),
        TranscriptStyle::UserPrefix => Style::default()
            .fg(Color::Green)
            .add_modifier(Modifier::BOLD),
        TranscriptStyle::User => Style::default()
            .fg(Color::Green)
            .add_modifier(Modifier::ITALIC),
        TranscriptStyle::AssistantPrefix => Style::default()
            .fg(Color::Blue)
            .add_modifier(Modifier::BOLD),
        TranscriptStyle::Assistant => Style::default().fg(Color::White),
        TranscriptStyle::StreamingCursor => Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::SLOW_BLINK),
        TranscriptStyle::SystemPrefix => Style::default()
            .fg(Color::Magenta)
            .add_modifier(Modifier::BOLD),
        TranscriptStyle::System => Style::default().fg(Color::DarkGray),
        TranscriptStyle::ToolBracket => Style::default().fg(Color::Gray),
        TranscriptStyle::ToolStatus => Style::default()
            .fg(Color::White)
            .add_modifier(Modifier::BOLD),
        TranscriptStyle::ToolError => Style::default().fg(Color::Red),
        TranscriptStyle::ToolRunning => Style::default().fg(Color::Cyan),
        TranscriptStyle::ToolSuccess => Style::default()
            .fg(Color::Green)
            .add_modifier(Modifier::BOLD),
        TranscriptStyle::ToolCancelled => Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::CROSSED_OUT | Modifier::BOLD),
        TranscriptStyle::ToolOutput => Style::default().fg(Color::DarkGray),
        TranscriptStyle::ThinkingPrefix => Style::default()
            .fg(Color::Magenta)
            .add_modifier(Modifier::DIM),
        TranscriptStyle::Thinking => Style::default()
            .fg(Color::DarkGray)
            .add_modifier(Modifier::DIM | Modifier::ITALIC),
    }
}

/// Returns the total line count from the transcript rendering.
/// Called after view() to update cached_line_count in state.
/// Takes the raw terminal width - margins are applied internally.
pub fn calculate_line_count(state: &TuiState, terminal_width: usize) -> usize {
    let effective_width = terminal_width.saturating_sub((TRANSCRIPT_MARGIN * 2) as usize);
    render_transcript(state, effective_width).len()
}

/// Calculates the available height for the transcript given the terminal height.
/// Encapsulates layout logic so callers don't need to know about INPUT_HEIGHT/STATUS_HEIGHT.
pub fn calculate_transcript_height(terminal_height: u16) -> usize {
    terminal_height.saturating_sub(INPUT_HEIGHT + STATUS_HEIGHT) as usize
}
