use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, Paragraph, Tabs};

use crate::app::{MonitorApp, Section, TargetPickerState};
use crate::tabs::agents::{render_active_agents, render_agent_overlay};
use crate::tabs::background::{render_background, render_background_detail};
use crate::tabs::config::{
    config_footer_hint, render_config, render_model_picker, render_pending_write,
};
use crate::tabs::jobs::render_jobs;
use crate::tabs::logs::{render_log_overlay, render_logs};
use crate::tabs::services::render_services;
use crate::tabs::threads::{render_threads, render_timing_overlay};
use crate::tabs::usage::render_usage;

pub fn render(f: &mut Frame, app: &MonitorApp) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(0),
            Constraint::Length(3),
        ])
        .split(f.area());

    render_tabs(f, app, chunks[0]);

    match app.active_section {
        Section::Services => render_services(f, app, chunks[1]),
        Section::ActiveAgents => render_active_agents(f, app, chunks[1]),
        Section::Background => render_background(f, app, chunks[1]),
        Section::Config => render_config(f, app, chunks[1]),
        Section::Threads => render_threads(f, app, chunks[1]),
        Section::Usage => render_usage(f, app, chunks[1]),
        Section::Jobs => render_jobs(f, app, chunks[1]),
        Section::Logs => render_logs(f, app, chunks[1]),
    }

    render_footer(f, app, chunks[2]);

    if app.log_overlay_open && app.active_section == Section::Logs {
        render_log_overlay(f, app, f.area());
    }

    if app.active_section == Section::Logs
        && let Some(picker) = &app.log_target_picker
    {
        render_picker(f, picker, f.area(), "target");
    }

    if app.active_section == Section::Threads
        && let Some(picker) = &app.thread_project_picker
    {
        render_picker(f, picker, f.area(), "project");
    }

    if app.active_section == Section::Config
        && let Some(picker) = &app.config_workspace_picker
    {
        render_picker(f, picker, f.area(), "workspace · (n) local overrides");
    }

    if let Some(state) = &app.agent_overlay {
        render_agent_overlay(f, state, f.area());
    }

    if let Some(state) = &app.timing_overlay {
        render_timing_overlay(f, state, f.area());
    }

    if let Some(state) = &app.background_detail {
        render_background_detail(f, state, f.area());
    }

    if let Some(picker) = &app.model_picker {
        render_model_picker(f, picker, f.area());
    }

    if app.active_section == Section::Config {
        render_pending_write(f, app, f.area());
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

fn render_footer(f: &mut Frame, app: &MonitorApp, area: Rect) {
    let text = if app.active_section == Section::Logs && app.log_query_editing {
        format!(
            "search: {}\u{2588}  (Enter accept · Esc clear)",
            app.log_query
        )
    } else if app.active_section == Section::Config && app.config_confirm.is_some() {
        config_footer_hint(app)
    } else if !app.status_message.is_empty() && app.status_section == app.active_section {
        app.status_message.clone()
    } else {
        format!("{} • M mouse", footer_hint(app))
    };
    let footer = Paragraph::new(text)
        .style(Style::default().fg(Color::DarkGray))
        .block(Block::default().borders(Borders::ALL).title("Hints"));
    f.render_widget(footer, area);
}

fn footer_hint(app: &MonitorApp) -> String {
    let hint = match app.active_section {
        Section::Services => {
            "↑↓ navigate • Enter toggle • r restart • R force • Tab/⇧Tab switch • q quit"
        }
        Section::ActiveAgents => "↑↓ navigate • Enter inspect • Tab/⇧Tab switch • q quit",
        Section::Background => "↑↓ navigate • Enter details • x kill • Tab/⇧Tab switch • q quit",
        Section::Jobs => {
            "↑↓ navigate • Enter model • d inherit • a archive • Tab/⇧Tab switch • q quit"
        }
        Section::Config => return config_footer_hint(app),
        Section::Threads => {
            "↑↓ navigate • Enter preview • o raw • i timings • t kind • p project • / search • Esc clear • y copy ID"
        }
        Section::Usage => {
            "↑↓ scroll • PgUp/PgDn page • t span • R refresh • Tab/⇧Tab switch • q quit"
        }
        Section::Logs => {
            "↑↓ select • / search • l level • f target • [ ] file • L tail • Esc clear • Enter open • G follow • Tab switch • q quit"
        }
    };
    hint.to_string()
}

/// Spinner frame index derived from wall-clock seconds: the monitor redraws
/// on its 1s tick, so the running-tool glyph advances one frame per tick.
pub(crate) fn spinner_frame_now() -> usize {
    usize::try_from(chrono::Utc::now().timestamp().max(0)).unwrap_or(0)
}

pub(crate) fn truncate_chars(value: &str, max_chars: usize) -> String {
    if value.chars().count() <= max_chars {
        return value.to_string();
    }

    if max_chars == 0 {
        String::new()
    } else if max_chars == 1 {
        "…".to_string()
    } else {
        format!("{}…", value.chars().take(max_chars - 1).collect::<String>())
    }
}

/// Highlight for the selected row: a subtle background instead of a full
/// reverse-video block, which reads as a white bar on dark terminals.
pub(crate) const SELECTED_BG: Color = Color::Indexed(238);

/// Filter-and-pick popup shared by the Logs target filter and the Threads
/// project filter.
fn render_picker(f: &mut Frame, picker: &TargetPickerState, area: Rect, noun: &str) {
    let popup = centered_rect(60, 60, area);
    f.render_widget(Clear, popup);

    let title = format!(
        " pick {noun} · {} match · Enter apply · Esc cancel ",
        picker.matches.len(),
    );
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan))
        .title(title);
    let inner = block.inner(popup);
    f.render_widget(block, popup);

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Min(0)])
        .split(inner);

    let filter_line = Line::from(vec![
        Span::styled("filter: ", Style::default().fg(Color::DarkGray)),
        Span::styled(
            if picker.filter.is_empty() {
                "(type to filter)".to_string()
            } else {
                picker.filter.clone()
            },
            Style::default().fg(Color::Yellow),
        ),
    ]);
    f.render_widget(Paragraph::new(filter_line), rows[0]);

    let visible = rows[1].height as usize;
    let offset = picker.selected.saturating_sub(visible.saturating_sub(1));
    let end = (offset + visible).min(picker.matches.len());

    let width = rows[1].width as usize;
    let items: Vec<ListItem> = picker.matches[offset..end]
        .iter()
        .enumerate()
        .map(|(i, &item_index)| {
            let global = offset + i;
            let (target, count) = &picker.items[item_index];
            let (label, suffix) = picker_row_text(target, *count, width);
            let row = Line::from(vec![
                Span::styled(label, Style::default().fg(Color::Cyan)),
                Span::styled(suffix, Style::default().fg(Color::DarkGray)),
            ]);
            let item = ListItem::new(row);
            if global == picker.selected {
                item.style(Style::default().bg(SELECTED_BG))
            } else {
                item
            }
        })
        .collect();
    f.render_widget(List::new(items), rows[1]);
}

