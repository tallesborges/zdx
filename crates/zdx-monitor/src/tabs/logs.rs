use std::io::{Read, Seek};
use std::path::{Path, PathBuf};
use std::time::SystemTime;
use std::{fs, io};

use crossterm::event::KeyCode;
use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, Paragraph, Wrap};
use zdx_engine::config::paths;

use crate::app::{MonitorApp, TargetPickerState, copy_text};
use crate::log_line::{LevelFilter, line_matches, parse_log_line};
use crate::ui::centered_rect;

pub(crate) const LOG_TAIL_LINES: usize = 500;

const LOG_TAIL_STEPS: [usize; 3] = [500, 2000, 10000];

/// Visible content rows in the Logs panel (same chrome as Config).
fn log_page_size(app: &MonitorApp) -> usize {
    (app.terminal_height.saturating_sub(8) as usize).max(1)
}

/// Indices of `lines` passing the active filters, in file order.
fn visible_log_indices(
    lines: &[String],
    level: LevelFilter,
    query: &str,
    target: Option<&str>,
) -> Vec<usize> {
    let query_lower = query.to_lowercase();
    lines
        .iter()
        .enumerate()
        .filter(|(_, line)| line_matches(line, level, &query_lower, target))
        .map(|(i, _)| i)
        .collect()
}

/// Clamp a (selected, offset) pair against a visible-list length. In follow
/// mode the selection pins to the last entry; otherwise it is clamped into
/// range and the offset is adjusted to keep it on screen.
pub(crate) fn clamp_log_view(
    total: usize,
    follow: bool,
    page: usize,
    selected: usize,
    offset: usize,
) -> (usize, usize) {
    if total == 0 {
        return (0, 0);
    }
    if follow {
        let selected = total - 1;
        return (selected, total.saturating_sub(page));
    }
    let selected = selected.min(total - 1);
    // Clamp against the largest offset that still fills the page first,
    // otherwise an offset left over from a longer list (e.g. before a filter
    // narrowed it) strands the selection alone at the top row.
    let max_offset = total.saturating_sub(page);
    let offset = offset.min(max_offset);
    let offset = if selected < offset {
        selected
    } else if page > 0 && selected >= offset + page {
        selected + 1 - page
    } else {
        offset
    };
    (selected, offset)
}

/// Rebuild `log_visible` from `log_lines` under the active filters, then clamp
/// selection/offset. Follow mode keeps the selection pinned to the newest
/// matching line.
pub(crate) fn recompute_log_visible(app: &mut MonitorApp) {
    app.log_visible = visible_log_indices(
        &app.log_lines,
        app.log_level_filter,
        &app.log_query,
        app.log_target_filter.as_deref(),
    );
    let (selected, offset) = clamp_log_view(
        app.log_visible.len(),
        app.log_follow,
        log_page_size(app),
        app.log_selected,
        app.log_offset,
    );
    app.log_selected = selected;
    app.log_offset = offset;
    if app.log_visible.is_empty() {
        app.log_overlay_open = false;
    }
}

/// Adjust `log_offset` so `log_selected` is in the visible window.
pub(crate) fn ensure_log_selected_visible(app: &mut MonitorApp) {
    let (selected, offset) = clamp_log_view(
        app.log_visible.len(),
        false,
        log_page_size(app),
        app.log_selected,
        app.log_offset,
    );
    app.log_selected = selected;
    app.log_offset = offset;
}

