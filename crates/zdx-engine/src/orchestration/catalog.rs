//! Read-only catalogs a chat surface shows (model picker, launcher, resumable
//! threads, a thread's status card, custom commands) and the one config
//! write behind the General `/model` picker. Every value is derived here,
//! from the chat's root config and the thread metadata; surfaces only lay it
//! out.

use std::path::{Path, PathBuf};

use anyhow::Result;
use serde::{Deserialize, Serialize};

use super::protocol::ErrorCode;
use super::thread_ops::{coded, validate_thread_id};
use crate::config::{Config, ModelMode, ThinkingLevel};
use crate::core::thread_persistence;
use crate::core::workers::WorkerManager;
use crate::subagents::{ORCHESTRATOR_SUBAGENT_NAME, apply_orchestrator_override};

/// How many threads the launcher's Continue offers.
pub const RESUMABLE_CAP: usize = 8;

/// The chat's config, or why it does not load (turns are refused then).
///
/// # Errors
/// The load error, worded for the chat.
pub fn chat_config(root: &Path) -> std::result::Result<Config, String> {
    Config::load_layered(&crate::config::paths::config_layer_paths_for(root)).map_err(|err| {
        format!(
            "This chat's workspace config does not load, so turns here are refused instead of running another config's model: {err:#}"
        )
    })
}

/// The config a topic's model selection resolves against: the chat's config
/// with the modes of the root the thread runs in.
///
/// # Errors
/// The stored root's config does not load.
pub fn topic_config(mut config: Config, chat_root: &Path, thread_id: &str) -> Result<Config> {
    if let Some(stored) = thread_persistence::read_thread_root_path(thread_id)? {
        config.use_modes_of(chat_root, Path::new(&stored))?;
    }
    Ok(config)
}

/// Applies a topic's stored selection and returns its label. A selection
/// naming a mode this config lacks leaves the default and says so: a surface
/// never shows the default as what runs there.
pub fn apply_topic_selection(
    config: &mut Config,
    model: Option<&str>,
    thinking: Option<ThinkingLevel>,
) -> String {
    match config.apply_thread_model_override(model, thinking) {
        Ok(()) => config.model_label(),
        Err(err) => format!(
            "{} (mode not configured here; turns are refused)",
            err.selection
        ),
    }
}

/// A topic's effective config and selection label, or the reason (prefixed
/// `⚠️ `) it does not resolve; nothing may be written from a fallback then.
///
/// # Errors
/// The reason, when the topic's config or its profile model does not resolve.
pub fn resolved_topic(
    chat: Config,
    chat_root: &Path,
    thread_id: &str,
) -> std::result::Result<(Config, String), String> {
    let mut config =
        topic_config(chat, chat_root, thread_id).map_err(|err| format!("⚠️ {err:#}"))?;
    if is_orchestrator(thread_id)
        && let Err(err) = apply_orchestrator_override(&mut config)
    {
        return Err(format!("⚠️ {err:#}; turns are refused"));
    }
    let model = thread_persistence::read_thread_model_override(thread_id)
        .ok()
        .flatten();
    let thinking = thread_persistence::read_thread_thinking_override(thread_id)
        .ok()
        .flatten();
    let label = apply_topic_selection(&mut config, model.as_deref(), thinking);
    Ok((config, label))
}

/// Modes whose primary model is available here.
#[must_use]
pub fn available_modes(config: &Config) -> Vec<ModelMode> {
    filter_available_modes(&config.model_modes, &config.subagent_available_models())
}

/// Keeps the modes whose primary resolves to an available model id; logs
/// each skipped one, so a misconfigured tier is visible.
#[must_use]
pub fn filter_available_modes(modes: &[ModelMode], available: &[String]) -> Vec<ModelMode> {
    modes
        .iter()
        .filter(|mode| {
            let ok = mode.model_available(available);
            if !ok {
                tracing::warn!(mode = %mode.name, primary = %mode.primary, "Skipping mode with unavailable primary model");
            }
            ok
        })
        .cloned()
        .collect()
}

