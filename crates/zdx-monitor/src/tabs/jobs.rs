use std::path::{Path, PathBuf};

use chrono::{DateTime, Local, Utc};
use crossterm::event::KeyCode;
use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap};
use zdx_engine::config::paths;
use zdx_engine::job_daemon::{
    self, DaemonContext, DaemonContextSource, DaemonContextState, DaemonJobs,
};
use zdx_engine::jobs::{
    self, ArchivedJob, JobOverview, JobSchedule, JobStatus, JobsHome, ModelEdit, Outcome,
    SettingOrigin,
};

use crate::app::MonitorApp;
use crate::tabs::config::{ModelFieldKind, ModelPickerState, PickerTarget};
use crate::ui::{SELECTED_BG, truncate_chars};

/// Jobs resolved the way the scheduled daemon resolves them, through
/// the shared engine view. Never uses the Monitor's own root. Reads the
/// daemon record, service plist, config, and subagent files, so call it on
/// tab entry, not on the refresh tick.
pub(crate) fn load_jobs() -> DaemonJobs {
    job_daemon::jobs()
}

/// Replaces the tab's snapshot, keeping the selection on `keep` (by name)
/// when it is still listed.
pub(crate) fn apply_snapshot(app: &mut MonitorApp, snapshot: DaemonJobs, keep: Option<&str>) {
    app.jobs = snapshot.jobs;
    app.jobs_daemon = Some(snapshot.daemon);
    app.jobs_error = snapshot.error;
    let last = app.jobs.len().saturating_sub(1);
    app.selected_index = keep
        .and_then(|name| app.jobs.iter().position(|item| item.name == name))
        .unwrap_or(app.selected_index)
        .min(last);
}

/// Jobs-tab keys: `Enter` edits the selected job's model in the shared
/// model picker, `d`/`Delete` removes its model override, `a` switches
/// between jobs and archived once jobs (read-only). Returns whether the key
/// was handled.
pub(crate) fn handle_jobs_key(app: &mut MonitorApp, key: KeyCode) -> bool {
    match key {
        KeyCode::Char('a') => toggle_archived(app, || JobsHome::current().archived()),
        KeyCode::Enter | KeyCode::Char('d') | KeyCode::Delete if app.jobs_archived.is_some() => {
            app.set_status("Archived jobs are read-only; press a to go back".to_string());
        }
        KeyCode::Enter => open_job_model_picker(app),
        KeyCode::Char('d') | KeyCode::Delete => clear_selected_model(app),
        _ => return false,
    }
    true
}

/// Shows the archive (loaded now, through the shared engine listing) or
/// goes back to the jobs list.
fn toggle_archived(app: &mut MonitorApp, load: impl FnOnce() -> anyhow::Result<Vec<ArchivedJob>>) {
    app.selected_index = 0;
    app.jobs_archived = match app.jobs_archived {
        Some(_) => None,
        None => Some(load().map_err(|err| format!("{err:#}"))),
    };
}

/// Opens the Config tab's model picker for the selected job, seeded
/// with the model the job resolves to now.
pub(crate) fn open_job_model_picker(app: &mut MonitorApp) {
    let Some(item) = app.jobs.get(app.selected_index) else {
        return;
    };
    let unknown = DaemonContextState::Unavailable {
        reason: "daemon settings not checked".to_string(),
    };
    let daemon = app.jobs_daemon.as_ref().unwrap_or(&unknown);
    let config = match job_daemon::job_config(daemon, item) {
        Ok(config) => config,
        Err(err) => {
            let message = format!("Cannot edit {}: {err:#}", item.name);
            app.set_status(message);
            return;
        }
    };
    let current = item
        .settings
        .as_ref()
        .map(|settings| settings.model.spec.clone())
        .or_else(|| item.declared_model.clone())
        .unwrap_or_default();
    let picker = ModelPickerState::new(
        format!("job {}", item.name),
        ModelFieldKind::Chat,
        &current,
        &config.providers,
        &config.model_modes,
        PickerTarget::Job {
            name: item.name.clone(),
            path: item.path.clone(),
            declared: item.declared_model.clone(),
        },
    );
    app.model_picker = Some(picker);
}

/// Saves the picker's choice as the job's `model:`.
pub(crate) fn save_picked_model(app: &mut MonitorApp, spec: &str) {
    save_model_with(app, ModelEdit::Set(spec), load_jobs);
}

fn clear_selected_model(app: &mut MonitorApp) {
    let Some(item) = app.jobs.get(app.selected_index) else {
        return;
    };
    if item.declared_model.is_none() {
        let message = format!("{} has no model override; it already inherits", item.name);
        app.set_status(message);
        return;
    }
    let target = PickerTarget::Job {
        name: item.name.clone(),
        path: item.path.clone(),
        declared: item.declared_model.clone(),
    };
    edit_model_with(app, &target, ModelEdit::Clear, load_jobs);
}

fn save_model_with(app: &mut MonitorApp, edit: ModelEdit<'_>, load: impl FnOnce() -> DaemonJobs) {
    let Some(target) = app.model_picker.take().map(|picker| picker.target) else {
        return;
    };
    edit_model_with(app, &target, edit, load);
}

/// Writes `edit` through the shared engine edit, then reloads the daemon
/// view. Errors leave the file unchanged and are shown in the status line.
fn edit_model_with(
    app: &mut MonitorApp,
    target: &PickerTarget,
    edit: ModelEdit<'_>,
    load: impl FnOnce() -> DaemonJobs,
) {
    let PickerTarget::Job {
        name,
        path,
        declared,
    } = target
    else {
        return;
    };
    let result = jobs::set_model(path, Some(declared.as_deref()), edit);
    apply_snapshot(app, load(), Some(name));
    let message = match result {
        Ok(_) => {
            let action = match edit {
                ModelEdit::Set(spec) => format!("Set {name} model to {spec}"),
                ModelEdit::Clear => format!("Cleared {name} model override"),
            };
            let now = app
                .jobs
                .iter()
                .find(|item| &item.name == name)
                .map(|item| match &item.settings {
                    Some(settings) => format!(
                        " · now {} ({})",
                        settings.model.spec,
                        origin_label(settings.model.origin)
                    ),
                    None => format!(
                        " · unresolved: {}",
                        item.error.as_deref().unwrap_or("unknown error")
                    ),
                })
                .unwrap_or_default();
            format!("{action}{now}")
        }
        Err(err) => format!("Failed to change {name} model (file unchanged): {err:#}"),
    };
    app.set_status(message);
}

