//! Jobs: saved prompts that run unattended, manually, on a recurring cron
//! schedule, or once at a set time.
//!
//! A job is one markdown file in `$ZDX_HOME/jobs/` with YAML frontmatter and a
//! prompt body. This module parses and resolves definitions; [`schedule`]
//! owns schedules and occurrences, [`store`] the durable state, [`plan`] the
//! scheduler's per-tick decisions, [`history`] a job's recent runs (grouped
//! attempts plus what each run posted), and [`migrate`] the move from the old
//! `automations` layout.

pub mod history;
pub mod migrate;
pub mod plan;
pub mod schedule;
pub mod store;

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use chrono::{DateTime, Local, TimeDelta, TimeZone, Utc};
use serde::{Deserialize, Serialize};

pub use self::schedule::{JobSchedule, ScheduleKind};
use self::schedule::{MAX_GRACE_SECS, MIN_GRACE_SECS, next_recurring_after, once_occurrence};
pub use self::store::{ArchivedJob, JobsHome, LastRun, Outcome, RunTrigger};
use crate::config::{Config, paths};
use crate::models::ModelSpec;
use crate::subagents::{self, SubagentDefinition};

/// Source location for a job definition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobSource {
    /// `<ZDX_HOME>/jobs/*.md`
    User,
}

impl JobSource {
    /// Returns a short display label.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::User => "user",
        }
    }
}

/// Parsed job definition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobDefinition {
    /// Stable job name (file stem).
    pub name: String,
    /// Absolute path to the source markdown file.
    pub path: PathBuf,
    /// Which directory this job came from.
    pub source: JobSource,
    /// When the job runs.
    pub schedule: JobSchedule,
    /// Declared `grace_secs`; [`JobDefinition::grace`] applies the default.
    pub grace_secs: Option<u32>,
    /// Optional model override for this job run.
    pub model: Option<String>,
    /// Optional named subagent for this job run.
    pub subagent: Option<String>,
    /// Optional working root, as written in frontmatter (`~/...` or absolute).
    /// Expanded and checked by [`resolve_run`], so a bad root fails only this
    /// job instead of discovery.
    pub root: Option<String>,
    /// Optional per-run timeout in seconds.
    pub timeout_secs: Option<u32>,
    /// Number of retries after the initial attempt.
    pub max_retries: u32,
    /// Prompt body (markdown content after frontmatter).
    pub prompt: String,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
struct JobFrontmatter {
    schedule: Option<String>,
    run_at: Option<String>,
    grace_secs: Option<u32>,
    model: Option<String>,
    subagent: Option<String>,
    root: Option<String>,
    timeout_secs: Option<u32>,
    max_retries: Option<u32>,
}

/// Which layer supplied a resolved job setting.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SettingOrigin {
    /// The job's own frontmatter.
    Job,
    /// The named subagent definition's `model`.
    Subagent,
    /// `[subagents.overrides.<name>]` in config.
    SubagentOverride,
    /// Inherited from the config layers in effect for the run: the caller's
    /// config, or the declared root's layers when `root` is set.
    Config,
    /// Inherited from the caller's root: the process that resolved the
    /// settings (`zdx jobs run|validate|list --root`).
    Caller,
    /// Inherited from the scheduled daemon's root (see
    /// [`crate::job_daemon`]), whichever process asks.
    Daemon,
}

impl SettingOrigin {
    /// Stable label, matching the serialized form.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Job => "job",
            Self::Subagent => "subagent",
            Self::SubagentOverride => "subagent_override",
            Self::Config => "config",
            Self::Caller => "caller",
            Self::Daemon => "daemon",
        }
    }

    /// Whether the value was inherited rather than set for this job
    /// or its subagent.
    #[must_use]
    pub const fn is_inherited(self) -> bool {
        matches!(self, Self::Config | Self::Caller | Self::Daemon)
    }
}

/// The working root a job runs in.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ResolvedJobRoot {
    pub path: PathBuf,
    /// `Job` when set in frontmatter, otherwise the inherited context's
    /// origin (`Caller` or `Daemon`).
    pub origin: SettingOrigin,
}

/// The model a job runs with.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ResolvedJobModel {
    /// Canonical spec with the thinking level inline, e.g.
    /// `claude-cli@work:claude-opus-5-5@high`.
    pub spec: String,
    /// Provider id, or the custom provider's prefix.
    pub provider: String,
    /// Named OAuth account; `None` means the provider's default account.
    pub account: Option<String>,
    /// Bare model id without provider or modifiers.
    pub model: String,
    /// Requested thinking level. Providers may still serve a model without
    /// reasoning at `off`.
    pub thinking: String,
    pub fast: bool,
    pub origin: SettingOrigin,
    /// Layer that supplied the thinking level; differs from `origin` when the
    /// winning spec had no `@<level>` suffix and inherited it.
    pub thinking_origin: SettingOrigin,
}

/// Resolved settings for one job, shared by every surface.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct JobRunSettings {
    pub root: ResolvedJobRoot,
    pub model: ResolvedJobModel,
    pub subagent: Option<String>,
}

/// A resolved job run: the config to run with plus its reported settings.
#[derive(Debug, Clone)]
pub struct ResolvedJobRun {
    pub config: Config,
    pub subagent: Option<SubagentDefinition>,
    pub settings: JobRunSettings,
}

/// Where a job stands now, derived from its schedule and durable state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum JobStatus {
    /// The file does not parse; the job cannot run.
    Invalid,
    /// No schedule: runs only with `zdx jobs run`.
    Manual,
    /// Recurring; `next_run_at` is the next occurrence.
    Scheduled,
    /// Once; `run_at` is in the future.
    Pending,
    /// Once; `run_at` passed and the job is still within its grace.
    Due,
    /// Once; past its grace without a run. The scheduler records it as
    /// missed and archives it.
    Expired,
    /// A run holds the job's lock.
    Running,
    /// Once; its run stopped without a result. The scheduler records it as
    /// interrupted and archives it.
    Interrupted,
    /// Once; its occurrence already has a final outcome (see `last_run`).
    Finished,
}

