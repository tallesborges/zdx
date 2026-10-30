use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use tokio_util::sync::CancellationToken;
use zdx_engine::config::{Config, paths};
use zdx_engine::telegram::flood::{TelegramApiError, TelegramTransportError};

use crate::telegram::{Audio, Document, Message, PhotoSize, TelegramClient, Video, Voice};
use crate::transcribe;
use crate::types::{
    ImageDelivery, IncomingAudio, IncomingDocument, IncomingImage, IncomingMessage, IncomingVideo,
    LocalAttachment, VideoStatus,
};

/// Bot API `getFile` refuses anything larger, so a bigger file can never be
/// downloaded.
const TELEGRAM_DOWNLOAD_LIMIT_BYTES: u64 = 20 * 1024 * 1024;
/// Images go to the model at most this size. Smaller than the provider cap on
/// purpose: it keeps uploads fast and cheap and still leaves screenshot text
/// legible; 1568px is Anthropic's standard-tier long edge, which it would
/// downscale to anyway. The saved file keeps full resolution.
const INLINE_IMAGE_EDGE: u32 = 1568;
const INLINE_IMAGE_BYTES: usize = 1024 * 1024;
const MAX_AUDIO_BYTES: u64 = 25 * 1024 * 1024; // 25MB
const MAX_DOCUMENT_BYTES: u64 = 25 * 1024 * 1024; // 25MB
const MAX_VIDEO_BYTES: u64 = zdx_engine::media::MAX_INLINE_BYTES as u64;
const SUPPORTED_IMAGE_MIMES: &[&str] = &["image/jpeg", "image/png", "image/gif", "image/webp"];
/// Tries per Telegram file (`getFile` plus the file fetch). A slow link to
/// Telegram drops single requests now and then; a fresh try usually works.
const DOWNLOAD_ATTEMPTS: u32 = 3;
/// Wait before retry `n` is `n` times this.
#[cfg(not(test))]
const DOWNLOAD_RETRY_DELAY: Duration = Duration::from_secs(2);
#[cfg(test)]
const DOWNLOAD_RETRY_DELAY: Duration = Duration::from_millis(10);

pub(crate) struct AllowlistConfig<'a> {
    pub user_ids: &'a HashSet<i64>,
    pub chat_ids: &'a HashSet<i64>,
}

struct MessageTarget {
    chat: i64,
    message: i64,
    thread: Option<i64>,
}

struct AttachmentCollection {
    images: Vec<IncomingImage>,
    audios: Vec<IncomingAudio>,
    documents: Vec<IncomingDocument>,
    videos: Vec<IncomingVideo>,
    notices: Vec<String>,
    had_attachments: bool,
}

/// An attachment that could not be loaded, with the sentence the agent gets
/// in its place. Any other load error yields a generic notice.
#[derive(Debug)]
struct AttachmentNotice(String);

impl std::fmt::Display for AttachmentNotice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for AttachmentNotice {}

/// A Telegram attachment whose download still failed on a network or server
/// error after every try. The message is not answered without it: the
/// handler keeps the message and offers to load it again, because Telegram
/// still has the file.
#[derive(Debug)]
pub(crate) struct DownloadUnavailable {
    /// What the user sent, e.g. `voice message`.
    pub label: String,
}

impl std::fmt::Display for DownloadUnavailable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} could not be downloaded from Telegram", self.label)
    }
}

impl std::error::Error for DownloadUnavailable {}

struct FileRef<'a> {
    id: &'a str,
    size: Option<u64>,
    name: Option<&'a str>,
}

impl<'a> From<&'a Video> for FileRef<'a> {
    fn from(video: &'a Video) -> Self {
        Self {
            id: &video.file_id,
            size: video.file_size,
            name: video.file_name.as_deref(),
        }
    }
}

impl<'a> From<&'a Document> for FileRef<'a> {
    fn from(document: &'a Document) -> Self {
        Self {
            id: &document.file_id,
            size: document.file_size,
            name: document.file_name.as_deref(),
        }
    }
}

struct AudioSource<'a> {
    file_id: &'a str,
    file_size: Option<u64>,
    mime_type: Option<&'a str>,
    file_name_hint: Option<&'a str>,
}

///
/// # Errors
/// [`DownloadUnavailable`] when a Telegram attachment could not be downloaded
/// after every try; any other error if the operation fails.
pub(crate) async fn parse_incoming_message(
    client: &TelegramClient,
    allowlist: AllowlistConfig<'_>,
    config: &Config,
    message: &Message,
    cancel_token: Option<&CancellationToken>,
) -> Result<Option<IncomingMessage>> {
    let target = MessageTarget {
        chat: message.chat.id,
        message: message.id,
        thread: message.effective_thread_id(),
    };

    let Some(user_id) = validate_access(client, allowlist, message, &target).await? else {
        return Ok(None);
    };

    let mut text = extract_text(message);
    let attachments = collect_attachments(client, config, message, &target, cancel_token).await?;
    let AttachmentCollection {
        images,
        audios,
        documents,
        videos,
        notices,
        had_attachments,
    } = attachments;

    if text.is_none()
        && images.is_empty()
        && audios.is_empty()
        && documents.is_empty()
        && videos.is_empty()
        && notices.is_empty()
    {
        return handle_empty_message(client, &target, had_attachments).await;
    }

    if text.as_deref().is_some_and(|value| value.trim().is_empty()) {
        text = None;
    }

    Ok(Some(IncomingMessage {
        chat_id: target.chat,
        message_id: target.message,
        user_id,
        text,
        images,
        audios,
        documents,
        videos,
        notices,
        message_thread_id: target.thread,
        is_forum: message.chat.is_forum_enabled(),
        replied_to: replied_to(message),
    }))
}

/// Longest replied-to text kept in the prompt, in characters.
const MAX_REPLIED_TO_CHARS: usize = 3000;

/// The message this one explicitly replies to, when it has text to show.
///
/// In a forum topic Telegram sets `reply_to_message` on every message to the
/// topic's creation message (whose id is the topic id), reply or not; that
/// one is not a reply.
fn replied_to(message: &Message) -> Option<crate::types::RepliedTo> {
    let reply = message.reply_to.as_deref()?;
    if message.effective_thread_id() == Some(reply.id) {
        return None;
    }
    let quote = message
        .quote
        .as_ref()
        .map(|quote| quote.text.trim())
        .filter(|text| !text.is_empty());
    let text = match quote {
        Some(quote) => quote.to_string(),
        None => reply
            .plain_text()
            .map(|text| text.trim().to_string())
            .filter(|text| !text.is_empty())
            .or_else(|| {
                reply
                    .caption
                    .as_deref()
                    .map(str::trim)
                    .filter(|caption| !caption.is_empty())
                    .map(str::to_string)
            })?,
    };
    let text = if text.chars().count() > MAX_REPLIED_TO_CHARS {
        let mut cut: String = text.chars().take(MAX_REPLIED_TO_CHARS).collect();
        cut.push_str("\n[…cut]");
        cut
    } else {
        text
    };
    Some(crate::types::RepliedTo {
        message_id: reply.id,
        from_bot: reply.from.as_ref().is_some_and(|user| user.is_bot),
        text,
        quoted: quote.is_some(),
    })
}

