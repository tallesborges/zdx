use std::collections::HashSet;
use std::time::Duration;

use anyhow::{Result, anyhow, bail};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use zdx_core::config::Config;

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

pub struct TelegramClient {
    http: reqwest::Client,
    base_url: String,
    token: String,
}

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
}

#[derive(Debug, Deserialize)]
pub struct Update {
    pub update_id: i64,
    pub message: Option<Message>,
}

#[derive(Debug, Deserialize)]
pub struct Message {
    pub message_id: i64,
    pub chat: Chat,
    pub from: Option<User>,
    pub text: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct Chat {
    pub id: i64,
    #[serde(rename = "type")]
    kind: String,
}

impl Chat {
    pub fn is_private(&self) -> bool {
        self.kind == "private"
    }
}

#[derive(Debug, Deserialize)]
pub struct User {
    pub id: i64,
}
