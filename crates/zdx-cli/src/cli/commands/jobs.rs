//! Job command handlers: run, list, validate, runs, set-model, migrate.

use std::fmt::Write as _;
use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use chrono::{DateTime, Local, NaiveDate, Utc};
use zdx_engine::core::context::{EffectivePrompt, PromptContextInclusion};
use zdx_engine::core::interrupt::InterruptedError;
use zdx_engine::core::thread_persistence::ThreadPersistenceOptions;
use zdx_engine::job_daemon::{self, DaemonContextSource, DaemonContextState};
use zdx_engine::jobs::schedule::{Occurrence, once_occurrence};
use zdx_engine::jobs::store::{Claim, JobBusy, JobRunRecord, new_run_id};
use zdx_engine::jobs::{
    self, JobDefinition, JobOverview, JobRunSettings, JobSchedule, JobsHome, LastRun, Outcome,
    RunTrigger,
};
use zdx_engine::pidfile::{self, ServiceStatus};
use zdx_engine::{config, prompts, subagents};

use super::exec;

#[derive(Debug)]
struct PreparedJobRun {
    config: config::Config,
    settings: JobRunSettings,
    effective_prompt: EffectivePrompt,
    tools_override: Option<String>,
    /// Background execution for the run: off when the job's subagent does
    /// not opt in.
    background_commands: bool,
}

/// Jobs are one-shot non-interactive runs, so they carry the exec layer and
/// add the job-specific rules on top of it.
fn job_instruction_layers() -> [&'static str; 2] {
    [
        prompts::EXEC_INSTRUCTION_LAYER,
        prompts::JOB_INSTRUCTION_LAYER,
    ]
}

/// Who is asking for a job run or listing: the caller's root and config
/// (already carrying CLI overrides), plus the raw `--system-prompt` override
/// so a run that reloads config for a declared `root` keeps it.
pub struct Caller<'a> {
    pub root: &'a Path,
    pub config: &'a config::Config,
    pub system_prompt_override: Option<&'a str>,
}

fn job_prompt_context() -> PromptContextInclusion {
    PromptContextInclusion {
        project_context: true,
        memory_index: true,
        skills: true,
    }
}

const ERROR_SUMMARY_MAX_LEN: usize = 400;

/// Delay before retry `attempt + 1`: one minute per failed attempt.
fn retry_delay(failed_attempt: u32) -> Duration {
    Duration::from_secs(60 * u64::from(failed_attempt))
}

/// Options for listing job runs.
#[derive(Debug, Clone, Default)]
pub struct RunsOptions {
    pub name: Option<String>,
    pub date: Option<String>,
    pub date_start: Option<String>,
    pub date_end: Option<String>,
    pub json: bool,
}

/// Prints job run history from `jobs_runs.jsonl`.
pub fn runs(options: RunsOptions) -> Result<()> {
    let RunsOptions {
        name,
        date,
        date_start,
        date_end,
        json,
    } = options;

    let exact_date = parse_run_date_filter(date.as_deref(), "date")?;
    let start_date = parse_run_date_filter(date_start.as_deref(), "date-start")?;
    let end_date = parse_run_date_filter(date_end.as_deref(), "date-end")?;
    if let (Some(start), Some(end)) = (start_date, end_date)
        && start > end
    {
        bail!("--date-start must be on or before --date-end");
    }

    let records = JobsHome::current().read_run_records()?;
    let needle = name.as_deref().map(str::trim);
    let filtered: Vec<&JobRunRecord> = records
        .iter()
        .filter(|record| needle.is_none_or(|needle| record.job == needle))
        .filter(|record| matches_run_date_filters(record, exact_date, start_date, end_date))
        .collect();

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&filtered).context("serialize job runs")?
        );
        return Ok(());
    }
    if filtered.is_empty() {
        match needle {
            Some(name) => println!("No runs found for job '{name}'."),
            None => println!("No job runs found."),
        }
        return Ok(());
    }

    for record in filtered {
        let outcome = record
            .outcome
            .map_or_else(|| if record.ok { "ok" } else { "failed" }, Outcome::as_str);
        println!(
            "{} | {} | {} | {} | {}ms | attempt {}/{} | thread {}",
            record.finished_at,
            record.job,
            record.trigger.as_str(),
            outcome,
            record.duration_ms,
            record.attempt,
            record.max_attempts,
            record.thread_id.as_deref().unwrap_or("-")
        );
        if let Some(scheduled_for) = &record.scheduled_for {
            println!("  scheduled for: {scheduled_for}");
        }
        if let Some(err) = &record.error {
            println!("  error: {err}");
        }
    }
    Ok(())
}

/// Prints a job's recent runs (`zdx_engine::jobs::history`), newest first.
pub fn history(name: &str, limit: usize, json: bool) -> Result<()> {
    let runs = jobs::history::recent_runs(&JobsHome::current(), name.trim(), limit, None)?;
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&runs).context("serialize job history")?
        );
    } else {
        println!("{}", jobs::history::prompt_section(name.trim(), &runs));
    }
    Ok(())
}

/// Lists jobs. `json` prints the shared overview (schedule, status, next
/// run, last outcome, declared and effective root/model).
pub fn list(caller: &Caller<'_>, json: bool) -> Result<()> {
    let home = JobsHome::current();
    let catalog = jobs::catalog(&home)?;
    let mut overviews: Vec<JobOverview> = catalog
        .jobs
        .iter()
        .map(|job| jobs::overview(caller.config, caller.root, &home, job))
        .chain(catalog.invalid.iter().map(jobs::invalid_overview))
        .collect();
    overviews.sort_by(|a, b| a.name.cmp(&b.name));

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&overviews).context("serialize jobs")?
        );
        return Ok(());
    }
    if overviews.is_empty() {
        println!("No jobs found.");
        return Ok(());
    }
    for item in &overviews {
        println!("{}", summary_line(item));
    }
    Ok(())
}

