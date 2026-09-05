//! Telegram command handlers.
//!
//! Thin CLI shim over `zdx_engine::telegram`, the shared outbound core also
//! used by the native `telegram` tool.

use anyhow::Result;
use zdx_engine::config::Config;
use zdx_engine::telegram::{ParseMode, TelegramOutbound, resolve_bot_token};

fn client(config: &Config, bot_token: Option<&str>) -> Result<TelegramOutbound> {
    let token = resolve_bot_token(config, bot_token)?;
    Ok(TelegramOutbound::new(token))
}

pub async fn create_topic(
    config: &Config,
    bot_token: Option<String>,
    chat_id: i64,
    name: &str,
) -> Result<()> {
    let message_thread_id = client(config, bot_token.as_deref())?
        .create_forum_topic(chat_id, name)
        .await?;
    println!("{message_thread_id}");
    Ok(())
}

pub async fn send_message(
    config: &Config,
    bot_token: Option<String>,
    chat_id: i64,
    message_thread_id: Option<i64>,
    text: &str,
    parse_mode: &str,
) -> Result<()> {
    client(config, bot_token.as_deref())?
        .send_message(
            chat_id,
            text,
            message_thread_id,
            ParseMode::parse(parse_mode)?,
        )
        .await?;
    println!("Sent message to Telegram.");
    Ok(())
}

pub async fn send_document(
    config: &Config,
    bot_token: Option<String>,
    chat_id: i64,
    message_thread_id: Option<i64>,
    path: &str,
    caption: Option<&str>,
) -> Result<()> {
    client(config, bot_token.as_deref())?
        .send_document(
            chat_id,
            std::path::Path::new(path),
            caption,
            message_thread_id,
        )
        .await?;
    println!("Sent document to Telegram.");
    Ok(())
}