/// The default config of new topics of a role.
///
/// # Errors
/// The orchestrator override names an undefined mode.
pub fn launch_config(
    mut config: Config,
    orchestrator: bool,
) -> std::result::Result<Config, String> {
    if orchestrator {
        apply_orchestrator_override(&mut config).map_err(|err| format!("{err:#}"))?;
    }
    Ok(config)
}

fn is_orchestrator(thread_id: &str) -> bool {
    thread_persistence::read_persistent_profile(thread_id)
        .ok()
        .flatten()
        .as_deref()
        == Some(ORCHESTRATOR_SUBAGENT_NAME)
}

/// What a model picker targets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PickerScope {
    /// The chat's default model (`/model` in General).
    General,
    /// A topic's own model.
    Topic,
    /// A new agent topic (launcher Custom).
    NewThread,
    /// A new orchestrator topic.
    NewOrchestrator,
    /// The topic a staged `/handoff` or `/btw` will create.
    Staging,
}

/// `{kind:"picker"}`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PickerQuery {
    pub root: PathBuf,
    pub scope: PickerScope,
    /// Topic: the topic's effective thread; staging: the source thread.
    #[serde(default)]
    pub thread_id: Option<String>,
    /// Staging: the profile the new topic starts as.
    #[serde(default)]
    pub profile: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PickerModel {
    /// Qualified id (`provider:id`, `provider@account:id`).
    pub id: String,
    /// The id without its thinking suffix.
    pub name: String,
    /// The spec to pick per thinking level display name.
    pub specs: std::collections::BTreeMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PickerMode {
    pub name: String,
    /// The selection a pick keeps (`mode:<name>`).
    #[serde(rename = "ref")]
    pub reference: String,
}

/// Everything a model picker shows.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Picker {
    /// The chat's config does not load; picks are refused with this text.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub config_error: Option<String>,
    /// Launcher scopes: the role's default does not resolve; picks are refused.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub launch_error: Option<String>,
    pub models: Vec<PickerModel>,
    /// Display name per provider key.
    pub provider_labels: std::collections::BTreeMap<String, String>,
    pub modes: Vec<PickerMode>,
    /// The header's model: the chat's, the topic's selection label, the role
    /// default, a staged topic's start, or the `⚠️ …` reason it does not
    /// resolve.
    pub label: String,
    /// The chat's model label.
    pub default_label: String,
    /// Topic: it has its own model or thinking level.
    pub overridden: bool,
    /// Topic: the stored model override.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub r#override: Option<String>,
    /// The current model spec.
    pub model: String,
    /// The current thinking level display name.
    pub thinking: String,
}