impl JobStatus {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Invalid => "invalid",
            Self::Manual => "manual",
            Self::Scheduled => "scheduled",
            Self::Pending => "pending",
            Self::Due => "due",
            Self::Expired => "expired",
            Self::Running => "running",
            Self::Interrupted => "interrupted",
            Self::Finished => "finished",
        }
    }
}

/// Live state of one job, shared by every listing surface.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct JobState {
    pub status: JobStatus,
    /// Next scheduled instant: the next recurring occurrence, or `run_at` of
    /// a once job that has not run yet (in the past while due or expired).
    pub next_run_at: Option<DateTime<Utc>>,
    /// Latest final outcome, from any trigger.
    pub last_run: Option<LastRun>,
}

impl JobState {
    fn invalid() -> Self {
        Self {
            status: JobStatus::Invalid,
            next_run_at: None,
            last_run: None,
        }
    }
}

/// Derives `job`'s live state from its schedule, claim, lock, and last
/// outcome. Reads a few small files; no scan.
#[must_use]
pub fn job_state<Tz: TimeZone>(
    home: &JobsHome,
    job: &JobDefinition,
    now: DateTime<Utc>,
    tz: &Tz,
) -> JobState {
    let mut last_run = home.last_run(&job.name);
    let running = home.is_job_running(&job.name);
    let (status, next_run_at) = match &job.schedule {
        JobSchedule::Manual => (JobStatus::Manual, None),
        JobSchedule::Recurring { parsed, .. } => {
            (JobStatus::Scheduled, next_recurring_after(parsed, now, tz))
        }
        JobSchedule::Once { run_at } => {
            let occurrence = once_occurrence(*run_at);
            let claim = home.read_claim(&job.name, &occurrence.key).ok().flatten();
            // The final claim wins over a `last.json` a crash left stale.
            last_run =
                store::newer_last_run(last_run, claim.as_ref().and_then(store::Claim::to_last_run));
            let status = match claim {
                Some(claim) if claim.outcome.is_some() => JobStatus::Finished,
                Some(_) if running => JobStatus::Running,
                Some(_) => JobStatus::Interrupted,
                None if occurrence.at > now => JobStatus::Pending,
                None if job
                    .grace()
                    .is_some_and(|grace| now - occurrence.at <= grace) =>
                {
                    JobStatus::Due
                }
                None => JobStatus::Expired,
            };
            let next = matches!(
                status,
                JobStatus::Pending | JobStatus::Due | JobStatus::Expired
            )
            .then_some(occurrence.at);
            (status, next)
        }
    };
    let status = if running && status != JobStatus::Finished {
        JobStatus::Running
    } else {
        status
    };
    JobState {
        status,
        next_run_at,
        last_run,
    }
}

/// Short listing of one job without resolving its model or root, for
/// surfaces that refresh often (the Mini App).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct JobSummary {
    pub name: String,
    /// `None` when the file is invalid.
    pub schedule: Option<JobSchedule>,
    #[serde(flatten)]
    pub state: JobState,
    pub error: Option<String>,
}

/// Summaries of every job file, valid or not.
///
/// # Errors
/// Returns an error when the jobs directory cannot be listed or the legacy
/// `automations` directory still holds job files.
pub fn summaries(home: &JobsHome, now: DateTime<Utc>) -> Result<Vec<JobSummary>> {
    let catalog = catalog(home)?;
    let mut out: Vec<JobSummary> = catalog
        .jobs
        .iter()
        .map(|job| JobSummary {
            name: job.name.clone(),
            schedule: Some(job.schedule.clone()),
            state: job_state(home, job, now, &Local),
            error: None,
        })
        .chain(catalog.invalid.iter().map(|invalid| JobSummary {
            name: invalid.name.clone(),
            schedule: None,
            state: JobState::invalid(),
            error: Some(invalid.error.clone()),
        }))
        .collect();
    out.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(out)
}

/// Declared and resolved view of one job for listing surfaces.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct JobOverview {
    pub name: String,
    pub path: PathBuf,
    /// `None` when the file is invalid; `error` then says why.
    pub schedule: Option<JobSchedule>,
    /// Effective grace in seconds; `None` for manual and invalid jobs.
    pub grace_secs: Option<u32>,
    #[serde(flatten)]
    pub state: JobState,
    pub declared_root: Option<String>,
    pub declared_model: Option<String>,
    pub subagent: Option<String>,
    pub timeout_secs: Option<u32>,
    pub max_retries: u32,
    /// `None` when resolution failed; `error` then says why.
    pub settings: Option<JobRunSettings>,
    pub error: Option<String>,
}

/// The config and root a job without its own `root` inherits.
#[derive(Debug, Clone, Copy)]
pub struct InheritedContext<'a> {
    pub config: &'a Config,
    pub root: &'a Path,
    /// Reported origin of an inherited root: `Caller` or `Daemon`.
    pub root_origin: SettingOrigin,
}

/// Resolves the root, config, subagent, and model one job runs with.
///
/// `base` and `caller_root` are the caller's config and root. Without a
/// `root` in frontmatter the run uses both unchanged. With one, the root must
/// be an existing directory and config layers are loaded for that root, so
/// its workspace overlays apply. Model layers apply in order: config →
/// subagent definition → `[subagents.overrides.<name>]` → job `model`;
/// a suffixless spec inherits the previous thinking level.
///
/// # Errors
/// Returns an error if the root is invalid or missing, config layers for the
/// root fail to load, or the named subagent cannot be resolved.
pub fn resolve_run(
    base: &Config,
    caller_root: &Path,
    job: &JobDefinition,
) -> Result<ResolvedJobRun> {
    resolve_run_from(
        Ok(InheritedContext {
            config: base,
            root: caller_root,
            root_origin: SettingOrigin::Caller,
        }),
        job,
    )
}

