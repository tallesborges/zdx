//! Suggested-reply buttons using the internal `<followups>` protocol.
//!
//! The model appends a `<followups>` block to its final reply; the bot strips
//! it, sends a separate message with one inline button per suggestion, and a
//! tap dispatches that suggestion as the user's next message (a normal new
//! turn). Nothing blocks while the buttons sit unanswered.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use serde_json::json;

use crate::bot::context::BotContext;
use crate::bot::queue::{ChatQueueMap, dispatch_message};
use crate::telegram::{CallbackQuery, InlineKeyboardButton, InlineKeyboardMarkup, TelegramClient};

const MAX_BUTTON_CHARS: usize = 64;

/// Suggestions awaiting a tap, keyed by (`chat_id`, `message_id`) of the
/// buttons message. Entries for ignored suggestions stay until restart;
/// they are tiny.
pub(crate) type FollowupMap = Arc<Mutex<HashMap<(i64, i64), Vec<String>>>>;

pub(crate) fn new_followup_map() -> FollowupMap {
    Arc::new(Mutex::new(HashMap::new()))
}

/// Sends the suggested-replies message and registers the options for
/// callback lookup.
pub(crate) async fn send_followups(
    context: &BotContext,
    chat_id: i64,
    topic_id: Option<i64>,
    items: Vec<String>,
) {
    if items.is_empty() {
        return;
    }

    let mut rows: Vec<Vec<InlineKeyboardButton>> = items
        .iter()
        .enumerate()
        .map(|(idx, item)| {
            vec![InlineKeyboardButton {
                text: truncate_chars(item, MAX_BUTTON_CHARS),
                callback_data: Some(format!("fu:{idx}")),
                url: None,
            }]
        })
        .collect();
    rows.push(vec![InlineKeyboardButton {
        text: "✕ Dismiss".to_string(),
        callback_data: Some("fu:x".to_string()),
        url: None,
    }]);
    let markup = InlineKeyboardMarkup {
        inline_keyboard: rows,
    };

    match context
        .client()
        .send_message_with_markup(
            chat_id,
            "💡 <i>Suggested replies</i>",
            None,
            topic_id,
            &markup,
        )
        .await
    {
        Ok(message) => {
            let mut map = context
                .followup_map()
                .lock()
                .expect("followup lock poisoned");
            map.insert((chat_id, message.id), items);
        }
        Err(err) => {
            tracing::warn!(chat_id, %err, "Failed to send follow-up buttons");
        }
    }
}

/// Handles a `fu:{idx}` callback: dispatches the tapped suggestion as the
/// user's next message.
pub(crate) async fn handle_callback(
    context: &Arc<BotContext>,
    queues: &ChatQueueMap,
    client: &TelegramClient,
    callback: &CallbackQuery,
    data: &str,
) {
    let Some(message) = callback.message.as_ref() else {
        let _ = client
            .answer_callback_query(&callback.id, Some("No message context"))
            .await;
        return;
    };
    let chat_id = message.chat.id;

    // Dismiss: delete the suggestions message without starting a turn.
    if data == "x" {
        {
            let mut map = context
                .followup_map()
                .lock()
                .expect("followup lock poisoned");
            map.remove(&(chat_id, message.id));
        }
        let _ = client.delete_message(chat_id, message.id).await;
        let _ = client.answer_callback_query(&callback.id, None).await;
        return;
    }

    let Some(idx) = data.parse::<usize>().ok() else {
        let _ = client.answer_callback_query(&callback.id, None).await;
        return;
    };

    let item = {
        let mut map = context
            .followup_map()
            .lock()
            .expect("followup lock poisoned");
        map.remove(&(chat_id, message.id))
            .and_then(|mut items| (idx < items.len()).then(|| items.swap_remove(idx)))
    };
    let Some(item) = item else {
        let _ = client
            .answer_callback_query(&callback.id, Some("These suggestions are no longer active"))
            .await;
        return;
    };

    let _ = client
        .edit_message_text(
            chat_id,
            message.id,
            &format!("▶️ {}", crate::handlers::message::escape_html(&item)),
            None,
        )
        .await;
    let _ = client.answer_callback_query(&callback.id, None).await;

    let chat_kind = if message.chat.is_private() {
        "private"
    } else {
        "supergroup"
    };
    let synthetic: Result<crate::telegram::Message, _> = serde_json::from_value(json!({
        "message_id": message.id,
        "chat": {
            "id": chat_id,
            "type": chat_kind,
            "is_forum": message.chat.is_forum_enabled(),
        },
        "from": { "id": callback.from.id, "is_bot": false },
        "text": item,
        "message_thread_id": message.effective_thread_id(),
    }));
    match synthetic {
        Ok(synthetic) => dispatch_message(queues, context, synthetic).await,
        Err(err) => {
            tracing::error!(chat_id, %err, "Failed to synthesize follow-up message");
        }
    }
}

fn truncate_chars(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let truncated: String = text.chars().take(max_chars.saturating_sub(1)).collect();
    format!("{truncated}…")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncates_long_button_labels() {
        let long = "x".repeat(80);
        let label = truncate_chars(&long, 64);
        assert_eq!(label.chars().count(), 64);
        assert!(label.ends_with('…'));
    }
}
