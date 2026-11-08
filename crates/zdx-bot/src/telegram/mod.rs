use std::collections::{HashMap, HashSet};
use std::io::Cursor;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use image::codecs::jpeg::JpegEncoder;
use image::imageops::FilterType;
use image::{GenericImageView, ImageReader};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use zdx_engine::config::{Config, ResolvedTelegramRuntime};
use zdx_engine::telegram::flood::{
    self, CallRecord, ChatPacer, FailureClass, FloodWaitActive, RetryPolicy, TelegramApiError,
    TelegramTransportError,
};

#[cfg(test)]
pub(crate) mod fake;
mod html;
pub(crate) mod markdown;
mod rich;
mod types;

#[allow(unused_imports)]
pub use rich::RichMessage;
#[allow(unused_imports)]
pub use types::{
    Audio, CallbackQuery, Document, InlineKeyboardButton, InlineKeyboardMarkup, Message,
    PaidMediaInfo, PhotoSize, TelegramFile, Update, Video, Voice, WebAppInfo,
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
        let ResolvedTelegramRuntime {
            bot_token,
            allowlist_user_ids,
            allowlist_chat_ids,
        } = config.resolve_telegram_runtime()?;

        Ok(Self {
            bot_token,
            allowlist_user_ids: allowlist_user_ids.into_iter().collect::<HashSet<_>>(),
            allowlist_chat_ids: allowlist_chat_ids.into_iter().collect::<HashSet<_>>(),
        })
    }
}

#[derive(Clone)]
pub struct TelegramClient {
    http: reqwest::Client,
    base_url: String,
    token: String,
    gate: Arc<ChatGate>,
    mode: SendMode,
}

const TELEGRAM_PARSE_MODE: &str = "HTML";
const TELEGRAM_CONNECT_TIMEOUT_SECS: u64 = 2;
const TELEGRAM_HTTP_TIMEOUT_SECS: u64 = 35;
const TELEGRAM_PHOTO_MAX_BYTES: usize = 10 * 1024 * 1024;

pub(crate) use zdx_engine::telegram::topic_link;
const TELEGRAM_DOCUMENT_MAX_BYTES: usize = 50 * 1024 * 1024;
const TELEGRAM_PHOTO_MAX_LONG_EDGE: u32 = 1920;
const TELEGRAM_PHOTO_MAX_ASPECT_RATIO: f64 = 20.0;
const TELEGRAM_PHOTO_MAX_ASPECT_RATIO_INT: u32 = 20;
const TELEGRAM_RESIZE_JPEG_QUALITIES: [u8; 3] = [85, 75, 65];

/// Reads a file that is uploaded as is. Its size is checked from the file's
/// metadata first, against the same limit `send_file` applies to the bytes,
/// so an oversized file is refused without being read into memory. Photos do
/// not come through here: they are re-encoded, so a large original can still
/// fit after `prepare_photo_for_telegram`.
fn read_upload(path: &Path, field: &str) -> Result<Vec<u8>> {
    let len = std::fs::metadata(path)
        .with_context(|| format!("read {field} file {}", path.display()))?
        .len();
    if len > TELEGRAM_DOCUMENT_MAX_BYTES as u64 {
        bail!("{field} exceeds Telegram limit ({len} bytes > {TELEGRAM_DOCUMENT_MAX_BYTES} bytes)");
    }
    std::fs::read(path).with_context(|| format!("read {field} file {}", path.display()))
}

struct PreparedPhoto {
    bytes: Vec<u8>,
    file_name: String,
    mime_type: String,
}

/// Whether Telegram rejected the message because it could not parse the HTML
/// markup (stray `<`, unknown entity, or an unclosed tag).
fn is_html_parse_error(err: &anyhow::Error) -> bool {
    api_error_description(err).is_some_and(|description| {
        description.contains("can't parse entities") || description.contains("Can't find end of")
    })
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

/// How a call may use the per-chat gate. See the module docs of
/// `zdx_engine::telegram::flood` for what is policy and what is documented.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SendMode {
    /// Never waits. Skipped locally while the chat is in a flood wait.
    /// Default, so callers on the polling loop or the service bridge never
    /// stall on one busy chat.
    Immediate,
    /// Waits for the chat's flood wait and, for new messages, for the pacing
    /// slot. One attempt.
    Paced,
    /// Like `Paced`, and retries explicit flood refusals within
    /// [`RetryPolicy::IMPORTANT`]. For reply text and files.
    Reliable,
}

/// What a call does to the chat, for pacing and logging.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CallKind {
    /// Creates a chat message: paced and counted.
    NewMessage,
    /// Changes existing state (edit, delete, pin, topic). Not paced or
    /// counted; waits out a flood wait when the mode allows waiting.
    Update,
    /// Typing indicator: always skipped during a flood wait, never waits.
    Ephemeral,
    /// Not tied to a chat (polling, files, commands, callback answers).
    Unscoped,
}

#[derive(Debug, Clone, Copy)]
struct CallSpec<'a> {
    method: &'a str,
    chat: Option<i64>,
    topic: Option<i64>,
    kind: CallKind,
}

impl<'a> CallSpec<'a> {
    fn new_message(method: &'a str, chat: i64, topic: Option<i64>) -> Self {
        Self {
            method,
            chat: Some(chat),
            topic,
            kind: CallKind::NewMessage,
        }
    }

    fn update(method: &'a str, chat: i64) -> Self {
        Self {
            method,
            chat: Some(chat),
            topic: None,
            kind: CallKind::Update,
        }
    }

    fn unscoped(method: &'a str) -> Self {
        Self {
            method,
            chat: None,
            topic: None,
            kind: CallKind::Unscoped,
        }
    }
}

/// Per-chat pacing and flood-wait state shared by every clone of a client
/// (one bot token, so the key is effectively bot + chat). Process-local: the
/// `Telegram` tool and `zdx telegram` use their own client and bypass it.
#[derive(Debug, Default)]
struct ChatGate {
    chats: std::sync::Mutex<HashMap<i64, ChatPacer>>,
}

