use anyhow::{Context, Result};
use zdx_engine::core::thread_persistence;

use super::status::current_thread_header_message;
use super::{resolve_effective_thread_id, thread_id_for_chat};
use crate::bot::context::{BotContext, ThreadHeader};
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
    context.record_thread_header(
        thread_id,
        ThreadHeader {
            chat_id,
            message_id: message.id,
            rendered: rendered_header(&text, &keyboard),
        },
    );

    if let Err(err) = context
        .client()
        .pin_message(chat_id, message.id, true)
        .await
    {
        tracing::warn!(
            chat_id,
            topic_id,
            message_id = message.id,
            err = %format!("{err:#}"),
            "Failed to pin thread header"
        );
    }

    Ok(())
}

/// Re-renders a thread's pinned header in place, when this process posted
/// it. Used by the service bridge so an orchestrator's card tracks its workers
/// without a manual Refresh. Best-effort: unknown headers are no-ops, and a
/// render identical to the last one Telegram accepted sends no edit. Runs on
/// the immediate client, so a flood wait skips it (the next refresh catches
/// up) instead of stalling the service bridge.
pub(crate) async fn refresh_thread_header(context: &BotContext, thread_id: &str) {
    let Some(header) = context.thread_header(thread_id) else {
        return;
    };
    let ThreadHeader {
        chat_id,
        message_id,
        ..
    } = header;
    let result = async {
        let text = header_text(context, chat_id, thread_id).await?;
        let keyboard = header_keyboard(context, chat_id, thread_id);
        let rendered = rendered_header(&text, &keyboard);
        if rendered == header.rendered {
            return Ok(());
        }
        match context
            .client()
            .edit_message_text(chat_id, message_id, &text, Some(&keyboard))
            .await
        {
            Ok(()) => {}
            Err(err) if is_message_not_modified(&err) => {}
            Err(err) => return Err(err),
        }
        context.record_thread_header(
            thread_id,
            ThreadHeader {
                chat_id,
                message_id,
                rendered,
            },
        );
        Ok(())
    }
    .await;
    if let Err(err) = result {
        tracing::warn!(thread_id, message_id, err = %format!("{err:#}"), "Failed to refresh thread header");
    }
}

/// Comparison key for a header render: text plus keyboard.
fn rendered_header(text: &str, keyboard: &InlineKeyboardMarkup) -> String {
    format!(
        "{text}\n{}",
        serde_json::to_string(keyboard).unwrap_or_default()
    )
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
            Ok(()) => {}
            Err(err) if is_message_not_modified(&err) => {}
            Err(err) => return Err(err).context("refresh thread header"),
        }
        if context
            .thread_header(&thread_id)
            .is_some_and(|header| header.message_id == message.id)
        {
            context.record_thread_header(
                &thread_id,
                ThreadHeader {
                    chat_id,
                    message_id: message.id,
                    rendered: rendered_header(&text, &keyboard),
                },
            );
        }
        Ok(())
    }
    .await;

    match result {
        Ok(()) => {
            let _ = client
                .answer_callback_query(&callback.id, Some("Thread status refreshed ✓"))
                .await;
        }
        Err(err) => {
            tracing::warn!(chat_id, topic_id, err = %format!("{err:#}"), "Failed to refresh thread header");
            let _ = client
                .answer_callback_query(&callback.id, Some("Couldn't refresh thread status"))
                .await;
        }
    }
}

fn is_message_not_modified(err: &anyhow::Error) -> bool {
    crate::telegram::api_error_description(err)
        .is_some_and(|description| description.contains("message is not modified"))
}

async fn header_text(context: &BotContext, chat_id: i64, thread_id: &str) -> Result<String> {
    let mut text = current_thread_header_message(context, chat_id, thread_id).await?;
    text.push_str("\n\n<i>Tap Refresh for current usage.</i>");
    Ok(text)
}

fn header_keyboard(context: &BotContext, chat_id: i64, thread_id: &str) -> InlineKeyboardMarkup {
    let mini_app_url = super::mini_app_base_url(context, chat_id);
    let orchestrator_url = orchestrator_link(thread_id, mini_app_url.as_deref());
    header_keyboard_for_url(
        mini_app_url.as_deref(),
        thread_id,
        orchestrator_url.as_deref(),
    )
}

/// Link back to the orchestrator that owns `thread_id`, for worker threads:
/// the owner is the worker's persisted parent, linked as its Telegram topic
/// when it has one, else opened in the Mini App. `None` for non-workers and
/// when neither link form exists.
fn orchestrator_link(thread_id: &str, mini_app_url: Option<&str>) -> Option<String> {
    let parent = thread_persistence::read_thread_summary(thread_id)
        .ok()
        .flatten()?
        .parent_thread_id?;
    let is_orchestrator = thread_persistence::read_persistent_profile(&parent)
        .ok()
        .flatten()
        .as_deref()
        == Some(zdx_engine::subagents::ORCHESTRATOR_SUBAGENT_NAME);
    if !is_orchestrator {
        return None;
    }
    super::parse_topic_thread_id(&parent)
        .and_then(|(chat, topic)| crate::telegram::topic_link(chat, topic))
        .or_else(|| mini_app_url.map(|base| format!("{base}?startapp={parent}")))
}

