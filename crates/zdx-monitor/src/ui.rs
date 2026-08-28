use std::time::Duration;

use chrono::{DateTime, Utc};
use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, Paragraph, Tabs};
use zdx_engine::core::usage_stats::{self, DailyUsage, UsageRow, UsageStats, UsageTotals};
use zdx_engine::providers::subscription_quota::{QuotaWindow, account_display};

use crate::app::{
    BackgroundDetailState, CachedQuotas, CachedUsageStats, MonitorApp, QuotaEntry, Section,
    TargetPickerState, UsageSpan,
};
use crate::tabs::agents::{render_active_agents, render_agent_overlay};
use crate::tabs::config::{render_config, render_model_picker};
use crate::tabs::logs::{render_log_overlay, render_logs};
use crate::tabs::threads::{render_threads, render_timing_overlay};

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
        Section::Background => render_background(f, app, chunks[1]),
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

    if app.active_section == Section::Logs
        && let Some(picker) = &app.log_target_picker
    {
        render_picker(f, picker, f.area(), "target");
    }

    if app.active_section == Section::Threads
        && let Some(picker) = &app.thread_project_picker
    {
        render_picker(f, picker, f.area(), "project");
    }

    if let Some(state) = &app.agent_overlay {
        render_agent_overlay(f, state, f.area());
    }

    if let Some(state) = &app.timing_overlay {
        render_timing_overlay(f, state, f.area());
    }

    if let Some(state) = &app.background_detail {
        render_background_detail(f, state, f.area());
    }

    if let Some(picker) = &app.model_picker {
        render_model_picker(f, picker, f.area());
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
                let display_details = &s.details;
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
                style.bg(SELECTED_BG)
            } else {
                style
            };
            ListItem::new(line).style(style)
        })
        .collect();
    let list = List::new(items).block(
        Block::default()
            .borders(Borders::ALL)
            .title("Services (Enter=toggle, r=restart, R=force)"),
    );
    f.render_widget(list, area);
}

fn render_footer(f: &mut Frame, app: &MonitorApp, area: Rect) {
    let text = if app.active_section == Section::Logs && app.log_query_editing {
        format!(
            "search: {}\u{2588}  (Enter accept · Esc clear)",
            app.log_query
        )
    } else if !app.status_message.is_empty() && app.status_section == app.active_section {
        app.status_message.clone()
    } else {
        format!("{} • M mouse", footer_hint(app.active_section))
    };
    let footer = Paragraph::new(text)
        .style(Style::default().fg(Color::DarkGray))
        .block(Block::default().borders(Borders::ALL).title("Hints"));
    f.render_widget(footer, area);
}

fn footer_hint(section: Section) -> &'static str {
    match section {
        Section::Services => {
            "↑↓ navigate • Enter toggle • r restart • R force • Tab/⇧Tab switch • q quit"
        }
        Section::ActiveAgents => "↑↓ navigate • Enter inspect • Tab/⇧Tab switch • q quit",
        Section::Background => "↑↓ navigate • Enter details • x kill • Tab/⇧Tab switch • q quit",
        Section::Automations => "↑↓ navigate • Tab/⇧Tab switch • q quit",
        Section::Config => {
            "↑↓ select model • Enter edit • d delete favorite / reset subagent • PgUp/PgDn scroll • Tab/⇧Tab switch • q quit"
        }
        Section::Threads => {
            "↑↓ navigate • Enter preview • o raw • i timings • t kind • p project • / search • Esc clear • y copy ID"
        }
        Section::Usage => {
            "↑↓ scroll • PgUp/PgDn page • t span • R refresh • Tab/⇧Tab switch • q quit"
        }
        Section::Logs => {
            "↑↓ select • / search • l level • f target • [ ] file • L tail • Esc clear • Enter open • G follow • Tab switch • q quit"
        }
    }
}

/// Spinner frame index derived from wall-clock seconds: the monitor redraws
/// on its 1s tick, so the running-tool glyph advances one frame per tick.
pub(crate) fn spinner_frame_now() -> usize {
    usize::try_from(chrono::Utc::now().timestamp().max(0)).unwrap_or(0)
}

