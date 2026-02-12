use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use zdx_core::config::Config;
use zdx_core::core::events::ToolOutput;
use zdx_core::tools::{ToolContext, ToolDefinition, ToolHandler};

mod types;

#[allow(unused_imports)]
pub use types::{
    Audio, CallbackQuery, Chat, Document, InlineKeyboardButton, InlineKeyboardMarkup, Message,
    PhotoSize, TelegramFile, Update, User, Voice,
};

pub struct TelegramSettings {
    pub bot_token: String,
    pub allowlist_user_ids: HashSet<i64>,
    pub allowlist_chat_ids: HashSet<i64>,
}

impl TelegramSettings {
    ///
    /// # Errors
    /// Returns an error if the operation fails.
    pub fn from_config(config: &Config) -> Result<Self> {
        let token = config
            .telegram
            .bot_token
            .as_deref()
            .map(str::trim)
            .filter(|token| !token.is_empty())
            .map(str::to_string)
            .or_else(|| {
                std::env::var("ZDX_TELEGRAM_BOT_TOKEN")
                    .ok()
                    .map(|token| token.trim().to_string())
                    .filter(|token| !token.is_empty())
            })
            .unwrap_or_default();
        if token.is_empty() {
            bail!("telegram.bot_token or ZDX_TELEGRAM_BOT_TOKEN is required");
        }

        let allowlist_user_ids: HashSet<i64> =
            config.telegram.allowlist_user_ids.iter().copied().collect();
        if allowlist_user_ids.is_empty() {
            bail!("telegram.allowlist_user_ids must contain at least one user ID");
        }

        let allowlist_chat_ids: HashSet<i64> =
            config.telegram.allowlist_chat_ids.iter().copied().collect();

        Ok(Self {
            bot_token: token,
            allowlist_user_ids,
            allowlist_chat_ids,
        })
    }
}

#[derive(Clone)]
pub struct TelegramClient {
    http: reqwest::Client,
    base_url: String,
    token: String,
}

const TELEGRAM_PARSE_MODE: &str = "Markdown";
const TELEGRAM_CONNECT_TIMEOUT_SECS: u64 = 2;
const TELEGRAM_HTTP_TIMEOUT_SECS: u64 = 35;

