use anyhow::{Result, anyhow};
use zdx_core::core::thread_log::ThreadEvent;

use crate::bot::context::BotContext;
use crate::ingest::AllowlistConfig;
use crate::telegram::Message;
use crate::types::IncomingMessage;
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

    // For forum-enabled group messages without a topic, create a new topic
    // Only create topics in forums (is_forum=true), not regular groups
    let topic_id = if incoming.is_forum && incoming.message_thread_id.is_none() {
        let topic_name = generate_topic_name(&incoming);
        match context
            .client()
            .create_forum_topic(incoming.chat_id, &topic_name)
            .await
        {
            Ok(id) => {
                eprintln!(
                    "Created topic '{}' (id: {}) for chat {}",
                    topic_name, id, incoming.chat_id
                );
                Some(id)
            }
            Err(err) => {
                eprintln!(
                    "Failed to create topic for chat {}: {}",
                    incoming.chat_id, err
                );
                // Fall back to no topic (will reply in General)
                None
            }
        }
    } else {
        incoming.message_thread_id
    };

    // Always reply to the original message - this shows the original message
    // context in the topic, which is useful for reference
    let reply_to_message_id = Some(incoming.message_id);

    eprintln!(
        "Accepted message from user {} in chat {}{}",
        incoming.user_id,
        incoming.chat_id,
        topic_id
            .map(|id| format!(" (topic {})", id))
            .unwrap_or_default()
    );

    if incoming.images.is_empty()
        && incoming.audios.is_empty()
        && let Some(text) = incoming.text.as_deref()
        && is_new_command(text)
    {
        let thread_id = thread_id_for_chat(incoming.chat_id, topic_id);
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

    let thread_id = thread_id_for_chat(incoming.chat_id, topic_id);
    let (mut thread, mut messages) = agent::load_thread_state(&thread_id)?;
    agent::record_user_message(&mut thread, &mut messages, &incoming)?;

    let _typing = context.client().start_typing(incoming.chat_id, topic_id);

    let result = agent::run_agent_turn_with_persist(
        messages,
        context.config(),
        context.root(),
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

/// Generate a topic name from the incoming message.
/// Uses the first few words of the text, or a timestamp if no text.
fn generate_topic_name(incoming: &IncomingMessage) -> String {
    const MAX_TOPIC_NAME_LEN: usize = 64; // Telegram limit is 128, but keep it short

    if let Some(text) = incoming.text.as_deref() {
        let trimmed = text.trim();
        if !trimmed.is_empty() {
            // Take first line
            let first_line = trimmed.lines().next().unwrap_or(trimmed);

            // Count characters (not bytes) for safe Unicode handling
            let char_count = first_line.chars().count();
            if char_count <= MAX_TOPIC_NAME_LEN {
                return first_line.to_string();
            }

            // Truncate at character boundary (safe for Unicode)
            let truncated: String = first_line.chars().take(MAX_TOPIC_NAME_LEN).collect();

            // Try to truncate at word boundary if possible
            if let Some(last_space) = truncated.rfind(' ')
                && last_space > MAX_TOPIC_NAME_LEN / 2
            {
                return format!("{}…", &truncated[..last_space]);
            }
            return format!("{}…", truncated.trim_end());
        }
    }

    // Fallback: use timestamp
    let now = chrono::Utc::now();
    now.format("Chat %Y-%m-%d %H:%M").to_string()
}

fn thread_id_for_chat(chat_id: i64, message_thread_id: Option<i64>) -> String {
    match message_thread_id {
        Some(topic_id) => format!("telegram-{}-topic-{}", chat_id, topic_id),
        None => format!("telegram-{}", chat_id),
    }
}

fn is_new_command(text: &str) -> bool {
    text.trim() == "/new"
}