/// Read up to `max_lines` final lines from a log file by tailing its end.
///
/// The read window scales with `max_lines` (~512 B per line, floor 256 KiB) so
/// a raised tail size actually reaches further back. At the default tail this
/// is the same 256 KiB window the tab has always used.
fn tail_lines(path: &Path, max_lines: usize) -> io::Result<Vec<String>> {
    let mut file = fs::File::open(path)?;
    let metadata = file.metadata()?;
    let file_size = metadata.len();
    let window = (max_lines as u64).saturating_mul(512).max(256 * 1024);
    let read_size: u64 = file_size.min(window);
    let start = file_size.saturating_sub(read_size);
    file.seek(io::SeekFrom::Start(start))?;
    let mut buf = Vec::with_capacity(read_size as usize);
    file.read_to_end(&mut buf)?;
    let text = String::from_utf8_lossy(&buf);
    let mut lines: Vec<String> = text.lines().map(String::from).collect();
    // If we started mid-file, the first line is likely a partial — drop it.
    if start > 0 && !lines.is_empty() {
        lines.remove(0);
    }
    let len = lines.len();
    if len > max_lines {
        lines.drain(0..len - max_lines);
    }
    Ok(lines)
}

/// Whether a log directory entry is one of the rolling `zdx.log` files.
/// `~/.zdx/logs` also holds unrelated files (e.g. `automations-daemon.log`).
fn is_zdx_log_name(name: &str) -> bool {
    name == "zdx.log" || name.starts_with("zdx.log.")
}

/// `zdx.log*` files in `~/.zdx/logs`, newest first. The rolling appender names
/// files `zdx.log.YYYY-MM-DD`, so a reverse name sort is a reverse date sort.
fn log_file_list() -> Vec<PathBuf> {
    let dir = paths::zdx_home().join("logs");
    let Ok(entries) = fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut files: Vec<PathBuf> = entries
        .flatten()
        .filter(|e| e.metadata().is_ok_and(|m| m.is_file()))
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(is_zdx_log_name)
        })
        .collect();
    files.sort_by(|a, b| b.file_name().cmp(&a.file_name()));
    files
}

/// Identity of a loaded log file, used to skip redundant re-reads.
#[derive(Clone, PartialEq, Eq)]
pub struct LoadedLogFile {
    path: PathBuf,
    len: u64,
    modified: Option<SystemTime>,
    tail_lines: usize,
}

fn log_file_stamp(path: &Path, tail: usize) -> Option<LoadedLogFile> {
    let metadata = fs::metadata(path).ok()?;
    Some(LoadedLogFile {
        path: path.to_path_buf(),
        len: metadata.len(),
        modified: metadata.modified().ok(),
        tail_lines: tail,
    })
}

/// Refresh the log file list, then tail the selected file if its identity
/// changed since the last load. Older files therefore load once instead of on
/// every tick.
pub(crate) fn load_active_log(app: &mut MonitorApp) {
    app.log_files = log_file_list();
    if app.log_files.is_empty() {
        app.log_file_index = 0;
        app.log_file_name = None;
        app.log_lines.clear();
        app.log_loaded = None;
        recompute_log_visible(app);
        return;
    }
    app.log_file_index = app.log_file_index.min(app.log_files.len() - 1);
    let path = app.log_files[app.log_file_index].clone();
    let stamp = log_file_stamp(&path, app.log_tail_lines);
    if stamp.is_some() && stamp == app.log_loaded {
        return;
    }
    app.log_file_name = path.file_name().map(|s| s.to_string_lossy().into_owned());
    app.log_lines = tail_lines(&path, app.log_tail_lines).unwrap_or_default();
    app.log_loaded = stamp;
    recompute_log_visible(app);
}

/// Move the viewed file within `log_files` (`delta` > 0 goes older).
fn switch_log_file(app: &mut MonitorApp, delta: isize) {
    if app.log_files.is_empty() {
        return;
    }
    let last = app.log_files.len() - 1;
    let target = app.log_file_index.saturating_add_signed(delta).min(last);
    if target == app.log_file_index {
        app.set_status(if delta > 0 {
            "Oldest log file"
        } else {
            "Newest log file"
        });
        return;
    }
    app.log_file_index = target;
    app.log_follow = true;
    app.log_loaded = None;
    load_active_log(app);
    let name = app.log_file_name.clone().unwrap_or_default();
    app.set_status(format!(
        "Log file: {name} [{}/{}]",
        app.log_file_index + 1,
        app.log_files.len()
    ));
}

