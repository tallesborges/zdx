use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow};
use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use zdx_core::config::{Config, paths};

use crate::telegram::{Audio, Document, Message, PhotoSize, TelegramClient, Voice};
use crate::transcribe;
use crate::types::{IncomingAudio, IncomingDocument, IncomingImage, IncomingMessage};

const MAX_IMAGE_BYTES: u64 = 3_932_160; // 3.75MB
const MAX_AUDIO_BYTES: u64 = 25 * 1024 * 1024; // 25MB
const MAX_DOCUMENT_BYTES: u64 = 25 * 1024 * 1024; // 25MB
const SUPPORTED_IMAGE_MIMES: &[&str] = &["image/jpeg", "image/png", "image/gif", "image/webp"];

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
    had_attachments: bool,
}

struct AudioSource<'a> {
    file_id: &'a str,
    file_size: Option<u64>,
    mime_type: Option<&'a str>,
    file_name_hint: Option<&'a str>,
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
    let target = MessageTarget {
        chat: message.chat.id,
        message: message.id,
        thread: message.effective_thread_id(),
    };

    let Some(user_id) = validate_access(client, allowlist, &message, &target).await? else {
        return Ok(None);
    };

    let mut text = extract_text(&message);
    let attachments = collect_attachments(client, config, &message, &target).await;
    let AttachmentCollection {
        images,
        audios,
        documents,
        had_attachments,
    } = attachments;

    if text.is_none() && images.is_empty() && audios.is_empty() && documents.is_empty() {
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
        message_thread_id: target.thread,
        is_forum: message.chat.is_forum_enabled(),
    }))
}

async fn validate_access(
    client: &TelegramClient,
    allowlist: AllowlistConfig<'_>,
    message: &Message,
    target: &MessageTarget,
) -> Result<Option<i64>> {
    if message.chat.is_group() {
        if !allowlist.chat_ids.contains(&target.chat) {
            eprintln!("Ignoring non-allowlisted group chat {}", target.chat);
            return Ok(None);
        }
    } else if !message.chat.is_private() {
        eprintln!("Ignoring unsupported chat type for chat {}", target.chat);
        return Ok(None);
    }

    let Some(user) = message.from.as_ref() else {
        eprintln!("Ignoring message without sender in chat {}", target.chat);
        return Ok(None);
    };

    if user.is_bot {
        return Ok(None);
    }

    if allowlist.user_ids.contains(&user.id) {
        return Ok(Some(user.id));
    }

    eprintln!("Denied user {} for chat {}", user.id, target.chat);
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

async fn collect_attachments(
    client: &TelegramClient,
    config: &Config,
    message: &Message,
    target: &MessageTarget,
) -> AttachmentCollection {
    let mut images = Vec::new();
    let mut audios = Vec::new();
    let mut documents = Vec::new();
    let mut had_attachments = false;

    if let Some(photos) = message.photo.as_deref() {
        had_attachments = true;
        if let Some(photo) = select_best_photo(photos) {
            push_attachment(
                &mut images,
                load_photo_attachment(client, target.chat, target.message, photo),
                "photo attachment",
            )
            .await;
        }
    }

    if let Some(document) = message.document.as_ref()
        && let Some(mime) = document.mime_type.as_deref()
    {
        had_attachments = true;
        if mime.starts_with("image/") {
            push_attachment(
                &mut images,
                load_document_image(client, target.chat, target.message, document),
                "document image",
            )
            .await;
        } else if mime.starts_with("audio/") {
            push_attachment(
                &mut audios,
                load_audio_attachment(client, config, target.chat, target.message, document),
                "document audio",
            )
            .await;
        } else {
            push_attachment(
                &mut documents,
                load_generic_document(client, target.chat, target.message, document),
                "generic document",
            )
            .await;
        }
    }

    if let Some(voice) = message.voice.as_ref() {
        had_attachments = true;
        push_attachment(
            &mut audios,
            load_voice_attachment(client, config, target.chat, target.message, voice),
            "voice attachment",
        )
        .await;
    }

    if let Some(audio) = message.audio.as_ref() {
        had_attachments = true;
        push_attachment(
            &mut audios,
            load_audio_message(client, config, target.chat, target.message, audio),
            "audio message",
        )
        .await;
    }

    AttachmentCollection {
        images,
        audios,
        documents,
        had_attachments,
    }
}

async fn push_attachment<T>(
    output: &mut Vec<T>,
    load_future: impl std::future::Future<Output = Result<Option<T>>>,
    label: &str,
) {
    match load_future.await {
        Ok(Some(item)) => output.push(item),
        Ok(None) => {}
        Err(err) => eprintln!("Failed to load {label}: {err}"),
    }
}

async fn handle_empty_message(
    client: &TelegramClient,
    target: &MessageTarget,
    had_attachments: bool,
) -> Result<Option<IncomingMessage>> {
    if had_attachments {
        eprintln!("Unsupported attachment in chat {}", target.chat);
        let _ = client
            .send_message(
                target.chat,
                "Sorry, I couldn't read that attachment.",
                Some(target.message),
                target.thread,
            )
            .await;
    } else {
        eprintln!("Ignoring empty message in chat {}", target.chat);
    }
    Ok(None)
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

async fn load_generic_document(
    client: &TelegramClient,
    chat_id: i64,
    message_id: i64,
    document: &Document,
) -> Result<Option<IncomingDocument>> {
    if document.file_size.unwrap_or(0) > MAX_DOCUMENT_BYTES {
        eprintln!("Skipping document > max size in chat {chat_id}");
        return Ok(None);
    }

    let (file_path, bytes) = download_file_bytes(client, &document.file_id).await?;
    if bytes.len() as u64 > MAX_DOCUMENT_BYTES {
        eprintln!("Downloaded document exceeds max size in chat {chat_id}");
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
        AudioSource {
            file_id: &voice.file_id,
            file_size: voice.file_size,
            mime_type: voice.mime_type.as_deref(),
            file_name_hint: None,
        },
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
        AudioSource {
            file_id: &audio.file_id,
            file_size: audio.file_size,
            mime_type: audio.mime_type.as_deref(),
            file_name_hint: audio.file_name.as_deref(),
        },
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
        AudioSource {
            file_id: &document.file_id,
            file_size: document.file_size,
            mime_type: document.mime_type.as_deref(),
            file_name_hint: document.file_name.as_deref(),
        },
    )
    .await
}

async fn load_audio_by_id(
    client: &TelegramClient,
    config: &Config,
    chat_id: i64,
    message_id: i64,
    source: AudioSource<'_>,
) -> Result<Option<IncomingAudio>> {
    if source.file_size.unwrap_or(0) > MAX_AUDIO_BYTES {
        eprintln!("Skipping audio > max size in chat {chat_id}");
        return Ok(None);
    }

    let (file_path, bytes) = download_file_bytes(client, source.file_id).await?;
    if bytes.len() as u64 > MAX_AUDIO_BYTES {
        eprintln!("Downloaded audio exceeds max size in chat {chat_id}");
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
    )
    .await
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
    fs::create_dir_all(&dir).context("create media directory")?;
    let safe_name = Path::new(filename)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(filename);
    let path = dir.join(format!("{message_id}_{safe_name}"));
    fs::write(&path, bytes).context("write media file")?;
    Ok(path)
}
