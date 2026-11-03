use std::sync::mpsc;
use std::time::Instant;

use anyhow::{Result, anyhow};
use crossterm::event::KeyCode;
use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, List, ListItem};
use zdx_engine::service::{self, Service};

use crate::app::{MonitorApp, Section};
use crate::ui::SELECTED_BG;

#[derive(Clone)]
pub struct ServiceInfo {
    pub service: Service,
    pub name: String,
    pub status: String,
    pub details: String,
}

pub(crate) fn load_services() -> Vec<ServiceInfo> {
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

pub(crate) fn toggle_selected_service(app: &mut MonitorApp) {
    if app.active_section == Section::Services
        && let Some(job) = &app.restart_job
    {
        let message = restart_in_progress_message(job);
        app.set_status(message);
        return;
    }
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

/// An in-flight background restart. `service::restart` waits for launchd to
/// spawn the replacement process, which launchd's throttle can delay by up to
/// ~10s, so it never runs on the UI thread.
pub struct RestartJob {
    pub service: Service,
    rx: mpsc::Receiver<Result<String>>,
}

fn restart_in_progress_message(job: &RestartJob) -> String {
    format!("Restart of {} already in progress", job.service.name())
}

pub(crate) fn restart_selected_service(app: &mut MonitorApp, force: bool) {
    if app.active_section != Section::Services {
        return;
    }
    if let Some(job) = &app.restart_job {
        let message = restart_in_progress_message(job);
        app.set_status(message);
        return;
    }
    let Some(info) = app.services.get(app.selected_index) else {
        return;
    };
    let svc = info.service;
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(service::restart(svc, force));
    });
    app.restart_job = Some(RestartJob { service: svc, rx });
    app.set_status(format!("Restarting {}…", svc.name()));
}

/// Collects a finished background restart, if any. Non-blocking.
pub(crate) fn poll_restart_result(app: &mut MonitorApp) {
    let Some(job) = &app.restart_job else {
        return;
    };
    let name = job.service.name();
    let result = match job.rx.try_recv() {
        Ok(result) => result,
        Err(mpsc::TryRecvError::Empty) => return,
        Err(mpsc::TryRecvError::Disconnected) => {
            Err(anyhow!("restart worker exited without a result"))
        }
    };
    app.restart_job = None;
    let message = match result {
        Ok(message) => message,
        Err(err) => restart_error_message(name, &err),
    };
    app.status_section = Section::Services;
    app.status_message = message;
    app.status_set_at = Some(Instant::now());
}

fn restart_error_message(name: &str, err: &anyhow::Error) -> String {
    if let Some(blocked) = err.downcast_ref::<service::RestartBlocked>() {
        let active_runs = blocked.active_runs();
        let suffix = if active_runs == 1 { "" } else { "s" };
        format!("Restart blocked: {active_runs} active agent run{suffix}; wait or press R to force")
    } else {
        format!("Failed to restart {name}: {err}")
    }
}

pub(crate) fn restart_force_for_key(key: KeyCode) -> bool {
    key == KeyCode::Char('R')
}

pub(crate) fn render_services(f: &mut Frame, app: &MonitorApp, area: Rect) {
    let name_width = Service::name_width();
    let items: Vec<ListItem> = app
        .services
        .iter()
        .enumerate()
        .map(|(i, s)| {
            let restarting = app
                .restart_job
                .as_ref()
                .is_some_and(|job| job.service == s.service);
            let (icon, status, style) = if restarting {
                ("◐", "restarting", Style::default().fg(Color::Yellow))
            } else if s.status == "running" {
                ("●", s.status.as_str(), Style::default().fg(Color::Green))
            } else {
                ("○", s.status.as_str(), Style::default().fg(Color::DarkGray))
            };
            let line = {
                let display_details = &s.details;
                if display_details.is_empty() {
                    format!(" {:<name_width$} {icon} {status}", s.name)
                } else {
                    format!(
                        " {:<name_width$} {icon} {status:<10} {display_details}",
                        s.name
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn restart_keys_distinguish_guarded_and_forced_modes() {
        assert!(!restart_force_for_key(KeyCode::Char('r')));
        assert!(restart_force_for_key(KeyCode::Char('R')));
    }
}
