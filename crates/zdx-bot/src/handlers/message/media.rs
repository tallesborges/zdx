use std::path::{Path, PathBuf};

use anyhow::Result;
use zdx_engine::core::chat_turn::{SendAs, send_as};

use crate::telegram::markdown::to_telegram_html;
use crate::telegram::{MediaSend, Message, TelegramClient};

/// Sends one local file with the Telegram method that fits its type
/// (photo, voice note, audio, or document).
pub(super) async fn send_media_file(
    client: &TelegramClient,
    chat_id: i64,
    path: &Path,
    options: &MediaSend<'_>,
) -> Result<Message> {
    if is_image_path(path) {
        client.send_photo_from_path(chat_id, path, options).await
    } else if is_voice_note_path(path) {
        client.send_voice_from_path(chat_id, path, options).await
    } else if is_audio_path(path) {
        client.send_audio_from_path(chat_id, path, options).await
    } else {
        client.send_document_from_path(chat_id, path, options).await
    }
}

#[derive(Default)]
pub(super) struct ParsedFinalResponse {
    pub(super) text: String,
    pub(super) media_paths: Vec<PathBuf>,
    pub(super) followups: Vec<String>,
}

/// [`zdx_engine::core::chat_turn::reply_parts`], with the text as Telegram
/// HTML.
pub(super) fn parse_final_response(final_text: &str) -> ParsedFinalResponse {
    let parts = zdx_engine::core::chat_turn::reply_parts(final_text);
    ParsedFinalResponse {
        text: to_telegram_html(&parts.text),
        media_paths: parts.media.into_iter().map(|media| media.path).collect(),
        followups: parts.followups,
    }
}

pub(super) fn is_valid_media_path(path: &Path) -> bool {
    path.is_absolute() && std::fs::metadata(path).is_ok_and(|meta| meta.is_file())
}

pub(super) fn is_image_path(path: &Path) -> bool {
    send_as(path) == SendAs::Photo
}

pub(super) fn is_voice_note_path(path: &Path) -> bool {
    send_as(path) == SendAs::Voice
}

pub(super) fn is_audio_path(path: &Path) -> bool {
    send_as(path) == SendAs::Audio
}
