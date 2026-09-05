//! Outbound Telegram Bot API calls: create a forum topic, send a message, send
//! a document.
//!
//! This is the shared core behind the `zdx telegram` CLI subcommands and the
//! native `telegram` tool, so both resolve tokens and talk to Telegram the same
//! way. It is deliberately narrow: only the three outbound operations those
//! surfaces expose.
//!
//! The Telegram bot runtime (`zdx-bot`) keeps its own richer client for the
//! long-poll loop, message editing, callbacks, and media types; that client
//! serves a different job and is not used here.

use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::config::Config;

const CONNECT_TIMEOUT_SECS: u64 = 10;
const HTTP_TIMEOUT_SECS: u64 = 60;

/// Telegram's hard limit on a message body.
pub const MAX_MESSAGE_CHARS: usize = 4096;
/// Telegram's hard limit on a document caption.
pub const MAX_CAPTION_CHARS: usize = 1024;

/// Message formatting mode accepted by the CLI and the tool.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParseMode {
    Html,
    Markdown,
    MarkdownV2,
    Plain,
}

impl ParseMode {
    /// Parses a user-facing parse-mode name.
    ///
    /// # Errors
    /// Returns an error if the name is not recognized.
    pub fn parse(value: &str) -> Result<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "html" => Ok(Self::Html),
            "markdown" => Ok(Self::Markdown),
            "markdown-v2" | "markdownv2" => Ok(Self::MarkdownV2),
            "plain" | "none" => Ok(Self::Plain),
            other => bail!(
                "invalid parse mode: {other} (expected html, markdown, markdown-v2, or plain)"
            ),
        }
    }

    #[must_use]
    fn api_value(self) -> Option<&'static str> {
        match self {
            Self::Html => Some("HTML"),
            Self::Markdown => Some("Markdown"),
            Self::MarkdownV2 => Some("MarkdownV2"),
            Self::Plain => None,
        }
    }
}

/// Minimal outbound Telegram Bot API client.
#[derive(Clone)]
pub struct TelegramOutbound {
    http: reqwest::Client,
    base_url: String,
    token: String,
}

impl TelegramOutbound {
    #[must_use]
    pub fn new(token: String) -> Self {
        let http = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(CONNECT_TIMEOUT_SECS))
            .timeout(Duration::from_secs(HTTP_TIMEOUT_SECS))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        Self {
            http,
            base_url: "https://api.telegram.org".to_string(),
            token,
        }
    }

    /// Creates a forum topic and returns its `message_thread_id`.
    ///
    /// # Errors
    /// Returns an error if the name is empty or the request fails.
    pub async fn create_forum_topic(&self, chat_id: i64, name: &str) -> Result<i64> {
        let name = name.trim();
        if name.is_empty() {
            bail!("topic name must not be empty");
        }

        let value: Value = self
            .post(
                "createForumTopic",
                &serde_json::json!({
                    "chat_id": chat_id,
                    "name": name,
                }),
            )
            .await?;

        value
            .get("message_thread_id")
            .and_then(Value::as_i64)
            .context("Telegram createForumTopic response had no message_thread_id")
    }

    /// Sends a text message.
    ///
    /// # Errors
    /// Returns an error if the text is empty, exceeds Telegram's limit, or the
    /// request fails.
    pub async fn send_message(
        &self,
        chat_id: i64,
        text: &str,
        message_thread_id: Option<i64>,
        parse_mode: ParseMode,
    ) -> Result<()> {
        let text = text.trim();
        if text.is_empty() {
            bail!("text must not be empty");
        }
        let chars = text.chars().count();
        if chars > MAX_MESSAGE_CHARS {
            bail!(
                "message is {chars} characters; Telegram's limit is {MAX_MESSAGE_CHARS}. Split it into several messages by section, or send a file instead"
            );
        }

        let mut body = serde_json::json!({ "chat_id": chat_id, "text": text });
        if let Some(thread) = message_thread_id {
            body["message_thread_id"] = thread.into();
        }
        if let Some(mode) = parse_mode.api_value() {
            body["parse_mode"] = mode.into();
        }

        let _: Value = self.post("sendMessage", &body).await?;
        Ok(())
    }

    /// Sends a local file as a document.
    ///
    /// # Errors
    /// Returns an error if the file is missing, the caption is too long, or the
    /// request fails.
    pub async fn send_document(
        &self,
        chat_id: i64,
        path: &Path,
        caption: Option<&str>,
        message_thread_id: Option<i64>,
    ) -> Result<()> {
        if !path.is_file() {
            bail!("file not found: {}", path.display());
        }
        if let Some(caption) = caption {
            let chars = caption.chars().count();
            if chars > MAX_CAPTION_CHARS {
                bail!(
                    "caption is {chars} characters; Telegram's limit is {MAX_CAPTION_CHARS}. Put the detail in a separate message instead"
                );
            }
        }

        let bytes =
            std::fs::read(path).with_context(|| format!("read document {}", path.display()))?;
        let file_name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("document.bin")
            .to_string();
        let mime = infer::get(&bytes).map_or_else(
            || "application/octet-stream".to_string(),
            |kind| kind.mime_type().to_string(),
        );

        let part = reqwest::multipart::Part::bytes(bytes)
            .file_name(file_name)
            .mime_str(&mime)
            .context("build Telegram document part")?;

        let mut form = reqwest::multipart::Form::new()
            .text("chat_id", chat_id.to_string())
            .part("document", part);
        if let Some(caption) = caption.map(str::trim).filter(|c| !c.is_empty()) {
            form = form.text("caption", caption.to_string());
        }
        if let Some(thread) = message_thread_id {
            form = form.text("message_thread_id", thread.to_string());
        }

        let url = self.url("sendDocument");
        let response = self
            .http
            .post(url)
            .multipart(form)
            .send()
            .await
            .context("Telegram sendDocument request failed")?;
        let bytes = response
            .bytes()
            .await
            .context("read Telegram sendDocument response")?;
        let _: Value = parse_response("sendDocument", &bytes)?;
        Ok(())
    }

    fn url(&self, method: &str) -> String {
        format!("{}/bot{}/{}", self.base_url, self.token, method)
    }

    async fn post<T: DeserializeOwned, B: Serialize>(&self, method: &str, body: &B) -> Result<T> {
        let response = self
            .http
            .post(self.url(method))
            .json(body)
            .send()
            .await
            .with_context(|| format!("Telegram request failed for {method}"))?;
        let bytes = response
            .bytes()
            .await
            .with_context(|| format!("read Telegram response for {method}"))?;
        parse_response(method, &bytes)
    }
}

