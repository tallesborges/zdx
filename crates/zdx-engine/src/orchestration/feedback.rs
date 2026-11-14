//! `feedback_submit`: Mini App annotations on an artifact, saved and queued
//! as a turn in the chat conversation that owns the thread.
//!
//! The order keeps retries safe. Everything that can refuse runs before
//! anything is saved (parse, route, sender). The bundle is saved, its media
//! prepared (inline images, transcripts) and the turn text composed; then the
//! bundle is marked queued and the input queued. A failed queue removes the
//! marker again, so an error reply always means nothing is queued and a retry
//! queues it. A retry of a queued submission is answered `duplicate`.
//!
//! The queue is in memory: `queued` does not mean a model read it, and a
//! service restart before the turn runs loses it. Not exactly-once.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use super::protocol::{Delivery, ErrorCode, Follow, Route, Submit};
use super::runtime::Orchestrator;
use super::thread_ops::{self, ThreadKind, coded, validate_thread_id};
use super::updates::InputImage;
use crate::core::artifact_feedback::{
    self, FeedbackError, FeedbackSubmission, SaveOutcome, SavedFeedback,
};
use crate::core::chat_turn::{AudioInput, ImageInput, UserInput, compose_user_text};
use crate::core::thread_persistence;

/// Submissions decoded at once. A submission can be tens of MiB of base64.
pub const FEEDBACK_CONCURRENCY: usize = 2;

/// `{op:"feedback_submit"}`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeedbackSubmit {
    /// The thread the user opened (a chat conversation id); never `active`.
    pub thread_id: String,
    /// The Telegram user who sends it.
    pub user_id: i64,
    /// The chat's root: its config gives the allowlists and transcription,
    /// and the turn's config layers load from it.
    pub root: PathBuf,
    /// The exact HTTP request body, as a JSON string.
    pub submission: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FeedbackStatus {
    /// Saved and queued as a turn now.
    Queued,
    /// This submission was queued before; nothing was queued again.
    Duplicate,
}

/// `{reply:"feedback"}`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeedbackReceipt {
    pub status: FeedbackStatus,
    /// The thread the turn runs on (after one alias hop).
    pub thread_id: String,
    /// The queued input (`queued` only; absent for a worker mirror topic).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_id: Option<u64>,
    pub annotations: usize,
    /// Sent although the file changed after annotation (`allow_stale`).
    pub stale: bool,
    /// The annotated artifact's name, for the chat notice (`queued` only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifact_name: Option<String>,
    /// Where the turn answers: post the notice there (`queued` only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub route: Option<Route>,
}

/// Submissions being saved or queued now, by effective thread and
/// `submission_id`, plus the decode slots.
pub struct FeedbackGate {
    in_flight: Mutex<HashSet<String>>,
    permits: Arc<tokio::sync::Semaphore>,
}

impl Default for FeedbackGate {
    fn default() -> Self {
        Self {
            in_flight: Mutex::new(HashSet::new()),
            permits: Arc::new(tokio::sync::Semaphore::new(FEEDBACK_CONCURRENCY)),
        }
    }
}

struct InFlight<'a> {
    gate: &'a FeedbackGate,
    key: String,
}

impl Drop for InFlight<'_> {
    fn drop(&mut self) {
        if let Ok(mut keys) = self.gate.in_flight.lock() {
            keys.remove(&self.key);
        }
    }
}

/// The chat (and topic) a bot-minted thread id belongs to.
fn route_for_thread(thread_id: &str) -> Option<Route> {
    if let Some((chat, topic)) = crate::telegram::parse_topic_conversation_id(thread_id) {
        return Some(Route {
            chat,
            topic: Some(topic),
        });
    }
    let chat = thread_id.strip_prefix("telegram-")?.parse().ok()?;
    Some(Route { chat, topic: None })
}

fn not_routable(thread_id: &str) -> anyhow::Error {
    let is_worker = thread_persistence::read_thread_summary_cached(thread_id)
        .ok()
        .flatten()
        .is_some_and(|summary| summary.is_worker());
    coded(
        ErrorCode::NotRoutable,
        if is_worker {
            "Feedback on a worker thread cannot be sent from the Mini App yet. Your notes are kept; describe them in the orchestrator's topic instead."
        } else {
            "This thread does not belong to a Telegram chat, so feedback cannot be sent from here yet. Your notes are kept."
        },
    )
}

/// The sender gate a chat message gets: an allowed user, in an allowed group
/// or in their own private chat with the bot.
fn sender_allowed(users: &[i64], chats: &[i64], user_id: i64, route: Route) -> bool {
    if !users.contains(&user_id) {
        return false;
    }
    if route.chat < 0 {
        chats.contains(&route.chat)
    } else {
        route.chat == user_id
    }
}