fn render_background(f: &mut Frame, app: &MonitorApp, area: Rect) {
    if app.background.is_empty() {
        let p = Paragraph::new(" No background processes")
            .style(Style::default().fg(Color::DarkGray))
            .block(Block::default().borders(Borders::ALL).title("Background"));
        f.render_widget(p, area);
        return;
    }

    let inner_width = area.width.saturating_sub(2) as usize;
    let mut items: Vec<ListItem> = Vec::new();
    let mut last_thread: Option<&Option<String>> = None;
    for (i, b) in app.background.iter().enumerate() {
        if last_thread != Some(&b.thread_id) {
            last_thread = Some(&b.thread_id);
            let label = b.thread_id.as_deref().unwrap_or("(no thread)");
            items.push(
                ListItem::new(format!(" thread {label}")).style(
                    Style::default()
                        .fg(Color::DarkGray)
                        .add_modifier(Modifier::BOLD),
                ),
            );
        }
        let prefix = format!("   ● pid {:<7} up {:<8} ", b.pid, b.uptime);
        let cmd_width = inner_width.saturating_sub(prefix.chars().count());
        let cmd = truncate_chars(&b.command, cmd_width);
        let line = format!("{prefix}{cmd}");
        let style = if i == app.selected_index {
            Style::default().fg(Color::Green).bg(SELECTED_BG)
        } else {
            Style::default().fg(Color::Green)
        };
        items.push(ListItem::new(line).style(style));
    }

    let title = format!("Background processes ({})", app.background.len());
    let list = List::new(items).block(Block::default().borders(Borders::ALL).title(title));
    f.render_widget(list, area);
}

pub(crate) fn truncate_chars(value: &str, max_chars: usize) -> String {
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

/// Highlight for the selected row: a subtle background instead of a full
/// reverse-video block, which reads as a white bar on dark terminals.
pub(crate) const SELECTED_BG: Color = Color::Indexed(238);

fn render_usage(f: &mut Frame, app: &MonitorApp, area: Rect) {
    let Some(cached) = &app.usage_stats else {
        let p = Paragraph::new(" Computing usage stats…")
            .style(Style::default().fg(Color::DarkGray))
            .block(Block::default().borders(Borders::ALL).title(" Usage "));
        f.render_widget(p, area);
        return;
    };

    let lines = build_usage_lines(cached, app.quotas.as_ref(), app.usage_span);
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
        " Usage ({}) — {} thread(s) scanned{scroll_info}{refreshing} ",
        app.usage_span.label(),
        cached.stats.threads_scanned
    );

    let p = Paragraph::new(Text::from(lines))
        .block(Block::default().borders(Borders::ALL).title(title))
        .scroll((scroll as u16, 0));
    f.render_widget(p, area);
}

/// Rendered line count of the cached usage view, used for scroll clamping.
pub(crate) fn usage_line_count(
    cached: &CachedUsageStats,
    quotas: Option<&CachedQuotas>,
    span: UsageSpan,
) -> usize {
    build_usage_lines(cached, quotas, span).len()
}

/// Build the styled display lines for the Usage tab. Mirrors the `zdx stats`
/// CLI output so both surfaces show identical numbers, with a live subscription
/// quota block on top.
fn build_usage_lines(
    cached: &CachedUsageStats,
    quotas: Option<&CachedQuotas>,
    span: UsageSpan,
) -> Vec<Line<'static>> {
    let stats = &cached.stats;
    let mut lines = subscription_lines(quotas);
    lines.extend(usage_banner_lines(cached, span.label()));

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
    lines.extend(usage_chart_lines(&stats.daily, cached.span));
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

