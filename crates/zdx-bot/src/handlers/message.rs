use anyhow::{Result, anyhow};
use tokio_util::sync::CancellationToken;
use zdx_core::core::events::AgentEvent;
use zdx_core::core::thread_persistence::{self, ThreadEvent};
use zdx_core::core::worktree;

use crate::bot::context::BotContext;
use crate::ingest::AllowlistConfig;
use crate::telegram::{InlineKeyboardButton, InlineKeyboardMarkup, Message};
use crate::{agent, ingest};

/// Minimum interval between Telegram status message edits (avoid rate limiting).
const STATUS_DEBOUNCE: std::time::Duration = std::time::Duration::from_secs(3);

pub(crate) async fn handle_message(context: &BotContext, message: Message) -> Result<()> {
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

    // Always reply to the original message - this shows the original message
    // context in the topic, which is useful for reference
    let reply_to_message_id = Some(incoming.message_id);

    // Handle commands that are blocked in General (topic wasn't created for these)
    if incoming.is_forum
        && incoming.message_thread_id.is_none()
        && incoming.images.is_empty()
        && incoming.audios.is_empty()
        && let Some(text) = incoming.text.as_deref()
        && (is_new_command(text) || is_worktree_create_command(text))
    {
        let message = if is_new_command(text) {
            "/new is not allowed in General."
        } else {
            "/worktree must be used inside a topic, not General."
        };
        context
            .client()
            .send_message(incoming.chat_id, message, reply_to_message_id, None)
            .await?;
        return Ok(());
    }

    // Handle /rebuild command (allowed from any context)
    if incoming.images.is_empty()
        && incoming.audios.is_empty()
        && let Some(text) = incoming.text.as_deref()
        && is_rebuild_command(text)
    {
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
        return Ok(());
    }

    // Use the topic_id from the message (set by dispatch_message for General messages)
    let topic_id = incoming.message_thread_id;

    eprintln!(
        "Accepted message from user {} in chat {}{}",
        incoming.user_id,
        incoming.chat_id,
        topic_id
            .map(|id| format!(" (topic {})", id))
            .unwrap_or_default()
    );

    let thread_id = thread_id_for_chat(incoming.chat_id, topic_id);
    if incoming.images.is_empty()
        && incoming.audios.is_empty()
        && let Some(text) = incoming.text.as_deref()
    {
        if is_new_command(text) {
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
                return Ok(());
            }
            agent::clear_thread_history(&thread_id)?;
            context
                .client()
                .send_message(
                    incoming.chat_id,
                    "History cleared. Start a new conversation anytime.",
                    reply_to_message_id,
                    topic_id,
                )
                .await?;
            return Ok(());
        }

        if is_worktree_create_command(text) {
            let worktree_root = match worktree::ensure_worktree(context.root(), &thread_id) {
                Ok(path) => path,
                Err(err) => {
                    let msg = format!(
                        "Failed to enable worktree: {}\n\nTip: start the bot from inside a git repo (or a subdirectory of one).",
                        err
                    );
                    context
                        .client()
                        .send_message(incoming.chat_id, &msg, reply_to_message_id, topic_id)
                        .await?;
                    return Ok(());
                }
            };
            let mut thread = zdx_core::core::thread_persistence::Thread::with_id(thread_id.clone())
                .map_err(|_| anyhow!("Failed to open thread log"))?;
            if let Err(err) = thread.set_root_path(&worktree_root) {
                context
                    .client()
                    .send_message(
                        incoming.chat_id,
                        &format!("Failed to persist worktree root: {}", err),
                        reply_to_message_id,
                        topic_id,
                    )
                    .await?;
                return Ok(());
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
            return Ok(());
        }
    }

    let worktree_root = thread_persistence::read_thread_root_path(&thread_id)?
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| context.root().to_path_buf());
    let (mut thread, mut messages) = agent::load_thread_state(&thread_id)?;
    agent::record_user_message(&mut thread, &mut messages, &incoming)?;

    // Keep Telegram native typing indicator alongside the Thinking status.
    let _typing = context.client().start_typing(incoming.chat_id, topic_id);

    // Send "Thinking..." status message with Cancel button.
    let cancel_key = (incoming.chat_id, incoming.message_id);
    let cancel_data = format!("cancel:{}:{}", cancel_key.0, cancel_key.1);
    let cancel_markup = InlineKeyboardMarkup {
        inline_keyboard: vec![vec![InlineKeyboardButton {
            text: "⏹ Cancel".to_string(),
            callback_data: Some(cancel_data),
            url: None,
        }]],
    };

    // Register cancellation token before sending status
    let cancel_token = CancellationToken::new();
    {
        let mut map = context.cancel_map().lock().await;
        map.insert(cancel_key, cancel_token.clone());
    }

    let status_msg = context
        .client()
        .send_message_with_markup(
            incoming.chat_id,
            "🧠 Thinking...",
            reply_to_message_id,
            topic_id,
            &cancel_markup,
        )
        .await;

    let status_message_id = status_msg.as_ref().ok().map(|m| m.message_id);

    // Spawn the agent turn — returns a handle with streaming events
    let handle = agent::spawn_agent_turn(
        messages,
        context.config(),
        &worktree_root,
        context.bot_system_prompt(),
        &thread_id,
        &thread,
        context.tool_config(),
    );

    let mut handle = match handle {
        Ok(h) => h,
        Err(err) => {
            eprintln!("Failed to spawn agent turn: {}", err);
            if let Some(msg_id) = status_message_id {
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
            // Clean up cancellation token
            let mut map = context.cancel_map().lock().await;
            map.remove(&cancel_key);
            return Err(err);
        }
    };

    // Consume streaming events, updating Telegram status with live activity
    let mut current_status = "🧠 Thinking...".to_string();
    let mut last_edit = std::time::Instant::now() - STATUS_DEBOUNCE; // Allow immediate first edit
    let mut final_text = String::new();
    let mut got_result = false;
    let mut had_error = false;

    loop {
        tokio::select! {
            biased;
            _ = cancel_token.cancelled() => {
                // User pressed Cancel — abort the agent task
                handle.task.abort();
                break;
            }
            event = handle.rx.recv() => {
                let Some(event) = event else {
                    // Channel closed — agent finished (or crashed)
                    break;
                };

                match &*event {
                    AgentEvent::TurnCompleted { final_text: text, .. } => {
                        final_text = text.clone();
                        got_result = true;
                        // Don't break yet — drain remaining events
                    }
                    AgentEvent::Error { message, .. } => {
                        eprintln!("Agent error event: {}", message);
                        had_error = true;
                    }
                    AgentEvent::Interrupted { partial_content } => {
                        if let Some(partial) = partial_content {
                            final_text = partial.clone();
                        }
                    }
                    other => {
                        // Update status based on event
                        if let Some(new_status) = agent::event_to_status(other)
                            && new_status != current_status
                        {
                            current_status = new_status;
                            // Debounce edits to avoid Telegram rate limits
                            if let Some(msg_id) = status_message_id {
                                let now = std::time::Instant::now();
                                if now.duration_since(last_edit) >= STATUS_DEBOUNCE {
                                    last_edit = now;
                                    let _ = context
                                        .client()
                                        .edit_message_text(
                                            incoming.chat_id,
                                            msg_id,
                                            &current_status,
                                            Some(&cancel_markup),
                                        )
                                        .await;
                                }
                            }
                        }
                    }
                }

                if got_result {
                    break;
                }
            }
        }
    }

    // Stop Telegram typing indicator
    drop(_typing);

    // Clean up cancellation token
    {
        let mut map = context.cancel_map().lock().await;
        map.remove(&cancel_key);
    }

    // Check if cancelled
    if cancel_token.is_cancelled() {
        eprintln!(
            "Agent turn cancelled for chat {}{}",
            incoming.chat_id,
            topic_id
                .map(|id| format!(" topic {}", id))
                .unwrap_or_default()
        );
        if let Some(msg_id) = status_message_id {
            let _ = context
                .client()
                .edit_message_text(incoming.chat_id, msg_id, "Cancelled ✓", None)
                .await;
        }
        return Ok(());
    }

    // Handle result
    if had_error && !got_result {
        if let Some(msg_id) = status_message_id {
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

    // Persist assistant message and send final response
    if got_result {
        thread
            .append(&ThreadEvent::assistant_message(&final_text))
            .map_err(|_| anyhow!("Failed to append assistant message"))?;
    }

    if !final_text.trim().is_empty() {
        eprintln!("Sending reply for chat {}", incoming.chat_id);
        if let Some(msg_id) = status_message_id {
            let edit_result = context
                .client()
                .edit_message_text(incoming.chat_id, msg_id, &final_text, None)
                .await;
            if let Err(ref err) = edit_result {
                eprintln!(
                    "Failed to edit status message {} in chat {}: {}",
                    msg_id, incoming.chat_id, err
                );
                // Delete the old status message to remove stale cancel button
                if let Err(del_err) = context
                    .client()
                    .delete_message(incoming.chat_id, msg_id)
                    .await
                {
                    eprintln!(
                        "Failed to delete stale status message {}: {}",
                        msg_id, del_err
                    );
                }
                // Fallback: send as a new message
                context
                    .client()
                    .send_message(incoming.chat_id, &final_text, reply_to_message_id, topic_id)
                    .await?;
            }
        } else {
            context
                .client()
                .send_message(incoming.chat_id, &final_text, reply_to_message_id, topic_id)
                .await?;
        }
    } else if let Some(msg_id) = status_message_id {
        // Empty response — remove the thinking message
        if let Err(err) = context
            .client()
            .delete_message(incoming.chat_id, msg_id)
            .await
        {
            eprintln!("Failed to delete empty status message {}: {}", msg_id, err);
        }
    }

    Ok(())
}

fn thread_id_for_chat(chat_id: i64, message_thread_id: Option<i64>) -> String {
    match message_thread_id {
        Some(topic_id) => format!("telegram-{}-topic-{}", chat_id, topic_id),
        None => format!("telegram-{}", chat_id),
    }
}

fn command_matches(text: &str, command: &str) -> bool {
    let trimmed = text.trim();
    if trimmed == command {
        return true;
    }
    if let Some(stripped) = trimmed.strip_prefix(command) {
        return stripped.starts_with('@');
    }
    false
}

fn is_new_command(text: &str) -> bool {
    command_matches(text, "/new")
}

fn is_rebuild_command(text: &str) -> bool {
    command_matches(text, "/rebuild")
}

fn is_worktree_create_command(text: &str) -> bool {
    ["/worktree create", "/worktree", "/wt"]
        .iter()
        .any(|cmd| command_matches(text, cmd))
}
