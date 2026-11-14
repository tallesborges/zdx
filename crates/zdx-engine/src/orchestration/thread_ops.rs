//! Thread operations a chat surface needs besides running turns: thread
//! metadata, delivery records, and resetting a thread. One implementation,
//! called by the orchestrator socket ([`super::protocol::Request`]) and by
//! their `zdx threads` CLI twins, so no surface writes thread files itself.

// `Option<Option<T>>` is the wire contract: absent keeps, `null` clears.
#![allow(clippy::option_option)]

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Deserializer, Serialize};

use super::protocol::ErrorCode;
use crate::config::{Config, ThinkingLevel, paths};
use crate::core::delivery::DeliveryRecord;
use crate::core::thread_persistence::{self, Thread, ThreadEvent};

/// An error with the [`ErrorCode`] a client maps it to. Other errors reply
/// without a code.
#[derive(Debug)]
pub struct CodedError {
    pub code: ErrorCode,
    pub message: String,
}

impl std::fmt::Display for CodedError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for CodedError {}

/// Builds an error carrying `code`.
pub fn coded(code: ErrorCode, message: impl Into<String>) -> anyhow::Error {
    anyhow::Error::new(CodedError {
        code,
        message: message.into(),
    })
}

/// The code of an error built with [`coded`], anywhere in its chain.
#[must_use]
pub fn error_code(err: &anyhow::Error) -> Option<ErrorCode> {
    err.chain()
        .find_map(|cause| cause.downcast_ref::<CodedError>())
        .map(|coded| coded.code)
}

/// Distinguishes a JSON `null` (clear) from an absent field (keep).
fn present<'de, D, T>(deserializer: D) -> std::result::Result<Option<Option<T>>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer).map(Some)
}

/// Metadata changes for one thread. An absent field is kept; for the
/// clearable ones, JSON `null` clears it.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct UpdateThread {
    /// Required on the socket; the CLI takes it from its argument.
    #[serde(default)]
    pub thread_id: String,
    /// Root config layers load from to check a `model` mode (the chat's
    /// root), and a `persistent_profile` on a thread with no root. Defaults
    /// to the thread's stored root. Never stored.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config_root: Option<PathBuf>,
    /// Root the thread runs in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root: Option<PathBuf>,
    /// Model spec or `mode:<name>`, checked against the config and stored as
    /// its selection.
    #[serde(
        default,
        deserialize_with = "present",
        skip_serializing_if = "Option::is_none"
    )]
    pub model: Option<Option<String>>,
    #[serde(
        default,
        deserialize_with = "present",
        skip_serializing_if = "Option::is_none"
    )]
    pub thinking: Option<Option<ThinkingLevel>>,
    #[serde(
        default,
        deserialize_with = "present",
        skip_serializing_if = "Option::is_none"
    )]
    pub alias_to: Option<Option<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_topic_title: Option<bool>,
    #[serde(
        default,
        deserialize_with = "present",
        skip_serializing_if = "Option::is_none"
    )]
    pub title: Option<Option<String>>,
    /// Lineage of a thread being created (a `/handoff` or `/btw` seed). Only
    /// a thread not written yet takes it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub handoff_from: Option<String>,
    /// Makes the thread a persistent-profile thread. Only a thread with no
    /// messages takes it; the profile must load from the thread's root.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub persistent_profile: Option<String>,
}

/// What [`update_thread`] changed that a surface shows.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThreadUpdated {
    pub thread_id: String,
    /// Display label of the model now set, when `model` set one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_label: Option<String>,
    /// The stored title, when `title` changed it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
}

/// Thread ids become file names, so only a plain name is accepted.
///
/// # Errors
/// Returns an `invalid` error for an unusable id.
pub fn validate_thread_id(id: &str) -> Result<()> {
    let valid = !id.is_empty()
        && id.len() <= 200
        && !id.starts_with('.')
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'));
    if !valid {
        return Err(coded(
            ErrorCode::Invalid,
            format!("invalid thread id '{id}': use letters, digits, '-', '_', or '.'"),
        ));
    }
    Ok(())
}

fn load_config(root: &Path) -> Result<Config> {
    Config::load_layered(&paths::config_layer_paths_for(root)).context("load config")
}

fn has_messages(thread_id: &str) -> Result<bool> {
    Ok(thread_persistence::load_thread_events(thread_id)?
        .iter()
        .any(|event| matches!(event, ThreadEvent::Message { .. })))
}