/// Format a reset instant as a short "resets in …" string.
fn format_reset_in(dt: DateTime<Utc>) -> String {
    let secs = (dt - Utc::now()).num_seconds();
    if secs <= 0 {
        return "reset due".to_string();
    }
    let days = secs / 86_400;
    let hours = (secs % 86_400) / 3_600;
    let mins = (secs % 3_600) / 60;
    if days > 0 {
        format!("resets in {days}d {hours}h")
    } else if hours > 0 {
        format!("resets in {hours}h {mins}m")
    } else {
        format!("resets in {mins}m")
    }
}

/// Color a window by how much quota it has consumed.
fn quota_percent_color(used_percent: f64) -> Color {
    if used_percent >= 90.0 {
        Color::Red
    } else if used_percent >= 75.0 {
        Color::Yellow
    } else {
        Color::Green
    }
}

/// Width (in cells) of the quota usage bar.
const QUOTA_BAR_WIDTH: usize = 20;

/// A filled/empty block bar for a 0..=100 percentage.
fn quota_bar(used_percent: f64) -> String {
    let pct = used_percent.clamp(0.0, 100.0);
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let filled = ((pct / 100.0) * QUOTA_BAR_WIDTH as f64).round() as usize;
    let filled = filled.min(QUOTA_BAR_WIDTH);
    format!(
        "{}{}",
        "█".repeat(filled),
        "░".repeat(QUOTA_BAR_WIDTH - filled)
    )
}

/// One window rendered as `label  ▕████░░░░▏  47%   resets in …`.
fn quota_window_line(w: &QuotaWindow) -> Line<'static> {
    let color = quota_percent_color(w.used_percent);
    let mut spans = vec![
        Span::raw(format!("    {:<7} ", w.label)),
        Span::styled(
            format!("▕{}▏", quota_bar(w.used_percent)),
            Style::default().fg(color),
        ),
        Span::styled(
            format!(" {:>3.0}%", w.used_percent),
            Style::default().fg(color).add_modifier(Modifier::BOLD),
        ),
    ];
    if let Some(dt) = w.resets_at {
        spans.push(Span::styled(
            format!("   {}", format_reset_in(dt)),
            Style::default().fg(Color::DarkGray),
        ));
    }
    if let Some(model) = &w.scope {
        spans.push(Span::styled(
            format!("   · {model}"),
            Style::default().fg(Color::DarkGray),
        ));
    }
    Line::from(spans)
}

/// Lines describing a provider's quota: a bold name/plan header followed by one
/// bar line per window, or a single dim `unavailable` line.
fn subscription_entry_lines(entry: &QuotaEntry) -> Vec<Line<'static>> {
    let dim = Style::default().fg(Color::DarkGray);
    let name = account_display(entry.provider, entry.account.as_deref());
    let Some(quota) = &entry.quota else {
        let reason = entry.error.as_deref().unwrap_or("unavailable");
        return vec![Line::from(Span::styled(
            format!("  {name}   {reason}"),
            dim,
        ))];
    };
    let plan = quota
        .plan
        .as_ref()
        .map(|p| format!("  [{p}]"))
        .unwrap_or_default();
    let stale = entry
        .error
        .as_ref()
        .map(|e| format!("   · stale ({e})"))
        .unwrap_or_default();
    let mut lines = vec![Line::from(vec![
        Span::styled(
            format!("  {name}"),
            Style::default().add_modifier(Modifier::BOLD),
        ),
        Span::styled(plan, dim),
        Span::styled(stale, dim),
    ])];
    for w in &quota.windows {
        lines.push(quota_window_line(w));
    }
    lines
}

/// The subscription-quota block rendered at the top of the Usage tab.
fn subscription_lines(quotas: Option<&CachedQuotas>) -> Vec<Line<'static>> {
    let dim = Style::default().fg(Color::DarkGray);
    let mut lines = vec![Line::from(Span::styled(
        "Subscriptions",
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    ))];
    match quotas {
        None => lines.push(Line::from(Span::styled("  loading…", dim))),
        Some(cached) if cached.entries.is_empty() => lines.push(Line::from(Span::styled(
            "  No subscription providers logged in.",
            dim,
        ))),
        Some(cached) => {
            for entry in &cached.entries {
                lines.extend(subscription_entry_lines(entry));
            }
        }
    }
    lines.push(Line::from(""));
    lines
}

