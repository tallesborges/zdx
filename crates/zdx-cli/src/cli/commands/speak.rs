//! Speak command handler.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::Utc;
use zdx_engine::audio::speak::{SpeechAudio, synthesize_speech};
use zdx_engine::config::{self, SpeechConfig};

pub struct SpeakRunOptions<'a> {
    pub root: &'a Path,
    pub text: &'a str,
    pub out: Option<&'a str>,
    pub model: Option<&'a str>,
    pub voice: Option<&'a str>,
    pub format: Option<&'a str>,
    pub config: &'a config::Config,
}

pub async fn run(options: SpeakRunOptions<'_>) -> Result<()> {
    let base = &options.config.speech;
    let speech = SpeechConfig {
        model: options
            .model
            .map(str::to_string)
            .or_else(|| base.model.clone()),
        voice: options
            .voice
            .map(str::to_string)
            .or_else(|| base.voice.clone()),
        format: options
            .format
            .map(str::to_string)
            .or_else(|| base.format.clone())
            .or_else(|| format_from_out_path(options.out)),
    };

    let audio = synthesize_speech(options.config, &speech, options.text, None).await?;

    let path = resolve_output_path(options.root, options.out, &audio);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("create output directory '{}'", parent.display()))?;
    }
    fs::write(&path, &audio.bytes)
        .with_context(|| format!("write audio to '{}'", path.display()))?;
    println!("{}", path.display());

    Ok(())
}

fn format_from_out_path(out: Option<&str>) -> Option<String> {
    let out = out.map(str::trim).filter(|v| !v.is_empty())?;
    let ext = Path::new(out).extension()?.to_str()?.to_ascii_lowercase();
    matches!(ext.as_str(), "mp3" | "aac" | "flac" | "wav" | "pcm").then_some(ext)
}

fn resolve_output_path(root: &Path, out: Option<&str>, audio: &SpeechAudio) -> PathBuf {
    if let Some(out) = out.map(str::trim).filter(|v| !v.is_empty()) {
        let path = PathBuf::from(out);
        return if path.is_absolute() {
            path
        } else {
            root.join(path)
        };
    }

    let ts = Utc::now().format("%Y%m%d-%H%M%S");
    config::paths::artifact_root()
        .join("speech")
        .join(format!("speech-{ts}.{}", audio.ext))
}