struct JobsView<'a> {
    items: &'a [JobOverview],
    error: Option<&'a str>,
    selected: usize,
    daemon: Option<&'a DaemonContextState>,
    home: Option<&'a Path>,
}

pub(crate) fn render_jobs(f: &mut Frame, app: &MonitorApp, area: Rect) {
    let home = paths::home_dir();
    if let Some(archived) = &app.jobs_archived {
        render_archived(f, archived, app.selected_index, home.as_deref(), area);
        return;
    }
    let view = JobsView {
        items: &app.jobs,
        error: app.jobs_error.as_deref(),
        selected: app.selected_index,
        daemon: app.jobs_daemon.as_ref(),
        home: home.as_deref(),
    };
    render_view(f, &view, area);
}

/// Short daemon-context label for the list title.
fn daemon_title(daemon: Option<&DaemonContextState>, home: Option<&Path>) -> String {
    match daemon {
        Some(DaemonContextState::Available { context }) => format!(
            "{} · root {}",
            source_label(context),
            display_path(&context.root, home)
        ),
        Some(DaemonContextState::Unavailable { .. }) => "daemon settings unavailable".to_string(),
        None => "daemon not checked".to_string(),
    }
}

fn source_label(context: &DaemonContext) -> &'static str {
    match context.source {
        DaemonContextSource::Running => "running daemon",
        DaemonContextSource::Installed => "installed daemon (next start)",
    }
}

/// Explains where inherited values come from, for the detail pane.
fn daemon_note(daemon: Option<&DaemonContextState>, home: Option<&Path>) -> String {
    match daemon {
        Some(DaemonContextState::Available { context }) => {
            let mut text = format!(
                "Inherited values come from the {} (root {}, config from {}).",
                source_label(context),
                display_path(&context.root, home),
                display_path(&context.config_dir, home)
            );
            if let Some(note) = &context.note {
                text.push(' ');
                text.push_str(note);
                text.push('.');
            }
            text
        }
        Some(DaemonContextState::Unavailable { reason }) => {
            format!("Daemon settings unavailable: {reason}.")
        }
        None => "Daemon settings not checked yet.".to_string(),
    }
}

/// Rows below which the detail pane is dropped so the list stays usable.
const MIN_HEIGHT_FOR_DETAIL: u16 = 14;
const MAX_DETAIL_HEIGHT: u16 = 13;
const LABEL_WIDTH: usize = 10;

fn render_view(f: &mut Frame, view: &JobsView, area: Rect) {
    let detail_height = if area.height >= MIN_HEIGHT_FOR_DETAIL && !view.items.is_empty() {
        (area.height / 2).min(MAX_DETAIL_HEIGHT)
    } else {
        0
    };
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(3), Constraint::Length(detail_height)])
        .split(area);

    let title = if area.width >= 60 {
        format!(
            " Jobs · * inherited · {} ",
            daemon_title(view.daemon, view.home)
        )
    } else {
        " Jobs · * inherited ".to_string()
    };
    let block = Block::default().borders(Borders::ALL).title(title);

    if let Some(error) = view.error {
        let text = Paragraph::new(format!("error: {error}"))
            .style(Style::default().fg(Color::Red))
            .wrap(Wrap { trim: false })
            .block(block);
        f.render_widget(text, chunks[0]);
        return;
    }
    if view.items.is_empty() {
        let text = Paragraph::new("No jobs found.")
            .style(Style::default().fg(Color::DarkGray))
            .block(block);
        f.render_widget(text, chunks[0]);
        return;
    }

    let width = chunks[0].width.saturating_sub(2) as usize;
    let items: Vec<ListItem> = view
        .items
        .iter()
        .enumerate()
        .map(|(i, overview)| {
            let item = ListItem::new(row_line(overview, width, view.home));
            if i == view.selected {
                item.style(Style::default().bg(SELECTED_BG))
            } else {
                item
            }
        })
        .collect();
    let mut state = ListState::default().with_selected(Some(view.selected));
    f.render_stateful_widget(List::new(items).block(block), chunks[0], &mut state);

    if detail_height > 0
        && let Some(overview) = view.items.get(view.selected)
    {
        let detail = Paragraph::new(detail_lines(overview, view.daemon, view.home))
            .wrap(Wrap { trim: false })
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(" Resolved now · not a past run "),
            );
        f.render_widget(detail, chunks[1]);
    }
}

