use anyhow::{Context, Result};
use zdx_engine::core::thread_persistence;

use super::status::current_thread_header_message;
use super::{resolve_effective_thread_id, thread_id_for_chat};
use crate::bot::context::BotContext;
use crate::telegram::{CallbackQuery, InlineKeyboardButton, InlineKeyboardMarkup, TelegramClient};

const REFRESH_CALLBACK: &str = "thread_header:refresh";

pub(crate) async fn post_thread_header(
    context: &BotContext,
    chat_id: i64,
    topic_id: i64,
    thread_id: &str,
) -> Result<()> {
    if !thread_persistence::thread_exists(thread_id) {
        let root = context.root_for_chat(chat_id).root;
        let mut thread = thread_persistence::Thread::with_id(thread_id.to_string())
            .context("open thread for header")?;
        thread
            .set_root_path(&root)
            .context("initialize thread for header")?;
    }

    let text = header_text(context, chat_id, thread_id).await?;
    let keyboard = header_keyboard(context, chat_id, thread_id);
    let message = context
        .client()
        .send_message_with_markup(chat_id, &text, None, Some(topic_id), &keyboard)
        .await
        .context("post thread header")?;

    if let Err(err) = context
        .client()
        .pin_message(chat_id, message.id, true)
        .await
    {
        tracing::warn!(
            chat_id,
            topic_id,
            message_id = message.id,
            %err,
            "Failed to pin thread header"
        );
    }

    Ok(())
}

pub(crate) async fn handle_callback(
    context: &BotContext,
    client: &TelegramClient,
    callback: &CallbackQuery,
) {
    let Some(message) = callback.message.as_ref() else {
        let _ = client
            .answer_callback_query(&callback.id, Some("No message context"))
            .await;
        return;
    };
    let Some(topic_id) = message.effective_thread_id() else {
        let _ = client
            .answer_callback_query(&callback.id, Some("This header is not inside a topic"))
            .await;
        return;
    };

    let chat_id = message.chat.id;
    let topic_thread_id = thread_id_for_chat(chat_id, Some(topic_id));
    let thread_id = resolve_effective_thread_id(&topic_thread_id);
    let result = async {
        let text = header_text(context, chat_id, &thread_id).await?;
        let keyboard = header_keyboard(context, chat_id, &thread_id);
        match client
            .edit_message_text(chat_id, message.id, &text, Some(&keyboard))
            .await
        {
            Ok(()) => Ok(()),
            Err(err) if is_message_not_modified(&err) => Ok(()),
            Err(err) => Err(err).context("refresh thread header"),
        }
    }
    .await;

    match result {
        Ok(()) => {
            let _ = client
                .answer_callback_query(&callback.id, Some("Thread status refreshed ✓"))
                .await;
        }
        Err(err) => {
            tracing::warn!(chat_id, topic_id, %err, "Failed to refresh thread header");
            let _ = client
                .answer_callback_query(&callback.id, Some("Couldn't refresh thread status"))
                .await;
        }
    }
}

fn is_message_not_modified(err: &anyhow::Error) -> bool {
    err.to_string().contains("message is not modified")
}

async fn header_text(context: &BotContext, chat_id: i64, thread_id: &str) -> Result<String> {
    let mut text = current_thread_header_message(context, chat_id, thread_id).await?;
    text.push_str("\n\n<i>Tap Refresh for current usage.</i>");
    Ok(text)
}

fn header_keyboard(context: &BotContext, chat_id: i64, thread_id: &str) -> InlineKeyboardMarkup {
    header_keyboard_for_url(
        super::mini_app_base_url(context, chat_id).as_deref(),
        thread_id,
    )
}

fn header_keyboard_for_url(mini_app_url: Option<&str>, thread_id: &str) -> InlineKeyboardMarkup {
    let mut rows = Vec::new();
    if let Some(mini_app_url) = mini_app_url {
        rows.push(vec![InlineKeyboardButton::url(
            "💬 Open Thread",
            format!("{mini_app_url}?startapp={thread_id}"),
        )]);
    }
    rows.push(vec![InlineKeyboardButton::callback(
        "↻ Refresh",
        REFRESH_CALLBACK,
    )]);
    InlineKeyboardMarkup {
        inline_keyboard: rows,
    }
}

#[cfg(test)]
mod tests {
    use anyhow::anyhow;

    use super::{REFRESH_CALLBACK, header_keyboard_for_url, is_message_not_modified};

    #[test]
    fn refresh_callback_fits_telegram_limit() {
        assert!(REFRESH_CALLBACK.len() <= 64);
    }

    #[test]
    fn configured_header_opens_effective_thread_and_refreshes() {
        let keyboard =
            header_keyboard_for_url(Some("https://t.me/zdx_bot/threads"), "source-thread-id");
        assert_eq!(keyboard.inline_keyboard.len(), 2);
        assert_eq!(
            keyboard.inline_keyboard[0][0].url.as_deref(),
            Some("https://t.me/zdx_bot/threads?startapp=source-thread-id")
        );
        assert_eq!(
            keyboard.inline_keyboard[1][0].callback_data.as_deref(),
            Some(REFRESH_CALLBACK)
        );
    }

    #[test]
    fn unconfigured_header_still_refreshes() {
        let keyboard = header_keyboard_for_url(None, "thread-id");
        assert_eq!(keyboard.inline_keyboard.len(), 1);
        assert_eq!(
            keyboard.inline_keyboard[0][0].callback_data.as_deref(),
            Some(REFRESH_CALLBACK)
        );
    }

    #[test]
    fn unchanged_refresh_is_successful() {
        assert!(is_message_not_modified(&anyhow!(
            "Telegram API error: message is not modified"
        )));
        assert!(!is_message_not_modified(&anyhow!(
            "Telegram API error: message can't be edited"
        )));
    }
}
