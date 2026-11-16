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

/// Prints the gateway settings as one JSON object (the token only with
/// `include_token`).
///
/// # Errors
/// Returns an error when the Telegram runtime is not validly configured.
pub fn settings(config: &Config, root: &std::path::Path, include_token: bool) -> Result<()> {
    let settings = zdx_engine::telegram::gateway_settings(config, root, include_token)?;
    println!("{}", serde_json::to_string(&settings)?);
    Ok(())
}

fn read_input(path: &std::path::Path) -> Result<String> {
    use anyhow::Context as _;
    if path.as_os_str() == "-" {
        std::io::read_to_string(std::io::stdin()).context("read stdin")
    } else {
        std::fs::read_to_string(path).with_context(|| format!("read {}", path.display()))
    }
}

/// `zdx telegram compose-text`: the CLI twin of `compose_user_text`.
///
/// # Errors
/// Returns an error when the input is not valid JSON.
pub fn compose_text(file: &std::path::Path) -> Result<()> {
    let input: zdx_engine::core::chat_turn::UserInput = serde_json::from_str(&read_input(file)?)?;
    let reply = zdx_engine::orchestration::protocol::Reply::Text {
        text: zdx_engine::core::chat_turn::compose_user_text(&input),
    };
    println!("{}", serde_json::to_string(&reply)?);
    Ok(())
}

/// `zdx telegram reply-parts`: the CLI twin of `reply_parts`.
///
/// # Errors
/// Returns an error when the input cannot be read.
pub fn reply_parts(file: &std::path::Path) -> Result<()> {
    let reply = zdx_engine::orchestration::protocol::Reply::ReplyParts(
        zdx_engine::core::chat_turn::reply_parts(&read_input(file)?),
    );
    println!("{}", serde_json::to_string(&reply)?);
    Ok(())
}

/// `zdx telegram prepare-image`: the CLI twin of `prepare_image`.
///
/// # Errors
/// Returns an error when the file cannot be read.
pub fn prepare_image(path: &std::path::Path) -> Result<()> {
    use anyhow::Context as _;
    let bytes = std::fs::read(path).with_context(|| format!("read {}", path.display()))?;
    let reply = zdx_engine::orchestration::protocol::Reply::PreparedImage(
        zdx_engine::core::chat_turn::prepare_inline_image(path, &bytes),
    );
    println!("{}", serde_json::to_string(&reply)?);
    Ok(())
}

/// `zdx telegram prepare-photo`: the CLI twin of `prepare_photo`.
///
/// # Errors
/// Returns an error when the file cannot be read or prepared.
pub fn prepare_photo(path: &std::path::Path) -> Result<()> {
    let reply = zdx_engine::orchestration::server::prepare_photo(path)?;
    println!("{}", serde_json::to_string(&reply)?);
    Ok(())
}

/// `zdx helper`: the CLI twin of `generate`, `btw_seed`, and `topic_start`.
/// They hold no service state, so this runs them here.
///
/// # Errors
/// Returns an error when the request cannot be read or parsed, is not one of
/// those ops, or fails.
pub async fn helper(file: &std::path::Path) -> Result<()> {
    use anyhow::{Context as _, bail};
    use zdx_engine::orchestration::helpers;
    use zdx_engine::orchestration::protocol::{Reply, Request};

    let text = if file.as_os_str() == "-" {
        std::io::read_to_string(std::io::stdin()).context("read stdin")?
    } else {
        std::fs::read_to_string(file).with_context(|| format!("read {}", file.display()))?
    };
    let request: Request = serde_json::from_str(&text).context("parse helper request")?;
    let reply = match request {
        Request::Generate(request) => Reply::Generated {
            text: helpers::generate(request).await?,
        },
        Request::BtwSeed {
            thread_id,
            question,
        } => Reply::Text {
            text: helpers::btw_seed(&thread_id, &question)?,
        },
        Request::TopicStart(request) => Reply::TopicStart(helpers::topic_start(&request)?),
        Request::SetChatModel { root, model } => Reply::ChatModel {
            selection: zdx_engine::orchestration::catalog::set_chat_model(&root, &model)?,
        },
        // The service knows its managed workers (never resumable, listed on
        // an orchestrator's card); without it nothing runs as a worker.
        request @ Request::Catalog(_) => {
            if let Some(client) = zdx_engine::orchestration::EngineClient::running() {
                zdx_engine::orchestration::client::ok(client.call(&request).await?)?
            } else {
                let Request::Catalog(query) = request else {
                    unreachable!()
                };
                Reply::Catalog {
                    body: zdx_engine::orchestration::catalog::catalog(query, None)?,
                }
            }
        }
        _ => bail!(
            "`zdx helper` takes a `generate`, `btw_seed`, `topic_start`, `catalog`, or `set_chat_model` request"
        ),
    };
    println!("{}", serde_json::to_string(&reply)?);
    Ok(())
}

/// `zdx telegram transcribe-audio`: the CLI twin of `transcribe_audio`.
///
/// # Errors
/// Returns an error when the file cannot be read or transcription fails.
pub async fn transcribe_audio(
    path: &std::path::Path,
    file_name: Option<String>,
    mime_type: Option<String>,
    root: &std::path::Path,
) -> Result<()> {
    let file_name = file_name.unwrap_or_else(|| {
        path.file_name().map_or_else(
            || "audio".to_string(),
            |name| name.to_string_lossy().into_owned(),
        )
    });
    let transcript = zdx_engine::orchestration::server::transcribe_audio(
        path,
        &file_name,
        mime_type.as_deref(),
        root,
    )
    .await?;
    let reply = zdx_engine::orchestration::protocol::Reply::Transcript { transcript };
    println!("{}", serde_json::to_string(&reply)?);
    Ok(())
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