/// `name - schedule - status[, next ...][, last ...]`, or the file error.
fn summary_line(item: &JobOverview) -> String {
    let Some(schedule) = &item.schedule else {
        return format!(
            "{} - invalid: {}",
            item.name,
            item.error.as_deref().unwrap_or("unknown error")
        );
    };
    let mut line = format!("{} - {}", item.name, schedule.describe());
    if item.state.status != jobs::JobStatus::Manual {
        let _ = write!(line, " - {}", item.state.status.as_str());
    }
    if let Some(next) = item.state.next_run_at {
        let _ = write!(line, ", next {}", local_time(next));
    }
    if let Some(last) = &item.state.last_run {
        let _ = write!(
            line,
            ", last {} {}",
            last.outcome.as_str(),
            local_time(last.finished_at)
        );
    }
    line
}

fn local_time(at: DateTime<Utc>) -> String {
    at.with_timezone(&Local)
        .format("%Y-%m-%d %H:%M %:z")
        .to_string()
}

/// Lists archived once jobs with their final outcome, newest first.
pub fn list_archived(json: bool) -> Result<()> {
    let archived = JobsHome::current().archived()?;
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&archived).context("serialize archived jobs")?
        );
        return Ok(());
    }
    if archived.is_empty() {
        println!("No archived jobs.");
        return Ok(());
    }
    for item in &archived {
        let when = item
            .scheduled_for
            .map_or_else(|| "unknown run_at".to_string(), local_time);
        match &item.last_run {
            Some(last) => {
                println!(
                    "{} - once {when} - {} ({}) - {}",
                    item.name,
                    last.outcome.as_str(),
                    last.trigger.as_str(),
                    item.path.display()
                );
                if let Some(error) = &last.error {
                    println!("  error: {error}");
                }
            }
            None => println!(
                "{} - once {when} - outcome unknown - {}",
                item.name,
                item.path.display()
            ),
        }
    }
    Ok(())
}

/// Lists jobs resolved the way the scheduled daemon resolves them (see
/// `zdx_engine::job_daemon`), independent of this process's root.
pub fn list_daemon(json: bool) -> Result<()> {
    let view = job_daemon::jobs();
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&view).context("serialize daemon jobs")?
        );
        return Ok(());
    }

    match &view.daemon {
        DaemonContextState::Available { context } => {
            let source = match context.source {
                DaemonContextSource::Running => "running daemon",
                DaemonContextSource::Installed => "installed daemon service",
            };
            println!("Daemon: {source}, root {}", context.root.display());
            if let Some(note) = &context.note {
                println!("Note: {note}");
            }
        }
        DaemonContextState::Unavailable { reason } => println!("Daemon: unavailable ({reason})"),
    }
    if let Some(error) = &view.error {
        bail!("{error}");
    }
    if view.jobs.is_empty() {
        println!("No jobs found.");
    }
    for item in &view.jobs {
        match (&item.settings, item.schedule.is_some()) {
            (Some(settings), _) => println!(
                "{} - model={} [{}] root={} [{}]",
                summary_line(item),
                settings.model.spec,
                settings.model.origin.as_str(),
                settings.root.path.display(),
                settings.root.origin.as_str(),
            ),
            (None, true) => println!(
                "{} - error: {}",
                summary_line(item),
                item.error.as_deref().unwrap_or("unresolved")
            ),
            (None, false) => println!("{}", summary_line(item)),
        }
    }
    Ok(())
}

/// Sets (`spec`) or clears (`clear`) the declared model of one job.
pub fn set_model(name: &str, spec: Option<&str>, clear: bool) -> Result<()> {
    let job = jobs::load_by_name(&JobsHome::current(), name)
        .with_context(|| format!("load job '{name}'"))?;
    let edit = match (spec, clear) {
        (Some(spec), false) => jobs::ModelEdit::Set(spec),
        (None, true) => jobs::ModelEdit::Clear,
        _ => bail!("pass either a model spec or --clear"),
    };
    let updated = jobs::set_model(&job.path, None, edit)?;
    match updated.model.as_deref() {
        Some(model) => println!("{name}: model = {model}"),
        None => println!("{name}: model cleared (inherits)"),
    }
    Ok(())
}

/// Validates every job file: it parses, and its root, subagent, and model
/// resolve. Reports every problem, then fails if there was one.
pub fn validate(caller: &Caller<'_>) -> Result<()> {
    let home = JobsHome::current();
    let catalog = jobs::catalog(&home)?;
    let mut problems = Vec::new();
    for invalid in &catalog.invalid {
        problems.push(format!(
            "{} ({}): {}",
            invalid.name,
            invalid.path.display(),
            invalid.error
        ));
    }
    let now = Utc::now();
    let mut lines = Vec::new();
    for job in &catalog.jobs {
        match prepare_job_run(caller, job) {
            Ok(run) => {
                let state = jobs::job_state(&home, job, now, &Local);
                lines.push(validation_line(job, &run.settings, state.status));
            }
            Err(err) => problems.push(format!("{}: {err:#}", job.name)),
        }
    }

    if !problems.is_empty() {
        bail!(
            "{} of {} job(s) failed validation:\n- {}",
            problems.len(),
            catalog.jobs.len() + catalog.invalid.len(),
            problems.join("\n- ")
        );
    }
    println!("Validated {} job(s).", catalog.jobs.len());
    for line in lines {
        println!("{line}");
    }
    Ok(())
}

fn validation_line(
    job: &JobDefinition,
    settings: &JobRunSettings,
    status: jobs::JobStatus,
) -> String {
    format!(
        "- {} ({}): schedule={}, status={}, grace_secs={}, model={} [{}], root={} [{}], subagent={}, timeout_secs={}, max_retries={}",
        job.name,
        job.source.as_str(),
        job.schedule.describe(),
        status.as_str(),
        job.effective_grace_secs()
            .map_or_else(|| "-".to_string(), |v| v.to_string()),
        settings.model.spec,
        settings.model.origin.as_str(),
        settings.root.path.display(),
        settings.root.origin.as_str(),
        job.subagent.as_deref().unwrap_or("<default>"),
        job.timeout_secs
            .map_or_else(|| "<default>".to_string(), |v| v.to_string()),
        job.max_retries
    )
}

