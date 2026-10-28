//! Staged (memory-only) slash-command flow for input-taking commands.
//!
//! Each command enters a per-topic staging session: the next message (text or
//! transcribed voice) is consumed as command input instead of running a normal
//! agent turn. Nothing is persisted to the real thread until the command
//! completes; `/cancel` deletes the staging messages and leaves the topic as it
//! was.
//!
//! Only `/prompt-builder` gates on confirmation, because accepting it runs a
//! prompt as the user's real message in the *current* thread: its suggestion is
//! shown with Accept / Discard buttons and regenerates when another message is
//! sent. `/handoff` and `/btw` both complete on input with no Accept tap, since
//! they only open a brand-new topic — `/handoff` generates a context block
//! first, `/btw` makes no LLM call at all.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::Result;
use serde_json::json;
use zdx_engine::config::{Config, ThinkingLevel};
use zdx_engine::core::handoff_generation::{build_side_thread_seed, generate_handoff};
use zdx_engine::core::prompt_builder_generation::generate_prompt_builder;
use zdx_engine::core::thread_persistence;

use crate::bot::context::BotContext;
use crate::bot::queue::{ChatQueueMap, dispatch_message};
use crate::commands::{BotCommand, parse_command};
use crate::handlers::message::{
    ModelPickerScope, build_provider_keyboard, escape_html, orchestrator_topic_name,
    post_thread_header, resolve_effective_thread_id, thread_id_for_chat,
};
use crate::telegram::markdown::{to_telegram_html, truncate_telegram_html};
use crate::telegram::{CallbackQuery, InlineKeyboardButton, InlineKeyboardMarkup, TelegramClient};
use crate::types::IncomingMessage;

/// Stale staging sessions are dropped on the next message so it runs as a
/// normal turn instead of being swallowed as command input.
const STAGING_TTL: Duration = Duration::from_mins(15);

/// Max characters of a generated suggestion shown in a preview or record
/// message (Telegram caps messages at 4096 chars including HTML tags).
const SUGGESTION_PREVIEW_MAX_CHARS: usize = 3000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StagingCommand {
    Handoff,
    Btw,
    PromptBuilder,
}

impl StagingCommand {
    /// Whether accepting this command opens a brand-new forum topic (and so
    /// requires being run inside one).
    fn opens_new_topic(self) -> bool {
        matches!(self, StagingCommand::Handoff | StagingCommand::Btw)
    }
}

/// What the topic opened by `/handoff` continues as. It defaults to the source
/// thread's role and is changed with the card's `Continue as:` button.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HandoffRole {
    Agent,
    /// The reserved `orchestrator` persistent profile.
    Orchestrator,
}

impl HandoffRole {
    fn of_profile(profile: Option<&str>) -> Self {
        if profile == Some(zdx_engine::subagents::ORCHESTRATOR_SUBAGENT_NAME) {
            HandoffRole::Orchestrator
        } else {
            HandoffRole::Agent
        }
    }

    /// The role of `thread_id`, which a handoff from it continues by default.
    fn of_thread(thread_id: &str) -> Self {
        match thread_persistence::read_persistent_profile(thread_id) {
            Ok(profile) => Self::of_profile(profile.as_deref()),
            Err(err) => {
                tracing::warn!(thread_id, err = %format!("{err:#}"), "Failed to read the source thread's profile");
                HandoffRole::Agent
            }
        }
    }

    fn toggled(self) -> Self {
        match self {
            HandoffRole::Agent => HandoffRole::Orchestrator,
            HandoffRole::Orchestrator => HandoffRole::Agent,
        }
    }

    /// The persistent profile the new thread is created with.
    fn profile(self) -> Option<&'static str> {
        match self {
            HandoffRole::Agent => None,
            HandoffRole::Orchestrator => Some(zdx_engine::subagents::ORCHESTRATOR_SUBAGENT_NAME),
        }
    }

    fn label(self) -> &'static str {
        match self {
            HandoffRole::Agent => "Agent",
            HandoffRole::Orchestrator => "Orchestrator",
        }
    }
}

