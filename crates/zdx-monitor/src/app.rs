use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read, Seek};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};
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
use zdx_engine::core::usage_stats::{self, UsageStats};
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
    pub terminal_height: u16,
    pub root: PathBuf,
    pub threads: Vec<ThreadInfo>,
    pub automations: Vec<AutomationInfo>,
    pub services: Vec<ServiceInfo>,
    pub active_agents: Vec<ActiveAgentInfo>,
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
}

/// A cached snapshot of the usage aggregation plus when it was computed.
pub struct CachedUsageStats {
    pub stats: UsageStats,
    pub computed_at: Instant,
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
    pub model: String,
    pub uptime: String,
    pub kind: Option<String>,
    pub subagent_name: Option<String>,
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
                    app.usage_line_count = ui::usage_line_count(&cached);
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
        terminal_height: 24,
        root: root.clone(),
        threads: load_threads(),
        automations: load_automations(&root),
        services,
        active_agents: load_active_agents(),
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
                let max = config_max_scroll(app);
                app.config_scroll = app.config_scroll.saturating_add(1).min(max);
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
                app.config_scroll = app.config_scroll.saturating_sub(1);
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
            app.set_status("Refreshing usage stats…");
        }
        KeyCode::Char('y') => copy_selected_thread_id(app),
        KeyCode::Char('r') => restart_selected_service(app),
        KeyCode::Enter => toggle_selected_service(app),
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

        let timeout = tick_rate.saturating_sub(last_tick.elapsed());
        if event::poll(timeout).context("poll events")? {
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
        }

        if last_tick.elapsed() >= tick_rate {
            refresh_app(&mut app);
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
        .map(|entry| {
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

            let (title, surface) = read_thread_meta(&path);

            ThreadInfo {
                id,
                title,
                surface,
                modified,
            }
        })
        .collect()
}

fn read_thread_meta(path: &Path) -> (Option<String>, Option<String>) {
    use std::io::BufRead;
    let Ok(file) = fs::File::open(path) else {
        return (None, None);
    };
    let reader = io::BufReader::new(file);
    let Some(Ok(line)) = reader.lines().next() else {
        return (None, None);
    };
    let first_line = line;
    let v: Value = match serde_json::from_str(&first_line) {
        Ok(v) => v,
        Err(_) => return (None, None),
    };
    let title = v.get("title").and_then(Value::as_str).map(String::from);
    let surface = v.get("surface").and_then(Value::as_str).map(String::from);
    (title, surface)
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
                model: r.model.unwrap_or_else(|| "-".to_string()),
                uptime: agent_activity::uptime_since(&r.started_at),
                kind: r.kind,
                subagent_name: r.subagent_name,
            }
        })
        .collect()
}
