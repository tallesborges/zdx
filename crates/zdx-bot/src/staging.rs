//! Staged (memory-only) slash-command flow for input-taking commands.
//!
//! `/handoff` and `/prompt-builder` enter a per-topic staging session: the
//! next message (text or transcribed voice) is consumed as command input
//! instead of running a normal agent turn, a generated suggestion is shown
//! with Accept / Discard buttons, and sending another message regenerates the
//! suggestion. Nothing is persisted to the real thread until Accept; Discard
//! deletes the staging messages and leaves the topic as it was. Accept:
//! handoff seeds a new topic; prompt-builder runs the generated prompt as the
//! user's real message in the current topic.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::Result;
use serde_json::json;
use zdx_engine::core::handoff_generation::generate_handoff;
use zdx_engine::core::prompt_builder_generation::generate_prompt_builder;
use zdx_engine::core::thread_persistence;

use crate::bot::context::BotContext;
use crate::bot::queue::{ChatQueueMap, dispatch_message};
use crate::commands::{BotCommand, parse_command};
use crate::handlers::message::{escape_html, thread_id_for_chat};
use crate::telegram::{CallbackQuery, InlineKeyboardButton, InlineKeyboardMarkup, TelegramClient};
use crate::types::IncomingMessage;

/// Stale staging sessions are dropped on the next message so it runs as a
/// normal turn instead of being swallowed as command input.
const STAGING_TTL: Duration = Duration::from_mins(15);

/// Max characters of the generated suggestion shown in the preview message
/// (Telegram caps messages at 4096 chars including HTML tags).
const SUGGESTION_PREVIEW_MAX_CHARS: usize = 3000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StagingCommand {
    Handoff,
    PromptBuilder,
}

pub(crate) struct StagingSession {
    command: StagingCommand,
    /// Full generated suggestion (the preview message may be truncated).
    suggestion_text: Option<String>,
    /// The bot's current suggestion message (deleted/replaced on regenerate).
    suggestion_message_id: Option<i64>,
    /// Other bot staging messages (ask-for-input prompt, hints, errors).
    bot_message_ids: Vec<i64>,
    /// The user's staging messages (best-effort deletion on cleanup).
    user_message_ids: Vec<i64>,
    created_at: Instant,
}

impl StagingSession {
    fn is_expired(&self) -> bool {
        self.created_at.elapsed() > STAGING_TTL
    }
}

/// Active staging sessions keyed by `thread_id`.
pub(crate) type StagingMap = Arc<Mutex<HashMap<String, StagingSession>>>;

pub(crate) fn new_staging_map() -> StagingMap {
    Arc::new(Mutex::new(HashMap::new()))
}

/// Handles the staging flow for one incoming message. Returns `true` when the
/// message was consumed (command start or staged input) and MUST NOT run a
/// normal agent turn.
pub(crate) async fn handle_staging_flow(
    context: &BotContext,
    incoming: &IncomingMessage,
    reply_to_message_id: Option<i64>,
    topic_id: Option<i64>,
    thread_id: &str,
) -> Result<bool> {
    if let Some(command) = staged_command_request(incoming) {
        start_staging(
            context,
            incoming,
            reply_to_message_id,
            topic_id,
            thread_id,
            command,
        )
        .await?;
        return Ok(true);
    }

    // Drop stale sessions so the message below runs as a normal turn.
    let expired = {
        let mut map = context.staging_map().lock().expect("staging lock poisoned");
        match map.get(thread_id) {
            Some(session) if session.is_expired() => map.remove(thread_id),
            Some(_) => None,
            None => return Ok(false),
        }
    };
    if let Some(session) = expired {
        cleanup_session_messages(context, incoming.chat_id, &session).await;
        return Ok(false);
    }

    if incoming
        .text
        .as_deref()
        .is_some_and(|text| text.trim() == "/cancel")
    {
        let session = {
            let mut map = context.staging_map().lock().expect("staging lock poisoned");
            map.remove(thread_id)
        };
        if let Some(mut session) = session {
            session.user_message_ids.push(incoming.message_id);
            cleanup_session_messages(context, incoming.chat_id, &session).await;
        }
        return Ok(true);
    }

    process_staged_input(context, incoming, topic_id, thread_id).await?;
    Ok(true)
}