/// Like [`resolve_run`], with the inherited context supplied by the caller.
/// `Err(reason)` means no inherited context is known: a job that
/// declares its own `root` still resolves, any other fails with `reason`.
///
/// # Errors
/// As [`resolve_run`], plus `reason` for a job that needs the
/// missing inherited context.
pub fn resolve_run_from(
    inherited: std::result::Result<InheritedContext<'_>, &str>,
    job: &JobDefinition,
) -> Result<ResolvedJobRun> {
    let (root, mut config) = if let Some(raw) = job.root.as_deref() {
        let path = resolve_declared_root(raw, paths::home_dir().as_deref())?;
        let config = Config::load_layered(&paths::config_layer_paths_for(&path))
            .with_context(|| format!("load config for root {}", path.display()))?;
        (
            ResolvedJobRoot {
                path,
                origin: SettingOrigin::Job,
            },
            config,
        )
    } else {
        let inherited = inherited.map_err(|reason| anyhow::anyhow!("{reason}"))?;
        (
            ResolvedJobRoot {
                path: inherited.root.to_path_buf(),
                origin: inherited.root_origin,
            },
            inherited.config.clone(),
        )
    };

    let mut model_origin = SettingOrigin::Config;
    let mut thinking_origin = SettingOrigin::Config;
    let mut apply = |config: &mut Config, spec: &str, origin: SettingOrigin| {
        config.apply_model_spec(spec);
        model_origin = origin;
        if ModelSpec::parse(spec).thinking.is_some() {
            thinking_origin = origin;
        }
    };

    let subagent = match job.subagent.as_deref() {
        Some(name) => {
            let definition = subagents::resolve_named(&root.path, name, None)
                .with_context(|| format!("load subagent '{name}'"))?;
            if let Some(model) = definition.model.as_deref() {
                apply(&mut config, model, SettingOrigin::Subagent);
            }
            if let Some(model) = config
                .subagents
                .overrides
                .get(name)
                .and_then(|over| over.model.clone())
            {
                apply(&mut config, &model, SettingOrigin::SubagentOverride);
            }
            Some(definition)
        }
        None => None,
    };
    if let Some(model) = job.model.as_deref() {
        apply(&mut config, model, SettingOrigin::Job);
    }

    let model = describe_model(&config, model_origin, thinking_origin);
    Ok(ResolvedJobRun {
        config,
        subagent,
        settings: JobRunSettings {
            root,
            model,
            subagent: job.subagent.clone(),
        },
    })
}

/// Resolves only the reported settings of one job.
///
/// # Errors
/// Same as [`resolve_run`].
pub fn resolve_run_settings(
    base: &Config,
    caller_root: &Path,
    job: &JobDefinition,
) -> Result<JobRunSettings> {
    resolve_run(base, caller_root, job).map(|run| run.settings)
}

/// Builds the listing view of one job, resolved for the caller. Resolution
/// errors are kept in `error` so one bad job does not hide the others.
#[must_use]
pub fn overview(
    base: &Config,
    caller_root: &Path,
    home: &JobsHome,
    job: &JobDefinition,
) -> JobOverview {
    overview_from(
        Ok(InheritedContext {
            config: base,
            root: caller_root,
            root_origin: SettingOrigin::Caller,
        }),
        job,
        job_state(home, job, Utc::now(), &Local),
    )
}

/// Like [`overview`], with the inherited context supplied by the caller (see
/// [`resolve_run_from`]) and the job's live state.
#[must_use]
pub fn overview_from(
    inherited: std::result::Result<InheritedContext<'_>, &str>,
    job: &JobDefinition,
    state: JobState,
) -> JobOverview {
    let (settings, error) = match resolve_run_from(inherited, job) {
        Ok(run) => (Some(run.settings), None),
        Err(error) => (None, Some(format!("{error:#}"))),
    };
    JobOverview {
        name: job.name.clone(),
        path: job.path.clone(),
        schedule: Some(job.schedule.clone()),
        grace_secs: job.effective_grace_secs(),
        state,
        declared_root: job.root.clone(),
        declared_model: job.model.clone(),
        subagent: job.subagent.clone(),
        timeout_secs: job.timeout_secs,
        max_retries: job.max_retries,
        settings,
        error,
    }
}

/// The listing view of a job file that does not parse.
#[must_use]
pub fn invalid_overview(invalid: &InvalidJob) -> JobOverview {
    JobOverview {
        name: invalid.name.clone(),
        path: invalid.path.clone(),
        schedule: None,
        grace_secs: None,
        state: JobState::invalid(),
        declared_root: None,
        declared_model: None,
        subagent: None,
        timeout_secs: None,
        max_retries: 0,
        settings: None,
        error: Some(invalid.error.clone()),
    }
}

fn resolve_declared_root(raw: &str, home: Option<&Path>) -> Result<PathBuf> {
    let path = expand_root(raw, home)?;
    if !path.is_dir() {
        bail!(
            "root '{}' does not exist or is not a directory",
            path.display()
        );
    }
    Ok(path)
}

fn expand_root(raw: &str, home: Option<&Path>) -> Result<PathBuf> {
    if raw.is_empty() {
        bail!("root cannot be empty");
    }
    let home_required = || {
        home.map(Path::to_path_buf)
            .ok_or_else(|| anyhow::anyhow!("cannot expand root '{raw}': home directory is unknown"))
    };
    let path = if raw == "~" {
        home_required()?
    } else if let Some(rest) = raw.strip_prefix("~/") {
        home_required()?.join(rest)
    } else {
        PathBuf::from(raw)
    };
    if !path.is_absolute() {
        bail!("root '{raw}' must be an absolute path or start with '~/'");
    }
    Ok(path)
}

fn describe_model(
    config: &Config,
    origin: SettingOrigin,
    thinking_origin: SettingOrigin,
) -> ResolvedJobModel {
    let spec = crate::models::format_model_thinking(&config.model, config.thinking_level);
    let parsed = ModelSpec::parse(&spec);
    let (provider, account, model) =
        if let Some((_, bare)) = config.providers.custom_provider_for_model(parsed.base) {
            let provider = parsed
                .base
                .split_once(':')
                .or_else(|| parsed.base.split_once('/'))
                .map(|(prefix, _)| prefix.trim().to_string())
                .unwrap_or_default();
            (provider, None, bare)
        } else {
            let selection = crate::providers::resolve_provider(&spec);
            (
                selection.kind.id().to_string(),
                selection.account,
                selection.model,
            )
        };
    ResolvedJobModel {
        provider,
        account,
        model,
        thinking: config.thinking_level.display_name().to_string(),
        fast: parsed.fast,
        origin,
        thinking_origin,
        spec,
    }
}