pub(crate) struct StagingSession {
    command: StagingCommand,
    /// `message_id` of the slash command that opened the session. Messages that
    /// predate it (e.g. one already sitting in the queue) are not its input.
    command_message_id: i64,
    /// Fully resolved model the new topic starts with, picked from the card's
    /// `🎛 Model` button. `None` means it inherits the source thread's model.
    model_override: Option<String>,
    /// Role the `/handoff` topic continues as; `None` for every other command.
    handoff_role: Option<HandoffRole>,
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

/// Whether a live `/btw` staging session is waiting for its question on this
/// thread. That input opens a side topic instead of touching the current
/// thread, so it must not wait behind a running turn in the queue.
pub(crate) fn awaiting_btw_input(
    context: &BotContext,
    thread_id: &str,
    synthetic_prompt: bool,
) -> bool {
    let map = context.staging_map().lock().expect("staging lock poisoned");
    btw_awaits_input(&map, thread_id, synthetic_prompt)
}

fn btw_awaits_input(
    sessions: &HashMap<String, StagingSession>,
    thread_id: &str,
    synthetic_prompt: bool,
) -> bool {
    !synthetic_prompt
        && sessions
            .get(thread_id)
            .is_some_and(|session| session.command == StagingCommand::Btw && !session.is_expired())
}

/// What an incoming message is to the staging session on its thread.
enum SessionInput {
    /// No live session takes this message; it runs as a normal turn.
    NotInput,
    /// The session expired and was removed; clean it up, then run the message
    /// as a normal turn.
    Expired(StagingSession),
    /// The message is the session's input.
    Input,
}

/// Decides whether the live session on `thread_id` takes this message as its
/// input. Removes the session only when it has expired.
fn classify_session_input(
    sessions: &mut HashMap<String, StagingSession>,
    thread_id: &str,
    message_id: i64,
    synthetic_prompt: bool,
) -> SessionInput {
    // Staged commands wait for the user. A bot-generated prompt (a worker
    // update) runs as its own turn and leaves the session
    // waiting; its synthetic id is always newer than the command's.
    if synthetic_prompt {
        return SessionInput::NotInput;
    }
    match sessions.get(thread_id) {
        Some(session) if session.is_expired() => sessions
            .remove(thread_id)
            .map_or(SessionInput::NotInput, SessionInput::Expired),
        // A message that predates the command was already in flight when the
        // session opened, so it is a normal turn, not staged input.
        Some(session) if message_id <= session.command_message_id => SessionInput::NotInput,
        Some(_) => SessionInput::Input,
        None => SessionInput::NotInput,
    }
}

/// Handles the staging flow for one incoming message. Returns `true` when the
/// message was consumed (command start or staged input) and MUST NOT run a
/// normal agent turn.
pub(crate) async fn handle_staging_flow(
    context: &Arc<BotContext>,
    queues: &ChatQueueMap,
    incoming: &IncomingMessage,
    reply_to_message_id: Option<i64>,
    topic_id: Option<i64>,
    thread_id: &str,
    synthetic_prompt: bool,
) -> Result<bool> {
    if !synthetic_prompt && let Some(command) = staged_command_request(incoming) {
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

    let input = {
        let mut map = context.staging_map().lock().expect("staging lock poisoned");
        classify_session_input(&mut map, thread_id, incoming.message_id, synthetic_prompt)
    };
    match input {
        SessionInput::NotInput => return Ok(false),
        // Drop stale sessions so the message runs as a normal turn.
        SessionInput::Expired(session) => {
            cleanup_session_messages(context, incoming.chat_id, &session).await;
            return Ok(false);
        }
        SessionInput::Input => {}
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

    process_staged_input(context, queues, incoming, topic_id, thread_id).await?;
    Ok(true)
}

fn staged_command_request(incoming: &IncomingMessage) -> Option<StagingCommand> {
    if !incoming.images.is_empty() || !incoming.audios.is_empty() {
        return None;
    }
    match incoming.text.as_deref().and_then(parse_command)? {
        BotCommand::Handoff => Some(StagingCommand::Handoff),
        BotCommand::Btw => Some(StagingCommand::Btw),
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
    // Handoff and btw both seed a brand-new topic, so they need a forum topic
    // to create it in; prompt-builder works anywhere (the accepted prompt runs
    // in the current chat).
    if command.opens_new_topic() && (!incoming.is_forum || topic_id.is_none()) {
        let name = match command {
            StagingCommand::Btw => "/btw",
            _ => "/handoff",
        };
        context
            .client()
            .send_message(
                incoming.chat_id,
                &format!("{name} needs a forum topic (a group with topics enabled)."),
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

    let handoff_role =
        (command == StagingCommand::Handoff).then(|| HandoffRole::of_thread(thread_id));
    let ask = context
        .client()
        .send_message_with_markup(
            incoming.chat_id,
            &ask_text(command, None),
            reply_to_message_id,
            topic_id,
            &staging_keyboard(command, handoff_role),
        )
        .await?;

    let session = StagingSession {
        command,
        command_message_id: incoming.message_id,
        model_override: None,
        handoff_role,
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
    context: &Arc<BotContext>,
    queues: &ChatQueueMap,
    incoming: &IncomingMessage,
    topic_id: Option<i64>,
    thread_id: &str,
) -> Result<()> {
    // Track the input message and take the previous suggestion (regenerate).
    let (command, handoff_role, previous_suggestion) = {
        let mut map = context.staging_map().lock().expect("staging lock poisoned");
        let Some(session) = map.get_mut(thread_id) else {
            return Ok(());
        };
        session.user_message_ids.push(incoming.message_id);
        session.suggestion_text = None;
        (
            session.command,
            session.handoff_role,
            session.suggestion_message_id.take(),
        )
    };
    if let Some(message_id) = previous_suggestion
        && let Err(err) = context
            .client()
            .delete_message(incoming.chat_id, message_id)
            .await
    {
        tracing::warn!(message_id, err = %format!("{err:#}"), "Failed to delete previous staging suggestion");
    }

    let Some(input) = effective_input_text(incoming) else {
        let hint_text = match command {
            StagingCommand::Handoff => {
                "Send text or a voice note to use as the handoff message, or /cancel to abort."
            }
            StagingCommand::Btw => {
                "Send text or a voice note with your side question, or /cancel to abort."
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
                &staging_keyboard(command, handoff_role),
            )
            .await?;
        track_bot_message(context, thread_id, hint.id);
        return Ok(());
    };

    // btw needs no LLM call, so there is nothing to preview or accept: open the
    // side topic straight away.
    if command == StagingCommand::Btw {
        return start_btw_topic(context, queues, incoming, topic_id, thread_id, input).await;
    }

    // Handoff generates a context block, but accepting it only ever opens a new
    // topic, so it does not gate on confirmation either.
    if command == StagingCommand::Handoff {
        return start_handoff_topic(context, queues, incoming, topic_id, thread_id, input).await;
    }

    let generating = context
        .client()
        .send_message_with_markup(
            incoming.chat_id,
            "⏳ Building prompt…",
            Some(incoming.message_id),
            topic_id,
            &InlineKeyboardMarkup {
                inline_keyboard: vec![],
            },
        )
        .await?;

    let result = run_prompt_builder_generation(context, incoming, thread_id, input).await;
    present_generation_result(context, incoming.chat_id, thread_id, generating.id, result).await;
    Ok(())
}

/// Edits the "building…" message into the prompt-builder preview (with Accept /
/// Discard buttons) or an error, updating the session accordingly.
async fn present_generation_result(
    context: &BotContext,
    chat_id: i64,
    thread_id: &str,
    generating_message_id: i64,
    result: Result<String>,
) {
    match result {
        Ok(suggestion) => {
            let preview = suggestion_preview(&suggestion);
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
                tracing::warn!(err = %format!("{err:#}"), "Failed to show staging suggestion");
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
                "⚠️ Generation failed:\n<pre>{}</pre>\nSend another message to retry, or /cancel.",
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
    let config = context.config_for_chat(incoming.chat_id);
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
    let config = context.config_for_chat(incoming.chat_id);
    let resolved_root = context.root_for_chat(incoming.chat_id);
    let root = thread_persistence::read_thread_root_path(thread_id)?
        .map_or(resolved_root.root, PathBuf::from);
    generate_prompt_builder(
        input,
        Some(config.prompt_builder_model.clone()),
        &root,
        None,
        Some(thread_id.to_string()),
    )
    .await
}

/// Handles `stg:{action}` callbacks: `a` accepts the staged suggestion, `m`
/// opens the model picker for the topic the command will create, `r` switches
/// the role a `/handoff` topic continues as, `d` discards the session and
/// deletes its messages.
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
    let thread_id = session_thread_id(chat_id, topic_id);

    // The model picker and the role switch keep the session alive, so they are
    // handled before the session is taken below.
    if data == "m" {
        open_model_picker(context, client, callback, chat_id, topic_id, &thread_id).await;
        return;
    }
    if data == "r" {
        toggle_handoff_role(context, client, callback, chat_id, &thread_id).await;
        return;
    }

    let session = {
        let mut map = context.staging_map().lock().expect("staging lock poisoned");
        map.remove(&thread_id)
    };
    let Some(session) = session else {
        // No in-memory session (common after a bot restart, since staging state
        // is memory-only). Make the stale buttons self-clearing so they can't
        // get stuck: Discard deletes its own message; Accept just drops the
        // buttons and tells the user to re-run.
        if data == "d" {
            let _ = client.delete_message(chat_id, message.id).await;
            let _ = client
                .answer_callback_query(&callback.id, Some("Discarded ✓"))
                .await;
        } else {
            let _ = client
                .edit_message_text(
                    chat_id,
                    message.id,
                    "⚠️ Session expired — re-run the command.",
                    None,
                )
                .await;
            let _ = client
                .answer_callback_query(
                    &callback.id,
                    Some("Session expired after restart — re-run the command"),
                )
                .await;
        }
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
            StagingCommand::PromptBuilder => {
                accept_prompt_builder(context, queues, client, callback, &thread_id, session).await;
            }
            // Only prompt-builder stages an Accept button, because accepting it
            // runs a prompt in this thread. Telegram can still redeliver a stale
            // callback, so answer politely.
            StagingCommand::Handoff | StagingCommand::Btw => {
                let _ = client
                    .answer_callback_query(&callback.id, Some("Nothing to accept"))
                    .await;
            }
        },
        _ => {
            let _ = client.answer_callback_query(&callback.id, None).await;
            tracing::warn!(?data, "Unknown staging callback");
        }
    }
}

/// Opens the side topic for `/btw` immediately — no preview, no Accept tap.
///
/// The seed is the user's question plus a pointer at the current thread, which
/// the new topic's agent resolves with `Read_Thread`. The question itself is
/// posted into the new topic as its visible record. On failure the staging
/// session is kept so the next message retries.
async fn start_btw_topic(
    context: &Arc<BotContext>,
    queues: &ChatQueueMap,
    incoming: &IncomingMessage,
    topic_id: Option<i64>,
    thread_id: &str,
    input: &str,
) -> Result<()> {
    let seed = build_side_thread_seed(thread_id, input);

    // btw completes in one step, so the session is done either way.
    let session = {
        let mut map = context.staging_map().lock().expect("staging lock poisoned");
        map.remove(thread_id)
    };
    let model_override = session
        .as_ref()
        .and_then(|session| session.model_override.clone());

    match seed_new_topic(
        context,
        queues,
        incoming.chat_id,
        incoming.user_id,
        thread_id,
        "Btw",
        seed,
        Some(btw_record(input)),
        None,
        model_override,
    )
    .await
    {
        Ok(()) => {
            if let Some(session) = session {
                cleanup_session_messages(context, incoming.chat_id, &session).await;
            }
        }
        Err(err) => {
            if let Some(session) = session {
                let mut map = context.staging_map().lock().expect("staging lock poisoned");
                map.insert(thread_id.to_string(), session);
            }
            let text = format!(
                "⚠️ Couldn't open the side topic:\n<pre>{}</pre>\nSend another message to retry, or /cancel.",
                escape_html(&format!("{err:#}"))
            );
            let sent = context
                .client()
                .send_message_with_markup(
                    incoming.chat_id,
                    &text,
                    Some(incoming.message_id),
                    topic_id,
                    &staging_keyboard(StagingCommand::Btw, None),
                )
                .await?;
            track_bot_message(context, thread_id, sent.id);
        }
    }
    Ok(())
}

/// Creates a forum topic seeded with `seed_text` and dispatches that text as
/// the new topic's first message. `record`, when present, is posted into the
/// new topic before the turn starts, since the seed is synthetic and never
/// shows up in the chat.
///
/// The new thread is pre-created so its meta records the `handoff_from` lineage
/// and a pending auto-title before the first turn opens it, and it inherits the
/// source thread's model/thinking overrides so it continues with the same
/// effective model and thinking level. `model_override` (picked with the card's
/// `🎛 Model` button) replaces that inheritance for this new thread only.
/// Shared by `/handoff` and `/btw`.
///
/// `profile` is the persistent profile the new thread starts with: `/handoff`
/// passes the reserved `orchestrator` profile when the topic continues as an
/// orchestrator (the source's role by default, or the card's `Continue as:`
/// pick), and `None` for an ordinary agent. The source thread is left
/// completely alone — it keeps its own workers and stays usable, so both
/// conversations can run on. `/btw` always passes `None`, since a side
/// question is an ordinary read-only thread.
#[allow(clippy::too_many_arguments)]
async fn seed_new_topic(
    context: &Arc<BotContext>,
    queues: &ChatQueueMap,
    chat_id: i64,
    from_user_id: i64,
    source_thread_id: &str,
    topic_prefix: &str,
    seed_text: String,
    record: Option<String>,
    profile: Option<&str>,
    model_override: Option<String>,
) -> Result<()> {
    let topic_name = chrono::Utc::now()
        .format(&format!("{topic_prefix} %Y-%m-%d %H:%M"))
        .to_string();
    let topic_name = if profile == Some(zdx_engine::subagents::ORCHESTRATOR_SUBAGENT_NAME) {
        orchestrator_topic_name(&topic_name)
    } else {
        topic_name
    };
    let new_topic_id = context
        .client()
        .create_forum_topic(chat_id, &topic_name)
        .await?;

    // Without a pick, a model is pinned only when the source pinned one; an
    // unpinned topic resolves its role's default on every turn, which is the
    // same model `new_topic_model` reports.
    let new_model = model_override.or_else(|| {
        let start = new_topic_model(context, chat_id, source_thread_id, profile);
        start.source_pinned.then_some(start.config.model)
    });
    let new_thread_id = thread_id_for_chat(chat_id, Some(new_topic_id));
    let created = record_seeded_thread(&new_thread_id, source_thread_id, profile, new_model);
    if let Err(err) = created {
        // Fail closed when a role was requested: running the default coding
        // agent where an orchestrator was asked for is exactly what the
        // handoff is meant to avoid, so surface the failure and let the user
        // retry instead.
        if profile.is_some() {
            return Err(err.context("record the profile on the handoff thread"));
        }
        tracing::warn!(chat_id, new_topic_id, err = %format!("{err:#}"), "Failed to record lineage on new thread");
    }

    if let Err(err) = post_thread_header(context, chat_id, new_topic_id, &new_thread_id).await {
        tracing::warn!(
            chat_id,
            topic_id = new_topic_id,
            thread_id = %new_thread_id,
            err = %format!("{err:#}"),
            "Created seeded topic but failed to post thread header"
        );
    }

    if let Some(record) = record
        && let Err(err) = context
            .client()
            .send_message(chat_id, &record, None, Some(new_topic_id))
            .await
    {
        tracing::warn!(
            chat_id,
            topic_id = new_topic_id,
            err = %format!("{err:#}"),
            "Created seeded topic but failed to post the context record"
        );
    }

    let synthetic: crate::telegram::Message = serde_json::from_value(json!({
        "message_id": new_topic_id,
        "chat": { "id": chat_id, "type": "supergroup", "is_forum": true },
        "from": { "id": from_user_id, "is_bot": false },
        "text": seed_text,
        "message_thread_id": new_topic_id,
    }))?;
    dispatch_message(queues, context, synthetic).await;
    Ok(())
}

/// Pre-creates a seeded topic's thread: `handoff_from` lineage, the persistent
/// `profile` it runs as, its starting model, and a pending auto-title. Only the
/// new thread is written; the source thread is never touched.
fn record_seeded_thread(
    new_thread_id: &str,
    source_thread_id: &str,
    profile: Option<&str>,
    model: Option<String>,
) -> Result<()> {
    let mut thread = thread_persistence::Thread::with_id(new_thread_id.to_string())?;
    thread.set_handoff_from(Some(source_thread_id.to_string()));
    if let Some(profile) = profile {
        thread.set_persistent_profile(profile)?;
    }
    if let Some(model) = model {
        thread.set_model_override(Some(model))?;
    }
    thread.set_pending_topic_title(true)
}

/// The model a staged topic starts with when nothing is picked on its card.
struct NewTopicModel {
    config: Config,
    /// Whether the source thread pinned a model or thinking level (`/model`).
    source_pinned: bool,
}

/// Resolves the starting model of a topic opened from `source_thread_id` as
/// `profile`, before any pick. One rule feeds the card's model picker and the
/// thread that is created, so the picker never shows a different model.
fn new_topic_model(
    context: &BotContext,
    chat_id: i64,
    source_thread_id: &str,
    profile: Option<&str>,
) -> NewTopicModel {
    let source_profile = thread_persistence::read_persistent_profile(source_thread_id)
        .ok()
        .flatten();
    let model = thread_persistence::read_thread_model_override(source_thread_id)
        .ok()
        .flatten();
    let thinking = thread_persistence::read_thread_thinking_override(source_thread_id)
        .ok()
        .flatten();
    layer_new_topic_model(
        context.config_for_chat(chat_id),
        HandoffRole::of_profile(source_profile.as_deref()),
        HandoffRole::of_profile(profile),
        model.as_deref(),
        thinking,
    )
}

/// A source that pinned its model hands on its own effective model, resolved
/// in the source's role. An unpinned source hands on nothing, so the new topic
/// starts on the default for its *own* role, which differs from the source's
/// model when the handoff changes the role.
fn layer_new_topic_model(
    mut config: Config,
    source_role: HandoffRole,
    new_role: HandoffRole,
    model_override: Option<&str>,
    thinking_override: Option<ThinkingLevel>,
) -> NewTopicModel {
    let source_pinned = model_override.is_some() || thinking_override.is_some();
    let role = if source_pinned { source_role } else { new_role };
    if role == HandoffRole::Orchestrator {
        zdx_engine::subagents::apply_orchestrator_override(&mut config);
    }
    config.apply_thread_model_override(model_override, thinking_override);
    NewTopicModel {
        config,
        source_pinned,
    }
}

/// The config a live staged command's new topic starts with before any pick:
/// what its model picker shows as the starting model and resolves a pick
/// against. Follows the `/handoff` card's current `Continue as:` role.
pub(crate) fn staged_topic_config(
    context: &BotContext,
    chat_id: i64,
    topic_id: Option<i64>,
) -> Config {
    let thread_id = session_thread_id(chat_id, topic_id);
    let profile = {
        let map = context.staging_map().lock().expect("staging lock poisoned");
        map.get(&thread_id)
            .and_then(|session| session.handoff_role)
            .and_then(HandoffRole::profile)
    };
    new_topic_model(context, chat_id, &thread_id, profile).config
}

/// Generates the handoff context and opens the new topic immediately — no
/// preview, no Accept tap.
///
/// The generated context is posted into the new topic as its visible record:
/// the seed itself is dispatched synthetically and never appears in the chat.
/// On failure the staging session is kept so the next message retries.
async fn start_handoff_topic(
    context: &Arc<BotContext>,
    queues: &ChatQueueMap,
    incoming: &IncomingMessage,
    topic_id: Option<i64>,
    thread_id: &str,
    input: &str,
) -> Result<()> {
    let generating = context
        .client()
        .send_message_with_markup(
            incoming.chat_id,
            "⏳ Generating handoff…",
            Some(incoming.message_id),
            topic_id,
            &InlineKeyboardMarkup {
                inline_keyboard: vec![],
            },
        )
        .await?;
    track_bot_message(context, thread_id, generating.id);

    let (model_override, role) = {
        let map = context.staging_map().lock().expect("staging lock poisoned");
        let session = map.get(thread_id);
        (
            session.and_then(|session| session.model_override.clone()),
            session.and_then(|session| session.handoff_role),
        )
    };
    let role = role.unwrap_or_else(|| HandoffRole::of_thread(thread_id));

    let seeded = match run_handoff_generation(context, incoming, thread_id, input).await {
        Ok(suggestion) => {
            let record = handoff_record(&suggestion);
            seed_new_topic(
                context,
                queues,
                incoming.chat_id,
                incoming.user_id,
                thread_id,
                "Handoff",
                suggestion,
                Some(record),
                role.profile(),
                model_override,
            )
            .await
        }
        Err(err) => Err(err),
    };

    match seeded {
        Ok(()) => {
            let session = {
                let mut map = context.staging_map().lock().expect("staging lock poisoned");
                map.remove(thread_id)
            };
            if let Some(session) = session {
                cleanup_session_messages(context, incoming.chat_id, &session).await;
            }
        }
        Err(err) => {
            let role = {
                let map = context.staging_map().lock().expect("staging lock poisoned");
                map.get(thread_id)
                    .and_then(|session| session.handoff_role)
                    .unwrap_or(role)
            };
            let text = format!(
                "⚠️ Handoff failed:\n<pre>{}</pre>\nSend another message to retry, or /cancel.",
                escape_html(&format!("{err:#}"))
            );
            if let Err(edit_err) = context
                .client()
                .edit_message_text(
                    incoming.chat_id,
                    generating.id,
                    &text,
                    Some(&staging_keyboard(StagingCommand::Handoff, Some(role))),
                )
                .await
            {
                tracing::warn!(%edit_err, "Failed to show handoff error");
            }
        }
    }
    Ok(())
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
            tracing::warn!(message_id, err = %format!("{err:#}"), "Failed to delete bot staging message");
        }
    }
    for &message_id in &session.user_message_ids {
        if let Err(err) = context.client().delete_message(chat_id, message_id).await {
            tracing::debug!(message_id, err = %format!("{err:#}"), "Could not delete user staging message (needs can_delete_messages)");
        }
    }
}

/// The prompt-builder preview: the only staged suggestion that still gates on
/// Accept, because accepting it runs the prompt in the current thread.
fn suggestion_preview(suggestion: &str) -> String {
    let html = to_telegram_html(suggestion);
    let truncated = truncate_telegram_html(&html, SUGGESTION_PREVIEW_MAX_CHARS);
    format!(
        "🛠 <b>Prompt preview</b>\n\n{truncated}\n\n<i>Accept runs this prompt here. Send another message to regenerate.</i>"
    )
}

/// The handoff context posted into the new topic. The seed reaches the agent
/// synthetically, so without this the context the turn started from would never
/// be visible anywhere.
fn handoff_record(suggestion: &str) -> String {
    let quoted = suggestion
        .lines()
        .map(|line| format!("> {line}"))
        .collect::<Vec<_>>()
        .join("\n");
    let html = to_telegram_html(&quoted);
    let truncated = truncate_telegram_html(&html, SUGGESTION_PREVIEW_MAX_CHARS);
    format!("🔀 <b>Handoff context</b>\n{truncated}")
}

/// The user's `/btw` question posted into the new topic. The seed reaches the
/// agent synthetically, so without this the prompt never shows up in the chat.
fn btw_record(question: &str) -> String {
    let quoted = format!("<blockquote>{}</blockquote>", escape_html(question));
    let truncated = truncate_telegram_html(&quoted, SUGGESTION_PREVIEW_MAX_CHARS);
    format!("💬 <b>Side question</b>\n{truncated}")
}

fn accept_discard_keyboard() -> InlineKeyboardMarkup {
    InlineKeyboardMarkup {
        inline_keyboard: vec![vec![
            InlineKeyboardButton::callback("✅ Accept", "stg:a"),
            InlineKeyboardButton::callback("🗑 Discard", "stg:d"),
        ]],
    }
}

/// The staged command's own card. Commands that open a new topic also offer the
/// model that topic will start with; the others only run in the current thread,
/// where `/model` already applies. `/handoff` (the only command with a
/// `handoff_role`) also offers the role its topic continues as, next to Model.
fn staging_keyboard(
    command: StagingCommand,
    handoff_role: Option<HandoffRole>,
) -> InlineKeyboardMarkup {
    let discard = InlineKeyboardButton::callback("🗑 Discard", "stg:d");
    if !command.opens_new_topic() {
        return InlineKeyboardMarkup {
            inline_keyboard: vec![vec![discard]],
        };
    }
    let model = InlineKeyboardButton::callback("🎛 Model", "stg:m");
    let inline_keyboard = match handoff_role {
        Some(role) => vec![
            vec![
                model,
                InlineKeyboardButton::callback(format!("Continue as: {}", role.label()), "stg:r"),
            ],
            vec![discard],
        ],
        None => vec![vec![model, discard]],
    };
    InlineKeyboardMarkup { inline_keyboard }
}

/// The prompt asking for the command's input, including the picked model when
/// the command opens a new topic and one was chosen.
fn ask_text(command: StagingCommand, model_override: Option<&str>) -> String {
    let base = match command {
        StagingCommand::Handoff => {
            "🔀 <b>Handoff</b>\nSend the message (text or voice) to start the new topic with — I'll generate the context and open it straight away. Send /cancel to abort."
        }
        StagingCommand::Btw => {
            "💬 <b>Side question</b>\nSend your question (text or voice) — I'll answer it in a new topic that reads this thread, leaving this one untouched. Send /cancel to abort."
        }
        StagingCommand::PromptBuilder => {
            "🛠 <b>Prompt builder</b>\nSend your intent (text or voice) — I'll draft a prompt you can accept to run here, or discard. Send /cancel to abort."
        }
    };
    match model_override {
        Some(model) if command.opens_new_topic() => format!(
            "{base}\n\nNew topic model: <code>{}</code>",
            escape_html(model)
        ),
        _ => base.to_string(),
    }
}

/// Staging state is keyed by the same effective thread id message intake uses,
/// so a resumed topic finds its session.
fn session_thread_id(chat_id: i64, topic_id: Option<i64>) -> String {
    resolve_effective_thread_id(&thread_id_for_chat(chat_id, topic_id))
}

/// Opens the model picker on the staged command's own card (`🎛 Model`).
async fn open_model_picker(
    context: &BotContext,
    client: &TelegramClient,
    callback: &CallbackQuery,
    chat_id: i64,
    topic_id: Option<i64>,
    thread_id: &str,
) {
    let Some(message) = callback.message.as_ref() else {
        return;
    };
    let live = {
        let map = context.staging_map().lock().expect("staging lock poisoned");
        map.get(thread_id)
            .is_some_and(|session| session.command.opens_new_topic())
    };
    if !live {
        let _ = client
            .answer_callback_query(&callback.id, Some("Session expired — re-run the command"))
            .await;
        return;
    }

    let keyboard = build_provider_keyboard(context, chat_id, ModelPickerScope::Staging);
    let header = crate::model_picker_header(context, chat_id, topic_id, ModelPickerScope::Staging);
    if let Err(err) = client
        .edit_message_text(chat_id, message.id, &header, Some(&keyboard))
        .await
    {
        tracing::warn!(err = %format!("{err:#}"), "Failed to open staging model picker");
    }
    let _ = client.answer_callback_query(&callback.id, None).await;
}

/// Switches the role a live `/handoff` topic continues as (`Continue as:`) and
/// updates the tapped card's buttons in place, keeping its text.
async fn toggle_handoff_role(
    context: &BotContext,
    client: &TelegramClient,
    callback: &CallbackQuery,
    chat_id: i64,
    thread_id: &str,
) {
    let Some(message) = callback.message.as_ref() else {
        return;
    };
    let role = {
        let mut map = context.staging_map().lock().expect("staging lock poisoned");
        map.get_mut(thread_id).and_then(|session| {
            let role = session.handoff_role?.toggled();
            session.handoff_role = Some(role);
            Some(role)
        })
    };
    let Some(role) = role else {
        let _ = client
            .answer_callback_query(&callback.id, Some("Session expired — re-run the command"))
            .await;
        return;
    };

    if let Err(err) = client
        .edit_message_reply_markup(
            chat_id,
            message.id,
            &staging_keyboard(StagingCommand::Handoff, Some(role)),
        )
        .await
    {
        tracing::warn!(err = %format!("{err:#}"), "Failed to update the handoff role button");
    }
    let notice = format!("The new topic continues as {}", role.label());
    let _ = client
        .answer_callback_query(&callback.id, Some(&notice))
        .await;
}

/// The model a live staged command will give its new topic, if one was picked.
pub(crate) fn session_model_override(
    context: &BotContext,
    chat_id: i64,
    topic_id: Option<i64>,
) -> Option<String> {
    let thread_id = session_thread_id(chat_id, topic_id);
    let map = context.staging_map().lock().expect("staging lock poisoned");
    map.get(&thread_id)
        .and_then(|session| session.model_override.clone())
}

/// Closes the staging model picker and restores the command's card in place.
/// `model` is the fully resolved pick, or `None` when the picker was cancelled.
/// Returns `false` when no staging session is live any more.
pub(crate) async fn finish_model_pick(
    context: &BotContext,
    client: &TelegramClient,
    chat_id: i64,
    topic_id: Option<i64>,
    message_id: i64,
    model: Option<String>,
) -> bool {
    let thread_id = session_thread_id(chat_id, topic_id);
    let card = {
        let mut map = context.staging_map().lock().expect("staging lock poisoned");
        map.get_mut(&thread_id).map(|session| {
            if model.is_some() {
                session.model_override = model;
            }
            (
                session.command,
                session.handoff_role,
                session.model_override.clone(),
            )
        })
    };
    let Some((command, handoff_role, model_override)) = card else {
        return false;
    };
    if let Err(err) = client
        .edit_message_text(
            chat_id,
            message_id,
            &ask_text(command, model_override.as_deref()),
            Some(&staging_keyboard(command, handoff_role)),
        )
        .await
    {
        tracing::warn!(err = %format!("{err:#}"), "Failed to restore staging card after model pick");
    }
    true
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::time::{Duration, Instant};

    use zdx_engine::core::thread_persistence;

    use super::{
        HandoffRole, STAGING_TTL, SessionInput, StagingCommand, StagingSession, ask_text,
        btw_awaits_input, btw_record, classify_session_input, handoff_record,
        layer_new_topic_model, record_seeded_thread, staging_keyboard, suggestion_preview,
    };

    fn session_created_at(created_at: Instant) -> StagingSession {
        session_for(StagingCommand::Handoff, 1, created_at)
    }

    fn session_for(
        command: StagingCommand,
        command_message_id: i64,
        created_at: Instant,
    ) -> StagingSession {
        StagingSession {
            command,
            command_message_id,
            model_override: None,
            handoff_role: None,
            suggestion_text: None,
            suggestion_message_id: None,
            bot_message_ids: vec![],
            user_message_ids: vec![],
            created_at,
        }
    }

    const THREAD: &str = "telegram--100-topic-3";

    fn live_sessions(command: StagingCommand) -> HashMap<String, StagingSession> {
        HashMap::from([(THREAD.to_string(), session_for(command, 50, Instant::now()))])
    }

    /// Regression: a worker update arriving while `/handoff` (or any staged
    /// command) waits for the user's input must not be taken as that input.
    /// Synthetic ids count down from `i64::MAX`, so the id check alone passes.
    #[test]
    fn synthetic_prompt_is_never_staged_input() {
        for command in [
            StagingCommand::Handoff,
            StagingCommand::Btw,
            StagingCommand::PromptBuilder,
        ] {
            let mut sessions = live_sessions(command);
            let input = classify_session_input(&mut sessions, THREAD, i64::MAX - 1, true);
            assert!(matches!(input, SessionInput::NotInput));
            assert!(sessions.contains_key(THREAD), "session keeps waiting");

            let input = classify_session_input(&mut sessions, THREAD, 51, false);
            assert!(matches!(input, SessionInput::Input));
        }
    }

    #[test]
    fn messages_older_than_the_command_are_not_staged_input() {
        let mut sessions = live_sessions(StagingCommand::Handoff);
        let input = classify_session_input(&mut sessions, THREAD, 49, false);
        assert!(matches!(input, SessionInput::NotInput));
    }

    #[test]
    fn expired_session_is_removed_and_not_input() {
        let old = Instant::now()
            .checked_sub(STAGING_TTL + Duration::from_secs(1))
            .expect("clock supports back-dating");
        let mut sessions = HashMap::from([(
            THREAD.to_string(),
            session_for(StagingCommand::Btw, 50, old),
        )]);
        let input = classify_session_input(&mut sessions, THREAD, 51, false);
        assert!(matches!(input, SessionInput::Expired(_)));
        assert!(sessions.is_empty());
    }

    /// A worker update must stay in the topic queue, not skip it as the
    /// question a live `/btw` session is waiting for.
    #[test]
    fn synthetic_prompt_does_not_route_as_btw_input() {
        let sessions = live_sessions(StagingCommand::Btw);
        assert!(btw_awaits_input(&sessions, THREAD, false));
        assert!(!btw_awaits_input(&sessions, THREAD, true));
        assert!(!btw_awaits_input(
            &live_sessions(StagingCommand::Handoff),
            THREAD,
            false
        ));
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
    fn suggestion_preview_converts_markdown_and_keeps_actions_hint() {
        let preview = suggestion_preview("**Fix** `<script>` & stuff\n\n> Check first");
        assert!(preview.contains("Prompt preview"));
        assert!(preview.contains("<b>Fix</b> <code>&lt;script&gt;</code> &amp; stuff"));
        assert!(preview.contains("<blockquote>Check first</blockquote>"));
        assert!(!preview.contains("<blockquote><b>Fix"));
        assert!(preview.contains("runs this prompt here"));
        assert!(preview.contains("regenerate"));
    }

    /// The handoff seed is dispatched synthetically, so this record is the only
    /// place the generated context is ever visible. It is quoted like the btw
    /// question and carries no Accept hint.
    #[test]
    fn handoff_record_quotes_markdown_and_offers_no_action() {
        let record = handoff_record("## Goal\n\n**Fix** `<script>` & stuff\n\n- one\n- two");
        assert!(record.starts_with("🔀 <b>Handoff context</b>\n<blockquote>"));
        assert!(record.contains("<b>Goal</b>"));
        assert!(record.contains("<b>Fix</b> &lt;script&gt; &amp; stuff"));
        assert!(record.contains("- one\n- two</blockquote>"));
        assert_eq!(record.matches("<blockquote>").count(), 1);
        assert!(!record.contains("Accept"));
        assert!(!record.contains("regenerate"));
    }

    /// The btw seed is synthetic too, so the user's question is echoed into
    /// the new topic as an escaped quote.
    #[test]
    fn btw_record_quotes_the_question_escaped() {
        let record = btw_record("what does <T> & `x` mean?");
        assert!(record.contains("Side question"));
        assert!(record.contains("<blockquote>what does &lt;T&gt; &amp; `x` mean?</blockquote>"));
    }

    /// Handoff and btw both seed a fresh topic, so both require running inside
    /// one; prompt-builder runs in place and works anywhere.
    #[test]
    fn only_topic_seeding_commands_require_a_forum_topic() {
        assert!(StagingCommand::Handoff.opens_new_topic());
        assert!(StagingCommand::Btw.opens_new_topic());
        assert!(!StagingCommand::PromptBuilder.opens_new_topic());
    }

    /// Only the commands that open a new topic offer a model for it; the others
    /// run in the current thread, where `/model` already applies.
    #[test]
    fn model_button_is_offered_only_by_topic_seeding_commands() {
        let labels = |command, role| {
            staging_keyboard(command, role)
                .inline_keyboard
                .into_iter()
                .flatten()
                .map(|button| button.text)
                .collect::<Vec<_>>()
        };
        assert!(
            labels(StagingCommand::Btw, None)
                .iter()
                .any(|text| text.contains("Model"))
        );
        assert!(
            labels(StagingCommand::Handoff, Some(HandoffRole::Agent))
                .iter()
                .any(|text| text.contains("Model"))
        );
        assert_eq!(
            labels(StagingCommand::PromptBuilder, None),
            vec!["🗑 Discard".to_string()]
        );
    }

    /// `/handoff` offers the role its topic continues as right beside Model,
    /// labelled with the current choice; `/btw` never offers a role.
    #[test]
    fn handoff_card_offers_the_role_beside_model() {
        for (role, label) in [
            (HandoffRole::Agent, "Continue as: Agent"),
            (HandoffRole::Orchestrator, "Continue as: Orchestrator"),
        ] {
            let keyboard = staging_keyboard(StagingCommand::Handoff, Some(role));
            let first_row = &keyboard.inline_keyboard[0];
            assert_eq!(first_row[0].callback_data.as_deref(), Some("stg:m"));
            assert_eq!(first_row[1].text, label);
            assert_eq!(first_row[1].callback_data.as_deref(), Some("stg:r"));
        }
        let btw = staging_keyboard(StagingCommand::Btw, None);
        assert!(
            btw.inline_keyboard
                .iter()
                .flatten()
                .all(|button| button.callback_data.as_deref() != Some("stg:r"))
        );
    }

    /// The role defaults to the source thread's, toggles between the two, and
    /// only Orchestrator records a persistent profile on the new thread.
    #[test]
    fn handoff_role_defaults_to_the_source_and_maps_to_the_profile() {
        let _home = zdx_engine::test_support::temp_zdx_home();
        let orchestrator = format!("orchestrator-{}", uuid::Uuid::new_v4());
        let mut thread = thread_persistence::Thread::with_id(orchestrator.clone()).unwrap();
        thread
            .set_persistent_profile(zdx_engine::subagents::ORCHESTRATOR_SUBAGENT_NAME)
            .unwrap();
        let ordinary = format!("ordinary-{}", uuid::Uuid::new_v4());
        thread_persistence::Thread::with_id(ordinary.clone())
            .unwrap()
            .set_root_path(std::path::Path::new("/tmp"))
            .unwrap();

        assert_eq!(
            HandoffRole::of_thread(&orchestrator),
            HandoffRole::Orchestrator
        );
        assert_eq!(HandoffRole::of_thread(&ordinary), HandoffRole::Agent);
        assert_eq!(HandoffRole::Agent.toggled(), HandoffRole::Orchestrator);
        assert_eq!(HandoffRole::Orchestrator.toggled(), HandoffRole::Agent);
        assert_eq!(HandoffRole::Agent.profile(), None);
        assert_eq!(
            HandoffRole::Orchestrator.profile(),
            Some(zdx_engine::subagents::ORCHESTRATOR_SUBAGENT_NAME)
        );
    }

    /// Continuing an ordinary thread as an orchestrator writes the reserved
    /// profile and lineage onto the new thread only; the source stays as it was.
    #[test]
    fn seeded_thread_records_the_role_without_touching_the_source() {
        let _home = zdx_engine::test_support::temp_zdx_home();
        let source = format!("source-{}", uuid::Uuid::new_v4());
        thread_persistence::Thread::with_id(source.clone())
            .unwrap()
            .set_root_path(std::path::Path::new("/tmp"))
            .unwrap();
        let successor = format!("successor-{}", uuid::Uuid::new_v4());
        let agent = format!("agent-{}", uuid::Uuid::new_v4());

        record_seeded_thread(
            &successor,
            &source,
            HandoffRole::Orchestrator.profile(),
            None,
        )
        .unwrap();
        record_seeded_thread(&agent, &source, HandoffRole::Agent.profile(), None).unwrap();

        assert_eq!(
            thread_persistence::read_persistent_profile(&successor)
                .unwrap()
                .as_deref(),
            Some(zdx_engine::subagents::ORCHESTRATOR_SUBAGENT_NAME)
        );
        assert_eq!(
            thread_persistence::read_thread_summary(&successor)
                .unwrap()
                .and_then(|summary| summary.handoff_from),
            Some(source.clone())
        );
        assert_eq!(
            thread_persistence::read_persistent_profile(&agent).unwrap(),
            None
        );
        assert_eq!(
            thread_persistence::read_persistent_profile(&source).unwrap(),
            None
        );
    }

    /// A picked model is shown on the card it was picked from, so the user can
    /// see what the new topic will start with before sending the input.
    #[test]
    fn picked_model_shows_on_the_card_of_topic_seeding_commands() {
        let text = ask_text(StagingCommand::Btw, Some("openai:gpt-6@high"));
        assert!(text.contains("New topic model: <code>openai:gpt-6@high</code>"));
        assert!(!ask_text(StagingCommand::Btw, None).contains("New topic model"));
        assert!(
            !ask_text(StagingCommand::PromptBuilder, Some("openai:gpt-6@high"))
                .contains("New topic model")
        );
    }

    /// The starting model shown by the staged picker follows the new role when
    /// the source pinned nothing, and stays the source's pinned model when it did.
    #[test]
    fn new_topic_model_follows_the_new_role_unless_the_source_pinned_one() {
        use zdx_engine::config::{Config, SubagentOverride, ThinkingLevel};

        let mut base = Config {
            model: "openai:agent-model@high".to_string(),
            thinking_level: ThinkingLevel::High,
            ..Default::default()
        };
        base.subagents.overrides.insert(
            zdx_engine::subagents::ORCHESTRATOR_SUBAGENT_NAME.to_string(),
            SubagentOverride {
                model: Some("openai:orchestrator-model@low".to_string()),
                thinking_level: Some(ThinkingLevel::Low),
            },
        );
        let start = |source, new, model: Option<&str>, thinking| {
            layer_new_topic_model(base.clone(), source, new, model, thinking)
        };

        let to_orchestrator = start(HandoffRole::Agent, HandoffRole::Orchestrator, None, None);
        assert_eq!(
            to_orchestrator.config.model,
            "openai:orchestrator-model@low"
        );
        assert!(!to_orchestrator.source_pinned);

        let to_agent = start(HandoffRole::Orchestrator, HandoffRole::Agent, None, None);
        assert_eq!(to_agent.config.model, "openai:agent-model@high");
        assert!(!to_agent.source_pinned);

        let pinned = start(
            HandoffRole::Agent,
            HandoffRole::Orchestrator,
            Some("openai:pinned-model@medium"),
            None,
        );
        assert_eq!(pinned.config.model, "openai:pinned-model@medium");
        assert!(pinned.source_pinned);

        let thinking_only = start(
            HandoffRole::Orchestrator,
            HandoffRole::Agent,
            None,
            Some(ThinkingLevel::Medium),
        );
        assert_eq!(
            thinking_only.config.model,
            "openai:orchestrator-model@medium"
        );
        assert!(thinking_only.source_pinned);
    }

    #[test]
    fn long_suggestions_are_truncated() {
        let long = "x".repeat(10_000);
        assert!(suggestion_preview(&long).chars().count() < 3_500);
        assert!(suggestion_preview(&long).contains('…'));
        assert!(handoff_record(&long).chars().count() < 3_500);
        assert!(handoff_record(&long).contains('…'));
    }
}