fn staged_command_request(incoming: &IncomingMessage) -> Option<StagingCommand> {
    if !incoming.images.is_empty() || !incoming.audios.is_empty() {
        return None;
    }
    match incoming.text.as_deref().and_then(parse_command)? {
        BotCommand::Handoff => Some(StagingCommand::Handoff),
        BotCommand::PromptBuilder => Some(StagingCommand::PromptBuilder),
        _ => None,
    }
}

/// Effective free-form input: typed text, or the first voice transcript.
fn effective_input_text(incoming: &IncomingMessage) -> Option<&str> {
    incoming
        .text
        .as_deref()
        .or_else(|| incoming.audios.iter().find_map(|a| a.transcript.as_deref()))
        .map(str::trim)
        .filter(|t| !t.is_empty())
}

async fn start_staging(
    context: &BotContext,
    incoming: &IncomingMessage,
    reply_to_message_id: Option<i64>,
    topic_id: Option<i64>,
    thread_id: &str,
    command: StagingCommand,
) -> Result<()> {
    // Handoff needs a forum topic to create the new topic in; prompt-builder
    // works anywhere (the accepted prompt runs in the current chat).
    if matches!(command, StagingCommand::Handoff) && (!incoming.is_forum || topic_id.is_none()) {
        context
            .client()
            .send_message(
                incoming.chat_id,
                "/handoff needs a forum topic (a group with topics enabled).",
                reply_to_message_id,
                topic_id,
            )
            .await?;
        return Ok(());
    }

    // Restarting a staged command replaces any existing session for this topic.
    let previous = {
        let mut map = context.staging_map().lock().expect("staging lock poisoned");
        map.remove(thread_id)
    };
    if let Some(previous) = previous {
        cleanup_session_messages(context, incoming.chat_id, &previous).await;
    }

    let ask_text = match command {
        StagingCommand::Handoff => {
            "🔀 <b>Handoff</b>\nSend the message (text or voice) to start the new topic with — I'll show a preview to accept or discard. Send /cancel to abort."
        }
        StagingCommand::PromptBuilder => {
            "🛠 <b>Prompt builder</b>\nSend your intent (text or voice) — I'll draft a prompt you can accept to run here, or discard. Send /cancel to abort."
        }
    };
    let ask = context
        .client()
        .send_message_with_markup(
            incoming.chat_id,
            ask_text,
            reply_to_message_id,
            topic_id,
            &discard_only_keyboard(),
        )
        .await?;

    let session = StagingSession {
        command,
        suggestion_text: None,
        suggestion_message_id: None,
        bot_message_ids: vec![ask.id],
        user_message_ids: vec![incoming.message_id],
        created_at: Instant::now(),
    };
    let mut map = context.staging_map().lock().expect("staging lock poisoned");
    map.insert(thread_id.to_string(), session);
    Ok(())
}