impl JobDefinition {
    /// Effective grace in seconds; `None` for manual jobs.
    #[must_use]
    pub fn effective_grace_secs(&self) -> Option<u32> {
        match self.schedule {
            JobSchedule::Manual => None,
            _ => self.grace_secs.or(self.schedule.default_grace_secs()),
        }
    }

    /// Effective grace; `None` for manual jobs.
    #[must_use]
    pub fn grace(&self) -> Option<TimeDelta> {
        self.effective_grace_secs()
            .map(|secs| TimeDelta::seconds(i64::from(secs)))
    }
}

/// A job file that does not parse. It fails alone: the other jobs still run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct InvalidJob {
    pub name: String,
    pub path: PathBuf,
    pub error: String,
}

/// Every job file in `$ZDX_HOME/jobs/`, split into valid and invalid.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct JobCatalog {
    /// Valid jobs, sorted by name.
    pub jobs: Vec<JobDefinition>,
    /// Files that failed to parse, sorted by name.
    pub invalid: Vec<InvalidJob>,
}

impl JobCatalog {
    /// Finds a job by name, or explains why it cannot run.
    ///
    /// # Errors
    /// Returns an error when the name is empty, unknown, or its file is
    /// invalid.
    pub fn find(&self, name: &str) -> Result<&JobDefinition> {
        let name = name.trim();
        if name.is_empty() {
            bail!("Job name cannot be empty");
        }
        if let Some(job) = self.jobs.iter().find(|job| job.name == name) {
            return Ok(job);
        }
        if let Some(invalid) = self.invalid.iter().find(|job| job.name == name) {
            bail!(
                "job '{name}' is invalid ({}): {}",
                invalid.path.display(),
                invalid.error
            );
        }
        bail!("Job '{name}' not found")
    }
}

/// Reads every job file in `home`. A file that does not parse is reported in
/// [`JobCatalog::invalid`] and does not affect the others.
///
/// # Errors
/// Returns an error when the jobs directory cannot be listed, or when the old
/// `$ZDX_HOME/automations/` directory still holds job files (run
/// `zdx jobs migrate`).
pub fn catalog(home: &JobsHome) -> Result<JobCatalog> {
    migrate::ensure_migrated(home)?;
    let mut by_name: BTreeMap<String, JobDefinition> = BTreeMap::new();
    let mut invalid = Vec::new();
    for path in markdown_files(&home.jobs_dir())? {
        let name = file_stem(&path).unwrap_or_else(|_| path.display().to_string());
        match parse_job_file(&path, JobSource::User) {
            Ok(definition) => {
                if let Some(existing) = by_name.get(&definition.name) {
                    invalid.push(InvalidJob {
                        name,
                        path: path.clone(),
                        error: format!(
                            "duplicate job name: '{}' already defines it",
                            existing.path.display()
                        ),
                    });
                } else {
                    by_name.insert(definition.name.clone(), definition);
                }
            }
            Err(err) => invalid.push(InvalidJob {
                name,
                path,
                error: format!("{err:#}"),
            }),
        }
    }
    invalid.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(JobCatalog {
        jobs: by_name.into_values().collect(),
        invalid,
    })
}

/// Loads one valid job by file-stem name.
///
/// # Errors
/// Returns an error when the catalog cannot be read or the job is unknown or
/// invalid.
pub fn load_by_name(home: &JobsHome, name: &str) -> Result<JobDefinition> {
    catalog(home)?.find(name).cloned()
}

/// Markdown files directly in `dir` (not recursive), sorted.
pub(crate) fn markdown_files(dir: &Path) -> Result<Vec<PathBuf>> {
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let mut files: Vec<PathBuf> = fs::read_dir(dir)
        .with_context(|| format!("read job dir {}", dir.display()))?
        .filter_map(std::result::Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.is_file()
                && path
                    .extension()
                    .and_then(std::ffi::OsStr::to_str)
                    .is_some_and(|ext| ext.eq_ignore_ascii_case("md"))
        })
        .collect();
    files.sort();
    Ok(files)
}

fn parse_job_file(path: &Path, source: JobSource) -> Result<JobDefinition> {
    let content =
        fs::read_to_string(path).with_context(|| format!("read job file {}", path.display()))?;
    parse_job_content(path, &content, source)
}

pub(crate) fn parse_job_content(
    path: &Path,
    content: &str,
    source: JobSource,
) -> Result<JobDefinition> {
    let (yaml, body) = split_frontmatter(content)?;

    let frontmatter: JobFrontmatter = if yaml.trim().is_empty() {
        JobFrontmatter::default()
    } else {
        serde_yaml::from_str(&yaml)
            .with_context(|| format!("parse YAML frontmatter in {}", path.display()))?
    };

    let name = file_stem(path)?;
    let cron = normalize_optional_string(frontmatter.schedule, "schedule")?;
    let run_at = normalize_optional_string(frontmatter.run_at, "run_at")?;
    let schedule = JobSchedule::parse(cron.as_deref(), run_at.as_deref())?;
    let model = normalize_optional_string(frontmatter.model, "model")?;
    let subagent = normalize_optional_string(frontmatter.subagent, "subagent")?;
    // Root problems (including empty) are reported by `resolve_run`, so they
    // fail only this job rather than discovery of every job.
    let root = frontmatter.root.map(|raw| raw.trim().to_string());

    if matches!(frontmatter.timeout_secs, Some(0)) {
        bail!("timeout_secs must be greater than zero");
    }
    if let Some(grace) = frontmatter.grace_secs {
        if schedule == JobSchedule::Manual {
            bail!("grace_secs needs `schedule` or `run_at`");
        }
        if !(MIN_GRACE_SECS..=MAX_GRACE_SECS).contains(&grace) {
            bail!("grace_secs must be between {MIN_GRACE_SECS} and {MAX_GRACE_SECS}");
        }
    }

    let prompt = body.trim().to_string();
    if prompt.is_empty() {
        bail!("Job prompt body cannot be empty");
    }

    Ok(JobDefinition {
        name,
        path: path.to_path_buf(),
        source,
        schedule,
        grace_secs: frontmatter.grace_secs,
        model,
        subagent,
        root,
        timeout_secs: frontmatter.timeout_secs,
        max_retries: frontmatter.max_retries.unwrap_or(0),
        prompt,
    })
}