async fn validate_access(
    client: &TelegramClient,
    allowlist: AllowlistConfig<'_>,
    message: &Message,
    target: &MessageTarget,
) -> Result<Option<i64>> {
    let Some(user) = message.from.as_ref() else {
        tracing::debug!(chat_id = target.chat, "Ignoring message without sender");
        return Ok(None);
    };

    if user.is_bot {
        return Ok(None);
    }

    let user_allowed = allowlist.user_ids.contains(&user.id);

    if message.chat.is_group() {
        if !allowlist.chat_ids.contains(&target.chat) {
            // Mirror the queue-level gate: allow /whereami in non-allowlisted
            // groups only when the sender is allowlisted, so the operator can
            // discover the chat ID before binding it.
            let is_whereami = message
                .text
                .as_deref()
                .and_then(crate::commands::parse_command)
                .is_some_and(|cmd| matches!(cmd, crate::commands::BotCommand::WhereAmI));
            if user_allowed && is_whereami {
                return Ok(Some(user.id));
            }
            tracing::debug!(chat_id = target.chat, "Ignoring non-allowlisted group chat");
            return Ok(None);
        }
    } else if !message.chat.is_private() {
        tracing::debug!(chat_id = target.chat, "Ignoring unsupported chat type");
        return Ok(None);
    }

    if user_allowed {
        return Ok(Some(user.id));
    }

    tracing::warn!(user_id = user.id, chat_id = target.chat, "Denied user");
    let _ = client
        .send_message(
            target.chat,
            "Access denied.",
            Some(target.message),
            target.thread,
        )
        .await;
    Ok(None)
}

#[allow(clippy::too_many_lines)]
async fn collect_attachments(
    client: &TelegramClient,
    config: &Config,
    message: &Message,
    target: &MessageTarget,
    cancel_token: Option<&CancellationToken>,
) -> Result<AttachmentCollection> {
    let mut images = Vec::new();
    let mut audios = Vec::new();
    let mut documents = Vec::new();
    let mut videos = Vec::new();
    let mut notices = Vec::new();
    let mut had_attachments = false;

    for current in std::iter::once(message).chain(message.grouped_messages.iter()) {
        let current_message_id = current.id;

        if let Some(photos) = current.photo.as_deref() {
            had_attachments = true;
            if let Some(photo) = select_best_photo(photos) {
                let source = FileRef {
                    id: &photo.file_id,
                    size: photo.file_size,
                    name: None,
                };
                push_attachment(
                    &mut images,
                    &mut notices,
                    load_image(client, target.chat, current_message_id, &source),
                    "photo",
                )
                .await?;
            }
        }

        if let Some(document) = current.document.as_ref()
            && let Some(mime) = document.mime_type.as_deref()
            && current.animation.is_none()
            && !mime.starts_with("video/")
        {
            had_attachments = true;
            if mime.starts_with("image/") {
                push_attachment(
                    &mut images,
                    &mut notices,
                    load_image(
                        client,
                        target.chat,
                        current_message_id,
                        &FileRef::from(document),
                    ),
                    "image file",
                )
                .await?;
            } else if mime.starts_with("audio/") {
                push_attachment(
                    &mut audios,
                    &mut notices,
                    load_audio_attachment(
                        client,
                        config,
                        target.chat,
                        current_message_id,
                        document,
                        cancel_token,
                    ),
                    "audio file",
                )
                .await?;
            } else {
                push_attachment(
                    &mut documents,
                    &mut notices,
                    load_generic_document(client, target.chat, current_message_id, document),
                    "file",
                )
                .await?;
            }
        }

        if let Some(voice) = current.voice.as_ref() {
            had_attachments = true;
            push_attachment(
                &mut audios,
                &mut notices,
                load_voice_attachment(
                    client,
                    config,
                    target.chat,
                    current_message_id,
                    voice,
                    cancel_token,
                ),
                "voice message",
            )
            .await?;
        }

        if let Some(audio) = current.audio.as_ref() {
            had_attachments = true;
            push_attachment(
                &mut audios,
                &mut notices,
                load_audio_message(
                    client,
                    config,
                    target.chat,
                    current_message_id,
                    audio,
                    cancel_token,
                ),
                "audio message",
            )
            .await?;
        }

        for (kind, video) in video_attachments(current) {
            had_attachments = true;
            push_attachment(
                &mut videos,
                &mut notices,
                load_video(client, target.chat, current_message_id, kind, &video),
                "video",
            )
            .await?;
        }
    }

    for attachment in &message.local_attachments {
        had_attachments = true;
        match attachment {
            LocalAttachment::Image { path, label } => {
                push_attachment(
                    &mut images,
                    &mut notices,
                    async { load_local_image(path, label).map(Some) },
                    "image",
                )
                .await?;
            }
            LocalAttachment::Audio { path, mime, label } => {
                push_attachment(
                    &mut audios,
                    &mut notices,
                    load_local_audio(config, path, mime, label, cancel_token),
                    "voice note",
                )
                .await?;
            }
        }
    }

    Ok(AttachmentCollection {
        images,
        audios,
        documents,
        videos,
        notices,
        had_attachments,
    })
}

/// Telegram video media on one message, labelled by kind, including a file
/// sent with a `video/*` MIME type. A GIF-style `animation` also sets
/// `document` for backward compatibility, so that document is not counted.
fn video_attachments(message: &Message) -> Vec<(&'static str, FileRef<'_>)> {
    let video_document = message.document.as_ref().filter(|document| {
        message.animation.is_none()
            && document
                .mime_type
                .as_deref()
                .is_some_and(|mime| mime.starts_with("video/"))
    });
    [
        ("Video", message.video.as_ref().map(FileRef::from)),
        ("Video note", message.video_note.as_ref().map(FileRef::from)),
        ("Animation", message.animation.as_ref().map(FileRef::from)),
        ("Video file", video_document.map(FileRef::from)),
    ]
    .into_iter()
    .filter_map(|(kind, video)| video.map(|video| (kind, video)))
    .collect()
}

