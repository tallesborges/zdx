use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, Paragraph, Tabs, Wrap};

use crate::app::{ConfigLine, MonitorApp, Section};

pub fn render(f: &mut Frame, app: &MonitorApp) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(0),
            Constraint::Length(3),
        ])
        .split(f.area());

    render_tabs(f, app, chunks[0]);

    match app.active_section {
        Section::Services => render_services(f, app, chunks[1]),
        Section::ActiveAgents => render_active_agents(f, app, chunks[1]),
        Section::Config => render_config(f, app, chunks[1]),
        Section::Threads => render_threads(f, app, chunks[1]),
        Section::Automations => render_automations(f, app, chunks[1]),
        Section::Logs => render_logs(f, app, chunks[1]),
    }

    render_footer(f, app, chunks[2]);

    if app.log_overlay_open && app.active_section == Section::Logs {
        render_log_overlay(f, app, f.area());
    }
}

fn render_tabs(f: &mut Frame, app: &MonitorApp, area: Rect) {
    let titles: Vec<&str> = Section::ALL.iter().map(|s| s.label()).collect();
    let selected = Section::ALL
        .iter()
        .position(|s| *s == app.active_section)
        .unwrap_or(0);
    let tabs = Tabs::new(titles)
        .block(Block::default().borders(Borders::ALL).title("ZDX Monitor"))
        .select(selected)
        .highlight_style(Style::default().add_modifier(Modifier::BOLD | Modifier::UNDERLINED));
    f.render_widget(tabs, area);
}

fn render_services(f: &mut Frame, app: &MonitorApp, area: Rect) {
    let items: Vec<ListItem> = app
        .services
        .iter()
        .enumerate()
        .map(|(i, s)| {
            let (icon, style) = if s.status == "running" {
                ("●", Style::default().fg(Color::Green))
            } else {
                ("○", Style::default().fg(Color::DarkGray))
            };
            let line = if s.details.is_empty() {
                format!(" {:<10} {icon} {}", s.name, s.status)
            } else {
                format!(" {:<10} {icon} {:<10} {}", s.name, s.status, s.details)
            };
            let style = if i == app.selected_index && app.active_section == Section::Services {
                style.add_modifier(Modifier::REVERSED)
            } else {
                style
            };
            ListItem::new(line).style(style)
        })
        .collect();
    let list = List::new(items).block(
        Block::default()
            .borders(Borders::ALL)
            .title("Services (Enter=toggle, r=restart)"),
    );
    f.render_widget(list, area);
}

fn render_footer(f: &mut Frame, app: &MonitorApp, area: Rect) {
    let text = if !app.status_message.is_empty() && app.status_section == app.active_section {
        app.status_message.clone()
    } else {
        footer_hint(app.active_section).to_string()
    };
    let footer = Paragraph::new(text)
        .style(Style::default().fg(Color::DarkGray))
        .block(Block::default().borders(Borders::ALL).title("Hints"));
    f.render_widget(footer, area);
}

fn footer_hint(section: Section) -> &'static str {
    match section {
        Section::Services => "↑↓ navigate • Enter toggle • r restart • Tab switch • q quit",
        Section::ActiveAgents | Section::Automations => "↑↓ navigate • Tab switch • q quit",
        Section::Config => "↑↓ scroll • PgUp/PgDn page • Tab switch • q quit",
        Section::Threads => "↑↓ navigate • y copy thread ID • Tab switch • q quit",
        Section::Logs => {
            "↑↓ select • PgUp/PgDn page • Enter open • G/End follow • Tab switch • q quit"
        }
    }
}

