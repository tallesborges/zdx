use std::borrow::Cow;

use serde::{Deserialize, Serialize};

use super::rich::RichMessage;

#[derive(Debug, Deserialize)]
pub struct Update {
    #[serde(rename = "update_id")]
    pub id: i64,
    pub message: Option<Message>,
    pub callback_query: Option<CallbackQuery>,
}

/// Incoming callback query from an inline keyboard button.
#[derive(Debug, Deserialize)]
pub struct CallbackQuery {
    pub id: String,
    pub from: User,
    /// The message that contained the inline keyboard (present when the button
    /// was attached to a message sent by the bot).
    #[serde(rename = "message")]
    pub message: Option<Message>,
    /// Data associated with the callback button (max 64 bytes).
    pub data: Option<String>,
}

/// Inline keyboard attached to a message.
#[derive(Debug, Clone, Serialize)]
pub struct InlineKeyboardMarkup {
    pub inline_keyboard: Vec<Vec<InlineKeyboardButton>>,
}

impl InlineKeyboardMarkup {
    /// Empty keyboard. `editMessageText` keeps the previous buttons when
    /// `reply_markup` is omitted, so removing them requires sending this.
    pub fn empty() -> Self {
        Self {
            inline_keyboard: Vec::new(),
        }
    }
}

/// A single button in an inline keyboard.
#[derive(Debug, Clone, Serialize)]
pub struct InlineKeyboardButton {
    pub text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub callback_data: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub web_app: Option<WebAppInfo>,
}

impl InlineKeyboardButton {
    pub fn callback(text: impl Into<String>, callback_data: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            callback_data: Some(callback_data.into()),
            url: None,
            web_app: None,
        }
    }

    pub fn url(text: impl Into<String>, url: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            callback_data: None,
            url: Some(url.into()),
            web_app: None,
        }
    }

    pub fn web_app(text: impl Into<String>, url: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            callback_data: None,
            url: None,
            web_app: Some(WebAppInfo { url: url.into() }),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct WebAppInfo {
    pub url: String,
}

#[derive(Debug, Deserialize)]
pub struct Message {
    #[serde(rename = "message_id")]
    pub id: i64,
    pub chat: Chat,
    pub from: Option<User>,
    /// Unix time (seconds) Telegram assigned to the message. Telegram always
    /// sends it; synthetic messages leave it `0`.
    #[serde(default)]
    pub date: i64,
    #[serde(default)]
    pub media_group_id: Option<String>,
    pub text: Option<String>,
    /// Content of a rich message (Bot API 10.1). Set instead of `text` when the
    /// sender used structured formatting (lists, headings, tables).
    #[serde(default)]
    pub rich_message: Option<RichMessage>,
    #[serde(default)]
    pub caption: Option<String>,
    #[serde(default)]
    pub photo: Option<Vec<PhotoSize>>,
    #[serde(default)]
    pub voice: Option<Voice>,
    #[serde(default)]
    pub audio: Option<Audio>,
    #[serde(default)]
    pub document: Option<Document>,
    #[serde(default)]
    pub video: Option<Video>,
    /// Round video message recorded in the Telegram client.
    #[serde(default)]
    pub video_note: Option<Video>,
    /// GIF or silent H.264/MPEG-4 clip.
    #[serde(default)]
    pub animation: Option<Video>,
    /// Paid media. Not handled by ingest; read only for intake metadata.
    #[serde(default)]
    pub paid_media: Option<PaidMediaInfo>,
    /// Unique identifier of a message thread or forum topic.
    #[serde(default, rename = "message_thread_id")]
    pub thread_id: Option<i64>,
    /// Original message this message replies to.
    #[serde(default, rename = "reply_to_message")]
    pub reply_to: Option<Box<Message>>,
    /// The part of `reply_to` the sender selected to quote, if any.
    #[serde(default)]
    pub quote: Option<TextQuote>,
    /// Inline keyboard of a message the bot sent, as Telegram returns it on a
    /// callback query. Kept raw: an unknown button type must not fail the
    /// update. Read through [`Message::link_rows`].
    #[serde(default)]
    pub(crate) reply_markup: Option<serde_json::Value>,
    /// Internal marker used when we route a General message into a newly
    /// created topic before handling it.
    #[serde(skip)]
    pub synthetic_topic_routed_from_general: bool,
    /// Internal marker for a message routed past the queue as the input a live
    /// `/btw` session is waiting for. If staging no longer consumes it (session
    /// expired or already used), it is re-dispatched as an ordinary message.
    #[serde(skip)]
    pub routed_as_btw_input: bool,
    /// Internal marker for a bot-generated prompt (worker update, Mini App
    /// artifact feedback). Staged commands wait for the user, so these never
    /// start or complete one. Consecutive pending worker updates are merged
    /// into a single orchestrator turn by the queue.
    #[serde(skip)]
    pub(crate) synthetic: Option<crate::bot::synthetic::SyntheticKind>,
    /// Files already on the bot host that ride along with a synthetic prompt
    /// (Mini App artifact feedback). Loaded by ingest; never set from Telegram.
    #[serde(skip)]
    pub(crate) local_attachments: Vec<crate::types::LocalAttachment>,
    /// Additional Telegram messages that belong to the same media album.
    #[serde(skip)]
    pub grouped_messages: Vec<Message>,
}

impl Message {
    /// Returns the best-effort forum topic/thread id for this message.
    ///
    /// Telegram usually sets `message_thread_id` on topic messages, but some
    /// clients/flows can omit it while still including it on `reply_to_message`.
    pub fn effective_thread_id(&self) -> Option<i64> {
        self.thread_id
            .or_else(|| self.reply_to.as_ref().and_then(|m| m.thread_id))
    }

    /// Rows of this message's inline keyboard in which every button only
    /// opens a link (`url`), such as a file's `Open artifact`. Edits that
    /// replace the follow-up buttons keep these rows.
    pub(crate) fn link_rows(&self) -> Vec<Vec<InlineKeyboardButton>> {
        let Some(rows) = self
            .reply_markup
            .as_ref()
            .and_then(|markup| markup.get("inline_keyboard"))
            .and_then(serde_json::Value::as_array)
        else {
            return Vec::new();
        };
        rows.iter()
            .filter_map(|row| {
                let buttons = row.as_array()?;
                let links = buttons
                    .iter()
                    .map(|button| {
                        let text = button.get("text")?.as_str()?;
                        let url = button.get("url")?.as_str()?;
                        Some(InlineKeyboardButton::url(text, url))
                    })
                    .collect::<Option<Vec<_>>>()?;
                (!links.is_empty()).then_some(links)
            })
            .collect()
    }

    /// Plain text the sender wrote, from `text` or from a rich message.
    ///
    /// A rich message (Bot API 10.1) leaves `text` unset, so anything reading
    /// `text` directly sees an empty message and silently drops the content.
    pub fn plain_text(&self) -> Option<Cow<'_, str>> {
        if let Some(text) = self.text.as_deref() {
            return Some(Cow::Borrowed(text));
        }
        self.rich_message
            .as_ref()
            .and_then(RichMessage::to_text)
            .map(Cow::Owned)
    }
}

