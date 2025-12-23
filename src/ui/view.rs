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
    widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap},
};

use crate::models::AVAILABLE_MODELS;
use crate::ui::state::{
    AuthType, CommandPaletteState, EngineState, LoginState, ModelPickerState, ScrollMode, TuiState,
};
use crate::ui::transcript::{Style as TranscriptStyle, StyledLine};

/// Height of the input area (lines).
pub const INPUT_HEIGHT: u16 = 5;

/// Height of header area (lines: title + status + border).
pub const HEADER_HEIGHT: u16 = 3;

/// Spinner speed divisor (render frames per spinner frame).
/// At 30fps render rate, 3 gives ~10fps spinner animation.
const SPINNER_SPEED_DIVISOR: usize = 3;

/// Renders the entire TUI to the frame.
///
/// This is a pure render function - it only reads state and draws to frame.
/// No mutations, no side effects.
pub fn view(state: &TuiState, frame: &mut Frame) {
    let area = frame.area();

    // Get terminal size for transcript rendering
    let transcript_width = area.width.saturating_sub(2) as usize;

    // Calculate transcript pane height
    let transcript_height = area.height.saturating_sub(HEADER_HEIGHT + INPUT_HEIGHT) as usize;

    // Pre-render transcript lines
    let all_lines = render_transcript(state, transcript_width);
    let total_lines = all_lines.len();

    // Calculate scroll offset based on mode
    let scroll_offset = match &state.scroll_mode {
        ScrollMode::FollowLatest => {
            // Show bottom of transcript
            total_lines.saturating_sub(transcript_height)
        }
        ScrollMode::Anchored { offset } => {
            // Clamp to valid range
            let max_offset = total_lines.saturating_sub(transcript_height);
            (*offset).min(max_offset)
        }
    };

    // Check if there's content below the viewport (for indicator)
    let has_content_below = scroll_offset + transcript_height < total_lines;

    // Slice visible lines
    let visible_end = (scroll_offset + transcript_height).min(total_lines);
    let visible_lines: Vec<Line<'static>> = all_lines
        .into_iter()
        .skip(scroll_offset)
        .take(visible_end - scroll_offset)
        .collect();

    // Create layout: header, transcript, input
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(HEADER_HEIGHT), // Header
            Constraint::Min(1),                // Transcript
            Constraint::Length(INPUT_HEIGHT),  // Input
        ])
        .split(area);

    // Render header
    render_header(state, frame, chunks[0], has_content_below);

    // Transcript area (already sliced to visible)
    let transcript = Paragraph::new(visible_lines)
        .wrap(Wrap { trim: false })
        .block(Block::default().borders(Borders::NONE));
    frame.render_widget(transcript, chunks[1]);

    // Input area
    frame.render_widget(&state.textarea, chunks[2]);

    // Render overlays (last, so they appear on top)
    if let Some(palette) = &state.command_palette {
        render_command_palette(frame, palette, area, chunks[2].y);
    }

    if let Some(picker) = &state.model_picker {
        render_model_picker(frame, picker, area, chunks[2].y);
    }

    if state.login_state.is_active() {
        render_login_overlay(frame, &state.login_state, area);
    }
}

