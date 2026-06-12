//! End-of-turn follow-up suggestion buttons.
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

const FOLLOWUPS_OPEN: &str = "<followups>";
const FOLLOWUPS_CLOSE: &str = "</followups>";
const FOLLOWUP_OPEN: &str = "<followup>";
const FOLLOWUP_CLOSE: &str = "</followup>";

const MAX_FOLLOWUPS: usize = 4;
const MAX_BUTTON_CHARS: usize = 64;

/// Suggestions awaiting a tap, keyed by (`chat_id`, `message_id`) of the
/// buttons message. Entries for ignored suggestions stay until restart;
/// they are tiny.
pub(crate) type FollowupMap = Arc<Mutex<HashMap<(i64, i64), Vec<String>>>>;

pub(crate) fn new_followup_map() -> FollowupMap {
    Arc::new(Mutex::new(HashMap::new()))
}

/// Extracts `<followups>` blocks from the final reply text.
///
/// Returns the cleaned text and the list of suggestions (capped at
/// `MAX_FOLLOWUPS`, deduplicated, empty items dropped).
pub(crate) fn extract_followups(input: &str) -> (String, Vec<String>) {
    let mut cleaned = String::new();
    let mut items = Vec::new();
    let mut cursor = 0;

    while let Some(start_rel) = input[cursor..].find(FOLLOWUPS_OPEN) {
        let start = cursor + start_rel;
        cleaned.push_str(&input[cursor..start]);

        let content_start = start + FOLLOWUPS_OPEN.len();
        let Some(close_rel) = input[content_start..].find(FOLLOWUPS_CLOSE) else {
            cleaned.push_str(&input[start..]);
            cursor = input.len();
            break;
        };
        let content_end = content_start + close_rel;
        collect_followup_items(&input[content_start..content_end], &mut items);
        cursor = content_end + FOLLOWUPS_CLOSE.len();
    }

    if cursor < input.len() {
        cleaned.push_str(&input[cursor..]);
    }

    items.truncate(MAX_FOLLOWUPS);
    (cleaned, items)
}

fn collect_followup_items(block: &str, items: &mut Vec<String>) {
    let mut cursor = 0;
    while let Some(start_rel) = block[cursor..].find(FOLLOWUP_OPEN) {
        let content_start = cursor + start_rel + FOLLOWUP_OPEN.len();
        let Some(close_rel) = block[content_start..].find(FOLLOWUP_CLOSE) else {
            break;
        };
        let content_end = content_start + close_rel;
        let item = block[content_start..content_end].trim();
        if !item.is_empty() && !items.iter().any(|existing| existing == item) {
            items.push(item.to_string());
        }
        cursor = content_end + FOLLOWUP_CLOSE.len();
    }
}

/// Sends the follow-up buttons message and registers the suggestions for
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
            "💡 <i>Next steps — tap or ignore:</i>",
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
    fn extracts_followups_block_and_cleans_text() {
        let input = "Reply text.\n\n<followups><followup>Run tests</followup><followup>Commit it</followup></followups>";
        let (cleaned, items) = extract_followups(input);
        assert_eq!(cleaned.trim(), "Reply text.");
        assert_eq!(
            items,
            vec!["Run tests".to_string(), "Commit it".to_string()]
        );
    }

    #[test]
    fn ignores_text_without_followups() {
        let (cleaned, items) = extract_followups("Just a reply.");
        assert_eq!(cleaned, "Just a reply.");
        assert!(items.is_empty());
    }

    #[test]
    fn dedupes_and_caps_items() {
        let input = "<followups>\
            <followup>A</followup><followup>A</followup>\
            <followup>B</followup><followup>C</followup>\
            <followup>D</followup><followup>E</followup>\
            </followups>";
        let (_, items) = extract_followups(input);
        assert_eq!(items, vec!["A", "B", "C", "D"]);
    }

    #[test]
    fn keeps_unclosed_block_as_text() {
        let input = "Reply. <followups><followup>A</followup>";
        let (cleaned, items) = extract_followups(input);
        assert_eq!(cleaned, input);
        assert!(items.is_empty());
    }

    #[test]
    fn truncates_long_button_labels() {
        let long = "x".repeat(80);
        let label = truncate_chars(&long, 64);
        assert_eq!(label.chars().count(), 64);
        assert!(label.ends_with('…'));
    }
}
