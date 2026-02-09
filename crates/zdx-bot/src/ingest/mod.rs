use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Result, anyhow};
use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use zdx_core::config::{Config, paths};

use crate::telegram::{Audio, Document, Message, PhotoSize, TelegramClient, Voice};
use crate::transcribe;
use crate::types::{IncomingAudio, IncomingImage, IncomingMessage};

const MAX_IMAGE_BYTES: u64 = 3_932_160; // 3.75MB
const MAX_AUDIO_BYTES: u64 = 25 * 1024 * 1024; // 25MB
const SUPPORTED_IMAGE_MIMES: &[&str] = &["image/jpeg", "image/png", "image/gif", "image/webp"];

pub(crate) struct AllowlistConfig<'a> {
    pub user_ids: &'a HashSet<i64>,
    pub chat_ids: &'a HashSet<i64>,
}

///
/// # Errors
/// Returns an error if the operation fails.
pub(crate) async fn parse_incoming_message(
    client: &TelegramClient,
    allowlist: AllowlistConfig<'_>,
    config: &Config,
    message: Message,
) -> Result<Option<IncomingMessage>> {
    let chat_id = message.chat.id;
    let message_id = message.message_id;

    // For groups/supergroups, check if chat is in the allowlist
    if message.chat.is_group() {
        if !allowlist.chat_ids.contains(&chat_id) {
            eprintln!("Ignoring non-allowlisted group chat {chat_id}");
            return Ok(None);
        }
    } else if !message.chat.is_private() {
        // Not a group, supergroup, or private chat - ignore
        eprintln!("Ignoring unsupported chat type for chat {chat_id}");
        return Ok(None);
    }

    let Some(user) = message.from.as_ref() else {
        eprintln!("Ignoring message without sender in chat {chat_id}");
        return Ok(None);
    };

    // Skip messages from bots (including our own messages)
    if user.is_bot {
        return Ok(None);
    }

    if !allowlist.user_ids.contains(&user.id) {
        eprintln!("Denied user {} for chat {}", user.id, chat_id);
        let _ = client
            .send_message(
                chat_id,
                "Access denied.",
                Some(message_id),
                message.effective_thread_id(),
            )
            .await;
        return Ok(None);
    }

    // Use best-effort thread id for forum topics.
    let message_thread_id = message.effective_thread_id();

    let mut text = extract_text(&message);
    let mut images = Vec::new();
    let mut audios = Vec::new();
    let mut had_attachments = false;

    if let Some(photos) = message.photo.as_deref() {
        had_attachments = true;
        if let Some(photo) = select_best_photo(photos) {
            match load_photo_attachment(client, chat_id, message_id, photo).await {
                Ok(Some(image)) => images.push(image),
                Ok(None) => {}
                Err(err) => eprintln!("Failed to load photo attachment: {err}"),
            }
        }
    }

    if let Some(document) = message.document.as_ref()
        && let Some(mime) = document.mime_type.as_deref()
    {
        if mime.starts_with("image/") {
            had_attachments = true;
            match load_document_image(client, chat_id, message_id, document).await {
                Ok(Some(image)) => images.push(image),
                Ok(None) => {}
                Err(err) => eprintln!("Failed to load document image: {err}"),
            }
        } else if mime.starts_with("audio/") {
            had_attachments = true;
            match load_audio_attachment(client, config, chat_id, message_id, document).await {
                Ok(Some(audio)) => audios.push(audio),
                Ok(None) => {}
                Err(err) => eprintln!("Failed to load document audio: {err}"),
            }
        }
    }

    if let Some(voice) = message.voice.as_ref() {
        had_attachments = true;
        match load_voice_attachment(client, config, chat_id, message_id, voice).await {
            Ok(Some(audio)) => audios.push(audio),
            Ok(None) => {}
            Err(err) => eprintln!("Failed to load voice attachment: {err}"),
        }
    }

    if let Some(audio) = message.audio.as_ref() {
        had_attachments = true;
        match load_audio_message(client, config, chat_id, message_id, audio).await {
            Ok(Some(audio)) => audios.push(audio),
            Ok(None) => {}
            Err(err) => eprintln!("Failed to load audio message: {err}"),
        }
    }

    if text.is_none() && images.is_empty() && audios.is_empty() {
        if had_attachments {
            eprintln!("Unsupported attachment in chat {chat_id}");
            let _ = client
                .send_message(
                    chat_id,
                    "Sorry, I couldn't read that attachment.",
                    Some(message_id),
                    message_thread_id,
                )
                .await;
        } else {
            eprintln!("Ignoring empty message in chat {chat_id}");
        }
        return Ok(None);
    }

    if let Some(text_value) = text.as_deref()
        && text_value.trim().is_empty()
    {
        text = None;
    }

    Ok(Some(IncomingMessage {
        chat_id,
        message_id,
        user_id: user.id,
        text,
        images,
        audios,
        message_thread_id,
        is_forum: message.chat.is_forum_enabled(),
    }))
}