/// Renders the header section (title + status line).
fn render_header(state: &TuiState, frame: &mut Frame, area: Rect, has_content_below: bool) {
    // Header line 1: Title and scroll indicator
    let mut title_spans = vec![
        Span::styled(
            "ZDX",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(" — "),
        Span::styled("q", Style::default().fg(Color::Yellow)),
        Span::raw(" to quit"),
    ];
    if has_content_below {
        title_spans.push(Span::raw("  "));
        title_spans.push(Span::styled("▼ more", Style::default().fg(Color::DarkGray)));
    }

    // Header line 2: Status line
    let status_state = match &state.engine_state {
        EngineState::Idle => ("Ready", Color::Green),
        EngineState::Waiting { .. } => ("Thinking...", Color::Yellow),
        EngineState::Streaming { .. } => ("Streaming...", Color::Yellow),
    };

    let auth_indicator = match state.auth_type {
        AuthType::OAuth => ("●", Color::Green, "OAuth"),
        AuthType::ApiKey => ("●", Color::Blue, "API"),
        AuthType::None => ("○", Color::Red, "No Auth"),
    };

    let mut status_spans = vec![
        Span::styled(&state.config.model, Style::default().fg(Color::DarkGray)),
        Span::raw(" "),
        Span::styled(auth_indicator.0, Style::default().fg(auth_indicator.1)),
        Span::styled(
            format!(" {}", auth_indicator.2),
            Style::default().fg(Color::DarkGray),
        ),
        Span::raw(" │ "),
        Span::styled(status_state.0, Style::default().fg(status_state.1)),
    ];

    if let Some(idx) = state.history_index {
        let total = state.command_history.len();
        status_spans.push(Span::raw(" │ "));
        status_spans.push(Span::styled(
            format!("history {}/{}", idx + 1, total),
            Style::default().fg(Color::DarkGray),
        ));
    }

    if state.command_palette.is_some() {
        status_spans.push(Span::raw(" │ "));
        status_spans.push(Span::styled(
            "/ Commands (Esc to cancel)",
            Style::default().fg(Color::Yellow),
        ));
    }

    let header = Paragraph::new(vec![Line::from(title_spans), Line::from(status_spans)])
        .alignment(Alignment::Center)
        .block(
            Block::default()
                .borders(Borders::BOTTOM)
                .border_style(Style::default().fg(Color::DarkGray)),
        );
    frame.render_widget(header, area);
}

/// Renders the transcript into ratatui Lines.
fn render_transcript(state: &TuiState, width: usize) -> Vec<Line<'static>> {
    let mut lines = Vec::new();

    for cell in &state.transcript {
        let styled_lines = cell.display_lines(width, state.spinner_frame / SPINNER_SPEED_DIVISOR);
        for styled_line in styled_lines {
            lines.push(convert_styled_line(styled_line));
        }
        // Add blank line between cells
        lines.push(Line::default());
    }

    // Remove trailing blank line if not waiting or streaming
    let is_active = matches!(
        state.engine_state,
        EngineState::Waiting { .. } | EngineState::Streaming { .. }
    );
    if !is_active && lines.last().map(|l| l.spans.is_empty()).unwrap_or(false) {
        lines.pop();
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
            .add_modifier(Modifier::CROSSED_OUT),
        TranscriptStyle::ToolOutput => Style::default().fg(Color::DarkGray),
    }
}