impl TelegramClient {
    pub fn new(token: String) -> Self {
        let http = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(TELEGRAM_CONNECT_TIMEOUT_SECS))
            .timeout(Duration::from_secs(TELEGRAM_HTTP_TIMEOUT_SECS))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        Self {
            http,
            base_url: "https://api.telegram.org".to_string(),
            token,
        }
    }

    ///
    /// # Errors
    /// Returns an error if the operation fails.
    pub async fn get_updates(&self, offset: Option<i64>, timeout: Duration) -> Result<Vec<Update>> {
        let request = GetUpdatesRequest {
            offset,
            timeout: timeout.as_secs(),
            allowed_updates: Some(vec!["message", "callback_query"]),
        };
        self.post("getUpdates", &request).await
    }

    ///
    /// # Errors
    /// Returns an error if the operation fails.
    pub async fn get_file(&self, file_id: &str) -> Result<TelegramFile> {
        let request = GetFileRequest { file_id };
        self.post("getFile", &request).await
    }

    ///
    /// # Errors
    /// Returns an error if the operation fails.
    pub async fn download_file(&self, file_path: &str) -> Result<Vec<u8>> {
        let url = format!("{}/file/bot{}/{}", self.base_url, self.token, file_path);
        let response = self
            .http
            .get(url)
            .send()
            .await
            .context("Telegram file download")?;

        if !response.status().is_success() {
            bail!(
                "Telegram file download failed with status {}",
                response.status()
            );
        }

        let bytes = response.bytes().await.context("read Telegram file bytes")?;
        Ok(bytes.to_vec())
    }

    ///
    /// # Errors
    /// Returns an error if the operation fails.
    pub async fn send_message(
        &self,
        chat_id: i64,
        text: &str,
        reply_to_message_id: Option<i64>,
        message_thread_id: Option<i64>,
    ) -> Result<()> {
        self.send_message_inner(chat_id, text, reply_to_message_id, message_thread_id, None)
            .await
            .map(|_| ())
    }

    /// Send a message using an explicit parse mode.
    ///
    /// This method does not auto-fallback to plain text on parse errors.
    ///
    /// # Errors
    /// Returns an error if the operation fails.
    pub async fn send_message_with_parse_mode(
        &self,
        chat_id: i64,
        text: &str,
        reply_to_message_id: Option<i64>,
        message_thread_id: Option<i64>,
        parse_mode: Option<&str>,
    ) -> Result<()> {
        self.send_message_raw(
            chat_id,
            text,
            reply_to_message_id,
            message_thread_id,
            parse_mode,
            None,
        )
        .await
        .map(|_| ())
    }

    /// Send a message with an inline keyboard. Returns the sent [`Message`] so
    /// the caller can later edit or delete it by `message_id`.
    ///
    /// # Errors
    /// Returns an error if the operation fails.
    pub async fn send_message_with_markup(
        &self,
        chat_id: i64,
        text: &str,
        reply_to_message_id: Option<i64>,
        message_thread_id: Option<i64>,
        reply_markup: &InlineKeyboardMarkup,
    ) -> Result<Message> {
        self.send_message_inner(
            chat_id,
            text,
            reply_to_message_id,
            message_thread_id,
            Some(reply_markup),
        )
        .await
    }

    /// Inner send with Markdown-fallback-to-plain logic.
    async fn send_message_inner(
        &self,
        chat_id: i64,
        text: &str,
        reply_to_message_id: Option<i64>,
        message_thread_id: Option<i64>,
        reply_markup: Option<&InlineKeyboardMarkup>,
    ) -> Result<Message> {
        // First try with Markdown parse mode
        let result = self
            .send_message_raw(
                chat_id,
                text,
                reply_to_message_id,
                message_thread_id,
                Some(TELEGRAM_PARSE_MODE),
                reply_markup,
            )
            .await;

        // If Markdown parsing failed, retry as plain text
        if let Err(ref e) = result {
            let err_msg = e.to_string();
            if err_msg.contains("can't parse entities") || err_msg.contains("Can't find end of") {
                return self
                    .send_message_raw(
                        chat_id,
                        text,
                        reply_to_message_id,
                        message_thread_id,
                        None,
                        reply_markup,
                    )
                    .await;
            }
        }

        result
    }

    async fn send_message_raw(
        &self,
        chat_id: i64,
        text: &str,
        reply_to_message_id: Option<i64>,
        message_thread_id: Option<i64>,
        parse_mode: Option<&str>,
        reply_markup: Option<&InlineKeyboardMarkup>,
    ) -> Result<Message> {
        let request = SendMessageRequest {
            chat_id,
            text,
            message_thread_id,
            reply_to_message_id,
            allow_sending_without_reply: Some(true),
            parse_mode,
            reply_markup,
        };
        self.post("sendMessage", &request).await
    }

    /// Edit the text (and optionally the inline keyboard) of a bot message.
    ///
    /// # Errors
    /// Returns an error if the operation fails.
    pub async fn edit_message_text(
        &self,
        chat_id: i64,
        message_id: i64,
        text: &str,
        reply_markup: Option<&InlineKeyboardMarkup>,
    ) -> Result<()> {
        // Try Markdown first
        let result = self
            .edit_message_text_raw(
                chat_id,
                message_id,
                text,
                Some(TELEGRAM_PARSE_MODE),
                reply_markup,
            )
            .await;

        // Fallback to plain text on parse errors
        if let Err(ref e) = result {
            let err_msg = e.to_string();
            if err_msg.contains("can't parse entities") || err_msg.contains("Can't find end of") {
                return self
                    .edit_message_text_raw(chat_id, message_id, text, None, reply_markup)
                    .await;
            }
        }

        result
    }

    async fn edit_message_text_raw(
        &self,
        chat_id: i64,
        message_id: i64,
        text: &str,
        parse_mode: Option<&str>,
        reply_markup: Option<&InlineKeyboardMarkup>,
    ) -> Result<()> {
        let request = EditMessageTextRequest {
            chat_id,
            message_id,
            text,
            parse_mode,
            reply_markup,
        };
        let _: Value = self.post("editMessageText", &request).await?;
        Ok(())
    }

    /// Delete a message. Bot can delete its own messages anytime, and other
    /// users' messages if it has `can_delete_messages` admin right.
    ///
    /// # Errors
    /// Returns an error if the operation fails.
    pub async fn delete_message(&self, chat_id: i64, message_id: i64) -> Result<()> {
        let request = DeleteMessageRequest {
            chat_id,
            message_id,
        };
        let _: bool = self.post("deleteMessage", &request).await?;
        Ok(())
    }

    /// Acknowledge a callback query (dismisses the loading spinner on the
    /// button). Optionally show a notification to the user.
    ///
    /// # Errors
    /// Returns an error if the operation fails.
    pub async fn answer_callback_query(
        &self,
        callback_query_id: &str,
        text: Option<&str>,
    ) -> Result<()> {
        let request = AnswerCallbackQueryRequest {
            callback_query_id,
            text,
        };
        let _: bool = self.post("answerCallbackQuery", &request).await?;
        Ok(())
    }

    /// Create a forum topic in a supergroup.
    /// Returns the `message_thread_id` of the created topic.
    ///
    /// # Errors
    /// Returns an error if the operation fails.
    pub async fn create_forum_topic(&self, chat_id: i64, name: &str) -> Result<i64> {
        let request = CreateForumTopicRequest { chat_id, name };
        let topic: ForumTopic = self.post("createForumTopic", &request).await?;
        Ok(topic.message_thread_id)
    }

    ///
    /// # Errors
    /// Returns an error if the operation fails.
    pub async fn send_chat_action(
        &self,
        chat_id: i64,
        action: &str,
        message_thread_id: Option<i64>,
    ) -> Result<()> {
        let request = SendChatActionRequest {
            chat_id,
            action,
            message_thread_id,
        };
        let _: bool = self.post("sendChatAction", &request).await?;
        Ok(())
    }

    pub fn start_typing(&self, chat_id: i64, message_thread_id: Option<i64>) -> TypingIndicator {
        let client = self.clone();
        let cancel = tokio_util::sync::CancellationToken::new();
        let cancel_clone = cancel.clone();

        tokio::spawn(async move {
            loop {
                let _ = client
                    .send_chat_action(chat_id, "typing", message_thread_id)
                    .await;

                tokio::select! {
                    () = cancel_clone.cancelled() => break,
                    () = tokio::time::sleep(Duration::from_secs(4)) => {}
                }
            }
        });

        TypingIndicator { cancel }
    }

    async fn post<T: DeserializeOwned, B: Serialize>(&self, method: &str, body: &B) -> Result<T> {
        let url = format!("{}/bot{}/{}", self.base_url, self.token, method);
        let response = self
            .http
            .post(url)
            .json(body)
            .send()
            .await
            .context("Telegram request")?;

        // Read raw bytes so we can deserialize twice if needed
        let bytes = response.bytes().await.context("read Telegram response")?;

        // First check if request succeeded
        let envelope: TelegramEnvelope =
            serde_json::from_slice(&bytes).context("parse Telegram response envelope")?;

        if !envelope.ok {
            // Parse error response (no result field)
            let error: TelegramError =
                serde_json::from_slice(&bytes).context("decode Telegram error")?;
            let description = error
                .description
                .unwrap_or_else(|| "Telegram API error".to_string());
            bail!("{description}");
        }

        // Parse success response (has result field)
        let success: TelegramSuccess<T> =
            serde_json::from_slice(&bytes).context("decode Telegram result")?;

        Ok(success.result)
    }
}

