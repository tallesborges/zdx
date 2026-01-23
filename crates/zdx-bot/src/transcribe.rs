use anyhow::{Result, anyhow};
use zdx_core::config::Config;

const DEFAULT_AUDIO_MODEL: &str = "whisper-1";
const DEFAULT_OPENAI_BASE_URL: &str = "https://api.openai.com/v1";

pub async fn transcribe_audio_if_configured(
    config: &Config,
    bytes: Vec<u8>,
    filename: &str,
    mime_type: Option<&str>,
) -> Result<Option<String>> {
    let api_key = match openai_api_key(config) {
        Some(key) => key,
        None => return Ok(None),
    };
    let base_url = openai_base_url(config);
    let model = std::env::var("ZDX_TELEGRAM_AUDIO_MODEL")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| DEFAULT_AUDIO_MODEL.to_string());

    let transcript =
        transcribe_audio(&base_url, &api_key, &model, bytes, filename, mime_type).await?;
    let trimmed = transcript.trim();
    if trimmed.is_empty() {
        Ok(None)
    } else {
        Ok(Some(trimmed.to_string()))
    }
}

fn openai_api_key(config: &Config) -> Option<String> {
    config
        .providers
        .openai
        .api_key
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .or_else(|| {
            std::env::var("OPENAI_API_KEY")
                .ok()
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty())
        })
}

fn openai_base_url(config: &Config) -> String {
    config
        .providers
        .openai
        .base_url
        .clone()
        .unwrap_or_else(|| DEFAULT_OPENAI_BASE_URL.to_string())
}

async fn transcribe_audio(
    base_url: &str,
    api_key: &str,
    model: &str,
    bytes: Vec<u8>,
    filename: &str,
    mime_type: Option<&str>,
) -> Result<String> {
    let client = reqwest::Client::new();
    let mut part = reqwest::multipart::Part::bytes(bytes).file_name(filename.to_string());
    if let Some(mime) = mime_type
        && !mime.trim().is_empty()
    {
        part = part.mime_str(mime)?;
    }

    let form = reqwest::multipart::Form::new()
        .text("model", model.to_string())
        .part("file", part);

    let url = format!("{}/audio/transcriptions", base_url.trim_end_matches('/'));
    let response = client
        .post(url)
        .bearer_auth(api_key)
        .multipart(form)
        .send()
        .await
        .map_err(|_| anyhow!("OpenAI transcription request failed"))?;

    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        return Err(anyhow!("OpenAI transcription failed: {} {}", status, body));
    }

    #[derive(serde::Deserialize)]
    struct TranscriptionResponse {
        text: String,
    }

    let payload: TranscriptionResponse = response
        .json()
        .await
        .map_err(|_| anyhow!("Failed to decode transcription response"))?;
    Ok(payload.text)
}