/// Applies `update` to one thread's metadata. Every write takes the thread
/// lock, so it is safe next to a running turn.
///
/// # Errors
/// `invalid` for an unusable id, model, profile, or root; `conflict` for a
/// profile on a thread with messages; otherwise the write error.
pub fn update_thread(update: &UpdateThread) -> Result<ThreadUpdated> {
    let id = update.thread_id.as_str();
    validate_thread_id(id)?;
    if let Some(Some(alias)) = &update.alias_to {
        validate_thread_id(alias)?;
    }
    let stored_root = thread_persistence::read_thread_root_path(id)?.map(PathBuf::from);
    let run_root = update.root.clone().or_else(|| stored_root.clone());
    if let Some(root) = &update.root
        && !root.is_dir()
    {
        return Err(coded(
            ErrorCode::Invalid,
            format!("root does not exist: {}", root.display()),
        ));
    }

    let model = match &update.model {
        Some(Some(spec)) => {
            let config_root = update
                .config_root
                .clone()
                .or_else(|| run_root.clone())
                .ok_or_else(|| {
                    coded(
                        ErrorCode::Invalid,
                        "a model needs `config_root` or a thread root to check it against",
                    )
                })?;
            let mut config = load_config(&config_root)?;
            if let Some(run_root) = &run_root {
                config.use_modes_of(&config_root, run_root)?;
            }
            config
                .apply_model_selection(spec)
                .map_err(|err| coded(ErrorCode::Invalid, err.to_string()))?;
            Some(Some((config.model_selection(), config.model_label())))
        }
        Some(None) => Some(None),
        None => None,
    };

    if let Some(profile) = &update.persistent_profile {
        if has_messages(id)? {
            return Err(coded(
                ErrorCode::Conflict,
                format!("thread '{id}' already has messages; only a new thread takes a profile"),
            ));
        }
        // Checked where it will run: the new root, else the stored one, else
        // the root its first turn will be submitted with (`config_root`),
        // which records it then. No root is stored for this check.
        let root = run_root
            .clone()
            .or_else(|| update.config_root.clone())
            .ok_or_else(|| {
                coded(
                    ErrorCode::Invalid,
                    "a persistent profile needs `config_root`, `root`, or a thread root to load it from",
                )
            })?;
        crate::subagents::load_persistent_profile(&root, profile)
            .map_err(|err| coded(ErrorCode::Invalid, format!("{err:#}")))?;
    }

    let mut thread = Thread::with_id(id.to_string())?;
    if let Some(source) = &update.handoff_from {
        validate_thread_id(source)?;
        thread.set_handoff_from(Some(source.clone()));
    }
    if let Some(profile) = &update.persistent_profile {
        thread.set_persistent_profile(profile)?;
    }
    if let Some(root) = &update.root {
        thread.set_root_path(root)?;
    }
    let mut updated = ThreadUpdated {
        thread_id: id.to_string(),
        ..ThreadUpdated::default()
    };
    match model {
        Some(Some((selection, label))) => {
            thread.set_model_override(Some(selection))?;
            updated.model_label = Some(label);
        }
        Some(None) => thread.set_model_override(None)?,
        None => {}
    }
    if let Some(thinking) = update.thinking {
        thread.set_thinking_override(thinking)?;
    }
    if let Some(alias) = &update.alias_to {
        thread.set_alias(alias.clone())?;
    }
    if let Some(pending) = update.pending_topic_title {
        thread.set_pending_topic_title(pending)?;
    }
    if let Some(title) = &update.title {
        updated.title = thread.set_title(title.clone())?;
    }
    // A seed that only names its lineage still has to exist.
    if update.handoff_from.is_some() {
        thread.ensure_written()?;
    }
    Ok(updated)
}

/// Appends one delivery record to a thread: write the pending record before
/// sending and the final one after, with the same id. `thread_id` is the
/// thread that holds the turn (after its alias hop). Returns the record's
/// problem summary ([`crate::core::delivery::user_summary`]); post it to the
/// chat only for the final record.
///
/// # Errors
/// `invalid` for an unusable id or a thread that does not exist; otherwise
/// the write error.
pub fn record_delivery(thread_id: &str, record: DeliveryRecord) -> Result<Option<String>> {
    validate_thread_id(thread_id)?;
    if !thread_persistence::thread_exists(thread_id) {
        return Err(coded(
            ErrorCode::NotFound,
            format!("thread '{thread_id}' does not exist"),
        ));
    }
    let summary = crate::core::delivery::user_summary(&record);
    Thread::with_id(thread_id.to_string())?
        .append(&ThreadEvent::delivery(record))
        .context("append delivery record")?;
    Ok(summary)
}