/// The banner/header block shown above the tables (title, scope, freshness).
fn usage_banner_lines(cached: &CachedUsageStats, span_label: &str) -> Vec<Line<'static>> {
    let dim = Style::default().fg(Color::DarkGray);
    vec![
        Line::from(Span::styled(
            "zdx usage stats (estimated)",
            Style::default().add_modifier(Modifier::BOLD),
        )),
        Line::from(vec![
            Span::styled(
                "Global across all ZDX threads under $ZDX_HOME/threads · span: ",
                dim,
            ),
            Span::styled(
                span_label.to_string(),
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(" (press t to change)", dim),
        ]),
        Line::from(Span::styled(
            "Estimated: old usage lacks per-request model/provider; includes saved \
             subagent/helper runs; image spend excluded; subscription providers shown as flat-rate.",
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

/// Width (in cells) of a labelled daily bar.
const DAILY_BAR_WIDTH: usize = 30;
/// At or below this many days in the window, render labelled bars; above it,
/// switch to a compact vertical bar chart so long windows stay readable.
const DAILY_BAR_MAX_DAYS: usize = 30;
/// Rows tall for the vertical daily bar chart (large windows).
const DAILY_CHART_HEIGHT: usize = 8;
/// Gutter width (chars) for the vertical chart's left value axis.
const DAILY_AXIS_GUTTER: usize = 6;

/// The daily-usage chart: one labelled token bar per day for a small window,
/// or a compact sparkline for a large one. Honors the active span (fixed
/// windows are zero-filled so gaps are visible; all-time uses observed days).
fn usage_chart_lines(daily: &[DailyUsage], span: UsageSpan) -> Vec<Line<'static>> {
    let dim = Style::default().fg(Color::DarkGray);
    let header = Line::from(Span::styled(
        format!("Daily tokens ({}):", span.label()),
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    ));
    let series = daily_series_for_span(daily, span);
    if series.is_empty() {
        return vec![
            header,
            Line::from(Span::styled("  no dated usage in this window", dim)),
        ];
    }
    let max = series.iter().map(|(_, t)| *t).max().unwrap_or(0).max(1);
    let mut lines = vec![header];
    if series.len() <= DAILY_BAR_MAX_DAYS {
        for (day, tokens) in &series {
            lines.push(daily_bar_line(*day, *tokens, max));
        }
    } else {
        lines.extend(daily_sparkline_lines(&series, max));
    }
    lines
}

/// The `(day, tokens)` series to plot for the active span. Fixed windows are
/// zero-filled across `since_day..=today` so missing days show as empty bars;
/// all-time plots only the days that have usage.
fn daily_series_for_span(daily: &[DailyUsage], span: UsageSpan) -> Vec<(i32, u64)> {
    match span.since_day() {
        Some(min) => {
            let today = usage_stats::today_utc();
            let map: std::collections::HashMap<i32, u64> =
                daily.iter().map(|d| (d.day, d.tokens)).collect();
            (min..=today)
                .map(|day| (day, map.get(&day).copied().unwrap_or(0)))
                .collect()
        }
        None => daily.iter().map(|d| (d.day, d.tokens)).collect(),
    }
}

/// A single labelled day bar: `MM-DD ▕████░░░░▏  1.2M`.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss
)]
fn daily_bar_line(day: i32, tokens: u64, max: u64) -> Line<'static> {
    let filled = (((tokens as f64) / (max as f64)) * DAILY_BAR_WIDTH as f64).round() as usize;
    let filled = filled.min(DAILY_BAR_WIDTH);
    let bar = format!(
        "{}{}",
        "█".repeat(filled),
        "░".repeat(DAILY_BAR_WIDTH - filled)
    );
    Line::from(vec![
        Span::styled(
            format!("  {} ", day_label(day)),
            Style::default().fg(Color::DarkGray),
        ),
        Span::styled(format!("▕{bar}▏"), Style::default().fg(Color::Cyan)),
        Span::styled(
            format!(" {:>8}", format_usage_tokens(tokens)),
            Style::default().fg(Color::White),
        ),
    ])
}