/// Splits a picker row into its label and count suffix, trimming the end of
/// the label so the count always fits in `width` columns.
fn picker_row_text(target: &str, count: Option<usize>, width: usize) -> (String, String) {
    let suffix = match count {
        Some(count) => format!("  ({count})"),
        None => "  (?)".to_string(),
    };
    let label_width = width.saturating_sub(suffix.chars().count());
    (truncate_chars(target, label_width), suffix)
}

/// Build a centered Rect using `percent_x` × `percent_y` of `area`.
pub(crate) fn centered_rect(percent_x: u16, percent_y: u16, area: Rect) -> Rect {
    let popup_w = area.width.saturating_mul(percent_x) / 100;
    let popup_h = area.height.saturating_mul(percent_y) / 100;
    let x = area.x + area.width.saturating_sub(popup_w) / 2;
    let y = area.y + area.height.saturating_sub(popup_h) / 2;
    Rect {
        x,
        y,
        width: popup_w,
        height: popup_h,
    }
}

#[cfg(test)]
mod tests {
    use ratatui::backend::TestBackend;

    use super::*;
    use crate::tabs::threads::TimingOverlayState;

    #[test]
    fn picker_row_keeps_count_visible_when_label_is_trimmed() {
        let (label, suffix) = picker_row_text("example  ~/projects/example", Some(3), 20);
        assert_eq!(suffix, "  (3)");
        assert_eq!(label, "example  ~/pro…");
        assert_eq!(label.chars().count() + suffix.chars().count(), 20);

        let (label, suffix) = picker_row_text("short", None, 20);
        assert_eq!((label.as_str(), suffix.as_str()), ("short", "  (?)"));
    }

    #[test]
    fn timing_overlay_renders_title_and_unavailable_state() {
        let backend = TestBackend::new(80, 12);
        let mut terminal = Terminal::new(backend).unwrap();
        let state = TimingOverlayState {
            title: "thread-1 · Demo".to_string(),
            lines: vec![
                "Turn 1".to_string(),
                "  Tool work (sum, not wall time): unavailable (0/1 measured)".to_string(),
            ],
            scroll: 0,
        };
        terminal
            .draw(|frame| render_timing_overlay(frame, &state, frame.area()))
            .unwrap();

        let buffer = terminal.backend().buffer();
        let mut text = String::new();
        for y in 0..buffer.area.height {
            for x in 0..buffer.area.width {
                text.push_str(buffer.cell((x, y)).unwrap().symbol());
            }
            text.push('\n');
        }
        assert!(text.contains("Trajectory · thread-1 · Demo"));
        assert!(text.contains("unavailable (0/1 measured)"));
    }
}
