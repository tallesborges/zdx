use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};
use std::{fs, io};

use anyhow::{Context, Result};
use crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEventKind, MouseEventKind,
};
use crossterm::execute;
use crossterm::terminal::{self, EnterAlternateScreen, LeaveAlternateScreen};
use ratatui::prelude::*;
use serde_json::Value;
use zdx_engine::config::{self, paths};
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
        match val {
            Value::Object(nested) => {
                if !first_group {
                    lines.push(ConfigLine::Separator);
                }
                first_group = false;
                let mut rows = Vec::new();
                flatten_object(nested, key, &mut rows);
                for (k, v) in rows {
                    lines.push(ConfigLine::Row(k, v));
                }
            }
            _ => {
                let display = if is_sensitive(key) && !matches!(val, Value::Null) {
                    "***".to_string()
                } else {
                    format_json_scalar(val)
                };
                lines.push(ConfigLine::Row(key.clone(), display));
            }
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
    pub active_section: Section,
    pub selected_index: usize,
    pub status_section: Section,
    pub status_message: String,
    pub should_quit: bool,
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
    pub uptime: String,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Section {
    Services,
    ActiveAgents,
    Config,
    Threads,
    Automations,
}

impl Section {
    pub const ALL: [Section; 5] = [
        Section::Services,
        Section::ActiveAgents,
        Section::Config,
        Section::Threads,
        Section::Automations,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Section::Services => "Services",
            Section::ActiveAgents => "Active Agents",
            Section::Config => "Config",
            Section::Threads => "Threads",
            Section::Automations => "Automations",
        }
    }

    fn next(self) -> Self {
        match self {
            Section::Services => Section::ActiveAgents,
            Section::ActiveAgents => Section::Config,
            Section::Config => Section::Threads,
            Section::Threads => Section::Automations,
            Section::Automations => Section::Services,
        }
    }
}

impl MonitorApp {
    fn item_count(&self) -> usize {
        match self.active_section {
            Section::Services => self.services.len(),
            Section::Config => 0,
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

/// Number of lines render_config will produce for these lines
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
    app.config_line_count
        .saturating_sub(config_page_size(app))
}

fn build_app(root: &Path) -> Result<MonitorApp> {
    let root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    let config = config::Config::load().context("load config")?;
    let config_lines = build_config_lines(&config);
    let config_line_count = rendered_line_count(&config_lines);

    Ok(MonitorApp {
        config_lines,
        config_line_count,
        config_scroll: 0,
        terminal_height: 24,
        root: root.clone(),
        threads: load_threads(),
        automations: load_automations(&root),
        services: load_services(),
        active_agents: load_active_agents(),
        active_section: Section::Services,
        selected_index: 0,
        status_section: Section::Services,
        status_message: String::new(),
        should_quit: false,
    })
}

fn setup_terminal() -> Result<Terminal<CrosstermBackend<io::Stdout>>> {
    terminal::enable_raw_mode().context("enable raw mode")?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)
        .context("enter alternate screen")?;
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

fn handle_key_event(app: &mut MonitorApp, key: KeyCode) {
    match key {
        KeyCode::Char('q') => app.should_quit = true,
        KeyCode::Tab => {
            app.active_section = app.active_section.next();
            app.selected_index = 0;
            app.config_scroll = 0;
        }
        KeyCode::Char('j') | KeyCode::Down => {
            if app.active_section == Section::Config {
                let max = config_max_scroll(app);
                app.config_scroll = app.config_scroll.saturating_add(1).min(max);
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
            } else if app.selected_index > 0 {
                app.selected_index -= 1;
            }
        }
        KeyCode::PageDown => {
            if app.active_section == Section::Config {
                let page = config_page_size(app);
                let max = config_max_scroll(app);
                app.config_scroll = app.config_scroll.saturating_add(page).min(max);
            }
        }
        KeyCode::PageUp => {
            if app.active_section == Section::Config {
                let page = config_page_size(app);
                app.config_scroll = app.config_scroll.saturating_sub(page);
            }
        }
        KeyCode::Char('y') => copy_selected_thread_id(app),
        KeyCode::Enter => toggle_selected_service(app),
        KeyCode::Char('r') => restart_selected_service(app),
        _ => {}
    }
}

fn handle_mouse_event(app: &mut MonitorApp, kind: MouseEventKind) {
    match kind {
        MouseEventKind::ScrollDown => match app.active_section {
            Section::Config => {
                let max = config_max_scroll(app);
                app.config_scroll = app.config_scroll.saturating_add(1).min(max);
            }
            _ => {
                let count = app.item_count();
                if count > 0 {
                    app.selected_index = (app.selected_index + 1).min(count - 1);
                }
            }
        },
        MouseEventKind::ScrollUp => match app.active_section {
            Section::Config => {
                app.config_scroll = app.config_scroll.saturating_sub(1);
            }
            _ => {
                app.selected_index = app.selected_index.saturating_sub(1);
            }
        },
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

fn refresh_app(app: &mut MonitorApp) {
    app.services = load_services();
    app.active_agents = load_active_agents();
    app.clamp_selection();
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
        app.terminal_height = terminal.size().map(|r| r.height).unwrap_or(24);

        let timeout = tick_rate.saturating_sub(last_tick.elapsed());
        if event::poll(timeout).context("poll events")? {
            match event::read().context("read event")? {
                Event::Key(key) if key.kind == KeyEventKind::Press => {
                    handle_key_event(&mut app, key.code);
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
    let mut service_names: BTreeSet<String> = BTreeSet::from(["daemon".to_string()]);
    if let Ok(bots) = config::BotsConfig::load() {
        for name in bots.bots.keys() {
            service_names.insert(config::named_bot_service_name(name));
        }
    }
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
            if stem == config::LEGACY_BOT_SERVICE_NAME
                || config::parse_named_bot_service_name(stem).is_some()
            {
                service_names.insert(stem.to_string());
            }
        }
    }

    service_names
        .into_iter()
        .filter_map(|service_name| {
            let display_name = if service_name == config::LEGACY_BOT_SERVICE_NAME {
                config::LEGACY_BOT_SERVICE_NAME.to_string()
            } else if let Some(name) = config::parse_named_bot_service_name(&service_name) {
                config::named_bot_service_display_name(name)
            } else {
                service_name.clone()
            };

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
                pidfile::ServiceStatus::Stopped
                    if config::parse_named_bot_service_name(&service_name).is_some() =>
                {
                    Some(ServiceInfo {
                        key: service_name.clone(),
                        name: display_name,
                        status: "stopped".to_string(),
                        details: String::new(),
                    })
                }
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
        key if config::parse_named_bot_service_name(key).is_some() => {
            let name = config::parse_named_bot_service_name(key).unwrap_or_default();
            command.arg("bot").arg("--bot").arg(name);
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
                uptime: agent_activity::uptime_since(&r.started_at),
            }
        })
        .collect()
}
