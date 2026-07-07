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
) -> Result<()> {
    let parsed = parse_final_response(final_text);
    let has_text = !parsed.text.trim().is_empty();

    if !has_text && parsed.media_paths.is_empty() && parsed.followups.is_empty() {
        if let Some(msg_id) = status_message_id
            && let Err(err) = context
                .client()
                .delete_message(incoming.chat_id, msg_id)
                .await
        {
            tracing::warn!(msg_id, %err, "Failed to delete empty status message");
        }
        return Ok(());
    }

    if has_text {
        send_text_response(
            context,
            incoming,
            reply_ctx,
            status_message_id,
            parsed.text.as_str(),
        )
        .await?;
    } else if let Some(msg_id) = status_message_id
        && let Err(err) = context
            .client()
            .delete_message(incoming.chat_id, msg_id)
            .await
    {
        tracing::warn!(msg_id, %err, "Failed to delete empty status message");
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

async fn send_text_response(
    context: &BotContext,
    incoming: &crate::types::IncomingMessage,
    reply_ctx: &ReplyContext,
    status_message_id: Option<i64>,
    text: &str,
) -> Result<()> {
    tracing::info!(chat_id = incoming.chat_id, "Sending reply");

    if let Some(ref reply_parameters) = reply_ctx.cross_topic_reply_parameters {
        if let Some(msg_id) = status_message_id
            && let Err(err) = context
                .client()
                .delete_message(incoming.chat_id, msg_id)
                .await
        {
            tracing::warn!(msg_id, %err, "Failed to delete status message");
        }

        context
            .client()
            .send_message_with_reply_params(
                incoming.chat_id,
                text,
                reply_ctx.topic_id,
                Some(reply_parameters.clone()),
            )
            .await?;
        return Ok(());
    }

    if let Some(msg_id) = status_message_id {
        let edit_result = context
            .client()
            .edit_message_text(incoming.chat_id, msg_id, text, None)
            .await;
        if let Err(ref err) = edit_result {
            tracing::warn!(msg_id, chat_id = incoming.chat_id, %err, "Failed to edit status message");
            if let Err(del_err) = context
                .client()
                .delete_message(incoming.chat_id, msg_id)
                .await
            {
                tracing::warn!(msg_id, err = %del_err, "Failed to delete stale status message");
            }
            let send_result = context
                .client()
                .send_message(
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
                        .send_message(incoming.chat_id, text, None, reply_ctx.topic_id)
                        .await?;
                } else {
                    send_result?;
                }
            }
        }
    } else {
        let send_result = context
            .client()
            .send_message(
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
                    .send_message(incoming.chat_id, text, None, reply_ctx.topic_id)
                    .await?;
            } else {
                send_result?;
            }
        }
    }

    Ok(())
}

pub(super) fn normalize_reply_text(text: &str) -> String {
    let mut out = String::new();
    let mut prev_blank = false;

    for line in text.lines() {
        let is_blank = line.trim().is_empty();
        if is_blank && prev_blank {
            continue;
        }

        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(line);
        prev_blank = is_blank;
    }

    out.trim().to_string()
}