/// Parses the raw body: `json_syntax` when it is not JSON, `json_data` when
/// it has the wrong shape.
fn parse_submission(raw: &str) -> Result<FeedbackSubmission> {
    serde_json::from_str(raw).map_err(|err| {
        let code = if err.is_data() {
            ErrorCode::JsonData
        } else {
            ErrorCode::JsonSyntax
        };
        coded(code, format!("Invalid feedback body: {err}"))
    })
}

fn feedback_error(thread_id: &str, error: &FeedbackError) -> anyhow::Error {
    let code = match error {
        FeedbackError::Invalid(_) => ErrorCode::Invalid,
        FeedbackError::ArtifactNotFound => ErrorCode::ArtifactNotFound,
        FeedbackError::UnsupportedArtifact(_) => ErrorCode::UnsupportedArtifact,
        FeedbackError::VersionMismatch => ErrorCode::VersionMismatch,
        FeedbackError::IdConflict => ErrorCode::IdConflict,
        FeedbackError::Busy => ErrorCode::Busy,
        FeedbackError::Storage(err) => {
            tracing::error!(thread_id, error = %format!("{err:#}"), "Failed to save artifact feedback");
            return anyhow::anyhow!("Saving feedback failed");
        }
    };
    coded(code, error.to_string())
}

/// The turn's user text and inline images: the prompt plus each saved image
/// and voice note under its label, as a chat message with those attachments
/// would give.
async fn compose_turn(saved: &SavedFeedback, root: &Path) -> (String, Vec<InputImage>) {
    let mut input = UserInput {
        text: Some(saved.prompt.clone()),
        ..UserInput::default()
    };
    let mut inline = Vec::new();
    for image in &saved.images {
        match tokio::fs::read(&image.path).await {
            Ok(bytes) => {
                let prepared = crate::core::chat_turn::prepare_inline_image(&image.path, &bytes);
                inline.extend(prepared.image);
                input.images.push(ImageInput {
                    path: image.path.clone(),
                    delivery: prepared.delivery,
                    label: Some(image.label.clone()),
                });
            }
            Err(err) => {
                tracing::warn!(path = %image.path.display(), %err, "Failed to read feedback image");
                input.notices.push(format!(
                    "An image attachment saved at {} could not be read, so it is missing from this message. Tell the user.",
                    image.path.display()
                ));
            }
        }
    }
    for audio in &saved.audios {
        let file_name = audio.path.file_name().map_or_else(
            || "voice-note.webm".to_string(),
            |name| name.to_string_lossy().into_owned(),
        );
        match super::server::transcribe_audio(&audio.path, &file_name, Some(&audio.mime), root)
            .await
        {
            Ok(transcript) => input.audios.push(AudioInput {
                path: audio.path.clone(),
                transcript,
                label: Some(audio.label.clone()),
            }),
            Err(err) if thread_ops::error_code(&err) == Some(ErrorCode::NotFound) => {
                input.notices.push(format!(
                    "A voice note saved at {} could not be read, so it is missing from this message. Tell the user.",
                    audio.path.display()
                ));
            }
            Err(err) => {
                tracing::error!(error = %format!("{err:#}"), "Audio transcription failed");
                input.audios.push(AudioInput {
                    path: audio.path.clone(),
                    transcript: None,
                    label: Some(audio.label.clone()),
                });
            }
        }
    }
    (compose_user_text(&input), inline)
}