#[derive(Debug, Deserialize)]
struct TelegramSendInput {
    chat_id: i64,
    text: String,
    #[serde(default)]
    reply_to_message_id: Option<i64>,
    #[serde(default)]
    message_thread_id: Option<i64>,
}

pub fn telegram_send_tool(client: TelegramClient) -> (ToolDefinition, ToolHandler) {
    let definition = ToolDefinition {
        name: "Telegram_Send".to_string(),
        description: "Send a Telegram message to a chat_id (DM or group/supergroup).".to_string(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "chat_id": {
                    "type": "integer",
                    "description": "Telegram chat ID (private DM or group/supergroup)"
                },
                "text": {
                    "type": "string",
                    "description": "Message text to send"
                },
                "reply_to_message_id": {
                    "type": "integer",
                    "description": "Optional message ID to reply to"
                },
                "message_thread_id": {
                    "type": "integer",
                    "description": "Optional forum topic ID (for supergroups with topics enabled)"
                }
            },
            "required": ["chat_id", "text"],
            "additionalProperties": false
        }),
    };

    let client = Arc::new(client);
    let handler: ToolHandler = Arc::new(move |input: &Value, ctx: &ToolContext| {
        let client = Arc::clone(&client);
        let input = input.clone();
        let timeout = ctx.timeout;
        Box::pin(async move {
            let parsed: TelegramSendInput = match serde_json::from_value(input) {
                Ok(parsed) => parsed,
                Err(err) => {
                    return ToolOutput::failure_with_details(
                        "invalid_input",
                        "Invalid input for Telegram_Send",
                        err.to_string(),
                    );
                }
            };

            let text = parsed.text.trim();
            if text.is_empty() {
                return ToolOutput::failure("invalid_input", "text must not be empty", None);
            }

            let send_future = client.send_message(
                parsed.chat_id,
                text,
                parsed.reply_to_message_id,
                parsed.message_thread_id,
            );

            let send_result = match timeout {
                Some(timeout) => match tokio::time::timeout(timeout, send_future).await {
                    Ok(result) => result,
                    Err(_) => {
                        return ToolOutput::failure(
                            "timeout",
                            format!(
                                "Tool execution timed out after {} seconds",
                                timeout.as_secs()
                            ),
                            Some(
                                "Consider breaking up large tasks or increasing the timeout"
                                    .to_string(),
                            ),
                        );
                    }
                },
                None => send_future.await,
            };

            match send_result {
                Ok(()) => ToolOutput::success(json!({
                    "sent": true,
                    "chat_id": parsed.chat_id,
                })),
                Err(err) => ToolOutput::failure_with_details(
                    "telegram_error",
                    "Failed to send Telegram message",
                    err.to_string(),
                ),
            }
        })
    });

    (definition, handler)
}

