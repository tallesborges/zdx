use serde::{Deserialize, Serialize};

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

/// A single button in an inline keyboard.
#[derive(Debug, Clone, Serialize)]
pub struct InlineKeyboardButton {
    pub text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub callback_data: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct Message {
    #[serde(rename = "message_id")]
    pub id: i64,
    pub chat: Chat,
    pub from: Option<User>,
    pub text: Option<String>,
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
    /// Unique identifier of a message thread or forum topic.
    #[serde(default, rename = "message_thread_id")]
    pub thread_id: Option<i64>,
    /// Original message this message replies to.
    #[serde(default, rename = "reply_to_message")]
    pub reply_to: Option<Box<Message>>,
    /// Internal marker used when we route a General message into a newly
    /// created topic before handling it.
    #[serde(skip)]
    pub synthetic_topic_routed_from_general: bool,
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

#[derive(Debug, Deserialize)]
pub struct TelegramFile {
    #[serde(default)]
    pub file_path: Option<String>,
}
