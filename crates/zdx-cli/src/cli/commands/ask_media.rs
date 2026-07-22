//! Ask-media command handler: one-shot audio/video/PDF understanding via Gemini.

use std::path::Path;

use anyhow::{Result, anyhow};
use zdx_engine::config;
use zdx_engine::media::{DEFAULT_ASK_MEDIA_MODEL, ask_media};

pub struct AskMediaRunOptions<'a> {
    pub file: Option<&'a str>,
    pub prompt: &'a str,
    pub model: Option<&'a str>,
    pub json: bool,
    pub config: &'a config::Config,
}

pub async fn run(options: AskMediaRunOptions<'_>) -> Result<()> {
    let file = options
        .file
        .ok_or_else(|| anyhow!("no media file provided (pass a file path)"))?;
    let model = options.model.unwrap_or(DEFAULT_ASK_MEDIA_MODEL);

    let answer = ask_media(Path::new(file), options.prompt, model, options.config).await?;

    if options.json {
        println!(
            "{}",
            serde_json::json!({ "file": file, "model": model, "answer": answer })
        );
    } else {
        println!("{answer}");
    }
    Ok(())
}
