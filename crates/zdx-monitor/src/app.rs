use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};
use std::{fs, io};

use anyhow::{Context, Result};
use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use crossterm::execute;
use crossterm::terminal::{self, EnterAlternateScreen, LeaveAlternateScreen};
use ratatui::prelude::*;
use serde_json::Value;
use zdx_core::config::{self, paths};
use zdx_core::{agent_activity, automations, pidfile};

use crate::ui;

pub struct MonitorApp {
    pub config: config::Config,
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

/// Run the monitor dashboard.
///
/// # Errors
///
/// Returns an error if configuration cannot be loaded or terminal operations fail.
pub fn run(root: &Path) -> Result<()> {
    let root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    let config = config::Config::load().context("load config")?;
    let threads = load_threads();
    let automations = load_automations(&root);
    let services = load_services();
    let active_agents = load_active_agents();

    let mut app = MonitorApp {
        config,
        root,
        threads,
        automations,
        services,
        active_agents,
        active_section: Section::Services,
        selected_index: 0,
        status_section: Section::Services,
        status_message: String::new(),
        should_quit: false,
    };

    // Setup terminal
    terminal::enable_raw_mode().context("enable raw mode")?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen).context("enter alternate screen")?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend).context("create terminal")?;

    let tick_rate = Duration::from_secs(1);
    let mut last_tick = Instant::now();

    loop {
        terminal.draw(|f| ui::render(f, &app))?;

        let timeout = tick_rate.saturating_sub(last_tick.elapsed());
        if event::poll(timeout).context("poll events")?
            && let Event::Key(key) = event::read().context("read event")?
            && key.kind == KeyEventKind::Press
        {
            match key.code {
                KeyCode::Char('q') => app.should_quit = true,
                KeyCode::Tab => {
                    app.active_section = app.active_section.next();
                    app.selected_index = 0;
                }
                KeyCode::Char('j') | KeyCode::Down => {
                    let count = app.item_count();
                    if count > 0 {
                        app.selected_index = (app.selected_index + 1).min(count - 1);
                    }
                }
                KeyCode::Char('k') | KeyCode::Up => {
                    if app.selected_index > 0 {
                        app.selected_index -= 1;
                    }
                }
                KeyCode::Char('y') => {
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
                KeyCode::Enter => {
                    if app.active_section == Section::Services
                        && let Some(service) = app.services.get(app.selected_index)
                    {
                        match toggle_service(service, &app.root) {
                            Ok(message) => app.set_status(message),
                            Err(err) => {
                                app.set_status(format!("Failed to toggle {}: {err}", service.name))
                            }
                        }
                    }
                }
                KeyCode::Char('r') => {
                    if app.active_section == Section::Services
                        && let Some(service) = app.services.get(app.selected_index)
                    {
                        match restart_service(service, &app.root) {
                            Ok(message) => app.set_status(message),
                            Err(err) => {
                                app.set_status(format!("Failed to restart {}: {err}", service.name))
                            }
                        }
                    }
                }
                _ => {}
            }
        }

        if last_tick.elapsed() >= tick_rate {
            app.services = load_services();
            app.active_agents = load_active_agents();
            app.clamp_selection();
            last_tick = Instant::now();
        }

        if app.should_quit {
            break;
        }
    }

    // Restore terminal
    terminal::disable_raw_mode().context("disable raw mode")?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen).context("leave alternate screen")?;
    terminal.show_cursor().context("show cursor")?;

    Ok(())
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
        Some(pid) => {
            wait_for_service_state(&service.key, false, Duration::from_secs(3));
            Ok(format!("Stopped {} (PID {})", service.name, pid))
        }
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

fn wait_for_service_state(name: &str, should_be_running: bool, timeout: Duration) {
    let start = Instant::now();
    while start.elapsed() < timeout {
        let running = matches!(
            pidfile::status(name),
            pidfile::ServiceStatus::Running { .. }
        );
        if running == should_be_running {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
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
