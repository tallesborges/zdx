//! One-shot media understanding: send a local audio/video/PDF/image file plus a
//! prompt to a Gemini model and return the model's text answer.
//!
//! This is a stateless helper (the file is re-sent on every call). It is the
//! shared core behind `zdx ask-media` and any future native tool.

use std::fs;
use std::path::Path;

use anyhow::{Context, Result, bail};

use crate::config::Config;
use crate::providers::gemini::{GeminiClient, GeminiConfig};
use crate::providers::{ProviderKind, resolve_provider};

/// Default model: fast, cheap, built for document parsing.
pub const DEFAULT_ASK_MEDIA_MODEL: &str = "gemini:gemini-3.5-flash-lite";

/// Max raw file size sent inline. Base64 inflates ~33%, so keep well under
/// Gemini's ~20 MB total inline request cap. Larger files need the File API
/// (not yet supported).
const MAX_INLINE_BYTES: usize = 15 * 1024 * 1024;

/// Reads `path`, sends it inline to a Gemini model with `prompt`, and returns
/// the model's text answer.
///
/// # Errors
/// Returns an error if `model_input` is not a Gemini model, the file is
/// missing/empty/too large, its type is unsupported, or the request fails.
pub async fn ask_media(
    path: &Path,
    prompt: &str,
    model_input: &str,
    config: &Config,
) -> Result<String> {
    let selection = resolve_provider(model_input);
    if selection.kind != ProviderKind::Gemini {
        bail!(
            "zdx ask-media only supports Gemini models (e.g. `gemini:gemini-3.5-flash-lite`); got `{model_input}`"
        );
    }

    let bytes = fs::read(path).with_context(|| format!("read media file `{}`", path.display()))?;
    if bytes.is_empty() {
        bail!("media file `{}` is empty", path.display());
    }
    if bytes.len() > MAX_INLINE_BYTES {
        bail!(
            "media file `{}` is {:.1} MiB; inline limit is {} MiB (File API upload not yet supported)",
            path.display(),
            bytes.len() as f64 / (1024.0 * 1024.0),
            MAX_INLINE_BYTES / (1024 * 1024)
        );
    }

    let mime = media_mime_for_path(path).ok_or_else(|| {
        anyhow::anyhow!(
            "unsupported media type for `{}` (supported: PDF, common image/audio/video formats)",
            path.display()
        )
    })?;

    let gemini_config = GeminiConfig::from_env(
        selection.model.clone(),
        None,
        config.providers.gemini.effective_base_url(),
        config.providers.gemini.effective_api_key(),
        None,
    )?;

    GeminiClient::new(gemini_config)
        .generate_text_from_media(mime, &bytes, prompt)
        .await
        .context("Gemini media understanding request failed")
}

/// Maps a file extension to a Gemini-supported MIME type.
fn media_mime_for_path(path: &Path) -> Option<&'static str> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    let mime = match ext.as_str() {
        // documents
        "pdf" => "application/pdf",
        "txt" | "text" => "text/plain",
        // images
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "webp" => "image/webp",
        "gif" => "image/gif",
        "heic" => "image/heic",
        "heif" => "image/heif",
        // audio
        "mp3" => "audio/mp3",
        "wav" => "audio/wav",
        "ogg" | "oga" | "opus" => "audio/ogg",
        "m4a" => "audio/mp4",
        "aac" => "audio/aac",
        "flac" => "audio/flac",
        // video
        "mp4" => "video/mp4",
        "mov" => "video/quicktime",
        "mpeg" | "mpg" => "video/mpeg",
        "webm" => "video/webm",
        "flv" => "video/x-flv",
        "wmv" => "video/wmv",
        "3gp" | "3gpp" => "video/3gpp",
        _ => return None,
    };
    Some(mime)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_common_media_extensions() {
        assert_eq!(
            media_mime_for_path(Path::new("a.pdf")),
            Some("application/pdf")
        );
        assert_eq!(
            media_mime_for_path(Path::new("A.PDF")),
            Some("application/pdf")
        );
        assert_eq!(
            media_mime_for_path(Path::new("clip.mp4")),
            Some("video/mp4")
        );
        assert_eq!(
            media_mime_for_path(Path::new("voice.ogg")),
            Some("audio/ogg")
        );
        assert_eq!(
            media_mime_for_path(Path::new("pic.jpeg")),
            Some("image/jpeg")
        );
    }

    #[test]
    fn rejects_unknown_extension() {
        assert_eq!(media_mime_for_path(Path::new("archive.zip")), None);
        assert_eq!(media_mime_for_path(Path::new("noext")), None);
    }
}
