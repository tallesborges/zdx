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
/// - Forum General (no topic): spawn a standalone task (concurrent)
/// - Forum topic / DM: enqueue for sequential processing
pub(crate) async fn dispatch_message(
    queues: &ChatQueueMap,
    context: &Arc<BotContext>,
    message: Message,
) {
    let is_forum_general = message.chat.is_forum_enabled() && message.message_thread_id.is_none();

    if is_forum_general {
        spawn_standalone(Arc::clone(context), message);
    } else {
        enqueue_message(queues, context, message).await;
    }
}

/// Spawn a standalone task for a message (no queuing, fully concurrent).
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
