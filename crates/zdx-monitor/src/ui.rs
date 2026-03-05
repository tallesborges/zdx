use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, List, ListItem, Paragraph, Tabs};

use crate::app::{MonitorApp, Section};

pub fn render(f: &mut Frame, app: &MonitorApp) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(3), Constraint::Min(0)])
        .split(f.area());

    render_tabs(f, app, chunks[0]);

    match app.active_section {
        Section::Services => render_services(f, app, chunks[1]),
        Section::Config => render_config(f, app, chunks[1]),
        Section::Threads => render_threads(f, app, chunks[1]),
        Section::Automations => render_automations(f, app, chunks[1]),
    }
}

fn render_tabs(f: &mut Frame, app: &MonitorApp, area: Rect) {
    let titles: Vec<&str> = Section::ALL.iter().map(|s| s.label()).collect();
    let selected = Section::ALL
        .iter()
        .position(|s| *s == app.active_section)
        .unwrap_or(0);
    let tabs = Tabs::new(titles)
        .block(Block::default().borders(Borders::ALL).title("ZDX Monitor"))
        .select(selected)
        .highlight_style(Style::default().add_modifier(Modifier::BOLD | Modifier::UNDERLINED));
    f.render_widget(tabs, area);
}

fn render_services(f: &mut Frame, app: &MonitorApp, area: Rect) {
    let items: Vec<ListItem> = app
        .services
        .iter()
        .map(|s| {
            let (icon, style) = if s.status == "running" {
                ("●", Style::default().fg(Color::Green))
            } else {
                ("○", Style::default().fg(Color::DarkGray))
            };
            let line = if s.details.is_empty() {
                format!(" {:<10} {icon} {}", s.name, s.status)
            } else {
                format!(" {:<10} {icon} {:<10} {}", s.name, s.status, s.details)
            };
            ListItem::new(line).style(style)
        })
        .collect();
    let list = List::new(items).block(Block::default().borders(Borders::ALL).title("Services"));
    f.render_widget(list, area);
}

fn render_config(f: &mut Frame, app: &MonitorApp, area: Rect) {
    let home = zdx_core::config::paths::zdx_home();
    let threads = zdx_core::config::paths::threads_dir();
    let text = format!(
        " Model:       {}\n Thinking:    {:?}\n ZDX Home:    {}\n Threads dir: {}",
        app.config.model,
        app.config.thinking_level,
        home.display(),
        threads.display(),
    );
    let p = Paragraph::new(text).block(Block::default().borders(Borders::ALL).title("Config"));
    f.render_widget(p, area);
}

fn render_threads(f: &mut Frame, app: &MonitorApp, area: Rect) {
    let items: Vec<ListItem> = app
        .threads
        .iter()
        .enumerate()
        .map(|(i, t)| {
            let short_id = if t.id.len() > 8 { &t.id[..8] } else { &t.id };
            let surface = t.surface.as_deref().unwrap_or("-");
            let title = t.title.as_deref().unwrap_or("(untitled)");
            let line = format!(" [{short_id}] {} | {surface:<9} | {title}", t.modified);
            let style = if i == app.selected_index {
                Style::default().add_modifier(Modifier::REVERSED)
            } else {
                Style::default()
            };
            ListItem::new(line).style(style)
        })
        .collect();
    let list = List::new(items).block(
        Block::default()
            .borders(Borders::ALL)
            .title("Threads (y=copy ID)"),
    );
    f.render_widget(list, area);
}

fn render_automations(f: &mut Frame, app: &MonitorApp, area: Rect) {
    let items: Vec<ListItem> = app
        .automations
        .iter()
        .enumerate()
        .map(|(i, a)| {
            let sched = a.schedule.as_deref().unwrap_or("-");
            let line = format!(" {:<20} | {sched}", a.name);
            let style = if i == app.selected_index {
                Style::default().add_modifier(Modifier::REVERSED)
            } else {
                Style::default()
            };
            ListItem::new(line).style(style)
        })
        .collect();
    let list = List::new(items).block(Block::default().borders(Borders::ALL).title("Automations"));
    f.render_widget(list, area);
}
