use crossterm::event::KeyCode;
use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, Paragraph};

use crate::app::{MonitorApp, copy_text, lines_text};
use crate::ui::{SELECTED_BG, truncate_chars};

/// One running background process shown in the Background tab.
pub struct BackgroundInfo {
    pub bg_id: String,
    pub pid: u32,
    /// Full originating thread id (`None` for no-thread runs); used to group.
    pub thread_id: Option<String>,
    pub command: String,
    pub uptime: String,
}

/// State for the Background tab's process detail overlay (drill-in on
/// `Enter`). Keyed by `bg_id` so the on-tick refresh keeps re-reading the
/// marker + log tails while the process runs.
pub struct BackgroundDetailState {
    pub bg_id: String,
    /// Header label, e.g. `bg-abc123 · pid 4567`.
    pub title: String,
    /// Rendered body, pre-wrapped to the terminal width at build time so
    /// scroll offsets count real rows.
    pub lines: Vec<Line<'static>>,
    /// Top-row offset; ignored while `follow` is set.
    pub scroll: usize,
    /// Pin the view to the newest output (like the Logs tab's `G` follow).
    pub follow: bool,
}

impl BackgroundDetailState {
    /// Handles a key while the overlay is open. Returns `true` when the
    /// overlay should close.
    fn handle_key(&mut self, key: KeyCode, page: usize) -> bool {
        let max = self.max_scroll(page);
        match key {
            KeyCode::Esc | KeyCode::Char('q') => return true,
            KeyCode::Down | KeyCode::Char('j') => self.scroll_to(self.offset(page) + 1, max),
            KeyCode::Up | KeyCode::Char('k') => {
                self.scroll_to(self.offset(page).saturating_sub(1), max);
            }
            KeyCode::PageDown => self.scroll_to(self.offset(page) + page, max),
            KeyCode::PageUp => self.scroll_to(self.offset(page).saturating_sub(page), max),
            KeyCode::Home => self.scroll_to(0, max),
            KeyCode::Char('G') | KeyCode::End => self.follow = true,
            _ => {}
        }
        false
    }

    pub(crate) fn scroll_to(&mut self, target: usize, max: usize) {
        self.follow = target >= max;
        self.scroll = target.min(max);
    }

    /// Current top row, resolving follow mode against the rendered height.
    pub fn offset(&self, page: usize) -> usize {
        let max = self.max_scroll(page);
        if self.follow {
            max
        } else {
            self.scroll.min(max)
        }
    }

    pub(crate) fn max_scroll(&self, page: usize) -> usize {
        self.lines.len().saturating_sub(page.max(1))
    }
}

/// Loads running background processes for the Background tab, sorted so
/// same-thread processes are adjacent (the render groups them under a header).
pub(crate) fn load_background() -> Vec<BackgroundInfo> {
    let mut v: Vec<BackgroundInfo> = zdx_engine::background_activity::list_background()
        .into_iter()
        .filter(zdx_engine::background_activity::BackgroundProcess::is_running)
        .map(|p| {
            let uptime = p.uptime();
            BackgroundInfo {
                bg_id: p.bg_id,
                pid: p.pid,
                thread_id: p.thread_id,
                command: p.command,
                uptime,
            }
        })
        .collect();
    v.sort_by(|a, b| a.thread_id.cmp(&b.thread_id).then(a.bg_id.cmp(&b.bg_id)));
    v
}

/// Max bytes shown per stream (stdout/stderr) in the Background detail overlay.
const BACKGROUND_DETAIL_TAIL_BYTES: usize = 64 * 1024;

/// Rows visible inside the full-frame Background detail overlay (borders eat 2).
fn background_detail_page_size(app: &MonitorApp) -> usize {
    (app.terminal_height.saturating_sub(2) as usize).max(1)
}

/// Wrap width inside the full-frame Background detail overlay.
fn background_detail_wrap_width(app: &MonitorApp) -> usize {
    (app.terminal_width.saturating_sub(2) as usize).max(20)
}