/// Telegram `TextQuote`: the quoted part of a replied-to message.
#[derive(Debug, Deserialize)]
pub struct TextQuote {
    pub text: String,
}

/// Telegram `PaidMediaInfo`. Items are counted but never kept, so nothing
/// from their bodies can reach a log or a prompt.
#[derive(Debug, Deserialize)]
pub struct PaidMediaInfo {
    #[serde(default)]
    pub paid_media: Vec<serde::de::IgnoredAny>,
}

#[derive(Debug, Deserialize)]
pub struct Chat {
    pub id: i64,
    #[serde(rename = "type")]
    kind: String,
    /// True if the supergroup has topics/forum enabled.
    #[serde(default)]
    pub is_forum: Option<bool>,
}

impl Chat {
    pub fn is_private(&self) -> bool {
        self.kind == "private"
    }

    pub fn is_group(&self) -> bool {
        self.kind == "group" || self.kind == "supergroup"
    }

    /// Returns true if this is a forum-enabled supergroup.
    pub fn is_forum_enabled(&self) -> bool {
        self.is_forum.unwrap_or(false)
    }
}

#[derive(Debug, Deserialize)]
pub struct User {
    pub id: i64,
    #[serde(default)]
    pub is_bot: bool,
}

#[derive(Debug, Deserialize)]
pub struct PhotoSize {
    pub file_id: String,
    pub width: i64,
    pub height: i64,
    #[serde(default)]
    pub file_size: Option<u64>,
}

#[derive(Debug, Deserialize)]
pub struct Voice {
    pub file_id: String,
    #[serde(default)]
    pub mime_type: Option<String>,
    #[serde(default)]
    pub file_size: Option<u64>,
}

#[derive(Debug, Deserialize)]
pub struct Audio {
    pub file_id: String,
    #[serde(default)]
    pub mime_type: Option<String>,
    #[serde(default)]
    pub file_size: Option<u64>,
    #[serde(default)]
    pub file_name: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct Document {
    pub file_id: String,
    #[serde(default)]
    pub file_name: Option<String>,
    #[serde(default)]
    pub mime_type: Option<String>,
    #[serde(default)]
    pub file_size: Option<u64>,
}

/// Shared shape of Telegram `Video`, `VideoNote`, and `Animation`; the fields a
/// type does not send stay `None`.
#[derive(Debug, Deserialize)]
pub struct Video {
    pub file_id: String,
    #[serde(default)]
    pub file_name: Option<String>,
    #[serde(default)]
    pub mime_type: Option<String>,
    #[serde(default)]
    pub file_size: Option<u64>,
}

#[derive(Debug, Deserialize)]
pub struct TelegramFile {
    #[serde(default)]
    pub file_path: Option<String>,
}