/// Renders the command palette as an overlay.
fn render_command_palette(
    frame: &mut Frame,
    palette: &CommandPaletteState,
    area: Rect,
    input_top_y: u16,
) {
    let commands = palette.filtered_commands();

    // Calculate palette dimensions
    let palette_width = 50.min(area.width.saturating_sub(4));
    let palette_height = (commands.len() as u16 + 6).max(7).min(area.height / 2);

    // Available vertical space (between header and input)
    let available_top = HEADER_HEIGHT;
    let available_bottom = input_top_y;
    let available_height = available_bottom.saturating_sub(available_top);

    // Position: centered both horizontally and vertically
    let palette_x = (area.width.saturating_sub(palette_width)) / 2;
    let palette_y = available_top + (available_height.saturating_sub(palette_height)) / 2;

    let palette_area = Rect::new(palette_x, palette_y, palette_width, palette_height);

    // Clear the area behind the palette
    frame.render_widget(Clear, palette_area);

    // Render outer border
    let outer_block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Yellow))
        .title(" Commands ")
        .title_style(
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        );
    frame.render_widget(outer_block, palette_area);

    // Inner area (inside border)
    let inner_area = Rect::new(
        palette_area.x + 1,
        palette_area.y + 1,
        palette_area.width.saturating_sub(2),
        palette_area.height.saturating_sub(2),
    );

    // Filter input line at TOP
    let max_filter_len = inner_area.width.saturating_sub(4) as usize;
    let filter_display = if palette.filter.is_empty() {
        "/".to_string()
    } else if palette.filter.len() > max_filter_len {
        let truncated = &palette.filter[palette.filter.len() - max_filter_len..];
        format!("/…{}", truncated)
    } else {
        format!("/{}", palette.filter)
    };
    let filter_line = Line::from(vec![
        Span::styled("> ", Style::default().fg(Color::DarkGray)),
        Span::styled(&filter_display, Style::default().fg(Color::Yellow)),
        Span::styled("█", Style::default().fg(Color::Yellow)),
    ]);
    let filter_para = Paragraph::new(filter_line);
    let filter_area = Rect::new(inner_area.x, inner_area.y, inner_area.width, 1);
    frame.render_widget(filter_para, filter_area);

    // Separator line
    let separator = "─".repeat(inner_area.width as usize);
    let separator_line = Paragraph::new(Line::from(Span::styled(
        &separator,
        Style::default().fg(Color::DarkGray),
    )));
    let separator_area = Rect::new(inner_area.x, inner_area.y + 1, inner_area.width, 1);
    frame.render_widget(separator_line, separator_area);

    // Command list area
    let list_height = inner_area.height.saturating_sub(4);
    let list_area = Rect::new(
        inner_area.x,
        inner_area.y + 2,
        inner_area.width,
        list_height,
    );

    // Build the list items
    let items: Vec<ListItem> = if commands.is_empty() {
        vec![ListItem::new(Line::from(Span::styled(
            "  No matching commands",
            Style::default().fg(Color::DarkGray),
        )))]
    } else {
        commands
            .iter()
            .map(|cmd| {
                let name = cmd.display_name();
                let desc = cmd.description;
                let line = Line::from(vec![
                    Span::styled(
                        format!("{:<16}", name),
                        Style::default()
                            .fg(Color::Cyan)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(desc, Style::default().fg(Color::White)),
                ]);
                ListItem::new(line)
            })
            .collect()
    };

    let list = List::new(items)
        .highlight_style(
            Style::default()
                .bg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("▶ ");

    let mut list_state = ListState::default();
    if !commands.is_empty() {
        list_state.select(Some(palette.selected));
    }
    frame.render_stateful_widget(list, list_area, &mut list_state);

    // Bottom separator
    let bottom_sep_y = inner_area.y + 2 + list_height;
    if bottom_sep_y < inner_area.y + inner_area.height {
        let bottom_separator_area = Rect::new(inner_area.x, bottom_sep_y, inner_area.width, 1);
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                &separator,
                Style::default().fg(Color::DarkGray),
            ))),
            bottom_separator_area,
        );
    }

    // Keyboard hints at the bottom
    let hints_y = inner_area.y + inner_area.height.saturating_sub(1);
    let hints_area = Rect::new(inner_area.x, hints_y, inner_area.width, 1);
    let hints_line = Line::from(vec![
        Span::styled("↑↓", Style::default().fg(Color::Yellow)),
        Span::styled(" navigate ", Style::default().fg(Color::DarkGray)),
        Span::styled("•", Style::default().fg(Color::DarkGray)),
        Span::styled(" Enter", Style::default().fg(Color::Yellow)),
        Span::styled(" select ", Style::default().fg(Color::DarkGray)),
        Span::styled("•", Style::default().fg(Color::DarkGray)),
        Span::styled(" Esc", Style::default().fg(Color::Yellow)),
        Span::styled(" cancel", Style::default().fg(Color::DarkGray)),
    ]);
    let hints_para = Paragraph::new(hints_line).alignment(Alignment::Center);
    frame.render_widget(hints_para, hints_area);
}

/// Renders the model picker as an overlay.
fn render_model_picker(frame: &mut Frame, picker: &ModelPickerState, area: Rect, input_top_y: u16) {
    let picker_width = 30.min(area.width.saturating_sub(4));
    let picker_height = (AVAILABLE_MODELS.len() as u16 + 5).min(area.height / 2);

    let available_top = HEADER_HEIGHT;
    let available_bottom = input_top_y;
    let available_height = available_bottom.saturating_sub(available_top);

    let picker_x = (area.width.saturating_sub(picker_width)) / 2;
    let picker_y = available_top + (available_height.saturating_sub(picker_height)) / 2;

    let picker_area = Rect::new(picker_x, picker_y, picker_width, picker_height);

    frame.render_widget(Clear, picker_area);

    let outer_block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Magenta))
        .title(" Select Model ")
        .title_style(
            Style::default()
                .fg(Color::Magenta)
                .add_modifier(Modifier::BOLD),
        );
    frame.render_widget(outer_block, picker_area);

    let inner_area = Rect::new(
        picker_area.x + 1,
        picker_area.y + 1,
        picker_area.width.saturating_sub(2),
        picker_area.height.saturating_sub(2),
    );

    let list_height = inner_area.height.saturating_sub(2);
    let list_area = Rect::new(inner_area.x, inner_area.y, inner_area.width, list_height);

    let items: Vec<ListItem> = AVAILABLE_MODELS
        .iter()
        .map(|model| {
            let line = Line::from(Span::styled(
                model.display_name,
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ));
            ListItem::new(line)
        })
        .collect();

    let list = List::new(items)
        .highlight_style(
            Style::default()
                .bg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("▶ ");

    let mut list_state = ListState::default();
    list_state.select(Some(picker.selected));
    frame.render_stateful_widget(list, list_area, &mut list_state);

    // Separator line
    let separator = "─".repeat(inner_area.width as usize);
    let sep_y = inner_area.y + list_height;
    if sep_y < inner_area.y + inner_area.height {
        let separator_area = Rect::new(inner_area.x, sep_y, inner_area.width, 1);
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                &separator,
                Style::default().fg(Color::DarkGray),
            ))),
            separator_area,
        );
    }

    // Keyboard hints
    let hints_y = inner_area.y + inner_area.height.saturating_sub(1);
    let hints_area = Rect::new(inner_area.x, hints_y, inner_area.width, 1);
    let hints_line = Line::from(vec![
        Span::styled("↑↓", Style::default().fg(Color::Magenta)),
        Span::styled(" navigate ", Style::default().fg(Color::DarkGray)),
        Span::styled("•", Style::default().fg(Color::DarkGray)),
        Span::styled(" Enter", Style::default().fg(Color::Magenta)),
        Span::styled(" select ", Style::default().fg(Color::DarkGray)),
        Span::styled("•", Style::default().fg(Color::DarkGray)),
        Span::styled(" Esc", Style::default().fg(Color::Magenta)),
        Span::styled(" cancel", Style::default().fg(Color::DarkGray)),
    ]);
    let hints_para = Paragraph::new(hints_line).alignment(Alignment::Center);
    frame.render_widget(hints_para, hints_area);
}