/// The scheduled occurrence a daemon-started run executes.
#[derive(Debug, Clone)]
pub struct ScheduledOccurrence {
    pub key: String,
    pub at: DateTime<Utc>,
}

/// How `zdx jobs run` was asked to run.
#[derive(Debug, Clone, Default)]
pub struct RunRequest {
    /// Run a once job as a test, without consuming its occurrence.
    pub keep: bool,
    /// Set when the scheduler started this run.
    pub scheduled: Option<ScheduledOccurrence>,
}

/// Runs one job by name.
///
/// A scheduled run, and a manual run of a once job without `--keep`, first
/// claims its occurrence; the claim gets the final outcome, and a once job is
/// then archived. Every run holds the job's lock, so a job never has two live
/// runs.
///
/// # Errors
/// Returns [`JobBusy`] when another run holds the lock, and an error when the
/// job cannot be loaded, its occurrence was already taken (manual), or the
/// run fails.
pub async fn run(
    caller: &Caller<'_>,
    thread_opts: &ThreadPersistenceOptions,
    name: &str,
    request: RunRequest,
) -> Result<()> {
    let home = JobsHome::current();
    let job = jobs::load_by_name(&home, name)?;
    let trigger = if request.scheduled.is_some() {
        RunTrigger::Daemon
    } else {
        RunTrigger::Manual
    };
    let occurrence = match occurrence_to_take(&job, &request) {
        Take::Occurrence(occurrence) => Some(occurrence),
        Take::Nothing => None,
        Take::Skip => return Ok(()),
    };

    let Some(lock) = home.try_lock_job(&job.name)? else {
        return Err(JobBusy {
            job: job.name.clone(),
        }
        .into());
    };
    let claim = match &occurrence {
        Some(occurrence) => {
            let claim = Claim::new(
                &job.name,
                &occurrence.key,
                trigger,
                job.schedule.kind(),
                occurrence.at,
            );
            if !home.create_claim(&claim)? {
                return already_taken(&home, &job, occurrence, request.scheduled.is_some());
            }
            Some(claim)
        }
        None => None,
    };

    let run_id = claim
        .as_ref()
        .map_or_else(new_run_id, |claim| claim.run_id.clone());
    let finished = Box::pin(run_attempts(
        caller,
        thread_opts,
        &job,
        RunContext {
            trigger,
            run_id: &run_id,
            occurrence: occurrence.as_ref(),
        },
    ))
    .await;
    record_outcome(&home, &job, claim, trigger, &run_id, &finished)?;
    drop(lock);

    match finished.error {
        None => Ok(()),
        Some(err) => Err(err),
    }
}

/// What a run does with its job's occurrence.
enum Take {
    /// Claim and consume this occurrence.
    Occurrence(Occurrence),
    /// Run without consuming an occurrence (manual, recurring, or `--keep`).
    Nothing,
    /// Do not run: a scheduled run whose job changed since it was planned.
    Skip,
}

fn occurrence_to_take(job: &JobDefinition, request: &RunRequest) -> Take {
    match (&request.scheduled, &job.schedule) {
        (Some(scheduled), JobSchedule::Once { run_at }) => {
            let occurrence = once_occurrence(*run_at);
            if occurrence.key == scheduled.key {
                Take::Occurrence(occurrence)
            } else {
                println!(
                    "Skipped '{}': run_at changed since it was planned.",
                    job.name
                );
                Take::Skip
            }
        }
        (Some(scheduled), JobSchedule::Recurring { .. }) => Take::Occurrence(Occurrence {
            at: scheduled.at,
            key: scheduled.key.clone(),
        }),
        (Some(_), JobSchedule::Manual) => {
            println!("Skipped '{}': it no longer has a schedule.", job.name);
            Take::Skip
        }
        (None, JobSchedule::Once { run_at }) if !request.keep => {
            Take::Occurrence(once_occurrence(*run_at))
        }
        (None, _) => Take::Nothing,
    }
}

/// A scheduled run skips an occurrence someone else took; a manual run
/// explains why the once job cannot run again.
fn already_taken(
    home: &JobsHome,
    job: &JobDefinition,
    occurrence: &Occurrence,
    scheduled: bool,
) -> Result<()> {
    let state = home
        .read_claim(&job.name, &occurrence.key)?
        .and_then(|claim| claim.outcome)
        .map_or("in progress", Outcome::as_str);
    if scheduled {
        println!(
            "Skipped '{}': occurrence {} was already taken ({state}).",
            job.name, occurrence.key
        );
        return Ok(());
    }
    bail!(
        "job '{}' already ran for run_at {} ({state}); change run_at to schedule it again, or \
         pass --keep for a test run",
        job.name,
        occurrence.at.to_rfc3339()
    )
}

/// Writes the final outcome to the claim and `last.json`, then archives a
/// once job whose occurrence this run consumed.
fn record_outcome(
    home: &JobsHome,
    job: &JobDefinition,
    claim: Option<Claim>,
    trigger: RunTrigger,
    run_id: &str,
    finished: &FinishedRun,
) -> Result<()> {
    let error = finished.error.as_ref().map(summarize_error);
    let consumed = claim.is_some();
    let scheduled_for = claim.as_ref().map(|claim| claim.scheduled_for);
    if let Some(claim) = claim {
        let mut claim = claim.finished(finished.outcome, error.clone());
        claim.thread_id.clone_from(&finished.thread_id);
        home.update_claim(&claim)?;
    }
    let last = LastRun {
        run_id: run_id.to_string(),
        trigger,
        outcome: finished.outcome,
        scheduled_for,
        finished_at: Utc::now(),
        error,
        thread_id: finished.thread_id.clone(),
    };
    if let Err(err) = home.write_last_run(&job.name, &last) {
        tracing::warn!(
            err = format!("{err:#}"),
            "Failed to record the job's last run"
        );
    }
    if let (true, JobSchedule::Once { run_at }) = (consumed, &job.schedule) {
        match home.archive_once_job(&job.path, *run_at) {
            Ok(Some(dest)) => println!("Archived '{}' to {}", job.name, dest.display()),
            Ok(None) => {}
            Err(err) => tracing::warn!(err = format!("{err:#}"), "Failed to archive once job"),
        }
    }
    Ok(())
}

