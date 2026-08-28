use std::collections::{BTreeMap, HashMap};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEvent, KeyEventKind,
    MouseButton, MouseEvent, MouseEventKind,
};
use crossterm::execute;
use crossterm::terminal::{self, EnterAlternateScreen, LeaveAlternateScreen};
use ratatui::prelude::*;
use zdx_engine::core::thread_index::{ThreadBrowseOptions, ThreadKindFilter};
use zdx_engine::core::usage_stats::{self, UsageStats};
use zdx_engine::providers::subscription_quota::{self, QuotaError, SubscriptionQuota};
use zdx_engine::service::{self, Service};
use zdx_engine::{automations, config};

use crate::log_line::{LevelFilter, line_target};
use crate::tabs::agents::{
    ActiveAgentInfo, AgentOverlayState, agent_overlay_page_size, handle_agent_overlay_key,
    handle_overlay_click, load_active_agents, open_agent_overlay, refresh_agent_overlay,
};
use crate::tabs::config::{
    ConfigLine, ModelPickerState, build_config_lines, config_max_scroll, config_page_size,
    delete_or_reset_selected, handle_model_picker_key, move_config_selection, open_model_picker,
    rendered_line_count,
};
use crate::tabs::logs::{
    LOG_TAIL_LINES, LoadedLogFile, ensure_log_selected_visible, handle_log_overlay_key,
    handle_log_query_key, handle_log_target_picker_key, handle_logs_key, load_active_log,
    recompute_log_visible,
};
use crate::tabs::threads::{
    ThreadInfo, ThreadsSnapshot, TimingOverlayState, copy_selected_thread_id,
    handle_thread_project_picker_key, handle_thread_query_key, handle_threads_key,
    handle_timing_overlay_key, poll_threads_result, refresh_threads_if_stale,
};
use crate::ui;

#[allow(clippy::struct_excessive_bools)]
pub struct MonitorApp {
    pub config_lines: Vec<ConfigLine>,
    pub config_line_count: usize,
    pub config_scroll: usize,
    /// Index into the editable model rows of the Config tab (see
    /// `editable_config_rows`). Selects which model field `Enter` edits.
    pub config_selected: usize,
    /// Open model-picker overlay for editing a Config model field, if any.
    pub model_picker: Option<ModelPickerState>,
    pub terminal_height: u16,
    pub terminal_width: u16,
    pub root: PathBuf,
    pub threads: Vec<ThreadInfo>,
    /// Run-kind filter for the Threads tab, cycled with `t`.
    pub thread_kind_filter: ThreadKindFilter,
    /// Active project filter (an exact thread `root_path`), chosen with `p`.
    pub thread_project_filter: Option<String>,
    /// Open project-picker overlay for the Threads tab, if any.
    pub thread_project_picker: Option<TargetPickerState>,
    /// Distinct project roots with thread counts, refreshed with every Threads
    /// query so `p` opens the picker without touching the index.
    pub thread_projects: Vec<(String, usize)>,
    /// Full-text query over titles and user/assistant text, edited with `/`.
    pub thread_query: String,
    /// Whether keystrokes are currently being captured into `thread_query`.
    pub thread_query_editing: bool,
    /// When the Threads result set was last loaded (drives the slow refresh).
    pub threads_loaded_at: Option<Instant>,
    /// Receiver for an in-flight background Threads query, if any. The query
    /// syncs `threads.sqlite`, which stats every thread file, so it never runs
    /// on the render thread.
    pub threads_rx: Option<mpsc::Receiver<ThreadsSnapshot>>,
    /// Filters the in-flight query (`threads_rx`) was started for, so a filter
    /// change while it runs supersedes it with a fresh query.
    pub threads_scan_options: Option<ThreadBrowseOptions>,
    /// Raw thread JSONL queued to open after the current key event.
    pub(crate) pending_open_path: Option<PathBuf>,
    pub automations: Vec<AutomationInfo>,
    pub services: Vec<ServiceInfo>,
    pub active_agents: Vec<ActiveAgentInfo>,
    /// Running background processes (from `background_activity`), for the
    /// Background tab. Sorted so same-thread processes are adjacent.
    pub background: Vec<BackgroundInfo>,
    /// Open transcript overlay for a selected active agent, if any.
    pub agent_overlay: Option<AgentOverlayState>,
    /// Open timing overlay for a selected saved thread, if any.
    pub timing_overlay: Option<TimingOverlayState>,
    /// Open process detail overlay on the Background tab, if any.
    pub background_detail: Option<BackgroundDetailState>,
    /// A `g` was pressed and the next key completes (or aborts) the sequence.
    pub g_prefix: zdx_transcript::keys::GPrefix,
    /// Mouse capture is active (default). `M` releases it so the terminal's
    /// native text selection/copy works, and restores it on the next press.
    pub mouse_captured: bool,
    /// List scrolling is swallowed until this instant (armed on overlay close).
    pub scroll_guard_until: Option<Instant>,
    pub log_file_name: Option<String>,
    /// Log files in `~/.zdx/logs` matching `zdx.log*`, newest first.
    pub log_files: Vec<PathBuf>,
    /// Index into `log_files` of the file being viewed (`[` older, `]` newer).
    /// Index 0 is the live file and is the only one re-tailed on refresh.
    pub log_file_index: usize,
    /// Identity of the currently loaded file, so an unchanged file is not
    /// re-read on every tick/keypress.
    pub log_loaded: Option<LoadedLogFile>,
    /// How many trailing lines to keep from the active file (cycled with `L`).
    pub log_tail_lines: usize,
    /// All lines tailed from the active log file, unfiltered. The source of
    /// truth: the detail overlay and `y` copy always read from here.
    pub log_lines: Vec<String>,
    /// Indices into `log_lines` that pass the active filters, in file order.
    /// `log_selected` / `log_offset` are positions in *this* list.
    pub log_visible: Vec<usize>,
    /// Minimum-severity filter, cycled with `l`.
    pub log_level_filter: LevelFilter,
    /// Case-insensitive substring filter, edited with `/`.
    pub log_query: String,
    /// Whether keystrokes are currently being captured into `log_query`.
    pub log_query_editing: bool,
    /// Active target prefix filter (e.g. `zdx_engine`), chosen with `f`.
    pub log_target_filter: Option<String>,
    /// Open target-picker overlay, if any.
    pub log_target_picker: Option<TargetPickerState>,
    pub log_selected: usize,
    pub log_offset: usize,
    pub log_follow: bool,
    pub log_overlay_open: bool,
    pub active_section: Section,
    pub selected_index: usize,
    pub status_section: Section,
    pub status_message: String,
    /// When the current status was set. Statuses are transient so the footer
    /// returns to the section's keybinding hints instead of hiding them
    /// forever after one action.
    pub status_set_at: Option<Instant>,
    pub should_quit: bool,
    /// Cached usage/cost aggregation for the Usage tab (computed lazily).
    pub usage_stats: Option<CachedUsageStats>,
    /// Vertical scroll offset for the Usage tab.
    pub usage_scroll: usize,
    /// Active time window for the Usage tab (toggled with `t`).
    pub usage_span: UsageSpan,
    /// Rendered line count of the cached usage view (for scroll clamping).
    pub usage_line_count: usize,
    /// Default model used to attribute legacy usage (mirrors `config.model`).
    pub default_model: String,
    /// Receiver for an in-flight background usage scan, if any. The scan runs
    /// off the UI thread so the dashboard never freezes during aggregation.
    pub usage_rx: Option<mpsc::Receiver<Result<UsageStats>>>,
    /// Span the in-flight usage scan (`usage_rx`) was started for, so its
    /// result can be tagged and a superseding span change can re-scan.
    pub usage_scan_span: Option<UsageSpan>,
    /// Cached subscription-quota snapshot per provider account (read-only OAuth).
    pub quotas: Option<CachedQuotas>,
    /// Receiver for an in-flight background quota fetch, if any.
    pub quota_rx: Option<mpsc::Receiver<QuotaFetchResult>>,
    /// Per-account rate-limit cooldown keyed by `provider`/`provider@account`:
    /// don't refetch before this instant.
    pub quota_backoff: HashMap<String, Instant>,
}

