use std::collections::HashSet;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use tokio_util::sync::CancellationToken;
use zdx_core::core::events::AgentEvent;
use zdx_core::core::thread_persistence::{self, ThreadEvent};
use zdx_core::core::worktree;

use crate::bot::context::BotContext;
use crate::commands::{BotCommand, parse_command};
use crate::ingest::AllowlistConfig;
use crate::telegram::{InlineKeyboardButton, InlineKeyboardMarkup, Message, ReplyParameters};
use crate::{agent, ingest};

/// Groups the reply-targeting fields that travel together through the turn pipeline.
struct ReplyContext {
    reply_to_message_id: Option<i64>,
    topic_id: Option<i64>,
    cross_topic_reply_parameters: Option<ReplyParameters>,
}

/// Minimum interval between Telegram status message edits (avoid rate limiting).
const STATUS_DEBOUNCE: std::time::Duration = std::time::Duration::from_secs(3);
const MEDIA_BLOCK_OPEN: &str = "<medias>";
const MEDIA_BLOCK_CLOSE: &str = "</medias>";
const MEDIA_TAG_OPEN: &str = "<media";
const MEDIA_TAG_CLOSE: &str = "</media>";

///
/// # Errors
/// Returns an error if the operation fails.
pub(crate) async fn handle_message(context: &BotContext, message: Message) -> Result<()> {
    let synthetic_topic_routed_from_general = message.synthetic_topic_routed_from_general;
    let allowlist = AllowlistConfig {
        user_ids: context.allowlist_user_ids(),
        chat_ids: context.allowlist_chat_ids(),
    };
    let Some(incoming) =
        ingest::parse_incoming_message(context.client(), allowlist, context.config(), message)
            .await?
    else {
        return Ok(());
    };

    // Skip reply_to when message_id == message_thread_id (topic-creating message).
    // Telegram rejects REPLY_MESSAGE_ID_INVALID for these.
    let reply_to_message_id = if synthetic_topic_routed_from_general
        || incoming.message_thread_id == Some(incoming.message_id)
    {
        None
    } else {
        Some(incoming.message_id)
    };
    let topic_id = incoming.message_thread_id;
    let cross_topic_reply_parameters = if synthetic_topic_routed_from_general && topic_id.is_some()
    {
        Some(ReplyParameters {
            message_id: incoming.message_id,
            chat_id: Some(incoming.chat_id),
            allow_sending_without_reply: Some(true),
        })
    } else {
        None
    };

    let reply_ctx = ReplyContext {
        reply_to_message_id,
        topic_id,
        cross_topic_reply_parameters,
    };

    if handle_general_forum_commands(context, &incoming, reply_ctx.reply_to_message_id).await?
        || handle_rebuild_command(context, &incoming, reply_ctx.reply_to_message_id).await?
    {
        return Ok(());
    }

    // Async topic title: spawn LLM-based title generation + rename for new topics.
    if synthetic_topic_routed_from_general && let Some(topic_id) = reply_ctx.topic_id {
        let effective_text = incoming
            .text
            .as_deref()
            .or_else(|| incoming.audios.iter().find_map(|a| a.transcript.as_deref()))
            .filter(|t| !t.trim().is_empty());

        if let Some(text) = effective_text {
            crate::topic_title::spawn_topic_title_update(
                context,
                incoming.chat_id,
                topic_id,
                text.to_string(),
            );
        }
    }

    eprintln!(
        "Accepted message from user {} in chat {}{}",
        incoming.user_id,
        incoming.chat_id,
        reply_ctx
            .topic_id
            .map(|id| format!(" (topic {id})"))
            .unwrap_or_default()
    );

    let thread_id = thread_id_for_chat(incoming.chat_id, reply_ctx.topic_id);
    if handle_thread_commands(
        context,
        &incoming,
        &thread_id,
        reply_ctx.reply_to_message_id,
        reply_ctx.topic_id,
    )
    .await?
    {
        return Ok(());
    }

    run_agent_turn(context, incoming, reply_ctx, &thread_id).await
}

