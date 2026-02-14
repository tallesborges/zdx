use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::{Mutex, mpsc};
use tokio_util::sync::CancellationToken;

use crate::bot::context::{BotContext, QueueCancelKey};
use crate::handlers::message::handle_message;
use crate::telegram::{InlineKeyboardButton, InlineKeyboardMarkup, Message};

/// Queue key: (`chat_id`, `topic_id`). Different topics run concurrently.
/// DMs use (`chat_id`, 0) since they have no topic.
type QueueKey = (i64, i64);

/// Item sent through the per-topic queue channel.
pub(crate) struct QueueItem {
    message: Message,
    /// Cancellation token for this queued item. Checked before processing.
    cancel_token: CancellationToken,
    /// If this item was queued (not first), holds the status message info
    /// so the worker can clean it up.
    queued_status: Option<QueuedStatus>,
}

pub(crate) struct QueueState {
    sender: mpsc::UnboundedSender<QueueItem>,
    /// Number of items currently pending for this key (including the item
    /// actively being processed by the worker).
    pending: usize,
}
struct QueuedStatus {
    chat: i64,
    /// `message_id` of the "⏳ Queued" bot message.
    status: i64,
    /// `message_id` of the user's original message (for deletion on cancel).
    original: i64,
}

pub(crate) type ChatQueueMap = Arc<Mutex<HashMap<QueueKey, QueueState>>>;

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
    let is_forum_general =
        message.chat.is_forum_enabled() && message.effective_thread_id().is_none();

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
                    message.thread_id = Some(topic_id);
                    message.synthetic_topic_routed_from_general = true;
                    enqueue_message(&queues, &context, message).await;
                }
                Err(err) => {
                    eprintln!(
                        "Failed to create topic for chat {}: {}",
                        message.chat.id, err
                    );
                    // Fall back to processing in General (no queue, replies in General)
                    if let Err(err) = handle_message(context.as_ref(), message).await {
                        eprintln!("Standalone message handling error: {err}");
                    }
                }
            }
        });
    } else {
        // Quick filter: skip bot messages and unauthorized users before enqueuing,
        // so they don't produce a spurious "⏳ Queued" status message.
        // (e.g. service messages from topic creation have thread_id set and would
        // otherwise get enqueued and show "Queued" before being discarded.)
        if !should_process_message(context, &message) {
            return;
        }
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
    // /new, /worktree, and /rebuild commands shouldn't create topics
    trimmed == "/new"
        || trimmed.starts_with("/new@")
        || trimmed == "/worktree"
        || trimmed.starts_with("/worktree@")
        || trimmed == "/worktree create"
        || trimmed.starts_with("/worktree create@")
        || trimmed == "/wt"
        || trimmed.starts_with("/wt@")
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
            eprintln!("Standalone message handling error: {err}");
        }
    });
}

