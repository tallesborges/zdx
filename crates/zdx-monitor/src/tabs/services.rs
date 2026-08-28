use anyhow::Result;
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

pub(crate) fn restart_selected_service(app: &mut MonitorApp, force: bool) {
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

pub(crate) fn restart_force_for_key(key: KeyCode) -> bool {
    key == KeyCode::Char('R')
}

pub(crate) fn render_services(f: &mut Frame, app: &MonitorApp, area: Rect) {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn restart_keys_distinguish_guarded_and_forced_modes() {
        assert!(!restart_force_for_key(KeyCode::Char('r')));
        assert!(restart_force_for_key(KeyCode::Char('R')));
    }
}
