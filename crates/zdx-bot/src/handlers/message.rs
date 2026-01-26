use anyhow::{Result, anyhow};
use zdx_core::core::thread_log::ThreadEvent;

use crate::bot::context::BotContext;
use crate::telegram::Message;
use crate::{agent, ingest};

pub(crate) async fn handle_message(context: &BotContext, message: Message) -> Result<()> {
    let Some(incoming) = ingest::parse_incoming_message(
        context.client(),
        context.allowlist(),
        context.config(),
        message,
    )
    .await?
    else {
        return Ok(());
    };

    eprintln!(
        "Accepted message from user {} in chat {}",
        incoming.user_id, incoming.chat_id
    );

    if incoming.images.is_empty()
        && incoming.audios.is_empty()
        && let Some(text) = incoming.text.as_deref()
        && is_new_command(text)
    {
        let thread_id = thread_id_for_chat(incoming.chat_id);
        agent::clear_thread_history(&thread_id)?;
        context
            .client()
            .send_message(
                incoming.chat_id,
                "History cleared. Start a new conversation anytime.",
                Some(incoming.message_id),
            )
            .await?;
        return Ok(());
    }

    let thread_id = thread_id_for_chat(incoming.chat_id);
    let (mut thread, mut messages) = agent::load_thread_state(&thread_id)?;
    agent::record_user_message(&mut thread, &mut messages, &incoming)?;

    let result = agent::run_agent_turn_with_persist(
        messages,
        context.config(),
        context.root(),
        context.system_prompt(),
        &thread_id,
        &thread,
        context.tool_config(),
    )
    .await;

    match result {
        Ok((final_text, _messages)) => {
            thread
                .append(&ThreadEvent::assistant_message(&final_text))
                .map_err(|_| anyhow!("Failed to append assistant message"))?;
            if !final_text.trim().is_empty() {
                eprintln!("Sending reply for chat {}", incoming.chat_id);
                context
                    .client()
                    .send_message(incoming.chat_id, &final_text, Some(incoming.message_id))
                    .await?;
            }
        }
        Err(err) => {
            eprintln!("Agent error: {}", err);
            let _ = context
                .client()
                .send_message(
                    incoming.chat_id,
                    "Sorry, something went wrong.",
                    Some(incoming.message_id),
                )
                .await;
        }
    }

    Ok(())
}

fn thread_id_for_chat(chat_id: i64) -> String {
    format!("telegram-{}", chat_id)
}

fn is_new_command(text: &str) -> bool {
    text.trim() == "/new"
}
