use std::sync::mpsc;
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use crossterm::event::KeyCode;
use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, Paragraph};
use zdx_engine::core::usage_stats::{self, DailyUsage, UsageRow, UsageStats, UsageTotals};
use zdx_engine::providers::subscription_quota::{
    self, QuotaError, QuotaWindow, SubscriptionQuota, account_display,
};

use crate::app::{MonitorApp, Section};
use crate::ui::truncate_chars;

/// Result payload from a background quota fetch: one entry per provider account.
pub(crate) type QuotaFetchResult = Vec<(
    &'static str,
    Option<String>,
    std::result::Result<SubscriptionQuota, QuotaError>,
)>;

/// A cached snapshot of the usage aggregation plus when it was computed.
pub struct CachedUsageStats {
    pub stats: UsageStats,
    pub computed_at: Instant,
    /// Span the snapshot was aggregated for. The daily chart is rendered from
    /// this (not the live selection) so a span change in flight never shows a
    /// narrower series zero-filled into a wider window.
    pub span: UsageSpan,
}

/// Cached per-provider subscription quota snapshot plus when it was fetched.
pub struct CachedQuotas {
    pub entries: Vec<QuotaEntry>,
    pub computed_at: Instant,
}

/// One provider's latest quota state. `quota` holds the last good value;
/// when `error` is set alongside a `quota`, the value is stale.
pub struct QuotaEntry {
    pub provider: &'static str,
    pub account: Option<String>,
    pub quota: Option<SubscriptionQuota>,
    pub error: Option<String>,
}

/// Time window applied to the Usage tab's aggregation. Cycled with `t`.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum UsageSpan {
    All,
    Last7d,
    Last30d,
    Last90d,
}

impl UsageSpan {
    /// Next window in the toggle cycle.
    fn next(self) -> Self {
        match self {
            UsageSpan::All => UsageSpan::Last7d,
            UsageSpan::Last7d => UsageSpan::Last30d,
            UsageSpan::Last30d => UsageSpan::Last90d,
            UsageSpan::Last90d => UsageSpan::All,
        }
    }

    /// Human-readable label for the banner/footer.
    pub fn label(self) -> &'static str {
        match self {
            UsageSpan::All => "all time",
            UsageSpan::Last7d => "last 7 days",
            UsageSpan::Last30d => "last 30 days",
            UsageSpan::Last90d => "last 90 days",
        }
    }

    /// Inclusive earliest UTC day number for this window (`None` = all time).
    pub(crate) fn since_day(self) -> Option<i32> {
        let days = match self {
            UsageSpan::All => return None,
            UsageSpan::Last7d => 6,
            UsageSpan::Last30d => 29,
            UsageSpan::Last90d => 89,
        };
        Some(usage_stats::today_utc() - days)
    }
}

/// Visible content rows in the Usage panel (same chrome as Config).
fn usage_page_size(app: &MonitorApp) -> usize {
    (app.terminal_height.saturating_sub(8) as usize).max(1)
}

/// Maximum valid scroll offset for the Usage panel.
pub(crate) fn usage_max_scroll(app: &MonitorApp) -> usize {
    app.usage_line_count.saturating_sub(usage_page_size(app))
}

/// How long a cached usage snapshot stays fresh before an on-tick refresh.
const USAGE_STALE_AFTER: Duration = Duration::from_secs(30);

/// Spawn a background usage scan unless one is already in flight. The scan
/// runs off the UI thread; its result is collected by `poll_usage_result`.
fn start_usage_scan(app: &mut MonitorApp) {
    if app.usage_rx.is_some() {
        return;
    }
    let model = app.default_model.clone();
    let span = app.usage_span;
    let since_day = span.since_day();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(usage_stats::aggregate_usage(&model, since_day));
    });
    app.usage_rx = Some(rx);
    app.usage_scan_span = Some(span);
}

