//! Transcribe command handler.

use std::fs;
use std::path::Path;

use anyhow::{Context, Result};
use zdx_engine::audio::transcribe::transcribe_audio_if_configured;
use zdx_engine::config::{self, TranscriptionConfig};

pub struct TranscribeRunOptions<'a> {
    pub file: &'a str,
    pub provider: Option<&'a str>,
    pub model: Option<&'a str>,
    pub language: Option<&'a str>,
    pub config: &'a config::Config,
}

pub async fn run(options: TranscribeRunOptions<'_>) -> Result<()> {
    let path = Path::new(options.file);
    let bytes = fs::read(path).with_context(|| format!("read audio file '{}'", path.display()))?;

    let filename = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("audio");
    let mime = mime_from_ext(path);

    let base = &options.config.transcription;
    let transcription = TranscriptionConfig {
        provider: options
            .provider
            .map(str::to_string)
            .or_else(|| base.provider.clone()),
        model: options
            .model
            .map(str::to_string)
            .or_else(|| base.model.clone()),
        language: options
            .language
            .map(str::to_string)
            .or_else(|| base.language.clone()),
    };

    let transcript =
        transcribe_audio_if_configured(options.config, &transcription, bytes, filename, mime, None)
            .await?;

    match transcript {
        Some(text) => println!("{text}"),
        None => eprintln!(
            "No transcription provider configured. Set OPENAI_API_KEY, MISTRAL_API_KEY, or XAI_API_KEY, or add a [transcription] provider in config.toml."
        ),
    }

    Ok(())
}

/// Guesses an audio MIME type from the file extension for the multipart upload.
///
/// Providers also infer the format from the filename, so an unknown extension
/// (`None`) is fine.
fn mime_from_ext(path: &Path) -> Option<&'static str> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    let mime = match ext.as_str() {
        "ogg" | "oga" | "opus" => "audio/ogg",
        "mp3" | "mpga" | "mpeg" => "audio/mpeg",
        "mp4" | "m4a" => "audio/mp4",
        "wav" => "audio/wav",
        "aac" => "audio/aac",
        "flac" => "audio/flac",
        "webm" => "audio/webm",
        _ => return None,
    };
    Some(mime)
}