fn render_archived(
    f: &mut Frame,
    archived: &Result<Vec<ArchivedJob>, String>,
    selected: usize,
    home: Option<&Path>,
    area: Rect,
) {
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" Archived once jobs · newest first · a back ");
    let items = match archived {
        Err(error) => {
            let text = Paragraph::new(format!("error: {error}"))
                .style(Style::default().fg(Color::Red))
                .wrap(Wrap { trim: false })
                .block(block);
            f.render_widget(text, area);
            return;
        }
        Ok(items) if items.is_empty() => {
            let text = Paragraph::new("No archived jobs.")
                .style(Style::default().fg(Color::DarkGray))
                .block(block);
            f.render_widget(text, area);
            return;
        }
        Ok(items) => items,
    };
    let detail_height = if area.height >= MIN_HEIGHT_FOR_DETAIL {
        8
    } else {
        0
    };
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(3), Constraint::Length(detail_height)])
        .split(area);
    let width = chunks[0].width.saturating_sub(2) as usize;
    let rows: Vec<ListItem> = items
        .iter()
        .enumerate()
        .map(|(i, item)| {
            let row = ListItem::new(archived_row(item, width));
            if i == selected {
                row.style(Style::default().bg(SELECTED_BG))
            } else {
                row
            }
        })
        .collect();
    let mut state = ListState::default().with_selected(Some(selected));
    f.render_stateful_widget(List::new(rows).block(block), chunks[0], &mut state);

    if detail_height > 0
        && let Some(item) = items.get(selected)
    {
        let mut lines = vec![labeled(
            "file",
            vec![Span::raw(display_path(&item.path, home))],
        )];
        if let Some(last) = &item.last_run {
            lines.push(labeled(
                "finished",
                vec![Span::raw(format!(
                    "{} ({})",
                    local_time(last.finished_at),
                    last.trigger.as_str()
                ))],
            ));
            if let Some(thread) = &last.thread_id {
                lines.push(labeled("thread", vec![Span::raw(thread.clone())]));
            }
            if let Some(error) = &last.error {
                lines.push(labeled(
                    "failure",
                    vec![Span::styled(error.clone(), Style::default().fg(Color::Red))],
                ));
            }
        }
        let detail = Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .block(Block::default().borders(Borders::ALL));
        f.render_widget(detail, chunks[1]);
    }
}

/// `name  outcome  run_at` for one archived job.
fn archived_row(item: &ArchivedJob, width: usize) -> Line<'static> {
    let name_width = (width * 2 / 5).clamp(1, 28);
    let (outcome, style) = match &item.last_run {
        Some(last) if last.outcome == Outcome::Succeeded => {
            (last.outcome.as_str(), Style::default().fg(Color::DarkGray))
        }
        Some(last) => (last.outcome.as_str(), Style::default().fg(Color::Red)),
        None => ("unknown", Style::default().fg(Color::Yellow)),
    };
    let when = item
        .scheduled_for
        .map_or_else(|| "unknown run_at".to_string(), local_time);
    let rest = width.saturating_sub(1 + name_width + 2 + 13);
    Line::from(vec![
        Span::raw(format!(
            " {}  ",
            pad(&truncate_chars(&item.name, name_width), name_width)
        )),
        Span::styled(format!("{}  ", pad(outcome, 11)), style),
        Span::styled(
            truncate_chars(&when, rest),
            Style::default().fg(Color::DarkGray),
        ),
    ])
}

/// One list row, laid out for `width` columns: name, then status, schedule,
/// model, and root as space allows. Inherited values end in `*`.
fn row_line(overview: &JobOverview, width: usize, home: Option<&Path>) -> Line<'static> {
    let name_width = if width >= 60 {
        22
    } else {
        (width * 2 / 5).max(1)
    };
    let status_width = if width >= 50 { 11 } else { 0 };
    let schedule_width = if width >= 80 { 14 } else { 0 };
    let column = |width: usize| if width > 0 { width + 2 } else { 0 };
    let rest =
        width.saturating_sub(1 + name_width + 2 + column(status_width) + column(schedule_width));

    let mut spans = vec![Span::raw(format!(
        " {}  ",
        pad(&truncate_chars(&overview.name, name_width), name_width)
    ))];
    if status_width > 0 {
        let (status, style) = status_label(overview);
        spans.push(Span::styled(
            format!(
                "{}  ",
                pad(&truncate_chars(&status, status_width), status_width)
            ),
            style,
        ));
    }
    if schedule_width > 0 {
        spans.push(Span::styled(
            format!(
                "{}  ",
                pad(
                    &truncate_chars(&schedule_label(overview), schedule_width),
                    schedule_width
                )
            ),
            Style::default().fg(Color::DarkGray),
        ));
    }

    let Some(settings) = &overview.settings else {
        let error = overview.error.as_deref().unwrap_or("unresolved");
        spans.push(Span::styled(
            truncate_chars(&format!("error: {error}"), rest),
            Style::default().fg(Color::Red),
        ));
        return Line::from(spans);
    };

    let show_root = rest >= 50;
    let model_width = if show_root { rest * 11 / 20 } else { rest };
    let model = marked(
        &settings.model.spec,
        settings.model.origin.is_inherited(),
        model_width,
        false,
    );
    spans.push(Span::styled(
        if show_root {
            format!("{}  ", pad(&model, model_width))
        } else {
            model
        },
        origin_style(settings.model.origin),
    ));
    if show_root {
        let root_width = rest.saturating_sub(model_width + 2);
        spans.push(Span::styled(
            marked(
                &display_path(&settings.root.path, home),
                settings.root.origin.is_inherited(),
                root_width,
                true,
            ),
            origin_style(settings.root.origin),
        ));
    }
    Line::from(spans)
}

/// Detail of the selected job: declared and resolved settings with
/// provenance, or the resolution error.
fn detail_lines(
    overview: &JobOverview,
    daemon: Option<&DaemonContextState>,
    home: Option<&Path>,
) -> Vec<Line<'static>> {
    let mut lines = vec![
        labeled("file", vec![Span::raw(display_path(&overview.path, home))]),
        labeled("schedule", vec![Span::raw(schedule_text(overview))]),
        labeled("status", status_spans(overview)),
    ];
    if let Some(error) = overview
        .state
        .last_run
        .as_ref()
        .and_then(|last| last.error.as_ref())
    {
        lines.push(labeled(
            "failure",
            vec![Span::styled(error.clone(), Style::default().fg(Color::Red))],
        ));
    }

    let Some(settings) = &overview.settings else {
        lines.extend(unresolved_lines(overview));
        if overview.declared_root.is_none() {
            lines.push(note_line(daemon_note(daemon, home)));
        }
        return lines;
    };

    let model = &settings.model;
    lines.push(labeled(
        "model",
        vec![
            Span::styled(model.spec.clone(), origin_style(model.origin)),
            Span::styled(
                format!("  {}", origin_label(model.origin)),
                Style::default().fg(Color::DarkGray),
            ),
        ],
    ));
    let mut parts = vec![
        format!("provider {}", model.provider),
        format!("account {}", model.account.as_deref().unwrap_or("default")),
    ];
    if model.thinking_origin == model.origin {
        parts.push(format!("thinking {}", model.thinking));
    } else {
        parts.push(format!(
            "thinking {} ({})",
            model.thinking,
            origin_label(model.thinking_origin)
        ));
    }
    if model.fast {
        parts.push("fast".to_string());
    }
    lines.push(labeled(
        "",
        vec![Span::styled(
            parts.join(" · "),
            Style::default().fg(Color::DarkGray),
        )],
    ));

    let root = &settings.root;
    lines.push(labeled(
        "root",
        vec![
            Span::styled(display_path(&root.path, home), origin_style(root.origin)),
            Span::styled(
                format!("  {}", origin_label(root.origin)),
                Style::default().fg(Color::DarkGray),
            ),
        ],
    ));
    if let Some(subagent) = &settings.subagent {
        lines.push(labeled("subagent", vec![Span::raw(subagent.clone())]));
    }
    if root.origin.is_inherited() {
        lines.push(note_line(daemon_note(daemon, home)));
    }
    lines
}