/// Collect a finished background usage scan, if any, into the cache. Non-
/// blocking: returns immediately when the scan is still running. Tags the
/// snapshot with the span it was computed for and, if the selection changed
/// while the scan ran, kicks off a fresh scan so the view self-heals.
pub(crate) fn poll_usage_result(app: &mut MonitorApp) {
    let Some(rx) = &app.usage_rx else {
        return;
    };
    match rx.try_recv() {
        Ok(result) => {
            app.usage_rx = None;
            let scan_span = app.usage_scan_span.take().unwrap_or(app.usage_span);
            match result {
                Ok(stats) => {
                    let cached = CachedUsageStats {
                        stats,
                        computed_at: Instant::now(),
                        span: scan_span,
                    };
                    app.usage_line_count =
                        usage_line_count(&cached, app.quotas.as_ref(), app.usage_span);
                    app.usage_stats = Some(cached);
                    app.usage_scroll = app.usage_scroll.min(usage_max_scroll(app));
                    // Selection moved on while this scan ran — re-scan now
                    // rather than waiting for the staleness tick.
                    if scan_span != app.usage_span {
                        start_usage_scan(app);
                    }
                }
                Err(err) => app.set_status(format!("Usage stats failed: {err}")),
            }
        }
        Err(mpsc::TryRecvError::Empty) => {}
        Err(mpsc::TryRecvError::Disconnected) => app.usage_rx = None,
    }
}

/// Starts a background usage refresh when the Usage tab is active and the
/// cache is missing or stale. Never runs on every tick, never blocks the UI.
/// The refresh key (`R`) calls `start_usage_scan` directly to force a scan.
pub(crate) fn refresh_usage(app: &mut MonitorApp) {
    if app.active_section != Section::Usage {
        return;
    }
    let stale = app
        .usage_stats
        .as_ref()
        .is_none_or(|c| c.computed_at.elapsed() >= USAGE_STALE_AFTER);
    if stale {
        start_usage_scan(app);
    }
}

/// How long a cached quota snapshot stays fresh. Deliberately slow — these are
/// undocumented network endpoints, not cheap local scans.
#[allow(clippy::duration_suboptimal_units)]
const QUOTA_STALE_AFTER: Duration = Duration::from_secs(5 * 60);

/// Spawn a background subscription-quota fetch unless one is in flight. Runs on
/// its own thread with a current-thread Tokio runtime (the monitor has no
/// ambient runtime); read-only — never refreshes or writes OAuth tokens.
fn start_quota_fetch(app: &mut MonitorApp) {
    if app.quota_rx.is_some() {
        return;
    }
    // Skip providers still inside a rate-limit cooldown so `R` and the on-tick
    // refresh cannot hammer a 429'd endpoint.
    let now = Instant::now();
    let ready: Vec<(
        &'static str,
        Option<String>,
        subscription_quota::QuotaFetcher,
    )> = subscription_quota::stored_accounts()
        .unwrap_or_default()
        .into_iter()
        .filter(|(provider, account, _)| {
            let key = subscription_quota::account_cache_key(provider, account.as_deref());
            app.quota_backoff
                .get(&key)
                .is_none_or(|until| *until <= now)
        })
        .collect();
    if ready.is_empty() {
        return;
    }
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let results = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map(|rt| {
                rt.block_on(async {
                    let mut out: QuotaFetchResult = Vec::with_capacity(ready.len());
                    for (provider, account, fetch) in ready {
                        let quota = fetch(account.clone()).await;
                        out.push((provider, account, quota));
                    }
                    out
                })
            })
            .unwrap_or_default();
        let _ = tx.send(results);
    });
    app.quota_rx = Some(rx);
}

/// Recompute the cached Usage view's line count (the subscription block affects
/// it). No-op when the usage cache is absent.
fn recompute_usage_line_count(app: &mut MonitorApp) {
    let count = match &app.usage_stats {
        Some(cached) => usage_line_count(cached, app.quotas.as_ref(), app.usage_span),
        None => return,
    };
    app.usage_line_count = count;
}

/// Default rate-limit cooldown when a 429 carried no `Retry-After`.
#[allow(clippy::duration_suboptimal_units)]
const QUOTA_BACKOFF_DEFAULT: Duration = Duration::from_secs(60);

