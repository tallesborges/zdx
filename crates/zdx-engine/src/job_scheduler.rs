//! Where the jobs scheduler runs, and the settings it
//! resolves for each job.
//!
//! Surfaces must show the scheduler's context, never their own working
//! directory. The context comes from one of two sources, reported as-is:
//! - `Running`: the live scheduler (the Go jobs program) wrote
//!   `run/scheduler.context.json` at start, and its PID still owns the scheduler
//!   PID file.
//! - `Installed`: the launchd plist, which applies at the next start. Used when
//!   the scheduler is stopped, or running without a context record (an older
//!   build).
//!
//! Otherwise the context is `Unavailable` with a reason, and jobs that
//! do not declare their own `root` stay unresolved instead of falling back to
//! the caller's root.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::Result;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::config::{Config, paths};
use crate::jobs::{self, InheritedContext, JobRunSettings, JobSpec, SettingOrigin};
use crate::pidfile::{self, ServiceStatus};
use crate::service::{self, InstalledLaunch, Service};

const RECORD_FILE: &str = "scheduler.context.json";

/// What the running scheduler records about itself at start.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct SchedulerRecord {
    pub pid: u32,
    /// `--root` the scheduler resolves inherited jobs against.
    pub root: PathBuf,
    /// Working directory the scheduler loads config layers from.
    pub config_dir: PathBuf,
    pub started_at: DateTime<Utc>,
}

/// Where the scheduler's settings come from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SchedulerContextSource {
    /// Reported by the running scheduler process.
    Running,
    /// Read from the installed launchd service; applies at its next start.
    Installed,
}

/// The root and config directory the scheduler resolves jobs with.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SchedulerContext {
    pub source: SchedulerContextSource,
    pub root: PathBuf,
    pub config_dir: PathBuf,
    /// PID of the running scheduler, if one is running.
    pub pid: Option<u32>,
    /// Start time reported by the running scheduler (`Running` only).
    pub started_at: Option<DateTime<Utc>>,
    /// Why the context is not the running process's own report, if it is not.
    pub note: Option<String>,
}

/// Scheduler context, or why it is not known.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum SchedulerContextState {
    Available { context: SchedulerContext },
    Unavailable { reason: String },
}

/// Finds the scheduler's context from the PID file, its record, and the
/// installed service.
#[must_use]
pub fn discover() -> SchedulerContextState {
    let running_pid = match pidfile::status(Service::Scheduler.name()) {
        ServiceStatus::Running { pid, .. } => Some(pid),
        ServiceStatus::Stopped => None,
    };
    let installed = service::installed_launch(Service::Scheduler)
        .map(|launch| launch.map_err(|err| format!("{err:#}")));
    discover_from(DiscoveryInputs {
        running_pid,
        record: read_record_at(&record_path()),
        installed,
        zdx_home: paths::zdx_home(),
    })
}

/// Inputs to [`discover`], separated so the decision is testable.
#[derive(Debug, Clone)]
pub struct DiscoveryInputs {
    pub running_pid: Option<u32>,
    pub record: Option<SchedulerRecord>,
    /// `None` when no service is installed.
    pub installed: Option<std::result::Result<InstalledLaunch, String>>,
    /// `ZDX_HOME` of the process asking.
    pub zdx_home: PathBuf,
}

/// Decides the scheduler context from already-read inputs.
#[must_use]
pub fn discover_from(inputs: DiscoveryInputs) -> SchedulerContextState {
    let unavailable = |reason: String| SchedulerContextState::Unavailable { reason };

    if let (Some(pid), Some(record)) = (inputs.running_pid, inputs.record.as_ref())
        && record.pid == pid
    {
        return SchedulerContextState::Available {
            context: SchedulerContext {
                source: SchedulerContextSource::Running,
                root: record.root.clone(),
                config_dir: record.config_dir.clone(),
                pid: Some(pid),
                started_at: Some(record.started_at),
                note: None,
            },
        };
    }

    let note = match inputs.running_pid {
        Some(pid) => format!(
            "scheduler PID {pid} is running but did not report its settings (it predates \
             this report); showing the installed service, which applies at its next start"
        ),
        None => "scheduler is not running; showing the installed service, which applies when \
                 it starts"
            .to_string(),
    };
    let launch = match inputs.installed {
        Some(Ok(launch)) => launch,
        Some(Err(err)) => return unavailable(format!("cannot read the scheduler service: {err}")),
        None => {
            return unavailable(match inputs.running_pid {
                Some(pid) => format!(
                    "scheduler PID {pid} is running but did not report its settings, and no \
                     scheduler service is installed"
                ),
                None => {
                    "scheduler is not running and no scheduler service is installed".to_string()
                }
            });
        }
    };
    if let Some(home) = &launch.zdx_home
        && home != &inputs.zdx_home
    {
        return unavailable(format!(
            "the scheduler service uses ZDX_HOME {}, but this process uses {}",
            home.display(),
            inputs.zdx_home.display()
        ));
    }
    SchedulerContextState::Available {
        context: SchedulerContext {
            source: SchedulerContextSource::Installed,
            root: launch.root,
            config_dir: launch.working_directory,
            pid: inputs.running_pid,
            started_at: None,
            note: Some(note),
        },
    }
}