/// Handle a key while the Logs target picker is open. Consumes every key.
pub(crate) fn handle_log_target_picker_key(app: &mut MonitorApp, key: KeyCode) {
    let Some(picker) = app.log_target_picker.as_mut() else {
        return;
    };
    match key {
        KeyCode::Esc => {
            app.log_target_picker = None;
        }
        KeyCode::Down => {
            let last = picker.matches.len().saturating_sub(1);
            picker.selected = (picker.selected + 1).min(last);
        }
        KeyCode::Up => {
            picker.selected = picker.selected.saturating_sub(1);
        }
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
            app.log_target_picker = None;
            if let Some(target) = chosen {
                app.log_target_filter = Some(target.clone());
                app.log_follow = true;
                recompute_log_visible(app);
                app.set_status(format!(
                    "Target filter: {target} ({} of {} lines)",
                    app.log_visible.len(),
                    app.log_lines.len(),
                ));
            }
        }
        _ => {}
    }
}

/// Handle a key while the Logs query is being edited. Consumes every key so a
/// typed `q` can't quit the monitor mid-search.
pub(crate) fn handle_log_query_key(app: &mut MonitorApp, key: KeyCode) {
    match key {
        KeyCode::Char(c) => {
            app.log_query.push(c);
            recompute_log_visible(app);
        }
        KeyCode::Backspace => {
            app.log_query.pop();
            recompute_log_visible(app);
        }
        KeyCode::Enter => {
            app.log_query_editing = false;
            let status = if app.log_query.is_empty() {
                "Search cleared".to_string()
            } else {
                format!(
                    "Search: /{} ({} matches)",
                    app.log_query,
                    app.log_visible.len()
                )
            };
            app.set_status(status);
        }
        KeyCode::Esc => {
            app.log_query_editing = false;
            app.log_query.clear();
            app.log_follow = true;
            recompute_log_visible(app);
            app.set_status("Search cleared");
        }
        _ => {}
    }
}

/// Handle a key while the Logs section is active. Returns `true` if the key
/// was consumed (so the generic dispatcher should not also act on it).
pub(crate) fn handle_logs_key(app: &mut MonitorApp, key: KeyCode) -> bool {
    handle_logs_filter_key(app, key) || handle_logs_nav_key(app, key)
}

/// Filter, file, and tail-size keys for the Logs tab.
fn handle_logs_filter_key(app: &mut MonitorApp, key: KeyCode) -> bool {
    match key {
        KeyCode::Char('/') => {
            app.log_query_editing = true;
            app.log_follow = false;
            true
        }
        KeyCode::Char('l') => {
            app.log_level_filter = app.log_level_filter.next();
            recompute_log_visible(app);
            app.set_status(format!(
                "Level filter: {} ({} of {} lines)",
                app.log_level_filter.label(),
                app.log_visible.len(),
                app.log_lines.len(),
            ));
            true
        }
        KeyCode::Char('f') => {
            app.log_target_picker = Some(TargetPickerState::new(&app.log_lines));
            true
        }
        KeyCode::Char('[') => {
            switch_log_file(app, 1);
            true
        }
        KeyCode::Char(']') => {
            switch_log_file(app, -1);
            true
        }
        KeyCode::Char('L') => {
            let next = LOG_TAIL_STEPS
                .iter()
                .find(|s| **s > app.log_tail_lines)
                .copied()
                .unwrap_or(LOG_TAIL_STEPS[0]);
            app.log_tail_lines = next;
            app.log_follow = true;
            app.log_loaded = None;
            load_active_log(app);
            app.set_status(format!(
                "Tail size: {next} ({} lines loaded)",
                app.log_lines.len()
            ));
            true
        }
        KeyCode::Esc => {
            let had_filter = app.log_level_filter != LevelFilter::All
                || !app.log_query.is_empty()
                || app.log_target_filter.is_some();
            if had_filter {
                app.log_level_filter = LevelFilter::All;
                app.log_query.clear();
                app.log_target_filter = None;
                app.log_follow = true;
                recompute_log_visible(app);
                app.set_status("Filters cleared");
            }
            true
        }
        _ => false,
    }
}

