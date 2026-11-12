//! Synthetic Telegram messages.
//!
//! Bot-generated prompts dispatched through a topic's normal per-topic queue,
//! exactly like a real user message (Mini App artifact feedback), and the id
//! counter shared with orchestrator turns the service starts on its own.

use std::sync::atomic::{AtomicI64, Ordering};

use serde_json::json;

/// Message ids for synthetic continuations. Counted down from `i64::MAX` so
/// they cannot collide with real Telegram ids, which the status and cancel maps
/// are keyed by.
static SYNTHETIC_MESSAGE_ID: AtomicI64 = AtomicI64::new(i64::MAX);

/// What produced a synthetic prompt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SyntheticKind {
    /// Annotations the user sent from the Mini App artifact preview. Runs as
    /// its own turn, like a user message.
    ArtifactFeedback,
}

/// A fresh synthetic message id: never a real Telegram id, so status and
/// cancel maps keyed by it cannot collide with a user's message.
pub(crate) fn next_synthetic_id() -> i64 {
    SYNTHETIC_MESSAGE_ID.fetch_sub(1, Ordering::Relaxed)
}

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

/// Wraps an already-parsed user message for the topic queue, keeping its real
/// id so the queue's status and cancel point at it. `None` when the value
/// does not parse (logged).
pub(crate) fn prepared_message(
    incoming: crate::types::IncomingMessage,
) -> Option<crate::telegram::Message> {
    let value = build_synthetic_value(
        incoming.chat_id,
        incoming.message_thread_id,
        incoming.user_id,
        incoming.message_id,
        "",
    );
    match serde_json::from_value::<crate::telegram::Message>(value) {
        Ok(mut message) => {
            message.prepared = Some(crate::telegram::PreparedMessage(Box::new(incoming)));
            Some(message)
        }
        Err(err) => {
            tracing::error!(%err, "Failed to queue a steer that missed its turn");
            None
        }
    }
}

/// Builds a synthetic user message with a fresh synthetic id. `None` when the
/// message value does not parse (logged).
pub(crate) fn build_synthetic_message(
    chat: i64,
    topic: Option<i64>,
    user: i64,
    prompt: &str,
    kind: SyntheticKind,
) -> Option<crate::telegram::Message> {
    let message_id = next_synthetic_id();
    let value = build_synthetic_value(chat, topic, user, message_id, prompt);
    match serde_json::from_value::<crate::telegram::Message>(value) {
        Ok(mut message) => {
            message.synthetic = Some(kind);
            Some(message)
        }
        Err(err) => {
            tracing::error!(%err, "Failed to build synthetic message");
            None
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