fn extract_text(message: &Message) -> Option<String> {
    let mut parts = Vec::new();
    if let Some(text) = message.text.as_deref() {
        let trimmed = text.trim();
        if !trimmed.is_empty() {
            parts.push(trimmed.to_string());
        }
    }
    if let Some(caption) = message.caption.as_deref() {
        let trimmed = caption.trim();
        if !trimmed.is_empty() {
            parts.push(trimmed.to_string());
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

async fn load_photo_attachment(
    client: &TelegramClient,
    chat_id: i64,
    message_id: i64,
    photo: &PhotoSize,
) -> Result<Option<IncomingImage>> {
    if photo.file_size.unwrap_or(0) > MAX_IMAGE_BYTES {
        eprintln!("Skipping photo > max image size in chat {chat_id}");
        return Ok(None);
    }

    let (file_path, bytes) = download_file_bytes(client, &photo.file_id).await?;
    if bytes.len() as u64 > MAX_IMAGE_BYTES {
        eprintln!("Downloaded photo exceeds max image size in chat {chat_id}");
        return Ok(None);
    }

    let Some(mime_type) = detect_image_mime(&bytes) else {
        eprintln!("Unsupported image type in chat {chat_id}");
        return Ok(None);
    };

    let filename =
        file_name_from_path(&file_path).unwrap_or_else(|| format!("photo_{message_id}.bin"));
    let local_path = save_media_bytes(chat_id, message_id, &filename, &bytes)?;
    let data = BASE64.encode(&bytes);

    Ok(Some(IncomingImage {
        local_path,
        mime_type,
        data,
    }))
}

async fn load_document_image(
    client: &TelegramClient,
    chat_id: i64,
    message_id: i64,
    document: &Document,
) -> Result<Option<IncomingImage>> {
    if document.file_size.unwrap_or(0) > MAX_IMAGE_BYTES {
        eprintln!("Skipping document image > max size in chat {chat_id}");
        return Ok(None);
    }

    let (file_path, bytes) = download_file_bytes(client, &document.file_id).await?;
    if bytes.len() as u64 > MAX_IMAGE_BYTES {
        eprintln!("Downloaded document image exceeds max size in chat {chat_id}");
        return Ok(None);
    }

    let Some(mime_type) = select_image_mime(document.mime_type.as_deref(), &bytes) else {
        eprintln!("Unsupported document image type in chat {chat_id}");
        return Ok(None);
    };

    let filename = document
        .file_name
        .as_deref()
        .and_then(file_name_from_path)
        .or_else(|| file_name_from_path(&file_path))
        .unwrap_or_else(|| format!("image_{message_id}.bin"));
    let local_path = save_media_bytes(chat_id, message_id, &filename, &bytes)?;
    let data = BASE64.encode(&bytes);

    Ok(Some(IncomingImage {
        local_path,
        mime_type,
        data,
    }))
}

async fn load_voice_attachment(
    client: &TelegramClient,
    config: &Config,
    chat_id: i64,
    message_id: i64,
    voice: &Voice,
) -> Result<Option<IncomingAudio>> {
    load_audio_by_id(
        client,
        config,
        chat_id,
        message_id,
        &voice.file_id,
        voice.file_size,
        voice.mime_type.as_deref(),
        None,
    )
    .await
}

async fn load_audio_message(
    client: &TelegramClient,
    config: &Config,
    chat_id: i64,
    message_id: i64,
    audio: &Audio,
) -> Result<Option<IncomingAudio>> {
    load_audio_by_id(
        client,
        config,
        chat_id,
        message_id,
        &audio.file_id,
        audio.file_size,
        audio.mime_type.as_deref(),
        audio.file_name.as_deref(),
    )
    .await
}

async fn load_audio_attachment(
    client: &TelegramClient,
    config: &Config,
    chat_id: i64,
    message_id: i64,
    document: &Document,
) -> Result<Option<IncomingAudio>> {
    load_audio_by_id(
        client,
        config,
        chat_id,
        message_id,
        &document.file_id,
        document.file_size,
        document.mime_type.as_deref(),
        document.file_name.as_deref(),
    )
    .await
}
async fn load_audio_by_id(
    client: &TelegramClient,
    config: &Config,
    chat_id: i64,
    message_id: i64,
    file_id: &str,
    file_size: Option<u64>,
    mime_type: Option<&str>,
    file_name_hint: Option<&str>,
) -> Result<Option<IncomingAudio>> {
    if file_size.unwrap_or(0) > MAX_AUDIO_BYTES {
        eprintln!("Skipping audio > max size in chat {chat_id}");
        return Ok(None);
    }

    let (file_path, bytes) = download_file_bytes(client, file_id).await?;
    if bytes.len() as u64 > MAX_AUDIO_BYTES {
        eprintln!("Downloaded audio exceeds max size in chat {chat_id}");
        return Ok(None);
    }

    let filename = file_name_hint
        .and_then(file_name_from_path)
        .or_else(|| file_name_from_path(&file_path))
        .unwrap_or_else(|| format!("audio_{message_id}.bin"));
    let local_path = save_media_bytes(chat_id, message_id, &filename, &bytes)?;

    let transcript =
        match transcribe::transcribe_audio_if_configured(config, bytes, &filename, mime_type).await
        {
            Ok(transcript) => transcript,
            Err(err) => {
                eprintln!("Audio transcription failed: {err}");
                None
            }
        };

    Ok(Some(IncomingAudio {
        local_path,
        transcript,
    }))
}

fn detect_image_mime(bytes: &[u8]) -> Option<String> {
    let kind = infer::get(bytes)?;
    let mime = kind.mime_type();
    if SUPPORTED_IMAGE_MIMES.contains(&mime) {
        Some(mime.to_string())
    } else {
        None
    }
}

fn select_image_mime(declared: Option<&str>, bytes: &[u8]) -> Option<String> {
    if let Some(mime) = declared
        && SUPPORTED_IMAGE_MIMES.contains(&mime)
    {
        return Some(mime.to_string());
    }
    detect_image_mime(bytes)
}

async fn download_file_bytes(client: &TelegramClient, file_id: &str) -> Result<(String, Vec<u8>)> {
    let file = client.get_file(file_id).await?;
    let file_path = file
        .file_path
        .ok_or_else(|| anyhow!("Telegram file missing file_path"))?;
    let bytes = client.download_file(&file_path).await?;
    Ok((file_path, bytes))
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
    fs::create_dir_all(&dir).map_err(|_| anyhow!("Failed to create media directory"))?;
    let safe_name = Path::new(filename)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(filename);
    let path = dir.join(format!("{message_id}_{safe_name}"));
    fs::write(&path, bytes).map_err(|_| anyhow!("Failed to write media file"))?;
    Ok(path)
}
