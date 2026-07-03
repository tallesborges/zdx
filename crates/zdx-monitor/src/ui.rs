use std::time::Duration;

use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, Paragraph, Tabs, Wrap};
use zdx_engine::core::usage_stats::{UsageRow, UsageStats, UsageTotals};

use crate::app::{CachedUsageStats, ConfigLine, MonitorApp, Section};

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
        Section::Usage => render_usage(f, app, chunks[1]),
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
            let line = {
                let mut display_details = s.details.clone();
                if app.supervised_services.contains(&s.key) {
                    if display_details.is_empty() {
                        display_details = "supervised".to_string();
                    } else {
                        display_details = format!("{display_details} · supervised");
                    }
                }
                if display_details.is_empty() {
                    format!(" {:<10} {icon} {}", s.name, s.status)
                } else {
                    format!(
                        " {:<10} {icon} {:<10} {}",
                        s.name, s.status, display_details
                    )
                }
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
            .title("Services (Enter=toggle, r=restart, ^R=supervise)"),
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
        Section::Services => {
            "↑↓ navigate • Enter toggle • r restart • ^R supervise • Tab switch • q quit"
        }
        Section::ActiveAgents | Section::Automations => "↑↓ navigate • Tab switch • q quit",
        Section::Config => "↑↓ scroll • PgUp/PgDn page • Tab switch • q quit",
        Section::Threads => "↑↓ navigate • y copy thread ID • Tab switch • q quit",
        Section::Usage => "↑↓ scroll • PgUp/PgDn page • R refresh • Tab switch • q quit",
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
        let percent = (current_scroll * 100)
            .checked_div(max_scroll)
            .unwrap_or(100);
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

fn render_usage(f: &mut Frame, app: &MonitorApp, area: Rect) {
    let Some(cached) = &app.usage_stats else {
        let p = Paragraph::new(" Computing usage stats…")
            .style(Style::default().fg(Color::DarkGray))
            .block(Block::default().borders(Borders::ALL).title(" Usage "));
        f.render_widget(p, area);
        return;
    };

    let lines = build_usage_lines(cached);
    let total_lines = lines.len();
    let visible_lines = area.height.saturating_sub(2) as usize;
    let max_scroll = total_lines.saturating_sub(visible_lines);
    let scroll = app.usage_scroll.min(max_scroll);

    let scroll_info = if total_lines > visible_lines {
        let percent = (scroll * 100).checked_div(max_scroll).unwrap_or(100);
        format!(" [{percent}%]")
    } else {
        String::new()
    };
    let refreshing = if app.usage_rx.is_some() {
        " · refreshing"
    } else {
        ""
    };
    let title = format!(
        " Usage — {} thread(s) scanned{scroll_info}{refreshing} ",
        cached.stats.threads_scanned
    );

    let p = Paragraph::new(Text::from(lines))
        .block(Block::default().borders(Borders::ALL).title(title))
        .scroll((scroll as u16, 0));
    f.render_widget(p, area);
}

/// Rendered line count of the cached usage view, used for scroll clamping.
pub(crate) fn usage_line_count(cached: &CachedUsageStats) -> usize {
    build_usage_lines(cached).len()
}

/// Build the styled display lines for the Usage tab. Mirrors the `zdx stats`
/// CLI output so both surfaces show identical numbers.
fn build_usage_lines(cached: &CachedUsageStats) -> Vec<Line<'static>> {
    let stats = &cached.stats;
    let mut lines = usage_banner_lines(cached);

    if stats.threads_scanned == 0 || stats.totals.requests == 0 {
        lines.push(Line::from(format!(
            "No usage found in {} thread(s).",
            stats.threads_scanned
        )));
        push_usage_warnings(&mut lines, stats);
        return lines;
    }

    lines.extend(usage_totals_lines(&stats.totals));
    lines.push(Line::from(""));
    lines.extend(usage_table("By provider:", None, &stats.by_provider));
    lines.push(Line::from(""));
    lines.extend(usage_table("By model:", Some("MODEL"), &stats.by_model));

    if stats.by_model.iter().any(|row| row.estimated) {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "* estimated — attributed without a per-request provider (older usage or fallback).",
            Style::default().fg(Color::DarkGray),
        )));
    }

    push_usage_warnings(&mut lines, stats);
    lines
}