/// Navigation keys for the Logs tab, operating on `log_visible` positions.
fn handle_logs_nav_key(app: &mut MonitorApp, key: KeyCode) -> bool {
    let total = app.log_visible.len();
    match key {
        KeyCode::Char('j') | KeyCode::Down => {
            if total > 0 && app.log_selected + 1 < total {
                app.log_selected += 1;
                if app.log_selected + 1 == total {
                    app.log_follow = true;
                }
                ensure_log_selected_visible(app);
            }
            true
        }
        KeyCode::Char('k') | KeyCode::Up => {
            if app.log_selected > 0 {
                app.log_selected -= 1;
                app.log_follow = false;
                ensure_log_selected_visible(app);
            }
            true
        }
        KeyCode::PageDown => {
            if total > 0 {
                let page = log_page_size(app);
                app.log_selected = (app.log_selected + page).min(total - 1);
                if app.log_selected + 1 == total {
                    app.log_follow = true;
                }
                ensure_log_selected_visible(app);
            }
            true
        }
        KeyCode::PageUp => {
            let page = log_page_size(app);
            app.log_selected = app.log_selected.saturating_sub(page);
            app.log_follow = false;
            ensure_log_selected_visible(app);
            true
        }
        KeyCode::Home => {
            app.log_selected = 0;
            app.log_follow = false;
            ensure_log_selected_visible(app);
            true
        }
        KeyCode::Char('G') | KeyCode::End => {
            if total > 0 {
                app.log_selected = total - 1;
                app.log_follow = true;
                let page = log_page_size(app);
                app.log_offset = total.saturating_sub(page);
            }
            true
        }
        KeyCode::Enter => {
            if total > 0 {
                app.log_overlay_open = true;
            }
            true
        }
        _ => false,
    }
}

pub(crate) fn handle_log_overlay_key(app: &mut MonitorApp, key: KeyCode) {
    match key {
        KeyCode::Esc | KeyCode::Char('q') | KeyCode::Enter => {
            app.log_overlay_open = false;
        }
        KeyCode::Char('y') => copy_selected_log_entry(app),
        _ => {}
    }
}

fn copy_selected_log_entry(app: &mut MonitorApp) {
    if let Some(line) = app.selected_log_line().cloned() {
        copy_text(app, &line, "Copied log entry");
    }
}

pub(crate) fn render_logs(f: &mut Frame, app: &MonitorApp, area: Rect) {
    if app.log_lines.is_empty() {
        let msg = match &app.log_file_name {
            Some(name) => format!(" {name} is empty"),
            None => " No log files found in ~/.zdx/logs".to_string(),
        };
        let p = Paragraph::new(msg)
            .style(Style::default().fg(Color::DarkGray))
            .block(Block::default().borders(Borders::ALL).title(" Logs "));
        f.render_widget(p, area);
        return;
    }

    let total = app.log_visible.len();
    if total == 0 {
        // The active filters are already spelled out in the block title.
        let msg = format!(
            " No lines match · {} tailed lines · Esc clear · l level",
            app.log_lines.len(),
        );
        let p = Paragraph::new(msg)
            .style(Style::default().fg(Color::DarkGray))
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(log_title(app, 0, 0)),
            );
        f.render_widget(p, area);
        return;
    }

    let inner_width = area.width.saturating_sub(2) as usize;
    let visible_rows = area.height.saturating_sub(2) as usize;
    // Re-clamp against the *actual* rendered area: `terminal_height` is updated
    // post-draw, so the stored offset may lag by one frame.
    let (selected, offset) = clamp_log_view(
        total,
        app.log_follow,
        visible_rows,
        app.log_selected,
        app.log_offset,
    );
    let end = (offset + visible_rows).min(total);

    let items: Vec<ListItem> = app.log_visible[offset..end]
        .iter()
        .enumerate()
        .map(|(i, &raw_index)| {
            let visible_index = offset + i;
            let raw = &app.log_lines[raw_index];
            let spans = truncate_spans(log_line_spans(raw), inner_width);
            let item = ListItem::new(Line::from(spans));
            if visible_index == selected {
                item.style(Style::default().bg(Color::DarkGray))
            } else {
                item
            }
        })
        .collect();

    let list = List::new(items).block(Block::default().borders(Borders::ALL).title(log_title(
        app,
        selected + 1,
        total,
    )));
    f.render_widget(list, area);
}

