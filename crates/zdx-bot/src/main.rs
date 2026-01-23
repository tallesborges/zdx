use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Result, anyhow};
use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use zdx_core::config::{Config, ThinkingLevel, paths};
use zdx_core::core::agent::{self, AgentOptions, ToolConfig, ToolSelection};
use zdx_core::core::context::build_effective_system_prompt_with_paths;
use zdx_core::core::thread_log::{self, ThreadEvent, ThreadLog};
use zdx_core::providers::{ChatContentBlock, ChatMessage, MessageContent};
use zdx_core::tools::{ToolRegistry, ToolSet};

use crate::telegram::{
    Audio, Document, Message, PhotoSize, TelegramClient, TelegramSettings, Voice,
};
use crate::types::{IncomingAudio, IncomingImage, IncomingMessage};

mod telegram;
mod transcribe;
mod types;

const MAX_IMAGE_BYTES: u64 = 3_932_160; // 3.75MB
const MAX_AUDIO_BYTES: u64 = 25 * 1024 * 1024; // 25MB
const SUPPORTED_IMAGE_MIMES: &[&str] = &["image/jpeg", "image/png", "image/gif", "image/webp"];

#[tokio::main]
async fn main() -> Result<()> {
    let mut config = Config::load().map_err(|_| anyhow!("Failed to load zdx config"))?;
    config.model = "claude-cli:claude-haiku-4-5".to_string();
    config.thinking_level = ThinkingLevel::Minimal;
    let settings = TelegramSettings::from_config(&config)?;
    let config_path = zdx_core::config::paths::config_path();
    if config_path.exists() {
        eprintln!("Config file: {}", config_path.display());
    }
    run_bot(config, settings).await
}

async fn run_bot(config: Config, settings: TelegramSettings) -> Result<()> {
    let root = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let effective = build_effective_system_prompt_with_paths(&config, &root)
        .map_err(|_| anyhow!("Failed to load system prompt context (AGENTS/skills)"))?;

    for warning in &effective.warnings {
        eprintln!("Warning: {}", warning.message);
    }

    let client = TelegramClient::new(settings.bot_token);
    let mut tool_registry = ToolRegistry::builtins();
    let (telegram_def, telegram_handler) = telegram::telegram_send_tool(client.clone());
    tool_registry.register(telegram_def, telegram_handler);
    let tool_config = ToolConfig::new(
        tool_registry,
        ToolSelection::Auto {
            base: ToolSet::Default,
            include: vec!["telegram_send".to_string()],
        },
    );
    let mut offset: Option<i64> = None;
    let poll_timeout = Duration::from_secs(30);
    let shutdown = tokio::signal::ctrl_c();
    tokio::pin!(shutdown);

    eprintln!(
        "zdx-bot started. Allowlist users: {}. Polling for updates...",
        settings.allowlist_user_ids.len()
    );

    loop {
        let current_offset = offset;
        tokio::select! {
            _ = &mut shutdown => {
                eprintln!("Shutting down Telegram bot.");
                break;
            }
            updates = client.get_updates(current_offset, poll_timeout) => {
                let updates = match updates {
                    Ok(updates) => updates,
                    Err(err) => {
                        eprintln!("Telegram polling error: {}", err);
                        tokio::time::sleep(Duration::from_secs(1)).await;
                        continue;
                    }
                };

                if !updates.is_empty() {
                    eprintln!("Received {} update(s)", updates.len());
                }
                for update in updates {
                    offset = Some(update.update_id + 1);
                    if let Some(message) = update.message
                        && let Err(err) = handle_message(
                            &client,
                            &config,
                            &settings.allowlist_user_ids,
                            &root,
                            effective.prompt.as_deref(),
                            message,
                            &tool_config,
                        )
                        .await
                    {
                        eprintln!("Message handling error: {}", err);
                    }
                }
            }
        }
    }

    Ok(())
}

async fn handle_message(
    client: &TelegramClient,
    config: &Config,
    allowlist: &HashSet<i64>,
    root: &Path,
    system_prompt: Option<&str>,
    message: Message,
    tool_config: &ToolConfig,
) -> Result<()> {
    let Some(incoming) = parse_incoming_message(client, allowlist, config, message).await? else {
        return Ok(());
    };

    eprintln!(
        "Accepted message from user {} in chat {}",
        incoming.user_id, incoming.chat_id
    );

    if incoming.images.is_empty()
        && incoming.audios.is_empty()
        && let Some(text) = incoming.text.as_deref()
        && is_new_command(text)
    {
        let thread_id = thread_id_for_chat(incoming.chat_id);
        clear_thread_history(&thread_id)?;
        client
            .send_message(
                incoming.chat_id,
                "History cleared. Start a new conversation anytime.",
                Some(incoming.message_id),
            )
            .await?;
        return Ok(());
    }

    let thread_id = thread_id_for_chat(incoming.chat_id);
    let (mut thread, mut messages) = load_thread_state(&thread_id)?;
    record_user_message(&mut thread, &mut messages, &incoming)?;

    let result = run_agent_turn_with_persist(
        messages,
        config,
        root,
        system_prompt,
        &thread_id,
        &thread,
        tool_config,
    )
    .await;

    match result {
        Ok((final_text, _messages)) => {
            thread
                .append(&ThreadEvent::assistant_message(&final_text))
                .map_err(|_| anyhow!("Failed to append assistant message"))?;
            if !final_text.trim().is_empty() {
                eprintln!("Sending reply for chat {}", incoming.chat_id);
                client
                    .send_message(incoming.chat_id, &final_text, Some(incoming.message_id))
                    .await?;
            }
        }
        Err(err) => {
            eprintln!("Agent error: {}", err);
            let _ = client
                .send_message(
                    incoming.chat_id,
                    "Sorry, something went wrong.",
                    Some(incoming.message_id),
                )
                .await;
        }
    }

    Ok(())
}