/// The model picker of one scope.
///
/// # Errors
/// `invalid` for a bad thread id or a missing one where the scope needs it.
#[allow(clippy::too_many_lines)] // one projection per scope
pub fn picker(query: &PickerQuery) -> Result<Picker> {
    let chat = match chat_config(&query.root) {
        Ok(config) => config,
        Err(error) => {
            return Ok(Picker {
                label: format!("⚠️ {error}"),
                config_error: Some(error),
                ..Picker::default()
            });
        }
    };
    let thread_id = || -> Result<&str> {
        let id = query
            .thread_id
            .as_deref()
            .ok_or_else(|| coded(ErrorCode::Invalid, "this picker needs `thread_id`"))?;
        validate_thread_id(id)?;
        Ok(id)
    };
    let models: Vec<PickerModel> = chat
        .subagent_available_models()
        .into_iter()
        .map(|id| PickerModel {
            name: crate::models::ModelSpec::parse(&id).without_thinking(),
            specs: ThinkingLevel::all()
                .iter()
                .map(|level| {
                    (
                        level.display_name().to_string(),
                        crate::models::format_model_thinking(&id, *level),
                    )
                })
                .collect(),
            id,
        })
        .collect();
    let provider_labels = models
        .iter()
        .map(|model| model.id.split(':').next().unwrap_or_default().to_string())
        .map(|key| {
            let label = crate::providers::provider_key_label(&key);
            (key, label)
        })
        .collect();
    let default_label = chat.model_label();
    let mode_refs = |modes: Vec<ModelMode>| {
        modes
            .into_iter()
            .map(|mode| PickerMode {
                reference: mode.as_ref_spec(),
                name: mode.name,
            })
            .collect::<Vec<_>>()
    };
    let mut picker = Picker {
        models,
        provider_labels,
        default_label: default_label.clone(),
        ..Picker::default()
    };
    let current = match query.scope {
        PickerScope::General => {
            picker.modes = mode_refs(available_modes(&chat));
            picker.label = default_label;
            chat
        }
        PickerScope::NewThread => {
            picker.label = default_label;
            chat
        }
        PickerScope::NewOrchestrator => match launch_config(chat.clone(), true) {
            Ok(config) => {
                picker.label = config.model_label();
                config
            }
            Err(err) => {
                picker.label = format!("⚠️ {err}");
                picker.launch_error = Some(err);
                chat
            }
        },
        PickerScope::Staging => {
            let source = thread_id()?;
            if let Some(profile) = query.profile.as_deref()
                && profile != ORCHESTRATOR_SUBAGENT_NAME
            {
                return Err(coded(
                    ErrorCode::Invalid,
                    format!("a seeded topic starts as an agent or `orchestrator`, not `{profile}`"),
                ));
            }
            picker.modes = mode_refs(available_modes(&chat));
            let start = crate::core::topic_start::TopicStart::for_source(
                chat,
                source,
                query.profile.as_deref(),
            );
            picker.label = start.label();
            start.config
        }
        PickerScope::Topic => {
            let id = thread_id()?;
            let stored = thread_persistence::read_thread_model_override(id)
                .ok()
                .flatten();
            let thinking = thread_persistence::read_thread_thinking_override(id)
                .ok()
                .flatten();
            picker.overridden = stored.is_some() || thinking.is_some();
            picker.r#override = stored;
            picker.modes = topic_config(chat.clone(), &query.root, id)
                .map(|config| mode_refs(available_modes(&config)))
                .unwrap_or_default();
            match resolved_topic(chat.clone(), &query.root, id) {
                Ok((config, label)) => {
                    picker.label = label;
                    config
                }
                Err(reason) => {
                    picker.label = reason;
                    chat
                }
            }
        }
    };
    picker.model.clone_from(&current.model);
    picker.thinking = current.thinking_level.display_name().to_string();
    Ok(picker)
}

/// Stores `model` as the chat's default in the workspace config of `root`.
/// Returns the stored selection.
///
/// # Errors
/// `invalid` when the chat's config does not load or the model names an
/// undefined mode; else the write error.
pub fn set_chat_model(root: &Path, model: &str) -> Result<String> {
    let mut config = chat_config(root).map_err(|err| coded(ErrorCode::Invalid, err))?;
    config
        .apply_model_selection(model)
        .map_err(|err| coded(ErrorCode::Invalid, err.to_string()))?;
    let selection = config.model_selection();
    Config::save_model_for_cwd(root, &selection)?;
    Ok(selection)
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LaunchMode {
    pub name: String,
    #[serde(rename = "ref")]
    pub reference: String,
    pub primary: String,
    pub thinking: String,
    pub description: String,
}

/// What the launcher of one role shows.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LauncherView {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub config_error: Option<String>,
    /// The role's default does not resolve; new topics of the role are
    /// refused.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub launch_error: Option<String>,
    pub modes: Vec<LaunchMode>,
    /// The default's `mode:<name>`, when it is a mode.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_mode: Option<String>,
    pub default_model: String,
    pub default_thinking: String,
}