struct TurnStatus {
    key: (i64, i64),
    token: CancellationToken,
    markup: InlineKeyboardMarkup,
    message_id: Option<i64>,
}

struct TurnResult {
    final_text: String,
    got_result: bool,
    had_error: bool,
}

async fn handle_general_forum_commands(
    context: &BotContext,
    incoming: &crate::types::IncomingMessage,
    reply_to_message_id: Option<i64>,
) -> Result<bool> {
    if !incoming.is_forum
        || incoming.message_thread_id.is_some()
        || !incoming.images.is_empty()
        || !incoming.audios.is_empty()
    {
        return Ok(false);
    }

    let Some(text) = incoming.text.as_deref() else {
        return Ok(false);
    };
    let Some(command) = parse_command(text) else {
        return Ok(false);
    };
    if !matches!(command, BotCommand::New | BotCommand::WorktreeCreate) {
        return Ok(false);
    }

    let message = match command {
        BotCommand::New => "/new is not allowed in General.",
        BotCommand::WorktreeCreate => "/worktree must be used inside a topic, not General.",
        BotCommand::Rebuild => unreachable!("rebuild is handled by handle_rebuild_command"),
    };
    context
        .client()
        .send_message(incoming.chat_id, message, reply_to_message_id, None)
        .await?;
    Ok(true)
}

async fn handle_rebuild_command(
    context: &BotContext,
    incoming: &crate::types::IncomingMessage,
    reply_to_message_id: Option<i64>,
) -> Result<bool> {
    if !incoming.images.is_empty() || !incoming.audios.is_empty() {
        return Ok(false);
    }
    if !incoming
        .text
        .as_deref()
        .is_some_and(|text| matches!(parse_command(text), Some(BotCommand::Rebuild)))
    {
        return Ok(false);
    }

    context
        .client()
        .send_message(
            incoming.chat_id,
            "♻️ Rebuilding bot… coming back shortly.",
            reply_to_message_id,
            incoming.message_thread_id,
        )
        .await?;
    context.request_rebuild();
    Ok(true)
}

async fn handle_thread_commands(
    context: &BotContext,
    incoming: &crate::types::IncomingMessage,
    thread_id: &str,
    reply_to_message_id: Option<i64>,
    topic_id: Option<i64>,
) -> Result<bool> {
    if !incoming.images.is_empty() || !incoming.audios.is_empty() {
        return Ok(false);
    }
    let Some(text) = incoming.text.as_deref() else {
        return Ok(false);
    };
    let Some(command) = parse_command(text) else {
        return Ok(false);
    };

    match command {
        BotCommand::New => {
            if incoming.is_forum && topic_id.is_some() {
                context
                    .client()
                    .send_message(
                        incoming.chat_id,
                        "/new is not allowed in topics.",
                        reply_to_message_id,
                        topic_id,
                    )
                    .await?;
                return Ok(true);
            }
            agent::clear_thread_history(thread_id)?;
            context
                .client()
                .send_message(
                    incoming.chat_id,
                    "History cleared. Start a new conversation anytime.",
                    reply_to_message_id,
                    topic_id,
                )
                .await?;
            return Ok(true);
        }
        BotCommand::Rebuild => return Ok(false),
        BotCommand::WorktreeCreate => {}
    }

    let worktree_root = match worktree::ensure_worktree(context.root(), thread_id) {
        Ok(path) => path,
        Err(err) => {
            let msg = format!(
                "Failed to enable worktree: {err}\n\nTip: start the bot from inside a git repo (or a subdirectory of one)."
            );
            context
                .client()
                .send_message(incoming.chat_id, &msg, reply_to_message_id, topic_id)
                .await?;
            return Ok(true);
        }
    };

    let mut thread = zdx_core::core::thread_persistence::Thread::with_id(thread_id.to_string())
        .context("open thread log")?;
    if let Err(err) = thread.set_root_path(&worktree_root) {
        context
            .client()
            .send_message(
                incoming.chat_id,
                &format!("Failed to persist worktree root: {err}"),
                reply_to_message_id,
                topic_id,
            )
            .await?;
        return Ok(true);
    }

    context
        .client()
        .send_message(
            incoming.chat_id,
            &format!("Worktree enabled: {}", worktree_root.display()),
            reply_to_message_id,
            topic_id,
        )
        .await?;
    Ok(true)
}