/// Opens the detail overlay for the selected background process: full
/// command, marker metadata, and the stdout/stderr log tails.
pub(crate) fn open_background_detail(app: &mut MonitorApp) {
    let Some(info) = app.background.get(app.selected_index) else {
        return;
    };
    let bg_id = info.bg_id.clone();
    let title = format!("{bg_id} · pid {}", info.pid);
    let Some(lines) = build_background_detail_lines(&bg_id, background_detail_wrap_width(app))
    else {
        app.set_status(format!("No record for {bg_id} (it may have just exited)"));
        return;
    };
    app.background_detail = Some(BackgroundDetailState {
        bg_id,
        title,
        lines,
        scroll: 0,
        follow: true,
    });
}

/// Rebuilds the open detail overlay from disk on the refresh tick so a running
/// process's output keeps streaming in. If the marker vanished (tombstone
/// pruned), the last snapshot is kept until the overlay is closed.
pub(crate) fn refresh_background_detail(app: &mut MonitorApp) {
    let width = background_detail_wrap_width(app);
    if let Some(state) = app.background_detail.as_mut()
        && let Some(lines) = build_background_detail_lines(&state.bg_id, width)
    {
        state.lines = lines;
    }
}

/// Builds the detail body for one background process, pre-wrapped to `width`.
/// `None` when no marker exists for `bg_id` anymore.
fn build_background_detail_lines(bg_id: &str, width: usize) -> Option<Vec<Line<'static>>> {
    use zdx_engine::background_activity as bg;

    let rec = bg::get(bg_id)?;
    let label = |name: &str| Span::styled(format!(" {name:<9}"), Style::default().fg(Color::Cyan));
    let dim = Style::default().fg(Color::DarkGray);

    let mut lines: Vec<Line<'static>> = Vec::new();
    let status = if rec.is_running() {
        Span::styled(
            format!("running · up {}", rec.uptime()),
            Style::default().fg(Color::Green),
        )
    } else {
        let code = rec
            .exit_code
            .map_or_else(|| "killed/unknown".to_string(), |c| format!("code {c}"));
        Span::styled(format!("exited ({code})"), Style::default().fg(Color::Red))
    };
    lines.push(Line::from(vec![label("status"), status]));
    lines.push(Line::from(vec![
        label("pid"),
        Span::raw(format!("{} (pgid {})", rec.pid, rec.pgid)),
    ]));
    lines.push(Line::from(vec![
        label("started"),
        Span::raw(rec.started_at.clone()),
    ]));
    if let Some(exited_at) = &rec.exited_at {
        lines.push(Line::from(vec![
            label("ended"),
            Span::raw(exited_at.clone()),
        ]));
    }
    lines.push(Line::from(vec![
        label("thread"),
        Span::raw(
            rec.thread_id
                .clone()
                .unwrap_or_else(|| "(no thread)".to_string()),
        ),
    ]));
    lines.push(Line::from(vec![label("cwd"), Span::raw(rec.cwd.clone())]));

    let header = |name: &str, color: Color| {
        Line::from(Span::styled(
            format!(" ── {name} "),
            Style::default().fg(color).add_modifier(Modifier::BOLD),
        ))
    };

    lines.push(Line::default());
    lines.push(header("command", Color::Cyan));
    for l in rec.command.lines() {
        let l = zdx_transcript::text::sanitize_for_display(l);
        lines.push(Line::from(format!(" {l}")));
    }

    for (name, path, color) in [
        ("stdout", bg::stdout_log_path(bg_id), Color::Green),
        ("stderr", bg::stderr_log_path(bg_id), Color::Yellow),
    ] {
        lines.push(Line::default());
        lines.push(header(name, color));
        let tail = bg::read_log_tail(&path, BACKGROUND_DETAIL_TAIL_BYTES);
        if tail.is_empty() {
            lines.push(Line::from(Span::styled(" (no output)", dim)));
        } else {
            for l in tail.lines() {
                let l = zdx_transcript::text::sanitize_for_display(l);
                lines.push(Line::from(format!(" {l}")));
            }
        }
    }

    Some(
        lines
            .iter()
            .flat_map(|line| zdx_transcript::wrap_line_to_width(line, width))
            .collect(),
    )
}