fn log_title(app: &MonitorApp, pos: usize, total: usize) -> String {
    use std::fmt::Write as _;

    let file_label = app.log_file_name.as_deref().unwrap_or("(no file)");
    let mut title = format!(" Logs ({file_label}");
    if app.log_files.len() > 1 {
        let _ = write!(
            title,
            " [{}/{}]",
            app.log_file_index + 1,
            app.log_files.len()
        );
    }
    let _ = write!(title, " · {pos}/{total}");
    if app.log_level_filter != crate::log_line::LevelFilter::All {
        let _ = write!(title, " · lvl={}", app.log_level_filter.label());
    }
    if let Some(target) = &app.log_target_filter {
        let _ = write!(title, " · @{target}");
    }
    if app.log_query_editing {
        let _ = write!(title, " · /{}\u{2588}", app.log_query);
    } else if !app.log_query.is_empty() {
        let _ = write!(title, " · /{}", app.log_query);
    }
    if app.log_tail_lines != 500 {
        let _ = write!(title, " · tail={}", app.log_tail_lines);
    }
    if app.log_follow {
        title.push_str(" · FOLLOW");
    }
    title.push_str(") ");
    title
}

pub(crate) fn render_log_overlay(f: &mut Frame, app: &MonitorApp, area: Rect) {
    let Some(line) = app.selected_log_line() else {
        return;
    };

    let popup_area = centered_rect(80, 60, area);
    f.render_widget(Clear, popup_area);

    let title = format!(
        " Log entry [{pos}/{total}] · Esc close · y copy ",
        pos = app.log_selected + 1,
        total = app.log_visible.len(),
    );

    let body = Paragraph::new(Line::from(log_line_spans(line)))
        .wrap(Wrap { trim: false })
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Cyan))
                .title(title),
        );
    f.render_widget(body, popup_area);
}

/// Build colored spans for a single log line.
///
/// Coloring:
/// - timestamp: dark gray
/// - level: ERROR=red+bold, WARN=yellow+bold, INFO=green+bold, DEBUG=cyan, TRACE=magenta
/// - span scope (`run_turn:execute_tool:`): blue
/// - target (`module::path:`): cyan
/// - message: red for ERROR, dark gray for DEBUG/TRACE, default otherwise
fn log_line_spans(line: &str) -> Vec<Span<'static>> {
    let parts = parse_log_line(line);
    if !parts.structured {
        let style = match parts.level {
            "ERROR" => Style::default().fg(Color::Red),
            "WARN" => Style::default().fg(Color::Yellow),
            "DEBUG" | "TRACE" => Style::default().fg(Color::DarkGray),
            _ => Style::default(),
        };
        return vec![Span::styled(line.to_string(), style)];
    }

    let level_style = match parts.level {
        "ERROR" => Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        "WARN" => Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD),
        "INFO" => Style::default()
            .fg(Color::Green)
            .add_modifier(Modifier::BOLD),
        "DEBUG" => Style::default().fg(Color::Cyan),
        "TRACE" => Style::default().fg(Color::Magenta),
        _ => Style::default(),
    };
    let message_style = match parts.level {
        "ERROR" => Style::default().fg(Color::Red),
        "DEBUG" | "TRACE" => Style::default().fg(Color::DarkGray),
        _ => Style::default(),
    };

    let mut out = vec![
        Span::styled(
            parts.timestamp.to_string(),
            Style::default().fg(Color::DarkGray),
        ),
        Span::raw(" "),
        Span::styled(parts.level.to_string(), level_style),
        Span::raw(" "),
    ];
    if !parts.spans.is_empty() {
        out.push(Span::styled(
            parts.spans.to_string(),
            Style::default().fg(Color::Blue),
        ));
        out.push(Span::raw(" "));
    }
    out.push(Span::styled(
        parts.target.to_string(),
        Style::default().fg(Color::Cyan),
    ));
    out.push(Span::raw(" "));
    out.push(Span::styled(parts.message.to_string(), message_style));
    out
}