/// A change to a job's declared `model`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelEdit<'a> {
    /// Set `model:` to this spec (`provider[@account]:model[@thinking][@fast]`).
    Set(&'a str),
    /// Remove `model:`, so the job inherits the model again.
    Clear,
}

/// Changes the `model` key of the job file at `path` and returns the
/// updated definition.
///
/// Only the `model:` line in the frontmatter changes (an inline comment on it
/// is kept); every other key, comment, and the prompt body stay byte for
/// byte. A new key is added as the last frontmatter line. The result is
/// parsed before it is written and must differ from the original only in
/// `model`, so a file this edit cannot handle (for example a multi-line
/// `model` value) fails without changes.
///
/// `expected` guards against concurrent edits: `Some(value)` requires the
/// declared model on disk to equal `value` (`Some(None)` = not set). The file
/// is also re-read just before the atomic rename, and the write is abandoned
/// if it changed in between.
///
/// # Errors
/// Returns an error for an invalid spec, a file that changed or cannot be
/// edited safely, or an I/O failure. The original file is left intact.
pub fn set_model(
    path: &Path,
    expected: Option<Option<&str>>,
    edit: ModelEdit<'_>,
) -> Result<JobDefinition> {
    let source = JobSource::User;
    let original =
        fs::read_to_string(path).with_context(|| format!("read job file {}", path.display()))?;
    let before = parse_job_content(path, &original, source)
        .with_context(|| format!("parse job {}", path.display()))?;
    if let Some(expected) = expected
        && before.model.as_deref() != expected
    {
        bail!(
            "job '{}' changed on disk (model is now {}); reload and try again",
            before.name,
            before.model.as_deref().unwrap_or("not set")
        );
    }

    let spec = match edit {
        ModelEdit::Set(spec) => Some(validate_model_spec(spec)?),
        ModelEdit::Clear => None,
    };
    if before.model.as_deref() == spec {
        return Ok(before);
    }

    let updated = edit_model_line(&original, spec)?;
    let after = parse_job_content(path, &updated, source)
        .context("the edited job no longer parses; file left unchanged")?;
    let expected_after = JobDefinition {
        model: spec.map(str::to_string),
        ..before.clone()
    };
    if after != expected_after {
        bail!(
            "cannot edit the model of '{}' safely; file left unchanged",
            before.name
        );
    }

    write_if_unchanged(path, &original, &updated)?;
    Ok(after)
}

fn validate_model_spec(spec: &str) -> Result<&str> {
    let spec = spec.trim();
    if spec.is_empty() {
        bail!("model cannot be empty");
    }
    if spec
        .chars()
        .any(|c| c == '"' || c == '\\' || c.is_control() || c.is_whitespace())
    {
        bail!("invalid model spec '{spec}'");
    }
    Ok(spec)
}

/// Replaces, removes, or appends the top-level `model:` frontmatter line.
fn edit_model_line(content: &str, spec: Option<&str>) -> Result<String> {
    let lines: Vec<&str> = content.split_inclusive('\n').collect();
    let close = lines
        .iter()
        .enumerate()
        .skip(1)
        .find(|(_, line)| matches!(line.trim(), "---" | "..."))
        .map(|(index, _)| index)
        .context("Unterminated YAML frontmatter")?;
    let model_lines: Vec<usize> = (1..close)
        .filter(|&index| is_model_key_line(lines[index]))
        .collect();
    if model_lines.len() > 1 {
        bail!("frontmatter has more than one `model` key");
    }
    let newline = if lines[0].ends_with("\r\n") {
        "\r\n"
    } else {
        "\n"
    };

    let mut out: Vec<String> = lines.iter().map(|line| (*line).to_string()).collect();
    match (model_lines.first(), spec) {
        (Some(&index), Some(spec)) => {
            let line = lines[index];
            let ending = &line[line.trim_end_matches(['\r', '\n']).len()..];
            let comment = inline_comment(line.trim_end_matches(['\r', '\n']));
            out[index] = format!("model: \"{spec}\"{comment}{ending}");
        }
        (Some(&index), None) => {
            out.remove(index);
        }
        (None, Some(spec)) => {
            out.insert(close, format!("model: \"{spec}\"{newline}"));
        }
        (None, None) => {}
    }
    Ok(out.concat())
}

fn is_model_key_line(line: &str) -> bool {
    line.strip_prefix("model")
        .is_some_and(|rest| rest.trim_start_matches([' ', '\t']).starts_with(':'))
}

/// The ` # comment` tail of a `model: value` line, or an empty string.
fn inline_comment(line: &str) -> &str {
    let value = line[line.find(':').map_or(line.len(), |i| i + 1)..].trim_start();
    let offset = line.len() - value.len();
    let search_from = match value.chars().next() {
        Some(quote @ ('"' | '\'')) => value[1..].find(quote).map_or(value.len(), |end| end + 2),
        _ => 0,
    };
    value[search_from..]
        .find(" #")
        .map_or("", |index| line[offset + search_from + index..].trim_end())
}