async fn enqueue_message(queues: &ChatQueueMap, context: &Arc<BotContext>, message: Message) {
    let key = (message.chat.id, message.effective_thread_id().unwrap_or(0));
    let queues_map = Arc::clone(queues);
    let (sender, should_show_queued) = {
        let mut map = queues.lock().await;
        if let Some(state) = map.get_mut(&key) {
            let should_show = state.pending > 0;
            state.pending += 1;
            (state.sender.clone(), should_show)
        } else {
            let (sender, receiver) = mpsc::unbounded_channel();
            spawn_queue_worker(key, receiver, Arc::clone(context), Arc::clone(&queues_map));
            map.insert(
                key,
                QueueState {
                    sender: sender.clone(),
                    pending: 1,
                },
            );
            (sender, false)
        }
    };

    let cancel_token = CancellationToken::new();
    let mut queued_status = None;

    // If there's already a queue worker (potentially busy), show "Queued" status
    if should_show_queued {
        let chat_id = message.chat.id;
        let topic_id = message.effective_thread_id();
        let user_message_id = message.id;
        let cancel_data = format!("cancel_q:{chat_id}:{user_message_id}");
        let cancel_markup = InlineKeyboardMarkup {
            inline_keyboard: vec![vec![InlineKeyboardButton {
                text: "✖ Cancel".to_string(),
                callback_data: Some(cancel_data),
                url: None,
            }]],
        };

        match context
            .client()
            .send_message_with_markup(
                chat_id,
                "⏳ Queued",
                Some(user_message_id),
                topic_id,
                &cancel_markup,
            )
            .await
        {
            Ok(status_msg) => {
                queued_status = Some(QueuedStatus {
                    chat: chat_id,
                    status: status_msg.id,
                    original: user_message_id,
                });

                // Only register in queue cancel map when status message succeeded,
                // so there's no orphan token if the status send fails.
                let queue_cancel_key: QueueCancelKey = (chat_id, user_message_id);
                {
                    let mut map = context.queue_cancel_map().lock().await;
                    map.insert(queue_cancel_key, cancel_token.clone());
                }
            }
            Err(err) => {
                eprintln!("Failed to send queued status: {err}");
            }
        }
    }

    let item = QueueItem {
        message,
        cancel_token,
        queued_status,
    };

    if let Err(err) = sender.send(item) {
        let item = err.0;
        {
            let mut queues = queues.lock().await;
            if let Some(state) = queues.get_mut(&key) {
                state.pending = state.pending.saturating_sub(1);
            }
        }
        let (sender, receiver) = mpsc::unbounded_channel();
        spawn_queue_worker(key, receiver, Arc::clone(context), Arc::clone(queues));
        {
            let mut queues = queues.lock().await;
            queues.insert(
                key,
                QueueState {
                    sender: sender.clone(),
                    pending: 1,
                },
            );
        }
        let _ = sender.send(item);
    }
}

fn spawn_queue_worker(
    key: QueueKey,
    mut receiver: mpsc::UnboundedReceiver<QueueItem>,
    context: Arc<BotContext>,
    queues: ChatQueueMap,
) {
    tokio::spawn(async move {
        while let Some(item) = receiver.recv().await {
            let QueueItem {
                message,
                cancel_token,
                queued_status,
            } = item;

            // Clean up queue cancel map entry
            if let Some(ref status) = queued_status {
                let queue_cancel_key: QueueCancelKey = (status.chat, status.original);
                let mut map = context.queue_cancel_map().lock().await;
                map.remove(&queue_cancel_key);
            }

            if cancel_token.is_cancelled() {
                // Item was cancelled while queued — update status and skip
                eprintln!("Skipping cancelled queued message for {key:?}");
                if let Some(status) = queued_status {
                    if let Err(err) = context
                        .client()
                        .edit_message_text(status.chat, status.status, "Cancelled ✓", None)
                        .await
                    {
                        eprintln!(
                            "Failed to edit cancelled queue status {}: {}",
                            status.status, err
                        );
                    }
                    // Best-effort: delete user's original message
                    if let Err(err) = context
                        .client()
                        .delete_message(status.chat, status.original)
                        .await
                    {
                        eprintln!(
                            "Failed to delete user message {} on queue cancel: {}",
                            status.original, err
                        );
                    }
                }
                continue;
            }

            // Not cancelled — about to process. Delete the "Queued" status
            // message (handle_message will send its own "Thinking..." status).
            if let Some(status) = queued_status
                && let Err(err) = context
                    .client()
                    .delete_message(status.chat, status.status)
                    .await
            {
                eprintln!(
                    "Failed to delete queued status message {}: {}",
                    status.status, err
                );
            }

            if let Err(err) = handle_message(context.as_ref(), message).await {
                eprintln!("Message handling error for {key:?}: {err}");
            }

            let mut queues = queues.lock().await;
            if let Some(state) = queues.get_mut(&key) {
                state.pending = state.pending.saturating_sub(1);
            }
        }
    });
}
