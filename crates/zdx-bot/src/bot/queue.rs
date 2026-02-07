use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::{Mutex, mpsc};

use crate::bot::context::BotContext;
use crate::handlers::message::handle_message;
use crate::telegram::Message;

/// Queue key: (chat_id, topic_id). Different topics run concurrently.
/// DMs use (chat_id, 0) since they have no topic.
type QueueKey = (i64, i64);

pub(crate) type ChatQueueMap = Arc<Mutex<HashMap<QueueKey, mpsc::UnboundedSender<Message>>>>;

pub(crate) fn new_chat_queues() -> ChatQueueMap {
    Arc::new(Mutex::new(HashMap::new()))
}

/// Route a message to the right handler:
/// - Forum General (no topic): create topic first, then enqueue
/// - Forum topic / DM: enqueue for sequential processing
pub(crate) async fn dispatch_message(
    queues: &ChatQueueMap,
    context: &Arc<BotContext>,
    message: Message,
) {
    let is_forum_general = message.chat.is_forum_enabled() && message.message_thread_id.is_none();

    if is_forum_general {
        // Quick allowlist check before creating topic (avoid creating topics for
        // unauthorized/bot messages). Full validation happens in handle_message.
        if !should_process_message(context, &message) {
            return;
        }

        // Check for commands that shouldn't create a topic
        if let Some(text) = message.text.as_deref()
            && is_command_blocked_in_general(text)
        {
            // Handle directly without creating a topic
            spawn_standalone(Arc::clone(context), message);
            return;
        }

        // Spawn task to create topic then enqueue (don't block update loop)
        let queues = Arc::clone(queues);
        let context = Arc::clone(context);
        tokio::spawn(async move {
            let topic_name = generate_topic_name(message.text.as_deref());
            match context
                .client()
                .create_forum_topic(message.chat.id, &topic_name)
                .await
            {
                Ok(topic_id) => {
                    eprintln!(
                        "Created topic '{}' (id: {}) for chat {}",
                        topic_name, topic_id, message.chat.id
                    );
                    // Enqueue with the new topic ID so handler knows to use it
                    let mut message = message;
                    message.message_thread_id = Some(topic_id);
                    enqueue_message(&queues, &context, message).await;
                }
                Err(err) => {
                    eprintln!(
                        "Failed to create topic for chat {}: {}",
                        message.chat.id, err
                    );
                    // Fall back to processing in General (no queue, replies in General)
                    if let Err(err) = handle_message(context.as_ref(), message).await {
                        eprintln!("Standalone message handling error: {}", err);
                    }
                }
            }
        });
    } else {
        enqueue_message(queues, context, message).await;
    }
}

/// Quick check if message should be processed (allowlist + bot filter).
/// Returns false for messages that should be silently ignored.
fn should_process_message(context: &BotContext, message: &Message) -> bool {
    // Check chat allowlist for groups
    if message.chat.is_group() && !context.allowlist_chat_ids().contains(&message.chat.id) {
        eprintln!("Ignoring non-allowlisted group chat {}", message.chat.id);
        return false;
    }

    // Check sender exists and is not a bot
    let Some(user) = message.from.as_ref() else {
        eprintln!(
            "Ignoring message without sender in chat {}",
            message.chat.id
        );
        return false;
    };

    if user.is_bot {
        return false;
    }

    // Check user allowlist
    if !context.allowlist_user_ids().contains(&user.id) {
        eprintln!("Denied user {} for chat {}", user.id, message.chat.id);
        return false;
    }

    true
}

/// Check if a command should be blocked in General (not create a topic).
fn is_command_blocked_in_general(text: &str) -> bool {
    let trimmed = text.trim();
    // /new, /worktree, and /restart commands shouldn't create topics
    trimmed == "/new"
        || trimmed.starts_with("/new@")
        || trimmed == "/worktree"
        || trimmed.starts_with("/worktree@")
        || trimmed == "/worktree create"
        || trimmed.starts_with("/worktree create@")
        || trimmed == "/wt"
        || trimmed.starts_with("/wt@")
        || trimmed == "/restart"
        || trimmed.starts_with("/restart@")
        || trimmed == "/rebuild"
        || trimmed.starts_with("/rebuild@")
}

/// Generate a topic name from message text.
fn generate_topic_name(text: Option<&str>) -> String {
    const MAX_TOPIC_NAME_LEN: usize = 64;

    if let Some(text) = text {
        let trimmed = text.trim();
        if !trimmed.is_empty() {
            let first_line = trimmed.lines().next().unwrap_or(trimmed);
            let char_count = first_line.chars().count();
            if char_count <= MAX_TOPIC_NAME_LEN {
                return first_line.to_string();
            }
            let truncated: String = first_line.chars().take(MAX_TOPIC_NAME_LEN).collect();
            if let Some(last_space) = truncated.rfind(' ')
                && last_space > MAX_TOPIC_NAME_LEN / 2
            {
                return format!("{}…", &truncated[..last_space]);
            }
            return format!("{}…", truncated.trim_end());
        }
    }

    let now = chrono::Utc::now();
    now.format("Chat %Y-%m-%d %H:%M").to_string()
}

/// Spawn a standalone task for a message (no queuing, fully concurrent).
/// Used only as fallback when topic creation fails.
fn spawn_standalone(context: Arc<BotContext>, message: Message) {
    tokio::spawn(async move {
        if let Err(err) = handle_message(context.as_ref(), message).await {
            eprintln!("Standalone message handling error: {}", err);
        }
    });
}

async fn enqueue_message(queues: &ChatQueueMap, context: &Arc<BotContext>, message: Message) {
    let key = (message.chat.id, message.message_thread_id.unwrap_or(0));
    let sender = {
        let mut queues = queues.lock().await;
        if let Some(sender) = queues.get(&key) {
            sender.clone()
        } else {
            let (sender, receiver) = mpsc::unbounded_channel();
            spawn_queue_worker(key, receiver, Arc::clone(context));
            queues.insert(key, sender.clone());
            sender
        }
    };

    if let Err(err) = sender.send(message) {
        let message = err.0;
        let (sender, receiver) = mpsc::unbounded_channel();
        spawn_queue_worker(key, receiver, Arc::clone(context));
        {
            let mut queues = queues.lock().await;
            queues.insert(key, sender.clone());
        }
        let _ = sender.send(message);
    }
}

fn spawn_queue_worker(
    key: QueueKey,
    mut receiver: mpsc::UnboundedReceiver<Message>,
    context: Arc<BotContext>,
) {
    tokio::spawn(async move {
        while let Some(message) = receiver.recv().await {
            if let Err(err) = handle_message(context.as_ref(), message).await {
                eprintln!("Message handling error for {:?}: {}", key, err);
            }
        }
    });
}