fn write_if_unchanged(path: &Path, original: &str, updated: &str) -> Result<()> {
    let file_name = path
        .file_name()
        .and_then(std::ffi::OsStr::to_str)
        .context("invalid job file name")?;
    let tmp = path.with_file_name(format!(".{file_name}.{}.tmp", std::process::id()));
    fs::write(&tmp, updated).with_context(|| format!("write {}", tmp.display()))?;
    let result = (|| {
        if let Ok(metadata) = fs::metadata(path) {
            fs::set_permissions(&tmp, metadata.permissions())
                .with_context(|| format!("copy permissions to {}", tmp.display()))?;
        }
        let current = fs::read_to_string(path)
            .with_context(|| format!("re-read job file {}", path.display()))?;
        if current != original {
            bail!(
                "{} changed on disk while saving; reload and try again",
                path.display()
            );
        }
        fs::rename(&tmp, path).with_context(|| format!("replace {}", path.display()))
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

fn file_stem(path: &Path) -> Result<String> {
    let stem = path
        .file_stem()
        .and_then(std::ffi::OsStr::to_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| anyhow::anyhow!("Invalid job file name: {}", path.display()))?;
    Ok(stem.to_string())
}

fn normalize_optional_string(value: Option<String>, field: &str) -> Result<Option<String>> {
    match value {
        Some(raw) => {
            let trimmed = raw.trim();
            if trimmed.is_empty() {
                bail!("{field} cannot be empty");
            }
            Ok(Some(trimmed.to_string()))
        }
        None => Ok(None),
    }
}

fn split_frontmatter(content: &str) -> Result<(String, String)> {
    let content = content.trim_start_matches('\u{feff}');
    let lines: Vec<&str> = content.lines().collect();

    let Some(first) = lines.first() else {
        bail!("Missing YAML frontmatter");
    };

    if first.trim() != "---" {
        bail!("Missing YAML frontmatter");
    }

    for idx in 1..lines.len() {
        let trimmed = lines[idx].trim();
        if trimmed == "---" || trimmed == "..." {
            let yaml = lines[1..idx].join("\n");
            let body = lines[idx + 1..].join("\n");
            return Ok((yaml, body));
        }
    }

    bail!("Unterminated YAML frontmatter")
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::*;

    #[test]
    fn parse_minimal_job_defaults() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("morning-report.md");
        fs::write(
            &file,
            "---\n---\nGenerate morning report from recent threads.",
        )
        .unwrap();

        let parsed = parse_job_file(&file, JobSource::User).unwrap();
        assert_eq!(parsed.name, "morning-report");
        assert_eq!(parsed.schedule, JobSchedule::Manual);
        assert!(parsed.model.is_none());
        assert!(parsed.subagent.is_none());
        assert_eq!(parsed.max_retries, 0);
        assert_eq!(
            parsed.prompt,
            "Generate morning report from recent threads."
        );
    }

    #[test]
    fn parse_job_with_subagent() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("morning-report.md");
        fs::write(
            &file,
            "---\nsubagent: explorer\n---\nGenerate morning report from recent threads.",
        )
        .unwrap();

        let parsed = parse_job_file(&file, JobSource::User).unwrap();
        assert_eq!(parsed.subagent.as_deref(), Some("explorer"));
    }

    #[test]
    fn parse_job_with_root() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("scoped.md");
        fs::write(&file, "---\nroot: \"~/work/space\"\n---\nprompt").unwrap();

        let parsed = parse_job_file(&file, JobSource::User).unwrap();
        assert_eq!(parsed.root.as_deref(), Some("~/work/space"));
    }

    #[test]
    fn empty_root_fails_resolution_not_discovery() {
        let root = tempdir().unwrap();
        let home = JobsHome::at(root.path());
        let user_dir = home.jobs_dir();
        fs::create_dir_all(&user_dir).unwrap();
        fs::write(
            user_dir.join("empty-root.md"),
            "---\nroot: \"  \"\n---\nprompt",
        )
        .unwrap();
        fs::write(user_dir.join("fine.md"), "---\n---\nprompt").unwrap();

        let all = catalog(&home).unwrap().jobs;
        assert_eq!(all.len(), 2);
        let broken = all.iter().find(|a| a.name == "empty-root").unwrap();
        let view = overview(&Config::default(), root.path(), &home, broken);
        assert!(view.error.unwrap().contains("root cannot be empty"));
    }

    #[test]
    fn expand_root_accepts_home_and_absolute_and_rejects_relative() {
        let home = Path::new("/home/example");
        assert_eq!(
            expand_root("~/work", Some(home)).unwrap(),
            PathBuf::from("/home/example/work")
        );
        assert_eq!(
            expand_root("~", Some(home)).unwrap(),
            PathBuf::from("/home/example")
        );
        assert_eq!(
            expand_root("/srv/space", Some(home)).unwrap(),
            PathBuf::from("/srv/space")
        );
        assert!(
            expand_root("work/space", Some(home))
                .unwrap_err()
                .to_string()
                .contains("must be an absolute path")
        );
        assert!(
            expand_root("~other/space", Some(home))
                .unwrap_err()
                .to_string()
                .contains("must be an absolute path")
        );
        assert!(expand_root("~/work", None).is_err());
        assert!(
            expand_root("", Some(home))
                .unwrap_err()
                .to_string()
                .contains("root cannot be empty")
        );
    }

    #[test]
    fn declared_root_wins_and_missing_inherited_context_fails_only_undeclared() {
        let _home = crate::test_support::temp_zdx_home();
        let caller = tempdir().unwrap();
        let declared = tempdir().unwrap();
        let config = Config::default();
        let inherited = || {
            Ok(InheritedContext {
                config: &config,
                root: caller.path(),
                root_origin: SettingOrigin::Daemon,
            })
        };

        let plain = definition(Some("claude-cli:claude-opus-5@low"));
        let run = resolve_run_from(inherited(), &plain).unwrap();
        assert_eq!(run.settings.root.path, caller.path());
        assert_eq!(run.settings.root.origin, SettingOrigin::Daemon);

        let mut scoped = definition(Some("claude-cli:claude-opus-5@low"));
        scoped.root = Some(declared.path().to_string_lossy().to_string());
        let run = resolve_run_from(inherited(), &scoped).unwrap();
        assert_eq!(run.settings.root.path, declared.path());
        assert_eq!(run.settings.root.origin, SettingOrigin::Job);

        let unknown = Err("daemon not running");
        assert!(resolve_run_from(unknown, &scoped).is_ok());
        let err = resolve_run_from(unknown, &plain).unwrap_err();
        assert!(format!("{err:#}").contains("daemon not running"));

        scoped.root = Some(declared.path().join("gone").to_string_lossy().to_string());
        assert!(resolve_run_from(inherited(), &scoped).is_err());
    }

    fn definition(model: Option<&str>) -> JobDefinition {
        JobDefinition {
            name: "demo".to_string(),
            path: PathBuf::from("/tmp/demo.md"),
            source: JobSource::User,
            schedule: JobSchedule::parse(Some("0 8 * * *"), None).unwrap(),
            grace_secs: None,
            model: model.map(str::to_string),
            subagent: None,
            root: None,
            timeout_secs: None,
            max_retries: 0,
            prompt: "prompt".to_string(),
        }
    }

    #[test]
    fn resolve_run_settings_reports_inherited_and_explicit_model() {
        let caller = tempdir().unwrap();
        let config = Config {
            model: "claude-cli@work:claude-sonnet-5@high".to_string(),
            thinking_level: crate::config::ThinkingLevel::High,
            ..Default::default()
        };

        let inherited = resolve_run_settings(&config, caller.path(), &definition(None)).unwrap();
        assert_eq!(inherited.model.spec, "claude-cli@work:claude-sonnet-5@high");
        assert_eq!(inherited.model.provider, "claude-cli");
        assert_eq!(inherited.model.account.as_deref(), Some("work"));
        assert_eq!(inherited.model.model, "claude-sonnet-5");
        assert_eq!(inherited.model.thinking, "high");
        assert_eq!(inherited.model.origin, SettingOrigin::Config);
        assert!(inherited.model.origin.is_inherited());

        let suffixless = resolve_run_settings(
            &config,
            caller.path(),
            &definition(Some("claude-cli:claude-opus-5")),
        )
        .unwrap();
        assert_eq!(suffixless.model.spec, "claude-cli:claude-opus-5@high");
        assert_eq!(suffixless.model.account, None);
        assert_eq!(suffixless.model.origin, SettingOrigin::Job);
        assert_eq!(suffixless.model.thinking_origin, SettingOrigin::Config);

        let explicit = resolve_run_settings(
            &config,
            caller.path(),
            &definition(Some("claude-cli:claude-opus-5@low")),
        )
        .unwrap();
        assert_eq!(explicit.model.thinking, "low");
        assert_eq!(explicit.model.thinking_origin, SettingOrigin::Job);
    }

    #[test]
    fn overview_keeps_resolution_errors_per_job() {
        let caller = tempdir().unwrap();
        let mut broken = definition(None);
        broken.root = Some("relative/root".to_string());

        let home = JobsHome::at(caller.path());
        let view = overview(&Config::default(), caller.path(), &home, &broken);
        assert!(view.settings.is_none());
        assert!(
            view.error
                .as_deref()
                .unwrap()
                .contains("must be an absolute path")
        );
        assert_eq!(view.declared_root.as_deref(), Some("relative/root"));

        let json = serde_json::to_value(overview(
            &Config::default(),
            caller.path(),
            &home,
            &definition(None),
        ))
        .unwrap();
        assert_eq!(json["settings"]["root"]["origin"], "caller");
        assert_eq!(json["settings"]["model"]["origin"], "config");
    }

    const EDIT_FIXTURE: &str = "---\n# owner: example\nroot: \"~/work\"   # keep me\nmodel: claude-cli:claude-opus-5@high # current pick\nsubagent: helper\nmax_retries: 1\n---\n\nBody line one.\n\nmodel: not frontmatter\n";

    fn write_fixture(content: &str) -> (tempfile::TempDir, PathBuf) {
        let dir = tempdir().unwrap();
        let path = dir.path().join("job.md");
        fs::write(&path, content).unwrap();
        (dir, path)
    }

    #[test]
    fn set_model_replaces_only_the_model_line() {
        let (_dir, path) = write_fixture(EDIT_FIXTURE);
        let updated = set_model(
            &path,
            Some(Some("claude-cli:claude-opus-5@high")),
            ModelEdit::Set("claude-cli@work:claude-sonnet-5@low@fast"),
        )
        .unwrap();
        assert_eq!(
            updated.model.as_deref(),
            Some("claude-cli@work:claude-sonnet-5@low@fast")
        );
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            EDIT_FIXTURE.replace(
                "model: claude-cli:claude-opus-5@high # current pick",
                "model: \"claude-cli@work:claude-sonnet-5@low@fast\" # current pick"
            )
        );
    }

    #[test]
    fn clear_then_set_round_trips_without_touching_other_lines() {
        let (_dir, path) = write_fixture(EDIT_FIXTURE);
        let cleared = set_model(&path, None, ModelEdit::Clear).unwrap();
        assert!(cleared.model.is_none());
        let without =
            EDIT_FIXTURE.replace("model: claude-cli:claude-opus-5@high # current pick\n", "");
        assert_eq!(fs::read_to_string(&path).unwrap(), without);

        set_model(
            &path,
            Some(None),
            ModelEdit::Set("stepfun:step-3.7-flash@low"),
        )
        .unwrap();
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            without.replace(
                "max_retries: 1\n---",
                "max_retries: 1\nmodel: \"stepfun:step-3.7-flash@low\"\n---"
            )
        );
    }

    #[test]
    fn set_model_keeps_paused_job_and_crlf_line_endings() {
        let content = "---\r\ntimeout_secs: 60\r\n---\r\nPrompt\r\n";
        let (_dir, path) = write_fixture(content);
        let updated =
            set_model(&path, Some(None), ModelEdit::Set("gemini:gemini-3.8-flash")).unwrap();
        assert_eq!(updated.schedule, JobSchedule::Manual);
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "---\r\ntimeout_secs: 60\r\nmodel: \"gemini:gemini-3.8-flash\"\r\n---\r\nPrompt\r\n"
        );
    }

    #[test]
    fn set_model_refuses_stale_unsafe_or_invalid_edits_and_leaves_file_intact() {
        let (_dir, path) = write_fixture(EDIT_FIXTURE);
        let cases: [(Option<Option<&str>>, ModelEdit<'_>, &str); 4] = [
            (Some(None), ModelEdit::Set("a:b"), "changed on disk"),
            (
                Some(Some("other:model")),
                ModelEdit::Clear,
                "changed on disk",
            ),
            (None, ModelEdit::Set("a:b \"x"), "invalid model spec"),
            (None, ModelEdit::Set("  "), "model cannot be empty"),
        ];
        for (expected, edit, message) in cases {
            let err = set_model(&path, expected, edit).unwrap_err();
            assert!(format!("{err:#}").contains(message), "{err:#}");
            assert_eq!(fs::read_to_string(&path).unwrap(), EDIT_FIXTURE);
        }

        let block = "---\nmodel: >\n  claude-cli:claude-opus-5\n---\nPrompt\n";
        let (_dir2, block_path) = write_fixture(block);
        assert!(set_model(&block_path, None, ModelEdit::Set("a:b")).is_err());
        assert_eq!(fs::read_to_string(&block_path).unwrap(), block);
        let leftovers: Vec<_> = fs::read_dir(block_path.parent().unwrap())
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name().to_string_lossy().ends_with(".tmp"))
            .collect();
        assert!(leftovers.is_empty());
    }

    #[test]
    fn parse_requires_frontmatter() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("invalid.md");
        fs::write(&file, "no frontmatter").unwrap();

        let err = parse_job_file(&file, JobSource::User).unwrap_err();
        assert!(err.to_string().contains("Missing YAML frontmatter"));
    }

    #[test]
    fn catalog_lists_valid_jobs_and_isolates_invalid_files() {
        let dir = tempdir().unwrap();
        let home = JobsHome::at(dir.path());
        let jobs = home.jobs_dir();
        fs::create_dir_all(jobs.join("archive")).unwrap();
        fs::write(
            jobs.join("report.md"),
            "---\nschedule: \"0 8 * * *\"\n---\nprompt",
        )
        .unwrap();
        fs::write(
            jobs.join("remind.md"),
            "---\nrun_at: \"2026-10-20T09:00:00-03:00\"\ngrace_secs: 3600\n---\nprompt",
        )
        .unwrap();
        fs::write(
            jobs.join("typo.md"),
            "---\nschedul: \"0 8 * * *\"\n---\nprompt",
        )
        .unwrap();
        fs::write(
            jobs.join("bad-cron.md"),
            "---\nschedule: \"0 25 * * *\"\n---\nprompt",
        )
        .unwrap();
        fs::write(
            jobs.join("both.md"),
            "---\nschedule: \"0 8 * * *\"\nrun_at: \"2026-10-20T12:00:00Z\"\n---\nprompt",
        )
        .unwrap();
        fs::write(
            jobs.join("no-offset.md"),
            "---\nrun_at: \"2026-10-20T09:00:00\"\n---\nprompt",
        )
        .unwrap();
        fs::write(
            jobs.join("manual-grace.md"),
            "---\ngrace_secs: 600\n---\nprompt",
        )
        .unwrap();
        fs::write(
            jobs.join("archive").join("old--20261001T120000Z.md"),
            "---\n---\nprompt",
        )
        .unwrap();

        let catalog = catalog(&home).unwrap();
        let valid: Vec<_> = catalog.jobs.iter().map(|job| job.name.as_str()).collect();
        assert_eq!(valid, ["remind", "report"], "archive/ is not scanned");
        let invalid: Vec<_> = catalog
            .invalid
            .iter()
            .map(|job| job.name.as_str())
            .collect();
        assert_eq!(
            invalid,
            ["bad-cron", "both", "manual-grace", "no-offset", "typo"]
        );
        let find_err = |name: &str| format!("{:#}", catalog.find(name).unwrap_err());
        assert!(
            find_err("typo").contains("unknown field"),
            "{}",
            find_err("typo")
        );
        assert!(find_err("bad-cron").contains("out of range"));
        assert!(find_err("missing").contains("not found"));
        let remind = catalog.find("remind").unwrap();
        assert_eq!(remind.effective_grace_secs(), Some(3600));
        assert_eq!(
            catalog.find("report").unwrap().effective_grace_secs(),
            Some(600)
        );
    }

    #[test]
    fn once_job_state_follows_its_claim_and_the_clock() {
        use chrono::{FixedOffset, TimeZone as _};

        let dir = tempdir().unwrap();
        let home = JobsHome::at(dir.path());
        fs::create_dir_all(home.jobs_dir()).unwrap();
        fs::write(
            home.jobs_dir().join("remind.md"),
            "---\nrun_at: \"2026-10-20T12:00:00Z\"\n---\nprompt",
        )
        .unwrap();
        let job = catalog(&home).unwrap().find("remind").unwrap().clone();
        let tz = FixedOffset::east_opt(0).unwrap();
        let at = |h, m| Utc.with_ymd_and_hms(2026, 10, 20, h, m, 0).unwrap();

        let state = job_state(&home, &job, at(11, 0), &tz);
        assert_eq!(state.status, JobStatus::Pending);
        assert_eq!(state.next_run_at, Some(at(12, 0)));
        assert_eq!(
            job_state(&home, &job, at(13, 0), &tz).status,
            JobStatus::Due
        );
        assert_eq!(
            job_state(&home, &job, at(18, 1), &tz).status,
            JobStatus::Expired
        );

        let claim = store::Claim::new(
            "remind",
            "20261020T120000Z",
            RunTrigger::Manual,
            ScheduleKind::Once,
            at(12, 0),
        );
        home.create_claim(&claim).unwrap();
        assert_eq!(
            job_state(&home, &job, at(13, 0), &tz).status,
            JobStatus::Interrupted
        );
        let lock = home.try_lock_job("remind").unwrap().unwrap();
        assert_eq!(
            job_state(&home, &job, at(13, 0), &tz).status,
            JobStatus::Running
        );
        drop(lock);
        let finished = claim.finished(Outcome::Succeeded, None);
        home.update_claim(&finished).unwrap();
        // A crash before `last.json` was written: the claim still shows.
        let state = job_state(&home, &job, at(13, 0), &tz);
        assert_eq!(state.status, JobStatus::Finished);
        assert_eq!(state.next_run_at, None);
        assert_eq!(state.last_run.unwrap().run_id, finished.run_id);
    }
}