async fn run_agent_turn(
    context: &BotContext,
    incoming: crate::types::IncomingMessage,
    reply_ctx: ReplyContext,
    thread_id: &str,
) -> Result<()> {
    let worktree_root = thread_persistence::read_thread_root_path(thread_id)?
        .map_or_else(|| context.root().to_path_buf(), std::path::PathBuf::from);
    let (mut thread, mut messages) = agent::load_thread_state(thread_id)?;
    agent::record_user_message(&mut thread, &mut messages, &incoming)?;
    let typing = context
        .client()
        .start_typing(incoming.chat_id, reply_ctx.topic_id);

    let status = setup_turn_status(
        context,
        &incoming,
        reply_ctx.reply_to_message_id,
        reply_ctx.topic_id,
    )
    .await;
    let mut handle = spawn_or_fail(
        context,
        &incoming,
        &worktree_root,
        thread_id,
        &thread,
        messages,
        &status,
    )
    .await?;
    let result = stream_turn_events(context, &incoming, &mut handle, &status).await;
    drop(typing);
    cleanup_turn_status(context, &status).await;
    finalize_turn(context, &incoming, &reply_ctx, &mut thread, &status, result).await
}

async fn setup_turn_status(
    context: &BotContext,
    incoming: &crate::types::IncomingMessage,
    reply_to_message_id: Option<i64>,
    topic_id: Option<i64>,
) -> TurnStatus {
    let key = (incoming.chat_id, incoming.message_id);
    let cancel_markup = InlineKeyboardMarkup {
        inline_keyboard: vec![vec![InlineKeyboardButton {
            text: "⏹ Cancel".to_string(),
            callback_data: Some(format!("cancel:{}:{}", key.0, key.1)),
            url: None,
        }]],
    };

    let token = CancellationToken::new();
    {
        let mut map = context.cancel_map().lock().await;
        map.insert(key, token.clone());
    }

    let mut message_id = context
        .client()
        .send_message_with_markup(
            incoming.chat_id,
            "🧠 Thinking...",
            reply_to_message_id,
            topic_id,
            &cancel_markup,
        )
        .await
        .ok()
        .map(|m| m.id);

    // Retry without reply_to on REPLY_MESSAGE_ID_INVALID
    if message_id.is_none() && reply_to_message_id.is_some() {
        message_id = context
            .client()
            .send_message_with_markup(
                incoming.chat_id,
                "🧠 Thinking...",
                None,
                topic_id,
                &cancel_markup,
            )
            .await
            .ok()
            .map(|m| m.id);
    }

    TurnStatus {
        key,
        token,
        markup: cancel_markup,
        message_id,
    }
}

async fn spawn_or_fail(
    context: &BotContext,
    incoming: &crate::types::IncomingMessage,
    worktree_root: &std::path::Path,
    thread_id: &str,
    thread: &zdx_core::core::thread_persistence::Thread,
    messages: Vec<zdx_core::providers::ChatMessage>,
    status: &TurnStatus,
) -> Result<agent::AgentTurnHandle> {
    let handle = agent::spawn_agent_turn(
        messages,
        context.config(),
        worktree_root,
        context.bot_surface_rules(),
        thread_id,
        thread,
        context.tool_config(),
    );

    match handle {
        Ok(handle) => Ok(handle),
        Err(err) => {
            eprintln!("Failed to spawn agent turn: {err}");
            if let Some(msg_id) = status.message_id {
                let _ = context
                    .client()
                    .edit_message_text(
                        incoming.chat_id,
                        msg_id,
                        "Sorry, something went wrong.",
                        None,
                    )
                    .await;
            }
            cleanup_turn_status(context, status).await;
            Err(err)
        }
    }
}