/// Result payload from a background quota fetch: one entry per provider account.
type QuotaFetchResult = Vec<(
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

pub struct AutomationInfo {
    pub name: String,
    pub schedule: Option<String>,
}

#[derive(Clone)]
pub struct ServiceInfo {
    pub service: Service,
    pub name: String,
    pub status: String,
    pub details: String,
}

/// One running background process shown in the Background tab.
pub struct BackgroundInfo {
    pub bg_id: String,
    pub pid: u32,
    /// Full originating thread id (`None` for no-thread runs); used to group.
    pub thread_id: Option<String>,
    pub command: String,
    pub uptime: String,
}

/// State for the Background tab's process detail overlay (drill-in on
/// `Enter`). Keyed by `bg_id` so the on-tick refresh keeps re-reading the
/// marker + log tails while the process runs.
pub struct BackgroundDetailState {
    pub bg_id: String,
    /// Header label, e.g. `bg-abc123 · pid 4567`.
    pub title: String,
    /// Rendered body, pre-wrapped to the terminal width at build time so
    /// scroll offsets count real rows.
    pub lines: Vec<Line<'static>>,
    /// Top-row offset; ignored while `follow` is set.
    pub scroll: usize,
    /// Pin the view to the newest output (like the Logs tab's `G` follow).
    pub follow: bool,
}

impl BackgroundDetailState {
    /// Handles a key while the overlay is open. Returns `true` when the
    /// overlay should close.
    fn handle_key(&mut self, key: KeyCode, page: usize) -> bool {
        let max = self.max_scroll(page);
        match key {
            KeyCode::Esc | KeyCode::Char('q') => return true,
            KeyCode::Down | KeyCode::Char('j') => self.scroll_to(self.offset(page) + 1, max),
            KeyCode::Up | KeyCode::Char('k') => {
                self.scroll_to(self.offset(page).saturating_sub(1), max);
            }
            KeyCode::PageDown => self.scroll_to(self.offset(page) + page, max),
            KeyCode::PageUp => self.scroll_to(self.offset(page).saturating_sub(page), max),
            KeyCode::Home => self.scroll_to(0, max),
            KeyCode::Char('G') | KeyCode::End => self.follow = true,
            _ => {}
        }
        false
    }

    fn scroll_to(&mut self, target: usize, max: usize) {
        self.follow = target >= max;
        self.scroll = target.min(max);
    }

    /// Current top row, resolving follow mode against the rendered height.
    pub fn offset(&self, page: usize) -> usize {
        let max = self.max_scroll(page);
        if self.follow {
            max
        } else {
            self.scroll.min(max)
        }
    }

    fn max_scroll(&self, page: usize) -> usize {
        self.lines.len().saturating_sub(page.max(1))
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Section {
    Services,
    ActiveAgents,
    Background,
    Config,
    Threads,
    Usage,
    Automations,
    Logs,
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

impl Section {
    pub const ALL: [Section; 8] = [
        Section::Services,
        Section::ActiveAgents,
        Section::Background,
        Section::Config,
        Section::Threads,
        Section::Usage,
        Section::Automations,
        Section::Logs,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Section::Services => "Services",
            Section::ActiveAgents => "Active Agents",
            Section::Background => "Background",
            Section::Config => "Config",
            Section::Threads => "Threads",
            Section::Usage => "Usage",
            Section::Automations => "Automations",
            Section::Logs => "Logs",
        }
    }

    fn next(self) -> Self {
        match self {
            Section::Services => Section::ActiveAgents,
            Section::ActiveAgents => Section::Background,
            Section::Background => Section::Config,
            Section::Config => Section::Threads,
            Section::Threads => Section::Usage,
            Section::Usage => Section::Automations,
            Section::Automations => Section::Logs,
            Section::Logs => Section::Services,
        }
    }

    fn prev(self) -> Self {
        match self {
            Section::Services => Section::Logs,
            Section::ActiveAgents => Section::Services,
            Section::Background => Section::ActiveAgents,
            Section::Config => Section::Background,
            Section::Threads => Section::Config,
            Section::Usage => Section::Threads,
            Section::Automations => Section::Usage,
            Section::Logs => Section::Automations,
        }
    }
}

impl MonitorApp {
    fn item_count(&self) -> usize {
        match self.active_section {
            Section::Services => self.services.len(),
            Section::Config | Section::Logs | Section::Usage => 0,
            Section::ActiveAgents => self.active_agents.len(),
            Section::Background => self.background.len(),
            Section::Threads => self.threads.len(),
            Section::Automations => self.automations.len(),
        }
    }

    pub(crate) fn clamp_selection(&mut self) {
        let count = self.item_count();
        if count == 0 {
            self.selected_index = 0;
        } else {
            self.selected_index = self.selected_index.min(count - 1);
        }
    }

    pub(crate) fn set_status(&mut self, message: impl Into<String>) {
        self.status_section = self.active_section;
        self.status_message = message.into();
        self.status_set_at = Some(Instant::now());
    }

    /// Raw (unfiltered, untruncated) line under the Logs selection.
    pub fn selected_log_line(&self) -> Option<&String> {
        let raw_index = *self.log_visible.get(self.log_selected)?;
        self.log_lines.get(raw_index)
    }
}

/// Visible content rows in the Usage panel (same chrome as Config).
fn usage_page_size(app: &MonitorApp) -> usize {
    (app.terminal_height.saturating_sub(8) as usize).max(1)
}

/// Maximum valid scroll offset for the Usage panel.
fn usage_max_scroll(app: &MonitorApp) -> usize {
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
fn poll_usage_result(app: &mut MonitorApp) {
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
                        ui::usage_line_count(&cached, app.quotas.as_ref(), app.usage_span);
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
fn refresh_usage(app: &mut MonitorApp) {
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
        Some(cached) => ui::usage_line_count(cached, app.quotas.as_ref(), app.usage_span),
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
fn poll_quota_result(app: &mut MonitorApp) {
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
fn refresh_quota(app: &mut MonitorApp) {
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

/// Default number of log lines tailed from the active log file.
/// Target-picker overlay for the Logs tab (`f`). Mirrors the model picker:
/// all items, a typed filter, and derived match indices.
pub struct TargetPickerState {
    pub filter: String,
    /// Distinct targets in the loaded lines with their line counts, most
    /// frequent first.
    pub items: Vec<(String, usize)>,
    /// Indices into `items` matching `filter`.
    pub matches: Vec<usize>,
    /// Index into `matches` of the highlighted row.
    pub selected: usize,
}

impl TargetPickerState {
    pub(crate) fn new(lines: &[String]) -> Self {
        let mut counts: BTreeMap<String, usize> = BTreeMap::new();
        for line in lines {
            let target = line_target(line);
            if !target.is_empty() {
                *counts.entry(target.to_string()).or_default() += 1;
            }
        }
        let mut items: Vec<(String, usize)> = counts.into_iter().collect();
        items.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        let mut state = Self {
            filter: String::new(),
            items,
            matches: Vec::new(),
            selected: 0,
        };
        state.recompute();
        state
    }

    /// Picker over pre-counted items (e.g. project roots from the thread index),
    /// keeping the caller's ordering.
    pub(crate) fn from_items(items: Vec<(String, usize)>) -> Self {
        let mut state = Self {
            filter: String::new(),
            items,
            matches: Vec::new(),
            selected: 0,
        };
        state.recompute();
        state
    }

    pub(crate) fn recompute(&mut self) {
        let filter = self.filter.to_lowercase();
        self.matches = self
            .items
            .iter()
            .enumerate()
            .filter(|(_, (target, _))| filter.is_empty() || target.to_lowercase().contains(&filter))
            .map(|(i, _)| i)
            .collect();
        self.selected = self.selected.min(self.matches.len().saturating_sub(1));
    }

    pub(crate) fn selected_target(&self) -> Option<&str> {
        let index = *self.matches.get(self.selected)?;
        self.items.get(index).map(|(target, _)| target.as_str())
    }
}

/// Switch the active tab and reset per-section scroll/selection state.
fn switch_section(app: &mut MonitorApp, section: Section) {
    app.active_section = section;
    app.selected_index = 0;
    app.config_scroll = 0;
    app.usage_scroll = 0;
    if app.active_section == Section::Logs {
        app.log_follow = true;
        recompute_log_visible(app);
    } else {
        app.log_query_editing = false;
    }
    if app.active_section == Section::Threads {
        refresh_threads_if_stale(app);
    } else {
        app.thread_query_editing = false;
        app.thread_project_picker = None;
    }
}

fn build_app(root: &Path) -> Result<MonitorApp> {
    let root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    let config = config::Config::load().context("load config")?;
    let default_model = config.model.clone();
    let config_lines = build_config_lines(&config, &root);
    let config_line_count = rendered_line_count(&config_lines);
    let services = load_services();

    let mut app = MonitorApp {
        config_lines,
        config_line_count,
        config_scroll: 0,
        config_selected: 0,
        model_picker: None,
        terminal_height: 24,
        terminal_width: 80,
        root: root.clone(),
        threads: Vec::new(),
        thread_kind_filter: ThreadKindFilter::All,
        thread_project_filter: None,
        thread_project_picker: None,
        thread_projects: Vec::new(),
        thread_query: String::new(),
        thread_query_editing: false,
        threads_loaded_at: None,
        threads_rx: None,
        threads_scan_options: None,
        pending_open_path: None,
        automations: load_automations(&root),
        services,
        active_agents: load_active_agents(),
        background: load_background(),
        agent_overlay: None,
        timing_overlay: None,
        background_detail: None,
        g_prefix: zdx_transcript::keys::GPrefix::default(),
        mouse_captured: true,
        scroll_guard_until: None,
        log_file_name: None,
        log_files: Vec::new(),
        log_file_index: 0,
        log_loaded: None,
        log_tail_lines: LOG_TAIL_LINES,
        log_lines: Vec::new(),
        log_visible: Vec::new(),
        log_level_filter: LevelFilter::All,
        log_query: String::new(),
        log_query_editing: false,
        log_target_filter: None,
        log_target_picker: None,
        log_selected: 0,
        log_offset: 0,
        log_follow: true,
        log_overlay_open: false,
        active_section: Section::Services,
        selected_index: 0,
        status_section: Section::Services,
        status_message: String::new(),
        status_set_at: None,
        should_quit: false,
        usage_stats: None,
        usage_scroll: 0,
        usage_span: UsageSpan::All,
        usage_line_count: 0,
        default_model,
        usage_rx: None,
        usage_scan_span: None,
        quotas: None,
        quota_rx: None,
        quota_backoff: HashMap::new(),
    };
    recompute_log_visible(&mut app);
    load_active_log(&mut app);
    Ok(app)
}

fn setup_terminal() -> Result<Terminal<CrosstermBackend<io::Stdout>>> {
    terminal::enable_raw_mode().context("enable raw mode")?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture).context("enter alternate screen")?;
    let backend = CrosstermBackend::new(stdout);
    Terminal::new(backend).context("create terminal")
}

fn restore_terminal(terminal: &mut Terminal<CrosstermBackend<io::Stdout>>) -> Result<()> {
    terminal::disable_raw_mode().context("disable raw mode")?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )
    .context("leave alternate screen")?;
    terminal.show_cursor().context("show cursor")
}

pub(crate) fn restart_force_for_key(key: KeyCode) -> bool {
    key == KeyCode::Char('R')
}

/// How long list scrolling stays swallowed after an overlay closes. Trackpad
/// momentum keeps emitting scroll events past the close; each swallowed event
/// re-arms the guard so the whole inertia tail dies out instead of moving the
/// list that was underneath the overlay.
const OVERLAY_CLOSE_SCROLL_GUARD: Duration = Duration::from_millis(250);

/// Whether any modal surface is stacked over the active section.
fn overlay_open(app: &MonitorApp) -> bool {
    app.model_picker.is_some()
        || app.timing_overlay.is_some()
        || app.background_detail.is_some()
        || app.agent_overlay.is_some()
        || app.log_overlay_open
        || app.log_target_picker.is_some()
        || app.thread_project_picker.is_some()
}

/// Whether keystrokes are currently being captured as text (queries, picker
/// filters), where the vim `g` prefix must not intercept typing.
fn text_input_active(app: &MonitorApp) -> bool {
    app.log_query_editing
        || app.thread_query_editing
        || app.model_picker.is_some()
        || app.log_target_picker.is_some()
        || app.thread_project_picker.is_some()
}

/// Key entry point: applies the `g` prefix, dispatches, and arms the scroll
/// guard when the key closed an overlay.
fn handle_key_event(app: &mut MonitorApp, key: KeyEvent) {
    let had_overlay = overlay_open(app);
    let text_input = text_input_active(app);
    if !text_input && key.code == KeyCode::Char('M') {
        toggle_mouse_capture(app);
        return;
    }
    let code = if text_input {
        app.g_prefix.reset();
        Some(key.code)
    } else {
        app.g_prefix.translate(key)
    };
    let Some(code) = code else {
        return;
    };
    dispatch_key_event(app, KeyEvent::new(code, key.modifiers));
    if had_overlay && !overlay_open(app) {
        app.scroll_guard_until = Some(Instant::now() + OVERLAY_CLOSE_SCROLL_GUARD);
    }
}

/// Releases or restores terminal mouse capture. While released, the terminal's
/// native selection/copy works everywhere; click/scroll handling resumes when
/// capture is restored.
fn toggle_mouse_capture(app: &mut MonitorApp) {
    let mut stdout = std::io::stdout();
    if app.mouse_captured {
        if execute!(stdout, DisableMouseCapture).is_ok() {
            app.mouse_captured = false;
            app.set_status("Mouse capture OFF — select/copy freely · M to re-enable");
        }
    } else if execute!(stdout, EnableMouseCapture).is_ok() {
        app.mouse_captured = true;
        app.set_status("Mouse capture ON");
    }
}

fn dispatch_key_event(app: &mut MonitorApp, key: KeyEvent) {
    if app.model_picker.is_some() {
        handle_model_picker_key(app, key.code);
        return;
    }
    if app.timing_overlay.is_some() {
        handle_timing_overlay_key(app, key.code);
        return;
    }
    if app.background_detail.is_some() {
        handle_background_detail_key(app, key.code);
        return;
    }
    if app.agent_overlay.is_some() {
        handle_agent_overlay_key(app, key.code);
        return;
    }
    if app.log_overlay_open {
        handle_log_overlay_key(app, key.code);
        return;
    }
    if app.active_section == Section::Logs && app.log_target_picker.is_some() {
        handle_log_target_picker_key(app, key.code);
        return;
    }
    if app.active_section == Section::Logs && app.log_query_editing {
        handle_log_query_key(app, key.code);
        return;
    }
    if app.active_section == Section::Logs && handle_logs_key(app, key.code) {
        return;
    }
    if app.active_section == Section::Threads && app.thread_project_picker.is_some() {
        handle_thread_project_picker_key(app, key.code);
        return;
    }
    if app.active_section == Section::Threads && app.thread_query_editing {
        handle_thread_query_key(app, key.code);
        return;
    }
    if app.active_section == Section::Threads && handle_threads_key(app, key.code) {
        return;
    }
    if app.active_section == Section::Usage && handle_usage_key(app, key.code) {
        return;
    }
    match key.code {
        KeyCode::Char('q') => app.should_quit = true,
        KeyCode::Tab => {
            switch_section(app, app.active_section.next());
        }
        KeyCode::BackTab => {
            switch_section(app, app.active_section.prev());
        }
        KeyCode::Char('j') | KeyCode::Down => {
            if app.active_section == Section::Config {
                move_config_selection(app, true);
            } else if app.active_section == Section::Usage {
                let max = usage_max_scroll(app);
                app.usage_scroll = app.usage_scroll.saturating_add(1).min(max);
            } else {
                let count = app.item_count();
                if count > 0 {
                    app.selected_index = (app.selected_index + 1).min(count - 1);
                }
            }
        }
        KeyCode::Char('k') | KeyCode::Up => {
            if app.active_section == Section::Config {
                move_config_selection(app, false);
            } else if app.active_section == Section::Usage {
                app.usage_scroll = app.usage_scroll.saturating_sub(1);
            } else if app.selected_index > 0 {
                app.selected_index -= 1;
            }
        }
        KeyCode::PageDown if app.active_section == Section::Config => {
            let page = config_page_size(app);
            let max = config_max_scroll(app);
            app.config_scroll = app.config_scroll.saturating_add(page).min(max);
        }
        KeyCode::PageUp if app.active_section == Section::Config => {
            let page = config_page_size(app);
            app.config_scroll = app.config_scroll.saturating_sub(page);
        }
        KeyCode::Char('d') | KeyCode::Delete if app.active_section == Section::Config => {
            delete_or_reset_selected(app);
        }
        KeyCode::Char('y') => copy_selected_thread_id(app),
        KeyCode::Char('x') if app.active_section == Section::Background => {
            kill_selected_background(app);
        }
        KeyCode::Char('r' | 'R') => restart_selected_service(app, restart_force_for_key(key.code)),
        KeyCode::Home => jump_to_edge(app, true),
        KeyCode::Char('G') | KeyCode::End => jump_to_edge(app, false),
        KeyCode::Enter => handle_enter_key(app),
        _ => {}
    }
}

/// `gg`/`G` on the active section: jump the selection (or scroll) to an edge.
fn jump_to_edge(app: &mut MonitorApp, top: bool) {
    match app.active_section {
        Section::Config => {
            app.config_scroll = if top { 0 } else { config_max_scroll(app) };
        }
        Section::Usage => {
            app.usage_scroll = if top { 0 } else { usage_max_scroll(app) };
        }
        _ => {
            let count = app.item_count();
            app.selected_index = if top { 0 } else { count.saturating_sub(1) };
        }
    }
}

fn handle_enter_key(app: &mut MonitorApp) {
    match app.active_section {
        Section::ActiveAgents => open_agent_overlay(app),
        Section::Background => open_background_detail(app),
        Section::Config => open_model_picker(app),
        _ => toggle_selected_service(app),
    }
}

/// Handle a key while the Usage section is active. Returns `true` if the key
/// was consumed (so the generic dispatcher should not also act on it).
fn handle_usage_key(app: &mut MonitorApp, key: KeyCode) -> bool {
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

/// Copies `text` via the shared clipboard (OSC 52 → system) and reports the
/// outcome in the status line.
pub(crate) fn copy_text(app: &mut MonitorApp, text: &str, done: &str) {
    match zdx_transcript::clipboard::Clipboard::copy(text) {
        Ok(()) => app.set_status(done),
        Err(_) => app.set_status("Copy failed"),
    }
}

fn handle_mouse_event(app: &mut MonitorApp, mouse: MouseEvent) {
    if handle_overlay_mouse(app, mouse) {
        return;
    }
    let kind = mouse.kind;
    // Trackpad momentum after an overlay close: swallow the inertia tail and
    // keep extending the guard until the events stop.
    if matches!(kind, MouseEventKind::ScrollDown | MouseEventKind::ScrollUp)
        && app
            .scroll_guard_until
            .is_some_and(|until| Instant::now() < until)
    {
        app.scroll_guard_until = Some(Instant::now() + OVERLAY_CLOSE_SCROLL_GUARD);
        return;
    }
    match kind {
        MouseEventKind::ScrollDown => {
            if app.active_section == Section::Config {
                let max = config_max_scroll(app);
                app.config_scroll = app.config_scroll.saturating_add(1).min(max);
            } else if app.active_section == Section::Usage {
                let max = usage_max_scroll(app);
                app.usage_scroll = app.usage_scroll.saturating_add(1).min(max);
            } else if app.active_section == Section::Logs {
                let total = app.log_lines.len();
                if total > 0 && app.log_selected + 1 < total {
                    app.log_selected += 1;
                    if app.log_selected + 1 == total {
                        app.log_follow = true;
                    }
                    ensure_log_selected_visible(app);
                }
            } else {
                let count = app.item_count();
                if count > 0 {
                    app.selected_index = (app.selected_index + 1).min(count - 1);
                }
            }
        }
        MouseEventKind::ScrollUp => {
            if app.active_section == Section::Config {
                app.config_scroll = app.config_scroll.saturating_sub(1);
            } else if app.active_section == Section::Usage {
                app.usage_scroll = app.usage_scroll.saturating_sub(1);
            } else if app.active_section == Section::Logs {
                if app.log_selected > 0 {
                    app.log_selected -= 1;
                    app.log_follow = false;
                    ensure_log_selected_visible(app);
                }
            } else {
                app.selected_index = app.selected_index.saturating_sub(1);
            }
        }
        _ => {}
    }
}

/// Mouse routing while a modal surface is stacked over the active section.
/// Returns `true` when the event was consumed (every open overlay consumes,
/// so scrolling never leaks to the list underneath).
fn handle_overlay_mouse(app: &mut MonitorApp, mouse: MouseEvent) -> bool {
    let kind = mouse.kind;
    if let Some(picker) = app.model_picker.as_mut() {
        match kind {
            MouseEventKind::ScrollUp => picker.selected = picker.selected.saturating_sub(1),
            MouseEventKind::ScrollDown => {
                let last = picker.matches.len().saturating_sub(1);
                picker.selected = (picker.selected + 1).min(last);
            }
            _ => {}
        }
        return true;
    }
    let overlay_page = agent_overlay_page_size(app);
    if let Some(state) = app.agent_overlay.as_mut() {
        if let Some(pane) = state.tool_pane.as_mut() {
            match kind {
                MouseEventKind::ScrollDown => pane.scroll = pane.scroll.saturating_add(1),
                MouseEventKind::ScrollUp => pane.scroll = pane.scroll.saturating_sub(1),
                _ => {}
            }
            return true;
        }
        let max_offset = state.max_scroll(overlay_page);
        let cur = state.top_line(overlay_page);
        match kind {
            MouseEventKind::ScrollDown => state.scroll = Some((cur + 1).min(max_offset)),
            MouseEventKind::ScrollUp => state.scroll = Some(cur.saturating_sub(1)),
            MouseEventKind::Down(MouseButton::Left) => {
                handle_overlay_click(state, mouse.row, overlay_page);
            }
            _ => {}
        }
        return true;
    }
    let page = (app.terminal_height.saturating_sub(2) as usize).max(1);
    if let Some(state) = app.timing_overlay.as_mut() {
        let max = state.lines.len().saturating_sub(page);
        match kind {
            MouseEventKind::ScrollDown => state.scroll = state.scroll.saturating_add(1).min(max),
            MouseEventKind::ScrollUp => state.scroll = state.scroll.saturating_sub(1),
            _ => {}
        }
        return true;
    }
    if let Some(state) = app.background_detail.as_mut() {
        let max = state.max_scroll(page);
        match kind {
            MouseEventKind::ScrollDown => state.scroll_to(state.offset(page) + 1, max),
            MouseEventKind::ScrollUp => {
                state.scroll_to(state.offset(page).saturating_sub(1), max);
            }
            _ => {}
        }
        return true;
    }
    app.log_overlay_open
}

fn toggle_selected_service(app: &mut MonitorApp) {
    if app.active_section == Section::Services
        && let Some(service) = app.services.get(app.selected_index)
    {
        match toggle_service(service) {
            Ok(message) => app.set_status(message),
            Err(err) => {
                app.set_status(format!("Failed to toggle {}: {err}", service.name));
            }
        }
    }
}

fn restart_selected_service(app: &mut MonitorApp, force: bool) {
    if app.active_section == Section::Services
        && let Some(service) = app.services.get(app.selected_index)
    {
        match service::restart(service.service, force) {
            Ok(message) => app.set_status(message),
            Err(err) => {
                if let Some(blocked) = err.downcast_ref::<service::RestartBlocked>() {
                    let active_runs = blocked.active_runs();
                    let suffix = if active_runs == 1 { "" } else { "s" };
                    app.set_status(format!(
                        "Restart blocked: {active_runs} active agent run{suffix}; wait or press R to force"
                    ));
                } else {
                    app.set_status(format!("Failed to restart {}: {err}", service.name));
                }
            }
        }
    }
}

/// How long a status message replaces the footer hints.
const STATUS_TTL: Duration = Duration::from_secs(4);

fn refresh_app(app: &mut MonitorApp) {
    if app
        .status_set_at
        .is_some_and(|set_at| set_at.elapsed() >= STATUS_TTL)
    {
        app.status_message.clear();
        app.status_set_at = None;
    }
    app.services = load_services();
    app.active_agents = load_active_agents();
    app.background = load_background();
    refresh_background_detail(app);
    load_active_log(app);
    app.clamp_selection();
    poll_usage_result(app);
    refresh_usage(app);
    poll_quota_result(app);
    refresh_quota(app);
    poll_threads_result(app);
}

/// Run the monitor dashboard.
///
/// # Errors
///
/// Returns an error if configuration cannot be loaded or terminal operations fail.
pub fn run(root: &Path) -> Result<()> {
    let mut app = build_app(root)?;
    let mut terminal = setup_terminal()?;

    let tick_rate = Duration::from_secs(1);
    let mut last_tick = Instant::now();

    loop {
        terminal.draw(|f| ui::render(f, &app))?;
        app.terminal_height = terminal.size().map_or(24, |r| r.height);
        app.terminal_width = terminal.size().map_or(80, |r| r.width);

        let timeout = tick_rate.saturating_sub(last_tick.elapsed());
        if event::poll(timeout).context("poll events")? {
            // Drain every queued event in one pass before redrawing. Otherwise a
            // burst (e.g. a fast mouse scroll) is processed one-per-frame and a
            // following key press like Esc is starved behind the backlog, which
            // looks like a freeze.
            loop {
                match event::read().context("read event")? {
                    Event::Key(key) if key.kind == KeyEventKind::Press => {
                        handle_key_event(&mut app, key);
                        if let Some(path) = app.pending_open_path.take() {
                            match open_path_in_editor(&mut terminal, &path) {
                                Ok(()) => {
                                    app.set_status(format!(
                                        "Opened raw thread {}",
                                        path.file_stem().unwrap_or_default().to_string_lossy()
                                    ));
                                }
                                Err(err) => {
                                    app.set_status(format!("Failed to open raw thread: {err}"));
                                }
                            }
                        }
                        refresh_app(&mut app);
                    }
                    Event::Mouse(mouse) => {
                        handle_mouse_event(&mut app, mouse);
                    }
                    _ => {}
                }
                if app.should_quit || !event::poll(Duration::ZERO).context("poll events")? {
                    break;
                }
            }
        }

        if last_tick.elapsed() >= tick_rate {
            refresh_app(&mut app);
            refresh_agent_overlay(&mut app);
            refresh_threads_if_stale(&mut app);
            last_tick = Instant::now();
        }

        if app.should_quit {
            break;
        }
    }

    restore_terminal(&mut terminal)
}

fn open_path_in_editor(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    path: &Path,
) -> Result<()> {
    restore_terminal(terminal)?;
    let open_result = open_in_editor(path);
    *terminal = setup_terminal()?;
    open_result.context(format!("open {} in editor", path.display()))
}

fn open_in_editor(path: &Path) -> io::Result<()> {
    let editor = std::env::var("VISUAL")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .or_else(|| {
            std::env::var("EDITOR")
                .ok()
                .filter(|value| !value.trim().is_empty())
        });

    let Some(editor) = editor else {
        return open::that(path);
    };
    let mut parts = editor.split_whitespace();
    let Some(program) = parts.next() else {
        return open::that(path);
    };
    std::process::Command::new(program)
        .args(parts)
        .arg(path)
        .status()
        .map(|_| ())
}

fn load_automations(root: &Path) -> Vec<AutomationInfo> {
    match automations::discover(root) {
        Ok(defs) => defs
            .into_iter()
            .map(|d| AutomationInfo {
                name: d.name,
                schedule: d.schedule,
            })
            .collect(),
        Err(_) => Vec::new(),
    }
}

fn load_services() -> Vec<ServiceInfo> {
    Service::ALL
        .into_iter()
        .map(|svc| {
            let state = service::state(svc);
            let launchd = if state.installed {
                "launchd"
            } else {
                "not installed"
            };
            let (status, details) = match (state.pid, state.uptime) {
                (Some(pid), uptime) => {
                    let uptime = uptime.map(service::format_uptime).unwrap_or_default();
                    (
                        "running".to_string(),
                        format!("PID {pid} | up {uptime} | {launchd}"),
                    )
                }
                (None, _) => ("stopped".to_string(), launchd.to_string()),
            };
            ServiceInfo {
                service: svc,
                name: svc.name().to_string(),
                status,
                details,
            }
        })
        .collect()
}

fn toggle_service(info: &ServiceInfo) -> Result<String> {
    if info.status == "running" {
        service::stop(info.service)
    } else {
        service::start(info.service)
    }
}

/// Loads running background processes for the Background tab, sorted so
/// same-thread processes are adjacent (the render groups them under a header).
fn load_background() -> Vec<BackgroundInfo> {
    let mut v: Vec<BackgroundInfo> = zdx_engine::background_activity::list_background()
        .into_iter()
        .filter(zdx_engine::background_activity::BackgroundProcess::is_running)
        .map(|p| {
            let uptime = p.uptime();
            BackgroundInfo {
                bg_id: p.bg_id,
                pid: p.pid,
                thread_id: p.thread_id,
                command: p.command,
                uptime,
            }
        })
        .collect();
    v.sort_by(|a, b| a.thread_id.cmp(&b.thread_id).then(a.bg_id.cmp(&b.bg_id)));
    v
}

/// Max bytes shown per stream (stdout/stderr) in the Background detail overlay.
const BACKGROUND_DETAIL_TAIL_BYTES: usize = 64 * 1024;

/// Rows visible inside the full-frame Background detail overlay (borders eat 2).
fn background_detail_page_size(app: &MonitorApp) -> usize {
    (app.terminal_height.saturating_sub(2) as usize).max(1)
}

/// Wrap width inside the full-frame Background detail overlay.
fn background_detail_wrap_width(app: &MonitorApp) -> usize {
    (app.terminal_width.saturating_sub(2) as usize).max(20)
}

/// Opens the detail overlay for the selected background process: full
/// command, marker metadata, and the stdout/stderr log tails.
fn open_background_detail(app: &mut MonitorApp) {
    let Some(info) = app.background.get(app.selected_index) else {
        return;
    };
    let bg_id = info.bg_id.clone();
    let title = format!("{bg_id} · pid {}", info.pid);
    let Some(lines) = build_background_detail_lines(&bg_id, background_detail_wrap_width(app))
    else {
        app.set_status(format!("No record for {bg_id} (it may have just exited)"));
        return;
    };
    app.background_detail = Some(BackgroundDetailState {
        bg_id,
        title,
        lines,
        scroll: 0,
        follow: true,
    });
}

/// Rebuilds the open detail overlay from disk on the refresh tick so a running
/// process's output keeps streaming in. If the marker vanished (tombstone
/// pruned), the last snapshot is kept until the overlay is closed.
fn refresh_background_detail(app: &mut MonitorApp) {
    let width = background_detail_wrap_width(app);
    if let Some(state) = app.background_detail.as_mut()
        && let Some(lines) = build_background_detail_lines(&state.bg_id, width)
    {
        state.lines = lines;
    }
}

/// Builds the detail body for one background process, pre-wrapped to `width`.
/// `None` when no marker exists for `bg_id` anymore.
fn build_background_detail_lines(bg_id: &str, width: usize) -> Option<Vec<Line<'static>>> {
    use zdx_engine::background_activity as bg;

    let rec = bg::get(bg_id)?;
    let label = |name: &str| Span::styled(format!(" {name:<9}"), Style::default().fg(Color::Cyan));
    let dim = Style::default().fg(Color::DarkGray);

    let mut lines: Vec<Line<'static>> = Vec::new();
    let status = if rec.is_running() {
        Span::styled(
            format!("running · up {}", rec.uptime()),
            Style::default().fg(Color::Green),
        )
    } else {
        let code = rec
            .exit_code
            .map_or_else(|| "killed/unknown".to_string(), |c| format!("code {c}"));
        Span::styled(format!("exited ({code})"), Style::default().fg(Color::Red))
    };
    lines.push(Line::from(vec![label("status"), status]));
    lines.push(Line::from(vec![
        label("pid"),
        Span::raw(format!("{} (pgid {})", rec.pid, rec.pgid)),
    ]));
    lines.push(Line::from(vec![
        label("started"),
        Span::raw(rec.started_at.clone()),
    ]));
    if let Some(exited_at) = &rec.exited_at {
        lines.push(Line::from(vec![
            label("ended"),
            Span::raw(exited_at.clone()),
        ]));
    }
    lines.push(Line::from(vec![
        label("thread"),
        Span::raw(
            rec.thread_id
                .clone()
                .unwrap_or_else(|| "(no thread)".to_string()),
        ),
    ]));
    lines.push(Line::from(vec![label("cwd"), Span::raw(rec.cwd.clone())]));

    let header = |name: &str, color: Color| {
        Line::from(Span::styled(
            format!(" ── {name} "),
            Style::default().fg(color).add_modifier(Modifier::BOLD),
        ))
    };

    lines.push(Line::default());
    lines.push(header("command", Color::Cyan));
    for l in rec.command.lines() {
        let l = zdx_transcript::text::sanitize_for_display(l);
        lines.push(Line::from(format!(" {l}")));
    }

    for (name, path, color) in [
        ("stdout", bg::stdout_log_path(bg_id), Color::Green),
        ("stderr", bg::stderr_log_path(bg_id), Color::Yellow),
    ] {
        lines.push(Line::default());
        lines.push(header(name, color));
        let tail = bg::read_log_tail(&path, BACKGROUND_DETAIL_TAIL_BYTES);
        if tail.is_empty() {
            lines.push(Line::from(Span::styled(" (no output)", dim)));
        } else {
            for l in tail.lines() {
                let l = zdx_transcript::text::sanitize_for_display(l);
                lines.push(Line::from(format!(" {l}")));
            }
        }
    }

    Some(
        lines
            .iter()
            .flat_map(|line| zdx_transcript::wrap_line_to_width(line, width))
            .collect(),
    )
}

fn handle_background_detail_key(app: &mut MonitorApp, key: KeyCode) {
    let page = background_detail_page_size(app);
    let Some(state) = app.background_detail.as_mut() else {
        return;
    };
    match key {
        KeyCode::Char('y') => {
            let command = zdx_engine::background_activity::get(&state.bg_id).map(|r| r.command);
            if let Some(command) = command {
                copy_text(app, &command, "Copied command");
            }
            return;
        }
        KeyCode::Char('Y') => {
            let text = lines_text(&state.lines);
            copy_text(app, &text, "Copied process detail");
            return;
        }
        _ => {}
    }
    if state.handle_key(key, page) {
        app.background_detail = None;
    }
}

/// Plain-text content of rendered lines, for clipboard copies.
pub(crate) fn lines_text(lines: &[Line<'static>]) -> String {
    lines
        .iter()
        .map(|line| {
            line.spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Stops the selected background process. Optimistically removes the row; the
/// on-tick refresh reconciles once the async kill lands. Runs the async kill on
/// its own thread with a current-thread Tokio runtime (the monitor has no
/// ambient runtime).
fn kill_selected_background(app: &mut MonitorApp) {
    let Some(info) = app.background.get(app.selected_index) else {
        return;
    };
    let bg_id = info.bg_id.clone();
    app.set_status(format!("Stopping {bg_id}…"));
    app.background.remove(app.selected_index);
    app.clamp_selection();
    std::thread::spawn(move || {
        if let Ok(rt) = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        {
            rt.block_on(async {
                let _ = zdx_engine::background_activity::kill_background(&bg_id).await;
            });
        }
    });
}
