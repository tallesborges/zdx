use anyhow::{Result, anyhow};
use zdx_core::config::Config;

const DEFAULT_AUDIO_MODEL: &str = "whisper-1";
const DEFAULT_MISTRAL_MODEL: &str = "voxtral-mini-latest";
const DEFAULT_OPENAI_BASE_URL: &str = "https://api.openai.com/v1";
const DEFAULT_MISTRAL_BASE_URL: &str = "https://api.mistral.ai/v1";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TranscriptionProvider {
    OpenAI,
    Mistral,
}

impl TranscriptionProvider {
    fn from_str(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "openai" => Some(Self::OpenAI),
            "mistral" => Some(Self::Mistral),
            _ => None,
        }
    }
}

pub async fn transcribe_audio_if_configured(
    config: &Config,
    bytes: Vec<u8>,
    filename: &str,
    mime_type: Option<&str>,
) -> Result<Option<String>> {
    // Determine provider (config > env var auto-detect)
    let provider = if let Some(provider_str) = std::env::var("ZDX_TRANSCRIPTION_PROVIDER")
        .ok()
        .and_then(|s| {
            let s = s.trim();
            if s.is_empty() {
                None
            } else {
                Some(s.to_string())
            }
        })
        .and_then(|s| TranscriptionProvider::from_str(&s))
    {
        // Explicit env var override
        provider_str
    } else if let Some(provider_str) = config
        .transcription
        .provider
        .as_deref()
        .and_then(|s| {
            let s = s.trim();
            if s.is_empty() {
                None
            } else {
                Some(s.to_string())
            }
        })
        .and_then(|s| TranscriptionProvider::from_str(&s))
    {
        // Explicit config setting
        provider_str
    } else {
        // Auto-detect: prefer OpenAI (backward compatible), fall back to Mistral
        if openai_api_key(config).is_some() {
            TranscriptionProvider::OpenAI
        } else if mistral_api_key(config).is_some() {
            TranscriptionProvider::Mistral
        } else {
            return Ok(None);
        }
    };

    // Determine model (env var > config > default per provider)
    let model = std::env::var("ZDX_TELEGRAM_AUDIO_MODEL")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .or_else(|| {
            config
                .transcription
                .model
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(String::from)
        })
        .unwrap_or_else(|| match provider {
            TranscriptionProvider::OpenAI => DEFAULT_AUDIO_MODEL.to_string(),
            TranscriptionProvider::Mistral => DEFAULT_MISTRAL_MODEL.to_string(),
        });

    // Get language hint if configured
    let language = config
        .transcription
        .language
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from);

    match provider {
        TranscriptionProvider::OpenAI => {
            let api_key =
                openai_api_key(config).ok_or_else(|| anyhow!("OpenAI API key not configured"))?;
            let base_url = openai_base_url(config);
            let transcript = transcribe_audio(
                "OpenAI",
                &base_url,
                &api_key,
                &model,
                bytes,
                filename,
                mime_type,
                language.as_deref(),
            )
            .await?;
            let trimmed = transcript.trim();
            if trimmed.is_empty() {
                Ok(None)
            } else {
                Ok(Some(trimmed.to_string()))
            }
        }
        TranscriptionProvider::Mistral => {
            let api_key =
                mistral_api_key(config).ok_or_else(|| anyhow!("Mistral API key not configured"))?;
            let base_url = mistral_base_url(config);
            let transcript = transcribe_audio(
                "Mistral",
                &base_url,
                &api_key,
                &model,
                bytes,
                filename,
                mime_type,
                language.as_deref(),
            )
            .await?;
            let trimmed = transcript.trim();
            if trimmed.is_empty() {
                Ok(None)
            } else {
                Ok(Some(trimmed.to_string()))
            }
        }
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

fn mistral_api_key(config: &Config) -> Option<String> {
    config
        .providers
        .mistral
        .api_key
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .or_else(|| {
            std::env::var("MISTRAL_API_KEY")
                .ok()
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty())
        })
}

fn mistral_base_url(config: &Config) -> String {
    config
        .providers
        .mistral
        .base_url
        .clone()
        .unwrap_or_else(|| DEFAULT_MISTRAL_BASE_URL.to_string())
}

async fn transcribe_audio(
    provider_name: &str,
    base_url: &str,
    api_key: &str,
    model: &str,
    bytes: Vec<u8>,
    filename: &str,
    mime_type: Option<&str>,
    language: Option<&str>,
) -> Result<String> {
    let client = reqwest::Client::new();
    let mut part = reqwest::multipart::Part::bytes(bytes).file_name(filename.to_string());
    if let Some(mime) = mime_type
        && !mime.trim().is_empty()
    {
        part = part.mime_str(mime)?;
    }

    let mut form = reqwest::multipart::Form::new()
        .text("model", model.to_string())
        .part("file", part);

    if let Some(lang) = language {
        form = form.text("language", lang.to_string());
    }

    let url = format!("{}/audio/transcriptions", base_url.trim_end_matches('/'));
    let response = client
        .post(url)
        .bearer_auth(api_key)
        .multipart(form)
        .send()
        .await
        .map_err(|_| anyhow!("{} transcription request failed", provider_name))?;

    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        return Err(anyhow!(
            "{} transcription failed: {} {}",
            provider_name,
            status,
            body
        ));
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
