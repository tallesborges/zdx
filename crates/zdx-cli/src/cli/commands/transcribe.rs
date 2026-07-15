//! Transcribe command handler.

use std::fmt::Write as _;
use std::fs;
use std::path::Path;

use anyhow::{Context, Result, anyhow};
use zdx_engine::audio::transcribe::{
    Transcript, is_provider_configured, supported_models, transcribe_audio_detailed,
};
use zdx_engine::config::{self, TranscriptionConfig};

pub struct TranscribeRunOptions<'a> {
    pub file: Option<&'a str>,
    pub model: Option<&'a str>,
    pub language: Option<&'a str>,
    pub diarize: bool,
    pub json: bool,
    pub config: &'a config::Config,
}

pub async fn run(options: TranscribeRunOptions<'_>) -> Result<()> {
    let file = options
        .file
        .ok_or_else(|| anyhow!("no audio file provided (pass a file path or --list-models)"))?;
    let path = Path::new(file);
    let bytes = fs::read(path).with_context(|| format!("read audio file '{}'", path.display()))?;

    let filename = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("audio");
    let mime = mime_from_ext(path);

    let base = &options.config.transcription;
    let transcription = TranscriptionConfig {
        model: options
            .model
            .map(str::to_string)
            .or_else(|| base.model.clone()),
        language: options
            .language
            .map(str::to_string)
            .or_else(|| base.language.clone()),
    };

    let transcript = transcribe_audio_detailed(
        options.config,
        &transcription,
        bytes,
        filename,
        mime,
        options.diarize,
        None,
    )
    .await?;

    match transcript {
        Some(transcript) if options.json => {
            println!("{}", serde_json::to_string_pretty(&transcript)?);
        }
        Some(transcript) if options.diarize && !transcript.segments.is_empty() => {
            println!("{}", format_diarized(&transcript));
        }
        Some(transcript) => println!("{}", transcript.text),
        None => eprintln!(
            "No transcription provider configured. Set OPENAI_API_KEY, MISTRAL_API_KEY, XAI_API_KEY, or ELEVENLABS_API_KEY, or set a model via --model or [transcription].model in config.toml."
        ),
    }

    Ok(())
}

/// Prints the supported transcription models, marking which have an API key.
pub fn list_models(config: &config::Config, json: bool) -> Result<()> {
    let models = supported_models();

    if json {
        let rows: Vec<_> = models
            .iter()
            .map(|m| {
                serde_json::json!({
                    "provider": m.provider,
                    "label": m.label,
                    "id": m.id,
                    "model": m.model,
                    "api_key_env": m.api_key_env,
                    "diarize": m.diarize,
                    "configured": is_provider_configured(config, m.provider),
                })
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&rows)?);
        return Ok(());
    }

    println!("Supported transcription models (use with --model):");
    for m in &models {
        let status = if is_provider_configured(config, m.provider) {
            "configured"
        } else {
            "no key"
        };
        let diarize = if m.diarize { " · diarize" } else { "" };
        println!(
            "  {:<28} {:<18} [{}]{}",
            m.id,
            m.api_key_env.unwrap_or("-"),
            status,
            diarize
        );
    }
    Ok(())
}

/// Renders a diarized transcript as `[mm:ss] Speaker N: text` blocks.
fn format_diarized(transcript: &Transcript) -> String {
    let mut out = String::new();
    for segment in &transcript.segments {
        if !out.is_empty() {
            out.push('\n');
        }
        if let Some(start) = segment.start {
            let _ = write!(out, "[{}] ", format_timestamp(start));
        }
        let _ = write!(
            out,
            "{}: {}",
            speaker_label(segment.speaker.as_deref()),
            segment.text
        );
    }
    out
}

/// Normalizes a provider speaker id (e.g. `speaker_0`) to `Speaker 1`.
fn speaker_label(speaker: Option<&str>) -> String {
    match speaker {
        Some(raw) => {
            let index = raw
                .rsplit(['_', '-'])
                .next()
                .and_then(|n| n.parse::<u32>().ok());
            match index {
                Some(n) => format!("Speaker {}", n + 1),
                None => format!("Speaker {raw}"),
            }
        }
        None => "Speaker 1".to_string(),
    }
}

fn format_timestamp(seconds: f64) -> String {
    let total = seconds.max(0.0).round() as u64;
    format!("{:02}:{:02}", total / 60, total % 60)
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