/// The launcher of `role` (`agent` or `orchestrator`).
#[must_use]
pub fn launcher(root: &Path, orchestrator: bool) -> LauncherView {
    let chat = match chat_config(root) {
        Ok(config) => config,
        Err(error) => {
            return LauncherView {
                config_error: Some(error),
                ..LauncherView::default()
            };
        }
    };
    let modes = available_modes(&chat)
        .into_iter()
        .map(|mode| LaunchMode {
            reference: mode.as_ref_spec(),
            thinking: mode.thinking.display_name().to_string(),
            name: mode.name,
            primary: mode.primary,
            description: mode.description,
        })
        .collect();
    let (default, launch_error) = match launch_config(chat.clone(), orchestrator) {
        Ok(config) => (config, None),
        Err(err) => (chat, Some(err)),
    };
    LauncherView {
        config_error: None,
        launch_error,
        modes,
        default_mode: default.mode_selection(),
        default_model: crate::models::ModelSpec::parse(&default.model).without_thinking(),
        default_thinking: default.thinking_level.display_name().to_string(),
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ResumeThread {
    pub id: String,
    pub title: String,
    /// RFC 3339.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub modified: Option<String>,
}

/// Newest-first threads the launcher's Continue offers for the chat's
/// project: top-level, not `telegram-*`, not a managed worker, root equal to
/// the chat's root, at most [`RESUMABLE_CAP`].
///
/// # Errors
/// The thread listing fails.
pub fn resumable(root: &Path, managed_workers: &[String]) -> Result<Vec<ResumeThread>> {
    Ok(
        resumable_threads(thread_persistence::list_threads()?, root, managed_workers)
            .into_iter()
            .map(|thread| ResumeThread {
                title: thread.display_title(),
                modified: thread
                    .modified
                    .map(|at| chrono::DateTime::<chrono::Utc>::from(at).to_rfc3339()),
                id: thread.id,
            })
            .collect(),
    )
}

/// The [`resumable`] rule over a newest-first listing.
#[must_use]
pub fn resumable_threads(
    all: Vec<thread_persistence::ThreadSummary>,
    root: &Path,
    managed_workers: &[String],
) -> Vec<thread_persistence::ThreadSummary> {
    let root = root.display().to_string();
    all.into_iter()
        .filter(|thread| !thread.id.starts_with("telegram-"))
        .filter(|thread| !managed_workers.contains(&thread.id))
        .filter(|thread| thread.root_path.as_deref() == Some(root.as_str()))
        .take(RESUMABLE_CAP)
        .collect()
}

/// Checks that `source` can be resumed in a new topic (exists, is no
/// `telegram-*` thread, is no alias) and returns its title.
///
/// # Errors
/// `invalid` or `not_found` with the reason.
pub fn resume_source(source: &str) -> Result<Option<String>> {
    validate_thread_id(source)?;
    if source.starts_with("telegram-") {
        return Err(coded(
            ErrorCode::Invalid,
            format!("cannot resume a telegram topic thread: {source}"),
        ));
    }
    if !thread_persistence::thread_exists(source) {
        return Err(coded(
            ErrorCode::NotFound,
            format!("source thread does not exist: {source}"),
        ));
    }
    if thread_persistence::read_thread_alias(source)?.is_some() {
        return Err(coded(
            ErrorCode::Invalid,
            format!("source thread is itself an alias: {source}"),
        ));
    }
    Ok(thread_persistence::read_thread_title(source).ok().flatten())
}

/// Where a status value comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SelectionSource {
    Default,
    Orchestrator,
    Topic,
}

impl SelectionSource {
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Default => "workspace/global default",
            Self::Orchestrator => "orchestrator override",
            Self::Topic => "topic override",
        }
    }
}

