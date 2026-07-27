//! Error-recovery buttons shown after a failed agent turn.
//!
//! When a turn fails without producing a reply (for example an
//! `overloaded_error`), the bot sends a separate message with a "Try again"
//! button. Tapping it re-runs the same turn from the already-persisted thread
//! state — no new user message is appended, so the failed request is retried
//! verbatim. Entries sit unanswered without blocking anything.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use crate::bot::context::BotContext;
use crate::telegram::{CallbackQuery, InlineKeyboardButton, InlineKeyboardMarkup, TelegramClient};

/// What a pending "Try again" button needs to re-run the failed turn.
#[allow(clippy::struct_field_names)] // these are distinct ids, not a shared prefix
pub(crate) struct RetryRequest {
    pub thread_id: String,
    pub topic_id: Option<i64>,
    pub reply_to_message_id: Option<i64>,
    /// The user's original message id, reused to key the new turn's status.
    pub user_message_id: i64,
}

/// Pending retry requests keyed by (`chat_id`, `message_id`) of the buttons
/// message. Entries are tiny and stay until tapped, dismissed, or restart.
pub(crate) type RetryMap = Arc<Mutex<HashMap<(i64, i64), RetryRequest>>>;

pub(crate) fn new_retry_map() -> RetryMap {
    Arc::new(Mutex::new(HashMap::new()))
}

/// Sends the "Try again" button and registers the retry request for lookup.
pub(crate) async fn send_retry_buttons(context: &BotContext, chat_id: i64, request: RetryRequest) {
    let markup = InlineKeyboardMarkup {
        inline_keyboard: vec![
            vec![InlineKeyboardButton {
                text: "🔄 Try again".to_string(),
                callback_data: Some("retry:go".to_string()),
                url: None,
            }],
            vec![InlineKeyboardButton {
                text: "✕ Dismiss".to_string(),
                callback_data: Some("retry:x".to_string()),
                url: None,
            }],
        ],
    };

    match context
        .client()
        .send_message_with_markup(
            chat_id,
            "🔁 <i>Retry the failed request?</i>",
            None,
            request.topic_id,
            &markup,
        )
        .await
    {
        Ok(message) => {
            let mut map = context.retry_map().lock().expect("retry lock poisoned");
            map.insert((chat_id, message.id), request);
        }
        Err(err) => {
            tracing::warn!(chat_id, %err, "Failed to send retry button");
        }
    }
}

/// Handles a `retry:{go|x}` callback.
pub(crate) async fn handle_callback(
    context: &Arc<BotContext>,
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

    // Dismiss: drop the request and delete the buttons message.
    if data == "x" {
        {
            let mut map = context.retry_map().lock().expect("retry lock poisoned");
            map.remove(&(chat_id, message.id));
        }
        let _ = client.delete_message(chat_id, message.id).await;
        let _ = client.answer_callback_query(&callback.id, None).await;
        return;
    }

    if data != "go" {
        let _ = client.answer_callback_query(&callback.id, None).await;
        return;
    }

    let request = {
        let mut map = context.retry_map().lock().expect("retry lock poisoned");
        map.remove(&(chat_id, message.id))
    };
    let Some(request) = request else {
        let _ = client
            .answer_callback_query(&callback.id, Some("This retry is no longer active"))
            .await;
        return;
    };

    let _ = client
        .edit_message_text(chat_id, message.id, "🔄 Retrying…", None)
        .await;
    let _ = client
        .answer_callback_query(&callback.id, Some("Retrying…"))
        .await;

    let context = Arc::clone(context);
    tokio::spawn(async move {
        if let Err(err) = crate::handlers::message::retry_agent_turn(
            &context,
            chat_id,
            request.user_message_id,
            &request.thread_id,
            request.topic_id,
            request.reply_to_message_id,
        )
        .await
        {
            tracing::error!(chat_id, %err, "Retry agent turn failed");
        }
    });
}