/// The banner/header block shown above the tables (title, scope, freshness).
fn usage_banner_lines(cached: &CachedUsageStats) -> Vec<Line<'static>> {
    let dim = Style::default().fg(Color::DarkGray);
    vec![
        Line::from(Span::styled(
            "zdx usage stats (estimated)",
            Style::default().add_modifier(Modifier::BOLD),
        )),
        Line::from(Span::styled(
            "Global across all ZDX threads under $ZDX_HOME/threads.",
            dim,
        )),
        Line::from(Span::styled(
            "Estimated: old usage lacks per-request model/provider; subagent/helper + image \
             spend excluded; subscription providers shown as flat-rate.",
            dim,
        )),
        Line::from(Span::styled(
            format!(
                "Updated {} ago · press R to refresh",
                format_age(cached.computed_at.elapsed())
            ),
            dim,
        )),
        Line::from(""),
    ]
}

/// Overall totals (request/token counts and billed/subscription summary).
fn usage_totals_lines(t: &UsageTotals) -> Vec<Line<'static>> {
    vec![
        Line::from(format!(
            "Overall: {} requests · {} tokens (in {} / out {} / cache-r {} / cache-w {})",
            t.requests,
            format_usage_tokens(t.tokens()),
            format_usage_tokens(t.input),
            format_usage_tokens(t.output),
            format_usage_tokens(t.cache_read),
            format_usage_tokens(t.cache_write),
        )),
        Line::from(format!(
            "Billed: {}   Subscription tokens: {}   Unknown-pricing rows: {}",
            format_usage_cost(t.billed_usd),
            format_usage_tokens(t.subscription_tokens),
            t.unknown_pricing_rows,
        )),
    ]
}

/// A titled table of usage rows. When `model_header` is set the rows include a
/// leading model column (the by-model table); otherwise it's provider-only.
fn usage_table(title: &str, model_header: Option<&str>, rows: &[UsageRow]) -> Vec<Line<'static>> {
    let dim = Style::default().fg(Color::DarkGray);
    let mut lines: Vec<Line<'static>> = Vec::with_capacity(rows.len() + 2);
    lines.push(Line::from(Span::styled(
        title.to_string(),
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    )));
    let header = match model_header {
        Some(model) => format!(
            "  {model:<34} {:<16} {:>8} {:>10} {:>14}",
            "PROVIDER", "REQ", "TOKENS", "COST"
        ),
        None => format!(
            "  {:<16} {:>8} {:>10} {:>14}",
            "PROVIDER", "REQ", "TOKENS", "COST"
        ),
    };
    lines.push(Line::from(Span::styled(header, dim)));
    for row in rows {
        let line = if model_header.is_some() {
            format!(
                "  {:<34} {:<16} {:>8} {:>10} {:>14}",
                truncate_chars(row.model.as_deref().unwrap_or("-"), 34),
                truncate_chars(&row.provider, 16),
                row.requests,
                format_usage_tokens(row.tokens()),
                usage_cost_cell(row),
            )
        } else {
            format!(
                "  {:<16} {:>8} {:>10} {:>14}",
                truncate_chars(&row.provider, 16),
                row.requests,
                format_usage_tokens(row.tokens()),
                usage_cost_cell(row),
            )
        };
        lines.push(Line::from(line));
    }
    lines
}

fn push_usage_warnings(lines: &mut Vec<Line<'static>>, stats: &UsageStats) {
    if stats.warnings.is_empty() {
        return;
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        format!("{} thread(s) skipped:", stats.warnings.len()),
        Style::default().fg(Color::Yellow),
    )));
    for warning in &stats.warnings {
        lines.push(Line::from(Span::styled(
            format!("  - {warning}"),
            Style::default().fg(Color::DarkGray),
        )));
    }
}

fn usage_cost_cell(row: &UsageRow) -> String {
    let base = if row.subscription {
        "subscription".to_string()
    } else if !row.cost_known {
        "unknown".to_string()
    } else {
        format_usage_cost(row.cost_usd)
    };
    if row.estimated {
        format!("{base}*")
    } else {
        base
    }
}

fn format_usage_cost(cost: f64) -> String {
    format!("${cost:.2}")
}

fn format_usage_tokens(count: u64) -> String {
    if count >= 1_000_000_000 {
        format!("{:.1}B", count as f64 / 1_000_000_000.0)
    } else if count >= 1_000_000 {
        format!("{:.1}M", count as f64 / 1_000_000.0)
    } else if count >= 1_000 {
        format!("{:.1}k", count as f64 / 1_000.0)
    } else {
        count.to_string()
    }
}

fn format_age(elapsed: Duration) -> String {
    let secs = elapsed.as_secs();
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3600 {
        format!("{}m", secs / 60)
    } else {
        format!("{}h", secs / 3600)
    }
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