struct RunContext<'a> {
    trigger: RunTrigger,
    run_id: &'a str,
    occurrence: Option<&'a Occurrence>,
}

struct FinishedRun {
    outcome: Outcome,
    error: Option<anyhow::Error>,
    thread_id: Option<String>,
}

/// Runs up to `max_retries + 1` attempts, appending one record per attempt.
/// Failed attempts wait [`retry_delay`] before the next one. A run that
/// cannot be prepared (missing root, bad subagent) is not retried, and an
/// interrupted attempt stops the run.
async fn run_attempts(
    caller: &Caller<'_>,
    thread_opts: &ThreadPersistenceOptions,
    job: &JobDefinition,
    context: RunContext<'_>,
) -> FinishedRun {
    let home = JobsHome::current();
    let attempts = job.max_retries.saturating_add(1);
    let effective_thread_opts = resolve_job_thread_opts(thread_opts, &job.name, Utc::now());
    let history = previous_runs_section(&home, &job.name, context.run_id);

    for attempt in 1..=attempts {
        let started_at = Utc::now();
        let started = Instant::now();

        // A run that cannot be prepared (missing root, bad subagent) is
        // recorded as failed and not retried: nothing transient can fix it,
        // and it must never fall back to another root.
        let (result, settings) = match prepare_job_run(caller, job) {
            Ok(mut prepared) => {
                add_previous_runs(&mut prepared, history.as_deref());
                let root_string = prepared.settings.root.path.to_string_lossy().to_string();
                let result = Box::pin(exec::run(exec::ExecRunOptions {
                    root: &root_string,
                    thread_opts: &effective_thread_opts,
                    prompt: &job.prompt,
                    config: &prepared.config,
                    model_override: Some(prepared.settings.model.spec.as_str()),
                    effective_prompt_override: Some(&prepared.effective_prompt),
                    tool_timeout_override: job.timeout_secs,
                    event_filter_override: None,
                    stream: false,
                    tools_override: prepared.tools_override.as_deref(),
                    no_tools: false,
                    no_system_prompt: false,
                    no_skills: false,
                    no_background_commands: !prepared.background_commands,
                    subagent: None,
                    activity_kind: Some("job"),
                    activity_parent_thread_id: None,
                    activity_subagent_name: None,
                }))
                .await;
                (result, Some(prepared.settings))
            }
            Err(err) => (Err(err.context("prepare job run")), None),
        };
        let prepared_ok = settings.is_some();
        let interrupted = result
            .as_ref()
            .err()
            .is_some_and(|err| err.downcast_ref::<InterruptedError>().is_some());
        let retry = result.is_err() && prepared_ok && !interrupted && attempt < attempts;
        let outcome = match &result {
            Ok(()) => Outcome::Succeeded,
            Err(_) if interrupted => Outcome::Interrupted,
            Err(_) if retry => Outcome::Retrying,
            Err(_) => Outcome::Failed,
        };

        let record = attempt_record(
            job,
            &context,
            &effective_thread_opts,
            AttemptResult {
                attempt,
                attempts,
                started_at,
                duration: started.elapsed(),
                outcome,
                error: result.as_ref().err(),
                settings: settings.as_ref(),
            },
        );
        if let Err(err) = home.append_run_record(&record) {
            tracing::warn!(err = format!("{err:#}"), "Failed to append job run log");
        }

        match result {
            Ok(()) => {
                return FinishedRun {
                    outcome,
                    error: None,
                    thread_id: effective_thread_opts.thread_id,
                };
            }
            Err(err) if retry => {
                let delay = retry_delay(attempt);
                tracing::warn!(
                    name = %job.name,
                    attempt,
                    attempts,
                    delay_secs = delay.as_secs(),
                    err = format!("{err:#}"),
                    "Job failed, retrying"
                );
                tokio::time::sleep(delay).await;
            }
            Err(err) => {
                return FinishedRun {
                    outcome,
                    error: Some(err.context(format!(
                        "job '{}' failed after {attempt} attempt(s)",
                        job.name
                    ))),
                    thread_id: effective_thread_opts.thread_id,
                };
            }
        }
    }
    unreachable!("the last attempt always returns")
}

#[derive(Clone, Copy)]
struct AttemptResult<'a> {
    attempt: u32,
    attempts: u32,
    started_at: DateTime<Utc>,
    duration: Duration,
    outcome: Outcome,
    error: Option<&'a anyhow::Error>,
    settings: Option<&'a JobRunSettings>,
}

fn attempt_record(
    job: &JobDefinition,
    context: &RunContext<'_>,
    thread_opts: &ThreadPersistenceOptions,
    result: AttemptResult<'_>,
) -> JobRunRecord {
    JobRunRecord {
        job: job.name.clone(),
        run_id: Some(context.run_id.to_string()),
        trigger: context.trigger,
        schedule_kind: Some(job.schedule.kind()),
        scheduled_for: context.occurrence.map(|o| o.at.to_rfc3339()),
        lateness_secs: context
            .occurrence
            .map(|o| (result.started_at - o.at).num_seconds()),
        thread_id: thread_opts.thread_id.clone(),
        attempt: result.attempt,
        max_attempts: result.attempts,
        started_at: result.started_at.to_rfc3339(),
        finished_at: Utc::now().to_rfc3339(),
        duration_ms: u64::try_from(result.duration.as_millis()).unwrap_or(u64::MAX),
        ok: result.error.is_none(),
        outcome: Some(result.outcome),
        error: result.error.map(summarize_error),
        schedule: Some(job.schedule.describe()),
        model: result.settings.map(|s| s.model.spec.clone()),
        subagent: job.subagent.clone(),
        root: result
            .settings
            .map(|s| s.root.path.to_string_lossy().to_string()),
    }
}

