use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::{Mutex, mpsc};

use crate::bot::context::BotContext;
use crate::handlers::message::handle_message;
use crate::telegram::Message;

pub(crate) type ChatQueueMap = Arc<Mutex<HashMap<i64, mpsc::UnboundedSender<Message>>>>;

pub(crate) fn new_chat_queues() -> ChatQueueMap {
    Arc::new(Mutex::new(HashMap::new()))
}

pub(crate) async fn enqueue_message(queues: &ChatQueueMap, context: &BotContext, message: Message) {
    let chat_id = message.chat.id;
    let sender = {
        let mut queues = queues.lock().await;
        if let Some(sender) = queues.get(&chat_id) {
            sender.clone()
        } else {
            let (sender, receiver) = mpsc::unbounded_channel();
            spawn_chat_worker(chat_id, receiver, context.clone());
            queues.insert(chat_id, sender.clone());
            sender
        }
    };

    if let Err(err) = sender.send(message) {
        let message = err.0;
        let (sender, receiver) = mpsc::unbounded_channel();
        spawn_chat_worker(chat_id, receiver, context.clone());
        {
            let mut queues = queues.lock().await;
            queues.insert(chat_id, sender.clone());
        }
        let _ = sender.send(message);
    }
}

fn spawn_chat_worker(
    chat_id: i64,
    mut receiver: mpsc::UnboundedReceiver<Message>,
    context: BotContext,
) {
    tokio::spawn(async move {
        while let Some(message) = receiver.recv().await {
            if let Err(err) = handle_message(&context, message).await {
                eprintln!("Message handling error for chat {}: {}", chat_id, err);
            }
        }
    });
}