impl ChatGate {
    fn with_chat<R>(&self, chat: i64, f: impl FnOnce(&mut ChatPacer) -> R) -> R {
        let mut chats = self.chats.lock().expect("chat gate lock poisoned");
        f(chats.entry(chat).or_default())
    }
}

/// Options for sending a file.
#[derive(Debug, Clone, Default)]
pub struct MediaSend<'a> {
    /// HTML caption (at most 1024 characters after entity parsing).
    pub caption: Option<&'a str>,
    pub reply_markup: Option<&'a InlineKeyboardMarkup>,
    pub reply_to_message_id: Option<i64>,
    pub message_thread_id: Option<i64>,
    pub reply_parameters: Option<ReplyParameters>,
}

/// Options for sending a text message (HTML with the repair/plain fallback).
#[derive(Debug, Clone)]
pub struct TextSend<'a> {
    pub chat_id: i64,
    pub text: &'a str,
    pub reply_to_message_id: Option<i64>,
    pub message_thread_id: Option<i64>,
    pub reply_parameters: Option<ReplyParameters>,
    pub reply_markup: Option<&'a InlineKeyboardMarkup>,
    pub disable_preview: bool,
}

/// One multipart file upload; rebuilt for every attempt because a
/// `reqwest` form can be sent only once.
struct FileUpload<'a> {
    method: &'a str,
    field: &'a str,
    data: &'a [u8],
    file_name: &'a str,
    mime_type: &'a str,
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
            gate: Arc::new(ChatGate::default()),
            mode: SendMode::Immediate,
        }
    }

    #[cfg(test)]
    pub(crate) fn with_base_url(token: String, base_url: String) -> Self {
        Self {
            base_url,
            ..Self::new(token)
        }
    }

    /// A handle whose calls wait for the chat's flood wait and pacing slot
    /// (one attempt). Use it only off the polling loop: it can wait for as
    /// long as Telegram's `retry_after`.
    #[must_use]
    pub fn paced(&self) -> Self {
        Self {
            mode: SendMode::Paced,
            ..self.clone()
        }
    }

    /// A handle for reply text and files: paced, and explicit flood refusals
    /// are retried within [`RetryPolicy::IMPORTANT`]. Ambiguous failures are
    /// never retried.
    #[must_use]
    pub fn reliable(&self) -> Self {
        Self {
            mode: SendMode::Reliable,
            ..self.clone()
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
        self.post(CallSpec::unscoped("getUpdates"), &request).await
    }

    /// Register bot commands for all relevant Telegram scopes (default,
    /// private chats, group chats). This ensures commands appear in the `/`
    /// menu regardless of any previously-set narrower scopes.
    ///
    /// # Errors
    /// Returns an error if the operation fails.
    pub(crate) async fn set_my_commands(
        &self,
        command_specs: &[crate::commands::TelegramCommandSpec],
    ) -> Result<()> {
        let commands: Vec<TelegramBotCommand<'_>> = command_specs
            .iter()
            .map(|spec| TelegramBotCommand {
                command: spec.command,
                description: spec.description,
            })
            .collect();

        for scope in BOT_COMMAND_SCOPES {
            // Delete any stale commands for this scope first
            let delete_req = DeleteMyCommandsRequest {
                scope: Some(scope.clone()),
            };
            let _: bool = self
                .post(CallSpec::unscoped("deleteMyCommands"), &delete_req)
                .await?;

            // Set commands for this scope
            let set_req = SetMyCommandsRequest {
                commands: commands.clone(),
                scope: Some(scope.clone()),
            };
            let _: bool = self
                .post(CallSpec::unscoped("setMyCommands"), &set_req)
                .await?;
        }

        Ok(())
    }

    ///
    /// # Errors
    /// Returns an error if the operation fails.
    pub async fn get_file(&self, file_id: &str) -> Result<TelegramFile> {
        let request = GetFileRequest { file_id };
        self.post(CallSpec::unscoped("getFile"), &request).await
    }

    ///
    /// # Errors
    /// Returns an error if the operation fails.
    pub async fn download_file(&self, file_path: &str) -> Result<Vec<u8>> {
        let spec = CallSpec::unscoped("downloadFile");
        let started = Instant::now();
        let result = self.download_file_once(file_path).await;
        self.log(
            &spec,
            1,
            Duration::ZERO,
            started.elapsed(),
            result.as_ref().map(|_| None),
        );
        result
    }

    async fn download_file_once(&self, file_path: &str) -> Result<Vec<u8>> {
        let url = format!("{}/file/bot{}/{}", self.base_url, self.token, file_path);
        let response = self
            .http
            .get(url)
            .send()
            .await
            .map_err(|err| TelegramTransportError::from_reqwest("downloadFile", err))?;

        let status = response.status();
        if !status.is_success() {
            return Err(TelegramApiError {
                method: "downloadFile".to_string(),
                code: Some(status.as_u16()),
                description: format!("file download failed with status {status}"),
                retry_after: None,
            }
            .into());
        }

        let bytes = response
            .bytes()
            .await
            .map_err(|err| TelegramTransportError::from_reqwest("downloadFile", err))?;
        Ok(bytes.to_vec())
    }

    /// Sends an image as a photo (resized/re-encoded to Telegram's limits).
    ///
    /// # Errors
    /// Returns an error if the operation fails.
    pub async fn send_photo_from_path(
        &self,
        chat_id: i64,
        photo_path: &Path,
        options: &MediaSend<'_>,
    ) -> Result<Message> {
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
        if prepared.bytes.len() > TELEGRAM_PHOTO_MAX_BYTES {
            bail!(
                "photo exceeds Telegram limit ({} bytes > {} bytes)",
                prepared.bytes.len(),
                TELEGRAM_PHOTO_MAX_BYTES
            );
        }

        self.send_file(
            chat_id,
            &FileUpload {
                method: "sendPhoto",
                field: "photo",
                data: &prepared.bytes,
                file_name: &prepared.file_name,
                mime_type: &prepared.mime_type,
            },
            options,
        )
        .await
    }

    /// Sends any file as a document.
    ///
    /// # Errors
    /// Returns an error if the operation fails.
    pub async fn send_document_from_path(
        &self,
        chat_id: i64,
        document_path: &Path,
        options: &MediaSend<'_>,
    ) -> Result<Message> {
        let document_data = read_upload(document_path, "document")?;
        let file_name = document_path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("document.bin");
        let mime_type = infer::get(&document_data).map_or_else(
            || "application/octet-stream".to_string(),
            |kind| kind.mime_type().to_string(),
        );

        self.send_file(
            chat_id,
            &FileUpload {
                method: "sendDocument",
                field: "document",
                data: &document_data,
                file_name,
                mime_type: &mime_type,
            },
            options,
        )
        .await
    }

    /// Send an OGG/Opus file as a Telegram voice note (waveform + playback speed).
    ///
    /// # Errors
    /// Returns an error if the operation fails.
    pub async fn send_voice_from_path(
        &self,
        chat_id: i64,
        voice_path: &Path,
        options: &MediaSend<'_>,
    ) -> Result<Message> {
        let voice_data = read_upload(voice_path, "voice")?;
        let file_name = voice_path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("voice.ogg");

        self.send_file(
            chat_id,
            &FileUpload {
                method: "sendVoice",
                field: "voice",
                data: &voice_data,
                file_name,
                mime_type: "audio/ogg",
            },
            options,
        )
        .await
    }

    /// Send an audio file (MP3/M4A/WAV) as a Telegram audio message.
    ///
    /// # Errors
    /// Returns an error if the operation fails.
    pub async fn send_audio_from_path(
        &self,
        chat_id: i64,
        audio_path: &Path,
        options: &MediaSend<'_>,
    ) -> Result<Message> {
        let audio_data = read_upload(audio_path, "audio")?;
        let file_name = audio_path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("audio.mp3");
        let mime_type = infer::get(&audio_data).map_or_else(
            || "audio/mpeg".to_string(),
            |kind| kind.mime_type().to_string(),
        );

        self.send_file(
            chat_id,
            &FileUpload {
                method: "sendAudio",
                field: "audio",
                data: &audio_data,
                file_name,
                mime_type: &mime_type,
            },
            options,
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
        self.send_message_inner(
            chat_id,
            text,
            reply_to_message_id,
            message_thread_id,
            None,
            None,
        )
        .await
        .map(|_| ())
    }

    /// Send a message with link previews suppressed.
    ///
    /// Used for final answers, which carry an appended Mini App deep link that
    /// would otherwise render a preview card under every reply.
    ///
    /// # Errors
    /// Returns an error if the operation fails.
    pub async fn send_message_without_preview(
        &self,
        chat_id: i64,
        text: &str,
        reply_to_message_id: Option<i64>,
        message_thread_id: Option<i64>,
    ) -> Result<()> {
        self.send_message_inner(
            chat_id,
            text,
            reply_to_message_id,
            message_thread_id,
            None,
            Some(NO_LINK_PREVIEW),
        )
        .await
        .map(|_| ())
    }

    /// Sends a text message with every option, returning the sent message.
    ///
    /// # Errors
    /// Returns an error if the operation fails.
    pub async fn send_text(&self, send: &TextSend<'_>) -> Result<Message> {
        self.send_message_with_html_fallback(SendMessageRawArgs {
            chat_id: send.chat_id,
            text: send.text,
            reply_to_message_id: send.reply_to_message_id,
            message_thread_id: send.message_thread_id,
            parse_mode: Some(TELEGRAM_PARSE_MODE),
            reply_markup: send.reply_markup,
            reply_parameters: send.reply_parameters.clone(),
            link_preview_options: send.disable_preview.then_some(NO_LINK_PREVIEW),
        })
        .await
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
            link_preview_options: None,
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
            None,
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
            None,
        )
        .await
        .map(|_| ())
    }

    /// Cross-topic reply with link previews suppressed. See
    /// [`Self::send_message_without_preview`].
    ///
    /// # Errors
    /// Returns an error if the operation fails.
    pub async fn send_message_with_reply_params_without_preview(
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
            Some(NO_LINK_PREVIEW),
        )
        .await
        .map(|_| ())
    }

    /// Inner send with HTML-fallback logic.
    async fn send_message_inner(
        &self,
        chat_id: i64,
        text: &str,
        reply_to_message_id: Option<i64>,
        message_thread_id: Option<i64>,
        reply_markup: Option<&InlineKeyboardMarkup>,
        link_preview_options: Option<LinkPreviewOptions>,
    ) -> Result<Message> {
        self.send_message_with_html_fallback(SendMessageRawArgs {
            chat_id,
            text,
            reply_to_message_id,
            message_thread_id,
            parse_mode: Some(TELEGRAM_PARSE_MODE),
            reply_markup,
            reply_parameters: None,
            link_preview_options,
        })
        .await
    }

    /// Inner send with `reply_parameters` and HTML-fallback logic.
    async fn send_message_inner_with_reply_params(
        &self,
        chat_id: i64,
        text: &str,
        message_thread_id: Option<i64>,
        reply_markup: Option<&InlineKeyboardMarkup>,
        reply_parameters: Option<ReplyParameters>,
        link_preview_options: Option<LinkPreviewOptions>,
    ) -> Result<Message> {
        self.send_message_with_html_fallback(SendMessageRawArgs {
            chat_id,
            text,
            reply_to_message_id: None,
            message_thread_id,
            parse_mode: Some(TELEGRAM_PARSE_MODE),
            reply_markup,
            reply_parameters,
            link_preview_options,
        })
        .await
    }

    /// Sends `args` as HTML; on a Telegram parse rejection retries with the
    /// text repaired by [`html::sanitize`], and only then drops formatting
    /// entirely by resending the original as plain text.
    async fn send_message_with_html_fallback(
        &self,
        args: SendMessageRawArgs<'_>,
    ) -> Result<Message> {
        let result = self.send_message_raw(args.clone()).await;
        let Err(err) = result else {
            return result;
        };
        if !is_html_parse_error(&err) {
            return Err(err);
        }

        let sanitized = html::sanitize(args.text);
        if sanitized != args.text {
            let mut retry = args.clone();
            retry.text = &sanitized;
            match self.send_message_raw(retry).await {
                Err(retry_err) if is_html_parse_error(&retry_err) => {}
                outcome => return outcome,
            }
        }

        let mut plain = args;
        plain.parse_mode = None;
        self.send_message_raw(plain).await
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
            link_preview_options: args.link_preview_options,
        };
        self.post(
            CallSpec::new_message("sendMessage", args.chat_id, args.message_thread_id),
            &request,
        )
        .await
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

        // On a parse rejection, repair the HTML before giving up on formatting
        let Err(err) = result else {
            return result;
        };
        if !is_html_parse_error(&err) {
            return Err(err);
        }

        let sanitized = html::sanitize(text);
        if sanitized != text {
            match self
                .edit_message_text_raw(
                    chat_id,
                    message_id,
                    &sanitized,
                    Some(TELEGRAM_PARSE_MODE),
                    reply_markup,
                )
                .await
            {
                Err(retry_err) if is_html_parse_error(&retry_err) => {}
                outcome => return outcome,
            }
        }

        self.edit_message_text_raw(chat_id, message_id, text, None, reply_markup)
            .await
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
        let _: Value = self
            .post(CallSpec::update("editMessageText", chat_id), &request)
            .await?;
        Ok(())
    }

    /// Replace only the inline keyboard of a bot message, keeping its text.
    ///
    /// # Errors
    /// Returns an error if the operation fails.
    pub async fn edit_message_reply_markup(
        &self,
        chat_id: i64,
        message_id: i64,
        reply_markup: &InlineKeyboardMarkup,
    ) -> Result<()> {
        let request = EditMessageReplyMarkupRequest {
            chat_id,
            message_id,
            reply_markup,
        };
        let _: Value = self
            .post(
                CallSpec::update("editMessageReplyMarkup", chat_id),
                &request,
            )
            .await?;
        Ok(())
    }

    /// Delete a message. Telegram allows it only for messages sent less than
    /// 48 hours ago: the bot's own, and other users' when it has the
    /// `can_delete_messages` admin right.
    ///
    /// # Errors
    /// Returns an error if the operation fails.
    pub async fn delete_message(&self, chat_id: i64, message_id: i64) -> Result<()> {
        let request = DeleteMessageRequest {
            chat_id,
            message_id,
        };
        let _: bool = self
            .post(CallSpec::update("deleteMessage", chat_id), &request)
            .await?;
        Ok(())
    }

    /// Pin a message in its chat.
    ///
    /// # Errors
    /// Returns an error if the operation fails.
    pub async fn pin_message(
        &self,
        chat_id: i64,
        message_id: i64,
        disable_notification: bool,
    ) -> Result<()> {
        let request = PinChatMessageRequest {
            chat_id,
            message_id,
            disable_notification,
        };
        let _: bool = self
            .post(CallSpec::update("pinChatMessage", chat_id), &request)
            .await?;
        Ok(())
    }

    /// Forward a message (any content type, keeping the "Forwarded from"
    /// attribution) into a chat, optionally into a forum topic. Silent.
    ///
    /// # Errors
    /// Returns an error if the operation fails.
    pub async fn forward_message(
        &self,
        chat_id: i64,
        from_chat_id: i64,
        message_id: i64,
        message_thread_id: Option<i64>,
    ) -> Result<()> {
        let request = ForwardMessageRequest {
            chat_id,
            from_chat_id,
            message_id,
            message_thread_id,
            disable_notification: true,
        };
        let _: Value = self
            .post(
                CallSpec::new_message("forwardMessage", chat_id, message_thread_id),
                &request,
            )
            .await?;
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
        let _: bool = self
            .post(CallSpec::unscoped("answerCallbackQuery"), &request)
            .await?;
        Ok(())
    }

    /// Create a forum topic in a supergroup.
    /// Returns the `message_thread_id` of the created topic.
    ///
    /// # Errors
    /// Returns an error if the operation fails.
    pub async fn create_forum_topic(&self, chat_id: i64, name: &str) -> Result<i64> {
        let request = CreateForumTopicRequest { chat_id, name };
        let topic: ForumTopic = self
            .post(CallSpec::update("createForumTopic", chat_id), &request)
            .await?;
        Ok(topic.message_thread_id)
    }

    /// Rename an existing forum topic.
    ///
    /// # Errors
    /// Returns an error if the operation fails.
    pub async fn edit_forum_topic(
        &self,
        chat_id: i64,
        message_thread_id: i64,
        name: &str,
    ) -> Result<()> {
        let request = EditForumTopicRequest {
            chat_id,
            message_thread_id,
            name,
        };
        let _: bool = self
            .post(CallSpec::update("editForumTopic", chat_id), &request)
            .await?;
        Ok(())
    }

    /// Delete a forum topic with all its messages. Needs the
    /// `can_delete_messages` admin right.
    ///
    /// # Errors
    /// Returns an error if the operation fails.
    pub async fn delete_forum_topic(&self, chat_id: i64, message_thread_id: i64) -> Result<()> {
        let request = DeleteForumTopicRequest {
            chat_id,
            message_thread_id,
        };
        let _: bool = self
            .post(CallSpec::update("deleteForumTopic", chat_id), &request)
            .await?;
        Ok(())
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
        let spec = CallSpec {
            method: "sendChatAction",
            chat: Some(chat_id),
            topic: message_thread_id,
            kind: CallKind::Ephemeral,
        };
        let _: bool = self.post(spec, &request).await?;
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

    /// Uploads one file with optional HTML caption and keyboard. On a caption
    /// parse rejection it retries with the repaired caption, then plain text,
    /// like text messages do.
    async fn send_file(
        &self,
        chat_id: i64,
        upload: &FileUpload<'_>,
        options: &MediaSend<'_>,
    ) -> Result<Message> {
        if upload.data.len() > TELEGRAM_DOCUMENT_MAX_BYTES {
            bail!(
                "{} exceeds Telegram limit ({} bytes > {} bytes)",
                upload.field,
                upload.data.len(),
                TELEGRAM_DOCUMENT_MAX_BYTES
            );
        }
        let caption = options
            .caption
            .map(str::trim)
            .filter(|caption| !caption.is_empty());
        let Some(caption) = caption else {
            return self.send_file_raw(chat_id, upload, options, None).await;
        };

        let result = self
            .send_file_raw(
                chat_id,
                upload,
                options,
                Some((caption, Some(TELEGRAM_PARSE_MODE))),
            )
            .await;
        let Err(err) = result else {
            return result;
        };
        if !is_html_parse_error(&err) {
            return Err(err);
        }
        let sanitized = html::sanitize(caption);
        if sanitized != caption {
            match self
                .send_file_raw(
                    chat_id,
                    upload,
                    options,
                    Some((&sanitized, Some(TELEGRAM_PARSE_MODE))),
                )
                .await
            {
                Err(retry_err) if is_html_parse_error(&retry_err) => {}
                outcome => return outcome,
            }
        }
        self.send_file_raw(chat_id, upload, options, Some((caption, None)))
            .await
    }

    async fn send_file_raw(
        &self,
        chat_id: i64,
        upload: &FileUpload<'_>,
        options: &MediaSend<'_>,
        caption: Option<(&str, Option<&str>)>,
    ) -> Result<Message> {
        let spec = CallSpec::new_message(upload.method, chat_id, options.message_thread_id);
        let build = || -> Result<reqwest::multipart::Form> {
            let part = reqwest::multipart::Part::bytes(upload.data.to_vec())
                .file_name(upload.file_name.to_string())
                .mime_str(upload.mime_type)
                .with_context(|| format!("set {} mime type", upload.field))?;
            let mut form = reqwest::multipart::Form::new()
                .text("chat_id", chat_id.to_string())
                .part(upload.field.to_string(), part);
            if let Some((caption, parse_mode)) = caption {
                form = form.text("caption", caption.to_string());
                if let Some(parse_mode) = parse_mode {
                    form = form.text("parse_mode", parse_mode.to_string());
                }
            }
            if let Some(reply_markup) = options.reply_markup {
                form = form.text(
                    "reply_markup",
                    serde_json::to_string(reply_markup).context("serialize reply_markup")?,
                );
            }
            if let Some(reply_to_message_id) = options.reply_to_message_id {
                form = form
                    .text("reply_to_message_id", reply_to_message_id.to_string())
                    .text("allow_sending_without_reply", "true".to_string());
            }
            if let Some(message_thread_id) = options.message_thread_id {
                form = form.text("message_thread_id", message_thread_id.to_string());
            }
            if let Some(reply_parameters) = &options.reply_parameters {
                form = form.text(
                    "reply_parameters",
                    serde_json::to_string(reply_parameters)
                        .context("serialize reply_parameters")?,
                );
            }
            Ok(form)
        };
        let url = self.url(upload.method);
        self.call(spec, || Ok(self.http.post(&url).multipart(build()?)))
            .await
    }

    fn url(&self, method: &str) -> String {
        format!("{}/bot{}/{}", self.base_url, self.token, method)
    }

    async fn post<T: DeserializeOwned, B: Serialize>(
        &self,
        spec: CallSpec<'_>,
        body: &B,
    ) -> Result<T> {
        let url = self.url(spec.method);
        self.call(spec, || Ok(self.http.post(&url).json(body)))
            .await
    }

    /// Runs one Bot API call through the chat gate, decodes the result, logs
    /// every attempt with its real outcome, and in `Reliable` mode retries
    /// explicit flood refusals. Ambiguous failures (including a result that
    /// does not decode) are never retried: the message may already be in the
    /// chat.
    ///
    /// The retry budget is per request. An HTML-repair or plain-text fallback
    /// and an invalid-reply resend are new requests with their own budget, and
    /// the deadline bounds the start of the last attempt, not its completion
    /// (the HTTP timeout still applies).
    async fn call<T: DeserializeOwned>(
        &self,
        spec: CallSpec<'_>,
        build: impl Fn() -> Result<reqwest::RequestBuilder>,
    ) -> Result<T> {
        let first_attempt = Instant::now();
        // Waits are bounded too: a call that cannot be admitted within the
        // retry deadline fails without sending rather than waiting longer.
        let admission_limit = first_attempt + RetryPolicy::IMPORTANT.deadline;
        let mut attempt = 0u32;
        loop {
            attempt += 1;
            let waited = match self.before_call(&spec, admission_limit).await {
                Ok(waited) => waited,
                Err(err) => {
                    self.log(&spec, attempt, Duration::ZERO, Duration::ZERO, Err(&err));
                    return Err(err);
                }
            };
            let started = Instant::now();
            let result = self
                .send_once(spec.method, build())
                .await
                .and_then(|value| flood::decode_result::<T>(spec.method, value));
            self.log(
                &spec,
                attempt,
                waited,
                started.elapsed(),
                result.as_ref().map(|(message_id, _)| *message_id),
            );
            let err = match result {
                Ok((_, decoded)) => return Ok(decoded),
                Err(err) => err,
            };
            let FailureClass::FloodWait(retry_after) = flood::classify(&err) else {
                return Err(err);
            };
            if let Some(chat) = spec.chat {
                self.gate.with_chat(chat, |pacer| {
                    pacer.note_flood_wait(Instant::now(), retry_after);
                });
            }
            if self.mode != SendMode::Reliable
                || RetryPolicy::IMPORTANT
                    .delay_before_retry(attempt, first_attempt.elapsed(), retry_after)
                    .is_none()
            {
                return Err(err);
            }
            if spec.chat.is_none() {
                tokio::time::sleep(retry_after).await;
            }
        }
    }

    /// Applies the chat gate. Returns how long the call waited, or a
    /// [`FloodWaitActive`] error when the call is skipped: immediately in
    /// `Immediate` mode and for typing, or when admission would come after
    /// `limit`.
    async fn before_call(&self, spec: &CallSpec<'_>, limit: Instant) -> Result<Duration> {
        let Some(chat) = spec.chat else {
            return Ok(Duration::ZERO);
        };
        let started = Instant::now();
        let may_wait = self.mode != SendMode::Immediate && spec.kind != CallKind::Ephemeral;
        if !may_wait {
            let remaining = self.gate.with_chat(chat, |pacer| {
                let remaining = pacer.cooldown_remaining(started);
                if remaining.is_none() && spec.kind == CallKind::NewMessage {
                    pacer.record_new_message(started);
                }
                remaining
            });
            return match remaining {
                Some(remaining) => Err(skipped(spec, chat, remaining)),
                None => Ok(Duration::ZERO),
            };
        }

        // Admission is checked and recorded in one step at send time; a
        // refusal or another sender may move it while this call sleeps.
        loop {
            let now = Instant::now();
            let admit_at = self.gate.with_chat(chat, |pacer| {
                if spec.kind == CallKind::NewMessage {
                    pacer.try_admit_new_message(now, chat < 0).err()
                } else {
                    pacer
                        .cooldown_remaining(now)
                        .map(|remaining| now + remaining)
                }
            });
            let Some(admit_at) = admit_at else {
                return Ok(started.elapsed());
            };
            if admit_at > limit {
                return Err(skipped(spec, chat, admit_at - now));
            }
            tokio::time::sleep(admit_at - now).await;
        }
    }

    async fn send_once(
        &self,
        method: &str,
        request: Result<reqwest::RequestBuilder>,
    ) -> Result<Value> {
        let response = request?
            .send()
            .await
            .map_err(|err| TelegramTransportError::from_reqwest(method, err))?;
        let bytes = response
            .bytes()
            .await
            .map_err(|err| TelegramTransportError::from_reqwest(method, err))?;
        flood::parse_response(method, &bytes)
    }

    fn log(
        &self,
        spec: &CallSpec<'_>,
        attempt: u32,
        waited: Duration,
        duration: Duration,
        outcome: Result<Option<i64>, &anyhow::Error>,
    ) {
        flood::log_call(
            &CallRecord {
                client: "bot",
                bot: flood::bot_identity(&self.token),
                method: spec.method,
                chat: spec.chat,
                topic: spec.topic,
                new_message: spec.kind == CallKind::NewMessage,
                attempt,
                waited,
                duration,
            },
            outcome,
        );
    }
}

