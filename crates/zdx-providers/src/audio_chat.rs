//! One-shot audio understanding over OpenAI-style Chat Completions: send one
//! audio clip plus a text prompt as an `input_audio` content part and return
//! the model's text answer.
//!
//! This module builds, sends, and parses the request. The per-provider values
//! (payload shape, formats, limits, request options) come from config as
//! [`AudioSettings`]; which models take audio comes from `model_overrides.toml`.
//! Both are resolved in `zdx-engine`.

use anyhow::{Context, Result, bail};
use base64::Engine as _;
use reqwest::header::HeaderMap;
use serde_json::{Value, json};
use zdx_types::ProviderError;

use crate::oauth::muse_code as oauth_muse_code;

/// Audio container formats `ask_media` can name for a provider.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioFormat {
    Wav,
    Mp3,
    Ogg,
    Flac,
    M4a,
    Aac,
}

impl AudioFormat {
    /// Maps a file extension (case-insensitive, without the dot).
    #[must_use]
    pub fn from_extension(ext: &str) -> Option<Self> {
        Some(match ext.to_ascii_lowercase().as_str() {
            "wav" => Self::Wav,
            "mp3" => Self::Mp3,
            "ogg" | "oga" | "opus" => Self::Ogg,
            "flac" => Self::Flac,
            "m4a" => Self::M4a,
            "aac" => Self::Aac,
            _ => return None,
        })
    }

    /// The `format` value providers expect.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Wav => "wav",
            Self::Mp3 => "mp3",
            Self::Ogg => "ogg",
            Self::Flac => "flac",
            Self::M4a => "m4a",
            Self::Aac => "aac",
        }
    }

    /// MIME type for a `data:` URI.
    #[must_use]
    pub fn mime(self) -> &'static str {
        match self {
            Self::Wav => "audio/wav",
            Self::Mp3 => "audio/mpeg",
            Self::Ogg => "audio/ogg",
            Self::Flac => "audio/flac",
            Self::M4a => "audio/mp4",
            Self::Aac => "audio/aac",
        }
    }
}

/// How the clip is written into the `input_audio` content part.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioPayload {
    /// `{"data": "data:<mime>;base64,<b64>"}`
    DataUri,
    /// `{"data": "data:<mime>;base64,<b64>", "format": "<fmt>"}`
    DataUriWithFormat,
    /// `{"data": "<b64>", "format": "<fmt>"}`
    Base64WithFormat,
    /// `"<b64>"`
    Base64String,
}

impl AudioPayload {
    /// Config names, in the order error messages list them.
    pub const NAMES: &'static [&'static str] = &[
        "data-uri",
        "data-uri-with-format",
        "base64-with-format",
        "base64",
    ];

    /// Parses a config name (see [`Self::NAMES`]).
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        Some(match name.trim() {
            "data-uri" => Self::DataUri,
            "data-uri-with-format" => Self::DataUriWithFormat,
            "base64-with-format" => Self::Base64WithFormat,
            "base64" => Self::Base64String,
            _ => return None,
        })
    }
}

/// Validated audio request settings for one provider. The values come from
/// config (shipped `audio_providers.toml` plus `[providers.<name>.audio]`, or
/// `[providers.custom.<name>.audio]`); this module only applies them, with no
/// per-provider logic.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioSettings {
    /// Provider name for messages (`StepFun`, or a custom provider's name).
    pub label: String,
    pub payload: AudioPayload,
    /// Formats sent as-is. Anything else is converted to WAV first.
    pub formats: Vec<AudioFormat>,
    /// Provider limit on the base64 string.
    pub max_base64_bytes: Option<usize>,
    /// Output budget to request.
    pub max_tokens: Option<u32>,
    /// Send `modalities: ["text"]`.
    pub text_modalities: bool,
}

/// One audio question for [`ask_audio`].
#[derive(Debug, Clone, Copy)]
pub struct AudioQuestion<'a> {
    pub model: &'a str,
    pub prompt: &'a str,
    pub audio: &'a [u8],
    pub format: AudioFormat,
}

/// How the request authenticates.
#[derive(Debug, Clone)]
pub enum AudioAuth<'a> {
    /// `Authorization: Bearer <key>`, already resolved by the caller.
    Bearer(String),
    /// Muse Code subscription key minted by `zdx login --muse-code`; re-minted
    /// once when Meta rejects it.
    MuseCode { account: Option<&'a str> },
}