fn parse_response<T: DeserializeOwned>(method: &str, bytes: &[u8]) -> Result<T> {
    let value: Value = serde_json::from_slice(bytes)
        .with_context(|| format!("parse Telegram response for {method}"))?;

    if value.get("ok").and_then(Value::as_bool) != Some(true) {
        let description = value
            .get("description")
            .and_then(Value::as_str)
            .unwrap_or("unknown error");
        bail!("Telegram API error on {method}: {description}");
    }

    let result = value.get("result").cloned().unwrap_or(Value::Bool(true));
    serde_json::from_value(result).with_context(|| format!("decode Telegram result for {method}"))
}

/// Resolves the bot token from an explicit override, config, then environment.
///
/// # Errors
/// Returns an error when no token is configured.
pub fn resolve_bot_token(config: &Config, override_token: Option<&str>) -> Result<String> {
    let candidates = [
        override_token.map(str::to_string),
        config.telegram.bot_token.clone(),
        std::env::var("ZDX_TELEGRAM_BOT_TOKEN").ok(),
        std::env::var("TELEGRAM_BOT_TOKEN").ok(),
    ];

    for candidate in candidates.into_iter().flatten() {
        let trimmed = candidate.trim();
        if !trimmed.is_empty() {
            return Ok(trimmed.to_string());
        }
    }

    bail!(
        "Telegram bot token is required (set telegram.bot_token, ZDX_TELEGRAM_BOT_TOKEN, or TELEGRAM_BOT_TOKEN)"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_known_parse_modes() {
        assert_eq!(ParseMode::parse("html").unwrap(), ParseMode::Html);
        assert_eq!(ParseMode::parse("HTML").unwrap(), ParseMode::Html);
        assert_eq!(
            ParseMode::parse("markdown-v2").unwrap(),
            ParseMode::MarkdownV2
        );
        assert_eq!(ParseMode::parse("plain").unwrap(), ParseMode::Plain);
        assert!(ParseMode::parse("rst").is_err());
    }

    #[test]
    fn plain_sends_no_parse_mode() {
        assert_eq!(ParseMode::Plain.api_value(), None);
        assert_eq!(ParseMode::Html.api_value(), Some("HTML"));
    }

    #[test]
    fn surfaces_telegram_api_errors() {
        let body = br#"{"ok":false,"description":"chat not found"}"#;
        let err = parse_response::<Value>("sendMessage", body).unwrap_err();
        assert!(format!("{err:#}").contains("chat not found"));
    }

    #[test]
    fn decodes_ok_results() {
        let body = br#"{"ok":true,"result":{"message_thread_id":42}}"#;
        let value: Value = parse_response("createForumTopic", body).unwrap();
        assert_eq!(value["message_thread_id"], 42);
    }
}