fn skipped(spec: &CallSpec<'_>, chat: i64, remaining: Duration) -> anyhow::Error {
    FloodWaitActive {
        method: spec.method.to_string(),
        chat_id: chat,
        remaining,
    }
    .into()
}

/// Telegram's description for an API refusal anywhere in the chain.
pub(crate) fn api_error_description(err: &anyhow::Error) -> Option<&str> {
    err.chain()
        .find_map(|cause| cause.downcast_ref::<TelegramApiError>())
        .map(|api| api.description.as_str())
}

/// The reply target vanished or is invalid; resending without it is safe
/// because the first request was refused.
pub(crate) fn is_invalid_reply_error(err: &anyhow::Error) -> bool {
    api_error_description(err).is_some_and(|description| {
        description.contains("REPLY_MESSAGE_ID_INVALID")
            || description.contains("message to be replied not found")
    })
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
    #[serde(skip_serializing_if = "Option::is_none")]
    scope: Option<BotCommandScopeValue>,
}

#[derive(Debug, Serialize)]
struct DeleteMyCommandsRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    scope: Option<BotCommandScopeValue>,
}

#[derive(Debug, Clone, Serialize)]
struct BotCommandScopeValue {
    r#type: &'static str,
}

const BOT_COMMAND_SCOPES: &[BotCommandScopeValue] = &[
    BotCommandScopeValue { r#type: "default" },
    BotCommandScopeValue {
        r#type: "all_private_chats",
    },
    BotCommandScopeValue {
        r#type: "all_group_chats",
    },
    BotCommandScopeValue {
        r#type: "all_chat_administrators",
    },
];

#[derive(Debug, Clone, Serialize)]
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

/// `link_preview_options` for `sendMessage`.
///
/// Only the disable flag is modelled: the bot appends its own deep link to
/// answers, and Telegram would otherwise attach a preview card for it.
#[derive(Debug, Clone, Copy, Serialize)]
struct LinkPreviewOptions {
    is_disabled: bool,
}

const NO_LINK_PREVIEW: LinkPreviewOptions = LinkPreviewOptions { is_disabled: true };

#[derive(Clone)]
struct SendMessageRawArgs<'a> {
    chat_id: i64,
    text: &'a str,
    reply_to_message_id: Option<i64>,
    message_thread_id: Option<i64>,
    parse_mode: Option<&'a str>,
    reply_markup: Option<&'a InlineKeyboardMarkup>,
    reply_parameters: Option<ReplyParameters>,
    link_preview_options: Option<LinkPreviewOptions>,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    link_preview_options: Option<LinkPreviewOptions>,
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

#[derive(Debug, Serialize)]
struct EditForumTopicRequest<'a> {
    chat_id: i64,
    message_thread_id: i64,
    name: &'a str,
}