/// Renders the login overlay.
fn render_login_overlay(frame: &mut Frame, login_state: &LoginState, area: Rect) {
    let popup_width = 60.min(area.width.saturating_sub(4));
    let popup_height = 9.min(area.height.saturating_sub(4));
    let popup_x = (area.width.saturating_sub(popup_width)) / 2;
    let popup_y = (area.height.saturating_sub(popup_height)) / 2;
    let popup_area = Rect::new(popup_x, popup_y, popup_width, popup_height);

    frame.render_widget(Clear, popup_area);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan))
        .title(" Anthropic Login ")
        .title_style(
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        );
    frame.render_widget(block, popup_area);

    let inner = Rect::new(
        popup_area.x + 2,
        popup_area.y + 1,
        popup_area.width.saturating_sub(4),
        popup_area.height.saturating_sub(2),
    );

    let lines: Vec<Line> = match login_state {
        LoginState::Idle => return,
        LoginState::AwaitingCode {
            url, input, error, ..
        } => {
            let display_url = truncate_middle(url, inner.width.saturating_sub(2) as usize);

            let mut l = vec![
                Line::from(Span::styled(
                    "Browser opened for authentication.",
                    Style::default().fg(Color::Green),
                )),
                Line::from(Span::styled(
                    display_url,
                    Style::default().fg(Color::DarkGray),
                )),
                Line::from(""),
                Line::from(Span::styled(
                    "Paste auth code:",
                    Style::default().fg(Color::White),
                )),
                Line::from(Span::styled(
                    format!("> {}█", input),
                    Style::default().fg(Color::Yellow),
                )),
            ];
            if let Some(e) = error {
                l.push(Line::from(""));
                l.push(Line::from(Span::styled(
                    e.as_str(),
                    Style::default().fg(Color::Red),
                )));
            }
            l.push(Line::from(""));
            l.push(Line::from(Span::styled(
                "Esc to cancel",
                Style::default().fg(Color::DarkGray),
            )));
            l
        }
        LoginState::Exchanging => vec![
            Line::from(""),
            Line::from(Span::styled(
                "Exchanging code...",
                Style::default().fg(Color::Yellow),
            )),
            Line::from(""),
            Line::from(Span::styled(
                "Esc to cancel",
                Style::default().fg(Color::DarkGray),
            )),
        ],
    };

    let para = Paragraph::new(lines);
    frame.render_widget(para, inner);
}

/// Truncates a string in the middle with "..." if too long.
fn truncate_middle(s: &str, max_len: usize) -> String {
    if s.len() <= max_len || max_len < 10 {
        return s.to_string();
    }
    let half = (max_len - 3) / 2;
    format!("{}...{}", &s[..half], &s[s.len() - half..])
}

/// Returns the total line count from the transcript rendering.
/// Called after view() to update cached_line_count in state.
pub fn calculate_line_count(state: &TuiState, width: usize) -> usize {
    render_transcript(state, width).len()
}