/// A compact multi-row vertical bar chart with a value axis (left) and date
/// ticks (below), used for windows too wide for one labelled bar per day.
fn daily_sparkline_lines(series: &[(i32, u64)], max: u64) -> Vec<Line<'static>> {
    let cyan = Style::default().fg(Color::Cyan);
    let dim = Style::default().fg(Color::DarkGray);
    let mid = DAILY_CHART_HEIGHT / 2;
    let mut lines: Vec<Line<'static>> = vertical_bar_rows(series, max, DAILY_CHART_HEIGHT)
        .into_iter()
        .enumerate()
        .map(|(i, row)| {
            // Value axis: max at top, half at the midline, 0 at the baseline.
            let label = if i == 0 {
                format_usage_tokens(max)
            } else if i == mid {
                format_usage_tokens(max / 2)
            } else if i == DAILY_CHART_HEIGHT - 1 {
                "0".to_string()
            } else {
                String::new()
            };
            Line::from(vec![
                Span::styled(format!("{label:>DAILY_AXIS_GUTTER$} │"), dim),
                Span::styled(row, cyan),
            ])
        })
        .collect();

    // Date axis: a baseline rule plus `MM-DD` ticks aligned under their columns.
    lines.push(Line::from(vec![
        Span::styled(format!("{:>DAILY_AXIS_GUTTER$} └", ""), dim),
        Span::styled(date_axis(series), dim),
    ]));

    let (peak_day, peak_tokens) = series
        .iter()
        .copied()
        .max_by_key(|(_, tokens)| *tokens)
        .unwrap_or((0, 0));
    lines.push(Line::from(Span::styled(
        format!(
            "{:>DAILY_AXIS_GUTTER$}   peak {} on {}",
            "",
            format_usage_tokens(peak_tokens),
            day_label(peak_day),
        ),
        dim,
    )));
    lines
}

/// Builds the date-axis row (length = `series.len()`): `MM-DD` labels placed at
/// evenly spaced columns, with the final label right-anchored to the last day
/// so the window's start and end dates are both readable.
fn date_axis(series: &[(i32, u64)]) -> String {
    let n = series.len();
    if n == 0 {
        return String::new();
    }
    let mut axis = vec![' '; n];
    let put = |axis: &mut Vec<char>, start: usize, label: &str| {
        for (k, ch) in label.chars().enumerate() {
            if let Some(slot) = axis.get_mut(start + k) {
                *slot = ch;
            }
        }
    };
    let last_label = day_label(series[n - 1].0);
    let len = last_label.chars().count();
    let final_start = n.saturating_sub(len);
    let step = (n / 6).max(2 * len + 2);
    for p in (0..n).step_by(step) {
        // Keep the first tick; drop any that would collide with the final one.
        if p == 0 || p + len < final_start {
            put(&mut axis, p, &day_label(series[p].0));
        }
    }
    put(&mut axis, final_start, &last_label);
    axis.into_iter().collect()
}

/// Renders `height` rows (top-to-bottom) of a vertical bar chart, one column per
/// value, using eighth-block glyphs so each column has sub-row resolution.
/// Nonzero days render at least one eighth; zero days show a baseline `·`.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss
)]
fn vertical_bar_rows(series: &[(i32, u64)], max: u64, height: usize) -> Vec<String> {
    const EIGHTHS: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
    let total = (height * 8) as f64;
    let mut rows = vec![String::with_capacity(series.len()); height];
    for (_, tokens) in series {
        let eighths = if *tokens == 0 {
            0
        } else {
            (((*tokens as f64) / (max as f64)) * total).round().max(1.0) as usize
        };
        for r in 0..height {
            // `r == 0` is the bottom row; fill from the bottom up.
            let cell = eighths.saturating_sub(r * 8).min(8);
            let ch = if cell == 0 {
                if r == 0 && *tokens == 0 { '·' } else { ' ' }
            } else {
                EIGHTHS[cell - 1]
            };
            rows[height - 1 - r].push(ch);
        }
    }
    rows
}