/// One job's resolution: its settings, or why it has none.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct JobResolution {
    pub settings: Option<JobRunSettings>,
    pub error: Option<String>,
}

impl From<Result<JobRunSettings>> for JobResolution {
    fn from(result: Result<JobRunSettings>) -> Self {
        match result {
            Ok(settings) => Self {
                settings: Some(settings),
                error: None,
            },
            Err(error) => Self {
                settings: None,
                error: Some(format!("{error:#}")),
            },
        }
    }
}

/// Resolves `jobs` the way the scheduler resolves them: inherited
/// values come from `scheduler`'s root and config directory. When the context
/// is unavailable or its config fails to load, a job without its own `root`
/// stays unresolved with that reason.
#[must_use]
pub fn resolve(scheduler: &SchedulerContextState, jobs: &[JobSpec]) -> Vec<JobResolution> {
    let config = match scheduler {
        SchedulerContextState::Available { context } => Some(
            Config::load_layered(&paths::config_layer_paths_for(&context.config_dir))
                .map_err(|err| format!("load scheduler config: {err:#}")),
        ),
        SchedulerContextState::Unavailable { .. } => None,
    };
    let missing = match (scheduler, &config) {
        (SchedulerContextState::Unavailable { reason }, _) => {
            Some(format!("scheduler settings unavailable: {reason}"))
        }
        (_, Some(Err(err))) => Some(err.clone()),
        _ => None,
    };
    jobs.iter()
        .map(|job| {
            let inherited = match (scheduler, &config, &missing) {
                (SchedulerContextState::Available { context }, Some(Ok(config)), None) => {
                    Ok(InheritedContext {
                        config,
                        root: &context.root,
                        root_origin: SettingOrigin::Scheduler,
                    })
                }
                (_, _, reason) => Err(reason
                    .as_deref()
                    .unwrap_or("scheduler settings unavailable")),
            };
            jobs::resolve_run_from(inherited, job)
                .map(|run| run.settings)
                .into()
        })
        .collect()
}

fn record_path() -> PathBuf {
    paths::zdx_home().join("run").join(RECORD_FILE)
}