/// Where and how to send the request.
#[derive(Debug, Clone)]
pub struct AudioRoute<'a> {
    pub settings: &'a AudioSettings,
    /// Chat Completions root; `/chat/completions` is appended.
    pub base_url: String,
    pub auth: AudioAuth<'a>,
}

/// Builds the Chat Completions body for one audio question.
#[must_use]
pub fn build_request_body(settings: &AudioSettings, question: &AudioQuestion<'_>) -> Value {
    let b64 = base64::engine::general_purpose::STANDARD.encode(question.audio);
    let data_uri = || format!("data:{};base64,{b64}", question.format.mime());
    let input_audio = match settings.payload {
        AudioPayload::DataUri => json!({ "data": data_uri() }),
        AudioPayload::DataUriWithFormat => {
            json!({ "data": data_uri(), "format": question.format.name() })
        }
        AudioPayload::Base64WithFormat => json!({ "data": b64, "format": question.format.name() }),
        AudioPayload::Base64String => json!(b64),
    };

    let mut body = json!({
        "model": question.model,
        "messages": [{
            "role": "user",
            "content": [
                { "type": "input_audio", "input_audio": input_audio },
                { "type": "text", "text": question.prompt }
            ]
        }]
    });
    if let Some(max_tokens) = settings.max_tokens {
        body["max_tokens"] = json!(max_tokens);
    }
    if settings.text_modalities {
        body["modalities"] = json!(["text"]);
    }
    body
}

/// Base64 length of `bytes` (padded).
#[must_use]
pub fn base64_len(bytes: usize) -> usize {
    bytes.div_ceil(3) * 4
}