async fn process_staged_input(
    context: &BotContext,
    incoming: &IncomingMessage,
    topic_id: Option<i64>,
    thread_id: &str,
) -> Result<()> {
    // Track the input message and take the previous suggestion (regenerate).
    let (command, previous_suggestion) = {
        let mut map = context.staging_map().lock().expect("staging lock poisoned");
        let Some(session) = map.get_mut(thread_id) else {
            return Ok(());
        };
        session.user_message_ids.push(incoming.message_id);
        session.suggestion_text = None;
        (session.command, session.suggestion_message_id.take())
    };
    if let Some(message_id) = previous_suggestion
        && let Err(err) = context
            .client()
            .delete_message(incoming.chat_id, message_id)
            .await
    {
        tracing::warn!(message_id, %err, "Failed to delete previous staging suggestion");
    }

    let Some(input) = effective_input_text(incoming) else {
        let hint_text = match command {
            StagingCommand::Handoff => {
                "Send text or a voice note to use as the handoff message, or /cancel to abort."
            }
            StagingCommand::PromptBuilder => {
                "Send text or a voice note describing the prompt you want, or /cancel to abort."
            }
        };
        let hint = context
            .client()
            .send_message_with_markup(
                incoming.chat_id,
                hint_text,
                Some(incoming.message_id),
                topic_id,
                &discard_only_keyboard(),
            )
            .await?;
        track_bot_message(context, thread_id, hint.id);
        return Ok(());
    };

    let generating_text = match command {
        StagingCommand::Handoff => "⏳ Generating handoff…",
        StagingCommand::PromptBuilder => "⏳ Building prompt…",
    };
    let generating = context
        .client()
        .send_message_with_markup(
            incoming.chat_id,
            generating_text,
            Some(incoming.message_id),
            topic_id,
            &InlineKeyboardMarkup {
                inline_keyboard: vec![],
            },
        )
        .await?;

    let result = match command {
        StagingCommand::Handoff => {
            run_handoff_generation(context, incoming, thread_id, input).await
        }
        StagingCommand::PromptBuilder => {
            run_prompt_builder_generation(context, incoming, thread_id, input).await
        }
    };
    present_generation_result(
        context,
        incoming.chat_id,
        thread_id,
        command,
        generating.id,
        result,
    )
    .await;
    Ok(())
}

/// Edits the "generating…" message into the suggestion preview (with Accept /
/// Discard buttons) or an error, updating the session accordingly.
async fn present_generation_result(
    context: &BotContext,
    chat_id: i64,
    thread_id: &str,
    command: StagingCommand,
    generating_message_id: i64,
    result: Result<String>,
) {
    match result {
        Ok(suggestion) => {
            let preview = suggestion_preview(command, &suggestion);
            if let Err(err) = context
                .client()
                .edit_message_text(
                    chat_id,
                    generating_message_id,
                    &preview,
                    Some(&accept_discard_keyboard()),
                )
                .await
            {
                tracing::warn!(%err, "Failed to show staging suggestion");
            }

            let stale = {
                let mut map = context.staging_map().lock().expect("staging lock poisoned");
                match map.get_mut(thread_id) {
                    Some(session) => {
                        session.suggestion_text = Some(suggestion);
                        session.suggestion_message_id = Some(generating_message_id);
                        false
                    }
                    None => true,
                }
            };
            if stale {
                // Session was discarded while generating; drop the suggestion.
                let _ = context
                    .client()
                    .delete_message(chat_id, generating_message_id)
                    .await;
            }
        }
        Err(err) => {
            let message = format!(
                "⚠️ Generation failed:\n<blockquote><code>{}</code></blockquote>\nSend another message to retry, or /cancel.",
                escape_html(&format!("{err:#}"))
            );
            if let Err(edit_err) = context
                .client()
                .edit_message_text(chat_id, generating_message_id, &message, None)
                .await
            {
                tracing::warn!(%edit_err, "Failed to show staging generation error");
            }
            track_bot_message(context, thread_id, generating_message_id);
        }
    }
}

async fn run_handoff_generation(
    context: &BotContext,
    incoming: &IncomingMessage,
    thread_id: &str,
    input: &str,
) -> Result<String> {
    let config = context.config();
    let resolved_root = context.root_for_chat(incoming.chat_id);
    let root = thread_persistence::read_thread_root_path(thread_id)?
        .map_or(resolved_root.root, PathBuf::from);
    generate_handoff(thread_id, input, &config.handoff_model, &root, None).await
}