/// What the run is told about the job's earlier runs; `None` when the run
/// log cannot be read (the run goes on without it).
fn previous_runs_section(home: &JobsHome, job: &str, run_id: &str) -> Option<String> {
    match jobs::history::recent_runs(home, job, jobs::history::DEFAULT_RECENT_RUNS, Some(run_id)) {
        Ok(runs) => Some(jobs::history::prompt_section(job, &runs)),
        Err(err) => {
            tracing::warn!(
                err = format!("{err:#}"),
                "Reading the job's previous runs failed"
            );
            None
        }
    }
}

/// Adds the previous-runs section to the run's first runtime context, on
/// both the default and the `subagent:` path.
fn add_previous_runs(prepared: &mut PreparedJobRun, section: Option<&str>) {
    let Some(section) = section else {
        return;
    };
    match prepared.effective_prompt.runtime_context.as_mut() {
        Some(context) => context.add_initial_section(section),
        None => {
            prepared.effective_prompt.runtime_context =
                Some(zdx_engine::core::context::RuntimeContext::with_section(
                    &prepared.settings.root.path,
                    section,
                ));
        }
    }
}

fn prepare_job_run(caller: &Caller<'_>, job: &JobDefinition) -> Result<PreparedJobRun> {
    let mut resolved = jobs::resolve_run(caller.config, caller.root, job)?;
    crate::cli::apply_system_prompt_override(&mut resolved.config, caller.system_prompt_override);
    let run_root = resolved.settings.root.path.clone();
    let model = resolved.config.model.clone();
    let instruction_layers = job_instruction_layers();

    // A job run with a subagent follows its definition, like any other
    // subagent run; a job without one keeps background execution.
    let background_commands = resolved
        .subagent
        .as_ref()
        .is_none_or(|definition| definition.background_commands);
    let (effective_prompt, tools_override) = match resolved.subagent.as_ref() {
        Some(definition) => {
            let (prompt, runtime_context) = subagents::render_prompt_with_layers(
                &resolved.config,
                &run_root,
                definition,
                &model,
                job_prompt_context(),
                &instruction_layers,
            )?;
            (
                EffectivePrompt {
                    prompt: Some(prompt),
                    runtime_context,
                    ..EffectivePrompt::default()
                },
                definition.tools.as_ref().map(|tools| tools.join(",")),
            )
        }
        None => (
            zdx_engine::core::context::build_prompt_with_context_and_layers(
                &resolved.config,
                &run_root,
                &model,
                &instruction_layers,
                job_prompt_context(),
            )
            .context("render job prompt")?,
            None,
        ),
    };

    Ok(PreparedJobRun {
        config: resolved.config,
        settings: resolved.settings,
        effective_prompt,
        tools_override,
        background_commands,
    })
}

fn resolve_job_thread_opts(
    thread_opts: &ThreadPersistenceOptions,
    job_name: &str,
    now: DateTime<Utc>,
) -> ThreadPersistenceOptions {
    if thread_opts.no_save || thread_opts.thread_id.is_some() {
        return thread_opts.clone();
    }

    ThreadPersistenceOptions {
        thread_id: Some(format!("job-{job_name}-{}", now.format("%Y%m%d-%H%M%S"))),
        no_save: false,
        ..Default::default()
    }
}

/// Moves pre-rename automations into `$ZDX_HOME/jobs/` and converts the run
/// log. `dry_run` prints the plan only.
pub fn migrate(dry_run: bool) -> Result<()> {
    let home = JobsHome::current();
    let plan = jobs::migrate::plan(&home)?;
    if plan.is_empty() {
        println!("Nothing to migrate.");
        return Ok(());
    }
    for (from, to) in &plan.moves {
        println!("move {} -> {}", from.display(), to.display());
    }
    if let Some(log) = &plan.runs_log {
        println!(
            "convert {} ({} record(s)) -> {}, keep the original as {}",
            log.from.display(),
            log.records,
            log.to.display(),
            log.backup.display()
        );
    }
    for path in &plan.unused {
        println!("no longer used (left in place): {}", path.display());
    }
    if dry_run {
        println!("Dry run: nothing changed.");
        return Ok(());
    }
    if let ServiceStatus::Running { pid, .. } = pidfile::status("daemon") {
        bail!(
            "the jobs daemon is running (PID {pid}); stop it first with `zdx service stop daemon`, \
             then run `zdx jobs migrate` again"
        );
    }
    jobs::migrate::apply(&home, &plan)?;
    println!(
        "Migrated. If the daemon service was installed before the rename, reinstall it so it runs \
         `zdx jobs daemon`: zdx --root <root> service install daemon"
    );
    Ok(())
}

fn summarize_error(err: &anyhow::Error) -> String {
    let text = format!("{err:#}").replace('\n', " | ");
    if text.len() > ERROR_SUMMARY_MAX_LEN {
        let mut end = ERROR_SUMMARY_MAX_LEN;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        format!("{}...", &text[..end])
    } else {
        text
    }
}

fn parse_run_date_filter(raw: Option<&str>, flag: &str) -> Result<Option<NaiveDate>> {
    let Some(raw) = raw else {
        return Ok(None);
    };

    let trimmed = raw.trim();
    if trimmed.is_empty() {
        bail!("--{flag} cannot be empty");
    }

    NaiveDate::parse_from_str(trimmed, "%Y-%m-%d")
        .with_context(|| format!("invalid --{flag} value '{trimmed}' (expected YYYY-MM-DD)"))
        .map(Some)
}