/// Extracts the answer text from a Chat Completions response.
///
/// # Errors
/// Returns an error when the response has no text, naming the finish reason.
pub fn parse_answer(value: &Value) -> Result<String> {
    let choice = value
        .get("choices")
        .and_then(|choices| choices.get(0))
        .context("response has no choices")?;
    let content = &choice["message"]["content"];
    let text = match content {
        Value::String(text) => text.clone(),
        Value::Array(parts) => parts
            .iter()
            .filter_map(|part| part.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    };
    if text.trim().is_empty() {
        let reason = choice
            .get("finish_reason")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        bail!("model returned no text for the audio (finish_reason: {reason})");
    }
    Ok(text)
}

/// Sends one audio clip with a prompt and returns the model's text answer.
///
/// The caller decides that the model takes audio and that the provider takes
/// `question.format`; this function only sends the request.
///
/// # Errors
/// Returns an error if credentials are missing, the request fails, or the
/// response has no text.
pub async fn ask_audio(route: AudioRoute<'_>, question: AudioQuestion<'_>) -> Result<String> {
    let label = route.settings.label.as_str();
    let base_url = route.base_url.as_str();
    let body = build_request_body(route.settings, &question);
    let http = crate::shared::streaming_http_client();

    match route.auth {
        AudioAuth::Bearer(api_key) => send(&http, label, base_url, &api_key, &body).await,
        AudioAuth::MuseCode { account } => {
            let api_key = crate::muse_code::resolve_api_key(account)?;
            match send(&http, label, base_url, &api_key, &body).await {
                Err(error) if is_credential_rejection(&error) => {
                    let refreshed = oauth_muse_code::remint_rejected_key(account, &api_key)
                        .await
                        .context("Muse Code key was rejected and could not be re-minted")?;
                    send(&http, label, base_url, &refreshed.access, &body).await
                }
                other => other,
            }
        }
    }
}

fn is_credential_rejection(error: &anyhow::Error) -> bool {
    error
        .downcast_ref::<ProviderError>()
        .and_then(|err| err.status)
        .is_some_and(|status| status == 401 || status == 403)
}

async fn send(
    http: &reqwest::Client,
    label: &str,
    base_url: &str,
    api_key: &str,
    body: &Value,
) -> Result<String> {
    let url = format!("{}/chat/completions", base_url.trim_end_matches('/'));
    let mut headers = HeaderMap::new();
    headers.insert(
        "Authorization",
        crate::shared::header_value(&format!("{label} API key"), &format!("Bearer {api_key}"))?,
    );
    headers.insert(
        "user-agent",
        reqwest::header::HeaderValue::from_static(crate::shared::USER_AGENT),
    );
    crate::shared::log_request("audio-chat", &url);

    let response = http
        .post(&url)
        .headers(headers)
        .json(body)
        .send()
        .await
        .map_err(|e| crate::shared::classify_reqwest_error(&e))?;
    let text = crate::shared::check_response_status("audio-chat", response)
        .await?
        .text()
        .await
        .context("read audio chat response body")?;
    let value: Value = serde_json::from_str(&text).context("parse audio chat response JSON")?;
    parse_answer(&value)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn question(format: AudioFormat) -> AudioQuestion<'static> {
        AudioQuestion {
            model: "model-a",
            prompt: "What is said?",
            audio: b"abc",
            format,
        }
    }

    fn settings(payload: AudioPayload) -> AudioSettings {
        AudioSettings {
            label: "Provider A".to_string(),
            payload,
            formats: vec![AudioFormat::Wav],
            max_base64_bytes: None,
            max_tokens: None,
            text_modalities: false,
        }
    }

    fn input_audio(payload: AudioPayload, format: AudioFormat) -> Value {
        let body = build_request_body(&settings(payload), &question(format));
        body["messages"][0]["content"][0]["input_audio"].clone()
    }

    #[test]
    fn writes_each_input_audio_shape() {
        // base64("abc") == "YWJj"
        assert_eq!(
            input_audio(AudioPayload::DataUri, AudioFormat::Ogg),
            json!({ "data": "data:audio/ogg;base64,YWJj" })
        );
        assert_eq!(
            input_audio(AudioPayload::DataUriWithFormat, AudioFormat::Mp3),
            json!({ "data": "data:audio/mpeg;base64,YWJj", "format": "mp3" })
        );
        assert_eq!(
            input_audio(AudioPayload::Base64WithFormat, AudioFormat::Wav),
            json!({ "data": "YWJj", "format": "wav" })
        );
        assert_eq!(
            input_audio(AudioPayload::Base64String, AudioFormat::Wav),
            json!("YWJj")
        );
    }

    #[test]
    fn body_carries_prompt_and_request_options() {
        let plain = build_request_body(
            &settings(AudioPayload::DataUri),
            &question(AudioFormat::Wav),
        );
        assert_eq!(plain["model"], "model-a");
        assert!(plain.get("max_tokens").is_none());
        assert!(plain.get("modalities").is_none());
        let content = &plain["messages"][0]["content"];
        assert_eq!(content[0]["type"], "input_audio");
        assert_eq!(
            content[1],
            json!({ "type": "text", "text": "What is said?" })
        );

        let mut options = settings(AudioPayload::DataUri);
        options.max_tokens = Some(8000);
        options.text_modalities = true;
        let body = build_request_body(&options, &question(AudioFormat::Wav));
        assert_eq!(body["max_tokens"], 8000);
        assert_eq!(body["modalities"], json!(["text"]));
    }

    #[test]
    fn payload_names_round_trip() {
        for name in AudioPayload::NAMES {
            assert!(AudioPayload::from_name(name).is_some(), "{name}");
        }
        assert_eq!(
            AudioPayload::from_name(" base64 "),
            Some(AudioPayload::Base64String)
        );
        assert_eq!(AudioPayload::from_name("data_uri"), None);
    }

    #[test]
    fn parses_string_and_part_answers() {
        let value = json!({ "choices": [{ "message": { "content": "three tree" } }] });
        assert_eq!(parse_answer(&value).unwrap(), "three tree");

        let value = json!({ "choices": [{ "message": { "content": [
            { "type": "text", "text": "a" }, { "type": "text", "text": "b" }
        ] } }] });
        assert_eq!(parse_answer(&value).unwrap(), "a\nb");
    }

    #[test]
    fn empty_answer_names_finish_reason() {
        let value = json!({ "choices": [{
            "finish_reason": "length",
            "message": { "content": null, "reasoning_content": "..." }
        }] });
        let err = parse_answer(&value).unwrap_err().to_string();
        assert!(err.contains("finish_reason: length"), "{err}");
        assert!(parse_answer(&json!({ "choices": [] })).is_err());
    }

    #[test]
    fn maps_extensions_and_base64_length() {
        assert_eq!(AudioFormat::from_extension("OGA"), Some(AudioFormat::Ogg));
        assert_eq!(AudioFormat::from_extension("opus"), Some(AudioFormat::Ogg));
        assert_eq!(AudioFormat::from_extension("mp4"), None);
        assert_eq!(base64_len(3), 4);
        assert_eq!(base64_len(4), 8);
    }
}