/// Truncate a span sequence to `max_chars` total characters, replacing the
/// overflow with `…`. Preserves per-span styling.
fn truncate_spans(spans: Vec<Span<'static>>, max_chars: usize) -> Vec<Span<'static>> {
    if max_chars == 0 {
        return Vec::new();
    }
    let total: usize = spans.iter().map(|s| s.content.chars().count()).sum();
    if total <= max_chars {
        return spans;
    }
    let limit = max_chars.saturating_sub(1); // reserve 1 char for the ellipsis
    let mut out: Vec<Span<'static>> = Vec::with_capacity(spans.len() + 1);
    let mut used = 0usize;
    for span in spans {
        let span_len = span.content.chars().count();
        if used + span_len <= limit {
            used += span_len;
            out.push(span);
        } else {
            let take = limit.saturating_sub(used);
            if take > 0 {
                let truncated: String = span.content.chars().take(take).collect();
                out.push(Span::styled(truncated, span.style));
            }
            break;
        }
    }
    out.push(Span::styled(
        "…".to_string(),
        Style::default().fg(Color::DarkGray),
    ));
    out
}

#[cfg(test)]
mod log_view_tests {
    use super::*;
    use crate::app::restart_force_for_key;

    fn sample_lines() -> Vec<String> {
        [
            "2026-07-31T10:00:00Z  INFO zdx_bot::bot: Accepted message chat_id=1",
            "2026-07-31T10:00:01Z DEBUG run_turn_inner: zdx_engine::core::agent: Turn start",
            "2026-07-31T10:00:02Z  WARN run_turn_inner:execute_tool: zdx_engine::tools: Tool failed tool=Bash",
            "2026-07-31T10:00:03Z ERROR zdx_engine::core::agent: Turn aborted",
        ]
        .iter()
        .map(|s| (*s).to_string())
        .collect()
    }

    #[test]
    fn restart_keys_distinguish_guarded_and_forced_modes() {
        assert!(!restart_force_for_key(KeyCode::Char('r')));
        assert!(restart_force_for_key(KeyCode::Char('R')));
    }

    #[test]
    fn level_filter_selects_expected_indices() {
        let lines = sample_lines();
        assert_eq!(
            visible_log_indices(&lines, LevelFilter::All, "", None),
            vec![0, 1, 2, 3]
        );
        assert_eq!(
            visible_log_indices(&lines, LevelFilter::Info, "", None),
            vec![0, 2, 3]
        );
        assert_eq!(
            visible_log_indices(&lines, LevelFilter::Error, "", None),
            vec![3]
        );
    }

    #[test]
    fn query_is_case_insensitive_and_combines_with_level() {
        let lines = sample_lines();
        assert_eq!(
            visible_log_indices(&lines, LevelFilter::All, "BASH", None),
            vec![2]
        );
        assert_eq!(
            visible_log_indices(&lines, LevelFilter::Error, "bash", None),
            Vec::<usize>::new()
        );
        // Span scope is part of the raw line, so it is searchable.
        assert_eq!(
            visible_log_indices(&lines, LevelFilter::All, "run_turn_inner", None),
            vec![1, 2]
        );
    }

    #[test]
    fn target_filter_narrows_to_a_subsystem() {
        let lines = sample_lines();
        assert_eq!(
            visible_log_indices(&lines, LevelFilter::All, "", Some("zdx_engine")),
            vec![1, 2, 3]
        );
        assert_eq!(
            visible_log_indices(&lines, LevelFilter::All, "", Some("zdx_bot")),
            vec![0]
        );
        assert_eq!(
            visible_log_indices(&lines, LevelFilter::Error, "", Some("zdx_engine")),
            vec![3]
        );
    }

    #[test]
    fn target_picker_lists_distinct_targets_by_frequency() {
        let picker = TargetPickerState::new(&sample_lines());
        assert_eq!(
            picker.items,
            vec![
                ("zdx_engine::core::agent".to_string(), 2),
                ("zdx_bot::bot".to_string(), 1),
                ("zdx_engine::tools".to_string(), 1),
            ]
        );
        assert_eq!(picker.selected_target(), Some("zdx_engine::core::agent"));
    }

    #[test]
    fn target_picker_filters_by_typed_text() {
        let mut picker = TargetPickerState::new(&sample_lines());
        picker.filter = "tools".to_string();
        picker.recompute();
        assert_eq!(picker.matches.len(), 1);
        assert_eq!(picker.selected_target(), Some("zdx_engine::tools"));
    }

    #[test]
    fn only_rolling_zdx_log_files_are_listed() {
        assert!(is_zdx_log_name("zdx.log"));
        assert!(is_zdx_log_name("zdx.log.2026-07-31"));
        assert!(!is_zdx_log_name("automations-daemon.log"));
        assert!(!is_zdx_log_name("zdx-bot.log"));
    }

    #[test]
    fn tail_window_scales_with_the_requested_line_count() {
        use std::fmt::Write as _;

        // ~195 B/line × 3000 lines ≈ 570 KiB. The default tail's window is
        // 256 KiB (~1340 of these lines), so asking for 2000 lines can only
        // succeed if the window itself widened.
        let dir = std::env::temp_dir().join(format!("zdx-monitor-tail-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("zdx.log.2026-07-31");
        let mut body = String::new();
        for i in 0..3000 {
            writeln!(
                &mut body,
                "2026-07-31T10:00:00Z  INFO zdx_engine::core::agent: line {i} {}",
                "x".repeat(130)
            )
            .unwrap();
        }
        fs::write(&path, &body).unwrap();
        assert!(body.len() > 2 * 256 * 1024, "fixture must exceed the floor");

        let small = tail_lines(&path, 500).unwrap();
        assert_eq!(small.len(), 500);
        assert!(small.last().unwrap().contains("line 2999"));

        let large = tail_lines(&path, 2000).unwrap();
        assert_eq!(large.len(), 2000);
        assert!(large.first().unwrap().contains("line 1000"));

        // A tail larger than the file yields the whole file.
        let all = tail_lines(&path, 10000).unwrap();
        assert_eq!(all.len(), 3000);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn clamp_follows_last_entry() {
        assert_eq!(clamp_log_view(10, true, 4, 0, 0), (9, 6));
        // Fewer entries than a page: offset stays at the top.
        assert_eq!(clamp_log_view(3, true, 10, 0, 0), (2, 0));
    }

    #[test]
    fn clamp_handles_shrinking_visible_set() {
        // Selection past the new end is pulled back into range.
        assert_eq!(clamp_log_view(3, false, 10, 42, 40), (2, 0));
        // Empty result resets both.
        assert_eq!(clamp_log_view(0, false, 10, 42, 40), (0, 0));
    }

    #[test]
    fn clamp_keeps_page_full_after_a_filter_narrows_the_list() {
        // Offset left over from a 343-line list must not strand the selection
        // alone on the top row of a 47-line filtered list.
        assert_eq!(clamp_log_view(47, false, 40, 46, 303), (46, 7));
    }

    #[test]
    fn clamp_scrolls_offset_to_keep_selection_visible() {
        assert_eq!(clamp_log_view(100, false, 5, 20, 0), (20, 16));
        assert_eq!(clamp_log_view(100, false, 5, 3, 10), (3, 3));
        assert_eq!(clamp_log_view(100, false, 5, 12, 10), (12, 10));
    }
}