async fn push_attachment<T>(
    output: &mut Vec<T>,
    notices: &mut Vec<String>,
    load_future: impl std::future::Future<Output = Result<Option<T>>>,
    label: &str,
) -> Result<()> {
    match load_future.await {
        Ok(Some(item)) => output.push(item),
        Ok(None) => {}
        Err(err) if transcribe::is_operation_cancelled(&err) => return Err(err),
        Err(err) => {
            tracing::error!(err = %format!("{err:#}"), label, "Failed to load attachment");
            if is_transient_download_error(&err) {
                return Err(DownloadUnavailable {
                    label: label.to_string(),
                }
                .into());
            }
            // The raw error is not passed on: a download error carries the
            // file URL, which embeds the bot token.
            notices.push(match err.downcast_ref::<AttachmentNotice>() {
                Some(notice) => notice.0.clone(),
                None => format!(
                    "One attachment ({label}) could not be loaded from Telegram, so it is missing from this message. Tell the user and ask them to resend it."
                ),
            });
        }
    }
    Ok(())
}

async fn handle_empty_message(
    client: &TelegramClient,
    target: &MessageTarget,
    had_attachments: bool,
) -> Result<Option<IncomingMessage>> {
    if had_attachments {
        tracing::warn!(chat_id = target.chat, "Unsupported attachment");
        let _ = client
            .send_message(
                target.chat,
                "Sorry, I couldn't read that attachment.",
                Some(target.message),
                target.thread,
            )
            .await;
    } else {
        tracing::debug!(chat_id = target.chat, "Ignoring empty message");
    }
    Ok(None)
}

fn extract_text(message: &Message) -> Option<String> {
    let mut parts = Vec::new();
    for current in std::iter::once(message).chain(message.grouped_messages.iter()) {
        if let Some(text) = current.plain_text() {
            let trimmed = text.trim();
            if !trimmed.is_empty() {
                parts.push(trimmed.to_string());
            }
        }
        if let Some(caption) = current.caption.as_deref() {
            let trimmed = caption.trim();
            if !trimmed.is_empty() {
                parts.push(trimmed.to_string());
            }
        }
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join("\n\n"))
    }
}

fn select_best_photo(photos: &[PhotoSize]) -> Option<&PhotoSize> {
    photos.iter().max_by_key(|photo| {
        let size = photo.file_size.unwrap_or(0);
        let width = u64::try_from(photo.width.max(0)).unwrap_or(0);
        let height = u64::try_from(photo.height.max(0)).unwrap_or(0);
        let area = width * height;
        (size, area)
    })
}

/// Downloads an image, saves the original, and prepares the copy the model
/// sees. An image the model cannot take inline is still saved and described,
/// never dropped.
async fn load_image(
    client: &TelegramClient,
    chat_id: i64,
    message_id: i64,
    source: &FileRef<'_>,
) -> Result<Option<IncomingImage>> {
    if let Some(size) = source
        .size
        .filter(|size| *size > TELEGRAM_DOWNLOAD_LIMIT_BYTES)
    {
        return Err(AttachmentNotice(format!(
            "An image attachment was not downloaded: it is {} and Telegram bots can only download files up to {}. Ask the user for a smaller file.",
            format_mib(size),
            format_mib(TELEGRAM_DOWNLOAD_LIMIT_BYTES)
        ))
        .into());
    }

    let (file_path, bytes) = download_file_bytes(client, source.id, None).await?;
    let file_name = source
        .name
        .and_then(file_name_from_path)
        .or_else(|| file_name_from_path(&file_path))
        .unwrap_or_else(|| format!("image_{message_id}.bin"));
    let local_path = save_media_bytes(chat_id, message_id, &file_name, &bytes)?;
    let delivery = prepare_inline_image(&local_path, &bytes);

    Ok(Some(IncomingImage {
        local_path,
        delivery,
        label: None,
    }))
}

/// Shrinks an image to the inline limits, or explains why the model cannot
/// see it. An image the decoder cannot read (GIF, for one) is sent as-is when
/// it is a type providers accept and already within the byte limit.
fn prepare_inline_image(local_path: &Path, bytes: &[u8]) -> ImageDelivery {
    if let Some((resized, mime_type)) =
        zdx_engine::images::downscale_to_fit(bytes, INLINE_IMAGE_EDGE, INLINE_IMAGE_BYTES)
    {
        return ImageDelivery::Inline {
            mime_type: mime_type.to_string(),
            data: BASE64.encode(resized),
            downscaled: true,
        };
    }

    let detected = infer::get(bytes).map(|kind| kind.mime_type());
    let reason = match detected {
        Some(mime) if SUPPORTED_IMAGE_MIMES.contains(&mime) => {
            if bytes.len() <= INLINE_IMAGE_BYTES {
                return ImageDelivery::Inline {
                    mime_type: mime.to_string(),
                    data: BASE64.encode(bytes),
                    downscaled: false,
                };
            }
            format!(
                "it is {} and could not be shrunk below the {} inline limit",
                format_mib(bytes.len() as u64),
                format_mib(INLINE_IMAGE_BYTES as u64)
            )
        }
        Some(mime) => format!("the model cannot take {mime} images directly"),
        None => "its image format was not recognized".to_string(),
    };

    ImageDelivery::SavedOnly {
        reason,
        ask_media: zdx_engine::media::can_ask_media(local_path, bytes.len() as u64),
    }
}

fn format_mib(bytes: u64) -> String {
    format!("{:.1} MiB", bytes as f64 / (1024.0 * 1024.0))
}

async fn load_generic_document(
    client: &TelegramClient,
    chat_id: i64,
    message_id: i64,
    document: &Document,
) -> Result<Option<IncomingDocument>> {
    if document.file_size.unwrap_or(0) > MAX_DOCUMENT_BYTES {
        tracing::debug!(chat_id, "Skipping document exceeding max size");
        return Ok(None);
    }

    let (file_path, bytes) = download_file_bytes(client, &document.file_id, None).await?;
    if bytes.len() as u64 > MAX_DOCUMENT_BYTES {
        tracing::debug!(chat_id, "Downloaded document exceeds max size");
        return Ok(None);
    }

    let file_name = document
        .file_name
        .as_deref()
        .and_then(file_name_from_path)
        .or_else(|| file_name_from_path(&file_path))
        .unwrap_or_else(|| format!("document_{message_id}.bin"));
    let local_path = save_media_bytes(chat_id, message_id, &file_name, &bytes)?;

    Ok(Some(IncomingDocument {
        local_path,
        file_name,
    }))
}