fn render_active_agents(f: &mut Frame, app: &MonitorApp, area: Rect) {
    if app.active_agents.is_empty() {
        let p = Paragraph::new(" No active agent runs")
            .style(Style::default().fg(Color::DarkGray))
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title("Active Agents"),
            );
        f.render_widget(p, area);
        return;
    }

    let inner_width = area.width.saturating_sub(2) as usize;
    let items: Vec<ListItem> = app
        .active_agents
        .iter()
        .enumerate()
        .map(|(i, a)| {
            let role = a.kind.as_deref().unwrap_or(&a.surface);
            let role_label = if let Some(name) = a.subagent_name.as_deref() {
                format!("{role}:{name}")
            } else {
                role.to_string()
            };
            let prefix = format!(
                " ● PID {} {} model:",
                a.pid,
                truncate_chars(&role_label, 18)
            );
            let suffix = format!(" thread:{} up {}", a.thread_id, a.uptime);
            let model_width = inner_width.saturating_sub(prefix.len() + suffix.len());
            let model = truncate_chars(&a.model, model_width);
            let line = format!("{prefix}{model:<model_width$}{suffix}");
            let style = if i == app.selected_index {
                Style::default()
                    .fg(Color::Green)
                    .add_modifier(Modifier::REVERSED)
            } else {
                Style::default().fg(Color::Green)
            };
            ListItem::new(line).style(style)
        })
        .collect();

    let title = format!("Active Agents ({})", app.active_agents.len());
    let list = List::new(items).block(Block::default().borders(Borders::ALL).title(title));
    f.render_widget(list, area);
}

fn truncate_chars(value: &str, max_chars: usize) -> String {
    if value.chars().count() <= max_chars {
        return value.to_string();
    }

    if max_chars == 0 {
        String::new()
    } else if max_chars == 1 {
        "…".to_string()
    } else {
        format!("{}…", value.chars().take(max_chars - 1).collect::<String>())
    }
}

fn render_config(f: &mut Frame, app: &MonitorApp, area: Rect) {
    let inner_width = area.width.saturating_sub(2) as usize;
    let key_col = 30usize;

    let mut lines: Vec<Line> = Vec::new();
    let mut is_first = true;

    for cl in &app.config_lines {
        match cl {
            ConfigLine::Section(name) => {
                if !is_first {
                    lines.push(Line::from(""));
                }
                is_first = false;

                lines.push(Line::from(vec![Span::styled(
                    format!(" ── {} ", name.to_uppercase()),
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                )]));
            }
            ConfigLine::Separator => {
                let dashes = "─".repeat(inner_width.saturating_sub(4));
                lines.push(Line::from(Span::styled(
                    format!("    {dashes}"),
                    Style::default().fg(Color::DarkGray),
                )));
            }
            ConfigLine::Row(key, value) => {
                let val_style = if value == "***" || value.starts_with("***") {
                    Style::default().fg(Color::DarkGray)
                } else {
                    Style::default()
                };

                lines.push(Line::from(vec![
                    Span::styled(
                        format!("    {key:<key_col$} "),
                        Style::default().fg(Color::DarkGray),
                    ),
                    Span::styled(value.clone(), val_style),
                ]));
            }
        }
    }

    let total_fields = app
        .config_lines
        .iter()
        .filter(|l| matches!(l, ConfigLine::Row(..)))
        .count();

    let visible_lines = area.height.saturating_sub(2) as usize;
    let total_lines = lines.len();

    let scroll_info = if total_lines > visible_lines {
        let max_scroll = total_lines - visible_lines;
        let current_scroll = app.config_scroll.min(max_scroll);
        let percent = if max_scroll > 0 {
            (current_scroll * 100) / max_scroll
        } else {
            100
        };
        format!(" [{percent}%]")
    } else {
        String::new()
    };

    let title = format!(" Config ({total_fields} fields){scroll_info} ");

    let p = Paragraph::new(Text::from(lines))
        .block(Block::default().borders(Borders::ALL).title(title))
        .scroll((app.config_scroll as u16, 0));

    f.render_widget(p, area);
}

fn render_threads(f: &mut Frame, app: &MonitorApp, area: Rect) {
    let items: Vec<ListItem> = app
        .threads
        .iter()
        .enumerate()
        .map(|(i, t)| {
            let short_id = if t.id.len() > 8 { &t.id[..8] } else { &t.id };
            let surface = t.surface.as_deref().unwrap_or("-");
            let title = t.title.as_deref().unwrap_or("(untitled)");
            let line = format!(" [{short_id}] {} | {surface:<9} | {title}", t.modified);
            let style = if i == app.selected_index {
                Style::default().add_modifier(Modifier::REVERSED)
            } else {
                Style::default()
            };
            ListItem::new(line).style(style)
        })
        .collect();
    let list = List::new(items).block(
        Block::default()
            .borders(Borders::ALL)
            .title("Threads (y=copy ID)"),
    );
    f.render_widget(list, area);
}