fn note_line(text: String) -> Line<'static> {
    labeled(
        "note",
        vec![Span::styled(text, Style::default().fg(Color::Yellow))],
    )
}

fn schedule_text(overview: &JobOverview) -> String {
    let Some(schedule) = &overview.schedule else {
        return "invalid file".to_string();
    };
    let mut parts = vec![match schedule {
        JobSchedule::Manual => "manual only".to_string(),
        JobSchedule::Recurring { cron, .. } => format!("recurring {cron}"),
        JobSchedule::Once { run_at } => {
            format!("once at {}", local_time(run_at.with_timezone(&Utc)))
        }
    }];
    if let Some(grace) = overview.grace_secs {
        parts.push(format!("grace {grace}s"));
    }
    if let Some(timeout) = overview.timeout_secs {
        parts.push(format!("timeout {timeout}s"));
    }
    parts.push(format!("retries {}", overview.max_retries));
    parts.join(" · ")
}

/// Compact schedule for the list: the cron expression, `once MM-DD HH:MM`
/// in local time, or `manual`.
fn schedule_label(overview: &JobOverview) -> String {
    match &overview.schedule {
        None => "invalid".to_string(),
        Some(JobSchedule::Manual) => "manual".to_string(),
        Some(JobSchedule::Recurring { cron, .. }) => cron.clone(),
        Some(JobSchedule::Once { run_at }) => format!(
            "once {}",
            run_at.with_timezone(&Local).format("%m-%d %H:%M")
        ),
    }
}

/// Status word for the list, red when the job cannot run or its last run
/// did not succeed.
fn status_label(overview: &JobOverview) -> (String, Style) {
    let status = overview.state.status;
    let last_bad = overview.state.last_run.as_ref().is_some_and(|last| {
        matches!(
            last.outcome,
            Outcome::Failed | Outcome::Missed | Outcome::Interrupted
        )
    });
    let style = match status {
        JobStatus::Invalid | JobStatus::Expired | JobStatus::Interrupted => {
            Style::default().fg(Color::Red)
        }
        JobStatus::Running | JobStatus::Due => Style::default().fg(Color::Yellow),
        _ if last_bad => Style::default().fg(Color::Red),
        _ => Style::default().fg(Color::DarkGray),
    };
    let text = match (&overview.state.last_run, status) {
        (Some(last), JobStatus::Scheduled | JobStatus::Manual) if last_bad => {
            format!("{} ({})", status.as_str(), last.outcome.as_str())
        }
        _ => status.as_str().to_string(),
    };
    (text, style)
}

/// Status, next run, and last outcome for the detail pane.
fn status_spans(overview: &JobOverview) -> Vec<Span<'static>> {
    let (_, style) = status_label(overview);
    let mut spans = vec![Span::styled(
        overview.state.status.as_str().to_string(),
        style,
    )];
    let mut rest = Vec::new();
    if let Some(next) = overview.state.next_run_at {
        rest.push(format!("next {}", local_time(next)));
    }
    if let Some(last) = &overview.state.last_run {
        rest.push(format!(
            "last {} {} ({})",
            last.outcome.as_str(),
            local_time(last.finished_at),
            last.trigger.as_str()
        ));
    }
    if !rest.is_empty() {
        spans.push(Span::styled(
            format!(" · {}", rest.join(" · ")),
            Style::default().fg(Color::DarkGray),
        ));
    }
    spans
}

fn local_time(at: DateTime<Utc>) -> String {
    at.with_timezone(&Local)
        .format("%Y-%m-%d %H:%M")
        .to_string()
}

/// Declared values and the resolution error for a job whose
/// settings could not be resolved.
fn unresolved_lines(overview: &JobOverview) -> Vec<Line<'static>> {
    let declared = |value: Option<&String>, fallback: &str| {
        value.map_or_else(|| fallback.to_string(), |value| format!("declared {value}"))
    };
    let mut lines = vec![
        labeled(
            "model",
            vec![Span::raw(declared(
                overview.declared_model.as_ref(),
                "not set (inherits)",
            ))],
        ),
        labeled(
            "root",
            vec![Span::raw(declared(
                overview.declared_root.as_ref(),
                "not set (inherits daemon root)",
            ))],
        ),
    ];
    if let Some(subagent) = &overview.subagent {
        lines.push(labeled("subagent", vec![Span::raw(subagent.clone())]));
    }
    lines.push(labeled(
        "error",
        vec![Span::styled(
            overview
                .error
                .clone()
                .unwrap_or_else(|| "unresolved".to_string()),
            Style::default().fg(Color::Red),
        )],
    ));
    lines
}

