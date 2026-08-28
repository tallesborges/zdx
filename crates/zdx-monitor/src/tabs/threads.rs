use std::collections::HashSet;
use std::path::Path;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use crossterm::event::KeyCode;
use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, Clear, List, ListItem};
use zdx_engine::core::thread_index::{self, ThreadBrowseOptions, ThreadKindFilter};
use zdx_engine::core::thread_persistence;
use zdx_engine::core::thread_timing::{format_thread_timing_report, inspect_thread_timings};

use crate::app::{MonitorApp, Section, TargetPickerState, copy_text};
use crate::tabs::agents::{AgentOverlayState, load_transcript_into, transcript_path};
use crate::ui::{SELECTED_BG, truncate_chars};

/// Max rows the Threads tab asks the index for (applied in SQL).
const THREAD_LIST_LIMIT: usize = 500;

/// How long a loaded Threads result stays fresh before the tick reloads it.
const THREAD_REFRESH_INTERVAL: Duration = Duration::from_secs(10);

/// Result of one background Threads query: the rows for the active filters
/// plus the project list feeding the `p` picker, both read from the same
/// synced index.
pub struct ThreadsSnapshot {
    threads: Vec<ThreadInfo>,
    projects: Vec<(String, usize)>,
}

pub struct ThreadInfo {
    pub id: String,
    pub title: Option<String>,
    /// Last component of the thread's root path, to identify the project.
    pub project: Option<String>,
    /// Run-kind badge (`explorer`, `tldr`, …); `None` for top-level threads.
    pub badge: Option<String>,
    pub modified: String,
    /// First user message, as stored in the thread index.
    pub preview: Option<String>,
}

pub struct TimingOverlayState {
    pub title: String,
    pub lines: Vec<String>,
    pub scroll: usize,
}

impl TimingOverlayState {
    pub(crate) fn handle_key(&mut self, key: KeyCode, page: usize) -> bool {
        let max = self.lines.len().saturating_sub(page.max(1));
        match key {
            KeyCode::Esc | KeyCode::Char('q') => return true,
            KeyCode::Down | KeyCode::Char('j') => {
                self.scroll = self.scroll.saturating_add(1).min(max);
            }
            KeyCode::Up | KeyCode::Char('k') => self.scroll = self.scroll.saturating_sub(1),
            KeyCode::PageDown => self.scroll = self.scroll.saturating_add(page).min(max),
            KeyCode::PageUp => self.scroll = self.scroll.saturating_sub(page),
            KeyCode::Home => self.scroll = 0,
            KeyCode::Char('G') | KeyCode::End => self.scroll = max,
            _ => {}
        }
        false
    }
}

/// Handle a key while the Threads section is active. Returns `true` if the key
/// was consumed (so the generic dispatcher should not also act on it).
pub(crate) fn handle_threads_key(app: &mut MonitorApp, key: KeyCode) -> bool {
    match key {
        KeyCode::Char('t') => {
            app.thread_kind_filter = app.thread_kind_filter.next();
            start_threads_query(app);
            app.set_status(format!("Kind filter: {}", app.thread_kind_filter.label()));
            true
        }
        KeyCode::Char('p') => {
            app.thread_project_picker =
                Some(TargetPickerState::from_items(app.thread_projects.clone()));
            true
        }
        KeyCode::Char('/') => {
            app.thread_query_editing = true;
            true
        }
        KeyCode::Enter => {
            open_thread_overlay(app);
            true
        }
        KeyCode::Char('i') => {
            open_thread_timing_overlay(app);
            true
        }
        KeyCode::Char('o') => {
            open_raw_thread(app);
            true
        }
        KeyCode::Esc => {
            let had_filter = app.thread_kind_filter != ThreadKindFilter::All
                || app.thread_project_filter.is_some()
                || !app.thread_query.is_empty();
            if had_filter {
                app.thread_kind_filter = ThreadKindFilter::All;
                app.thread_project_filter = None;
                app.thread_query.clear();
                start_threads_query(app);
                app.set_status("Filters cleared");
            }
            true
        }
        _ => false,
    }
}