fn render_automations(f: &mut Frame, app: &MonitorApp, area: Rect) {
    let items: Vec<ListItem> = app
        .automations
        .iter()
        .enumerate()
        .map(|(i, a)| {
            let sched = a.schedule.as_deref().unwrap_or("-");
            let line = format!(" {:<20} | {sched}", a.name);
            let style = if i == app.selected_index {
                Style::default().add_modifier(Modifier::REVERSED)
            } else {
                Style::default()
            };
            ListItem::new(line).style(style)
        })
        .collect();
    let list = List::new(items).block(Block::default().borders(Borders::ALL).title("Automations"));
    f.render_widget(list, area);
}

fn render_logs(f: &mut Frame, app: &MonitorApp, area: Rect) {
    if app.log_lines.is_empty() {
        let msg = match &app.log_file_name {
            Some(name) => format!(" {name} is empty"),
            None => " No log files found in ~/.zdx/logs".to_string(),
        };
        let p = Paragraph::new(msg)
            .style(Style::default().fg(Color::DarkGray))
            .block(Block::default().borders(Borders::ALL).title(" Logs "));
        f.render_widget(p, area);
        return;
    }

    let inner_width = area.width.saturating_sub(2) as usize;
    let visible_rows = area.height.saturating_sub(2) as usize;
    let total = app.log_lines.len();
    let selected = app.log_selected.min(total - 1);

    // Re-clamp the offset against the *actual* rendered area (terminal_height
    // is updated post-draw and may lag by one frame).
    let mut offset = app.log_offset;
    if offset > selected {
        offset = selected;
    } else if visible_rows > 0 && selected >= offset + visible_rows {
        offset = selected + 1 - visible_rows;
    }
    let end = (offset + visible_rows).min(total);

    let items: Vec<ListItem> = app.log_lines[offset..end]
        .iter()
        .enumerate()
        .map(|(i, raw)| {
            let global_index = offset + i;
            let spans = truncate_spans(log_line_spans(raw), inner_width);
            let item = ListItem::new(Line::from(spans));
            if global_index == selected {
                item.style(Style::default().bg(Color::DarkGray))
            } else {
                item
            }
        })
        .collect();

    let file_label = app.log_file_name.as_deref().unwrap_or("(no file)");
    let follow_tag = if app.log_follow { " · FOLLOW" } else { "" };
    let title = format!(
        " Logs ({file_label} · {pos}/{total}{follow_tag}) ",
        pos = selected + 1,
    );

    let list = List::new(items).block(Block::default().borders(Borders::ALL).title(title));
    f.render_widget(list, area);
}

fn render_log_overlay(f: &mut Frame, app: &MonitorApp, area: Rect) {
    let Some(line) = app.log_lines.get(app.log_selected) else {
        return;
    };

    let popup_area = centered_rect(80, 60, area);
    f.render_widget(Clear, popup_area);

    let title = format!(
        " Log entry [{pos}/{total}] · Esc close · y copy ",
        pos = app.log_selected + 1,
        total = app.log_lines.len(),
    );

    let body = Paragraph::new(Line::from(log_line_spans(line)))
        .wrap(Wrap { trim: false })
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Cyan))
                .title(title),
        );
    f.render_widget(body, popup_area);
}

/// Build a centered Rect using `percent_x` × `percent_y` of `area`.
fn centered_rect(percent_x: u16, percent_y: u16, area: Rect) -> Rect {
    let popup_w = area.width.saturating_mul(percent_x) / 100;
    let popup_h = area.height.saturating_mul(percent_y) / 100;
    let x = area.x + area.width.saturating_sub(popup_w) / 2;
    let y = area.y + area.height.saturating_sub(popup_h) / 2;
    Rect {
        x,
        y,
        width: popup_w,
        height: popup_h,
    }
}

/// Components of a tracing compact-format log line.
struct LogParts<'a> {
    timestamp: &'a str,
    level: &'a str,
    target: &'a str,
    message: &'a str,
    structured: bool,
}