/// Downloads a video for `ask_media`. The limit is `ask_media`'s inline cap, so
/// a saved video is always one the agent can inspect; a larger one is reported
/// to the agent instead of dropped.
async fn load_video(
    client: &TelegramClient,
    chat_id: i64,
    message_id: i64,
    kind: &'static str,
    video: &FileRef<'_>,
) -> Result<Option<IncomingVideo>> {
    let too_large = |size_bytes| {
        tracing::debug!(
            chat_id,
            size_bytes,
            "Skipping video exceeding ask_media limit"
        );
        Ok(Some(IncomingVideo {
            kind,
            status: VideoStatus::TooLarge { size_bytes },
        }))
    };
    if let Some(size) = video.size.filter(|size| *size > MAX_VIDEO_BYTES) {
        return too_large(size);
    }

    let (file_path, bytes) = download_file_bytes(client, video.id, None).await?;
    if bytes.len() as u64 > MAX_VIDEO_BYTES {
        return too_large(bytes.len() as u64);
    }

    let file_name = video
        .name
        .and_then(file_name_from_path)
        .or_else(|| file_name_from_path(&file_path))
        .unwrap_or_else(|| format!("video_{message_id}.mp4"));
    let local_path = save_media_bytes(chat_id, message_id, &file_name, &bytes)?;

    Ok(Some(IncomingVideo {
        kind,
        status: VideoStatus::Saved(local_path),
    }))
}

async fn load_voice_attachment(
    client: &TelegramClient,
    config: &Config,
    chat_id: i64,
    message_id: i64,
    voice: &Voice,
    cancel_token: Option<&CancellationToken>,
) -> Result<Option<IncomingAudio>> {
    load_audio_by_id(
        client,
        config,
        chat_id,
        message_id,
        AudioSource {
            file_id: &voice.file_id,
            file_size: voice.file_size,
            mime_type: voice.mime_type.as_deref(),
            file_name_hint: None,
        },
        cancel_token,
    )
    .await
}

async fn load_audio_message(
    client: &TelegramClient,
    config: &Config,
    chat_id: i64,
    message_id: i64,
    audio: &Audio,
    cancel_token: Option<&CancellationToken>,
) -> Result<Option<IncomingAudio>> {
    load_audio_by_id(
        client,
        config,
        chat_id,
        message_id,
        AudioSource {
            file_id: &audio.file_id,
            file_size: audio.file_size,
            mime_type: audio.mime_type.as_deref(),
            file_name_hint: audio.file_name.as_deref(),
        },
        cancel_token,
    )
    .await
}

async fn load_audio_attachment(
    client: &TelegramClient,
    config: &Config,
    chat_id: i64,
    message_id: i64,
    document: &Document,
    cancel_token: Option<&CancellationToken>,
) -> Result<Option<IncomingAudio>> {
    load_audio_by_id(
        client,
        config,
        chat_id,
        message_id,
        AudioSource {
            file_id: &document.file_id,
            file_size: document.file_size,
            mime_type: document.mime_type.as_deref(),
            file_name_hint: document.file_name.as_deref(),
        },
        cancel_token,
    )
    .await
}

async fn load_audio_by_id(
    client: &TelegramClient,
    config: &Config,
    chat_id: i64,
    message_id: i64,
    source: AudioSource<'_>,
    cancel_token: Option<&CancellationToken>,
) -> Result<Option<IncomingAudio>> {
    if cancel_token.is_some_and(CancellationToken::is_cancelled) {
        return Err(transcribe::OperationCancelled.into());
    }

    if source.file_size.unwrap_or(0) > MAX_AUDIO_BYTES {
        tracing::debug!(chat_id, "Skipping audio exceeding max size");
        return Ok(None);
    }

    let (file_path, bytes) = download_file_bytes(client, source.file_id, cancel_token).await?;
    if bytes.len() as u64 > MAX_AUDIO_BYTES {
        tracing::debug!(chat_id, "Downloaded audio exceeds max size");
        return Ok(None);
    }

    let filename = source
        .file_name_hint
        .and_then(file_name_from_path)
        .or_else(|| file_name_from_path(&file_path))
        .unwrap_or_else(|| format!("audio_{message_id}.bin"));
    let local_path = save_media_bytes(chat_id, message_id, &filename, &bytes)?;

    let transcript = match transcribe::transcribe_audio_if_configured(
        config,
        bytes,
        &filename,
        source.mime_type,
        cancel_token,
    )
    .await
    {
        Ok(transcript) => transcript,
        Err(err) if transcribe::is_operation_cancelled(&err) => return Err(err),
        Err(err) => {
            tracing::error!(error = %format!("{err:#}"), "Audio transcription failed");
            None
        }
    };

    Ok(Some(IncomingAudio {
        local_path,
        transcript,
        label: None,
    }))
}

/// Loads an image a surface already saved on this host, the same way as a
/// downloaded one: the saved file stays the original, the model gets a fitted
/// copy or a reason it cannot see it.
pub(crate) fn load_local_image(path: &Path, label: &str) -> Result<IncomingImage> {
    let bytes = fs::read(path).map_err(|err| {
        tracing::warn!(path = %path.display(), err = %err, "Failed to read local image attachment");
        AttachmentNotice(format!(
            "An image attachment saved at {} could not be read, so it is missing from this message. Tell the user.",
            path.display()
        ))
    })?;
    let delivery = prepare_inline_image(path, &bytes);
    Ok(IncomingImage {
        local_path: path.to_path_buf(),
        delivery,
        label: Some(label.to_string()),
    })
}

/// Transcribes a voice note a surface already saved on this host. A failed or
/// unconfigured transcription leaves `transcript` empty; the saved file stays.
pub(crate) async fn load_local_audio(
    config: &Config,
    path: &Path,
    mime: &str,
    label: &str,
    cancel_token: Option<&CancellationToken>,
) -> Result<Option<IncomingAudio>> {
    if cancel_token.is_some_and(CancellationToken::is_cancelled) {
        return Err(transcribe::OperationCancelled.into());
    }
    let bytes = fs::read(path).map_err(|err| {
        tracing::warn!(path = %path.display(), err = %err, "Failed to read local audio attachment");
        AttachmentNotice(format!(
            "A voice note saved at {} could not be read, so it is missing from this message. Tell the user.",
            path.display()
        ))
    })?;
    let filename = file_name_from_path(&path.to_string_lossy())
        .unwrap_or_else(|| "voice-note.webm".to_string());
    let transcript = match transcribe::transcribe_audio_if_configured(
        config,
        bytes,
        &filename,
        Some(mime),
        cancel_token,
    )
    .await
    {
        Ok(transcript) => transcript,
        Err(err) if transcribe::is_operation_cancelled(&err) => return Err(err),
        Err(err) => {
            tracing::error!(error = %format!("{err:#}"), "Audio transcription failed");
            None
        }
    };
    Ok(Some(IncomingAudio {
        local_path: path.to_path_buf(),
        transcript,
        label: Some(label.to_string()),
    }))
}