/// Handle a key while the Threads project picker is open. Consumes every key.
pub(crate) fn handle_thread_project_picker_key(app: &mut MonitorApp, key: KeyCode) {
    let Some(picker) = app.thread_project_picker.as_mut() else {
        return;
    };
    match key {
        KeyCode::Esc => app.thread_project_picker = None,
        KeyCode::Down => {
            let last = picker.matches.len().saturating_sub(1);
            picker.selected = (picker.selected + 1).min(last);
        }
        KeyCode::Up => picker.selected = picker.selected.saturating_sub(1),
        KeyCode::Backspace => {
            picker.filter.pop();
            picker.recompute();
        }
        KeyCode::Char(c) => {
            picker.filter.push(c);
            picker.recompute();
        }
        KeyCode::Enter => {
            let chosen = picker.selected_target().map(str::to_string);
            app.thread_project_picker = None;
            if let Some(root_path) = chosen {
                app.thread_project_filter = Some(root_path.clone());
                start_threads_query(app);
                app.set_status(format!("Project filter: {root_path}"));
            }
        }
        _ => {}
    }
}

/// Handle a key while the Threads query is being edited. Consumes every key so
/// a typed `q` can't quit the monitor mid-search. The query runs on `Enter`,
/// not per keystroke: a one-letter prefix matches most of the FTS corpus and
/// would stall the UI on every character.
pub(crate) fn handle_thread_query_key(app: &mut MonitorApp, key: KeyCode) {
    match key {
        KeyCode::Char(c) => app.thread_query.push(c),
        KeyCode::Backspace => {
            app.thread_query.pop();
        }
        KeyCode::Enter => {
            app.thread_query_editing = false;
            start_threads_query(app);
            let status = if app.thread_query.is_empty() {
                "Search cleared".to_string()
            } else {
                format!("Search: /{}", app.thread_query)
            };
            app.set_status(status);
        }
        KeyCode::Esc => {
            app.thread_query_editing = false;
            app.thread_query.clear();
            start_threads_query(app);
            app.set_status("Search cleared");
        }
        _ => {}
    }
}

/// Filters the Threads tab is currently showing.
fn thread_browse_options(app: &MonitorApp) -> ThreadBrowseOptions {
    ThreadBrowseOptions {
        kind: app.thread_kind_filter,
        project: app.thread_project_filter.clone(),
        query: Some(app.thread_query.clone()).filter(|q| !q.trim().is_empty()),
        limit: THREAD_LIST_LIMIT,
    }
}

/// Spawns a background Threads query unless one is already in flight.
///
/// Reading the index syncs it first, and that sync stats every thread file
/// (~125ms at 12k threads) before re-parsing whatever changed, so it must
/// never run inline: `switch_section` is on the path between a keypress and
/// the next `terminal.draw`, and the Threads tab sits directly before Usage in
/// the tab order, so an inline sync stalls every pass through it.
fn start_threads_query(app: &mut MonitorApp) {
    if app.threads_rx.is_some() {
        return;
    }
    let options = thread_browse_options(app);
    app.threads_scan_options = Some(options.clone());
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(ThreadsSnapshot {
            threads: load_threads(&options),
            projects: thread_index::browse_projects().unwrap_or_default(),
        });
    });
    app.threads_rx = Some(rx);
}

/// Collects a finished background Threads query into the view. Non-blocking:
/// returns immediately while the query is still running. If the filters moved
/// on while it ran, re-queries now rather than waiting for the staleness tick.
pub(crate) fn poll_threads_result(app: &mut MonitorApp) {
    let Some(rx) = &app.threads_rx else {
        return;
    };
    match rx.try_recv() {
        Ok(snapshot) => {
            app.threads_rx = None;
            let scanned = app.threads_scan_options.take();
            app.threads = snapshot.threads;
            app.thread_projects = snapshot.projects;
            app.threads_loaded_at = Some(Instant::now());
            app.clamp_selection();
            if scanned.is_some_and(|options| options != thread_browse_options(app)) {
                start_threads_query(app);
            }
        }
        Err(mpsc::TryRecvError::Empty) => {}
        Err(mpsc::TryRecvError::Disconnected) => app.threads_rx = None,
    }
}

/// Starts a Threads query when the tab is active and its rows are missing or
/// older than [`THREAD_REFRESH_INTERVAL`]. Filter changes bypass this and call
/// [`start_threads_query`] directly.
pub(crate) fn refresh_threads_if_stale(app: &mut MonitorApp) {
    if app.active_section != Section::Threads {
        return;
    }
    if app
        .threads_loaded_at
        .is_some_and(|at| at.elapsed() < THREAD_REFRESH_INTERVAL)
    {
        return;
    }
    start_threads_query(app);
}