/// Split a log line into `<timestamp> <LEVEL> <target> <message>`. Falls back
/// to `structured = false` when the line doesn't match that shape.
fn parse_log_line(line: &str) -> LogParts<'_> {
    let bytes = line.as_bytes();
    let len = bytes.len();
    let mut i = 0;

    while i < len && bytes[i].is_ascii_whitespace() {
        i += 1;
    }
    let timestamp_start = i;
    while i < len && !bytes[i].is_ascii_whitespace() {
        i += 1;
    }
    let timestamp_end = i;
    while i < len && bytes[i].is_ascii_whitespace() {
        i += 1;
    }
    let level_start = i;
    while i < len && !bytes[i].is_ascii_whitespace() {
        i += 1;
    }
    let level_end = i;
    while i < len && bytes[i].is_ascii_whitespace() {
        i += 1;
    }
    let target_start = i;
    while i < len && !bytes[i].is_ascii_whitespace() {
        i += 1;
    }
    let target_end = i;
    while i < len && bytes[i].is_ascii_whitespace() {
        i += 1;
    }

    let timestamp = &line[timestamp_start..timestamp_end];
    let level = &line[level_start..level_end];
    let target = &line[target_start..target_end];
    let message = &line[i..];

    let structured =
        matches!(level, "ERROR" | "WARN" | "INFO" | "DEBUG" | "TRACE") && target.ends_with(':');

    LogParts {
        timestamp,
        level,
        target,
        message,
        structured,
    }
}

/// Build colored spans for a single log line.
///
/// Coloring:
/// - timestamp: dark gray
/// - level: ERROR=red+bold, WARN=yellow+bold, INFO=green+bold, DEBUG=cyan, TRACE=magenta
/// - target (`module::path:`): cyan
/// - message: red for ERROR, dark gray for DEBUG/TRACE, default otherwise
fn log_line_spans(line: &str) -> Vec<Span<'static>> {
    let parts = parse_log_line(line);
    if !parts.structured {
        let style = match parts.level {
            "ERROR" => Style::default().fg(Color::Red),
            "WARN" => Style::default().fg(Color::Yellow),
            "DEBUG" | "TRACE" => Style::default().fg(Color::DarkGray),
            _ => Style::default(),
        };
        return vec![Span::styled(line.to_string(), style)];
    }

    let level_style = match parts.level {
        "ERROR" => Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        "WARN" => Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD),
        "INFO" => Style::default()
            .fg(Color::Green)
            .add_modifier(Modifier::BOLD),
        "DEBUG" => Style::default().fg(Color::Cyan),
        "TRACE" => Style::default().fg(Color::Magenta),
        _ => Style::default(),
    };
    let message_style = match parts.level {
        "ERROR" => Style::default().fg(Color::Red),
        "DEBUG" | "TRACE" => Style::default().fg(Color::DarkGray),
        _ => Style::default(),
    };

    vec![
        Span::styled(
            parts.timestamp.to_string(),
            Style::default().fg(Color::DarkGray),
        ),
        Span::raw(" "),
        Span::styled(parts.level.to_string(), level_style),
        Span::raw(" "),
        Span::styled(parts.target.to_string(), Style::default().fg(Color::Cyan)),
        Span::raw(" "),
        Span::styled(parts.message.to_string(), message_style),
    ]
}

/// Truncate a span sequence to `max_chars` total characters, replacing the
/// overflow with `…`. Preserves per-span styling.
fn truncate_spans(spans: Vec<Span<'static>>, max_chars: usize) -> Vec<Span<'static>> {
    if max_chars == 0 {
        return Vec::new();
    }
    let total: usize = spans.iter().map(|s| s.content.chars().count()).sum();
    if total <= max_chars {
        return spans;
    }
    let limit = max_chars.saturating_sub(1); // reserve 1 char for the ellipsis
    let mut out: Vec<Span<'static>> = Vec::with_capacity(spans.len() + 1);
    let mut used = 0usize;
    for span in spans {
        let span_len = span.content.chars().count();
        if used + span_len <= limit {
            used += span_len;
            out.push(span);
        } else {
            let take = limit.saturating_sub(used);
            if take > 0 {
                let truncated: String = span.content.chars().take(take).collect();
                out.push(Span::styled(truncated, span.style));
            }
            break;
        }
    }
    out.push(Span::styled(
        "…".to_string(),
        Style::default().fg(Color::DarkGray),
    ));
    out
}