fn read_record_at(path: &Path) -> Option<SchedulerRecord> {
    let text = fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).ok()
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::*;

    fn launch(root: &Path, zdx_home: &Path) -> InstalledLaunch {
        InstalledLaunch {
            root: root.to_path_buf(),
            working_directory: root.to_path_buf(),
            zdx_home: Some(zdx_home.to_path_buf()),
            args: Vec::new(),
        }
    }

    fn record(pid: u32, root: &Path) -> SchedulerRecord {
        SchedulerRecord {
            pid,
            root: root.to_path_buf(),
            config_dir: root.to_path_buf(),
            started_at: Utc::now(),
        }
    }

    fn context(state: &SchedulerContextState) -> &SchedulerContext {
        match state {
            SchedulerContextState::Available { context } => context,
            SchedulerContextState::Unavailable { reason } => panic!("unavailable: {reason}"),
        }
    }

    #[test]
    fn running_record_wins_over_installed_service() {
        let home = Path::new("/home/example/.zdx");
        let state = discover_from(DiscoveryInputs {
            running_pid: Some(42),
            record: Some(record(42, Path::new("/srv/live"))),
            installed: Some(Ok(launch(Path::new("/srv/installed"), home))),
            zdx_home: home.to_path_buf(),
        });
        let context = context(&state);
        assert_eq!(context.source, SchedulerContextSource::Running);
        assert_eq!(context.root, PathBuf::from("/srv/live"));
        assert_eq!(context.pid, Some(42));
        assert!(context.note.is_none());
    }

    #[test]
    fn running_without_record_reports_installed_with_note() {
        let home = Path::new("/home/example/.zdx");
        for stale in [None, Some(record(7, Path::new("/srv/old")))] {
            let state = discover_from(DiscoveryInputs {
                running_pid: Some(42),
                record: stale,
                installed: Some(Ok(launch(Path::new("/srv/installed"), home))),
                zdx_home: home.to_path_buf(),
            });
            let context = context(&state);
            assert_eq!(context.source, SchedulerContextSource::Installed);
            assert_eq!(context.root, PathBuf::from("/srv/installed"));
            assert_eq!(context.pid, Some(42));
            assert!(context.note.as_deref().unwrap().contains("next start"));
        }
    }

    #[test]
    fn stopped_scheduler_reports_installed_service() {
        let home = Path::new("/home/example/.zdx");
        let state = discover_from(DiscoveryInputs {
            running_pid: None,
            record: Some(record(42, Path::new("/srv/old"))),
            installed: Some(Ok(launch(Path::new("/srv/installed"), home))),
            zdx_home: home.to_path_buf(),
        });
        let context = context(&state);
        assert_eq!(context.source, SchedulerContextSource::Installed);
        assert_eq!(context.pid, None);
        assert!(context.note.as_deref().unwrap().contains("not running"));
    }

    #[test]
    fn unknown_context_is_unavailable_never_a_fallback() {
        let home = PathBuf::from("/home/example/.zdx");
        let cases = [
            (None, None, "not running and no scheduler service"),
            (Some(42), None, "no scheduler service is installed"),
            (None, Some(Err("bad plist".to_string())), "bad plist"),
            (
                None,
                Some(Ok(launch(Path::new("/srv"), Path::new("/other/.zdx")))),
                "ZDX_HOME /other/.zdx",
            ),
        ];
        for (running_pid, installed, expected) in cases {
            let state = discover_from(DiscoveryInputs {
                running_pid,
                record: None,
                installed,
                zdx_home: home.clone(),
            });
            match state {
                SchedulerContextState::Unavailable { reason } => {
                    assert!(reason.contains(expected), "{reason}");
                }
                SchedulerContextState::Available { .. } => panic!("expected unavailable"),
            }
        }
    }

    fn write_config(dir: &Path, body: &str) {
        fs::create_dir_all(dir.join(".zdx")).unwrap();
        fs::write(dir.join(".zdx").join("config.toml"), body).unwrap();
    }

    fn available(root: &Path, source: SchedulerContextSource) -> SchedulerContextState {
        SchedulerContextState::Available {
            context: SchedulerContext {
                source,
                root: root.to_path_buf(),
                config_dir: root.to_path_buf(),
                pid: None,
                started_at: None,
                note: None,
            },
        }
    }

    fn spec(job: &str, model: Option<&str>, root: Option<&Path>) -> JobSpec {
        JobSpec {
            job: job.to_string(),
            model: model.map(str::to_string),
            subagent: None,
            root: root.map(|root| root.display().to_string()),
        }
    }

    #[test]
    fn jobs_resolve_against_scheduler_root_and_config() {
        let _home = crate::test_support::temp_zdx_home();
        let scheduler_root = tempdir().unwrap();
        write_config(
            scheduler_root.path(),
            "model = \"claude-cli@work:claude-opus-5@high\"\n",
        );
        let own_root = tempdir().unwrap();
        write_config(own_root.path(), "model = \"openai-codex:gpt-5.4@low\"\n");

        let resolved = resolve(
            &available(scheduler_root.path(), SchedulerContextSource::Running),
            &[
                spec("inherited", None, None),
                spec("suffixless", Some("claude-cli:claude-sonnet-5"), None),
                spec("scoped", None, Some(own_root.path())),
            ],
        );
        let get = |i: usize| resolved[i].settings.clone().unwrap();

        let inherited = get(0);
        assert_eq!(inherited.root.path, scheduler_root.path());
        assert_eq!(inherited.root.origin, SettingOrigin::Scheduler);
        assert_eq!(inherited.model.spec, "claude-cli@work:claude-opus-5@high");
        assert_eq!(inherited.model.account.as_deref(), Some("work"));
        assert_eq!(inherited.model.thinking, "high");
        assert_eq!(inherited.model.origin, SettingOrigin::Config);

        let suffixless = get(1);
        assert_eq!(suffixless.model.spec, "claude-cli:claude-sonnet-5@high");
        assert_eq!(suffixless.model.account, None);
        assert_eq!(suffixless.model.origin, SettingOrigin::Job);
        assert_eq!(suffixless.model.thinking_origin, SettingOrigin::Config);

        let scoped = get(2);
        assert_eq!(scoped.root.path, own_root.path());
        assert_eq!(scoped.root.origin, SettingOrigin::Job);
        assert_eq!(scoped.model.spec, "openai-codex:gpt-5.4@low");
    }

    #[test]
    fn unavailable_or_broken_scheduler_leaves_only_inherited_jobs_unresolved() {
        let _home = crate::test_support::temp_zdx_home();
        let own_root = tempdir().unwrap();
        let broken_root = tempdir().unwrap();
        write_config(broken_root.path(), "model = [\n");
        let states = [
            (
                SchedulerContextState::Unavailable {
                    reason: "scheduler is not running".to_string(),
                },
                "scheduler settings unavailable: scheduler is not running",
            ),
            (
                available(broken_root.path(), SchedulerContextSource::Installed),
                "load scheduler config",
            ),
        ];
        for (state, expected) in states {
            let resolved = resolve(
                &state,
                &[
                    spec("inherited", None, None),
                    spec("scoped", None, Some(own_root.path())),
                ],
            );
            assert!(resolved[0].settings.is_none());
            assert!(resolved[0].error.as_deref().unwrap().contains(expected));
            assert_eq!(
                resolved[1].settings.as_ref().unwrap().root.path,
                own_root.path()
            );
        }
    }
}
