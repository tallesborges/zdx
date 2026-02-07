use anyhow::{Result, anyhow};
use zdx_core::core::thread_log::{self, ThreadEvent};
use zdx_core::core::worktree;

use crate::bot::context::BotContext;
use crate::ingest::AllowlistConfig;
use crate::telegram::Message;
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

    // Handle /restart command (allowed from any context)
    if incoming.images.is_empty()
        && incoming.audios.is_empty()
        && let Some(text) = incoming.text.as_deref()
        && is_restart_command(text)
    {
        context
            .client()
            .send_message(
                incoming.chat_id,
                "♻️ Restarting bot… rebuilding and coming back shortly.",
                reply_to_message_id,
                incoming.message_thread_id,
            )
            .await?;
        context.request_restart();
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

    let _typing = context.client().start_typing(incoming.chat_id, topic_id);

    let result = agent::run_agent_turn_with_persist(
        messages,
        context.config(),
        &worktree_root,
        context.bot_system_prompt(),
        &thread_id,
        &thread,
        context.tool_config(),
    )
    .await;

    match result {
        Ok((final_text, _messages)) => {
            thread
                .append(&ThreadEvent::assistant_message(&final_text))
                .map_err(|_| anyhow!("Failed to append assistant message"))?;
            if !final_text.trim().is_empty() {
                eprintln!("Sending reply for chat {}", incoming.chat_id);
                context
                    .client()
                    .send_message(incoming.chat_id, &final_text, reply_to_message_id, topic_id)
                    .await?;
            }
        }
        Err(err) => {
            eprintln!("Agent error: {}", err);
            let _ = context
                .client()
                .send_message(
                    incoming.chat_id,
                    "Sorry, something went wrong.",
                    reply_to_message_id,
                    topic_id,
                )
                .await;
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

fn is_restart_command(text: &str) -> bool {
    command_matches(text, "/restart") || command_matches(text, "/rebuild")
}

fn is_worktree_create_command(text: &str) -> bool {
    ["/worktree create", "/worktree", "/wt"]
        .iter()
        .any(|cmd| command_matches(text, cmd))
}