async fn stream_turn_events(
    context: &BotContext,
    incoming: &crate::types::IncomingMessage,
    handle: &mut agent::AgentTurnHandle,
    status: &TurnStatus,
) -> TurnResult {
    let mut current_status = "🧠 Thinking...".to_string();
    let mut last_edit = std::time::Instant::now()
        .checked_sub(STATUS_DEBOUNCE)
        .expect("debounce subtraction should always succeed");
    let mut final_text = String::new();
    let mut got_result = false;
    let mut had_error = false;

    loop {
        tokio::select! {
            biased;
            () = status.token.cancelled() => {
                handle.task.abort();
                break;
            }
            event = handle.rx.recv() => {
                let Some(event) = event else { break; };
                match &*event {
                    AgentEvent::TurnCompleted { final_text: text, .. } => {
                        final_text.clone_from(text);
                        got_result = true;
                    }
                    AgentEvent::Error { message, .. } => {
                        eprintln!("Agent error event: {message}");
                        had_error = true;
                    }
                    AgentEvent::Interrupted { partial_content } => {
                        if let Some(partial) = partial_content {
                            final_text.clone_from(partial);
                        }
                    }
                    other => update_status(context, incoming.chat_id, status, other, &mut current_status, &mut last_edit).await,
                }
                if got_result { break; }
            }
        }
    }

    TurnResult {
        final_text,
        got_result,
        had_error,
    }
}

async fn update_status(
    context: &BotContext,
    chat_id: i64,
    status: &TurnStatus,
    event: &AgentEvent,
    current_status: &mut String,
    last_edit: &mut std::time::Instant,
) {
    let Some(new_status) = agent::event_to_status(event) else {
        return;
    };
    if new_status == *current_status {
        return;
    }

    *current_status = new_status;
    let Some(msg_id) = status.message_id else {
        return;
    };
    let now = std::time::Instant::now();
    if now.duration_since(*last_edit) < STATUS_DEBOUNCE {
        return;
    }
    *last_edit = now;
    let _ = context
        .client()
        .edit_message_text(chat_id, msg_id, current_status, Some(&status.markup))
        .await;
}

async fn cleanup_turn_status(context: &BotContext, status: &TurnStatus) {
    let mut map = context.cancel_map().lock().await;
    map.remove(&status.key);
}

async fn finalize_turn(
    context: &BotContext,
    incoming: &crate::types::IncomingMessage,
    reply_ctx: &ReplyContext,
    thread: &mut zdx_core::core::thread_persistence::Thread,
    status: &TurnStatus,
    result: TurnResult,
) -> Result<()> {
    if status.token.is_cancelled() {
        eprintln!(
            "Agent turn cancelled for chat {}{}",
            incoming.chat_id,
            reply_ctx
                .topic_id
                .map(|id| format!(" topic {id}"))
                .unwrap_or_default()
        );
        if let Some(msg_id) = status.message_id {
            let _ = context
                .client()
                .edit_message_text(incoming.chat_id, msg_id, "Cancelled ✓", None)
                .await;
        }
        return Ok(());
    }

    if result.had_error && !result.got_result {
        if let Some(msg_id) = status.message_id {
            let _ = context
                .client()
                .edit_message_text(
                    incoming.chat_id,
                    msg_id,
                    "Sorry, something went wrong.",
                    None,
                )
                .await;
        }
        return Ok(());
    }

    if result.got_result {
        thread
            .append(&ThreadEvent::assistant_message(&result.final_text))
            .context("append assistant message")?;
    }

    send_final_response(
        context,
        incoming,
        reply_ctx,
        status.message_id,
        &result.final_text,
    )
    .await
}