/// Downloads a Telegram file, trying again on a network or server error.
async fn download_file_bytes(
    client: &TelegramClient,
    file_id: &str,
    cancel_token: Option<&CancellationToken>,
) -> Result<(String, Vec<u8>)> {
    let mut attempt = 1;
    loop {
        match download_file_once(client, file_id).await {
            Ok(file) => return Ok(file),
            Err(err) if attempt < DOWNLOAD_ATTEMPTS && is_transient_download_error(&err) => {
                tracing::warn!(attempt, err = %format!("{err:#}"), "Telegram download failed; trying again");
                let delay = DOWNLOAD_RETRY_DELAY * attempt;
                match cancel_token {
                    Some(token) => tokio::select! {
                        () = token.cancelled() => return Err(transcribe::OperationCancelled.into()),
                        () = tokio::time::sleep(delay) => {}
                    },
                    None => tokio::time::sleep(delay).await,
                }
                attempt += 1;
            }
            Err(err) => return Err(err),
        }
    }
}

async fn download_file_once(client: &TelegramClient, file_id: &str) -> Result<(String, Vec<u8>)> {
    let file = client.get_file(file_id).await?;
    let file_path = file
        .file_path
        .ok_or_else(|| anyhow!("Telegram file missing file_path"))?;
    let bytes = client.download_file(&file_path).await?;
    Ok((file_path, bytes))
}

/// A failure another try can fix: no readable answer from Telegram, or a
/// server error. A refusal such as "file is too big" is not one.
fn is_transient_download_error(err: &anyhow::Error) -> bool {
    err.chain().any(|cause| {
        cause.is::<TelegramTransportError>()
            || cause
                .downcast_ref::<TelegramApiError>()
                .and_then(|api| api.code)
                .is_some_and(|code| code >= 500)
    })
}

fn file_name_from_path(path: &str) -> Option<String> {
    Path::new(path)
        .file_name()
        .and_then(|name| name.to_str())
        .map(std::string::ToString::to_string)
}

