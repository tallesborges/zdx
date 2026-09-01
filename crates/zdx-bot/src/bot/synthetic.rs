//! Synthetic Telegram messages.
//!
//! Bot-generated prompts dispatched through a topic's normal per-topic queue,
//! exactly like a real user message. Shared by `/goal` continuations and by
//! orchestrator worker-completion callbacks so both use one id counter and one
//! dispatch path.

use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};

use serde_json::json;

use crate::bot::context::BotContext;
use crate::bot::queue::{ChatQueueMap, dispatch_message};

/// Message ids for synthetic continuations. Counted down from `i64::MAX` so
/// they cannot collide with real Telegram ids, which the status and cancel maps
/// are keyed by.
static SYNTHETIC_MESSAGE_ID: AtomicI64 = AtomicI64::new(i64::MAX);

/// Builds the synthetic Telegram message value. The chat type is derived from
/// the chat id sign (Telegram group/supergroup ids are negative, private chat
/// ids positive) so DM-thread callbacks are not misclassified as group
/// messages and dropped by the chat allowlist.
fn build_synthetic_value(
    chat: i64,
    topic: Option<i64>,
    user: i64,
    message_id: i64,
    prompt: &str,
) -> serde_json::Value {
    let is_group = chat < 0;
    let mut value = json!({
        "message_id": message_id,
        "chat": {
            "id": chat,
            "type": if is_group { "supergroup" } else { "private" },
            "is_forum": is_group && topic.is_some(),
        },
        "from": { "id": user, "is_bot": false },
        "text": prompt,
    });
    if let Some(topic) = topic {
        value["message_thread_id"] = json!(topic);
    }
    value
}

/// Builds a synthetic user message and dispatches it through the topic's
/// existing queue. Returns `false` when the message could not be built.
pub(crate) async fn dispatch_synthetic_prompt(
    context: &Arc<BotContext>,
    queues: &ChatQueueMap,
    chat: i64,
    topic: Option<i64>,
    user: i64,
    prompt: String,
) -> bool {
    let message_id = SYNTHETIC_MESSAGE_ID.fetch_sub(1, Ordering::Relaxed);
    let value = build_synthetic_value(chat, topic, user, message_id, &prompt);

    match serde_json::from_value::<crate::telegram::Message>(value) {
        Ok(message) => {
            dispatch_message(queues, context, message).await;
            true
        }
        Err(err) => {
            tracing::error!(%err, "Failed to build synthetic message");
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dm_thread_synthetic_message_stays_private() {
        let value = build_synthetic_value(777, Some(42), 777, i64::MAX - 1, "update");
        assert_eq!(value["chat"]["type"], "private");
        assert_eq!(value["chat"]["is_forum"], false);
        assert_eq!(value["message_thread_id"], 42);

        let message: crate::telegram::Message = serde_json::from_value(value).unwrap();
        assert!(message.chat.is_private());
        assert_eq!(message.effective_thread_id(), Some(42));
    }

    #[test]
    fn forum_topic_synthetic_message_stays_supergroup() {
        let value = build_synthetic_value(-100_123, Some(9), 777, i64::MAX - 2, "update");
        assert_eq!(value["chat"]["type"], "supergroup");
        assert_eq!(value["chat"]["is_forum"], true);
        assert_eq!(value["message_thread_id"], 9);
    }

    #[test]
    fn plain_dm_synthetic_message_has_no_thread() {
        let value = build_synthetic_value(777, None, 777, i64::MAX - 3, "update");
        assert_eq!(value["chat"]["type"], "private");
        assert_eq!(value["chat"]["is_forum"], false);
        assert!(value.get("message_thread_id").is_none());
    }
}
