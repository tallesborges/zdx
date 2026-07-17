use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::io::{Read, Seek};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant, SystemTime};
use std::{fs, io};

use anyhow::{Context, Result};
use crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEvent, KeyEventKind,
    MouseEventKind,
};
use crossterm::execute;
use crossterm::terminal::{self, EnterAlternateScreen, LeaveAlternateScreen};
use ratatui::prelude::*;
use serde_json::Value;
use zdx_engine::config::{self, paths};
use zdx_engine::core::thread_persistence;
use zdx_engine::models::available_models;
use zdx_engine::core::usage_stats::{self, UsageStats};
use zdx_engine::providers::subscription_quota::{self, QuotaError, SubscriptionQuota};
use zdx_engine::{agent_activity, automations, pidfile};

use crate::ui;

/// A single displayable line in the Config tab.
#[derive(Clone)]
pub enum ConfigLine {
    /// Section header (derived from a top-level object key, e.g. "providers").
    Section(String),
    /// Subtle separator between sub-groups within a section (e.g. between providers).
    Separator,
    /// Key-value row inside a section.
    Row(String, String),
}

const SENSITIVE_PATTERNS: &[&str] = &["api_key", "token", "secret", "password", "webhook"];

fn is_sensitive(key: &str) -> bool {
    let lower = key.to_lowercase();
    SENSITIVE_PATTERNS.iter().any(|p| lower.contains(p))
}

fn format_json_scalar(val: &Value) -> String {
    match val {
        Value::Null => "(unset)".to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n.to_string(),
        Value::String(s) if s.is_empty() => "(empty)".to_string(),
        Value::String(s) if s.len() > 60 => format!("{}…", &s[..57]),
        Value::String(s) => s.clone(),
        Value::Array(arr) if arr.is_empty() => "(empty)".to_string(),
        Value::Array(arr) => arr
            .iter()
            .map(|v| match v {
                Value::String(s) => s.clone(),
                other => other.to_string(),
            })
            .collect::<Vec<_>>()
            .join(", "),
        Value::Object(_) => "(object)".to_string(),
    }
}

fn flatten_object(
    obj: &serde_json::Map<String, Value>,
    prefix: &str,
    out: &mut Vec<(String, String)>,
) {
    for (key, val) in obj {
        let full_key = if prefix.is_empty() {
            key.clone()
        } else {
            format!("{prefix}.{key}")
        };
        match val {
            Value::Object(nested) => flatten_object(nested, &full_key, out),
            Value::Array(arr) if arr.iter().any(Value::is_object) => {
                for (i, item) in arr.iter().enumerate() {
                    let indexed = format!("{full_key}[{i}]");
                    if let Value::Object(nested) = item {
                        flatten_object(nested, &indexed, out);
                    } else {
                        let display = if is_sensitive(&full_key) && !matches!(item, Value::Null) {
                            "***".to_string()
                        } else {
                            format_json_scalar(item)
                        };
                        out.push((indexed, display));
                    }
                }
            }
            _ => {
                let display = if is_sensitive(&full_key) && !matches!(val, Value::Null) {
                    "***".to_string()
                } else {
                    format_json_scalar(val)
                };
                out.push((full_key, display));
            }
        }
    }
}

/// Build the display lines for a single section's object.
/// When the object's direct values are themselves objects (e.g. providers),
/// a `Separator` is emitted between each sub-group.
fn build_section_lines(section_obj: &serde_json::Map<String, Value>) -> Vec<ConfigLine> {
    let mut lines = Vec::new();
    let mut first_group = true;

    for (key, val) in section_obj {
        if let Value::Object(nested) = val {
            if !first_group {
                lines.push(ConfigLine::Separator);
            }
            first_group = false;
            let mut rows = Vec::new();
            flatten_object(nested, key, &mut rows);
            for (k, v) in rows {
                lines.push(ConfigLine::Row(k, v));
            }
        } else {
            let display = if is_sensitive(key) && !matches!(val, Value::Null) {
                "***".to_string()
            } else {
                format_json_scalar(val)
            };
            lines.push(ConfigLine::Row(key.clone(), display));
        }
    }

    lines
}

/// Serialize `config` to JSON and flatten it into displayable lines.
/// Top-level scalars are grouped under a synthetic "core" section.
/// Top-level objects each become their own named section.
pub fn build_config_lines(config: &config::Config) -> Vec<ConfigLine> {
    let value = match serde_json::to_value(config) {
        Ok(v) => v,
        Err(e) => return vec![ConfigLine::Row("error".to_string(), e.to_string())],
    };
    let Some(obj) = value.as_object() else {
        return Vec::new();
    };

    let mut core_rows: Vec<(String, String)> = Vec::new();
    let mut sections: Vec<(String, Vec<ConfigLine>)> = Vec::new();

    for (key, val) in obj {
        if let Value::Object(nested) = val {
            let section_lines = build_section_lines(nested);
            sections.push((key.clone(), section_lines));
        } else {
            let display = if is_sensitive(key) && !matches!(val, Value::Null) {
                "***".to_string()
            } else {
                format_json_scalar(val)
            };
            core_rows.push((key.clone(), display));
        }
    }

    let mut lines = Vec::new();

    // Show the main model with its thinking level inline (`model@thinking`) and
    // drop the standalone `thinking_level` row; role models carry `@thinking`
    // in their own stored value already.
    if let Some(level) = core_rows
        .iter()
        .find(|(k, _)| k == "thinking_level")
        .map(|(_, v)| v.clone())
    {
        if let Some(model_row) = core_rows.iter_mut().find(|(k, _)| k == "model") {
            model_row.1 = format!("{}@{level}", model_row.1);
        }
        core_rows.retain(|(k, _)| k != "thinking_level");
    }

    if !core_rows.is_empty() {
        lines.push(ConfigLine::Section("core".to_string()));
        for (k, v) in core_rows {
            lines.push(ConfigLine::Row(k, v));
        }
    }

    for (name, section_lines) in sections {
        lines.push(ConfigLine::Section(name));
        lines.extend(section_lines);
    }

    lines
}

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
    pub automations: Vec<AutomationInfo>,
    pub services: Vec<ServiceInfo>,
    pub active_agents: Vec<ActiveAgentInfo>,
    /// Open transcript overlay for a selected active agent, if any.
    pub agent_overlay: Option<AgentOverlayState>,
    pub log_file_name: Option<String>,
    pub log_lines: Vec<String>,
    pub log_selected: usize,
    pub log_offset: usize,
    pub log_follow: bool,
    pub log_overlay_open: bool,
    pub active_section: Section,
    pub selected_index: usize,
    pub status_section: Section,
    pub status_message: String,
    pub should_quit: bool,
    /// Services that should be kept running by the supervisor (toggled with Ctrl+R).
    pub supervised_services: BTreeSet<String>,
    /// Per-service cooldown for automatic restart attempts.
    pub last_auto_restart: BTreeMap<String, Instant>,
    /// Cached usage/cost aggregation for the Usage tab (computed lazily).
    pub usage_stats: Option<CachedUsageStats>,
    /// Vertical scroll offset for the Usage tab.
    pub usage_scroll: usize,
    /// Rendered line count of the cached usage view (for scroll clamping).
    pub usage_line_count: usize,
    /// Default model used to attribute legacy usage (mirrors `config.model`).
    pub default_model: String,
    /// Receiver for an in-flight background usage scan, if any. The scan runs
    /// off the UI thread so the dashboard never freezes during aggregation.
    pub usage_rx: Option<mpsc::Receiver<Result<UsageStats>>>,
    /// Cached subscription-quota snapshot per provider (read-only OAuth).
    pub quotas: Option<CachedQuotas>,
    /// Receiver for an in-flight background quota fetch, if any.
    pub quota_rx: Option<mpsc::Receiver<QuotaFetchResult>>,
    /// Per-provider rate-limit cooldown: don't refetch before this instant.
    pub quota_backoff: HashMap<&'static str, Instant>,
}