#[derive(Debug, Serialize)]
struct DeleteForumTopicRequest {
    chat_id: i64,
    message_thread_id: i64,
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
struct EditMessageReplyMarkupRequest<'a> {
    chat_id: i64,
    message_id: i64,
    reply_markup: &'a InlineKeyboardMarkup,
}

#[derive(Debug, Serialize)]
struct DeleteMessageRequest {
    chat_id: i64,
    message_id: i64,
}

#[derive(Debug, Serialize)]
struct PinChatMessageRequest {
    chat_id: i64,
    message_id: i64,
    disable_notification: bool,
}

#[derive(Debug, Serialize)]
struct ForwardMessageRequest {
    chat_id: i64,
    from_chat_id: i64,
    message_id: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    message_thread_id: Option<i64>,
    disable_notification: bool,
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
    use std::time::{Duration, Instant};

    use zdx_engine::telegram::flood::{self, FailureClass};

    use super::fake::{FakeTelegram, Reply, TOKEN};
    use super::{
        InlineKeyboardButton, InlineKeyboardMarkup, MediaSend, TelegramClient, TextSend,
        is_photo_aspect_ratio_invalid, is_photo_long_edge_too_large,
    };

    const CHAT: i64 = -1001;

    fn client(fake: &FakeTelegram) -> TelegramClient {
        TelegramClient::with_base_url(TOKEN.to_string(), fake.base_url.clone())
    }