async fn send_final_response(
    context: &BotContext,
    incoming: &crate::types::IncomingMessage,
    reply_ctx: &ReplyContext,
    status_message_id: Option<i64>,
    final_text: &str,
) -> Result<()> {
    let parsed = parse_final_response(final_text);
    let has_text = !parsed.text.trim().is_empty();

    if !has_text && parsed.media_paths.is_empty() {
        if let Some(msg_id) = status_message_id
            && let Err(err) = context
                .client()
                .delete_message(incoming.chat_id, msg_id)
                .await
        {
            eprintln!("Failed to delete empty status message {msg_id}: {err}");
        }
        return Ok(());
    }

    if has_text {
        send_text_response(
            context,
            incoming,
            reply_ctx,
            status_message_id,
            parsed.text.as_str(),
        )
        .await?;
    } else if let Some(msg_id) = status_message_id
        && let Err(err) = context
            .client()
            .delete_message(incoming.chat_id, msg_id)
            .await
    {
        eprintln!("Failed to delete empty status message {msg_id}: {err}");
    }

    send_media_responses(context, incoming, reply_ctx, &parsed.media_paths, has_text).await
}

async fn send_text_response(
    context: &BotContext,
    incoming: &crate::types::IncomingMessage,
    reply_ctx: &ReplyContext,
    status_message_id: Option<i64>,
    text: &str,
) -> Result<()> {
    eprintln!("Sending reply for chat {}", incoming.chat_id);

    if let Some(ref reply_parameters) = reply_ctx.cross_topic_reply_parameters {
        if let Some(msg_id) = status_message_id
            && let Err(err) = context
                .client()
                .delete_message(incoming.chat_id, msg_id)
                .await
        {
            eprintln!("Failed to delete status message {msg_id}: {err}");
        }

        context
            .client()
            .send_message_with_reply_params(
                incoming.chat_id,
                text,
                reply_ctx.topic_id,
                Some(reply_parameters.clone()),
            )
            .await?;
        return Ok(());
    }

    if let Some(msg_id) = status_message_id {
        let edit_result = context
            .client()
            .edit_message_text(incoming.chat_id, msg_id, text, None)
            .await;
        if let Err(ref err) = edit_result {
            eprintln!(
                "Failed to edit status message {} in chat {}: {}",
                msg_id, incoming.chat_id, err
            );
            if let Err(del_err) = context
                .client()
                .delete_message(incoming.chat_id, msg_id)
                .await
            {
                eprintln!("Failed to delete stale status message {msg_id}: {del_err}");
            }
            let send_result = context
                .client()
                .send_message(
                    incoming.chat_id,
                    text,
                    reply_ctx.reply_to_message_id,
                    reply_ctx.topic_id,
                )
                .await;
            if let Err(ref e) = send_result {
                if e.to_string().contains("REPLY_MESSAGE_ID_INVALID") {
                    context
                        .client()
                        .send_message(incoming.chat_id, text, None, reply_ctx.topic_id)
                        .await?;
                } else {
                    send_result?;
                }
            }
        }
    } else {
        let send_result = context
            .client()
            .send_message(
                incoming.chat_id,
                text,
                reply_ctx.reply_to_message_id,
                reply_ctx.topic_id,
            )
            .await;
        if let Err(ref e) = send_result {
            if e.to_string().contains("REPLY_MESSAGE_ID_INVALID") {
                context
                    .client()
                    .send_message(incoming.chat_id, text, None, reply_ctx.topic_id)
                    .await?;
            } else {
                send_result?;
            }
        }
    }

    Ok(())
}