fn thread_id_for_chat(chat_id: i64) -> String {
    format!("telegram-{}", chat_id)
}

fn is_new_command(text: &str) -> bool {
    text.trim() == "/new"
}

async fn parse_incoming_message(
    client: &TelegramClient,
    allowlist: &HashSet<i64>,
    config: &Config,
    message: Message,
) -> Result<Option<IncomingMessage>> {
    if !message.chat.is_private() {
        eprintln!("Ignoring non-DM chat {}", message.chat.id);
        return Ok(None);
    }

    let chat_id = message.chat.id;
    let message_id = message.message_id;

    let Some(user) = message.from.as_ref() else {
        eprintln!("Ignoring message without sender in chat {}", chat_id);
        return Ok(None);
    };

    if !allowlist.contains(&user.id) {
        eprintln!("Denied user {} for chat {}", user.id, chat_id);
        let _ = client
            .send_message(chat_id, "Access denied.", Some(message_id))
            .await;
        return Ok(None);
    }

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
                Err(err) => eprintln!("Failed to load photo attachment: {}", err),
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
                Err(err) => eprintln!("Failed to load document image: {}", err),
            }
        } else if mime.starts_with("audio/") {
            had_attachments = true;
            match load_audio_attachment(client, config, chat_id, message_id, document).await {
                Ok(Some(audio)) => audios.push(audio),
                Ok(None) => {}
                Err(err) => eprintln!("Failed to load document audio: {}", err),
            }
        }
    }

    if let Some(voice) = message.voice.as_ref() {
        had_attachments = true;
        match load_voice_attachment(client, config, chat_id, message_id, voice).await {
            Ok(Some(audio)) => audios.push(audio),
            Ok(None) => {}
            Err(err) => eprintln!("Failed to load voice attachment: {}", err),
        }
    }

    if let Some(audio) = message.audio.as_ref() {
        had_attachments = true;
        match load_audio_message(client, config, chat_id, message_id, audio).await {
            Ok(Some(audio)) => audios.push(audio),
            Ok(None) => {}
            Err(err) => eprintln!("Failed to load audio message: {}", err),
        }
    }

    if text.is_none() && images.is_empty() && audios.is_empty() {
        if had_attachments {
            eprintln!("Unsupported attachment in chat {}", chat_id);
            let _ = client
                .send_message(
                    chat_id,
                    "Sorry, I couldn't read that attachment.",
                    Some(message_id),
                )
                .await;
        } else {
            eprintln!("Ignoring empty message in chat {}", chat_id);
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
        let area = (photo.width.max(0) as u64) * (photo.height.max(0) as u64);
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
        eprintln!("Skipping photo > max image size in chat {}", chat_id);
        return Ok(None);
    }

    let (file_path, bytes) = download_file_bytes(client, &photo.file_id).await?;
    if bytes.len() as u64 > MAX_IMAGE_BYTES {
        eprintln!(
            "Downloaded photo exceeds max image size in chat {}",
            chat_id
        );
        return Ok(None);
    }

    let mime_type = match detect_image_mime(&bytes) {
        Some(mime) => mime,
        None => {
            eprintln!("Unsupported image type in chat {}", chat_id);
            return Ok(None);
        }
    };

    let filename =
        file_name_from_path(&file_path).unwrap_or_else(|| format!("photo_{}.bin", message_id));
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
        eprintln!("Skipping document image > max size in chat {}", chat_id);
        return Ok(None);
    }

    let (file_path, bytes) = download_file_bytes(client, &document.file_id).await?;
    if bytes.len() as u64 > MAX_IMAGE_BYTES {
        eprintln!(
            "Downloaded document image exceeds max size in chat {}",
            chat_id
        );
        return Ok(None);
    }

    let mime_type = match select_image_mime(document.mime_type.as_deref(), &bytes) {
        Some(mime) => mime,
        None => {
            eprintln!("Unsupported document image type in chat {}", chat_id);
            return Ok(None);
        }
    };

    let filename = document
        .file_name
        .as_deref()
        .and_then(file_name_from_path)
        .or_else(|| file_name_from_path(&file_path))
        .unwrap_or_else(|| format!("image_{}.bin", message_id));
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

