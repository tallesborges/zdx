//! Telegram command handlers.
//!
//! Thin CLI shim over `zdx_engine::telegram`, the shared outbound core also
//! used by the native `telegram` tool.

use anyhow::Result;
use zdx_engine::config::Config;
use zdx_engine::telegram::{
    ParseMode, PostDetails, SentMessage, TelegramOutbound, record_post, resolve_bot_token,
};

fn client(config: &Config, bot_token: Option<&str>) -> Result<TelegramOutbound> {
    let token = resolve_bot_token(config, bot_token)?;
    Ok(TelegramOutbound::new(token))
}

/// Thread of the agent run calling this command, when it runs inside one.
fn source_thread_id() -> Option<String> {
    std::env::var("ZDX_THREAD_ID")
        .ok()
        .filter(|id| !id.trim().is_empty())
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

#[allow(clippy::too_many_arguments)]
pub async fn send_message(
    config: &Config,
    bot_token: Option<String>,
    chat_id: i64,
    message_thread_id: Option<i64>,
    text: &str,
    parse_mode: &str,
    context: Option<&str>,
    json: bool,
) -> Result<()> {
    let sent = client(config, bot_token.as_deref())?
        .send_message(
            chat_id,
            text,
            message_thread_id,
            ParseMode::parse(parse_mode)?,
        )
        .await?;
    let source = source_thread_id();
    let recorded = record_post(
        &sent,
        PostDetails {
            text: Some(text.trim()),
            context,
            source_thread_id: source.as_deref(),
            ..PostDetails::default()
        },
    );
    print_receipt("message", &sent, &recorded, json)
}

#[allow(clippy::too_many_arguments)]
pub async fn send_document(
    config: &Config,
    bot_token: Option<String>,
    chat_id: i64,
    message_thread_id: Option<i64>,
    path: &str,
    caption: Option<&str>,
    context: Option<&str>,
    json: bool,
) -> Result<()> {
    let path = std::path::Path::new(path);
    let sent = client(config, bot_token.as_deref())?
        .send_document(chat_id, path, caption, message_thread_id)
        .await?;
    let source = source_thread_id();
    let file = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
    let recorded = record_post(
        &sent,
        PostDetails {
            text: caption.map(str::trim),
            file: Some(&file),
            context,
            source_thread_id: source.as_deref(),
        },
    );
    print_receipt("document", &sent, &recorded, json)
}

/// Prints the receipt. A failed record is reported on stderr (and in the
/// JSON) but does not fail the command: the message is already delivered and
/// must not be resent.
fn print_receipt(
    kind: &str,
    sent: &SentMessage,
    recorded: &Result<Option<String>>,
    json: bool,
) -> Result<()> {
    if let Err(err) = recorded {
        eprintln!(
            "Warning: sent, but not recorded in the destination thread, so a reply there will not see it: {err:#}"
        );
    }
    if json {
        println!("{}", serde_json::to_string(&receipt_json(sent, recorded))?);
    } else {
        println!("{}", receipt_line(kind, sent));
    }
    Ok(())
}

fn receipt_json(sent: &SentMessage, recorded: &Result<Option<String>>) -> serde_json::Value {
    let mut value = serde_json::to_value(sent).unwrap_or_default();
    match recorded {
        Ok(thread_id) => value["recorded_in_thread"] = serde_json::json!(thread_id),
        Err(err) => {
            value["recorded_in_thread"] = serde_json::Value::Null;
            value["record_error"] = format!("{err:#}").into();
        }
    }
    value
}

fn receipt_line(kind: &str, sent: &SentMessage) -> String {
    match &sent.url {
        Some(url) => format!(
            "Sent {kind} to Telegram (message_id {}): {url}",
            sent.message_id
        ),
        None => format!("Sent {kind} to Telegram (message_id {}).", sent.message_id),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn receipt_line_reports_message_id_and_link() {
        let mut sent = SentMessage {
            chat_id: -1_001_234_567_890,
            message_thread_id: Some(17),
            message_id: 4242,
            url: Some("https://t.me/c/1234567890/17/4242".to_string()),
        };
        assert_eq!(
            receipt_line("message", &sent),
            "Sent message to Telegram (message_id 4242): https://t.me/c/1234567890/17/4242"
        );
        sent.url = None;
        assert_eq!(
            receipt_line("document", &sent),
            "Sent document to Telegram (message_id 4242)."
        );
    }

    #[test]
    fn json_receipt_has_stable_fields() {
        let sent = SentMessage {
            chat_id: -1_001_234_567_890,
            message_thread_id: None,
            message_id: 99,
            url: Some("https://t.me/c/1234567890/99".to_string()),
        };
        assert_eq!(
            receipt_json(&sent, &Ok(Some("telegram--1001234567890".to_string()))),
            serde_json::json!({
                "chat_id": -1_001_234_567_890_i64,
                "message_thread_id": null,
                "message_id": 99,
                "url": "https://t.me/c/1234567890/99",
                "recorded_in_thread": "telegram--1001234567890"
            })
        );
        let failed = receipt_json(&sent, &Err(anyhow::anyhow!("disk full")));
        assert_eq!(failed["recorded_in_thread"], serde_json::Value::Null);
        assert_eq!(failed["record_error"], "disk full");
    }
}