async fn send_media_responses(
    context: &BotContext,
    incoming: &crate::types::IncomingMessage,
    reply_ctx: &ReplyContext,
    media_paths: &[PathBuf],
    sent_text: bool,
) -> Result<()> {
    if media_paths.is_empty() {
        return Ok(());
    }

    let valid_media_paths: Vec<PathBuf> = media_paths
        .iter()
        .filter(|path| is_valid_media_path(path))
        .cloned()
        .collect();

    if valid_media_paths.is_empty() {
        if !sent_text {
            context
                .client()
                .send_message(
                    incoming.chat_id,
                    "I couldn't find a valid local media file to send.",
                    None,
                    reply_ctx.topic_id,
                )
                .await?;
        }
        return Ok(());
    }

    for media_path in valid_media_paths {
        let reply_parameters = reply_ctx.cross_topic_reply_parameters.clone();
        let reply_to_message_id = if reply_parameters.is_some() {
            None
        } else {
            reply_ctx.reply_to_message_id
        };

        let send_result = if is_image_path(&media_path) {
            context
                .client()
                .send_photo_from_path(
                    incoming.chat_id,
                    &media_path,
                    None,
                    reply_to_message_id,
                    reply_ctx.topic_id,
                    reply_parameters,
                )
                .await
        } else {
            context
                .client()
                .send_document_from_path(
                    incoming.chat_id,
                    &media_path,
                    None,
                    reply_to_message_id,
                    reply_ctx.topic_id,
                    reply_parameters,
                )
                .await
        };

        if let Err(err) = send_result {
            eprintln!("Failed to send media file {}: {err}", media_path.display());
            context
                .client()
                .send_message(
                    incoming.chat_id,
                    &format!("Failed to send media file {}: {err}", media_path.display()),
                    None,
                    reply_ctx.topic_id,
                )
                .await?;
        }
    }

    Ok(())
}

#[derive(Default)]
struct ParsedFinalResponse {
    text: String,
    media_paths: Vec<PathBuf>,
}

fn parse_final_response(final_text: &str) -> ParsedFinalResponse {
    let text_without_wrappers = strip_media_wrappers(final_text);
    let (text_without_media_tags, raw_media_values) = extract_media_tags(&text_without_wrappers);
    let mut media_paths = Vec::new();
    let mut seen = HashSet::new();

    for raw in raw_media_values {
        if let Some(path) = parse_media_path(&raw)
            && seen.insert(path.clone())
        {
            media_paths.push(path);
        }
    }

    ParsedFinalResponse {
        text: normalize_reply_text(&text_without_media_tags),
        media_paths,
    }
}

fn strip_media_wrappers(input: &str) -> String {
    input
        .replace(MEDIA_BLOCK_OPEN, "")
        .replace(MEDIA_BLOCK_CLOSE, "")
}

fn extract_media_tags(input: &str) -> (String, Vec<String>) {
    let mut cleaned = String::new();
    let mut media_values = Vec::new();
    let mut cursor = 0;

    while let Some(start_rel) = input[cursor..].find(MEDIA_TAG_OPEN) {
        let start = cursor + start_rel;
        let Some(after_tag_name) = input.as_bytes().get(start + MEDIA_TAG_OPEN.len()) else {
            break;
        };
        // Skip accidental matches such as "<medias>".
        if *after_tag_name == b's' {
            let skip_to = start + MEDIA_TAG_OPEN.len();
            cleaned.push_str(&input[cursor..skip_to]);
            cursor = skip_to;
            continue;
        }

        cleaned.push_str(&input[cursor..start]);

        let Some(open_end_rel) = input[start..].find('>') else {
            cleaned.push_str(&input[start..]);
            cursor = input.len();
            break;
        };
        let open_end = start + open_end_rel;
        let open_tag = &input[start..=open_end];

        if open_tag.ends_with("/>") {
            cursor = open_end + 1;
            continue;
        }

        let content_start = open_end + 1;
        let Some(close_rel) = input[content_start..].find(MEDIA_TAG_CLOSE) else {
            cleaned.push_str(&input[start..]);
            cursor = input.len();
            break;
        };
        let content_end = content_start + close_rel;
        let inner = input[content_start..content_end].trim();
        if !inner.is_empty() {
            media_values.push(inner.to_string());
        }

        cursor = content_end + MEDIA_TAG_CLOSE.len();
    }

    if cursor < input.len() {
        cleaned.push_str(&input[cursor..]);
    }

    (cleaned, media_values)
}