/// Result payload from a background quota fetch: one entry per provider.
type QuotaFetchResult = Vec<(
    &'static str,
    std::result::Result<SubscriptionQuota, QuotaError>,
)>;

/// A cached snapshot of the usage aggregation plus when it was computed.
pub struct CachedUsageStats {
    pub stats: UsageStats,
    pub computed_at: Instant,
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
    pub quota: Option<SubscriptionQuota>,
    pub error: Option<String>,
}

pub struct ThreadInfo {
    pub id: String,
    pub title: Option<String>,
    pub surface: Option<String>,
    pub modified: String,
}

pub struct AutomationInfo {
    pub name: String,
    pub schedule: Option<String>,
}

#[derive(Clone)]
pub struct ServiceInfo {
    pub key: String,
    pub name: String,
    pub status: String,
    pub details: String,
}

pub struct ActiveAgentInfo {
    pub pid: u32,
    pub surface: String,
    pub thread_id: String,
    /// Full (un-truncated) thread id used to locate the transcript file.
    /// `None` for tracked runs that don't persist a thread.
    pub full_thread_id: Option<String>,
    pub model: String,
    pub provider: String,
    pub thinking: String,
    pub uptime: String,
    pub kind: Option<String>,
    pub subagent_name: Option<String>,
}

/// State for the Active Agents transcript overlay (drill-in on `Enter`).
pub struct AgentOverlayState {
    /// Thread id captured when the overlay was opened (never re-derived from
    /// the live selection).
    pub thread_id: String,
    /// Header label, e.g. `provider:model@thinking abc12345`.
    pub title: String,
    /// Rendered transcript lines (formatted markdown via `zdx-transcript`).
    pub lines: Vec<Line<'static>>,
    /// Manual top-line scroll offset. `None` follows the newest content.
    pub scroll: Option<usize>,
    /// The captured run is no longer active (marker gone). Presentation only —
    /// reads continue while the overlay is open.
    pub ended: bool,
    /// No thread id was available for this run.
    pub unavailable: bool,
    /// Last-seen transcript file size, to skip reparsing unchanged files.
    pub file_len: u64,
    /// Last-seen transcript file mtime, to skip reparsing unchanged files.
    pub file_mtime: Option<SystemTime>,
    /// Width the transcript was last rendered at (re-render on resize).
    pub width: usize,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Section {
    Services,
    ActiveAgents,
    Config,
    Threads,
    Usage,
    Automations,
    Logs,
}

impl Section {
    pub const ALL: [Section; 7] = [
        Section::Services,
        Section::ActiveAgents,
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
            Section::ActiveAgents => Section::Config,
            Section::Config => Section::Threads,
            Section::Threads => Section::Usage,
            Section::Usage => Section::Automations,
            Section::Automations => Section::Logs,
            Section::Logs => Section::Services,
        }
    }
}

impl MonitorApp {
    fn item_count(&self) -> usize {
        match self.active_section {
            Section::Services => self.services.len(),
            Section::Config | Section::Logs | Section::Usage => 0,
            Section::ActiveAgents => self.active_agents.len(),
            Section::Threads => self.threads.len(),
            Section::Automations => self.automations.len(),
        }
    }

    fn clamp_selection(&mut self) {
        let count = self.item_count();
        if count == 0 {
            self.selected_index = 0;
        } else {
            self.selected_index = self.selected_index.min(count - 1);
        }
    }

    fn set_status(&mut self, message: impl Into<String>) {
        self.status_section = self.active_section;
        self.status_message = message.into();
    }
}

/// Number of lines `render_config` will produce for these lines
/// (sections get a blank spacer before them, except the first).
fn rendered_line_count(config_lines: &[ConfigLine]) -> usize {
    let sections = config_lines
        .iter()
        .filter(|l| matches!(l, ConfigLine::Section(_)))
        .count();
    // Every ConfigLine is 1 rendered line; sections also get a blank line before (except first)
    config_lines.len() + sections.saturating_sub(1)
}

/// Visible content rows in the Config panel (terminal height minus chrome).
fn config_page_size(app: &MonitorApp) -> usize {
    // layout: 3 (tabs) + content + 3 (footer); panel borders take 2 more rows
    (app.terminal_height.saturating_sub(8) as usize).max(1)
}

/// Maximum valid scroll offset so the last line stays visible.
fn config_max_scroll(app: &MonitorApp) -> usize {
    app.config_line_count.saturating_sub(config_page_size(app))
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
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(usage_stats::aggregate_usage(&model));
    });
    app.usage_rx = Some(rx);
}