fn matches_run_date_filters(
    record: &JobRunRecord,
    exact: Option<NaiveDate>,
    start: Option<NaiveDate>,
    end: Option<NaiveDate>,
) -> bool {
    if exact.is_none() && start.is_none() && end.is_none() {
        return true;
    }

    let Some(record_date) = DateTime::parse_from_rfc3339(&record.finished_at)
        .ok()
        .map(|dt| dt.with_timezone(&Utc).date_naive())
    else {
        return false;
    };

    exact.is_none_or(|exact| record_date == exact)
        && start.is_none_or(|start| record_date >= start)
        && end.is_none_or(|end| record_date <= end)
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone;
    use tempfile::tempdir;
    use zdx_engine::config::{Config, SkillSourceToggles, ThinkingLevel};
    use zdx_engine::jobs::JobSource;

    use super::*;

    #[test]
    fn resolve_job_thread_opts_defaults_to_prefixed_thread() {
        let opts = ThreadPersistenceOptions {
            thread_id: None,
            no_save: false,
            ..Default::default()
        };

        let now = Utc.with_ymd_and_hms(2026, 2, 12, 7, 45, 0).unwrap();
        let resolved = resolve_job_thread_opts(&opts, "daily-report", now);
        assert_eq!(
            resolved.thread_id.as_deref(),
            Some("job-daily-report-20260212-074500")
        );
        assert!(!resolved.no_save);
    }

    #[test]
    fn resolve_job_thread_opts_daemon_uses_timestamped_thread_id() {
        let opts = ThreadPersistenceOptions {
            thread_id: None,
            no_save: false,
            ..Default::default()
        };

        let now = Utc.with_ymd_and_hms(2026, 2, 12, 7, 45, 0).unwrap();
        let resolved = resolve_job_thread_opts(&opts, "daily-report", now);
        assert_eq!(
            resolved.thread_id.as_deref(),
            Some("job-daily-report-20260212-074500")
        );
        assert!(!resolved.no_save);
    }

    #[test]
    fn resolve_job_thread_opts_honors_no_thread() {
        let opts = ThreadPersistenceOptions {
            thread_id: None,
            no_save: true,
            ..Default::default()
        };

        let now = Utc.with_ymd_and_hms(2026, 2, 12, 7, 45, 0).unwrap();
        let resolved = resolve_job_thread_opts(&opts, "daily-report", now);
        assert!(resolved.thread_id.is_none());
        assert!(resolved.no_save);
    }

    #[test]
    fn resolve_job_thread_opts_honors_explicit_thread() {
        let opts = ThreadPersistenceOptions {
            thread_id: Some("custom-thread".to_string()),
            no_save: false,
            ..Default::default()
        };

        let now = Utc.with_ymd_and_hms(2026, 2, 12, 7, 45, 0).unwrap();
        let resolved = resolve_job_thread_opts(&opts, "daily-report", now);
        assert_eq!(resolved.thread_id.as_deref(), Some("custom-thread"));
        assert!(!resolved.no_save);
    }

    #[test]
    fn retries_wait_longer_after_each_failure() {
        assert_eq!(retry_delay(1), Duration::from_mins(1));
        assert_eq!(retry_delay(2), Duration::from_mins(2));
    }

    #[test]
    fn parse_run_date_filter_accepts_valid_date() {
        let date = parse_run_date_filter(Some("2026-02-11"), "date").unwrap();
        assert_eq!(
            date,
            Some(chrono::NaiveDate::from_ymd_opt(2026, 2, 11).unwrap())
        );
    }

    #[test]
    fn matches_run_date_filters_uses_finished_at() {
        let record = JobRunRecord {
            job: "daily".to_string(),
            run_id: None,
            trigger: RunTrigger::Manual,
            schedule_kind: None,
            scheduled_for: None,
            lateness_secs: None,
            outcome: None,
            thread_id: Some("job-daily-20260211-080000".to_string()),
            attempt: 1,
            max_attempts: 1,
            started_at: "2026-02-11T08:00:00Z".to_string(),
            finished_at: "2026-02-11T08:00:03Z".to_string(),
            duration_ms: 3000,
            ok: true,
            error: None,
            schedule: None,
            model: None,
            subagent: None,
            root: None,
        };

        let exact = chrono::NaiveDate::from_ymd_opt(2026, 2, 11).unwrap();
        let miss = chrono::NaiveDate::from_ymd_opt(2026, 2, 12).unwrap();

        assert!(matches_run_date_filters(&record, Some(exact), None, None));
        assert!(!matches_run_date_filters(&record, Some(miss), None, None));
        assert!(matches_run_date_filters(
            &record,
            None,
            Some(exact),
            Some(exact)
        ));
    }

    #[test]
    fn prepare_job_run_uses_effective_model_and_project_context() {
        // Building the prompt materializes bundled skills into $ZDX_HOME.
        let _home = zdx_engine::test_support::temp_zdx_home();
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("AGENTS.md"), "Job project note").unwrap();

        let mut config = Config {
            model: "anthropic:claude-opus-4-6".to_string(),
            thinking_level: ThinkingLevel::Low,
            system_prompt: Some("Base prompt".to_string()),
            ..Default::default()
        };
        config.subagents.enabled = false;
        config.skills.sources = SkillSourceToggles {
            zdx_user: false,
            zdx_project: false,
            codex_user: false,
            claude_user: false,
            claude_project: false,
            agents_user: false,
            agents_project: false,
        };

        let job = JobDefinition {
            name: "demo".to_string(),
            path: dir.path().join("demo.md"),
            source: JobSource::User,
            schedule: JobSchedule::Manual,
            grace_secs: None,
            model: Some("openai-codex:gpt-5.4".to_string()),
            subagent: None,
            root: None,
            timeout_secs: None,
            max_retries: 0,
            prompt: "Do the thing".to_string(),
        };

        let prepared = prepare_job_run(&test_caller(&config, dir.path()), &job).unwrap();
        let prompt = prepared.effective_prompt.prompt.clone().unwrap_or_default();

        assert_eq!(prepared.settings.model.spec, "openai-codex:gpt-5.4@low");
        assert_eq!(prepared.settings.model.origin, jobs::SettingOrigin::Job);
        assert_eq!(
            prepared.settings.model.thinking_origin,
            jobs::SettingOrigin::Config
        );
        assert_eq!(prepared.config.model, "openai-codex:gpt-5.4@low");
        assert_eq!(prepared.settings.root.origin, jobs::SettingOrigin::Caller);
        assert!(prompt.contains("Job project note"));
        assert_eq!(prompt.matches(NON_INTERACTIVE_MARKER).count(), 1);
        assert_eq!(prompt.matches(JOB_MARKER).count(), 1);
        assert!(
            initial_context(&prepared).contains("<runtime_context>"),
            "runtime context (skills, memory index) must reach the run"
        );
    }

    /// Both prompt paths get the job's previous runs in the first runtime
    /// context, after the usual snapshot, and a prompt without a snapshot
    /// gets one holding just that section.
    #[test]
    fn previous_runs_reach_the_runtime_context() {
        let _home = zdx_engine::test_support::temp_zdx_home();
        let dir = tempdir().unwrap();
        let mut config = Config {
            model: "anthropic:claude-opus-4-6".to_string(),
            ..Default::default()
        };
        config.skills.sources = no_skill_sources();
        let job = JobDefinition {
            name: "demo".to_string(),
            path: dir.path().join("demo.md"),
            source: JobSource::User,
            schedule: JobSchedule::Manual,
            grace_secs: None,
            model: None,
            subagent: None,
            root: None,
            timeout_secs: None,
            max_retries: 0,
            prompt: "Do the thing".to_string(),
        };
        let home = JobsHome::current();
        let section = previous_runs_section(&home, "demo", "current").expect("section");
        assert!(section.contains("No earlier recorded run of `demo`"));

        let mut prepared = prepare_job_run(&test_caller(&config, dir.path()), &job).unwrap();
        add_previous_runs(&mut prepared, Some(&section));
        let initial = initial_context(&prepared);
        assert!(initial.starts_with("<runtime_context>"));
        assert!(initial.ends_with(
            "No earlier recorded run of `demo` (`zdx jobs history demo`).\n</runtime_context>"
        ));
        assert_eq!(initial.matches("<runtime_context>").count(), 1);

        prepared.effective_prompt.runtime_context = None;
        add_previous_runs(&mut prepared, Some(&section));
        assert!(initial_context(&prepared).contains("# Previous Runs of This Job"));
    }

    fn test_caller<'a>(config: &'a Config, root: &'a Path) -> Caller<'a> {
        Caller {
            root,
            config,
            system_prompt_override: None,
        }
    }

    fn initial_context(prepared: &PreparedJobRun) -> String {
        prepared
            .effective_prompt
            .runtime_context
            .as_ref()
            .and_then(|context| context.initial.clone())
            .unwrap_or_default()
    }

    const NON_INTERACTIVE_MARKER: &str = "This run is non-interactive";
    const JOB_MARKER: &str = "This run is a ZDX job";

    fn no_skill_sources() -> SkillSourceToggles {
        SkillSourceToggles {
            zdx_user: false,
            zdx_project: false,
            codex_user: false,
            claude_user: false,
            claude_project: false,
            agents_user: false,
            agents_project: false,
        }
    }

    #[test]
    fn prepare_job_run_subagent_keeps_non_interactive_layers_and_context() {
        let _home = zdx_engine::test_support::temp_zdx_home();
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("notes.md"), "workspace file").unwrap();
        let subagents_dir = dir.path().join(".zdx").join("subagents");
        std::fs::create_dir_all(&subagents_dir).unwrap();
        std::fs::write(
            subagents_dir.join("helper.md"),
            "---\ndescription: Helper\nmodel: openai-codex:gpt-5.4@high\ntools:\n  - read\n---\nHelper body sentinel.",
        )
        .unwrap();

        let mut config = Config {
            model: "anthropic:claude-opus-4-6".to_string(),
            ..Default::default()
        };
        config.skills.sources = no_skill_sources();

        let job = JobDefinition {
            name: "demo".to_string(),
            path: dir.path().join("demo.md"),
            source: JobSource::User,
            schedule: JobSchedule::Manual,
            grace_secs: None,
            model: None,
            subagent: Some("helper".to_string()),
            root: None,
            timeout_secs: None,
            max_retries: 0,
            prompt: "Do the thing".to_string(),
        };

        let prepared = prepare_job_run(&test_caller(&config, dir.path()), &job).unwrap();
        let prompt = prepared.effective_prompt.prompt.clone().unwrap_or_default();

        assert!(prompt.contains("Helper body sentinel."));
        assert_eq!(prompt.matches(NON_INTERACTIVE_MARKER).count(), 1);
        assert_eq!(prompt.matches(JOB_MARKER).count(), 1);
        assert!(initial_context(&prepared).contains("<runtime_context>"));
        assert_eq!(prepared.tools_override.as_deref(), Some("read"));
        assert_eq!(prepared.settings.model.spec, "openai-codex:gpt-5.4@high");
        assert_eq!(
            prepared.settings.model.origin,
            jobs::SettingOrigin::Subagent
        );
    }

    #[test]
    fn prepare_job_run_uses_declared_root_and_its_config() {
        let _home = zdx_engine::test_support::temp_zdx_home();
        let caller = tempdir().unwrap();
        std::fs::write(caller.path().join("AGENTS.md"), "Caller root note").unwrap();
        let declared = tempdir().unwrap();
        std::fs::write(declared.path().join("AGENTS.md"), "Declared root note").unwrap();
        std::fs::create_dir_all(declared.path().join(".zdx")).unwrap();
        std::fs::write(
            declared.path().join(".zdx").join("config.toml"),
            "model = \"openai-codex:gpt-5.4@medium\"\n",
        )
        .unwrap();

        let config = Config {
            model: "anthropic:claude-opus-4-6@high".to_string(),
            ..Default::default()
        };
        let job = JobDefinition {
            name: "demo".to_string(),
            path: caller.path().join("demo.md"),
            source: JobSource::User,
            schedule: JobSchedule::Manual,
            grace_secs: None,
            model: None,
            subagent: None,
            root: Some(declared.path().to_string_lossy().to_string()),
            timeout_secs: None,
            max_retries: 0,
            prompt: "Do the thing".to_string(),
        };

        let prepared = prepare_job_run(&test_caller(&config, caller.path()), &job).unwrap();
        let prompt = prepared.effective_prompt.prompt.clone().unwrap_or_default();

        assert_eq!(prepared.settings.root.path, declared.path());
        assert_eq!(prepared.settings.root.origin, jobs::SettingOrigin::Job);
        assert_eq!(prepared.settings.model.spec, "openai-codex:gpt-5.4@medium");
        assert_eq!(prepared.settings.model.origin, jobs::SettingOrigin::Config);
        assert!(prompt.contains("Declared root note"));
        assert!(!prompt.contains("Caller root note"));
    }

    #[test]
    fn prepare_job_run_keeps_cli_system_prompt_with_declared_root() {
        let _home = zdx_engine::test_support::temp_zdx_home();
        let caller = tempdir().unwrap();
        let declared = tempdir().unwrap();
        std::fs::create_dir_all(declared.path().join(".zdx")).unwrap();
        std::fs::write(
            declared.path().join(".zdx").join("config.toml"),
            "system_prompt = \"WORKSPACE-PROMPT\"\n",
        )
        .unwrap();
        let job = JobDefinition {
            name: "demo".to_string(),
            path: caller.path().join("demo.md"),
            source: JobSource::User,
            schedule: JobSchedule::Manual,
            grace_secs: None,
            model: Some("anthropic:claude-opus-4-6".to_string()),
            subagent: None,
            root: Some(declared.path().to_string_lossy().to_string()),
            timeout_secs: None,
            max_retries: 0,
            prompt: "Do the thing".to_string(),
        };
        let config = Config::default();
        let caller_ctx = Caller {
            root: caller.path(),
            config: &config,
            system_prompt_override: Some("CLI-PROMPT"),
        };

        let prepared = prepare_job_run(&caller_ctx, &job).unwrap();
        let prompt = prepared.effective_prompt.prompt.unwrap_or_default();
        assert!(prompt.contains("CLI-PROMPT"));
        assert!(!prompt.contains("WORKSPACE-PROMPT"));
    }

    #[test]
    fn prepare_job_run_rejects_missing_root() {
        let caller = tempdir().unwrap();
        let missing = caller.path().join("does-not-exist");
        let job = JobDefinition {
            name: "demo".to_string(),
            path: caller.path().join("demo.md"),
            source: JobSource::User,
            schedule: JobSchedule::Manual,
            grace_secs: None,
            model: None,
            subagent: None,
            root: Some(missing.to_string_lossy().to_string()),
            timeout_secs: None,
            max_retries: 0,
            prompt: "Do the thing".to_string(),
        };

        let err =
            prepare_job_run(&test_caller(&Config::default(), caller.path()), &job).unwrap_err();
        assert!(
            format!("{err:#}").contains("does not exist or is not a directory"),
            "{err:#}"
        );
    }

    #[test]
    fn validate_fails_for_missing_subagent() {
        let dir = tempdir().unwrap();
        let config = Config::default();
        let job = JobDefinition {
            name: "demo".to_string(),
            path: dir.path().join("demo.md"),
            source: JobSource::User,
            schedule: JobSchedule::Manual,
            grace_secs: None,
            model: None,
            subagent: Some("missing-subagent".to_string()),
            root: None,
            timeout_secs: None,
            max_retries: 0,
            prompt: "Do the thing".to_string(),
        };

        let Err(err) = prepare_job_run(&test_caller(&config, dir.path()), &job) else {
            panic!("missing subagent should fail validation");
        };
        assert!(format!("{err:#}").contains("missing-subagent"));
    }

    #[test]
    fn validate_fails_for_subagent_with_unknown_tool() {
        let dir = tempdir().unwrap();
        let subagents_dir = dir.path().join(".zdx").join("subagents");
        std::fs::create_dir_all(&subagents_dir).unwrap();
        std::fs::write(
            subagents_dir.join("broken.md"),
            "---\ndescription: Broken\ntools:\n  - reed\n---\nPrompt",
        )
        .unwrap();

        let config = Config::default();
        let job = JobDefinition {
            name: "demo".to_string(),
            path: dir.path().join("demo.md"),
            source: JobSource::User,
            schedule: JobSchedule::Manual,
            grace_secs: None,
            model: None,
            subagent: Some("broken".to_string()),
            root: None,
            timeout_secs: None,
            max_retries: 0,
            prompt: "Do the thing".to_string(),
        };

        let err = prepare_job_run(&test_caller(&config, dir.path()), &job).unwrap_err();
        assert!(format!("{err:#}").contains("Unknown tool(s): reed"));
    }

    #[test]
    fn prepare_job_run_rejects_unknown_subagent() {
        let dir = tempdir().unwrap();
        let config = Config {
            model: "anthropic:claude-opus-4-6".to_string(),
            ..Default::default()
        };

        let job = JobDefinition {
            name: "demo".to_string(),
            path: dir.path().join("demo.md"),
            source: JobSource::User,
            schedule: JobSchedule::Manual,
            grace_secs: None,
            model: None,
            subagent: Some("task".to_string()),
            root: None,
            timeout_secs: None,
            max_retries: 0,
            prompt: "Do the thing".to_string(),
        };

        let err = prepare_job_run(&test_caller(&config, dir.path()), &job).unwrap_err();
        assert!(format!("{err:#}").contains("load subagent 'task'"));
    }
}
