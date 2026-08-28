use std::path::Path;

use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, List, ListItem};
use zdx_engine::automations;

use crate::app::MonitorApp;
use crate::ui::SELECTED_BG;

pub struct AutomationInfo {
    pub name: String,
    pub schedule: Option<String>,
}

pub(crate) fn load_automations(root: &Path) -> Vec<AutomationInfo> {
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

pub(crate) fn render_automations(f: &mut Frame, app: &MonitorApp, area: Rect) {
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