/// How a chat conversation's messages run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThreadKind {
    /// An ordinary agent thread: submit with `ordinary`.
    Ordinary,
    /// A persistent-profile thread: submit without `ordinary`.
    Profile,
    /// A worker's legacy mirror topic: messages go to `enqueue_from_topic`
    /// for `effective_thread_id` (the worker), never to a turn here.
    WorkerMirror,
}

/// Where a chat conversation's messages go ([`resolve_thread`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThreadRoute {
    /// The conversation's own id (`telegram-<chat>[-topic-<topic>]`).
    pub thread_id: String,
    /// The thread its turns run on, after one `alias_to` hop.
    pub effective_thread_id: String,
    pub kind: ThreadKind,
    /// Profile of a `profile` thread.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
    /// The effective thread has a file yet.
    pub exists: bool,
    /// Its stored root, when it has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root: Option<String>,
    /// The first turn should title the topic.
    pub pending_topic_title: bool,
}

/// Resolves a chat conversation (a Telegram chat, or one topic of it) to
/// the thread and kind its messages run as, the way the bot routes them.
///
/// # Errors
/// Returns an error when thread metadata cannot be read.
pub fn resolve_thread(chat_id: i64, topic_id: Option<i64>) -> Result<ThreadRoute> {
    let thread_id = crate::telegram::conversation_thread_id(chat_id, topic_id);
    let mirror = thread_persistence::read_thread_worker_topic(&thread_id)?;
    let effective =
        thread_persistence::read_thread_alias(&thread_id)?.unwrap_or_else(|| thread_id.clone());
    let profile = if mirror {
        None
    } else {
        thread_persistence::read_persistent_profile(&effective)?
    };
    let kind = if mirror {
        ThreadKind::WorkerMirror
    } else if profile.is_some() {
        ThreadKind::Profile
    } else {
        ThreadKind::Ordinary
    };
    Ok(ThreadRoute {
        exists: thread_persistence::thread_exists(&effective),
        root: thread_persistence::read_thread_root_path(&effective)?,
        pending_topic_title: thread_persistence::read_thread_pending_topic_title(&effective)?,
        thread_id,
        effective_thread_id: effective,
        kind,
        profile,
    })
}