fn normalize_reply_text(text: &str) -> String {
    let mut out = String::new();
    let mut prev_blank = false;

    for line in text.lines() {
        let is_blank = line.trim().is_empty();
        if is_blank && prev_blank {
            continue;
        }

        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(line);
        prev_blank = is_blank;
    }

    out.trim().to_string()
}

fn parse_media_path(raw: &str) -> Option<PathBuf> {
    let candidate = raw
        .trim()
        .trim_matches('`')
        .trim_matches('"')
        .trim_matches('\'')
        .trim_start_matches('(')
        .trim_end_matches(')')
        .trim_end_matches([',', ';']);

    if candidate.starts_with('/') {
        Some(PathBuf::from(candidate))
    } else {
        None
    }
}

fn is_valid_media_path(path: &Path) -> bool {
    path.is_absolute() && std::fs::metadata(path).is_ok_and(|meta| meta.is_file())
}

fn is_image_path(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| {
            matches!(
                ext.to_ascii_lowercase().as_str(),
                "png" | "jpg" | "jpeg" | "webp"
            )
        })
}

fn thread_id_for_chat(chat_id: i64, message_thread_id: Option<i64>) -> String {
    match message_thread_id {
        Some(topic_id) => format!("telegram-{chat_id}-topic-{topic_id}"),
        None => format!("telegram-{chat_id}"),
    }
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::{is_image_path, parse_final_response};

    #[test]
    fn parse_final_response_extracts_media_wrapper_format() {
        let parsed = parse_final_response("Done.\n<medias><media>/tmp/out.png</media></medias>");
        assert_eq!(parsed.text, "Done.");
        assert_eq!(parsed.media_paths, vec![PathBuf::from("/tmp/out.png")]);
    }

    #[test]
    fn parse_final_response_extracts_multiple_media_entries() {
        let parsed = parse_final_response(
            "<medias><media>/tmp/one.png</media><media>/tmp/two.pdf</media></medias>",
        );
        assert!(parsed.text.is_empty());
        assert_eq!(
            parsed.media_paths,
            vec![PathBuf::from("/tmp/one.png"), PathBuf::from("/tmp/two.pdf")]
        );
    }

    #[test]
    fn parse_final_response_extracts_bare_media_entries_without_wrapper() {
        let parsed =
            parse_final_response("Done.\n<media>/tmp/one.png</media>\n<media>/tmp/two.pdf</media>");
        assert_eq!(parsed.text, "Done.");
        assert_eq!(
            parsed.media_paths,
            vec![PathBuf::from("/tmp/one.png"), PathBuf::from("/tmp/two.pdf")]
        );
    }

    #[test]
    fn parse_final_response_ignores_media_path_attribute_format() {
        let parsed = parse_final_response("<media path=\"/tmp/out.png\"/>");
        assert!(parsed.text.is_empty());
        assert!(parsed.media_paths.is_empty());
    }

    #[test]
    fn parse_final_response_ignores_plain_absolute_paths_without_media_xml() {
        let parsed = parse_final_response("/tmp/report.pdf");
        assert_eq!(parsed.text, "/tmp/report.pdf");
        assert!(parsed.media_paths.is_empty());
    }

    #[test]
    fn image_extension_routing_is_detected() {
        assert!(is_image_path(Path::new("/tmp/screenshot.webp")));
        assert!(!is_image_path(Path::new("/tmp/report.pdf")));
    }
}