    fn text(text: &str) -> TextSend<'_> {
        TextSend {
            chat_id: CHAT,
            text,
            reply_to_message_id: None,
            message_thread_id: Some(7),
            reply_parameters: None,
            reply_markup: None,
            disable_preview: true,
        }
    }

    /// A document, voice note, or audio file over the upload limit is refused
    /// from its size alone: the file here cannot be read at all (mode 000),
    /// so the size error proves the bytes were never loaded, and no request
    /// is made. A file within the limit still uploads.
    #[cfg(unix)]
    #[tokio::test]
    async fn oversized_uploads_are_refused_before_the_file_is_read() {
        use std::os::unix::fs::PermissionsExt as _;

        let fake = FakeTelegram::start().await;
        let dir = std::env::temp_dir().join(format!("zdx-upload-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).expect("dir");
        let client = client(&fake);
        let options = MediaSend::default();
        for (name, field) in [
            ("big.pdf", "document"),
            ("big.ogg", "voice"),
            ("big.mp3", "audio"),
        ] {
            let path = dir.join(name);
            let file = std::fs::File::create(&path).expect("create");
            file.set_len(super::TELEGRAM_DOCUMENT_MAX_BYTES as u64 + 1)
                .expect("sparse size");
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o000)).expect("chmod");
            let err = match field {
                "document" => client.send_document_from_path(CHAT, &path, &options).await,
                "voice" => client.send_voice_from_path(CHAT, &path, &options).await,
                _ => client.send_audio_from_path(CHAT, &path, &options).await,
            }
            .expect_err("over the limit");
            assert_eq!(
                format!("{err:#}"),
                format!(
                    "{field} exceeds Telegram limit ({} bytes > {} bytes)",
                    super::TELEGRAM_DOCUMENT_MAX_BYTES + 1,
                    super::TELEGRAM_DOCUMENT_MAX_BYTES
                )
            );
        }
        assert!(fake.methods().is_empty(), "no upload was attempted");

        let small = dir.join("small.pdf");
        std::fs::write(&small, b"%PDF-1.4 synthetic").expect("write");
        client
            .send_document_from_path(CHAT, &small, &options)
            .await
            .expect("within the limit");
        assert_eq!(fake.methods(), ["sendDocument"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn reliable_send_waits_out_a_flood_refusal_and_retries() {
        let fake = FakeTelegram::start().await;
        fake.script(
            "sendMessage",
            &[Reply::Error(
                429,
                "Too Many Requests: retry after 1",
                Some(1),
            )],
        );
        let started = Instant::now();
        let sent = client(&fake).reliable().send_text(&text("hi")).await;
        assert!(sent.is_ok(), "{sent:?}");
        assert_eq!(fake.methods(), ["sendMessage", "sendMessage"]);
        assert!(started.elapsed() >= Duration::from_millis(900));
    }

    #[tokio::test]
    async fn reliable_send_gives_up_when_the_wait_exceeds_the_deadline() {
        let fake = FakeTelegram::start().await;
        fake.script(
            "sendMessage",
            &[Reply::Error(
                429,
                "Too Many Requests: retry after 500",
                Some(500),
            )],
        );
        let reliable = client(&fake).reliable();
        let err = reliable.send_text(&text("hi")).await.unwrap_err();
        assert_eq!(
            flood::classify(&err),
            FailureClass::FloodWait(Duration::from_secs(500))
        );
        assert_eq!(fake.methods(), ["sendMessage"]);

        // A later reliable or paced call to the chat cannot be admitted within
        // the deadline either: it fails at once, without a request.
        let started = Instant::now();
        assert!(reliable.send_text(&text("summary")).await.is_err());
        assert!(reliable.paced().delete_message(CHAT, 5).await.is_err());
        assert!(started.elapsed() < Duration::from_secs(1));
        assert_eq!(fake.methods(), ["sendMessage"]);
    }

    /// A flood wait that grows while a reliable retry is waiting (another
    /// refusal for the same chat) delays the retry to the new end.
    #[tokio::test]
    async fn a_retry_waits_for_a_cooldown_extended_while_it_waits() {
        let fake = FakeTelegram::start().await;
        fake.script(
            "sendMessage",
            &[Reply::Error(
                429,
                "Too Many Requests: retry after 1",
                Some(1),
            )],
        );
        let reliable = client(&fake).reliable();
        let gate = std::sync::Arc::clone(&reliable.gate);
        let send = tokio::spawn(async move { reliable.send_text(&text("hi")).await });
        tokio::time::sleep(Duration::from_millis(300)).await;
        gate.with_chat(CHAT, |pacer| {
            pacer.note_flood_wait(Instant::now(), Duration::from_secs(3));
        });

        assert!(send.await.expect("join").is_ok());
        let seen = fake.seen();
        assert_eq!(seen.len(), 2);
        let gap = seen[1].at.duration_since(seen[0].at);
        assert!(gap >= Duration::from_millis(3200), "gap was {gap:?}");
    }

    /// An extension past the retry deadline ends the wait without a request.
    #[tokio::test]
    async fn a_retry_gives_up_when_the_cooldown_grows_past_the_deadline() {
        let fake = FakeTelegram::start().await;
        fake.script(
            "sendMessage",
            &[Reply::Error(
                429,
                "Too Many Requests: retry after 1",
                Some(1),
            )],
        );
        let reliable = client(&fake).reliable();
        let gate = std::sync::Arc::clone(&reliable.gate);
        let send = tokio::spawn(async move { reliable.send_text(&text("hi")).await });
        tokio::time::sleep(Duration::from_millis(300)).await;
        gate.with_chat(CHAT, |pacer| {
            pacer.note_flood_wait(Instant::now(), Duration::from_secs(500));
        });

        let err = send.await.expect("join").unwrap_err();
        assert!(matches!(flood::classify(&err), FailureClass::FloodWait(_)));
        assert_eq!(fake.methods(), ["sendMessage"]);
    }

    #[tokio::test]
    async fn concurrent_senders_released_by_a_flood_wait_stay_spaced() {
        let fake = FakeTelegram::start().await;
        fake.script(
            "sendMessage",
            &[Reply::Error(
                429,
                "Too Many Requests: retry after 1",
                Some(1),
            )],
        );
        let paced = client(&fake).paced();
        assert!(
            paced
                .send_message(CHAT, "one", None, Some(7))
                .await
                .is_err()
        );

        // Two topics of the same chat wait out the same flood wait.
        let (a, b) = tokio::join!(
            paced.send_message(CHAT, "a", None, Some(7)),
            paced.send_message(CHAT, "b", None, Some(8)),
        );
        assert!(a.is_ok() && b.is_ok());
        let seen = fake.seen();
        assert_eq!(seen.len(), 3);
        let gap = seen[2].at.duration_since(seen[1].at);
        assert!(gap >= Duration::from_millis(950), "gap was {gap:?}");
    }

    #[tokio::test]
    async fn ambiguous_answers_are_never_resent() {
        let fake = FakeTelegram::start().await;
        fake.script("sendMessage", &[Reply::Garbage]);
        let err = client(&fake)
            .reliable()
            .send_text(&text("hi"))
            .await
            .unwrap_err();
        assert_eq!(flood::classify(&err), FailureClass::Unknown);
        assert_eq!(fake.methods(), ["sendMessage"]);
    }

    #[tokio::test]
    async fn accepted_but_undecodable_answers_are_unknown_and_not_echoed() {
        let fake = FakeTelegram::start().await;
        fake.script("sendMessage", &[Reply::Undecodable]);
        let err = client(&fake)
            .reliable()
            .send_text(&text("hi"))
            .await
            .unwrap_err();
        assert_eq!(flood::classify(&err), FailureClass::Unknown);
        assert!(!format!("{err:#}").contains("secret-marker"));
        assert_eq!(fake.methods(), ["sendMessage"]);
    }

    #[tokio::test]
    async fn immediate_calls_skip_a_chat_in_flood_wait_without_a_request() {
        let fake = FakeTelegram::start().await;
        fake.script(
            "sendMessage",
            &[Reply::Error(
                429,
                "Too Many Requests: retry after 30",
                Some(30),
            )],
        );
        let client = client(&fake);
        assert!(
            client
                .send_message(CHAT, "one", None, Some(7))
                .await
                .is_err()
        );

        // Same chat, other topic and other methods: skipped locally.
        let err = client
            .send_message(CHAT, "two", None, Some(8))
            .await
            .unwrap_err();
        assert!(matches!(flood::classify(&err), FailureClass::FloodWait(_)));
        assert!(client.edit_message_text(CHAT, 5, "x", None).await.is_err());
        assert!(
            client
                .send_chat_action(CHAT, "typing", Some(7))
                .await
                .is_err()
        );
        assert_eq!(fake.methods(), ["sendMessage"]);

        // Another chat is not held back by ZDX.
        assert!(
            client
                .send_message(-2002, "three", None, None)
                .await
                .is_ok()
        );
        assert_eq!(fake.methods(), ["sendMessage", "sendMessage"]);
    }

    #[tokio::test]
    async fn paced_calls_wait_for_the_flood_wait_to_end() {
        let fake = FakeTelegram::start().await;
        fake.script(
            "sendMessage",
            &[Reply::Error(
                429,
                "Too Many Requests: retry after 1",
                Some(1),
            )],
        );
        let paced = client(&fake).paced();
        // Paced sends make one attempt.
        assert!(paced.send_message(CHAT, "one", None, None).await.is_err());
        let started = Instant::now();
        assert!(paced.delete_message(CHAT, 5).await.is_ok());
        assert!(started.elapsed() >= Duration::from_millis(900));
        assert_eq!(fake.methods(), ["sendMessage", "deleteMessage"]);
    }

    #[tokio::test]
    async fn paced_new_messages_are_spaced_per_chat() {
        let fake = FakeTelegram::start().await;
        let paced = client(&fake).paced();
        let started = Instant::now();
        for _ in 0..3 {
            paced
                .send_message(CHAT, "x", None, None)
                .await
                .expect("send");
        }
        assert!(started.elapsed() >= Duration::from_millis(1900));
    }

    #[tokio::test]
    async fn files_carry_html_caption_and_keyboard() {
        let fake = FakeTelegram::start().await;
        let path = std::env::temp_dir().join(format!("zdx-test-{}.html", uuid::Uuid::new_v4()));
        std::fs::write(&path, "<html></html>").expect("write file");
        let markup = InlineKeyboardMarkup {
            inline_keyboard: vec![vec![InlineKeyboardButton::callback("Next", "fu:0")]],
        };
        let message = client(&fake)
            .send_document_from_path(
                CHAT,
                &path,
                &MediaSend {
                    caption: Some("<b>Report</b>"),
                    reply_markup: Some(&markup),
                    message_thread_id: Some(7),
                    ..MediaSend::default()
                },
            )
            .await
            .expect("send document");
        let _ = std::fs::remove_file(&path);

        assert_eq!(message.id, 101);
        let seen = fake.seen();
        assert_eq!(seen.len(), 1);
        let body = &seen[0].body;
        assert!(body.contains("<b>Report</b>"));
        assert!(body.contains("name=\"parse_mode\"\r\n\r\nHTML"));
        assert!(body.contains("fu:0"));
    }

    #[tokio::test]
    async fn transport_errors_do_not_leak_the_token() {
        // Nothing listens on this port.
        let client =
            TelegramClient::with_base_url(TOKEN.to_string(), "http://127.0.0.1:9".to_string());
        let err = client
            .send_message(CHAT, "hi", None, None)
            .await
            .unwrap_err();
        assert!(!format!("{err:#}").contains("test-secret"));
        assert_eq!(flood::classify(&err), FailureClass::NotSent);
    }

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