/// Formats a UTC day number (days since epoch) as `MM-DD`.
fn day_label(day: i32) -> String {
    DateTime::from_timestamp(i64::from(day) * 86_400, 0)
        .map_or_else(|| day.to_string(), |dt| dt.format("%m-%d").to_string())
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
                Style::default().bg(SELECTED_BG)
            } else {
                Style::default()
            };
            ListItem::new(line).style(style)
        })
        .collect();
    let list = List::new(items).block(Block::default().borders(Borders::ALL).title("Automations"));
    f.render_widget(list, area);
}

/// Filter-and-pick popup shared by the Logs target filter and the Threads
/// project filter.
fn render_picker(f: &mut Frame, picker: &TargetPickerState, area: Rect, noun: &str) {
    let popup = centered_rect(60, 60, area);
    f.render_widget(Clear, popup);

    let title = format!(
        " pick {noun} · {} match · Enter apply · Esc cancel ",
        picker.matches.len(),
    );
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan))
        .title(title);
    let inner = block.inner(popup);
    f.render_widget(block, popup);

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Min(0)])
        .split(inner);

    let filter_line = Line::from(vec![
        Span::styled("filter: ", Style::default().fg(Color::DarkGray)),
        Span::styled(
            if picker.filter.is_empty() {
                "(type to filter)".to_string()
            } else {
                picker.filter.clone()
            },
            Style::default().fg(Color::Yellow),
        ),
    ]);
    f.render_widget(Paragraph::new(filter_line), rows[0]);

    let visible = rows[1].height as usize;
    let offset = picker.selected.saturating_sub(visible.saturating_sub(1));
    let end = (offset + visible).min(picker.matches.len());

    let items: Vec<ListItem> = picker.matches[offset..end]
        .iter()
        .enumerate()
        .map(|(i, &item_index)| {
            let global = offset + i;
            let (target, count) = &picker.items[item_index];
            let row = Line::from(vec![
                Span::styled(target.clone(), Style::default().fg(Color::Cyan)),
                Span::styled(format!("  ({count})"), Style::default().fg(Color::DarkGray)),
            ]);
            let item = ListItem::new(row);
            if global == picker.selected {
                item.style(Style::default().bg(SELECTED_BG))
            } else {
                item
            }
        })
        .collect();
    f.render_widget(List::new(items), rows[1]);
}

/// Full-frame detail view for one background process: marker metadata, the
/// full command, and the stdout/stderr log tails. Lines are pre-wrapped at
/// build time, so scrolling is a plain row-window here.
fn render_background_detail(f: &mut Frame, state: &BackgroundDetailState, area: Rect) {
    f.render_widget(Clear, area);
    let visible_rows = (area.height.saturating_sub(2) as usize).max(1);
    let offset = state.offset(visible_rows);
    let end = (offset + visible_rows).min(state.lines.len());
    let items: Vec<ListItem> = state.lines[offset..end]
        .iter()
        .map(|line| ListItem::new(line.clone()))
        .collect();

    let position = if state.lines.len() > visible_rows {
        format!(" [{}/{}]", offset + 1, state.lines.len())
    } else {
        String::new()
    };
    let follow = if state.follow { " · following" } else { "" };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Green))
        .title(format!(" Background · {} ", state.title))
        .title_bottom(format!(
            " j/k scroll · gg top · G follow · y cmd · Y text · Esc close{position}{follow} "
        ));
    f.render_widget(List::new(items).block(block), area);
}

