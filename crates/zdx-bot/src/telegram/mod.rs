use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Result, anyhow, bail};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use zdx_core::config::Config;
use zdx_core::core::events::ToolOutput;
use zdx_core::tools::{ToolContext, ToolDefinition, ToolHandler};

mod types;

#[allow(unused_imports)]
pub use types::{Audio, Chat, Document, Message, PhotoSize, TelegramFile, Update, User, Voice};

pub struct TelegramSettings {
    pub bot_token: String,
    pub allowlist_user_ids: HashSet<i64>,
}

impl TelegramSettings {
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

        Ok(Self {
            bot_token: token,
            allowlist_user_ids,
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

impl TelegramClient {
    pub fn new(token: String) -> Self {
        Self {
            http: reqwest::Client::new(),
            base_url: "https://api.telegram.org".to_string(),
            token,
        }
    }

    pub async fn get_updates(&self, offset: Option<i64>, timeout: Duration) -> Result<Vec<Update>> {
        let request = GetUpdatesRequest {
            offset,
            timeout: timeout.as_secs(),
            allowed_updates: Some(vec!["message"]),
        };
        self.post("getUpdates", &request).await
    }

    pub async fn get_file(&self, file_id: &str) -> Result<TelegramFile> {
        let request = GetFileRequest { file_id };
        self.post("getFile", &request).await
    }

    pub async fn download_file(&self, file_path: &str) -> Result<Vec<u8>> {
        let url = format!("{}/file/bot{}/{}", self.base_url, self.token, file_path);
        let response = self
            .http
            .get(url)
            .send()
            .await
            .map_err(|_| anyhow!("Telegram file download failed"))?;

        if !response.status().is_success() {
            bail!(
                "Telegram file download failed with status {}",
                response.status()
            );
        }

        let bytes = response
            .bytes()
            .await
            .map_err(|_| anyhow!("Failed to read Telegram file bytes"))?;
        Ok(bytes.to_vec())
    }

    pub async fn send_message(
        &self,
        chat_id: i64,
        text: &str,
        reply_to_message_id: Option<i64>,
    ) -> Result<()> {
        let request = SendMessageRequest {
            chat_id,
            text,
            reply_to_message_id,
            allow_sending_without_reply: Some(true),
            parse_mode: Some(TELEGRAM_PARSE_MODE),
        };
        let _: Message = self.post("sendMessage", &request).await?;
        Ok(())
    }

    async fn post<T: DeserializeOwned, B: Serialize>(&self, method: &str, body: &B) -> Result<T> {
        let url = format!("{}/bot{}/{}", self.base_url, self.token, method);
        let response = self
            .http
            .post(url)
            .json(body)
            .send()
            .await
            .map_err(|_| anyhow!("Telegram request failed"))?;

        let payload: TelegramResponse<T> = response
            .json()
            .await
            .map_err(|_| anyhow!("Failed to decode Telegram response"))?;

        if !payload.ok {
            let description = payload
                .description
                .unwrap_or_else(|| "Telegram API error".to_string());
            bail!("{}", description);
        }

        Ok(payload.result)
    }
}

#[derive(Debug, Deserialize)]
struct TelegramSendInput {
    chat_id: i64,
    text: String,
    #[serde(default)]
    reply_to_message_id: Option<i64>,
}

pub fn telegram_send_tool(client: TelegramClient) -> (ToolDefinition, ToolHandler) {
    let definition = ToolDefinition {
        name: "Telegram_Send".to_string(),
        description: "Send a Telegram DM to a chat_id.".to_string(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "chat_id": {
                    "type": "integer",
                    "description": "Telegram chat ID (private DM)"
                },
                "text": {
                    "type": "string",
                    "description": "Message text to send"
                },
                "reply_to_message_id": {
                    "type": "integer",
                    "description": "Optional message ID to reply to"
                }
            },
            "required": ["chat_id", "text"],
            "additionalProperties": false
        }),
    };

    let client = Arc::new(client);
    let handler: ToolHandler = Arc::new(move |input: &Value, ctx: &ToolContext| {
        let client = client.clone();
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

            let send_future = client.send_message(parsed.chat_id, text, parsed.reply_to_message_id);

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

#[derive(Debug, Deserialize)]
struct TelegramResponse<T> {
    ok: bool,
    result: T,
    #[serde(default)]
    description: Option<String>,
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
    reply_to_message_id: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    allow_sending_without_reply: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    parse_mode: Option<&'a str>,
}

#[derive(Debug, Serialize)]
struct GetFileRequest<'a> {
    file_id: &'a str,
}
