use std::collections::HashSet;
use std::io::Cursor;
use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use image::codecs::jpeg::JpegEncoder;
use image::imageops::FilterType;
use image::{GenericImageView, ImageReader};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use zdx_core::config::Config;

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

const TELEGRAM_PARSE_MODE: &str = "HTML";
const TELEGRAM_CONNECT_TIMEOUT_SECS: u64 = 2;
const TELEGRAM_HTTP_TIMEOUT_SECS: u64 = 35;
const TELEGRAM_PHOTO_MAX_BYTES: usize = 10 * 1024 * 1024;
const TELEGRAM_DOCUMENT_MAX_BYTES: usize = 50 * 1024 * 1024;
const TELEGRAM_PHOTO_MAX_LONG_EDGE: u32 = 1920;
const TELEGRAM_PHOTO_MAX_ASPECT_RATIO: f64 = 20.0;
const TELEGRAM_PHOTO_MAX_ASPECT_RATIO_INT: u32 = 20;
const TELEGRAM_RESIZE_JPEG_QUALITIES: [u8; 3] = [85, 75, 65];

struct PreparedPhoto {
    bytes: Vec<u8>,
    file_name: String,
    mime_type: String,
}

fn prepare_photo_for_telegram(
    photo_data: Vec<u8>,
    file_name: &str,
    mime_type: &str,
) -> Result<PreparedPhoto> {
    let Some((width, height)) = read_photo_dimensions(&photo_data) else {
        return Ok(PreparedPhoto {
            bytes: photo_data,
            file_name: file_name.to_string(),
            mime_type: mime_type.to_string(),
        });
    };

    let needs_full_hd_resize = is_photo_long_edge_too_large(width, height);
    let needs_aspect_crop = is_photo_aspect_ratio_invalid(width, height);
    let needs_reencode_for_size = photo_data.len() > TELEGRAM_PHOTO_MAX_BYTES;
    let needs_reencode_for_format = !mime_type.eq_ignore_ascii_case("image/jpeg");
    if !needs_full_hd_resize
        && !needs_aspect_crop
        && !needs_reencode_for_size
        && !needs_reencode_for_format
    {
        return Ok(PreparedPhoto {
            bytes: photo_data,
            file_name: file_name.to_string(),
            mime_type: mime_type.to_string(),
        });
    }

    let image = image::load_from_memory(&photo_data).context("decode photo")?;
    let normalized = normalize_photo_for_telegram(image);
    let resized_bytes = encode_jpeg_with_size_cap(&normalized).context("encode resized photo")?;
    let resized_name = jpeg_file_name(file_name);

    Ok(PreparedPhoto {
        bytes: resized_bytes,
        file_name: resized_name,
        mime_type: "image/jpeg".to_string(),
    })
}

fn read_photo_dimensions(photo_data: &[u8]) -> Option<(u32, u32)> {
    let cursor = Cursor::new(photo_data);
    let reader = ImageReader::new(cursor).with_guessed_format().ok()?;
    reader.into_dimensions().ok()
}

fn normalize_photo_for_telegram(image: image::DynamicImage) -> image::DynamicImage {
    let mut normalized = image;

    let (mut width, mut height) = normalized.dimensions();
    if is_photo_aspect_ratio_invalid(width, height) {
        normalized = crop_photo_to_aspect_ratio_limit(&normalized);
        (width, height) = normalized.dimensions();
    }

    if is_photo_long_edge_too_large(width, height) {
        normalized = resize_photo_to_full_hd_limit(&normalized);
    }

    normalized
}

fn crop_photo_to_aspect_ratio_limit(image: &image::DynamicImage) -> image::DynamicImage {
    let (width, height) = image.dimensions();
    if width == 0 || height == 0 {
        return image.clone();
    }

    if width >= height {
        let target_width = (height.saturating_mul(TELEGRAM_PHOTO_MAX_ASPECT_RATIO_INT)).min(width);
        if target_width == width {
            return image.clone();
        }
        let x = (width - target_width) / 2;
        image.crop_imm(x, 0, target_width, height)
    } else {
        let target_height = (width.saturating_mul(TELEGRAM_PHOTO_MAX_ASPECT_RATIO_INT)).min(height);
        if target_height == height {
            return image.clone();
        }
        let y = (height - target_height) / 2;
        image.crop_imm(0, y, width, target_height)
    }
}