#[allow(clippy::too_many_arguments)]
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
        eprintln!("Skipping audio > max size in chat {}", chat_id);
        return Ok(None);
    }

    let (file_path, bytes) = download_file_bytes(client, file_id).await?;
    if bytes.len() as u64 > MAX_AUDIO_BYTES {
        eprintln!("Downloaded audio exceeds max size in chat {}", chat_id);
        return Ok(None);
    }

    let filename = file_name_hint
        .and_then(file_name_from_path)
        .or_else(|| file_name_from_path(&file_path))
        .unwrap_or_else(|| format!("audio_{}.bin", message_id));
    let local_path = save_media_bytes(chat_id, message_id, &filename, &bytes)?;

    let transcript =
        match transcribe::transcribe_audio_if_configured(config, bytes, &filename, mime_type).await
        {
            Ok(transcript) => transcript,
            Err(err) => {
                eprintln!("Audio transcription failed: {}", err);
                None
            }
        };

    Ok(Some(IncomingAudio {
        local_path,
        transcript,
    }))
}

fn build_user_text(incoming: &IncomingMessage) -> String {
    let mut parts = Vec::new();
    if let Some(text) = incoming.text.as_ref()
        && !text.trim().is_empty()
    {
        parts.push(text.clone());
    }

    for audio in &incoming.audios {
        if let Some(transcript) = &audio.transcript {
            parts.push(format!("Audio transcript:\n{}", transcript));
        } else {
            parts.push(format!(
                "Audio attachment saved at {} (transcription unavailable).",
                audio.local_path.display()
            ));
        }
    }

    for image in &incoming.images {
        parts.push(format!(
            "Image attachment saved at {}.",
            image.local_path.display()
        ));
    }

    if parts.is_empty() {
        "User sent an attachment.".to_string()
    } else {
        parts.join("\n\n")
    }
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
        .map(|name| name.to_string())
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
    let path = dir.join(format!("{}_{}", message_id, safe_name));
    fs::write(&path, bytes).map_err(|_| anyhow!("Failed to write media file"))?;
    Ok(path)
}

fn load_thread_state(thread_id: &str) -> Result<(ThreadLog, Vec<ChatMessage>)> {
    let thread = ThreadLog::with_id(thread_id.to_string())
        .map_err(|_| anyhow!("Failed to open thread log"))?;
    let messages = thread_log::load_thread_as_messages(thread_id)
        .map_err(|_| anyhow!("Failed to load thread history"))?;
    Ok((thread, messages))
}

fn clear_thread_history(thread_id: &str) -> Result<()> {
    let thread = ThreadLog::with_id(thread_id.to_string())
        .map_err(|_| anyhow!("Failed to resolve thread log"))?;
    let path = thread.path();
    if path.exists() {
        fs::remove_file(path).map_err(|_| anyhow!("Failed to clear thread history"))?;
    }
    Ok(())
}

fn record_user_message(
    thread: &mut ThreadLog,
    messages: &mut Vec<ChatMessage>,
    incoming: &IncomingMessage,
) -> Result<()> {
    let text = build_user_text(incoming);
    thread
        .append(&ThreadEvent::user_message(text.clone()))
        .map_err(|_| anyhow!("Failed to append user message"))?;

    if incoming.images.is_empty() {
        messages.push(ChatMessage::user(text));
        return Ok(());
    }

    let mut blocks = Vec::with_capacity(1 + incoming.images.len());
    blocks.push(ChatContentBlock::Text(text));
    for image in &incoming.images {
        blocks.push(ChatContentBlock::Image {
            mime_type: image.mime_type.clone(),
            data: image.data.clone(),
        });
    }

    messages.push(ChatMessage {
        role: "user".to_string(),
        content: MessageContent::Blocks(blocks),
    });
    Ok(())
}

async fn run_agent_turn_with_persist(
    messages: Vec<ChatMessage>,
    config: &Config,
    root: &Path,
    system_prompt: Option<&str>,
    thread_id: &str,
    thread: &ThreadLog,
    tool_config: &ToolConfig,
) -> Result<(String, Vec<ChatMessage>)> {
    let agent_opts = AgentOptions {
        root: root.to_path_buf(),
        tool_config: tool_config.clone(),
    };

    let (agent_tx, agent_rx) = agent::create_event_channel();
    let persist_handle = thread_log::spawn_thread_persist_task(thread.clone(), agent_rx);

    let result = agent::run_turn(
        messages,
        config,
        &agent_opts,
        system_prompt,
        Some(thread_id),
        agent_tx,
    )
    .await;

    let _ = persist_handle.await;

    result
}