/// Deletes a thread's history (`/new` in a chat without topics). Refused
/// while a turn runs on it anywhere.
///
/// # Errors
/// `invalid` for an unusable id, `conflict` while a turn runs; otherwise the
/// removal error.
pub fn reset_thread(thread_id: &str) -> Result<()> {
    validate_thread_id(thread_id)?;
    // Held while the file goes, so no turn can start on it meanwhile.
    let _lease =
        crate::core::thread_lease::ThreadRunLease::try_acquire(thread_id).map_err(|err| {
            if crate::core::thread_lease::is_busy(&err) {
                coded(
                    ErrorCode::Conflict,
                    format!("thread '{thread_id}' has a turn running; cancel it first"),
                )
            } else {
                err
            }
        })?;
    thread_persistence::remove_thread_file(thread_id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::delivery::{DeliveryItem, DeliveryRecord};

    #[test]
    fn update_distinguishes_absent_from_null() {
        let update: UpdateThread =
            serde_json::from_str(r#"{"thread_id":"t","model":null,"pending_topic_title":true}"#)
                .unwrap();
        assert_eq!(update.model, Some(None));
        assert_eq!(update.alias_to, None);
        assert_eq!(update.pending_topic_title, Some(true));
    }

    #[test]
    fn update_sets_and_clears_meta_and_refuses_bad_input() {
        let _home = crate::test_support::temp_zdx_home();
        let root = tempfile::tempdir().unwrap();
        let updated = update_thread(&UpdateThread {
            thread_id: "telegram-1-topic-2".to_string(),
            root: Some(root.path().to_path_buf()),
            model: Some(Some("anthropic:claude-sonnet-5".to_string())),
            alias_to: Some(Some("source".to_string())),
            pending_topic_title: Some(true),
            handoff_from: Some("source".to_string()),
            ..UpdateThread::default()
        })
        .unwrap();
        assert!(updated.model_label.is_some());
        let id = "telegram-1-topic-2";
        assert_eq!(
            thread_persistence::read_thread_model_override(id).unwrap(),
            Some("anthropic:claude-sonnet-5@off".to_string()),
            "stored as the selection, like /model"
        );
        assert_eq!(
            thread_persistence::read_thread_alias(id).unwrap(),
            Some("source".to_string())
        );
        assert!(thread_persistence::read_thread_pending_topic_title(id).unwrap());

        update_thread(&UpdateThread {
            thread_id: id.to_string(),
            model: Some(None),
            alias_to: Some(None),
            ..UpdateThread::default()
        })
        .unwrap();
        assert_eq!(
            thread_persistence::read_thread_model_override(id).unwrap(),
            None
        );
        assert_eq!(thread_persistence::read_thread_alias(id).unwrap(), None);

        let err = update_thread(&UpdateThread {
            thread_id: "../x".to_string(),
            ..UpdateThread::default()
        })
        .unwrap_err();
        assert_eq!(error_code(&err), Some(ErrorCode::Invalid));
        let err = update_thread(&UpdateThread {
            thread_id: id.to_string(),
            model: Some(Some("mode:undefined".to_string())),
            ..UpdateThread::default()
        })
        .unwrap_err();
        assert_eq!(error_code(&err), Some(ErrorCode::Invalid), "{err:#}");
    }

    #[test]
    fn a_profile_goes_only_on_a_thread_without_messages() {
        let _home = crate::test_support::temp_zdx_home();
        let root = tempfile::tempdir().unwrap();
        let profile = |id: &str| UpdateThread {
            thread_id: id.to_string(),
            root: Some(root.path().to_path_buf()),
            persistent_profile: Some("orchestrator".to_string()),
            ..UpdateThread::default()
        };
        update_thread(&profile("fresh")).unwrap();
        assert_eq!(
            thread_persistence::read_persistent_profile("fresh")
                .unwrap()
                .as_deref(),
            Some("orchestrator")
        );

        let mut thread = Thread::with_id("used".to_string()).unwrap();
        thread.append(&ThreadEvent::user_message("hello")).unwrap();
        let err = update_thread(&profile("used")).unwrap_err();
        assert_eq!(error_code(&err), Some(ErrorCode::Conflict));

        // A profile checked against the chat's config root stores no root:
        // the thread follows the chat's binding until its first turn.
        update_thread(&UpdateThread {
            thread_id: "unrooted".to_string(),
            config_root: Some(root.path().to_path_buf()),
            persistent_profile: Some("orchestrator".to_string()),
            ..UpdateThread::default()
        })
        .unwrap();
        assert!(
            thread_persistence::read_thread_root_path("unrooted")
                .unwrap()
                .is_none()
        );
        let err = update_thread(&UpdateThread {
            thread_id: "nowhere".to_string(),
            persistent_profile: Some("orchestrator".to_string()),
            ..UpdateThread::default()
        })
        .unwrap_err();
        assert_eq!(error_code(&err), Some(ErrorCode::Invalid));
    }

    #[test]
    fn delivery_records_append_and_reset_removes_the_thread() {
        let _home = crate::test_support::temp_zdx_home();
        let record = DeliveryRecord::new("telegram", Some(-1), Some(2), vec![DeliveryItem::text()]);
        let err = record_delivery("missing", record.clone()).unwrap_err();
        assert_eq!(error_code(&err), Some(ErrorCode::NotFound));

        let mut thread = Thread::with_id("chat".to_string()).unwrap();
        thread.append(&ThreadEvent::user_message("hello")).unwrap();
        let mut record = record;
        assert!(
            record_delivery("chat", record.clone()).unwrap().is_some(),
            "a pending item is a problem until it is sent"
        );
        record.items[0].mark_sent(Some(11));
        assert_eq!(record_delivery("chat", record).unwrap(), None);
        let events = thread_persistence::load_thread_events("chat").unwrap();
        assert!(
            events
                .iter()
                .any(|event| matches!(event, ThreadEvent::Delivery { .. }))
        );

        reset_thread("chat").unwrap();
        assert!(!thread_persistence::thread_exists("chat"));
    }
}
