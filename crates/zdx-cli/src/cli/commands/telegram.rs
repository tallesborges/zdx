//! Telegram command handlers.

use anyhow::{Result, bail};
use zdx_bot::telegram::TelegramClient;
use zdx_core::config::Config;

pub async fn create_topic(
    config: &Config,
    bot_token: Option<String>,
    chat_id: i64,
    name: &str,
) -> Result<()> {
    let topic_name = name.trim();
    if topic_name.is_empty() {
        bail!("topic name must not be empty");
    }

    let token = resolve_bot_token(config, bot_token.as_deref())?;
    let client = TelegramClient::new(token);
    let message_thread_id = client.create_forum_topic(chat_id, topic_name).await?;
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
    let body = text.trim();
    if body.is_empty() {
        bail!("text must not be empty");
    }

    let token = resolve_bot_token(config, bot_token.as_deref())?;
    let client = TelegramClient::new(token);
    let parse_mode = resolve_parse_mode(parse_mode)?;
    match parse_mode {
        ParseMode::Markdown => {
            // Keep legacy behavior: try Markdown first, then fallback to plain text.
            client
                .send_message(chat_id, body, None, message_thread_id)
                .await?;
        }
        ParseMode::MarkdownV2 => {
            client
                .send_message_with_parse_mode(
                    chat_id,
                    body,
                    None,
                    message_thread_id,
                    Some("MarkdownV2"),
                )
                .await?;
        }
        ParseMode::Html => {
            client
                .send_message_with_parse_mode(chat_id, body, None, message_thread_id, Some("HTML"))
                .await?;
        }
        ParseMode::Plain => {
            client
                .send_message_with_parse_mode(chat_id, body, None, message_thread_id, None)
                .await?;
        }
    }
    println!("Sent message to Telegram.");
    Ok(())
}

enum ParseMode {
    Markdown,
    MarkdownV2,
    Html,
    Plain,
}

fn resolve_parse_mode(parse_mode: &str) -> Result<ParseMode> {
    match parse_mode {
        "markdown" => Ok(ParseMode::Markdown),
        "markdown-v2" => Ok(ParseMode::MarkdownV2),
        "html" => Ok(ParseMode::Html),
        "plain" => Ok(ParseMode::Plain),
        _ => bail!("invalid parse mode: {parse_mode}"),
    }
}

pub async fn send_document(
    config: &Config,
    bot_token: Option<String>,
    chat_id: i64,
    message_thread_id: Option<i64>,
    path: &str,
    caption: Option<&str>,
) -> Result<()> {
    let file_path = std::path::Path::new(path);
    if !file_path.is_file() {
        bail!("file not found: {path}");
    }

    let token = resolve_bot_token(config, bot_token.as_deref())?;
    let client = TelegramClient::new(token);
    client
        .send_document_from_path(chat_id, file_path, caption, None, message_thread_id, None)
        .await?;
    println!("Sent document to Telegram.");
    Ok(())
}

fn resolve_bot_token(config: &Config, override_token: Option<&str>) -> Result<String> {
    if let Some(token) = normalize_optional(override_token) {
        return Ok(token);
    }

    if let Some(token) = normalize_optional(config.telegram.bot_token.as_deref()) {
        return Ok(token);
    }

    if let Some(token) = std::env::var("ZDX_TELEGRAM_BOT_TOKEN")
        .ok()
        .as_deref()
        .and_then(normalize_string)
    {
        return Ok(token);
    }

    if let Some(token) = std::env::var("TELEGRAM_BOT_TOKEN")
        .ok()
        .as_deref()
        .and_then(normalize_string)
    {
        return Ok(token);
    }

    bail!(
        "Telegram bot token is required (use --bot-token or set telegram.bot_token / ZDX_TELEGRAM_BOT_TOKEN / TELEGRAM_BOT_TOKEN)"
    )
}

fn normalize_optional(input: Option<&str>) -> Option<String> {
    input.and_then(normalize_string)
}

fn normalize_string(value: &str) -> Option<String> {
    let trimmed = value.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}