pub(crate) fn copy_selected_thread_id(app: &mut MonitorApp) {
    if app.active_section == Section::Threads
        && let Some(t) = app.threads.get(app.selected_index)
    {
        let id = t.id.clone();
        copy_text(app, &id, &format!("Copied thread ID {id}"));
    }
}

fn open_raw_thread(app: &mut MonitorApp) {
    let Some(thread) = app.threads.get(app.selected_index) else {
        return;
    };
    let path = transcript_path(&thread.id);
    if path.exists() {
        app.pending_open_path = Some(path);
    } else {
        app.set_status(format!("Raw thread file not found: {}", thread.id));
    }
}

/// Rows shown in the Threads tab. Filtering, ordering, and the cap all happen
/// in `threads.sqlite`; a cache error yields an empty list rather than a
/// directory walk. Runs on the background query thread, never on the UI thread.
fn load_threads(options: &ThreadBrowseOptions) -> Vec<ThreadInfo> {
    let Ok(rows) = thread_index::browse_threads(options) else {
        return Vec::new();
    };

    rows.into_iter()
        .map(|row| ThreadInfo {
            id: row.id,
            title: row.title,
            project: row
                .root_path
                .as_deref()
                .map(Path::new)
                .and_then(Path::file_name)
                .map(|name| name.to_string_lossy().to_string()),
            badge: thread_badge(row.origin_kind.as_deref(), row.subagent_name.as_deref()),
            modified: row
                .modified
                .map(|t| {
                    let dt: chrono::DateTime<chrono::Local> = t.into();
                    dt.format("%Y-%m-%d %H:%M").to_string()
                })
                .unwrap_or_default(),
            preview: row.preview.filter(|p| !p.trim().is_empty()),
        })
        .collect()
}

/// Short label for a thread's run kind: the named subagent for `subagent`
/// runs, the suffix for `helper:*` runs, `None` for top-level threads.
fn thread_badge(origin_kind: Option<&str>, subagent_name: Option<&str>) -> Option<String> {
    match origin_kind? {
        "subagent" => Some(subagent_name.unwrap_or("subagent").to_string()),
        kind => Some(kind.strip_prefix("helper:").unwrap_or(kind).to_string()),
    }
}

/// Opens the transcript overlay for the currently selected saved thread.
/// Reuses the Active Agents overlay: a saved thread is simply a run that is
/// no longer active, and the file is still re-read while the overlay is open.
fn open_thread_overlay(app: &mut MonitorApp) {
    let Some(t) = app.threads.get(app.selected_index) else {
        return;
    };
    let short_id = t.id.get(..8).unwrap_or(&t.id);
    let label = t.title.clone().unwrap_or_else(|| {
        t.preview
            .as_deref()
            .and_then(|p| p.lines().find(|l| !l.trim().is_empty()))
            .map_or_else(|| "(untitled)".to_string(), |l| l.trim().to_string())
    });
    let title = format!(
        "{} {short_id} {label}",
        t.badge.as_deref().unwrap_or("thread"),
    );
    let mut state = AgentOverlayState {
        thread_id: t.id.clone(),
        title,
        lines: Vec::new(),
        cells: Vec::new(),
        tools: Vec::new(),
        thinking: Vec::new(),
        expanded_thinking: HashSet::new(),
        tool_selected: None,
        tool_pane: None,
        scroll: None,
        ended: true,
        unavailable: false,
        file_len: 0,
        file_mtime: None,
        running_sig: Vec::new(),
        run_phase: None,
        running_tool: None,
        width: app.terminal_width.saturating_sub(2) as usize,
    };
    load_transcript_into(&mut state);
    app.agent_overlay = Some(state);
}

fn open_thread_timing_overlay(app: &mut MonitorApp) {
    let Some(thread) = app.threads.get(app.selected_index) else {
        return;
    };
    let events = thread_persistence::load_thread_events(&thread.id).unwrap_or_default();
    app.timing_overlay = Some(timing_overlay_from_events(
        &thread.id,
        thread.title.as_deref(),
        &events,
    ));
}

pub(crate) fn handle_timing_overlay_key(app: &mut MonitorApp, key: KeyCode) {
    let page = (app.terminal_height.saturating_sub(2) as usize).max(1);
    let Some(state) = app.timing_overlay.as_mut() else {
        return;
    };
    if state.handle_key(key, page) {
        app.timing_overlay = None;
    }
}