pub(crate) fn handle_background_detail_key(app: &mut MonitorApp, key: KeyCode) {
    let page = background_detail_page_size(app);
    let Some(state) = app.background_detail.as_mut() else {
        return;
    };
    match key {
        KeyCode::Char('y') => {
            let command = zdx_engine::background_activity::get(&state.bg_id).map(|r| r.command);
            if let Some(command) = command {
                copy_text(app, &command, "Copied command");
            }
            return;
        }
        KeyCode::Char('Y') => {
            let text = lines_text(&state.lines);
            copy_text(app, &text, "Copied process detail");
            return;
        }
        _ => {}
    }
    if state.handle_key(key, page) {
        app.background_detail = None;
    }
}

/// Stops the selected background process. Optimistically removes the row; the
/// on-tick refresh reconciles once the async kill lands. Runs the async kill on
/// its own thread with a current-thread Tokio runtime (the monitor has no
/// ambient runtime).
pub(crate) fn kill_selected_background(app: &mut MonitorApp) {
    let Some(info) = app.background.get(app.selected_index) else {
        return;
    };
    let bg_id = info.bg_id.clone();
    app.set_status(format!("Stopping {bg_id}…"));
    app.background.remove(app.selected_index);
    app.clamp_selection();
    std::thread::spawn(move || {
        if let Ok(rt) = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        {
            rt.block_on(async {
                let _ = zdx_engine::background_activity::kill_background(&bg_id).await;
            });
        }
    });
}

pub(crate) fn render_background(f: &mut Frame, app: &MonitorApp, area: Rect) {
    if app.background.is_empty() {
        let p = Paragraph::new(" No background processes")
            .style(Style::default().fg(Color::DarkGray))
            .block(Block::default().borders(Borders::ALL).title("Background"));
        f.render_widget(p, area);
        return;
    }

    let inner_width = area.width.saturating_sub(2) as usize;
    let mut items: Vec<ListItem> = Vec::new();
    let mut last_thread: Option<&Option<String>> = None;
    for (i, b) in app.background.iter().enumerate() {
        if last_thread != Some(&b.thread_id) {
            last_thread = Some(&b.thread_id);
            let label = b.thread_id.as_deref().unwrap_or("(no thread)");
            items.push(
                ListItem::new(format!(" thread {label}")).style(
                    Style::default()
                        .fg(Color::DarkGray)
                        .add_modifier(Modifier::BOLD),
                ),
            );
        }
        let prefix = format!("   ● pid {:<7} up {:<8} ", b.pid, b.uptime);
        let cmd_width = inner_width.saturating_sub(prefix.chars().count());
        let cmd = truncate_chars(&b.command, cmd_width);
        let line = format!("{prefix}{cmd}");
        let style = if i == app.selected_index {
            Style::default().fg(Color::Green).bg(SELECTED_BG)
        } else {
            Style::default().fg(Color::Green)
        };
        items.push(ListItem::new(line).style(style));
    }

    let title = format!("Background processes ({})", app.background.len());
    let list = List::new(items).block(Block::default().borders(Borders::ALL).title(title));
    f.render_widget(list, area);
}

/// Full-frame detail view for one background process: marker metadata, the
/// full command, and the stdout/stderr log tails. Lines are pre-wrapped at
/// build time, so scrolling is a plain row-window here.
pub(crate) fn render_background_detail(f: &mut Frame, state: &BackgroundDetailState, area: Rect) {
    f.render_widget(Clear, area);
    let visible_rows = (area.height.saturating_sub(2) as usize).max(1);
    let offset = state.offset(visible_rows);
    let end = (offset + visible_rows).min(state.lines.len());
    let items: Vec<ListItem> = state.lines[offset..end]
        .iter()
        .map(|line| ListItem::new(line.clone()))
        .collect();

    let position = if state.lines.len() > visible_rows {
        format!(" [{}/{}]", offset + 1, state.lines.len())
    } else {
        String::new()
    };
    let follow = if state.follow { " · following" } else { "" };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Green))
        .title(format!(" Background · {} ", state.title))
        .title_bottom(format!(
            " j/k scroll · gg top · G follow · y cmd · Y text · Esc close{position}{follow} "
        ));
    f.render_widget(List::new(items).block(block), area);
}