/// Which layer set a topic's model and thinking level.
#[must_use]
pub fn selection_sources(
    orchestrator_model: Option<&str>,
    topic_model: Option<&str>,
    legacy_topic_thinking: Option<ThinkingLevel>,
) -> (SelectionSource, SelectionSource) {
    let model_source = if topic_model.is_some() {
        SelectionSource::Topic
    } else if orchestrator_model.is_some() {
        SelectionSource::Orchestrator
    } else {
        SelectionSource::Default
    };
    // A mode reference carries its mode's thinking level.
    let sets_thinking = |model: &str| {
        crate::config::mode_ref_name(model).is_some()
            || crate::models::ModelSpec::parse(model).thinking.is_some()
    };
    let thinking_source =
        if legacy_topic_thinking.is_some() || topic_model.is_some_and(sets_thinking) {
            SelectionSource::Topic
        } else if orchestrator_model.is_some_and(sets_thinking) {
            SelectionSource::Orchestrator
        } else {
            SelectionSource::Default
        };
    (model_source, thinking_source)
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct StatusUsage {
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write: u64,
}

impl From<thread_persistence::Usage> for StatusUsage {
    fn from(usage: thread_persistence::Usage) -> Self {
        Self {
            input: usage.input,
            output: usage.output,
            cache_read: usage.cache_read,
            cache_write: usage.cache_write,
        }
    }
}

/// The cost line of a status card.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum StatusPricing {
    /// Model not in the registry.
    Unknown,
    /// OAuth provider.
    Subscription,
    Priced {
        cost: f64,
        savings: f64,
        /// Per 1M tokens.
        input: f64,
        output: f64,
        cache_read: f64,
        cache_write: f64,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StatusWorker {
    pub thread_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mirror_url: Option<String>,
}

/// A thread's status card, derived.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThreadStatus {
    /// The card shows only this (the chat's config, or the thread root's
    /// modes, do not load).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub config_error: Option<String>,
    pub orchestrator: bool,
    /// Why the stored selection does not resolve here (turns are refused).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub refused: Option<String>,
    pub model: String,
    pub model_source: SelectionSource,
    /// `mode:<name>` when the selection is a mode.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
    pub thinking: String,
    pub thinking_source: SelectionSource,
    pub thread_id: String,
    pub root: PathBuf,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    /// The model's context window; 0 when unknown.
    pub context_limit: u64,
    pub context_tokens: u64,
    pub usage: StatusUsage,
    pub pricing: StatusPricing,
    /// Orchestrator threads: their workers, most recently touched first.
    pub workers: Vec<StatusWorker>,
    /// The orchestrator thread that owns this one, when any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_orchestrator: Option<String>,
}

/// `{kind:"thread_status"}`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StatusQuery {
    pub root: PathBuf,
    pub thread_id: String,
}