pub(crate) fn timing_overlay_from_events(
    thread_id: &str,
    title: Option<&str>,
    events: &[thread_persistence::ThreadEvent],
) -> TimingOverlayState {
    let title = title.map_or_else(
        || thread_id.to_string(),
        |title| format!("{thread_id} · {title}"),
    );
    TimingOverlayState {
        title,
        lines: format_thread_timing_report(&inspect_thread_timings(events)),
        scroll: 0,
    }
}

pub(crate) fn render_threads(f: &mut Frame, app: &MonitorApp, area: Rect) {
    let rows = (area.height.saturating_sub(2) as usize).max(1);
    let offset = app.selected_index.saturating_sub(rows.saturating_sub(1));
    let end = (offset + rows).min(app.threads.len());
    let width = area.width.saturating_sub(2) as usize;

    let items: Vec<ListItem> = app.threads[offset..end]
        .iter()
        .enumerate()
        .map(|(i, t)| {
            let selected = offset + i == app.selected_index;
            let marker = if selected { "▌" } else { " " };
            let mut spans = vec![
                Span::styled(marker, Style::default().fg(Color::Cyan)),
                Span::styled(format!("{} ", t.modified), Style::default().fg(Color::Gray)),
                Span::styled(
                    format!(
                        "{:<20} ",
                        truncate_chars(t.project.as_deref().unwrap_or("-"), 20)
                    ),
                    Style::default().fg(Color::Blue),
                ),
            ];
            if let Some(badge) = &t.badge {
                spans.push(Span::styled(
                    format!("[{badge}] "),
                    Style::default().fg(Color::Magenta),
                ));
            }
            // Child runs have no title, so their stored preview is the only
            // thing that identifies them on a one-line row.
            let label = t.title.as_deref().map_or_else(
                || {
                    t.preview.as_deref().map_or_else(
                        || "(untitled)".to_string(),
                        |preview| {
                            preview
                                .lines()
                                .find(|l| !l.trim().is_empty())
                                .unwrap_or("(untitled)")
                                .trim()
                                .to_string()
                        },
                    )
                },
                str::to_string,
            );
            let used: usize = spans.iter().map(|s| s.content.chars().count()).sum();
            spans.push(Span::raw(truncate_chars(
                &label,
                width.saturating_sub(used + 11),
            )));
            spans.push(Span::styled(
                format!("  {}", t.id.get(..8).unwrap_or(&t.id)),
                Style::default().fg(Color::DarkGray),
            ));

            let item = ListItem::new(Line::from(spans));
            if selected {
                item.style(Style::default().bg(SELECTED_BG))
            } else {
                item
            }
        })
        .collect();

    let mut parts = vec![
        format!("Threads ({})", app.threads.len()),
        app.thread_kind_filter.label().to_string(),
    ];
    if let Some(project) = &app.thread_project_filter {
        parts.push(
            Path::new(project)
                .file_name()
                .map_or_else(|| project.clone(), |n| n.to_string_lossy().to_string()),
        );
    }
    if app.thread_query_editing {
        parts.push(format!("/{}_", app.thread_query));
    } else if !app.thread_query.is_empty() {
        parts.push(format!("/{}", app.thread_query));
    }
    if app.threads_rx.is_some() {
        parts.push("refreshing".to_string());
    }
    let title = format!(" {} ", parts.join(" · "));

    let list = List::new(items).block(Block::default().borders(Borders::ALL).title(title));
    f.render_widget(list, area);
}

pub(crate) fn render_timing_overlay(f: &mut Frame, state: &TimingOverlayState, area: Rect) {
    f.render_widget(Clear, area);
    let visible_rows = area.height.saturating_sub(2) as usize;
    let max = state.lines.len().saturating_sub(visible_rows);
    let offset = state.scroll.min(max);
    let end = (offset + visible_rows).min(state.lines.len());
    let items: Vec<ListItem> = state.lines[offset..end]
        .iter()
        .map(|line| ListItem::new(line.as_str()))
        .collect();
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan))
        .title(format!(" Timings · {} ", state.title))
        .title_bottom(" j/k scroll · gg/G top/bottom · Esc close ");
    f.render_widget(List::new(items).block(block), area);
}