/// Collect a finished background quota fetch, merging results while preserving
/// the last good value for a provider whose refresh failed. Providers absent
/// from `results` (e.g. skipped for cooldown) keep their existing entry.
pub(crate) fn poll_quota_result(app: &mut MonitorApp) {
    let Some(rx) = &app.quota_rx else {
        return;
    };
    let results = match rx.try_recv() {
        Ok(results) => results,
        Err(mpsc::TryRecvError::Empty) => return,
        Err(mpsc::TryRecvError::Disconnected) => {
            app.quota_rx = None;
            return;
        }
    };
    app.quota_rx = None;

    let mut entries: Vec<QuotaEntry> = app.quotas.take().map(|c| c.entries).unwrap_or_default();
    for (provider, account, res) in results {
        let key = subscription_quota::account_cache_key(provider, account.as_deref());
        let idx = entries
            .iter()
            .position(|e| e.provider == provider && e.account == account);
        let prev_quota = idx.and_then(|i| entries[i].quota.clone());
        let new_entry = match res {
            Ok(quota) => {
                app.quota_backoff.remove(&key);
                Some(QuotaEntry {
                    provider,
                    account: account.clone(),
                    quota: Some(quota),
                    error: None,
                })
            }
            // Not logged in: drop the row unless we already had a value.
            Err(QuotaError::NotAuthenticated) => prev_quota.map(|quota| QuotaEntry {
                provider,
                account: account.clone(),
                quota: Some(quota),
                error: Some(QuotaError::NotAuthenticated.reason()),
            }),
            Err(err) => {
                if let QuotaError::RateLimited { retry_after_secs } = err {
                    let cooldown =
                        retry_after_secs.map_or(QUOTA_BACKOFF_DEFAULT, Duration::from_secs);
                    app.quota_backoff.insert(key, Instant::now() + cooldown);
                }
                Some(QuotaEntry {
                    provider,
                    account: account.clone(),
                    quota: prev_quota,
                    error: Some(err.reason()),
                })
            }
        };
        match (idx, new_entry) {
            (Some(i), Some(entry)) => entries[i] = entry,
            (Some(i), None) => {
                entries.remove(i);
            }
            (None, Some(entry)) => entries.push(entry),
            (None, None) => {}
        }
    }
    app.quotas = Some(CachedQuotas {
        entries,
        computed_at: Instant::now(),
    });
    recompute_usage_line_count(app);
}

/// Starts a background quota refresh when the Usage tab is active and the cache
/// is missing or stale. Independent of the usage-aggregation scan.
pub(crate) fn refresh_quota(app: &mut MonitorApp) {
    if app.active_section != Section::Usage {
        return;
    }
    let stale = app
        .quotas
        .as_ref()
        .is_none_or(|c| c.computed_at.elapsed() >= QUOTA_STALE_AFTER);
    if stale {
        start_quota_fetch(app);
    }
}

/// Handle a key while the Usage section is active. Returns `true` if the key
/// was consumed (so the generic dispatcher should not also act on it).
pub(crate) fn handle_usage_key(app: &mut MonitorApp, key: KeyCode) -> bool {
    match key {
        KeyCode::PageDown => {
            let page = usage_page_size(app);
            let max = usage_max_scroll(app);
            app.usage_scroll = app.usage_scroll.saturating_add(page).min(max);
            true
        }
        KeyCode::PageUp => {
            let page = usage_page_size(app);
            app.usage_scroll = app.usage_scroll.saturating_sub(page);
            true
        }
        KeyCode::Char('R') => {
            start_usage_scan(app);
            start_quota_fetch(app);
            app.set_status("Refreshing usage stats…");
            true
        }
        KeyCode::Char('t') => {
            app.usage_span = app.usage_span.next();
            app.usage_scroll = 0;
            start_usage_scan(app);
            app.set_status(format!("Usage span: {}", app.usage_span.label()));
            true
        }
        _ => false,
    }
}

pub(crate) fn render_usage(f: &mut Frame, app: &MonitorApp, area: Rect) {
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

#[cfg(test)]
mod tests {
    use super::*;

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
}