async fn run_prompt_builder_generation(
    context: &BotContext,
    incoming: &IncomingMessage,
    thread_id: &str,
    input: &str,
) -> Result<String> {
    let config = context.config();
    let resolved_root = context.root_for_chat(incoming.chat_id);
    let root = thread_persistence::read_thread_root_path(thread_id)?
        .map_or(resolved_root.root, PathBuf::from);
    generate_prompt_builder(
        input,
        Some(config.prompt_builder_model.clone()),
        &root,
        None,
    )
    .await
}

/// Handles `stg:{action}` callbacks: `a` accepts the staged suggestion, `d`
/// discards the session and deletes its messages.
pub(crate) async fn handle_callback(
    context: &Arc<BotContext>,
    queues: &ChatQueueMap,
    client: &TelegramClient,
    callback: &CallbackQuery,
    data: &str,
) {
    let Some(message) = callback.message.as_ref() else {
        let _ = client
            .answer_callback_query(&callback.id, Some("No message context"))
            .await;
        return;
    };
    let chat_id = message.chat.id;
    let topic_id = message.effective_thread_id();
    let thread_id = thread_id_for_chat(chat_id, topic_id);

    let session = {
        let mut map = context.staging_map().lock().expect("staging lock poisoned");
        map.remove(&thread_id)
    };
    let Some(session) = session else {
        let _ = client
            .answer_callback_query(&callback.id, Some("No active staged command here"))
            .await;
        return;
    };

    match data {
        "d" => {
            cleanup_session_messages(context, chat_id, &session).await;
            let _ = client
                .answer_callback_query(&callback.id, Some("Discarded ✓"))
                .await;
        }
        "a" => match session.command {
            StagingCommand::Handoff => {
                accept_handoff(
                    context, queues, client, callback, chat_id, &thread_id, session,
                )
                .await;
            }
            StagingCommand::PromptBuilder => {
                accept_prompt_builder(context, queues, client, callback, &thread_id, session).await;
            }
        },
        _ => {
            let _ = client.answer_callback_query(&callback.id, None).await;
            tracing::warn!(?data, "Unknown staging callback");
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn accept_handoff(
    context: &Arc<BotContext>,
    queues: &ChatQueueMap,
    client: &TelegramClient,
    callback: &CallbackQuery,
    chat_id: i64,
    source_thread_id: &str,
    session: StagingSession,
) {
    let Some(suggestion) = session.suggestion_text.clone() else {
        let _ = client
            .answer_callback_query(&callback.id, Some("Nothing staged to accept yet"))
            .await;
        let mut map = context.staging_map().lock().expect("staging lock poisoned");
        map.insert(source_thread_id.to_string(), session);
        return;
    };

    let topic_name = chrono::Utc::now()
        .format("Handoff %Y-%m-%d %H:%M")
        .to_string();
    let new_topic_id = match client.create_forum_topic(chat_id, &topic_name).await {
        Ok(topic_id) => topic_id,
        Err(err) => {
            tracing::error!(chat_id, %err, "Failed to create handoff topic");
            let _ = client
                .answer_callback_query(&callback.id, Some("Failed to create the new topic"))
                .await;
            let mut map = context.staging_map().lock().expect("staging lock poisoned");
            map.insert(source_thread_id.to_string(), session);
            return;
        }
    };

    // Pre-create the new thread so its meta records the handoff lineage and
    // a pending auto-title before the first turn opens it. Inherit the source
    // thread's model/thinking overrides so the handoff continues with the same
    // effective model and thinking level.
    let inherited_model = thread_persistence::read_thread_model_override(source_thread_id)
        .ok()
        .flatten();
    let inherited_thinking = thread_persistence::read_thread_thinking_override(source_thread_id)
        .ok()
        .flatten();
    let new_thread_id = thread_id_for_chat(chat_id, Some(new_topic_id));
    let created = thread_persistence::Thread::with_id(new_thread_id).and_then(|mut thread| {
        thread.set_handoff_from(Some(source_thread_id.to_string()));
        if let Some(model) = inherited_model {
            thread.set_model_override(Some(model))?;
        }
        if let Some(level) = inherited_thinking {
            thread.set_thinking_override(Some(level))?;
        }
        thread.set_pending_topic_title(true)
    });
    if let Err(err) = created {
        tracing::warn!(chat_id, new_topic_id, %err, "Failed to record handoff lineage on new thread");
    }

    cleanup_session_messages(context, chat_id, &session).await;
    let _ = client
        .answer_callback_query(&callback.id, Some("Handoff topic created ✓"))
        .await;

    let synthetic: Result<crate::telegram::Message, _> = serde_json::from_value(json!({
        "message_id": new_topic_id,
        "chat": { "id": chat_id, "type": "supergroup", "is_forum": true },
        "from": { "id": callback.from.id, "is_bot": false },
        "text": suggestion,
        "message_thread_id": new_topic_id,
    }));
    match synthetic {
        Ok(synthetic) => dispatch_message(queues, context, synthetic).await,
        Err(err) => {
            tracing::error!(chat_id, %err, "Failed to synthesize handoff message");
        }
    }
}

/// Accepts a staged prompt-builder suggestion: the generated prompt becomes
/// the user's real message and runs a normal agent turn in the current topic.
/// The suggestion message is kept (edited) as the reply anchor for the turn.
async fn accept_prompt_builder(
    context: &Arc<BotContext>,
    queues: &ChatQueueMap,
    client: &TelegramClient,
    callback: &CallbackQuery,
    source_thread_id: &str,
    mut session: StagingSession,
) {
    let Some(message) = callback.message.as_ref() else {
        return;
    };
    let chat_id = message.chat.id;

    let Some(suggestion) = session.suggestion_text.clone() else {
        let _ = client
            .answer_callback_query(&callback.id, Some("Nothing staged to accept yet"))
            .await;
        let mut map = context.staging_map().lock().expect("staging lock poisoned");
        map.insert(source_thread_id.to_string(), session);
        return;
    };

    // Keep the suggestion message as the turn's reply anchor; drop its buttons.
    let _ = client
        .edit_message_text(chat_id, message.id, "▶️ Prompt accepted — running…", None)
        .await;
    session.suggestion_message_id = None;
    cleanup_session_messages(context, chat_id, &session).await;
    let _ = client
        .answer_callback_query(&callback.id, Some("Running prompt ✓"))
        .await;

    let chat_kind = if message.chat.is_private() {
        "private"
    } else {
        "supergroup"
    };
    let synthetic: Result<crate::telegram::Message, _> = serde_json::from_value(json!({
        "message_id": message.id,
        "chat": {
            "id": chat_id,
            "type": chat_kind,
            "is_forum": message.chat.is_forum_enabled(),
        },
        "from": { "id": callback.from.id, "is_bot": false },
        "text": suggestion,
        "message_thread_id": message.effective_thread_id(),
    }));
    match synthetic {
        Ok(synthetic) => dispatch_message(queues, context, synthetic).await,
        Err(err) => {
            tracing::error!(chat_id, %err, "Failed to synthesize prompt-builder message");
        }
    }
}

fn track_bot_message(context: &BotContext, thread_id: &str, message_id: i64) {
    let mut map = context.staging_map().lock().expect("staging lock poisoned");
    if let Some(session) = map.get_mut(thread_id) {
        session.bot_message_ids.push(message_id);
    }
}

/// Deletes all staging messages: the bot's own always work; the user's are
/// best-effort (needs `can_delete_messages`, impossible in DMs).
async fn cleanup_session_messages(context: &BotContext, chat_id: i64, session: &StagingSession) {
    let bot_ids = session
        .bot_message_ids
        .iter()
        .chain(session.suggestion_message_id.iter());
    for &message_id in bot_ids {
        if let Err(err) = context.client().delete_message(chat_id, message_id).await {
            tracing::warn!(message_id, %err, "Failed to delete bot staging message");
        }
    }
    for &message_id in &session.user_message_ids {
        if let Err(err) = context.client().delete_message(chat_id, message_id).await {
            tracing::debug!(message_id, %err, "Could not delete user staging message (needs can_delete_messages)");
        }
    }
}

fn suggestion_preview(command: StagingCommand, suggestion: &str) -> String {
    let (icon, title, hint) = match command {
        StagingCommand::Handoff => (
            "🔀",
            "Handoff preview",
            "Accept opens a new topic seeded with this context. Send another message to regenerate.",
        ),
        StagingCommand::PromptBuilder => (
            "🛠",
            "Prompt preview",
            "Accept runs this prompt here. Send another message to regenerate.",
        ),
    };
    let truncated = truncate_chars(suggestion, SUGGESTION_PREVIEW_MAX_CHARS);
    format!(
        "{icon} <b>{title}</b>\n<blockquote>{}</blockquote>\n<i>{hint}</i>",
        escape_html(&truncated)
    )
}

fn accept_discard_keyboard() -> InlineKeyboardMarkup {
    InlineKeyboardMarkup {
        inline_keyboard: vec![vec![
            InlineKeyboardButton {
                text: "✅ Accept".to_string(),
                callback_data: Some("stg:a".to_string()),
                url: None,
            },
            InlineKeyboardButton {
                text: "🗑 Discard".to_string(),
                callback_data: Some("stg:d".to_string()),
                url: None,
            },
        ]],
    }
}

fn discard_only_keyboard() -> InlineKeyboardMarkup {
    InlineKeyboardMarkup {
        inline_keyboard: vec![vec![InlineKeyboardButton {
            text: "🗑 Discard".to_string(),
            callback_data: Some("stg:d".to_string()),
            url: None,
        }]],
    }
}

fn truncate_chars(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let truncated: String = text.chars().take(max_chars.saturating_sub(1)).collect();
    format!("{truncated}…")
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::{STAGING_TTL, StagingCommand, StagingSession, suggestion_preview, truncate_chars};

    fn session_created_at(created_at: Instant) -> StagingSession {
        StagingSession {
            command: StagingCommand::Handoff,
            suggestion_text: None,
            suggestion_message_id: None,
            bot_message_ids: vec![],
            user_message_ids: vec![],
            created_at,
        }
    }

    #[test]
    fn fresh_session_is_not_expired() {
        assert!(!session_created_at(Instant::now()).is_expired());
    }

    #[test]
    fn old_session_is_expired() {
        let old = Instant::now()
            .checked_sub(STAGING_TTL + Duration::from_secs(1))
            .expect("clock supports back-dating");
        assert!(session_created_at(old).is_expired());
    }

    #[test]
    fn suggestion_preview_escapes_html_and_keeps_actions_hint() {
        let preview = suggestion_preview(StagingCommand::Handoff, "fix <script> & stuff");
        assert!(preview.contains("Handoff preview"));
        assert!(preview.contains("fix &lt;script&gt; &amp; stuff"));
        assert!(preview.contains("regenerate"));
    }

    #[test]
    fn prompt_builder_preview_says_it_runs_here() {
        let preview = suggestion_preview(StagingCommand::PromptBuilder, "do the thing");
        assert!(preview.contains("Prompt preview"));
        assert!(preview.contains("runs this prompt here"));
        assert!(preview.contains("regenerate"));
    }

    #[test]
    fn suggestion_preview_truncates_long_suggestions() {
        let long = "x".repeat(10_000);
        let preview = suggestion_preview(StagingCommand::Handoff, &long);
        assert!(preview.chars().count() < 3_500);
        assert!(preview.contains('…'));
    }

    #[test]
    fn truncate_keeps_short_text_verbatim() {
        assert_eq!(truncate_chars("short", 10), "short");
    }
}