fn resize_photo_to_full_hd_limit(image: &image::DynamicImage) -> image::DynamicImage {
    let (width, height) = image.dimensions();
    let longest_edge = f64::from(width.max(height));
    let scale = f64::from(TELEGRAM_PHOTO_MAX_LONG_EDGE) / longest_edge;
    let scaled_width = ((f64::from(width) * scale).floor()).max(1.0) as u32;
    let scaled_height = ((f64::from(height) * scale).floor()).max(1.0) as u32;
    image.resize_exact(scaled_width, scaled_height, FilterType::Lanczos3)
}

fn encode_jpeg_with_size_cap(image: &image::DynamicImage) -> Result<Vec<u8>> {
    let mut last = Vec::new();

    for quality in TELEGRAM_RESIZE_JPEG_QUALITIES {
        let bytes = encode_jpeg(image, quality)?;
        if bytes.len() <= TELEGRAM_PHOTO_MAX_BYTES {
            return Ok(bytes);
        }
        last = bytes;
    }

    Ok(last)
}

fn encode_jpeg(image: &image::DynamicImage, quality: u8) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    let mut encoder = JpegEncoder::new_with_quality(&mut bytes, quality);
    encoder
        .encode_image(image)
        .with_context(|| format!("encode JPEG (quality {quality})"))?;
    Ok(bytes)
}

fn jpeg_file_name(file_name: &str) -> String {
    Path::new(file_name)
        .file_stem()
        .and_then(|stem| stem.to_str())
        .map_or_else(|| "photo.jpg".to_string(), |stem| format!("{stem}.jpg"))
}

fn is_photo_long_edge_too_large(width: u32, height: u32) -> bool {
    width.max(height) > TELEGRAM_PHOTO_MAX_LONG_EDGE
}