fn git_branch(root: &Path) -> Option<String> {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["branch", "--show-current"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let branch = String::from_utf8_lossy(&output.stdout).trim().to_string();
    (!branch.is_empty()).then_some(branch)
}

/// The status of `thread_id` in the chat whose root is `chat_root`.
/// `workers` lists an orchestrator's workers (the service's manager).
/// Blocking: reads the thread and runs `git`.
///
/// # Errors
/// `invalid` for a bad id; else the thread metadata read error.
#[allow(clippy::too_many_lines)] // the bot's status card, value by value
pub fn thread_status(
    chat_root: &Path,
    thread_id: &str,
    workers: Option<&WorkerManager>,
) -> Result<ThreadStatus> {
    validate_thread_id(thread_id)?;
    let root = thread_persistence::read_thread_root_path(thread_id)?
        .map_or_else(|| chat_root.to_path_buf(), PathBuf::from);
    let orchestrator = is_orchestrator(thread_id);
    let blank = |error: String| ThreadStatus {
        config_error: Some(error),
        orchestrator,
        refused: None,
        model: String::new(),
        model_source: SelectionSource::Default,
        mode: None,
        thinking: String::new(),
        thinking_source: SelectionSource::Default,
        thread_id: thread_id.to_string(),
        root: root.clone(),
        branch: None,
        context_limit: 0,
        context_tokens: 0,
        usage: StatusUsage::default(),
        pricing: StatusPricing::Unknown,
        workers: Vec::new(),
        parent_orchestrator: None,
    };
    let mut config = match chat_config(chat_root) {
        Ok(config) => config,
        Err(error) => return Ok(blank(error)),
    };
    if let Err(err) = config.use_modes_of(chat_root, &root) {
        return Ok(blank(format!("{err:#}")));
    }
    let orchestrator_model = orchestrator
        .then(|| {
            config
                .subagents
                .overrides
                .get(ORCHESTRATOR_SUBAGENT_NAME)
                .and_then(|over| over.model.clone())
        })
        .flatten();
    let profile_error = orchestrator
        .then(|| apply_orchestrator_override(&mut config).err())
        .flatten()
        .map(|err| format!("{err:#}"));
    let model_override = thread_persistence::read_thread_model_override(thread_id)?;
    let thinking_override = thread_persistence::read_thread_thinking_override(thread_id)?;
    let (model_source, thinking_source) = selection_sources(
        orchestrator_model.as_deref(),
        model_override.as_deref(),
        thinking_override,
    );
    let unresolved = config
        .apply_thread_model_override(model_override.as_deref(), thinking_override)
        .err();
    let events = thread_persistence::load_thread_events(thread_id).unwrap_or_default();
    let (cumulative, latest) = thread_persistence::extract_usage_from_thread_events(&events);
    let meta = crate::models::ModelOption::find_by_id(&config.model);
    let pricing = match meta {
        None => StatusPricing::Unknown,
        Some(_)
            if crate::providers::provider_for_model(&config.model).auth_mode()
                == crate::providers::ProviderAuthMode::OAuth =>
        {
            StatusPricing::Subscription
        }
        Some(model) => StatusPricing::Priced {
            cost: model.pricing.cost(
                cumulative.input,
                cumulative.output,
                cumulative.cache_read,
                cumulative.cache_write,
            ),
            savings: model.pricing.cache_savings(cumulative.cache_read),
            input: model.pricing.input,
            output: model.pricing.output,
            cache_read: model.pricing.cache_read,
            cache_write: model.pricing.cache_write,
        },
    };
    let workers = match (orchestrator, workers) {
        (true, Some(manager)) => manager
            .list_for_owner(thread_id)
            .into_iter()
            .map(|worker| StatusWorker {
                title: thread_persistence::read_thread_title(&worker.thread_id)
                    .ok()
                    .flatten(),
                status: worker.status.as_str().to_string(),
                mirror_url: worker.mirror_url.clone(),
                thread_id: worker.thread_id,
            })
            .collect(),
        _ => Vec::new(),
    };
    let parent_orchestrator = thread_persistence::read_thread_summary_cached(thread_id)
        .ok()
        .flatten()
        .and_then(|summary| summary.parent_thread_id)
        .filter(|parent| is_orchestrator(parent));
    Ok(ThreadStatus {
        config_error: None,
        orchestrator,
        refused: profile_error.or_else(|| unresolved.map(|err| err.to_string())),
        mode: config.mode_selection(),
        thinking: config.thinking_level.display_name().to_string(),
        model_source,
        thinking_source,
        thread_id: thread_id.to_string(),
        branch: git_branch(&root),
        root,
        context_limit: meta.map_or(0, |model| model.context_limit),
        context_tokens: latest.context_input() + latest.output,
        usage: cumulative.into(),
        pricing,
        workers,
        parent_orchestrator,
        model: config.model,
    })
}

/// `{kind:"custom_commands"}`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommandsQuery {
    pub root: PathBuf,
    #[serde(default)]
    pub thread_id: Option<String>,
    /// Names the surface handles itself; never shadowed.
    #[serde(default)]
    pub native: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CatalogCommand {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// `project`, `user`, or `builtin`.
    pub source: String,
    pub content: String,
}

/// Custom commands for a thread (rooted at its stored root, else the
/// chat's), project first, then user, then built-in.
///
/// # Errors
/// `invalid` for a bad thread id.
pub fn custom_commands(query: &CommandsQuery) -> Result<Vec<CatalogCommand>> {
    if let Some(thread_id) = query.thread_id.as_deref() {
        validate_thread_id(thread_id)?;
    }
    let root = query
        .thread_id
        .as_deref()
        .map(thread_persistence::read_thread_root_path)
        .transpose()?
        .flatten()
        .map_or_else(|| query.root.clone(), PathBuf::from);
    let native: Vec<&str> = query.native.iter().map(String::as_str).collect();
    let loaded = crate::custom_commands::load_custom_commands(&root, &native);
    for warning in &loaded.warnings {
        tracing::warn!(path = %warning.path.display(), message = %warning.message, "Custom command warning");
    }
    let mut commands = loaded.commands;
    commands.sort_by_key(|command| match command.source {
        crate::custom_commands::CustomCommandSource::Project => 0,
        crate::custom_commands::CustomCommandSource::User => 1,
        crate::custom_commands::CustomCommandSource::BuiltIn => 2,
    });
    Ok(commands
        .into_iter()
        .map(|command| CatalogCommand {
            source: command.source.as_str().to_string(),
            name: command.name,
            description: command.description,
            content: command.content,
        })
        .collect())
}

/// `{op:"catalog"}`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CatalogQuery {
    Picker(PickerQuery),
    Launcher {
        root: PathBuf,
        /// `agent` or `orchestrator`.
        role: String,
    },
    Resumable {
        root: PathBuf,
    },
    ResumeSource {
        thread_id: String,
    },
    ThreadStatus(StatusQuery),
    CustomCommands(CommandsQuery),
}