/// Build a centered Rect using `percent_x` × `percent_y` of `area`.
pub(crate) fn centered_rect(percent_x: u16, percent_y: u16, area: Rect) -> Rect {
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

#[cfg(test)]
mod tests {
    use ratatui::backend::TestBackend;
    use zdx_engine::providers::subscription_quota::SubscriptionQuota;

    use super::*;
    use crate::app::QuotaEntry;
    use crate::tabs::threads::TimingOverlayState;

    #[test]
    fn percent_color_thresholds() {
        assert_eq!(quota_percent_color(0.0), Color::Green);
        assert_eq!(quota_percent_color(74.9), Color::Green);
        assert_eq!(quota_percent_color(75.0), Color::Yellow);
        assert_eq!(quota_percent_color(89.9), Color::Yellow);
        assert_eq!(quota_percent_color(90.0), Color::Red);
        assert_eq!(quota_percent_color(100.0), Color::Red);
    }

    #[test]
    fn near_limit_window_renders_red_span() {
        let entry = QuotaEntry {
            provider: "claude-cli",
            account: None,
            quota: Some(SubscriptionQuota {
                plan: None,
                windows: vec![
                    QuotaWindow {
                        label: "5h".to_string(),
                        used_percent: 12.0,
                        resets_at: None,
                        scope: None,
                    },
                    QuotaWindow {
                        label: "weekly".to_string(),
                        used_percent: 95.0,
                        resets_at: None,
                        scope: None,
                    },
                ],
            }),
            error: None,
        };
        let lines = subscription_entry_lines(&entry);
        let spans: Vec<_> = lines.iter().flat_map(|l| l.spans.iter()).collect();
        let green = spans
            .iter()
            .any(|s| s.content.contains("12%") && s.style.fg == Some(Color::Green));
        let red = spans
            .iter()
            .any(|s| s.content.contains("95%") && s.style.fg == Some(Color::Red));
        // Each window renders a filled/empty bar of fixed width.
        let has_bar = spans.iter().any(|s| s.content.contains('█'));
        assert!(green, "low window percent should be green");
        assert!(red, "near-limit window percent should be red");
        assert!(has_bar, "windows should render a bar");
    }

    #[test]
    fn unavailable_entry_renders_dim_reason() {
        let entry = QuotaEntry {
            provider: "openai-codex",
            account: None,
            quota: None,
            error: Some("rate limited".to_string()),
        };
        let lines = subscription_entry_lines(&entry);
        assert_eq!(lines.len(), 1);
        assert!(
            lines[0]
                .spans
                .iter()
                .any(|s| s.content.contains("Codex") && s.content.contains("rate limited"))
        );
    }

    #[test]
    fn quota_bar_fills_proportionally() {
        assert_eq!(quota_bar(0.0).chars().filter(|c| *c == '█').count(), 0);
        assert_eq!(
            quota_bar(100.0).chars().filter(|c| *c == '█').count(),
            QUOTA_BAR_WIDTH
        );
        assert_eq!(
            quota_bar(50.0).chars().filter(|c| *c == '█').count(),
            QUOTA_BAR_WIDTH / 2
        );
        // Out-of-range values are clamped, never panic or overflow the bar.
        assert_eq!(
            quota_bar(150.0).chars().filter(|c| *c == '█').count(),
            QUOTA_BAR_WIDTH
        );
    }

    #[test]
    fn timing_overlay_renders_title_and_unavailable_state() {
        let backend = TestBackend::new(80, 12);
        let mut terminal = Terminal::new(backend).unwrap();
        let state = TimingOverlayState {
            title: "thread-1 · Demo".to_string(),
            lines: vec![
                "Turn 1".to_string(),
                "  Tool work (sum, not wall time): unavailable (0/1 measured)".to_string(),
            ],
            scroll: 0,
        };
        terminal
            .draw(|frame| render_timing_overlay(frame, &state, frame.area()))
            .unwrap();

        let buffer = terminal.backend().buffer();
        let mut text = String::new();
        for y in 0..buffer.area.height {
            for x in 0..buffer.area.width {
                text.push_str(buffer.cell((x, y)).unwrap().symbol());
            }
            text.push('\n');
        }
        assert!(text.contains("Timings · thread-1 · Demo"));
        assert!(text.contains("unavailable (0/1 measured)"));
    }
}