impl Orchestrator {
    /// Saves Mini App feedback and queues it as a turn in the conversation
    /// that owns `request.thread_id` (see the module docs for the order).
    ///
    /// # Errors
    /// A coded error means nothing was queued: `json_syntax`, `json_data`,
    /// `invalid`, `not_routable`, `forbidden`, `in_flight`, `busy`,
    /// `version_mismatch`, `id_conflict`, `artifact_not_found`,
    /// `unsupported_artifact`, `conflict` (another run holds the thread).
    #[allow(clippy::too_many_lines)] // the ordered steps in one place
    pub async fn feedback_submit(
        self: &Arc<Self>,
        request: FeedbackSubmit,
    ) -> Result<FeedbackReceipt> {
        let FeedbackSubmit {
            thread_id: requested,
            user_id,
            root,
            submission,
        } = request;
        let submission = parse_submission(&submission)?;
        // `active` moves to whichever thread was used last, so a write
        // through it could land in another conversation than the one shown.
        if requested == "active" {
            return Err(coded(
                ErrorCode::Invalid,
                "Reopen the thread from its chat to send feedback.",
            ));
        }
        validate_thread_id(&requested)?;
        let root = root
            .canonicalize()
            .with_context(|| format!("project root does not exist: {}", root.display()))?;
        let route = route_for_thread(&requested).ok_or_else(|| not_routable(&requested))?;
        let config = crate::config::Config::load_layered(
            &crate::config::paths::config_layer_paths_for(&root),
        )
        .context("load config")?;
        let (users, chats) = config.telegram_allowlists();
        if !sender_allowed(&users, &chats, user_id, route) {
            return Err(coded(
                ErrorCode::Forbidden,
                "You cannot send feedback into this chat",
            ));
        }
        let target = thread_ops::resolve_thread(route.chat, route.topic)?;
        let effective = target.effective_thread_id.clone();

        let gate = self.feedback_gate();
        let key = format!("{effective}/{}", submission.submission_id);
        let fresh = gate
            .in_flight
            .lock()
            .is_ok_and(|mut keys| keys.insert(key.clone()));
        if !fresh {
            return Err(coded(
                ErrorCode::InFlight,
                "This feedback is still being sent. Try again in a moment.",
            ));
        }
        let _in_flight = InFlight { gate, key };
        let Ok(permit) = Arc::clone(&gate.permits).try_acquire_owned() else {
            return Err(coded(
                ErrorCode::Busy,
                "Another feedback upload is in progress. Try again in a moment.",
            ));
        };

        // Part of the retry identity: the same id from another user or for
        // another destination is a conflict, not a duplicate.
        let binding = format!("user={user_id};dest={requested}");
        let save_thread = effective.clone();
        let saved = tokio::task::spawn_blocking(move || {
            let _permit = permit;
            artifact_feedback::save_feedback(&save_thread, &binding, &submission)
        })
        .await
        .context("feedback save task")?;
        let saved = match saved {
            Ok(SaveOutcome::Saved(saved)) => saved,
            Ok(SaveOutcome::Duplicate) => {
                return Ok(FeedbackReceipt {
                    status: FeedbackStatus::Duplicate,
                    thread_id: effective,
                    input_id: None,
                    annotations: 0,
                    stale: false,
                    artifact_name: None,
                    route: None,
                });
            }
            Err(error) => return Err(feedback_error(&effective, &error)),
        };

        let (text, images) = compose_turn(&saved, &root).await;
        artifact_feedback::mark_queued(&saved.dir)?;
        let queued = match target.kind {
            ThreadKind::WorkerMirror => self
                .workers()
                .enqueue_from_topic(&effective, &text)
                .map(|_| None),
            kind => self
                .submit(Submit {
                    thread_id: Some(effective.clone()),
                    root,
                    profile: None,
                    ordinary: kind == ThreadKind::Ordinary,
                    rerun: false,
                    model: None,
                    route: Some(route),
                    text,
                    images,
                    follow: Follow::None,
                    delivery: Delivery::Queue,
                    replay: false,
                })
                .map(|submitted| Some(submitted.input_id)),
        };
        let input_id = match queued {
            Ok(input_id) => input_id,
            Err(err) => {
                // Nothing was queued: drop the marker so a retry queues it.
                if let Err(unmark) = artifact_feedback::unmark_queued(&saved.dir) {
                    tracing::error!(error = %format!("{unmark:#}"), "Feedback not queued, and its queued marker could not be removed; a retry will report a duplicate");
                }
                return Err(err);
            }
        };
        tracing::info!(
            chat_id = route.chat,
            topic_id = ?route.topic,
            annotations = saved.annotation_count,
            "Artifact feedback queued"
        );
        Ok(FeedbackReceipt {
            status: FeedbackStatus::Queued,
            thread_id: effective,
            input_id,
            annotations: saved.annotation_count,
            stale: saved.stale,
            artifact_name: Some(saved.artifact_name),
            route: Some(route),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn routes_only_bot_minted_thread_ids() {
        assert_eq!(
            route_for_thread("telegram--1001234-topic-77"),
            Some(Route {
                chat: -1_001_234,
                topic: Some(77)
            })
        );
        assert_eq!(
            route_for_thread("telegram-4242"),
            Some(Route {
                chat: 4242,
                topic: None
            })
        );
        assert_eq!(route_for_thread("20261018-cli-thread"), None);
        assert_eq!(route_for_thread("telegram-abc"), None);
    }

    #[test]
    fn private_chats_must_be_the_senders_own() {
        let users = [7, 8];
        let chats = [-100];
        let dm = |chat| Route { chat, topic: None };
        assert!(sender_allowed(&users, &chats, 7, dm(7)));
        assert!(!sender_allowed(&users, &chats, 7, dm(8)));
        assert!(sender_allowed(&users, &chats, 7, dm(-100)));
        assert!(!sender_allowed(&users, &chats, 7, dm(-200)));
        assert!(!sender_allowed(&users, &chats, 9, dm(9)));
    }

    #[test]
    fn body_errors_are_typed() {
        let code = |raw: &str| thread_ops::error_code(&parse_submission(raw).unwrap_err());
        assert_eq!(code("{not json"), Some(ErrorCode::JsonSyntax));
        assert_eq!(code(""), Some(ErrorCode::JsonSyntax));
        assert_eq!(code(r#"{"submission_id":5}"#), Some(ErrorCode::JsonData));
    }
}