/// Answers one catalog query. `workers` is the service's manager (managed
/// workers are never resumable; an orchestrator card lists its workers).
/// Blocking.
///
/// # Errors
/// `invalid`/`not_found` with the reason; else read errors.
pub fn catalog(query: CatalogQuery, workers: Option<&WorkerManager>) -> Result<serde_json::Value> {
    let value = match query {
        CatalogQuery::Picker(query) => serde_json::to_value(picker(&query)?)?,
        CatalogQuery::Launcher { root, role } => {
            let orchestrator = match role.as_str() {
                "agent" => false,
                "orchestrator" => true,
                other => {
                    return Err(coded(
                        ErrorCode::Invalid,
                        format!("role must be `agent` or `orchestrator`, not `{other}`"),
                    ));
                }
            };
            serde_json::to_value(launcher(&root, orchestrator))?
        }
        CatalogQuery::Resumable { root } => {
            let managed: Vec<String> = workers
                .map(|manager| {
                    manager
                        .list_all()
                        .into_iter()
                        .map(|worker| worker.thread_id)
                        .collect()
                })
                .unwrap_or_default();
            serde_json::json!({ "threads": resumable(&root, &managed)? })
        }
        CatalogQuery::ResumeSource { thread_id } => {
            serde_json::json!({ "title": resume_source(&thread_id)? })
        }
        CatalogQuery::ThreadStatus(query) => {
            serde_json::to_value(thread_status(&query.root, &query.thread_id, workers)?)?
        }
        CatalogQuery::CustomCommands(query) => {
            serde_json::json!({ "commands": custom_commands(&query)? })
        }
    };
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selection_sources_name_the_layer() {
        assert_eq!(
            selection_sources(None, Some("openai:m@high"), None),
            (SelectionSource::Topic, SelectionSource::Topic)
        );
        assert_eq!(
            selection_sources(Some("mode:fast"), None, None),
            (SelectionSource::Orchestrator, SelectionSource::Orchestrator)
        );
        assert_eq!(
            selection_sources(None, Some("openai:m"), Some(ThinkingLevel::Low)),
            (SelectionSource::Topic, SelectionSource::Topic)
        );
        assert_eq!(
            selection_sources(None, None, None),
            (SelectionSource::Default, SelectionSource::Default)
        );
    }

    #[test]
    fn resumable_threads_filters_by_root_workers_and_telegram() {
        use thread_persistence::ThreadSummary;

        let summary = |id: &str, root_path: Option<&str>| ThreadSummary {
            id: id.to_string(),
            root_path: root_path.map(str::to_string),
            ..Default::default()
        };
        let mut all = vec![
            summary("uuid-keep-1", Some("/proj/a")),
            summary("telegram-1-topic-2", Some("/proj/a")),
            summary("uuid-other", Some("/proj/b")),
            summary("uuid-noroot", None),
            summary("uuid-worker", Some("/proj/a")),
            summary("uuid-keep-2", Some("/proj/a")),
        ];
        all.extend((0..10).map(|n| summary(&format!("uuid-more-{n}"), Some("/proj/a"))));
        let kept = resumable_threads(all, Path::new("/proj/a"), &["uuid-worker".to_string()]);
        let ids: Vec<&str> = kept.iter().map(|t| t.id.as_str()).collect();
        assert_eq!(ids[..2], ["uuid-keep-1", "uuid-keep-2"]);
        assert_eq!(ids.len(), RESUMABLE_CAP);
    }

    #[test]
    fn resume_source_refuses_topics_aliases_and_missing_threads() {
        let _home = crate::test_support::temp_zdx_home();
        let code = |id: &str| super::super::thread_ops::error_code(&resume_source(id).unwrap_err());
        assert_eq!(code("telegram-1-topic-2"), Some(ErrorCode::Invalid));
        assert_eq!(code("missing"), Some(ErrorCode::NotFound));
        let mut alias = thread_persistence::Thread::with_id("alias".to_string()).unwrap();
        alias.set_alias(Some("x".to_string())).unwrap();
        assert_eq!(code("alias"), Some(ErrorCode::Invalid));
        let mut plain = thread_persistence::Thread::with_id("plain".to_string()).unwrap();
        plain.set_title(Some("Release".to_string())).unwrap();
        assert_eq!(resume_source("plain").unwrap().as_deref(), Some("Release"));
    }

    #[test]
    fn picker_and_status_project_a_topic_override() {
        let home = crate::test_support::temp_zdx_home();
        let root = home.path().join("project");
        std::fs::create_dir_all(&root).unwrap();
        let mut thread = thread_persistence::Thread::with_id("topic".to_string()).unwrap();
        thread
            .set_model_override(Some("anthropic:claude-sonnet-5@high".to_string()))
            .unwrap();
        let picker = picker(&PickerQuery {
            root: root.clone(),
            scope: PickerScope::Topic,
            thread_id: Some("topic".to_string()),
            profile: None,
        })
        .unwrap();
        assert!(picker.overridden);
        assert_eq!(
            picker.r#override.as_deref(),
            Some("anthropic:claude-sonnet-5@high")
        );
        assert_eq!(picker.thinking, "high");
        assert!(
            picker
                .models
                .iter()
                .all(|model| model.specs.len() == ThinkingLevel::all().len())
        );

        let status = thread_status(&root, "topic", None).unwrap();
        assert_eq!(status.model_source, SelectionSource::Topic);
        assert_eq!(status.thinking, "high");
        assert!(status.refused.is_none());
        assert!(!status.orchestrator);

        thread
            .set_model_override(Some("mode:gone".to_string()))
            .unwrap();
        let status = thread_status(&root, "topic", None).unwrap();
        assert!(
            status
                .refused
                .as_deref()
                .is_some_and(|r| r.contains("mode:gone")),
            "{status:?}"
        );
    }
}
