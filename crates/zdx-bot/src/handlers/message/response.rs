use anyhow::Result;

use super::ReplyContext;
use super::media::{parse_final_response, send_media_responses};
use crate::bot::context::BotContext;

pub(super) async fn send_final_response(
    context: &BotContext,
    incoming: &crate::types::IncomingMessage,
    reply_ctx: &ReplyContext,
    status_message_id: Option<i64>,
    final_text: &str,
    thread_id: &str,
) -> Result<()> {
    let parsed = parse_final_response(final_text);
    let has_text = !parsed.text.trim().is_empty();

    // The reply is always a fresh message so its Telegram timestamp marks when
    // the turn actually ended and completion raises a notification (edits do not).
    if let Some(msg_id) = status_message_id
        && let Err(err) = context
            .client()
            .delete_message(incoming.chat_id, msg_id)
            .await
    {
        tracing::warn!(msg_id, %err, "Failed to delete status message");
    }

    if !has_text && parsed.media_paths.is_empty() && parsed.followups.is_empty() {
        return Ok(());
    }

    if has_text {
        let text = with_thread_link(context, incoming.chat_id, thread_id, parsed.text.as_str());
        send_text_response(context, incoming, reply_ctx, &text).await?;
    }

    send_media_responses(context, incoming, reply_ctx, &parsed.media_paths, has_text).await?;

    crate::followups::send_followups(
        context,
        incoming.chat_id,
        reply_ctx.topic_id,
        parsed.followups,
    )
    .await;
    Ok(())
}

/// Appends a subtle Mini App deep link for this thread to an answer.
///
/// Saves the trip through the pinned thread header just to reach the Mini App.
/// The link is skipped when the Mini App is not configured for this chat, and
/// the answer is sent with link previews disabled so it stays a plain line of
/// text instead of a preview card.
fn with_thread_link(context: &BotContext, chat_id: i64, thread_id: &str, text: &str) -> String {
    append_thread_link(
        super::mini_app_base_url(context, chat_id).as_deref(),
        thread_id,
        text,
    )
}

/// Pure half of [`with_thread_link`], so the rendered suffix is testable
/// without constructing a whole bot context.
fn append_thread_link(base: Option<&str>, thread_id: &str, text: &str) -> String {
    match base {
        Some(base) => {
            format!("{text}\n\n<a href=\"{base}?startapp={thread_id}\">↗ Open thread</a>")
        }
        None => text.to_string(),
    }
}

async fn send_text_response(
    context: &BotContext,
    incoming: &crate::types::IncomingMessage,
    reply_ctx: &ReplyContext,
    text: &str,
) -> Result<()> {
    tracing::info!(chat_id = incoming.chat_id, "Sending reply");

    if let Some(ref reply_parameters) = reply_ctx.cross_topic_reply_parameters {
        context
            .client()
            .send_message_with_reply_params_without_preview(
                incoming.chat_id,
                text,
                reply_ctx.topic_id,
                Some(reply_parameters.clone()),
            )
            .await?;
        return Ok(());
    }

    let send_result = context
        .client()
        .send_message_without_preview(
            incoming.chat_id,
            text,
            reply_ctx.reply_to_message_id,
            reply_ctx.topic_id,
        )
        .await;
    if let Err(ref e) = send_result {
        if e.to_string().contains("REPLY_MESSAGE_ID_INVALID") {
            context
                .client()
                .send_message_without_preview(incoming.chat_id, text, None, reply_ctx.topic_id)
                .await?;
        } else {
            send_result?;
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::append_thread_link;

    #[test]
    fn appends_a_deep_link_only_when_the_mini_app_is_configured() {
        assert_eq!(
            append_thread_link(
                Some("https://t.me/zdx_2026_bot/app"),
                "telegram--1001234567890-topic-17771",
                "done",
            ),
            "done\n\n<a href=\"https://t.me/zdx_2026_bot/app?startapp=telegram--1001234567890-topic-17771\">↗ Open thread</a>"
        );

        // Mini App disabled or unset: the answer must go out untouched.
        assert_eq!(
            append_thread_link(None, "telegram--100-topic-1", "done"),
            "done"
        );
    }
}