fn save_media_bytes(
    chat_id: i64,
    message_id: i64,
    filename: &str,
    bytes: &[u8],
) -> Result<PathBuf> {
    let dir = paths::zdx_home().join("telegram").join(chat_id.to_string());
    fs::create_dir_all(&dir).context("create media directory")?;
    let safe_name = Path::new(filename)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(filename);
    let path = dir.join(format!("{message_id}_{safe_name}"));
    fs::write(&path, bytes).context("write media file")?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use std::collections::{HashMap, HashSet};
    use std::fs;
    use std::io::Cursor;
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use axum::extract::Path as AxumPath;
    use axum::http::StatusCode;
    use axum::response::IntoResponse;
    use axum::routing::{get, post};
    use axum::{Json, Router};
    use base64::Engine as _;
    use serde_json::json;
    use zdx_engine::config::Config;

    use super::{
        AllowlistConfig, BASE64, DOWNLOAD_ATTEMPTS, DownloadUnavailable, INLINE_IMAGE_BYTES,
        INLINE_IMAGE_EDGE, MAX_VIDEO_BYTES, TELEGRAM_DOWNLOAD_LIMIT_BYTES, extract_text,
        parse_incoming_message,
    };
    use crate::agent::build_user_text;
    use crate::telegram::{Message, TelegramClient};
    use crate::types::{ImageDelivery, IncomingMessage, VideoStatus};

    fn message(value: serde_json::Value) -> Message {
        serde_json::from_value(value).expect("valid message")
    }

    fn base(id: i64) -> serde_json::Value {
        json!({
            "message_id": id,
            "chat": { "id": 42, "type": "supergroup", "is_forum": true },
            "from": { "id": 7, "is_bot": false },
        })
    }

    fn with(id: i64, fields: &serde_json::Value) -> Message {
        let mut value = base(id);
        let object = value.as_object_mut().expect("object");
        for (key, field) in fields.as_object().expect("object") {
            object.insert(key.clone(), field.clone());
        }
        message(value)
    }

    /// Shape Telegram delivers when the sender pastes a formatted list: no
    /// `text` at all, everything under `rich_message`.
    #[test]
    fn reads_the_text_of_a_rich_message() {
        let message = with(
            1,
            &json!({
                "rich_message": { "blocks": [
                    { "type": "paragraph", "text": "August:" },
                    { "type": "list", "items": [
                        { "label": "•", "blocks": [{ "type": "paragraph", "text": "rent: 460.00" }] },
                        { "label": "•", "blocks": [{ "type": "paragraph", "text": "lessons: 296.00" }] },
                    ]},
                ]},
            }),
        );

        assert_eq!(
            extract_text(&message).as_deref(),
            Some("August:\n\n• rent: 460.00\n• lessons: 296.00")
        );
    }

    #[test]
    fn plain_text_and_captions_are_unchanged() {
        let mut album = with(2, &json!({ "caption": "first photo" }));
        album
            .grouped_messages
            .push(with(3, &json!({ "text": "note" })));

        assert_eq!(extract_text(&album).as_deref(), Some("first photo\n\nnote"));
    }

    #[test]
    fn a_message_without_any_text_stays_empty() {
        assert!(extract_text(&with(4, &json!({ "rich_message": { "blocks": [] } }))).is_none());
    }

    /// In a forum topic every message points at the topic's creation message;
    /// only an explicit reply (or quote) brings the replied-to text along, and
    /// it comes before the user's words in the prompt.
    #[tokio::test]
    async fn explicit_replies_carry_the_replied_to_text() {
        let alert = json!({
            "message_id": 70,
            "message_thread_id": 69,
            "chat": { "id": 42, "type": "supergroup", "is_forum": true },
            "from": { "id": 99, "is_bot": true },
            "text": "⏰ Pay the cleaner: due 30/10 at 15:00",
        });
        let topic_root = json!({
            "message_id": 69,
            "chat": { "id": 42, "type": "supergroup", "is_forum": true },
            "from": { "id": 99, "is_bot": true },
        });
        let no_server = || "http://127.0.0.1:9".to_string();

        let plain = with(
            71,
            &json!({ "message_thread_id": 69, "text": "how much?", "reply_to_message": topic_root }),
        );
        let incoming = ingest(no_server(), plain).await.expect("turn runs");
        assert!(incoming.replied_to.is_none());
        assert_eq!(build_user_text(&incoming), "how much?");

        let reply = with(
            72,
            &json!({ "message_thread_id": 69, "text": "how much?", "reply_to_message": alert }),
        );
        let incoming = ingest(no_server(), reply).await.expect("turn runs");
        assert_eq!(
            build_user_text(&incoming),
            "<reply_to>\nThe user's message below replies to message 70 in this chat, sent by the bot (or another run posting through it):\n⏰ Pay the cleaner: due 30/10 at 15:00\n</reply_to>\n\nhow much?"
        );

        let quoted = with(
            73,
            &json!({
                "message_thread_id": 69,
                "text": "why?",
                "reply_to_message": alert,
                "quote": { "text": "due 30/10", "position": 21 },
            }),
        );
        let prompt = build_user_text(&ingest(no_server(), quoted).await.expect("turn runs"));
        assert!(
            prompt.starts_with(
                "<reply_to>\nThe user's message below quotes this part of message 70 in this chat, sent by the bot (or another run posting through it):\ndue 30/10\n</reply_to>"
            ),
            "{prompt}"
        );
    }

    const TOKEN: &str = "test-token";
    const VIDEO_BYTES: &[u8] = b"synthetic-mp4-bytes";

    /// Fake Bot API: `getFile` maps a file id to `videos/<id>.mp4`, and the file
    /// endpoint serves fixed bytes. Returns the base URL and the `getFile` count.
    async fn fake_telegram() -> (String, Arc<AtomicUsize>, tokio::task::JoinHandle<()>) {
        fake_telegram_with(HashMap::new()).await
    }

    /// Fake Bot API. `files` maps a file id to its `file_path` and bytes; any
    /// other id is served as `videos/<id>.mp4` with fixed bytes, except
    /// `broken`, which `getFile` rejects; `flaky`, whose first file fetch
    /// answers 502; and `down`, whose file fetch always answers 502. Returns
    /// the base URL and the `getFile` count.
    async fn fake_telegram_with(
        files: HashMap<&'static str, (&'static str, Vec<u8>)>,
    ) -> (String, Arc<AtomicUsize>, tokio::task::JoinHandle<()>) {
        let files = Arc::new(files);
        let by_path: Arc<HashMap<String, Vec<u8>>> = Arc::new(
            files
                .values()
                .map(|(path, bytes)| ((*path).to_string(), bytes.clone()))
                .collect(),
        );
        let get_file_calls = Arc::new(AtomicUsize::new(0));
        let calls = Arc::clone(&get_file_calls);
        let flaky_fetches = Arc::new(AtomicUsize::new(0));
        let app = Router::new()
            .route(
                &format!("/bot{TOKEN}/getFile"),
                post(move |Json(body): Json<serde_json::Value>| {
                    let calls = Arc::clone(&calls);
                    let files = Arc::clone(&files);
                    async move {
                        calls.fetch_add(1, Ordering::SeqCst);
                        let file_id = body["file_id"].as_str().unwrap_or_default().to_string();
                        if file_id == "broken" {
                            return Json(json!({
                                "ok": false,
                                "error_code": 400,
                                "description": "Bad Request: file is too big",
                            }));
                        }
                        let file_path = files.get(file_id.as_str()).map_or_else(
                            || format!("videos/{file_id}.mp4"),
                            |(path, _)| (*path).to_string(),
                        );
                        Json(json!({
                            "ok": true,
                            "result": { "file_id": file_id, "file_path": file_path },
                        }))
                    }
                }),
            )
            .route(
                &format!("/file/bot{TOKEN}/{{*path}}"),
                get(move |AxumPath(path): AxumPath<String>| {
                    let by_path = Arc::clone(&by_path);
                    let flaky_fetches = Arc::clone(&flaky_fetches);
                    async move {
                        let first_flaky = path.contains("flaky")
                            && flaky_fetches.fetch_add(1, Ordering::SeqCst) == 0;
                        if first_flaky || path.contains("down") {
                            return StatusCode::BAD_GATEWAY.into_response();
                        }
                        by_path
                            .get(&path)
                            .cloned()
                            .unwrap_or_else(|| VIDEO_BYTES.to_vec())
                            .into_response()
                    }
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind fake Telegram");
        let addr = listener.local_addr().expect("fake Telegram address");
        let server = tokio::spawn(async move {
            axum::serve(listener, app)
                .await
                .expect("serve fake Telegram");
        });
        (format!("http://{addr}"), get_file_calls, server)
    }

    async fn try_ingest(
        base_url: String,
        message: Message,
    ) -> anyhow::Result<Option<IncomingMessage>> {
        let client = TelegramClient::with_base_url(TOKEN.to_string(), base_url);
        let user_ids = HashSet::from([7]);
        let chat_ids = HashSet::from([42]);
        parse_incoming_message(
            &client,
            AllowlistConfig {
                user_ids: &user_ids,
                chat_ids: &chat_ids,
            },
            &Config::default(),
            &message,
            None,
        )
        .await
    }

    async fn ingest(base_url: String, message: Message) -> Option<IncomingMessage> {
        try_ingest(base_url, message).await.expect("ingest message")
    }

    fn saved_video_path(incoming: &IncomingMessage) -> PathBuf {
        assert_eq!(incoming.videos.len(), 1, "exactly one video attachment");
        let VideoStatus::Saved(path) = &incoming.videos[0].status else {
            panic!("video was not saved");
        };
        path.clone()
    }

    /// Mini App feedback rides on a synthetic prompt with files already on
    /// disk: images take the normal inline path, and a file that cannot be read
    /// becomes a notice instead of vanishing.
    #[tokio::test]
    async fn local_attachments_load_like_downloaded_ones() {
        let home = zdx_engine::test_support::temp_zdx_home();
        let (base_url, get_file_calls, server) = fake_telegram().await;
        let image_path = home.path().join("annotation-1-drawing.png");
        let mut png = Vec::new();
        image::DynamicImage::new_rgba8(4, 4)
            .write_to(&mut Cursor::new(&mut png), image::ImageFormat::Png)
            .expect("encode png");
        fs::write(&image_path, &png).expect("write png");
        let missing_audio = home.path().join("annotation-1-voice-1.webm");

        let mut message = with(13, &json!({ "text": "[artifact feedback] keep this" }));
        message.local_attachments = vec![
            crate::types::LocalAttachment::Image {
                path: image_path.clone(),
                label: "Drawing sketch for note 1".to_string(),
            },
            crate::types::LocalAttachment::Audio {
                path: missing_audio.clone(),
                mime: "audio/webm".to_string(),
                label: "Voice note 1 for note 1".to_string(),
            },
        ];

        let incoming = ingest(base_url, message).await.expect("turn runs");
        assert_eq!(
            get_file_calls.load(Ordering::SeqCst),
            0,
            "nothing downloaded"
        );
        assert_eq!(incoming.images.len(), 1);
        assert_eq!(incoming.images[0].local_path, image_path);
        assert!(incoming.images[0].inline().is_some());
        assert!(incoming.audios.is_empty());
        let prompt = build_user_text(&incoming);
        assert!(
            prompt.starts_with("[artifact feedback] keep this"),
            "{prompt}"
        );
        assert!(
            prompt.contains(&format!(
                "Drawing sketch for note 1: image attachment saved at {}",
                image_path.display()
            )),
            "{prompt}"
        );
        assert!(
            prompt.contains(&format!(
                "A voice note saved at {} could not be read",
                missing_audio.display()
            )),
            "{prompt}"
        );
        server.abort();
    }

    /// The reported regression: a captioned video used to reach the agent as
    /// its caption alone.
    #[tokio::test]
    async fn captioned_video_reaches_the_agent_as_a_saved_file() {
        let _home = zdx_engine::test_support::temp_zdx_home();
        let (base_url, _, server) = fake_telegram().await;
        let message = with(
            10,
            &json!({
                "caption": "check the keyboard animation",
                "video": { "file_id": "vid1", "file_size": 19, "mime_type": "video/mp4" },
            }),
        );

        let incoming = ingest(base_url, message).await.expect("turn runs");
        let path = saved_video_path(&incoming);
        assert_eq!(fs::read(&path).expect("saved video"), VIDEO_BYTES);
        assert!(path.to_string_lossy().ends_with("10_vid1.mp4"));

        let prompt = build_user_text(&incoming);
        assert!(prompt.starts_with("check the keyboard animation"));
        assert!(prompt.contains(&format!(
            "Video attachment saved at {} — use the `ask_media` tool on it",
            path.display()
        )));
        server.abort();
    }

    /// A video with no caption used to be dropped as an empty message.
    #[tokio::test]
    async fn video_note_without_caption_runs_a_turn() {
        let _home = zdx_engine::test_support::temp_zdx_home();
        let (base_url, _, server) = fake_telegram().await;
        let message = with(11, &json!({ "video_note": { "file_id": "note1" } }));

        let incoming = ingest(base_url, message).await.expect("turn runs");
        assert!(incoming.text.is_none());
        let path = saved_video_path(&incoming);
        assert!(build_user_text(&incoming).starts_with(&format!(
            "Video note attachment saved at {}",
            path.display()
        )));
        server.abort();
    }

    /// Telegram also sets `document` on an animation; it must load once.
    #[tokio::test]
    async fn animation_is_loaded_once_despite_its_document_twin() {
        let _home = zdx_engine::test_support::temp_zdx_home();
        let (base_url, get_file_calls, server) = fake_telegram().await;
        let message = with(
            12,
            &json!({
                "animation": { "file_id": "gif1", "file_name": "loop.mp4", "mime_type": "video/mp4" },
                "document": { "file_id": "gif1", "file_name": "loop.mp4", "mime_type": "video/mp4" },
            }),
        );

        let incoming = ingest(base_url, message).await.expect("turn runs");
        assert!(incoming.documents.is_empty());
        assert_eq!(incoming.videos[0].kind, "Animation");
        assert!(saved_video_path(&incoming).ends_with("12_loop.mp4"));
        assert_eq!(get_file_calls.load(Ordering::SeqCst), 1);
        server.abort();
    }

    #[tokio::test]
    async fn video_sent_as_a_file_uses_the_video_path() {
        let _home = zdx_engine::test_support::temp_zdx_home();
        let (base_url, _, server) = fake_telegram().await;
        let message = with(
            13,
            &json!({
                "document": { "file_id": "doc1", "file_name": "screen.mov", "mime_type": "video/quicktime" },
            }),
        );

        let incoming = ingest(base_url, message).await.expect("turn runs");
        assert!(incoming.documents.is_empty());
        assert_eq!(incoming.videos[0].kind, "Video file");
        assert!(saved_video_path(&incoming).ends_with("13_screen.mov"));
        server.abort();
    }

    /// A video over the `ask_media` limit is not downloaded, but the agent is
    /// told why instead of the video vanishing.
    #[tokio::test]
    async fn oversized_video_is_reported_without_download() {
        let _home = zdx_engine::test_support::temp_zdx_home();
        let (base_url, get_file_calls, server) = fake_telegram().await;
        let message = with(
            14,
            &json!({ "video": { "file_id": "big1", "file_size": MAX_VIDEO_BYTES + 1 } }),
        );

        let incoming = ingest(base_url, message).await.expect("turn runs");
        assert!(matches!(
            incoming.videos[0].status,
            VideoStatus::TooLarge { .. }
        ));
        assert_eq!(get_file_calls.load(Ordering::SeqCst), 0);
        assert_eq!(
            build_user_text(&incoming),
            "Video attachment was not downloaded: it is 15.0 MiB and the limit is 15 MiB. \
             Ask the user for a shorter or compressed clip."
        );
        server.abort();
    }

    fn png(width: u32, height: u32) -> Vec<u8> {
        let img = image::RgbImage::from_fn(width, height, |x, y| {
            image::Rgb([(x % 256) as u8, (y % 256) as u8, 128])
        });
        let mut buf = Vec::new();
        image::DynamicImage::ImageRgb8(img)
            .write_to(&mut Cursor::new(&mut buf), image::ImageFormat::Png)
            .expect("encode test PNG");
        buf
    }

    fn inline_bytes(delivery: &ImageDelivery) -> (Vec<u8>, bool) {
        let ImageDelivery::Inline {
            data, downscaled, ..
        } = delivery
        else {
            panic!("image was not sent inline");
        };
        (BASE64.decode(data).expect("valid base64"), *downscaled)
    }

    fn photo_sizes(small: &str, big: &str) -> serde_json::Value {
        json!([
            { "file_id": small, "width": 90, "height": 60, "file_size": 1000 },
            { "file_id": big, "width": 900, "height": 600, "file_size": 9000 },
        ])
    }

    /// Two photos sent as one album reach a single turn as two inline images,
    /// each from its largest size, with the caption first.
    #[tokio::test]
    async fn two_photo_album_reaches_one_turn_as_two_images() {
        let _home = zdx_engine::test_support::temp_zdx_home();
        let (base_url, get_file_calls, server) = fake_telegram_with(HashMap::from([
            ("a_big", ("photos/a_big.png", png(90, 60))),
            ("b_big", ("photos/b_big.png", png(90, 60))),
        ]))
        .await;
        let mut album = with(
            20,
            &json!({ "media_group_id": "g1", "caption": "compare these", "photo": photo_sizes("a_small", "a_big") }),
        );
        album.grouped_messages.push(with(
            21,
            &json!({ "media_group_id": "g1", "photo": photo_sizes("b_small", "b_big") }),
        ));

        let incoming = ingest(base_url, album).await.expect("turn runs");

        assert_eq!(get_file_calls.load(Ordering::SeqCst), 2);
        assert_eq!(incoming.images.len(), 2);
        assert!(incoming.images[0].local_path.ends_with("20_a_big.png"));
        assert!(incoming.images[1].local_path.ends_with("21_b_big.png"));
        for image in &incoming.images {
            assert!(!inline_bytes(&image.delivery).1, "small image sent as-is");
        }
        let prompt = build_user_text(&incoming);
        assert!(prompt.starts_with("compare these\n\nImage attachment saved at "));
        assert_eq!(prompt.matches("Image attachment saved at").count(), 2);
        server.abort();
    }

    /// A large image is saved whole, and the model gets a smaller copy within
    /// the inline limits. The byte-budget path is covered by
    /// `zdx_tools::image_downscale`; noisy fixtures big enough to hit it here
    /// take minutes in a debug build.
    #[tokio::test]
    async fn large_image_is_saved_whole_and_sent_downscaled() {
        let _home = zdx_engine::test_support::temp_zdx_home();
        // Small in pixels on purpose: image work is slow in a debug build.
        let original = png(3200, 100);
        let (base_url, _, server) = fake_telegram_with(HashMap::from([(
            "shot",
            ("documents/shot.png", original.clone()),
        )]))
        .await;
        let message = with(
            22,
            &json!({ "document": { "file_id": "shot", "file_name": "screen.png", "mime_type": "image/png", "file_size": original.len() } }),
        );

        let incoming = ingest(base_url, message).await.expect("turn runs");
        let image = &incoming.images[0];
        assert_eq!(fs::read(&image.local_path).expect("saved"), original);
        let (sent, downscaled) = inline_bytes(&image.delivery);
        assert!(downscaled);
        assert!(sent.len() <= INLINE_IMAGE_BYTES);
        let (width, height) = image::load_from_memory(&sent)
            .map(|img| (img.width(), img.height()))
            .expect("decodable inline copy");
        assert_eq!(
            (width, height),
            (INLINE_IMAGE_EDGE, 49),
            "aspect ratio kept"
        );
        assert!(build_user_text(&incoming).contains("You see a downscaled copy"));
        server.abort();
    }

    /// Image files the model cannot take inline are saved and described, not
    /// dropped: HEIC points to `ask_media`; an unknown format says neither path
    /// can read it.
    #[tokio::test]
    async fn non_inline_images_are_saved_with_guidance() {
        let _home = zdx_engine::test_support::temp_zdx_home();
        let mut heic = vec![0, 0, 0, 0x18];
        heic.extend_from_slice(b"ftypheic\0\0\0\0mif1heic");
        heic.resize(64, 0);
        let svg = b"<svg xmlns='http://www.w3.org/2000/svg'/>".to_vec();
        let (base_url, _, server) = fake_telegram_with(HashMap::from([
            ("heic", ("documents/photo.heic", heic)),
            ("svg", ("documents/logo.svg", svg)),
        ]))
        .await;
        let mut message = with(
            23,
            &json!({ "document": { "file_id": "heic", "file_name": "IMG_1.HEIC", "mime_type": "image/heic" } }),
        );
        message.grouped_messages.push(with(
            24,
            &json!({ "document": { "file_id": "svg", "file_name": "logo.svg", "mime_type": "image/svg+xml" } }),
        ));

        let incoming = ingest(base_url, message).await.expect("turn runs");
        assert_eq!(incoming.images.len(), 2);
        assert!(incoming.images.iter().all(|image| image.inline().is_none()));
        assert!(incoming.images[0].local_path.ends_with("23_IMG_1.HEIC"));
        assert!(incoming.images[1].local_path.exists());

        let prompt = build_user_text(&incoming);
        assert!(
            prompt.contains("not attached for you to see: the model cannot take image/heif images directly. Use the `ask_media` tool"),
            "{prompt}"
        );
        assert!(
            prompt.contains(
                "its image format was not recognized. `ask_media` cannot read this file either"
            ),
            "{prompt}"
        );
        server.abort();
    }

    /// A photo that cannot be downloaded still runs a turn that tells the
    /// agent, without leaking the download error.
    #[tokio::test]
    async fn failed_image_downloads_become_notices() {
        let _home = zdx_engine::test_support::temp_zdx_home();
        let (base_url, get_file_calls, server) = fake_telegram().await;
        let mut message = with(
            25,
            &json!({ "photo": [{ "file_id": "broken", "width": 900, "height": 600 }] }),
        );
        message.grouped_messages.push(with(
            26,
            &json!({ "document": { "file_id": "huge", "file_name": "big.png", "mime_type": "image/png", "file_size": TELEGRAM_DOWNLOAD_LIMIT_BYTES + 1 } }),
        ));

        let incoming = ingest(base_url, message).await.expect("turn runs");
        assert!(incoming.images.is_empty());
        assert_eq!(
            get_file_calls.load(Ordering::SeqCst),
            1,
            "oversized file not requested"
        );
        assert_eq!(
            incoming.notices,
            vec![
                "One attachment (photo) could not be loaded from Telegram, so it is missing from this message. Tell the user and ask them to resend it.".to_string(),
                "An image attachment was not downloaded: it is 20.0 MiB and Telegram bots can only download files up to 20.0 MiB. Ask the user for a smaller file.".to_string(),
            ]
        );
        assert!(!build_user_text(&incoming).contains(TOKEN));
        server.abort();
    }

    /// One dropped download is tried again, so the voice message still
    /// reaches the turn.
    #[tokio::test]
    async fn a_failed_download_is_tried_again() {
        let _home = zdx_engine::test_support::temp_zdx_home();
        let (base_url, get_file_calls, server) = fake_telegram().await;
        let message = with(30, &json!({ "voice": { "file_id": "flaky" } }));

        let incoming = ingest(base_url, message).await.expect("turn runs");
        assert_eq!(get_file_calls.load(Ordering::SeqCst), 2);
        assert_eq!(incoming.audios.len(), 1);
        assert!(incoming.notices.is_empty(), "{:?}", incoming.notices);
        server.abort();
    }

    /// A download that keeps failing does not reach the agent as a notice:
    /// the message is held so the user can load it again.
    #[tokio::test]
    async fn a_download_that_keeps_failing_holds_the_message() {
        let _home = zdx_engine::test_support::temp_zdx_home();
        let (base_url, get_file_calls, server) = fake_telegram().await;
        let message = with(
            31,
            &json!({ "caption": "listen to this", "voice": { "file_id": "down" } }),
        );

        let Err(err) = try_ingest(base_url, message).await else {
            panic!("message ran a turn");
        };
        let unavailable = err
            .downcast_ref::<DownloadUnavailable>()
            .expect("download unavailable");
        assert_eq!(unavailable.label, "voice message");
        assert_eq!(
            get_file_calls.load(Ordering::SeqCst),
            DOWNLOAD_ATTEMPTS as usize
        );
        server.abort();
    }
}
