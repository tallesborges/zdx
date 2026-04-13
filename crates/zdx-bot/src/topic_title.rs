//! Async topic title generation.
//!
//! After a topic is created from General, spawns a background LLM call
//! to generate a descriptive title, renames the topic via Telegram API,
//! and syncs the persisted ZDX thread title.

use zdx_engine::core::{thread_persistence, title_generation};

use crate::bot::context::BotContext;

/// Spawn a fire-and-forget task that generates a topic title via LLM
/// and renames the Telegram forum topic.
pub(crate) fn spawn_topic_title_update(
    context: &BotContext,
    chat_id: i64,
    topic_id: i64,
    message_text: String,
) {
    let client = context.client().clone();
    let title_model = context.config().title_model;
    let root = context.root().to_path_buf();

    tokio::spawn(async move {
        match title_generation::generate_title(&message_text, &title_model, &root).await {
            Ok(title) => {
                if let Err(err) = client.edit_forum_topic(chat_id, topic_id, &title).await {
                    tracing::error!(topic_id, %err, "Failed to rename topic");
                } else {
                    let thread_id = format!("telegram-{chat_id}-topic-{topic_id}");
                    if let Err(err) =
                        thread_persistence::set_thread_title(&thread_id, Some(title.clone()))
                    {
                        tracing::warn!(topic_id, thread_id = %thread_id, %err, "Renamed topic but failed to update thread title");
                    }
                    tracing::info!(topic_id, title = %title, "Renamed topic");
                }
            }
            Err(err) => {
                tracing::error!(topic_id, %err, "Topic title generation failed");
            }
        }
    });
}
