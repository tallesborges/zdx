//! Async topic title generation.
//!
//! After a topic is created from General, spawns a background LLM call
//! to generate a descriptive title, renames the topic via Telegram API,
//! and syncs the persisted ZDX thread title.

use zdx_core::core::title_generation;
use zdx_core::core::thread_persistence;

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
    let title_model = context.config().title_model.clone();
    let root = context.root().to_path_buf();

    tokio::spawn(async move {
        match title_generation::generate_title(&message_text, &title_model, &root).await {
            Ok(title) => {
                if let Err(err) = client.edit_forum_topic(chat_id, topic_id, &title).await {
                    eprintln!("Failed to rename topic {topic_id}: {err}");
                } else {
                    let thread_id = format!("telegram-{chat_id}-topic-{topic_id}");
                    if let Err(err) =
                        thread_persistence::set_thread_title(&thread_id, Some(title.clone()))
                    {
                        eprintln!(
                            "Renamed topic {topic_id} but failed to update thread title for {thread_id}: {err}"
                        );
                    }
                    eprintln!("Renamed topic {topic_id} to '{title}'");
                }
            }
            Err(err) => {
                eprintln!("Topic title generation failed for topic {topic_id}: {err}");
            }
        }
    });
}
