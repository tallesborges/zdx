//! Audio transcription support for Telegram voice messages.

use anyhow::{Context, Result, anyhow};
use zdx_core::config::Config;
use zdx_core::providers::{ProviderKind, resolve_provider};

const DEFAULT_OPENAI_MODEL: &str = "whisper-1";
const DEFAULT_MISTRAL_MODEL: &str = "voxtral-mini-latest";

#[derive(serde::Deserialize)]
struct TranscriptionResponse {
    text: String,
}

/// Supported transcription providers.
const TRANSCRIPTION_PROVIDERS: &[ProviderKind] = &[ProviderKind::OpenAI, ProviderKind::Mistral];

/// Transcribes audio if a supported provider is configured.
///
/// Returns `Ok(None)` if no transcription provider is available.
///
/// # Errors
/// Returns an error if the operation fails.
pub async fn transcribe_audio_if_configured(
    config: &Config,
    bytes: Vec<u8>,
    filename: &str,
    mime_type: Option<&str>,
) -> Result<Option<String>> {
    let Some((provider, model)) = resolve_model(config)? else {
        return Ok(None);
    };

    let provider_config = config.providers.get(provider);
    let api_key = provider.resolve_api_key(provider_config.api_key.as_deref())?;
    let base_url = provider.resolve_base_url(provider_config.base_url.as_deref())?;

    let language = config
        .telegram
        .transcription
        .language
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());

    let transcript = transcribe_audio(TranscriptionRequest {
        provider_name: provider.label(),
        base_url: &base_url,
        api_key: &api_key,
        model: &model,
        bytes,
        filename,
        mime_type,
        language,
    })
    .await?;

    let trimmed = transcript.trim();
    if trimmed.is_empty() {
        Ok(None)
    } else {
        Ok(Some(trimmed.to_string()))
    }
}

/// Resolves the transcription provider and model.
///
/// Priority: `ZDX_TRANSCRIPTION_MODEL` env var > config > auto-detect first provider with API key.
/// Returns `Ok(None)` if no provider is available.
fn resolve_model(config: &Config) -> Result<Option<(ProviderKind, String)>> {
    let model_str = std::env::var("ZDX_TRANSCRIPTION_MODEL")
        .ok()
        .or_else(|| config.telegram.transcription.model.clone())
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty());

    if let Some(model_str) = model_str {
        let selection = resolve_provider(&model_str);
        if !TRANSCRIPTION_PROVIDERS.contains(&selection.kind) {
            return Err(anyhow!(
                "Unsupported transcription provider: {}. Only OpenAI and Mistral are supported.",
                selection.kind.label()
            ));
        }
        return Ok(Some((selection.kind, selection.model)));
    }

    // Auto-detect: first provider with available API key
    Ok(TRANSCRIPTION_PROVIDERS.iter().find_map(|&provider| {
        let provider_config = config.providers.get(provider);
        provider
            .resolve_api_key(provider_config.api_key.as_deref())
            .ok()
            .map(|_| (provider, default_model(provider).to_string()))
    }))
}

fn default_model(provider: ProviderKind) -> &'static str {
    match provider {
        ProviderKind::Mistral => DEFAULT_MISTRAL_MODEL,
        _ => DEFAULT_OPENAI_MODEL,
    }
}

struct TranscriptionRequest<'a> {
    provider_name: &'a str,
    base_url: &'a str,
    api_key: &'a str,
    model: &'a str,
    bytes: Vec<u8>,
    filename: &'a str,
    mime_type: Option<&'a str>,
    language: Option<&'a str>,
}

async fn transcribe_audio(request: TranscriptionRequest<'_>) -> Result<String> {
    let TranscriptionRequest {
        provider_name,
        base_url,
        api_key,
        model,
        bytes,
        filename,
        mime_type,
        language,
    } = request;

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
        .post(&url)
        .bearer_auth(api_key)
        .multipart(form)
        .send()
        .await
        .with_context(|| {
            format!(
                "{provider_name} transcription request failed (url={url}, model={model}, filename={filename}, mime_type={})",
                mime_type.unwrap_or("unknown")
            )
        })?;

    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        return Err(anyhow!(
            "{provider_name} transcription failed: {status} {body}"
        ));
    }

    let payload: TranscriptionResponse = response.json().await.with_context(|| {
        format!(
            "decode {provider_name} transcription response (model={model}, filename={filename})"
        )
    })?;
    Ok(payload.text)
}