fn is_photo_aspect_ratio_invalid(width: u32, height: u32) -> bool {
    let min = width.min(height);
    if min == 0 {
        return true;
    }
    let max = width.max(height);
    f64::from(max) / f64::from(min) > TELEGRAM_PHOTO_MAX_ASPECT_RATIO
}

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
    pub(crate) async fn set_my_commands(
        &self,
        command_specs: &[crate::commands::TelegramCommandSpec],
    ) -> Result<()> {
        let commands = command_specs
            .iter()
            .map(|spec| TelegramBotCommand {
                command: spec.command,
                description: spec.description,
            })
            .collect();
        let request = SetMyCommandsRequest { commands };
        let _: bool = self.post("setMyCommands", &request).await?;
        Ok(())
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
    pub async fn send_photo_from_path(
        &self,
        chat_id: i64,
        photo_path: &Path,
        caption: Option<&str>,
        reply_to_message_id: Option<i64>,
        message_thread_id: Option<i64>,
        reply_parameters: Option<ReplyParameters>,
    ) -> Result<()> {
        let photo_data = std::fs::read(photo_path)
            .with_context(|| format!("read photo file {}", photo_path.display()))?;
        let file_name = photo_path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("photo.png");
        let mime_type = infer::get(&photo_data).map_or_else(
            || "application/octet-stream".to_string(),
            |kind| kind.mime_type().to_string(),
        );
        let prepared = prepare_photo_for_telegram(photo_data, file_name, &mime_type)
            .with_context(|| format!("prepare photo for Telegram {}", photo_path.display()))?;

        self.send_photo(
            chat_id,
            &prepared.bytes,
            &prepared.file_name,
            &prepared.mime_type,
            caption,
            reply_to_message_id,
            message_thread_id,
            reply_parameters,
        )
        .await
    }

    ///
    /// # Errors
    /// Returns an error if the operation fails.
    pub async fn send_document_from_path(
        &self,
        chat_id: i64,
        document_path: &Path,
        caption: Option<&str>,
        reply_to_message_id: Option<i64>,
        message_thread_id: Option<i64>,
        reply_parameters: Option<ReplyParameters>,
    ) -> Result<()> {
        let document_data = std::fs::read(document_path)
            .with_context(|| format!("read document file {}", document_path.display()))?;
        let file_name = document_path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("document.bin");
        let mime_type = infer::get(&document_data).map_or_else(
            || "application/octet-stream".to_string(),
            |kind| kind.mime_type().to_string(),
        );

        self.send_document(
            chat_id,
            &document_data,
            file_name,
            &mime_type,
            caption,
            reply_to_message_id,
            message_thread_id,
            reply_parameters,
        )
        .await
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
        self.send_message_raw(SendMessageRawArgs {
            chat_id,
            text,
            reply_to_message_id,
            message_thread_id,
            parse_mode,
            reply_markup: None,
            reply_parameters: None,
        })
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

    /// Send a message with explicit [`ReplyParameters`] for cross-topic references.
    ///
    /// # Errors
    /// Returns an error if the operation fails.
    pub async fn send_message_with_reply_params(
        &self,
        chat_id: i64,
        text: &str,
        message_thread_id: Option<i64>,
        reply_parameters: Option<ReplyParameters>,
    ) -> Result<()> {
        self.send_message_inner_with_reply_params(
            chat_id,
            text,
            message_thread_id,
            None,
            reply_parameters,
        )
        .await
        .map(|_| ())
    }

    /// Inner send with HTML-fallback-to-plain logic.
    async fn send_message_inner(
        &self,
        chat_id: i64,
        text: &str,
        reply_to_message_id: Option<i64>,
        message_thread_id: Option<i64>,
        reply_markup: Option<&InlineKeyboardMarkup>,
    ) -> Result<Message> {
        // First try with HTML parse mode
        let result = self
            .send_message_raw(SendMessageRawArgs {
                chat_id,
                text,
                reply_to_message_id,
                message_thread_id,
                parse_mode: Some(TELEGRAM_PARSE_MODE),
                reply_markup,
                reply_parameters: None,
            })
            .await;

        // If HTML parsing failed, retry as plain text
        if let Err(ref e) = result {
            let err_msg = e.to_string();
            if err_msg.contains("can't parse entities") || err_msg.contains("Can't find end of") {
                return self
                    .send_message_raw(SendMessageRawArgs {
                        chat_id,
                        text,
                        reply_to_message_id,
                        message_thread_id,
                        parse_mode: None,
                        reply_markup,
                        reply_parameters: None,
                    })
                    .await;
            }
        }

        result
    }

    /// Inner send with `reply_parameters` and HTML-fallback-to-plain logic.
    async fn send_message_inner_with_reply_params(
        &self,
        chat_id: i64,
        text: &str,
        message_thread_id: Option<i64>,
        reply_markup: Option<&InlineKeyboardMarkup>,
        reply_parameters: Option<ReplyParameters>,
    ) -> Result<Message> {
        let result = self
            .send_message_raw(SendMessageRawArgs {
                chat_id,
                text,
                reply_to_message_id: None,
                message_thread_id,
                parse_mode: Some(TELEGRAM_PARSE_MODE),
                reply_markup,
                reply_parameters,
            })
            .await;

        if let Err(ref e) = result {
            let err_msg = e.to_string();
            if err_msg.contains("can't parse entities") || err_msg.contains("Can't find end of") {
                return self
                    .send_message_raw(SendMessageRawArgs {
                        chat_id,
                        text,
                        reply_to_message_id: None,
                        message_thread_id,
                        parse_mode: None,
                        reply_markup,
                        reply_parameters: None,
                    })
                    .await;
            }
        }

        result
    }

    async fn send_message_raw(&self, args: SendMessageRawArgs<'_>) -> Result<Message> {
        let request = SendMessageRequest {
            chat_id: args.chat_id,
            text: args.text,
            message_thread_id: args.message_thread_id,
            reply_to_message_id: args.reply_to_message_id,
            allow_sending_without_reply: Some(true),
            parse_mode: args.parse_mode,
            reply_markup: args.reply_markup,
            reply_parameters: args.reply_parameters,
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
        // Try HTML first
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

    #[allow(clippy::too_many_arguments)]
    async fn send_photo(
        &self,
        chat_id: i64,
        photo_data: &[u8],
        file_name: &str,
        mime_type: &str,
        caption: Option<&str>,
        reply_to_message_id: Option<i64>,
        message_thread_id: Option<i64>,
        reply_parameters: Option<ReplyParameters>,
    ) -> Result<()> {
        if photo_data.len() > TELEGRAM_PHOTO_MAX_BYTES {
            bail!(
                "photo exceeds Telegram limit ({} bytes > {} bytes)",
                photo_data.len(),
                TELEGRAM_PHOTO_MAX_BYTES
            );
        }

        let part = reqwest::multipart::Part::bytes(photo_data.to_vec())
            .file_name(file_name.to_string())
            .mime_str(mime_type)
            .context("set photo mime type")?;

        let mut form = reqwest::multipart::Form::new()
            .text("chat_id", chat_id.to_string())
            .part("photo", part);

        if let Some(caption) = caption
            && !caption.trim().is_empty()
        {
            form = form.text("caption", caption.to_string());
        }
        if let Some(reply_to_message_id) = reply_to_message_id {
            form = form
                .text("reply_to_message_id", reply_to_message_id.to_string())
                .text("allow_sending_without_reply", "true".to_string());
        }
        if let Some(message_thread_id) = message_thread_id {
            form = form.text("message_thread_id", message_thread_id.to_string());
        }
        if let Some(reply_parameters) = reply_parameters {
            form = form.text(
                "reply_parameters",
                serde_json::to_string(&reply_parameters).context("serialize reply_parameters")?,
            );
        }

        let _: Message = self.post_multipart("sendPhoto", form).await?;
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    async fn send_document(
        &self,
        chat_id: i64,
        document_data: &[u8],
        file_name: &str,
        mime_type: &str,
        caption: Option<&str>,
        reply_to_message_id: Option<i64>,
        message_thread_id: Option<i64>,
        reply_parameters: Option<ReplyParameters>,
    ) -> Result<()> {
        if document_data.len() > TELEGRAM_DOCUMENT_MAX_BYTES {
            bail!(
                "document exceeds Telegram limit ({} bytes > {} bytes)",
                document_data.len(),
                TELEGRAM_DOCUMENT_MAX_BYTES
            );
        }

        let part = reqwest::multipart::Part::bytes(document_data.to_vec())
            .file_name(file_name.to_string())
            .mime_str(mime_type)
            .context("set document mime type")?;

        let mut form = reqwest::multipart::Form::new()
            .text("chat_id", chat_id.to_string())
            .part("document", part);

        if let Some(caption) = caption
            && !caption.trim().is_empty()
        {
            form = form.text("caption", caption.to_string());
        }
        if let Some(reply_to_message_id) = reply_to_message_id {
            form = form
                .text("reply_to_message_id", reply_to_message_id.to_string())
                .text("allow_sending_without_reply", "true".to_string());
        }
        if let Some(message_thread_id) = message_thread_id {
            form = form.text("message_thread_id", message_thread_id.to_string());
        }
        if let Some(reply_parameters) = reply_parameters {
            form = form.text(
                "reply_parameters",
                serde_json::to_string(&reply_parameters).context("serialize reply_parameters")?,
            );
        }

        let _: Message = self.post_multipart("sendDocument", form).await?;
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
            .context("Telegram request")?;

        let bytes = response.bytes().await.context("read Telegram response")?;
        Self::parse_telegram_response(&bytes)
    }

    async fn post_multipart<T: DeserializeOwned>(
        &self,
        method: &str,
        form: reqwest::multipart::Form,
    ) -> Result<T> {
        let url = format!("{}/bot{}/{}", self.base_url, self.token, method);
        let response = self
            .http
            .post(url)
            .multipart(form)
            .send()
            .await
            .context("Telegram multipart request")?;

        let bytes = response.bytes().await.context("read Telegram response")?;
        Self::parse_telegram_response(&bytes)
    }

    fn parse_telegram_response<T: DeserializeOwned>(bytes: &[u8]) -> Result<T> {
        let envelope: TelegramEnvelope =
            serde_json::from_slice(bytes).context("parse Telegram response envelope")?;

        if !envelope.ok {
            let error: TelegramError =
                serde_json::from_slice(bytes).context("decode Telegram error")?;
            let description = error
                .description
                .unwrap_or_else(|| "Telegram API error".to_string());
            bail!("{description}");
        }

        let success: TelegramSuccess<T> =
            serde_json::from_slice(bytes).context("decode Telegram result")?;

        Ok(success.result)
    }
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
struct SetMyCommandsRequest<'a> {
    commands: Vec<TelegramBotCommand<'a>>,
}

#[derive(Debug, Serialize)]
struct TelegramBotCommand<'a> {
    command: &'a str,
    description: &'a str,
}

#[derive(Debug, Clone, Serialize)]
pub struct ReplyParameters {
    pub message_id: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub chat_id: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub allow_sending_without_reply: Option<bool>,
}

struct SendMessageRawArgs<'a> {
    chat_id: i64,
    text: &'a str,
    reply_to_message_id: Option<i64>,
    message_thread_id: Option<i64>,
    parse_mode: Option<&'a str>,
    reply_markup: Option<&'a InlineKeyboardMarkup>,
    reply_parameters: Option<ReplyParameters>,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    reply_parameters: Option<ReplyParameters>,
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

#[cfg(test)]
mod tests {
    use super::{is_photo_aspect_ratio_invalid, is_photo_long_edge_too_large};

    #[test]
    fn detects_full_hd_long_edge_limit() {
        assert!(is_photo_long_edge_too_large(2560, 1440));
        assert!(is_photo_long_edge_too_large(1080, 2400));
        assert!(!is_photo_long_edge_too_large(1920, 1080));
        assert!(!is_photo_long_edge_too_large(1080, 1920));
    }

    #[test]
    fn detects_aspect_ratio_limit() {
        assert!(is_photo_aspect_ratio_invalid(30000, 1000));
        assert!(!is_photo_aspect_ratio_invalid(3840, 2160));
    }
}
