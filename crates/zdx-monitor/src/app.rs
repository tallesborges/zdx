use std::path::Path;
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
    pub threads: Vec<ThreadInfo>,
    pub automations: Vec<AutomationInfo>,
    pub services: Vec<ServiceInfo>,
    pub active_agents: Vec<ActiveAgentInfo>,
    pub active_section: Section,
    pub selected_index: usize,
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
            Section::Services | Section::Config => 0,
            Section::ActiveAgents => self.active_agents.len(),
            Section::Threads => self.threads.len(),
            Section::Automations => self.automations.len(),
        }
    }
}

/// Run the monitor dashboard.
///
/// # Errors
///
/// Returns an error if configuration cannot be loaded or terminal operations fail.
pub fn run(root: &Path) -> Result<()> {
    let config = config::Config::load().context("load config")?;
    let threads = load_threads();
    let automations = load_automations(root);
    let services = load_services();
    let active_agents = load_active_agents();

    let mut app = MonitorApp {
        config,
        threads,
        automations,
        services,
        active_agents,
        active_section: Section::Services,
        selected_index: 0,
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
                    }
                }
                _ => {}
            }
        }

        if last_tick.elapsed() >= tick_rate {
            app.services = load_services();
            app.active_agents = load_active_agents();
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
    let services = ["bot", "daemon"];
    services
        .iter()
        .map(|name| match pidfile::status(name) {
            pidfile::ServiceStatus::Running { pid, started } => {
                let uptime = started
                    .and_then(|s| s.elapsed().ok())
                    .map(format_duration)
                    .unwrap_or_default();
                ServiceInfo {
                    name: (*name).to_string(),
                    status: "running".to_string(),
                    details: format!("PID {pid} | up {uptime}"),
                }
            }
            pidfile::ServiceStatus::Stopped => ServiceInfo {
                name: (*name).to_string(),
                status: "stopped".to_string(),
                details: String::new(),
            },
        })
        .collect()
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