/// Collect a finished background usage scan, if any, into the cache. Non-
/// blocking: returns immediately when the scan is still running.
fn poll_usage_result(app: &mut MonitorApp) {
    let Some(rx) = &app.usage_rx else {
        return;
    };
    match rx.try_recv() {
        Ok(result) => {
            app.usage_rx = None;
            match result {
                Ok(stats) => {
                    let cached = CachedUsageStats {
                        stats,
                        computed_at: Instant::now(),
                    };
                    app.usage_line_count = ui::usage_line_count(&cached, app.quotas.as_ref());
                    app.usage_stats = Some(cached);
                    app.usage_scroll = app.usage_scroll.min(usage_max_scroll(app));
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
    let ready = |provider: &'static str| {
        app.quota_backoff
            .get(provider)
            .is_none_or(|until| *until <= now)
    };
    let fetch_claude = ready(subscription_quota::PROVIDER_CLAUDE);
    let fetch_codex = ready(subscription_quota::PROVIDER_CODEX);
    if !fetch_claude && !fetch_codex {
        return;
    }
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let results = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map(|rt| {
                rt.block_on(async {
                    let mut out: QuotaFetchResult = Vec::new();
                    if fetch_claude {
                        out.push((
                            subscription_quota::PROVIDER_CLAUDE,
                            subscription_quota::fetch_claude_quota().await,
                        ));
                    }
                    if fetch_codex {
                        out.push((
                            subscription_quota::PROVIDER_CODEX,
                            subscription_quota::fetch_codex_quota().await,
                        ));
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
        Some(cached) => ui::usage_line_count(cached, app.quotas.as_ref()),
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
    for (provider, res) in results {
        let idx = entries.iter().position(|e| e.provider == provider);
        let prev_quota = idx.and_then(|i| entries[i].quota.clone());
        let new_entry = match res {
            Ok(quota) => {
                app.quota_backoff.remove(provider);
                Some(QuotaEntry {
                    provider,
                    quota: Some(quota),
                    error: None,
                })
            }
            // Not logged in: drop the row unless we already had a value.
            Err(QuotaError::NotAuthenticated) => prev_quota.map(|quota| QuotaEntry {
                provider,
                quota: Some(quota),
                error: Some(QuotaError::NotAuthenticated.reason()),
            }),
            Err(err) => {
                if let QuotaError::RateLimited { retry_after_secs } = err {
                    let cooldown =
                        retry_after_secs.map_or(QUOTA_BACKOFF_DEFAULT, Duration::from_secs);
                    app.quota_backoff
                        .insert(provider, Instant::now() + cooldown);
                }
                Some(QuotaEntry {
                    provider,
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

/// Number of log lines tailed from the newest log file.
const LOG_TAIL_LINES: usize = 500;

/// Visible content rows in the Logs panel (same chrome as Config).
fn log_page_size(app: &MonitorApp) -> usize {
    (app.terminal_height.saturating_sub(8) as usize).max(1)
}

/// Adjust `log_offset` so `log_selected` is in the visible window.
fn ensure_log_selected_visible(app: &mut MonitorApp) {
    let page = log_page_size(app);
    if app.log_selected < app.log_offset {
        app.log_offset = app.log_selected;
    } else if app.log_selected >= app.log_offset + page {
        app.log_offset = app.log_selected + 1 - page;
    }
}

/// Read up to `max_lines` final lines from a log file by tailing the last 256 KiB.
fn tail_lines(path: &Path, max_lines: usize) -> io::Result<Vec<String>> {
    let mut file = fs::File::open(path)?;
    let metadata = file.metadata()?;
    let file_size = metadata.len();
    let read_size: u64 = file_size.min(256 * 1024);
    let start = file_size.saturating_sub(read_size);
    file.seek(io::SeekFrom::Start(start))?;
    let mut buf = Vec::with_capacity(read_size as usize);
    file.read_to_end(&mut buf)?;
    let text = String::from_utf8_lossy(&buf);
    let mut lines: Vec<String> = text.lines().map(String::from).collect();
    // If we started mid-file, the first line is likely a partial — drop it.
    if start > 0 && !lines.is_empty() {
        lines.remove(0);
    }
    let len = lines.len();
    if len > max_lines {
        lines.drain(0..len - max_lines);
    }
    Ok(lines)
}

/// Find the newest file in `~/.zdx/logs/` (by mtime) and tail its last lines.
fn load_logs(max_lines: usize) -> (Option<String>, Vec<String>) {
    let dir = paths::zdx_home().join("logs");
    let Ok(entries) = fs::read_dir(&dir) else {
        return (None, Vec::new());
    };
    let mut newest: Option<(std::time::SystemTime, PathBuf)> = None;
    for entry in entries.flatten() {
        let Ok(metadata) = entry.metadata() else {
            continue;
        };
        if !metadata.is_file() {
            continue;
        }
        let mtime = metadata.modified().unwrap_or(std::time::UNIX_EPOCH);
        let path = entry.path();
        match &newest {
            Some((t, _)) if *t >= mtime => {}
            _ => newest = Some((mtime, path)),
        }
    }
    let Some((_, path)) = newest else {
        return (None, Vec::new());
    };
    let file_name = path.file_name().map(|s| s.to_string_lossy().into_owned());
    let lines = tail_lines(&path, max_lines).unwrap_or_default();
    (file_name, lines)
}

fn build_app(root: &Path) -> Result<MonitorApp> {
    let root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    let config = config::Config::load().context("load config")?;
    let default_model = config.model.clone();
    let config_lines = build_config_lines(&config);
    let config_line_count = rendered_line_count(&config_lines);
    let (log_file_name, log_lines) = load_logs(LOG_TAIL_LINES);
    let services = load_services();

    Ok(MonitorApp {
        config_lines,
        config_line_count,
        config_scroll: 0,
        config_selected: 0,
        model_picker: None,
        terminal_height: 24,
        terminal_width: 80,
        root: root.clone(),
        threads: load_threads(),
        automations: load_automations(&root),
        services,
        active_agents: load_active_agents(),
        agent_overlay: None,
        log_file_name,
        log_lines,
        log_selected: 0,
        log_offset: 0,
        log_follow: true,
        log_overlay_open: false,
        active_section: Section::Services,
        selected_index: 0,
        status_section: Section::Services,
        status_message: String::new(),
        should_quit: false,
        supervised_services: BTreeSet::new(),
        last_auto_restart: BTreeMap::new(),
        usage_stats: None,
        usage_scroll: 0,
        usage_line_count: 0,
        default_model,
        usage_rx: None,
        quotas: None,
        quota_rx: None,
        quota_backoff: HashMap::new(),
    })
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

fn handle_key_event(app: &mut MonitorApp, key: KeyEvent) {
    if app.model_picker.is_some() {
        handle_model_picker_key(app, key.code);
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
    if app.active_section == Section::Logs && handle_logs_key(app, key.code) {
        return;
    }
    if key.code == KeyCode::Char('r')
        && key
            .modifiers
            .contains(crossterm::event::KeyModifiers::CONTROL)
    {
        toggle_supervision(app);
        return;
    }
    match key.code {
        KeyCode::Char('q') => app.should_quit = true,
        KeyCode::Tab => {
            app.active_section = app.active_section.next();
            app.selected_index = 0;
            app.config_scroll = 0;
            app.usage_scroll = 0;
            if app.active_section == Section::Logs {
                app.log_follow = true;
                let total = app.log_lines.len();
                if total > 0 {
                    app.log_selected = total - 1;
                    let page = log_page_size(app);
                    app.log_offset = total.saturating_sub(page);
                }
            }
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
        KeyCode::PageDown if app.active_section == Section::Usage => {
            let page = usage_page_size(app);
            let max = usage_max_scroll(app);
            app.usage_scroll = app.usage_scroll.saturating_add(page).min(max);
        }
        KeyCode::PageUp if app.active_section == Section::Usage => {
            let page = usage_page_size(app);
            app.usage_scroll = app.usage_scroll.saturating_sub(page);
        }
        KeyCode::Char('R') if app.active_section == Section::Usage => {
            start_usage_scan(app);
            start_quota_fetch(app);
            app.set_status("Refreshing usage stats…");
        }
        KeyCode::Char('y') => copy_selected_thread_id(app),
        KeyCode::Char('r') => restart_selected_service(app),
        KeyCode::Enter => {
            if app.active_section == Section::ActiveAgents {
                open_agent_overlay(app);
            } else if app.active_section == Section::Config {
                open_model_picker(app);
            } else {
                toggle_selected_service(app);
            }
        }
        _ => {}
    }
}

/// Handle a key while the Logs section is active. Returns `true` if the key
/// was consumed (so the generic dispatcher should not also act on it).
fn handle_logs_key(app: &mut MonitorApp, key: KeyCode) -> bool {
    let total = app.log_lines.len();
    match key {
        KeyCode::Char('j') | KeyCode::Down => {
            if total > 0 && app.log_selected + 1 < total {
                app.log_selected += 1;
                if app.log_selected + 1 == total {
                    app.log_follow = true;
                }
                ensure_log_selected_visible(app);
            }
            true
        }
        KeyCode::Char('k') | KeyCode::Up => {
            if app.log_selected > 0 {
                app.log_selected -= 1;
                app.log_follow = false;
                ensure_log_selected_visible(app);
            }
            true
        }
        KeyCode::PageDown => {
            if total > 0 {
                let page = log_page_size(app);
                app.log_selected = (app.log_selected + page).min(total - 1);
                if app.log_selected + 1 == total {
                    app.log_follow = true;
                }
                ensure_log_selected_visible(app);
            }
            true
        }
        KeyCode::PageUp => {
            let page = log_page_size(app);
            app.log_selected = app.log_selected.saturating_sub(page);
            app.log_follow = false;
            ensure_log_selected_visible(app);
            true
        }
        KeyCode::Char('G') | KeyCode::End => {
            if total > 0 {
                app.log_selected = total - 1;
                app.log_follow = true;
                let page = log_page_size(app);
                app.log_offset = total.saturating_sub(page);
            }
            true
        }
        KeyCode::Enter => {
            if total > 0 {
                app.log_overlay_open = true;
            }
            true
        }
        _ => false,
    }
}

fn handle_log_overlay_key(app: &mut MonitorApp, key: KeyCode) {
    match key {
        KeyCode::Esc | KeyCode::Char('q') | KeyCode::Enter => {
            app.log_overlay_open = false;
        }
        KeyCode::Char('y') => copy_selected_log_entry(app),
        _ => {}
    }
}

fn copy_selected_log_entry(app: &mut MonitorApp) {
    if let Some(line) = app.log_lines.get(app.log_selected).cloned() {
        let _ = std::process::Command::new("pbcopy")
            .stdin(std::process::Stdio::piped())
            .spawn()
            .and_then(|mut child| {
                use std::io::Write;
                if let Some(ref mut stdin) = child.stdin {
                    stdin.write_all(line.as_bytes())?;
                }
                child.wait()
            });
        app.set_status("Copied log entry");
    }
}

fn handle_mouse_event(app: &mut MonitorApp, kind: MouseEventKind) {
    if app.model_picker.is_some() {
        if let Some(picker) = app.model_picker.as_mut() {
            match kind {
                MouseEventKind::ScrollUp => picker.selected = picker.selected.saturating_sub(1),
                MouseEventKind::ScrollDown => {
                    let last = picker.matches.len().saturating_sub(1);
                    picker.selected = (picker.selected + 1).min(last);
                }
                _ => {}
            }
        }
        return;
    }
    let overlay_page = agent_overlay_page_size(app);
    if let Some(state) = app.agent_overlay.as_mut() {
        let max_offset = state.lines.len().saturating_sub(overlay_page);
        let cur = state.scroll.unwrap_or(max_offset);
        match kind {
            MouseEventKind::ScrollDown => state.scroll = Some((cur + 1).min(max_offset)),
            MouseEventKind::ScrollUp => state.scroll = Some(cur.saturating_sub(1)),
            _ => {}
        }
        return;
    }
    if app.log_overlay_open {
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

fn copy_selected_thread_id(app: &mut MonitorApp) {
    if app.active_section == Section::Threads
        && let Some(t) = app.threads.get(app.selected_index)
    {
        let _ = std::process::Command::new("pbcopy")
            .stdin(std::process::Stdio::piped())
            .spawn()
            .and_then(|mut child| {
                use std::io::Write;
                if let Some(ref mut stdin) = child.stdin {
                    stdin.write_all(t.id.as_bytes())?;
                }
                child.wait()
            });
        app.set_status(format!("Copied thread ID {}", t.id));
    }
}

fn toggle_selected_service(app: &mut MonitorApp) {
    if app.active_section == Section::Services
        && let Some(service) = app.services.get(app.selected_index)
    {
        match toggle_service(service, &app.root) {
            Ok(message) => app.set_status(message),
            Err(err) => {
                app.set_status(format!("Failed to toggle {}: {err}", service.name));
            }
        }
    }
}

fn restart_selected_service(app: &mut MonitorApp) {
    if app.active_section == Section::Services
        && let Some(service) = app.services.get(app.selected_index)
    {
        match restart_service(service, &app.root) {
            Ok(message) => app.set_status(message),
            Err(err) => {
                app.set_status(format!("Failed to restart {}: {err}", service.name));
            }
        }
    }
}

fn toggle_supervision(app: &mut MonitorApp) {
    if app.active_section != Section::Services {
        return;
    }
    let Some(service) = app.services.get(app.selected_index) else {
        return;
    };
    let key = service.key.clone();
    let name = service.name.clone();
    if app.supervised_services.remove(&key) {
        pidfile::unmark_supervised(&key);
        app.last_auto_restart.remove(&key);
        app.set_status(format!("{name}: supervision off"));
    } else {
        match pidfile::mark_supervised(&key) {
            Ok(()) => {
                app.supervised_services.insert(key);
                app.set_status(format!("{name}: supervision on"));
            }
            Err(err) => {
                app.set_status(format!("{name}: supervision failed: {err}"));
            }
        }
    }
}

/// Minimum interval between automatic service restart attempts.
const SERVICE_RESTART_COOLDOWN: Duration = Duration::from_secs(5);

/// After service refresh, auto-restart any supervised service that should be running but isn't.
fn supervise_services(app: &mut MonitorApp) {
    if app.supervised_services.is_empty() {
        return;
    }

    let now = Instant::now();

    let stalled: Vec<usize> = app
        .services
        .iter()
        .enumerate()
        .filter(|(_, s)| app.supervised_services.contains(&s.key) && s.status != "running")
        .map(|(i, _)| i)
        .collect();

    for idx in stalled {
        let key = app.services[idx].key.clone();

        let last = app
            .last_auto_restart
            .get(&key)
            .copied()
            .unwrap_or_else(|| now.checked_sub(SERVICE_RESTART_COOLDOWN).unwrap_or(now));
        if now.duration_since(last) < SERVICE_RESTART_COOLDOWN {
            continue;
        }

        let service = app.services[idx].clone();
        app.last_auto_restart.insert(key, now);
        match start_service(&service, &app.root) {
            Ok(msg) => app.set_status(format!("Auto-restart: {msg}")),
            Err(err) => app.set_status(format!("Auto-restart failed: {err}")),
        }
    }
}

fn refresh_app(app: &mut MonitorApp) {
    app.services = load_services();
    app.active_agents = load_active_agents();
    let (log_file_name, log_lines) = load_logs(LOG_TAIL_LINES);
    app.log_file_name = log_file_name;
    app.log_lines = log_lines;
    let total = app.log_lines.len();
    if total == 0 {
        app.log_selected = 0;
        app.log_offset = 0;
        app.log_overlay_open = false;
    } else if app.log_follow {
        app.log_selected = total - 1;
        let page = log_page_size(app);
        app.log_offset = total.saturating_sub(page);
    } else {
        app.log_selected = app.log_selected.min(total - 1);
        ensure_log_selected_visible(app);
    }
    app.clamp_selection();
    supervise_services(app);
    poll_usage_result(app);
    refresh_usage(app);
    poll_quota_result(app);
    refresh_quota(app);
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
                        refresh_app(&mut app);
                    }
                    Event::Mouse(mouse) => {
                        handle_mouse_event(&mut app, mouse.kind);
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
            last_tick = Instant::now();
        }

        if app.should_quit {
            break;
        }
    }

    for key in &app.supervised_services {
        pidfile::unmark_supervised(key);
    }

    restore_terminal(&mut terminal)
}

fn load_threads() -> Vec<ThreadInfo> {
    let dir = paths::threads_dir();
    let mut entries: Vec<_> = match fs::read_dir(&dir) {
        Ok(rd) => rd
            .filter_map(Result::ok)
            .filter(|e| e.path().extension().is_some_and(|ext| ext == "jsonl"))
            .collect(),
        Err(_) => return Vec::new(),
    };

    // Sort by mtime descending
    entries.sort_by(|a, b| {
        let ma = a
            .metadata()
            .and_then(|m| m.modified())
            .unwrap_or(std::time::UNIX_EPOCH);
        let mb = b
            .metadata()
            .and_then(|m| m.modified())
            .unwrap_or(std::time::UNIX_EPOCH);
        mb.cmp(&ma)
    });

    entries.truncate(50);

    entries
        .into_iter()
        .filter_map(|entry| {
            let path = entry.path();
            let id = path
                .file_stem()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string();

            let modified = entry
                .metadata()
                .and_then(|m| m.modified())
                .ok()
                .map(|t| {
                    let dt: chrono::DateTime<chrono::Local> = t.into();
                    dt.format("%Y-%m-%d %H:%M").to_string()
                })
                .unwrap_or_default();

            let (title, surface, origin_kind) = read_thread_meta(&path);
            // Hide child runs (subagents/helpers) from the dashboard list.
            if origin_kind.is_some() {
                return None;
            }

            Some(ThreadInfo {
                id,
                title,
                surface,
                modified,
            })
        })
        .collect()
}

fn read_thread_meta(path: &Path) -> (Option<String>, Option<String>, Option<String>) {
    use std::io::BufRead;
    let Ok(file) = fs::File::open(path) else {
        return (None, None, None);
    };
    let reader = io::BufReader::new(file);
    let Some(Ok(line)) = reader.lines().next() else {
        return (None, None, None);
    };
    let first_line = line;
    let v: Value = match serde_json::from_str(&first_line) {
        Ok(v) => v,
        Err(_) => return (None, None, None),
    };
    let title = v.get("title").and_then(Value::as_str).map(String::from);
    let surface = v.get("surface").and_then(Value::as_str).map(String::from);
    let origin_kind = v
        .get("origin_kind")
        .and_then(Value::as_str)
        .map(String::from);
    (title, surface, origin_kind)
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
    let mut service_names: BTreeSet<String> =
        BTreeSet::from(["daemon".to_string(), "bot".to_string()]);
    let run_dir = paths::zdx_home().join("run");
    if let Ok(entries) = fs::read_dir(&run_dir) {
        for entry in entries.filter_map(Result::ok) {
            let path = entry.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("pid") {
                continue;
            }
            let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
                continue;
            };
            if stem == "bot" {
                service_names.insert(stem.to_string());
            }
        }
    }

    service_names
        .into_iter()
        .filter_map(|service_name| {
            let display_name = service_name.clone();

            match pidfile::status(&service_name) {
                pidfile::ServiceStatus::Running { pid, started } => {
                    let uptime = started
                        .and_then(|s| s.elapsed().ok())
                        .map(format_duration)
                        .unwrap_or_default();
                    Some(ServiceInfo {
                        key: service_name.clone(),
                        name: display_name,
                        status: "running".to_string(),
                        details: format!("PID {pid} | up {uptime}"),
                    })
                }
                pidfile::ServiceStatus::Stopped if service_name == "daemon" => Some(ServiceInfo {
                    key: service_name.clone(),
                    name: display_name,
                    status: "stopped".to_string(),
                    details: String::new(),
                }),
                pidfile::ServiceStatus::Stopped if service_name == "bot" => Some(ServiceInfo {
                    key: service_name.clone(),
                    name: display_name,
                    status: "stopped".to_string(),
                    details: String::new(),
                }),
                pidfile::ServiceStatus::Stopped => None,
            }
        })
        .collect()
}

fn start_service(service: &ServiceInfo, root: &Path) -> Result<String> {
    if service.status == "running" {
        return Ok(format!("{} is already running", service.name));
    }

    let exe = std::env::current_exe().context("resolve current executable")?;
    let mut command = Command::new(exe);
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());

    match service.key.as_str() {
        "daemon" => {
            command
                .arg("--root")
                .arg(root)
                .arg("automations")
                .arg("daemon");
        }
        "bot" => {
            command.arg("--root").arg(root).arg("bot");
        }
        _ => anyhow::bail!("unsupported service '{}'", service.name),
    }

    let child = command
        .spawn()
        .with_context(|| format!("spawn {}", service.name))?;
    Ok(format!("Started {} (PID {})", service.name, child.id()))
}

fn stop_service(service: &ServiceInfo) -> Result<String> {
    match pidfile::terminate(&service.key)? {
        Some(pid) => Ok(format!("Stopping {} (PID {})…", service.name, pid)),
        None => Ok(format!("{} is already stopped", service.name)),
    }
}

fn toggle_service(service: &ServiceInfo, root: &Path) -> Result<String> {
    if service.status == "running" {
        stop_service(service)
    } else {
        start_service(service, root)
    }
}

fn restart_service(service: &ServiceInfo, root: &Path) -> Result<String> {
    if service.status == "running" {
        let _ = stop_service(service)?;
    }
    start_service(service, root).map(|message| format!("Restarted {} • {message}", service.name))
}

fn format_duration(d: std::time::Duration) -> String {
    let secs = d.as_secs();
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3600 {
        format!("{}m", secs / 60)
    } else if secs < 86400 {
        format!("{}h {}m", secs / 3600, (secs % 3600) / 60)
    } else {
        format!("{}d {}h", secs / 86400, (secs % 86400) / 3600)
    }
}

fn load_active_agents() -> Vec<ActiveAgentInfo> {
    agent_activity::list_active()
        .into_iter()
        .map(|r| {
            let short_thread = r
                .thread_id
                .as_deref()
                .map_or("-", |id| if id.len() > 8 { &id[..8] } else { id })
                .to_string();
            ActiveAgentInfo {
                pid: r.pid,
                surface: r.surface.unwrap_or_else(|| "-".to_string()),
                thread_id: short_thread,
                full_thread_id: r.thread_id.filter(|id| !id.is_empty()),
                model: r.model.unwrap_or_else(|| "-".to_string()),
                provider: r.provider.unwrap_or_else(|| "-".to_string()),
                thinking: r.thinking.unwrap_or_else(|| "-".to_string()),
                uptime: agent_activity::uptime_since(&r.started_at),
                kind: r.kind,
                subagent_name: r.subagent_name,
            }
        })
        .collect()
}

/// Path to a thread's transcript JSONL.
fn transcript_path(id: &str) -> PathBuf {
    paths::threads_dir().join(format!("{id}.jsonl"))
}

/// Max transcript cells kept for rendering (bounds render size, not I/O).
/// Applied after building cells so `tool_use`/`tool_result` pairs never split.
const TRANSCRIPT_MAX_CELLS: usize = 200;

/// Reads a thread transcript and renders it to formatted ratatui lines using
/// the shared `zdx-transcript` renderer (markdown, wrapping, tool pairing).
/// Best-effort; a missing file yields no lines.
fn read_thread_transcript(id: &str, width: usize) -> Vec<Line<'static>> {
    let events = thread_persistence::load_thread_events(id).unwrap_or_default();
    let cells = zdx_transcript::build_transcript_from_events(&events);
    let start = cells.len().saturating_sub(TRANSCRIPT_MAX_CELLS);
    zdx_transcript::cells_to_lines(&cells[start..], width.max(1))
}

/// Number of visible transcript rows in the full-screen overlay.
fn agent_overlay_page_size(app: &MonitorApp) -> usize {
    (app.terminal_height.saturating_sub(2) as usize).max(1)
}

/// Opens the transcript overlay for the currently selected active agent.
fn open_agent_overlay(app: &mut MonitorApp) {
    let Some(a) = app.active_agents.get(app.selected_index) else {
        return;
    };
    let title = format!("{}:{}@{} {}", a.provider, a.model, a.thinking, a.thread_id);
    let width = app.terminal_width.saturating_sub(2) as usize;
    match a.full_thread_id.clone() {
        Some(id) => {
            let mut state = AgentOverlayState {
                thread_id: id,
                title,
                lines: Vec::new(),
                scroll: None,
                ended: false,
                unavailable: false,
                file_len: 0,
                file_mtime: None,
                width,
            };
            load_transcript_into(&mut state);
            app.agent_overlay = Some(state);
        }
        None => {
            app.agent_overlay = Some(AgentOverlayState {
                thread_id: String::new(),
                title,
                lines: vec![Line::from("transcript unavailable (no thread id)")],
                scroll: None,
                ended: false,
                unavailable: true,
                file_len: 0,
                file_mtime: None,
                width,
            });
        }
    }
}

/// File length + mtime used to detect transcript changes between ticks.
/// Missing/unreadable file collapses to `(0, None)`.
fn transcript_file_fingerprint(path: &Path) -> (u64, Option<SystemTime>) {
    fs::metadata(path).map_or((0, None), |m| (m.len(), m.modified().ok()))
}

/// (Re)loads the transcript for an open overlay and records file len/mtime.
fn load_transcript_into(state: &mut AgentOverlayState) {
    let path = transcript_path(&state.thread_id);
    let (len, mtime) = transcript_file_fingerprint(&path);
    state.file_len = len;
    state.file_mtime = mtime;
    state.lines = read_thread_transcript(&state.thread_id, state.width);
}

/// Timed-tick refresh for the open transcript overlay. Skips reparsing when the
/// file and render width are unchanged. Marks the run ended when its marker
/// disappears, but keeps reading (the final assistant message may still be
/// persisting).
fn refresh_agent_overlay(app: &mut MonitorApp) {
    let width = app.terminal_width.saturating_sub(2) as usize;
    let Some(state) = app.agent_overlay.as_mut() else {
        return;
    };
    if state.unavailable {
        return;
    }
    state.ended = !app
        .active_agents
        .iter()
        .any(|a| a.full_thread_id.as_deref() == Some(state.thread_id.as_str()));

    let path = transcript_path(&state.thread_id);
    let (len, mtime) = transcript_file_fingerprint(&path);
    if len == state.file_len && mtime == state.file_mtime && width == state.width {
        return;
    }
    state.file_len = len;
    state.file_mtime = mtime;
    state.width = width;
    state.lines = read_thread_transcript(&state.thread_id, width);
}

/// Handles a key while the transcript overlay is open.
fn handle_agent_overlay_key(app: &mut MonitorApp, key: KeyCode) {
    let page = agent_overlay_page_size(app);
    let Some(state) = app.agent_overlay.as_mut() else {
        return;
    };
    let max_offset = state.lines.len().saturating_sub(page);
    // `None` means following the newest content; step from the bottom.
    let cur = state.scroll.unwrap_or(max_offset);
    match key {
        KeyCode::Esc | KeyCode::Char('q') => app.agent_overlay = None,
        KeyCode::Char('j') | KeyCode::Down => state.scroll = Some((cur + 1).min(max_offset)),
        KeyCode::Char('k') | KeyCode::Up => state.scroll = Some(cur.saturating_sub(1)),
        KeyCode::PageDown => state.scroll = Some((cur + page).min(max_offset)),
        KeyCode::PageUp => state.scroll = Some(cur.saturating_sub(page)),
        KeyCode::Char('g') | KeyCode::Home => state.scroll = Some(0),
        KeyCode::Char('G') | KeyCode::End => state.scroll = None,
        _ => {}
    }
}

// ============================================================================
// Config model editing (Config tab → model picker overlay)
// ============================================================================

/// Kind of an editable model field, which determines the picker's model source,
/// whether it has a thinking step, and how it is persisted.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ModelFieldKind {
    /// Chat/agent model (`available_models`, has a thinking step).
    Chat,
    /// Speech-to-text model (curated STT list, no thinking).
    Transcription,
    /// Text-to-speech model (curated TTS list, no thinking).
    Speech,
}

/// An editable model row on the Config tab.
pub struct EditableModelField {
    /// Index into `config_lines` of the row.
    pub line_index: usize,
    /// Config path to persist (`model`, `title_model`, `transcription.model`…).
    pub path: String,
    /// Field kind.
    pub kind: ModelFieldKind,
}

/// Editable model rows on the Config tab, resolved with section context so the
/// (section, key) pair maps to the right config path and kind.
pub(crate) fn editable_model_fields(lines: &[ConfigLine]) -> Vec<EditableModelField> {
    let mut out = Vec::new();
    let mut section = String::new();
    for (i, cl) in lines.iter().enumerate() {
        match cl {
            ConfigLine::Section(name) => section.clone_from(name),
            ConfigLine::Row(key, _) => {
                let mapped = match (section.as_str(), key.as_str()) {
                    (
                        "core",
                        "model" | "title_model" | "tldr_model" | "handoff_model"
                        | "read_thread_model",
                    ) => Some((key.clone(), ModelFieldKind::Chat)),
                    ("transcription", "model") => {
                        Some(("transcription.model".to_string(), ModelFieldKind::Transcription))
                    }
                    ("speech", "model") => {
                        Some(("speech.model".to_string(), ModelFieldKind::Speech))
                    }
                    _ => None,
                };
                if let Some((path, kind)) = mapped {
                    out.push(EditableModelField {
                        line_index: i,
                        path,
                        kind,
                    });
                }
            }
            ConfigLine::Separator => {}
        }
    }
    out
}

/// Rendered row offset of `config_lines[target]`, mirroring `render_config`
/// (a blank spacer precedes every section except the first).
fn config_line_render_row(lines: &[ConfigLine], target: usize) -> usize {
    let mut row = 0usize;
    let mut is_first = true;
    for (i, cl) in lines.iter().enumerate() {
        if i == target {
            return row;
        }
        match cl {
            ConfigLine::Section(_) => {
                if !is_first {
                    row += 1;
                }
                row += 1;
                is_first = false;
            }
            ConfigLine::Separator | ConfigLine::Row(..) => row += 1,
        }
    }
    row
}

/// Scrolls the Config panel so the selected editable row stays visible.
fn ensure_config_selection_visible(app: &mut MonitorApp) {
    let fields = editable_model_fields(&app.config_lines);
    let Some(field) = fields.get(app.config_selected) else {
        return;
    };
    let render_row = config_line_render_row(&app.config_lines, field.line_index);
    let page = config_page_size(app);
    if render_row < app.config_scroll {
        app.config_scroll = render_row;
    } else if render_row >= app.config_scroll + page {
        app.config_scroll = render_row + 1 - page;
    }
}

/// Moves the Config model-row selection and keeps it visible.
fn move_config_selection(app: &mut MonitorApp, forward: bool) {
    let count = editable_model_fields(&app.config_lines).len();
    if count == 0 {
        return;
    }
    app.config_selected = if forward {
        (app.config_selected + 1).min(count - 1)
    } else {
        app.config_selected.saturating_sub(1)
    };
    ensure_config_selection_visible(app);
}

/// Opens the model picker for the currently selected Config model row.
fn open_model_picker(app: &mut MonitorApp) {
    let fields = editable_model_fields(&app.config_lines);
    let Some(field) = fields.get(app.config_selected) else {
        return;
    };
    let (path, kind, line_index) = (field.path.clone(), field.kind, field.line_index);
    let current = match &app.config_lines[line_index] {
        // Ignore the `(unset)`/`(empty)` display placeholders.
        ConfigLine::Row(_, v) if matches!(v.as_str(), "(unset)" | "(empty)") => String::new(),
        ConfigLine::Row(_, v) => v.clone(),
        _ => String::new(),
    };
    app.model_picker = Some(ModelPickerState::new(path, kind, &current));
}

/// Reloads config lines from disk after an edit, clamping selection.
fn reload_config_lines(app: &mut MonitorApp) {
    let Ok(cfg) = config::Config::load() else {
        app.set_status("Failed to reload config");
        return;
    };
    app.default_model.clone_from(&cfg.model);
    app.config_lines = build_config_lines(&cfg);
    app.config_line_count = rendered_line_count(&app.config_lines);
    let count = editable_model_fields(&app.config_lines).len();
    if app.config_selected >= count {
        app.config_selected = count.saturating_sub(1);
    }
}

/// Which step of the model+thinking picker is active.
#[derive(PartialEq, Eq)]
pub enum PickerPhase {
    Model,
    Thinking,
}

/// Two-step picker for editing a Config model field. Chat models pick a model
/// then a thinking level; audio (STT/TTS) models pick a model only and commit.
pub struct ModelPickerState {
    /// Config path being edited (e.g. `title_model`, `transcription.model`).
    pub field: String,
    /// Field kind (drives model source, thinking step, and persistence).
    pub kind: ModelFieldKind,
    /// Active step.
    pub phase: PickerPhase,
    /// Typed filter text (model step).
    pub filter: String,
    /// All selectable model ids (`provider:id`), sorted.
    pub items: Vec<String>,
    /// Indices into `items` matching the current filter.
    pub matches: Vec<usize>,
    /// Index into `matches` of the highlighted row.
    pub selected: usize,
    /// Model chosen in step 1 (used by the thinking step).
    pub chosen_model: String,
    /// Thinking level the field had when the picker opened.
    pub thinking_current: config::ThinkingLevel,
    /// Index into `config::ThinkingLevel::all()` of the highlighted level.
    pub thinking_selected: usize,
}

impl ModelPickerState {
    fn new(field: String, kind: ModelFieldKind, current: &str) -> Self {
        let (model_part, thinking_part) = zdx_engine::models::split_model_thinking(current);
        let thinking_current = thinking_part.unwrap_or(config::ThinkingLevel::Low);
        let thinking_selected = config::ThinkingLevel::all()
            .iter()
            .position(|l| *l == thinking_current)
            .unwrap_or(0);

        let mut items: Vec<String> = match kind {
            ModelFieldKind::Chat => available_models()
                .iter()
                .map(|m| format!("{}:{}", m.provider, m.id))
                .collect(),
            ModelFieldKind::Transcription => {
                zdx_engine::audio::transcribe::transcription_model_options()
            }
            ModelFieldKind::Speech => zdx_engine::audio::speak::speech_model_options(),
        };
        items.sort();
        items.dedup();

        let mut state = Self {
            field,
            kind,
            phase: PickerPhase::Model,
            filter: String::new(),
            items,
            matches: Vec::new(),
            selected: 0,
            chosen_model: model_part.to_string(),
            thinking_current,
            thinking_selected,
        };
        state.recompute();
        // Preselect the current value (exact `provider:id` or bare `id`).
        if let Some(pos) = state.matches.iter().position(|&i| {
            let item = &state.items[i];
            item.as_str() == model_part || item.rsplit(':').next() == Some(model_part)
        }) {
            state.selected = pos;
        }
        state
    }

    /// Whether this field has a thinking step (chat models only).
    fn has_thinking(&self) -> bool {
        self.kind == ModelFieldKind::Chat
    }

    fn recompute(&mut self) {
        let needle = self.filter.to_lowercase();
        self.matches = self
            .items
            .iter()
            .enumerate()
            .filter(|(_, it)| needle.is_empty() || it.to_lowercase().contains(&needle))
            .map(|(i, _)| i)
            .collect();
        if self.selected >= self.matches.len() {
            self.selected = self.matches.len().saturating_sub(1);
        }
    }

    /// The `provider:id` of the highlighted model row, if any.
    pub fn selected_model(&self) -> Option<&str> {
        self.matches
            .get(self.selected)
            .map(|&i| self.items[i].as_str())
    }

    /// The highlighted thinking level.
    pub fn selected_thinking(&self) -> config::ThinkingLevel {
        config::ThinkingLevel::all()
            .get(self.thinking_selected)
            .copied()
            .unwrap_or(config::ThinkingLevel::Low)
    }
}

/// Persists the picker's chosen model (+ thinking for chat models).
fn commit_model_picker(app: &mut MonitorApp) {
    let Some(picker) = app.model_picker.as_ref() else {
        return;
    };
    let field = picker.field.clone();
    let model = picker.chosen_model.clone();
    let level = picker.selected_thinking();

    let (result, shown) = match picker.kind {
        // Main model: separate `model` + `thinking_level` fields.
        ModelFieldKind::Chat if field == "model" => (
            config::Config::save_model_field("model", &model)
                .and_then(|()| config::Config::save_thinking_level(level)),
            zdx_engine::models::format_model_thinking(&model, level),
        ),
        // Role chat models: thinking carried inline as `model@thinking`.
        ModelFieldKind::Chat => {
            let combined = zdx_engine::models::format_model_thinking(&model, level);
            (
                config::Config::save_model_field(&field, &combined),
                combined,
            )
        }
        // Audio (STT/TTS): model only, no thinking.
        ModelFieldKind::Transcription | ModelFieldKind::Speech => {
            (config::Config::save_model_field(&field, &model), model.clone())
        }
    };

    match result {
        Ok(()) => {
            app.model_picker = None;
            reload_config_lines(app);
            app.set_status(format!("Set {field} = {shown}"));
        }
        Err(e) => app.set_status(format!("Failed to set {field}: {e}")),
    }
}

/// Handles a key while the model picker overlay is open.
fn handle_model_picker_key(app: &mut MonitorApp, key: KeyCode) {
    let Some(picker) = app.model_picker.as_mut() else {
        return;
    };
    match picker.phase {
        PickerPhase::Model => match key {
            KeyCode::Esc => app.model_picker = None,
            KeyCode::Up => picker.selected = picker.selected.saturating_sub(1),
            KeyCode::Down => {
                let last = picker.matches.len().saturating_sub(1);
                picker.selected = (picker.selected + 1).min(last);
            }
            KeyCode::Backspace => {
                picker.filter.pop();
                picker.recompute();
            }
            KeyCode::Char(c) => {
                picker.filter.push(c);
                picker.recompute();
            }
            KeyCode::Enter => {
                if let Some(model) = picker.selected_model().map(str::to_string) {
                    picker.chosen_model = model;
                    if picker.has_thinking() {
                        picker.phase = PickerPhase::Thinking;
                    } else {
                        commit_model_picker(app);
                    }
                }
            }
            _ => {}
        },
        PickerPhase::Thinking => match key {
            KeyCode::Esc => picker.phase = PickerPhase::Model,
            KeyCode::Up => picker.thinking_selected = picker.thinking_selected.saturating_sub(1),
            KeyCode::Down => {
                let last = config::ThinkingLevel::all().len().saturating_sub(1);
                picker.thinking_selected = (picker.thinking_selected + 1).min(last);
            }
            KeyCode::Enter => commit_model_picker(app),
            _ => {}
        },
    }
}

#[cfg(test)]
mod transcript_tests {
    use zdx_engine::core::thread_persistence::ThreadEvent;

    use super::*;

    fn parse(lines: &[&str]) -> Vec<ThreadEvent> {
        lines
            .iter()
            .filter_map(|l| serde_json::from_str::<ThreadEvent>(l).ok())
            .collect()
    }

    fn line_text(line: &Line) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    fn render(events: &[ThreadEvent], width: usize) -> Vec<String> {
        let cells = zdx_transcript::build_transcript_from_events(events);
        zdx_transcript::cells_to_lines(&cells, width)
            .iter()
            .map(line_text)
            .collect()
    }

    #[test]
    fn renders_formatted_transcript_with_paired_tools_and_notice() {
        let events = parse(&[
            r#"{"type":"meta","schema_version":1,"ts":"t"}"#,
            r#"{"type":"message","role":"user","text":"hello there","ts":"t"}"#,
            r#"{"type":"message","role":"assistant","text":"**bold** answer","ts":"t"}"#,
            r#"{"type":"tool_use","id":"t1","name":"grep","input":{"pattern":"foo"},"ts":"t"}"#,
            r#"{"type":"tool_result","tool_use_id":"t1","output":"match","ok":true,"ts":"t"}"#,
            r#"{"type":"notice","kind":"refusal","message":"heads up","ts":"t"}"#,
            r#"{"type":"usage","input_tokens":1,"output_tokens":2,"cache_read_tokens":0,"cache_write_tokens":0,"ts":"t"}"#,
            r"{ malformed line",
        ]);
        let lines = render(&events, 80);
        let joined = lines.join("\n");

        assert!(
            joined.contains("hello there"),
            "user text present: {joined}"
        );
        assert!(
            joined.contains("answer"),
            "assistant text present: {joined}"
        );
        assert!(joined.contains("grep"), "tool name present: {joined}");
        assert!(joined.contains("heads up"), "notice present: {joined}");
        // Markdown bold markers are consumed, not rendered literally.
        assert!(!joined.contains("**bold**"), "markdown parsed: {joined}");
    }

    #[test]
    fn inserts_blank_line_between_cells() {
        let events = parse(&[
            r#"{"type":"message","role":"user","text":"one","ts":"t"}"#,
            r#"{"type":"message","role":"assistant","text":"two","ts":"t"}"#,
        ]);
        let cells = zdx_transcript::build_transcript_from_events(&events);
        let lines = zdx_transcript::cells_to_lines(&cells, 80);
        let blanks = lines.iter().filter(|l| line_text(l).is_empty()).count();
        assert!(
            blanks >= cells.len(),
            "one blank separator per cell: blanks={blanks} cells={}",
            cells.len()
        );
    }

    #[test]
    fn narrower_width_wraps_into_more_lines() {
        let events = parse(&[
            r#"{"type":"message","role":"assistant","text":"the quick brown fox jumps over the lazy dog again and again","ts":"t"}"#,
        ]);
        let cells = zdx_transcript::build_transcript_from_events(&events);
        let wide = zdx_transcript::cells_to_lines(&cells, 100).len();
        let narrow = zdx_transcript::cells_to_lines(&cells, 20).len();
        assert!(narrow > wide, "narrow={narrow} wide={wide}");
    }

    #[test]
    fn editable_fields_resolve_path_and_kind_by_section() {
        let lines = vec![
            ConfigLine::Section("core".into()),
            ConfigLine::Row("model".into(), "x".into()),
            ConfigLine::Row("title_model".into(), "y".into()),
            ConfigLine::Row("verbose".into(), "true".into()),
            ConfigLine::Section("transcription".into()),
            ConfigLine::Row("model".into(), "z".into()),
            ConfigLine::Row("language".into(), "en".into()),
            ConfigLine::Section("speech".into()),
            ConfigLine::Row("model".into(), "w".into()),
        ];
        let fields = editable_model_fields(&lines);
        let got: Vec<(&str, ModelFieldKind)> =
            fields.iter().map(|f| (f.path.as_str(), f.kind)).collect();
        assert_eq!(
            got,
            vec![
                ("model", ModelFieldKind::Chat),
                ("title_model", ModelFieldKind::Chat),
                ("transcription.model", ModelFieldKind::Transcription),
                ("speech.model", ModelFieldKind::Speech),
            ]
        );
    }

    #[test]
    fn model_picker_filters_and_reports_selection() {
        let mut p = ModelPickerState::new(
            "title_model".to_string(),
            ModelFieldKind::Chat,
            "no-such-model",
        );
        assert!(!p.items.is_empty(), "registry should list models");
        let before = p.matches.len();
        p.filter.push_str("claude");
        p.recompute();
        assert!(p.matches.len() <= before);
        assert!(
            p.matches
                .iter()
                .all(|&i| p.items[i].to_lowercase().contains("claude"))
        );
        if let Some(sel) = p.selected_model() {
            assert!(sel.to_lowercase().contains("claude"));
        }
    }

    #[test]
    fn speech_picker_lists_curated_options_without_thinking() {
        let p = ModelPickerState::new("speech.model".to_string(), ModelFieldKind::Speech, "");
        assert!(!p.has_thinking());
        assert!(p.items.iter().all(|o| o.contains(':')));
        assert!(p.items.iter().any(|o| o.starts_with("mistral:")));
    }

    #[test]
    fn model_picker_parses_inline_thinking_suffix() {
        let p = ModelPickerState::new(
            "title_model".to_string(),
            ModelFieldKind::Chat,
            "gemini:some-model@high",
        );
        assert_eq!(p.thinking_current, config::ThinkingLevel::High);
        assert_eq!(p.chosen_model, "gemini:some-model");

        // No suffix defaults to Low.
        let p2 = ModelPickerState::new(
            "tldr_model".to_string(),
            ModelFieldKind::Chat,
            "gemini:some-model",
        );
        assert_eq!(p2.thinking_current, config::ThinkingLevel::Low);
    }
}