fn labeled(label: &str, mut value: Vec<Span<'static>>) -> Line<'static> {
    let mut spans = vec![Span::styled(
        format!(" {label:<LABEL_WIDTH$}"),
        Style::default().fg(Color::DarkGray),
    )];
    spans.append(&mut value);
    Line::from(spans)
}

fn origin_label(origin: SettingOrigin) -> String {
    let source = origin.as_str().replace('_', " ");
    if origin.is_inherited() {
        format!("inherited from {source}")
    } else {
        format!("set by {source}")
    }
}

fn origin_style(origin: SettingOrigin) -> Style {
    if origin.is_inherited() {
        Style::default()
    } else {
        Style::default().fg(Color::Cyan)
    }
}

/// Fits `text` in `width` columns, ending in `*` when inherited. Paths keep
/// their tail (`from_start`), since the last components identify them.
fn marked(text: &str, inherited: bool, width: usize, from_start: bool) -> String {
    let budget = if inherited {
        width.saturating_sub(1)
    } else {
        width
    };
    let fitted = if from_start {
        truncate_start(text, budget)
    } else {
        truncate_chars(text, budget)
    };
    if inherited && width > 0 {
        format!("{fitted}*")
    } else {
        fitted
    }
}

fn pad(text: &str, width: usize) -> String {
    format!("{text:<width$}")
}

fn truncate_start(value: &str, max_chars: usize) -> String {
    let count = value.chars().count();
    if count <= max_chars {
        return value.to_string();
    }
    if max_chars == 0 {
        return String::new();
    }
    let tail: String = value.chars().skip(count - (max_chars - 1)).collect();
    format!("…{tail}")
}

fn display_path(path: &Path, home: Option<&Path>) -> String {
    match home.and_then(|home| path.strip_prefix(home).ok()) {
        Some(rest) if rest.as_os_str().is_empty() => "~".to_string(),
        Some(rest) => PathBuf::from("~").join(rest).display().to_string(),
        None => path.display().to_string(),
    }
}

#[cfg(test)]
mod tests {
    use ratatui::backend::TestBackend;
    use zdx_engine::jobs::{JobRunSettings, ResolvedJobModel, ResolvedJobRoot};

    use super::*;

    const HOME: &str = "/home/user";

    fn daemon(source: DaemonContextSource, note: Option<&str>) -> DaemonContextState {
        DaemonContextState::Available {
            context: DaemonContext {
                source,
                root: PathBuf::from(format!("{HOME}/workspace")),
                config_dir: PathBuf::from(format!("{HOME}/workspace")),
                pid: Some(42),
                started_at: None,
                note: note.map(str::to_string),
            },
        }
    }

    fn unavailable() -> DaemonContextState {
        DaemonContextState::Unavailable {
            reason: "daemon is not running and no daemon service is installed".to_string(),
        }
    }

    fn detail_text(item: &JobOverview, state: &DaemonContextState) -> String {
        detail_lines(item, Some(state), Some(Path::new(HOME)))
            .iter()
            .map(line_text)
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn line_text(line: &Line) -> String {
        line.spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect()
    }

    fn overview(
        name: &str,
        model_origin: SettingOrigin,
        thinking_origin: SettingOrigin,
        root_origin: SettingOrigin,
    ) -> JobOverview {
        JobOverview {
            name: name.to_string(),
            path: PathBuf::from(format!("{HOME}/.zdx/jobs/{name}.md")),
            schedule: Some(JobSchedule::parse(Some("0 9 * * *"), None).unwrap()),
            grace_secs: Some(600),
            state: jobs::JobState {
                status: JobStatus::Scheduled,
                next_run_at: None,
                last_run: None,
            },
            declared_root: None,
            declared_model: None,
            subagent: None,
            timeout_secs: Some(600),
            max_retries: 1,
            settings: Some(JobRunSettings {
                root: ResolvedJobRoot {
                    path: PathBuf::from(format!("{HOME}/projects/example")),
                    origin: root_origin,
                },
                model: ResolvedJobModel {
                    spec: "provider-a@work:model-x@high".to_string(),
                    provider: "provider-a".to_string(),
                    account: Some("work".to_string()),
                    model: "model-x".to_string(),
                    thinking: "high".to_string(),
                    fast: false,
                    origin: model_origin,
                    thinking_origin,
                },
                subagent: None,
            }),
            error: None,
        }
    }

    fn failed(name: &str) -> JobOverview {
        JobOverview {
            declared_root: Some("~/missing".to_string()),
            settings: None,
            error: Some("root '/home/user/missing' does not exist or is not a directory".into()),
            ..overview(
                name,
                SettingOrigin::Config,
                SettingOrigin::Config,
                SettingOrigin::Daemon,
            )
        }
    }

    #[test]
    fn wide_row_marks_inherited_values_only() {
        let home = Path::new(HOME);
        let explicit = overview(
            "daily-report",
            SettingOrigin::Job,
            SettingOrigin::Job,
            SettingOrigin::Job,
        );
        let text = line_text(&row_line(&explicit, 120, Some(home)));
        assert!(text.contains("daily-report"));
        assert!(text.contains("0 9 * * *"));
        assert!(text.contains("provider-a@work:model-x@high "));
        assert!(text.ends_with("~/projects/example"));

        let inherited = overview(
            "daily-report",
            SettingOrigin::Config,
            SettingOrigin::Config,
            SettingOrigin::Daemon,
        );
        let text = line_text(&row_line(&inherited, 120, Some(home)));
        assert!(text.contains("provider-a@work:model-x@high*"));
        assert!(text.ends_with("~/projects/example*"));
    }

    #[test]
    fn rows_show_status_once_schedule_and_failed_last_runs() {
        let home = Path::new(HOME);
        let mut once = overview(
            "renew-domain",
            SettingOrigin::Job,
            SettingOrigin::Job,
            SettingOrigin::Job,
        );
        once.schedule = Some(JobSchedule::parse(None, Some("2030-01-02T09:00:00-03:00")).unwrap());
        once.state.status = JobStatus::Pending;
        let text = line_text(&row_line(&once, 120, Some(home)));
        assert!(text.contains("pending"), "{text}");
        assert!(text.contains("once 01-0"), "{text}");

        let mut failing = overview(
            "weekly-digest",
            SettingOrigin::Job,
            SettingOrigin::Job,
            SettingOrigin::Job,
        );
        failing.state.last_run = Some(jobs::LastRun {
            run_id: "run-1".to_string(),
            trigger: jobs::RunTrigger::Daemon,
            outcome: Outcome::Failed,
            scheduled_for: None,
            finished_at: chrono::Utc::now(),
            error: Some("HTTP 401: Invalid API Key".to_string()),
            thread_id: None,
        });
        let (label, style) = status_label(&failing);
        assert_eq!(label, "scheduled (failed)");
        assert_eq!(style.fg, Some(Color::Red));
        let detail = detail_text(&failing, &daemon(DaemonContextSource::Running, None));
        assert!(detail.contains("last failed"), "{detail}");
        assert!(detail.contains("failure   HTTP 401"), "{detail}");

        let invalid = jobs::invalid_overview(&jobs::InvalidJob {
            name: "typo".to_string(),
            path: PathBuf::from(format!("{HOME}/.zdx/jobs/typo.md")),
            error: "unknown field `schedul`".to_string(),
        });
        let text = line_text(&row_line(&invalid, 120, Some(home)));
        assert!(text.contains("invalid"), "{text}");
        assert!(text.contains("error: unknown field"), "{text}");
    }

    #[test]
    fn error_row_shows_error_instead_of_settings() {
        let text = line_text(&row_line(&failed("broken"), 120, Some(Path::new(HOME))));
        assert!(text.contains("broken"));
        assert!(text.contains("error: root '/home/user/missing' does not exist"));
        assert!(!text.contains("provider-a"));
    }

    #[test]
    fn narrow_row_fits_width_and_keeps_model() {
        let item = overview(
            "a-very-long-job-name",
            SettingOrigin::Config,
            SettingOrigin::Config,
            SettingOrigin::Daemon,
        );
        for width in [20, 40, 69] {
            let text = line_text(&row_line(&item, width, Some(Path::new(HOME))));
            assert!(text.chars().count() <= width, "{width}: {text:?}");
            assert!(text.ends_with('*'), "{width}: {text:?}");
            assert!(!text.contains("~/projects"), "root hidden at {width}");
        }
    }

    #[test]
    fn detail_shows_provenance_and_daemon_source() {
        let item = overview(
            "daily-report",
            SettingOrigin::Job,
            SettingOrigin::Config,
            SettingOrigin::Daemon,
        );
        let text = detail_text(&item, &daemon(DaemonContextSource::Running, None));
        assert!(text.contains("provider-a@work:model-x@high  set by job"));
        assert!(text.contains("account work"));
        assert!(text.contains("thinking high (inherited from config)"));
        assert!(text.contains("~/projects/example  inherited from daemon"));
        assert!(text.contains("from the running daemon (root ~/workspace"));

        let text = detail_text(
            &item,
            &daemon(
                DaemonContextSource::Installed,
                Some("daemon is not running; showing the installed service"),
            ),
        );
        assert!(text.contains("installed daemon (next start)"));
        assert!(text.contains("daemon is not running"));

        let explicit_root = overview(
            "daily-report",
            SettingOrigin::SubagentOverride,
            SettingOrigin::SubagentOverride,
            SettingOrigin::Job,
        );
        let text = detail_text(&explicit_root, &daemon(DaemonContextSource::Running, None));
        assert!(text.contains("set by subagent override"));
        assert!(text.contains("thinking high"));
        assert!(!text.contains("thinking high ("));
        assert!(!text.contains("note"));
    }

    #[test]
    fn detail_shows_declared_values_and_error() {
        let text = detail_text(
            &failed("broken"),
            &daemon(DaemonContextSource::Running, None),
        );
        assert!(text.contains("not set (inherits)"));
        assert!(text.contains("declared ~/missing"));
        assert!(text.contains("error"));
        assert!(text.contains("does not exist"));
        assert!(!text.contains("note"), "declared root needs no daemon note");

        let inherited = JobOverview {
            declared_root: None,
            error: Some("daemon settings unavailable: daemon is not running".into()),
            ..failed("needs-daemon")
        };
        let text = detail_text(&inherited, &unavailable());
        assert!(text.contains("not set (inherits daemon root)"));
        assert!(text.contains("Daemon settings unavailable: daemon is not running"));
    }

    fn render_text(view: &JobsView, width: u16, height: u16) -> String {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|frame| render_view(frame, view, frame.area()))
            .unwrap();
        let buffer = terminal.backend().buffer();
        let mut text = String::new();
        for y in 0..buffer.area.height {
            for x in 0..buffer.area.width {
                text.push_str(buffer.cell((x, y)).unwrap().symbol());
            }
            text.push('\n');
        }
        text
    }

    #[test]
    fn renders_list_and_detail_at_small_width() {
        let running = daemon(DaemonContextSource::Running, None);
        let items = vec![
            overview(
                "daily-report",
                SettingOrigin::Job,
                SettingOrigin::Job,
                SettingOrigin::Job,
            ),
            failed("broken"),
        ];
        let view = JobsView {
            items: &items,
            error: None,
            selected: 1,
            daemon: Some(&running),
            home: Some(Path::new(HOME)),
        };
        let text = render_text(&view, 40, 20);
        assert!(text.contains("daily-report"));
        assert!(text.contains("not a past run"));
        assert!(text.contains("declared ~/missing"));
        assert!(text.contains("error"));

        let short = render_text(&view, 40, 8);
        assert!(short.contains("broken"));
        assert!(!short.contains("not a past run"));
    }

    #[test]
    fn renders_listing_error() {
        let view = JobsView {
            items: &[],
            error: Some("discover jobs: duplicate name"),
            selected: 0,
            daemon: None,
            home: None,
        };
        let text = render_text(&view, 80, 10);
        assert!(text.contains("error: discover jobs: duplicate name"));
    }

    #[test]
    fn title_names_the_daemon_context_not_the_monitor() {
        let home = Some(Path::new(HOME));
        let running = daemon(DaemonContextSource::Running, None);
        let installed = daemon(DaemonContextSource::Installed, Some("not running"));
        assert_eq!(
            daemon_title(Some(&running), home),
            "running daemon · root ~/workspace"
        );
        assert_eq!(
            daemon_title(Some(&installed), home),
            "installed daemon (next start) · root ~/workspace"
        );
        assert_eq!(
            daemon_title(Some(&unavailable()), home),
            "daemon settings unavailable"
        );

        let items = vec![overview(
            "daily-report",
            SettingOrigin::Config,
            SettingOrigin::Config,
            SettingOrigin::Daemon,
        )];
        let view = JobsView {
            items: &items,
            error: None,
            selected: 0,
            daemon: Some(&running),
            home,
        };
        let text = render_text(&view, 100, 20);
        assert!(text.contains("running daemon · root ~/workspace"));
        assert!(text.contains("inherited from daemon"));
    }

    #[test]
    fn archive_toggle_lists_outcomes_read_only_and_goes_back() {
        let home = zdx_engine::test_support::temp_zdx_home();
        let mut app = crate::app::build_app(home.path()).unwrap();
        app.active_section = crate::app::Section::Jobs;
        app.jobs = vec![overview(
            "daily-report",
            SettingOrigin::Job,
            SettingOrigin::Job,
            SettingOrigin::Job,
        )];
        let archived = vec![ArchivedJob {
            name: "renew-domain".to_string(),
            path: PathBuf::from(format!(
                "{HOME}/.zdx/jobs/archive/renew-domain--20301020T120000Z.md"
            )),
            key: Some("20301020T120000Z".to_string()),
            scheduled_for: Some(chrono::Utc::now()),
            last_run: Some(jobs::LastRun {
                run_id: "run-1".to_string(),
                trigger: jobs::RunTrigger::Daemon,
                outcome: Outcome::Failed,
                scheduled_for: None,
                finished_at: chrono::Utc::now(),
                error: Some("HTTP 401: Invalid API Key".to_string()),
                thread_id: Some("job-renew-domain-20301020-120000".to_string()),
            }),
        }];

        toggle_archived(&mut app, || Ok(archived.clone()));
        assert!(matches!(&app.jobs_archived, Some(Ok(items)) if items.len() == 1));
        assert!(handle_jobs_key(&mut app, KeyCode::Enter));
        assert!(app.model_picker.is_none(), "archived jobs open no picker");
        assert!(app.status_message.contains("read-only"));

        let mut terminal = Terminal::new(TestBackend::new(70, 20)).unwrap();
        terminal
            .draw(|frame| render_jobs(frame, &app, frame.area()))
            .unwrap();
        let buffer = terminal.backend().buffer();
        let text: String = (0..buffer.area.height)
            .flat_map(|y| (0..buffer.area.width).map(move |x| (x, y)))
            .map(|(x, y)| buffer.cell((x, y)).unwrap().symbol().to_string())
            .collect();
        assert!(text.contains("Archived once jobs"), "{text}");
        assert!(text.contains("renew-domain"), "{text}");
        assert!(text.contains("failed"), "{text}");
        assert!(text.contains("HTTP 401"), "{text}");

        toggle_archived(&mut app, || unreachable!("going back does not reload"));
        assert!(app.jobs_archived.is_none());
    }

    mod editing {
        use std::fs;

        use ratatui::backend::TestBackend;
        use zdx_engine::job_daemon::jobs_for;

        use super::super::*;
        use crate::tabs::config::{PickerPhase, handle_model_picker_key, render_model_picker};

        const PAUSED: &str =
            "---\n# paused on purpose\ntimeout_secs: 60\n---\nInherited job prompt.\n";
        const EXPLICIT: &str = "---\nschedule: \"0 9 * * 1\"\nmodel: \"claude-cli:claude-sonnet-5@low\" # pinned\n---\nBody.\n";

        struct Fixture {
            _home: zdx_engine::test_support::TestZdxHomeGuard,
            app: MonitorApp,
            dir: PathBuf,
            state: DaemonContextState,
        }

        impl Fixture {
            fn new() -> Self {
                let home = zdx_engine::test_support::temp_zdx_home();
                let dir = home.path().join("jobs");
                fs::create_dir_all(&dir).unwrap();
                fs::write(dir.join("explicit.md"), EXPLICIT).unwrap();
                fs::write(dir.join("paused.md"), PAUSED).unwrap();
                let daemon_root = home.path().join("daemon-root");
                fs::create_dir_all(daemon_root.join(".zdx")).unwrap();
                fs::write(
                    daemon_root.join(".zdx").join("config.toml"),
                    "model = \"claude-cli@work:claude-opus-5@high\"\n",
                )
                .unwrap();
                let state = DaemonContextState::Available {
                    context: DaemonContext {
                        source: DaemonContextSource::Running,
                        root: daemon_root.clone(),
                        config_dir: daemon_root,
                        pid: Some(1),
                        started_at: None,
                        note: None,
                    },
                };
                let mut app = crate::app::build_app(home.path()).unwrap();
                app.active_section = crate::app::Section::Jobs;
                app.status_section = crate::app::Section::Jobs;
                apply_snapshot(
                    &mut app,
                    jobs_for(state.clone(), &jobs::JobsHome::at(home.path())),
                    None,
                );
                Self {
                    _home: home,
                    app,
                    dir,
                    state,
                }
            }

            fn select(&mut self, name: &str) {
                self.app.selected_index = self
                    .app
                    .jobs
                    .iter()
                    .position(|item| item.name == name)
                    .unwrap();
            }

            fn loader(&self) -> impl FnOnce() -> DaemonJobs + use<> {
                let (state, dir) = (self.state.clone(), self.dir.clone());
                let home = jobs::JobsHome::at(dir.parent().unwrap());
                move || jobs_for(state, &home)
            }

            fn file(&self, name: &str) -> String {
                fs::read_to_string(self.dir.join(format!("{name}.md"))).unwrap()
            }
        }

        #[test]
        fn picker_opens_on_the_resolved_daemon_model() {
            let mut fx = Fixture::new();
            fx.select("paused");
            open_job_model_picker(&mut fx.app);
            let picker = fx.app.model_picker.as_ref().expect("picker opened");
            assert_eq!(picker.field, "job paused");
            assert_eq!(picker.chosen_model, "claude-cli@work:claude-opus-5");
            assert_eq!(
                picker.thinking_current,
                zdx_engine::config::ThinkingLevel::High
            );
            assert!(matches!(
                &picker.target,
                PickerTarget::Job { name, declared: None, .. } if name == "paused"
            ));

            fx.select("explicit");
            open_job_model_picker(&mut fx.app);
            let picker = fx.app.model_picker.as_ref().unwrap();
            assert_eq!(picker.chosen_model, "claude-cli:claude-sonnet-5");
            assert!(matches!(
                &picker.target,
                PickerTarget::Job { declared: Some(model), .. }
                    if model == "claude-cli:claude-sonnet-5@low"
            ));
        }

        #[test]
        fn cancel_writes_nothing() {
            let mut fx = Fixture::new();
            fx.select("paused");
            open_job_model_picker(&mut fx.app);
            handle_model_picker_key(&mut fx.app, KeyCode::Enter);
            assert!(fx.app.model_picker.as_ref().unwrap().phase == PickerPhase::Thinking);
            handle_model_picker_key(&mut fx.app, KeyCode::Esc);
            handle_model_picker_key(&mut fx.app, KeyCode::Esc);
            assert!(fx.app.model_picker.is_none());
            assert_eq!(fx.file("paused"), PAUSED);
        }

        #[test]
        fn inherited_becomes_explicit_then_resets_keeping_the_file() {
            let mut fx = Fixture::new();
            fx.select("paused");
            open_job_model_picker(&mut fx.app);
            let loader = fx.loader();
            save_model_with(
                &mut fx.app,
                ModelEdit::Set("stepfun:step-3.7-flash@low"),
                loader,
            );
            assert!(fx.app.model_picker.is_none());
            assert_eq!(
                fx.file("paused"),
                PAUSED.replace(
                    "timeout_secs: 60\n---",
                    "timeout_secs: 60\nmodel: \"stepfun:step-3.7-flash@low\"\n---"
                )
            );
            let selected = &fx.app.jobs[fx.app.selected_index];
            assert_eq!(selected.name, "paused");
            let model = &selected.settings.as_ref().unwrap().model;
            assert_eq!(model.spec, "stepfun:step-3.7-flash@low");
            assert_eq!(model.origin, SettingOrigin::Job);
            assert_eq!(
                selected.schedule,
                Some(JobSchedule::Manual),
                "paused job stays paused"
            );
            assert!(
                fx.app
                    .status_message
                    .contains("now stepfun:step-3.7-flash@low (set by job)"),
                "{}",
                fx.app.status_message
            );

            let target = PickerTarget::Job {
                name: "paused".to_string(),
                path: selected.path.clone(),
                declared: selected.declared_model.clone(),
            };
            let loader = fx.loader();
            edit_model_with(&mut fx.app, &target, ModelEdit::Clear, loader);
            assert_eq!(fx.file("paused"), PAUSED);
            let selected = &fx.app.jobs[fx.app.selected_index];
            assert_eq!(selected.name, "paused");
            assert_eq!(
                selected.settings.as_ref().unwrap().model.origin,
                SettingOrigin::Config
            );
            assert!(fx.app.status_message.contains("inherited from config"));
        }

        #[test]
        fn reset_on_inherited_job_is_a_no_op() {
            let mut fx = Fixture::new();
            fx.select("paused");
            assert!(handle_jobs_key(&mut fx.app, KeyCode::Char('d')));
            assert!(fx.app.status_message.contains("already inherits"));
            assert_eq!(fx.file("paused"), PAUSED);
        }

        #[test]
        fn concurrent_change_fails_visibly_and_keeps_the_new_file() {
            let mut fx = Fixture::new();
            fx.select("explicit");
            open_job_model_picker(&mut fx.app);
            let changed = EXPLICIT.replace("claude-sonnet-5@low", "claude-sonnet-5@max");
            fs::write(fx.dir.join("explicit.md"), &changed).unwrap();

            let loader = fx.loader();
            save_model_with(
                &mut fx.app,
                ModelEdit::Set("gemini:gemini-3.8-flash@low"),
                loader,
            );
            assert_eq!(fx.file("explicit"), changed);
            assert!(
                fx.app.status_message.contains("file unchanged")
                    && fx.app.status_message.contains("changed on disk"),
                "{}",
                fx.app.status_message
            );
            let selected = &fx.app.jobs[fx.app.selected_index];
            assert_eq!(selected.name, "explicit");
            assert_eq!(
                selected.declared_model.as_deref(),
                Some("claude-cli:claude-sonnet-5@max")
            );
        }

        #[test]
        fn picker_renders_in_a_narrow_terminal() {
            let mut fx = Fixture::new();
            fx.select("paused");
            open_job_model_picker(&mut fx.app);
            let picker = fx.app.model_picker.as_ref().unwrap();
            let mut terminal = Terminal::new(TestBackend::new(40, 12)).unwrap();
            terminal
                .draw(|frame| render_model_picker(frame, picker, frame.area()))
                .unwrap();
            let buffer = terminal.backend().buffer();
            let text: String = (0..buffer.area.height)
                .flat_map(|y| (0..buffer.area.width).map(move |x| (x, y)))
                .map(|(x, y)| buffer.cell((x, y)).unwrap().symbol().to_string())
                .collect();
            assert!(text.contains("job"), "{text}");
            assert!(text.contains("filter"), "{text}");
        }
    }
}
