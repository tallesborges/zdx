//! Jobs: saved prompts that run unattended (docs/SPEC.md §8 Jobs).
//!
//! The Go jobs program (`apps/shared/jobs`, `zdx-go jobs`) owns job files,
//! schedules, state, the scheduler, and run bookkeeping. Rust owns what needs
//! config, providers, or threads: this module resolves the root and model a
//! job runs with ([`resolve_run_from`]), [`records`] reads the run log, and
//! [`history`] lists a job's recent runs (grouped attempts plus what each run
//! posted) for its next run and for `zdx jobs history`.

pub mod history;
pub mod records;

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::config::{Config, paths};
use crate::models::ModelSpec;
use crate::subagents::{self, SubagentDefinition};

/// A job's declared values that resolution reads, as the jobs program sends
/// them (`zdx jobs attempt`, `zdx jobs resolve`).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct JobSpec {
    /// Job name (file stem).
    pub job: String,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub subagent: Option<String>,
    /// As written (`~/...` or absolute); checked here, so a bad root fails
    /// only this job.
    #[serde(default)]
    pub root: Option<String>,
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
    /// settings (the `--root` of `zdx jobs attempt` or `zdx jobs resolve`).
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
pub fn resolve_run(base: &Config, caller_root: &Path, job: &JobSpec) -> Result<ResolvedJobRun> {
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
    job: &JobSpec,
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
    // Every explicit layer (the subagent's `model:`, its override, the job's
    // own `model:`) must resolve: a mode it names that is not configured fails
    // the run instead of falling through to whatever the layers below pick.
    // An absent layer inherits.
    let mut apply = |config: &mut Config,
                     spec: &str,
                     origin: SettingOrigin,
                     what: &dyn Fn() -> String|
     -> Result<()> {
        let mode = config.find_mode(spec).is_some();
        config
            .apply_model_selection(spec)
            .map_err(anyhow::Error::new)
            .with_context(what)?;
        model_origin = origin;
        // A mode sets the level too; a spec only with its `@<level>`.
        if mode || ModelSpec::parse(spec).thinking.is_some() {
            thinking_origin = origin;
        }
        Ok(())
    };

    let subagent = match job.subagent.as_deref() {
        Some(name) => {
            let definition = subagents::resolve_named(&root.path, name, None)
                .with_context(|| format!("load subagent '{name}'"))?;
            if let Some(model) = definition.model.as_deref() {
                apply(&mut config, model, SettingOrigin::Subagent, &|| {
                    format!("subagent '{name}' definition model")
                })?;
            }
            if let Some(model) = config
                .subagents
                .overrides
                .get(name)
                .and_then(|over| over.model.clone())
            {
                apply(
                    &mut config,
                    &model,
                    SettingOrigin::SubagentOverride,
                    &|| format!("subagent '{name}' override model"),
                )?;
            }
            Some(definition)
        }
        None => None,
    };
    if let Some(model) = job.model.as_deref() {
        apply(&mut config, model, SettingOrigin::Job, &|| {
            "job model".to_string()
        })?;
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
    job: &JobSpec,
) -> Result<JobRunSettings> {
    resolve_run(base, caller_root, job).map(|run| run.settings)
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

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::*;

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

    fn definition(model: Option<&str>) -> JobSpec {
        JobSpec {
            job: "demo".to_string(),
            model: model.map(str::to_string),
            subagent: None,
            root: None,
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

        // A mode resolves each run from the job's config, thinking included;
        // an unknown mode fails the run.
        let config = Config {
            model_modes: vec![crate::config::ModelMode {
                name: "fast".to_string(),
                description: String::new(),
                primary: "claude-cli:claude-haiku-5@low".to_string(),
                alternatives: Vec::new(),
                thinking: crate::config::ThinkingLevel::Low,
            }],
            ..config
        };
        let mode =
            resolve_run_settings(&config, caller.path(), &definition(Some("mode:fast"))).unwrap();
        assert_eq!(mode.model.spec, "claude-cli:claude-haiku-5@low");
        assert_eq!(mode.model.origin, SettingOrigin::Job);
        assert_eq!(mode.model.thinking_origin, SettingOrigin::Job);
        let unknown = resolve_run_settings(&config, caller.path(), &definition(Some("mode:nope")))
            .unwrap_err();
        assert!(
            crate::config::UnknownModeError::is_in(&unknown),
            "a job's own missing mode fails instead of running the default: {unknown:#}"
        );
    }

    /// A job's subagent override naming an undefined mode fails the run; a
    /// defined one resolves from the config the run is given, so an edit
    /// applies to the next run.
    #[test]
    fn job_subagent_mode_overrides_fail_or_resolve_per_run() {
        let _home = crate::test_support::temp_zdx_home();
        let caller = tempdir().unwrap();
        let mut config = Config {
            model: "claude-cli:claude-sonnet-5@high".to_string(),
            thinking_level: crate::config::ThinkingLevel::High,
            model_modes: vec![crate::config::ModelMode {
                name: "fast".to_string(),
                description: String::new(),
                primary: "claude-cli:claude-haiku-5@low".to_string(),
                alternatives: Vec::new(),
                thinking: crate::config::ThinkingLevel::Low,
            }],
            ..Default::default()
        };
        let set_override = |config: &mut Config, model: &str| {
            config.subagents.overrides.insert(
                "explorer".to_string(),
                crate::config::SubagentOverride {
                    model: Some(model.to_string()),
                    thinking_level: None,
                },
            );
        };
        let job = JobSpec {
            subagent: Some("explorer".to_string()),
            ..definition(None)
        };

        set_override(&mut config, "mode:fast");
        let run = resolve_run_settings(&config, caller.path(), &job).unwrap();
        assert_eq!(run.model.spec, "claude-cli:claude-haiku-5@low");
        assert_eq!(run.model.origin, SettingOrigin::SubagentOverride);

        config.model_modes[0].primary = "claude-cli:claude-sonnet-5@medium".to_string();
        config.model_modes[0].thinking = crate::config::ThinkingLevel::Medium;
        let run = resolve_run_settings(&config, caller.path(), &job).unwrap();
        assert_eq!(run.model.spec, "claude-cli:claude-sonnet-5@medium");

        set_override(&mut config, "mode:gone");
        let err = resolve_run_settings(&config, caller.path(), &job).unwrap_err();
        assert!(crate::config::UnknownModeError::is_in(&err), "{err:#}");
        assert!(
            format!("{err:#}").contains("subagent 'explorer' override model"),
            "{err:#}"
        );
    }

    #[test]
    fn a_bad_declared_root_fails_only_that_job() {
        let caller = tempdir().unwrap();
        for (root, message) in [
            ("relative/root", "must be an absolute path"),
            ("  ", "root cannot be empty"),
        ] {
            let job = JobSpec {
                root: Some(root.trim().to_string()),
                ..definition(None)
            };
            let err = resolve_run_settings(&Config::default(), caller.path(), &job).unwrap_err();
            assert!(format!("{err:#}").contains(message), "{err:#}");
        }
        let settings =
            resolve_run_settings(&Config::default(), caller.path(), &definition(None)).unwrap();
        let json = serde_json::to_value(settings).unwrap();
        assert_eq!(json["root"]["origin"], "caller");
        assert_eq!(json["model"]["origin"], "config");
    }
}