fn header_keyboard_for_url(
    mini_app_url: Option<&str>,
    thread_id: &str,
    orchestrator_url: Option<&str>,
) -> InlineKeyboardMarkup {
    let mut rows = Vec::new();
    if let Some(mini_app_url) = mini_app_url {
        rows.push(vec![InlineKeyboardButton::url(
            "💬 Open Thread",
            format!("{mini_app_url}?startapp={thread_id}"),
        )]);
    }
    if let Some(orchestrator_url) = orchestrator_url {
        rows.push(vec![InlineKeyboardButton::url(
            "🎛 Open orchestrator",
            orchestrator_url,
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
    use zdx_engine::core::thread_persistence::Thread;

    use super::{
        REFRESH_CALLBACK, header_keyboard_for_url, is_message_not_modified, orchestrator_link,
        rendered_header,
    };

    #[test]
    fn refresh_callback_fits_telegram_limit() {
        assert!(REFRESH_CALLBACK.len() <= 64);
    }

    #[test]
    fn configured_header_opens_effective_thread_and_refreshes() {
        let keyboard = header_keyboard_for_url(
            Some("https://t.me/zdx_bot/threads"),
            "source-thread-id",
            None,
        );
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
    fn worker_header_links_back_to_its_orchestrator() {
        let keyboard = header_keyboard_for_url(
            Some("https://t.me/zdx_bot/threads"),
            "worker-id",
            Some("https://t.me/c/1/2"),
        );
        assert_eq!(keyboard.inline_keyboard.len(), 3);
        assert_eq!(keyboard.inline_keyboard[1][0].text, "🎛 Open orchestrator");
        assert_eq!(
            keyboard.inline_keyboard[1][0].url.as_deref(),
            Some("https://t.me/c/1/2")
        );
    }

    #[test]
    fn unconfigured_header_still_refreshes() {
        let keyboard = header_keyboard_for_url(None, "thread-id", None);
        assert_eq!(keyboard.inline_keyboard.len(), 1);
        assert_eq!(
            keyboard.inline_keyboard[0][0].callback_data.as_deref(),
            Some(REFRESH_CALLBACK)
        );
    }

    /// Only a worker whose persisted parent is an orchestrator gets the
    /// back-link: a group topic parent links as its `t.me/c` topic, a DM
    /// parent falls back to the Mini App, and a non-orchestrator parent
    /// (e.g. a subagent child) yields nothing.
    #[test]
    fn orchestrator_link_follows_the_persisted_parent() {
        let _home = zdx_engine::test_support::temp_zdx_home();
        let group_home = "telegram--1001234567890-topic-17771";
        let dm_home = "telegram-5678901234-topic-3088";
        for home in [group_home, dm_home] {
            let mut thread = Thread::with_id(home.to_string()).unwrap();
            thread
                .set_persistent_profile(zdx_engine::subagents::ORCHESTRATOR_SUBAGENT_NAME)
                .unwrap();
        }
        let mut plain = Thread::with_id("plain-parent".to_string()).unwrap();
        plain.set_root_path(std::path::Path::new("/tmp")).unwrap();

        let worker_of = |parent: &str| {
            let mut thread = Thread::with_id(format!("worker-of-{parent}")).unwrap();
            thread.set_origin(None, Some(parent.to_string()), None);
            thread.set_root_path(std::path::Path::new("/tmp")).unwrap();
            thread.id
        };
        let mini_app = Some("https://t.me/zdx_bot/app");

        assert_eq!(
            orchestrator_link(&worker_of(group_home), mini_app).as_deref(),
            Some("https://t.me/c/1234567890/17771")
        );
        assert_eq!(
            orchestrator_link(&worker_of(dm_home), mini_app).as_deref(),
            Some("https://t.me/zdx_bot/app?startapp=telegram-5678901234-topic-3088")
        );
        assert_eq!(orchestrator_link(&worker_of(dm_home), None), None);
        assert_eq!(
            orchestrator_link(&worker_of("plain-parent"), mini_app),
            None
        );
        assert_eq!(orchestrator_link("no-such-thread", mini_app), None);
    }

    fn api_error(description: &str) -> anyhow::Error {
        zdx_engine::telegram::flood::TelegramApiError {
            method: "editMessageText".to_string(),
            code: Some(400),
            description: description.to_string(),
            retry_after: None,
        }
        .into()
    }

    #[test]
    fn unchanged_refresh_is_successful() {
        assert!(is_message_not_modified(&api_error(
            "Bad Request: message is not modified"
        )));
        assert!(!is_message_not_modified(&api_error(
            "Bad Request: message can't be edited"
        )));
        assert!(!is_message_not_modified(&anyhow!(
            "local error mentioning message is not modified"
        )));
    }

    #[test]
    fn header_render_key_covers_text_and_keyboard() {
        let keyboard = header_keyboard_for_url(None, "thread-id", None);
        let other = header_keyboard_for_url(Some("https://t.me/b/app"), "thread-id", None);
        assert_eq!(
            rendered_header("a", &keyboard),
            rendered_header("a", &keyboard)
        );
        assert_ne!(
            rendered_header("a", &keyboard),
            rendered_header("b", &keyboard)
        );
        assert_ne!(
            rendered_header("a", &keyboard),
            rendered_header("a", &other)
        );
    }
}
