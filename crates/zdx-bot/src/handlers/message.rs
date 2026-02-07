use anyhow::{Result, anyhow};
use tokio_util::sync::CancellationToken;
use zdx_core::core::thread_log::{self, ThreadEvent};
use zdx_core::core::worktree;

use crate::bot::context::BotContext;
use crate::ingest::AllowlistConfig;
use crate::telegram::{InlineKeyboardButton, InlineKeyboardMarkup, Message};
use crate::{agent, ingest};

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
            let worktree_root = worktree::ensure_worktree(context.root(), &thread_id)
                .map_err(|err| anyhow!("Failed to ensure worktree for {}: {}", thread_id, err))?;
            let mut thread = zdx_core::core::thread_log::ThreadLog::with_id(thread_id.clone())
                .map_err(|_| anyhow!("Failed to open thread log"))?;
            thread
                .set_root_path(&worktree_root)
                .map_err(|err| anyhow!("Failed to set thread root: {}", err))?;
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

    let worktree_root = thread_log::read_thread_root_path(&thread_id)?
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| context.root().to_path_buf());
    let (mut thread, mut messages) = agent::load_thread_state(&thread_id)?;
    agent::record_user_message(&mut thread, &mut messages, &incoming)?;

    // Keep Telegram native typing indicator alongside the Thinking status.
    let _typing = context.client().start_typing(incoming.chat_id, topic_id);

    // Send "Thinking..." status message with Cancel button.
    // The cancel callback data uses the user message ID (per-chat unique), so
    // stale buttons from older turns can't cancel a new turn.
    let cancel_key = (incoming.chat_id, incoming.message_id);
    let cancel_data = format!("cancel:{}:{}", cancel_key.0, cancel_key.1);
    let cancel_markup = InlineKeyboardMarkup {
        inline_keyboard: vec![vec![InlineKeyboardButton {
            text: "⏹ Cancel".to_string(),
            callback_data: Some(cancel_data),
            url: None,
        }]],
    };

    // Register cancellation token before sending status, so immediate button
    // taps can always find the token.
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

    // Race the agent turn against the cancellation token
    let agent_result = tokio::select! {
        result = agent::run_agent_turn_with_persist(
            messages,
            context.config(),
            &worktree_root,
            context.bot_system_prompt(),
            &thread_id,
            &thread,
            context.tool_config(),
        ) => Some(result),
        _ = cancel_token.cancelled() => None,
    };

    // Stop Telegram typing indicator as soon as processing finishes/cancels,
    // before we edit/send final status text.
    drop(_typing);

    // Clean up cancellation token
    {
        let mut map = context.cancel_map().lock().await;
        map.remove(&cancel_key);
    }

    match agent_result {
        Some(Ok((final_text, _messages))) => {
            thread
                .append(&ThreadEvent::assistant_message(&final_text))
                .map_err(|_| anyhow!("Failed to append assistant message"))?;
            if !final_text.trim().is_empty() {
                eprintln!("Sending reply for chat {}", incoming.chat_id);
                // Try to edit the status message with the final response
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
                            .send_message(
                                incoming.chat_id,
                                &final_text,
                                reply_to_message_id,
                                topic_id,
                            )
                            .await?;
                    }
                } else {
                    // No status message was sent (send_message_with_markup failed)
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
        }
        Some(Err(err)) => {
            eprintln!("Agent error: {}", err);
            if let Some(msg_id) = status_message_id {
                if let Err(edit_err) = context
                    .client()
                    .edit_message_text(
                        incoming.chat_id,
                        msg_id,
                        "Sorry, something went wrong.",
                        None,
                    )
                    .await
                {
                    eprintln!(
                        "Failed to edit error status message {}: {}",
                        msg_id, edit_err
                    );
                }
            } else if let Err(send_err) = context
                .client()
                .send_message(
                    incoming.chat_id,
                    "Sorry, something went wrong.",
                    reply_to_message_id,
                    topic_id,
                )
                .await
            {
                eprintln!(
                    "Failed to send error message to chat {}: {}",
                    incoming.chat_id, send_err
                );
            }
        }
        None => {
            // Cancelled by user
            eprintln!(
                "Agent turn cancelled for chat {}{}",
                incoming.chat_id,
                topic_id
                    .map(|id| format!(" topic {}", id))
                    .unwrap_or_default()
            );
            if let Some(msg_id) = status_message_id
                && let Err(err) = context
                    .client()
                    .edit_message_text(incoming.chat_id, msg_id, "Cancelled ✓", None)
                    .await
            {
                eprintln!(
                    "Failed to edit cancelled status message {}: {}",
                    msg_id, err
                );
            }
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
