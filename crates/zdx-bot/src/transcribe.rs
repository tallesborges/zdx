//! Audio transcription support for Telegram voice messages.

use anyhow::{Result, anyhow};
use zdx_core::config::Config;
use zdx_core::providers::{ProviderKind, resolve_api_key, resolve_base_url};

const DEFAULT_OPENAI_MODEL: &str = "whisper-1";
const DEFAULT_MISTRAL_MODEL: &str = "voxtral-mini-latest";
const DEFAULT_OPENAI_BASE_URL: &str = "https://api.openai.com/v1";
const DEFAULT_MISTRAL_BASE_URL: &str = "https://api.mistral.ai/v1";

/// Supported transcription providers.
const TRANSCRIPTION_PROVIDERS: &[ProviderKind] = &[ProviderKind::OpenAI, ProviderKind::Mistral];

/// Transcribes audio if a supported provider is configured.
///
/// Returns `Ok(None)` if no transcription provider is available.
pub async fn transcribe_audio_if_configured(
    config: &Config,
    bytes: Vec<u8>,
    filename: &str,
    mime_type: Option<&str>,
) -> Result<Option<String>> {
    let provider = match detect_provider(config) {
        Some(p) => p,
        None => return Ok(None),
    };

    let provider_config = config.providers.get(provider);
    let api_key = resolve_api_key(
        provider_config.api_key.as_deref(),
        provider.api_key_env_var().unwrap_or_default(),
        provider.id(),
    )?;
    let base_url = resolve_base_url(
        provider_config.base_url.as_deref(),
        &format!("{}_BASE_URL", provider.id().to_uppercase()),
        default_base_url(provider),
        provider.label(),
    )?;

    let model = resolve_model(config, provider);
    let language = config
        .telegram
        .transcription
        .language
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());

    let transcript = transcribe_audio(
        provider.label(),
        &base_url,
        &api_key,
        &model,
        bytes,
        filename,
        mime_type,
        language,
    )
    .await?;

    let trimmed = transcript.trim();
    if trimmed.is_empty() {
        Ok(None)
    } else {
        Ok(Some(trimmed.to_string()))
    }
}

/// Detects which transcription provider to use.
///
/// Priority: env var > config > auto-detect (first available).
fn detect_provider(config: &Config) -> Option<ProviderKind> {
    // Check env var override
    if let Some(provider) = parse_provider_from_env("ZDX_TRANSCRIPTION_PROVIDER") {
        return Some(provider);
    }

    // Check config setting
    if let Some(provider) = config
        .telegram
        .transcription
        .provider
        .as_deref()
        .and_then(parse_provider_str)
    {
        return Some(provider);
    }

    // Auto-detect: first provider with available API key
    for &provider in TRANSCRIPTION_PROVIDERS {
        let provider_config = config.providers.get(provider);
        if resolve_api_key(
            provider_config.api_key.as_deref(),
            provider.api_key_env_var().unwrap_or_default(),
            provider.id(),
        )
        .is_ok()
        {
            return Some(provider);
        }
    }

    None
}

fn parse_provider_from_env(var: &str) -> Option<ProviderKind> {
    std::env::var(var)
        .ok()
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .and_then(parse_provider_str)
}

fn parse_provider_str(s: &str) -> Option<ProviderKind> {
    match s.to_lowercase().as_str() {
        "openai" => Some(ProviderKind::OpenAI),
        "mistral" => Some(ProviderKind::Mistral),
        _ => None,
    }
}

fn default_base_url(provider: ProviderKind) -> &'static str {
    match provider {
        ProviderKind::OpenAI => DEFAULT_OPENAI_BASE_URL,
        ProviderKind::Mistral => DEFAULT_MISTRAL_BASE_URL,
        _ => DEFAULT_OPENAI_BASE_URL,
    }
}

fn default_model(provider: ProviderKind) -> &'static str {
    match provider {
        ProviderKind::OpenAI => DEFAULT_OPENAI_MODEL,
        ProviderKind::Mistral => DEFAULT_MISTRAL_MODEL,
        _ => DEFAULT_OPENAI_MODEL,
    }
}

fn resolve_model(config: &Config, provider: ProviderKind) -> String {
    // env var > config > default per provider
    std::env::var("ZDX_TELEGRAM_AUDIO_MODEL")
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .or_else(|| {
            config
                .telegram
                .transcription
                .model
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(String::from)
        })
        .unwrap_or_else(|| default_model(provider).to_string())
}

#[allow(clippy::too_many_arguments)]
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