/// Telegram API success response (ok: true, result present)
#[derive(Debug, Deserialize)]
struct TelegramSuccess<T> {
    result: T,
}

/// Telegram API error response (ok: false, no result)
#[derive(Debug, Deserialize)]
struct TelegramError {
    #[serde(default)]
    description: Option<String>,
}

/// Raw envelope to check ok field first
#[derive(Debug, Deserialize)]
struct TelegramEnvelope {
    ok: bool,
}

#[derive(Debug, Serialize)]
struct GetUpdatesRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    offset: Option<i64>,
    timeout: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    allowed_updates: Option<Vec<&'static str>>,
}

#[derive(Debug, Serialize)]
struct SendMessageRequest<'a> {
    chat_id: i64,
    text: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    message_thread_id: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reply_to_message_id: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    allow_sending_without_reply: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    parse_mode: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reply_markup: Option<&'a InlineKeyboardMarkup>,
}

#[derive(Debug, Serialize)]
struct GetFileRequest<'a> {
    file_id: &'a str,
}

#[derive(Debug, Serialize)]
struct SendChatActionRequest<'a> {
    chat_id: i64,
    action: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    message_thread_id: Option<i64>,
}

#[derive(Debug, Serialize)]
struct CreateForumTopicRequest<'a> {
    chat_id: i64,
    name: &'a str,
}

#[derive(Debug, Deserialize)]
struct ForumTopic {
    message_thread_id: i64,
}

#[derive(Debug, Serialize)]
struct EditMessageTextRequest<'a> {
    chat_id: i64,
    message_id: i64,
    text: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    parse_mode: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reply_markup: Option<&'a InlineKeyboardMarkup>,
}

#[derive(Debug, Serialize)]
struct DeleteMessageRequest {
    chat_id: i64,
    message_id: i64,
}

#[derive(Debug, Serialize)]
struct AnswerCallbackQueryRequest<'a> {
    callback_query_id: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    text: Option<&'a str>,
}

pub struct TypingIndicator {
    cancel: tokio_util::sync::CancellationToken,
}

impl Drop for TypingIndicator {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}
