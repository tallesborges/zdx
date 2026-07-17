use anyhow::{Context, Result, anyhow};
use tokio_util::sync::CancellationToken;

use crate::audio::send_checked;
pub use crate::audio::{OperationCancelled, is_operation_cancelled};
use crate::config::{Config, TranscriptionConfig};
use crate::providers::{ProviderKind, resolve_provider};

/// How a transcription provider authenticates its HTTP request.
#[derive(Debug, Clone, Copy)]
enum SttAuth {
    /// `Authorization: Bearer <key>` (OpenAI-compatible providers, xAI).
    Bearer,
    /// `xi-api-key: <key>` header (`ElevenLabs`).
    XiApiKey,
}

/// Static descriptor for a supported speech-to-text provider.
///
/// This is the single source of truth for STT provider metadata: default model,
/// diarization capability, auth scheme, and auto-detect priority (list order).
/// Wire-format details (endpoint, multipart fields, response shape) stay in the
/// explicit `build_stt_form`/`parse_transcript` matches since they diverge too
/// much to table-drive cleanly.
struct SttProvider {
    kind: ProviderKind,
    default_model: &'static str,
    diarize: bool,
    auth: SttAuth,
}

/// Supported transcription providers, in auto-detect priority order.
const STT_PROVIDERS: &[SttProvider] = &[
    SttProvider {
        kind: ProviderKind::OpenAI,
        default_model: "whisper-1",
        diarize: false,
        auth: SttAuth::Bearer,
    },
    SttProvider {
        kind: ProviderKind::Mistral,
        default_model: "voxtral-mini-latest",
        diarize: true,
        auth: SttAuth::Bearer,
    },
    SttProvider {
        kind: ProviderKind::Xai,
        default_model: "grok-stt",
        diarize: false,
        auth: SttAuth::Bearer,
    },
    SttProvider {
        kind: ProviderKind::ElevenLabs,
        default_model: "scribe_v2",
        diarize: true,
        auth: SttAuth::XiApiKey,
    },
];

fn stt_provider(kind: ProviderKind) -> Option<&'static SttProvider> {
    STT_PROVIDERS.iter().find(|p| p.kind == kind)
}

impl SttProvider {
    /// The `provider:model` selector string for this provider's default model.
    fn selector(&self) -> String {
        format!("{}:{}", self.kind.id(), self.default_model)
    }
}

/// Curated `provider:model` transcription (STT) options — one default per
/// supported provider. Source of truth for the monitor Config picker.
#[must_use]
pub fn transcription_model_options() -> Vec<String> {
    STT_PROVIDERS.iter().map(SttProvider::selector).collect()
}

fn require_stt_provider(kind: ProviderKind) -> Result<&'static SttProvider> {
    stt_provider(kind).ok_or_else(|| {
        anyhow!(
            "Unsupported transcription provider: {}. Only OpenAI, Mistral, xAI, and ElevenLabs are supported.",
            kind.label()
        )
    })
}

/// Returns whether a provider has an API key available (config or env).
fn provider_has_key(config: &Config, kind: ProviderKind) -> bool {
    kind.resolve_api_key(config.providers.get(kind).api_key.as_deref())
        .is_ok()
}

/// Returns whether the given transcription provider id has an API key available.
///
/// Shared definition of "configured" used by both auto-detection and
/// `zdx transcribe --list-models`.
#[must_use]
pub fn is_provider_configured(config: &Config, provider_id: &str) -> bool {
    ProviderKind::from_id(provider_id).is_some_and(|kind| provider_has_key(config, kind))
}

/// A transcription result. `segments` is populated only when diarization is
/// requested and the provider returns speaker-attributed segments.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Transcript {
    pub text: String,
    pub segments: Vec<TranscriptSegment>,
}

/// A speaker-attributed span of a diarized transcript.
#[derive(Debug, Clone, serde::Serialize)]
pub struct TranscriptSegment {
    /// Raw provider speaker id (e.g. `speaker_0`); `None` when unattributed.
    pub speaker: Option<String>,
    /// Segment start time in seconds, if provided.
    pub start: Option<f64>,
    /// Segment end time in seconds, if provided.
    pub end: Option<f64>,
    pub text: String,
}

/// A supported transcription model, for discovery via `zdx transcribe --list-models`.
#[derive(Debug, Clone, serde::Serialize)]
pub struct TranscriptionModel {
    /// Provider id, e.g. `elevenlabs`.
    pub provider: &'static str,
    /// Human label, e.g. `ElevenLabs`.
    pub label: &'static str,
    /// Full `--model` selector, e.g. `elevenlabs:scribe_v2`.
    pub id: String,
    /// Default model id for the provider.
    pub model: &'static str,
    /// Environment variable holding the API key, if any.
    pub api_key_env: Option<&'static str>,
    /// Whether the provider supports `--diarize`.
    pub diarize: bool,
}

/// Returns the transcription models zdx supports, in auto-detect priority order.
#[must_use]
pub fn supported_models() -> Vec<TranscriptionModel> {
    STT_PROVIDERS
        .iter()
        .map(|p| TranscriptionModel {
            provider: p.kind.id(),
            label: p.kind.label(),
            id: p.selector(),
            model: p.default_model,
            api_key_env: p.kind.api_key_env_var(),
            diarize: p.diarize,
        })
        .collect()
}

#[derive(serde::Deserialize)]
struct TextOnlyResponse {
    text: String,
}

/// `ElevenLabs` Scribe response with word-level diarization.
#[derive(serde::Deserialize)]
struct ElevenLabsResponse {
    text: String,
    #[serde(default)]
    words: Vec<ElevenLabsWord>,
}

#[derive(serde::Deserialize)]
struct ElevenLabsWord {
    #[serde(default)]
    text: String,
    speaker_id: Option<String>,
    start: Option<f64>,
    end: Option<f64>,
}

/// Mistral Voxtral response with segment-level diarization.
#[derive(serde::Deserialize)]
struct VoxtralResponse {
    text: String,
    #[serde(default)]
    segments: Vec<VoxtralSegment>,
}

#[derive(serde::Deserialize)]
struct VoxtralSegment {
    speaker: Option<String>,
    start: Option<f64>,
    end: Option<f64>,
    #[serde(default)]
    text: String,
}

/// Transcribes audio if a supported provider is configured.
///
/// Returns `Ok(None)` if no transcription provider is available.
///
/// # Errors
/// Returns an error if the operation fails.
pub async fn transcribe_audio_if_configured(
    config: &Config,
    transcription: &TranscriptionConfig,
    bytes: Vec<u8>,
    filename: &str,
    mime_type: Option<&str>,
    cancel_token: Option<&CancellationToken>,
) -> Result<Option<String>> {
    let result = transcribe_audio_detailed(
        config,
        transcription,
        bytes,
        filename,
        mime_type,
        false,
        cancel_token,
    )
    .await?;
    Ok(result.map(|transcript| transcript.text))
}

/// Transcribes audio, optionally with speaker diarization, returning the full
/// [`Transcript`] (text plus speaker segments).
///
/// Returns `Ok(None)` if no transcription provider is available.
///
/// # Errors
/// Returns an error if the operation fails, or if `diarize` is requested for a
/// provider that does not support it.
pub async fn transcribe_audio_detailed(
    config: &Config,
    transcription: &TranscriptionConfig,
    bytes: Vec<u8>,
    filename: &str,
    mime_type: Option<&str>,
    diarize: bool,
    cancel_token: Option<&CancellationToken>,
) -> Result<Option<Transcript>> {
    let Some((provider, model)) = resolve_model(config, transcription)? else {
        return Ok(None);
    };
    let descriptor = require_stt_provider(provider)?;

    if diarize && !descriptor.diarize {
        return Err(anyhow!(
            "Diarization is not supported by {}. Only Mistral (Voxtral) and ElevenLabs support --diarize.",
            provider.label()
        ));
    }

    let provider_config = config.providers.get(provider);
    let api_key = provider.resolve_api_key(provider_config.api_key.as_deref())?;
    let base_url = provider.resolve_base_url(provider_config.base_url.as_deref())?;

    let language = transcription
        .language
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());

    let transcript = transcribe_audio(TranscriptionRequest {
        provider,
        provider_name: provider.label(),
        auth: descriptor.auth,
        base_url: &base_url,
        api_key: &api_key,
        model: &model,
        bytes,
        filename,
        mime_type,
        language,
        diarize,
        cancel_token,
    })
    .await?;

    if transcript.text.trim().is_empty() && transcript.segments.is_empty() {
        Ok(None)
    } else {
        Ok(Some(transcript))
    }
}

/// Resolves the transcription provider and model.
///
/// Priority: `ZDX_TRANSCRIPTION_MODEL` env var > config model > auto-detect.
/// A model string may be a full `provider:model` id, or a bare provider
/// name/alias (e.g. `elevenlabs`) which selects that provider's default model.
/// Returns `Ok(None)` if no provider is available.
fn resolve_model(
    config: &Config,
    transcription: &TranscriptionConfig,
) -> Result<Option<(ProviderKind, String)>> {
    let model_str = std::env::var("ZDX_TRANSCRIPTION_MODEL")
        .ok()
        .or_else(|| transcription.model.clone())
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty());

    if let Some(model_str) = model_str {
        // A bare provider name/alias selects that provider's default model.
        if let Some(provider) = ProviderKind::from_id(&model_str) {
            let descriptor = require_stt_provider(provider)?;
            return Ok(Some((provider, descriptor.default_model.to_string())));
        }
        let selection = resolve_provider(&model_str);
        require_stt_provider(selection.kind)?;
        return Ok(Some((selection.kind, selection.model)));
    }

    Ok(STT_PROVIDERS.iter().find_map(|p| {
        provider_has_key(config, p.kind).then(|| (p.kind, p.default_model.to_string()))
    }))
}

struct TranscriptionRequest<'a> {
    provider: ProviderKind,
    provider_name: &'a str,
    auth: SttAuth,
    base_url: &'a str,
    api_key: &'a str,
    model: &'a str,
    bytes: Vec<u8>,
    filename: &'a str,
    mime_type: Option<&'a str>,
    language: Option<&'a str>,
    diarize: bool,
    cancel_token: Option<&'a CancellationToken>,
}

async fn transcribe_audio(request: TranscriptionRequest<'_>) -> Result<Transcript> {
    let TranscriptionRequest {
        provider,
        provider_name,
        auth,
        base_url,
        api_key,
        model,
        bytes,
        filename,
        mime_type,
        language,
        diarize,
        cancel_token,
    } = request;

    let client = reqwest::Client::new();
    let mut part = reqwest::multipart::Part::bytes(bytes).file_name(filename.to_string());
    if let Some(mime) = mime_type
        && !mime.trim().is_empty()
    {
        part = part.mime_str(mime)?;
    }

    let (url, form) = build_stt_form(provider, base_url, model, language, diarize, part);

    let post = client.post(&url);
    let post = match auth {
        SttAuth::Bearer => post.bearer_auth(api_key),
        SttAuth::XiApiKey => post.header("xi-api-key", api_key),
    };

    let context =
        format!("{provider_name} transcription (url={url}, model={model}, filename={filename})");
    let response = send_checked(post.multipart(form), cancel_token, &context).await?;

    let body = response.text().await.with_context(|| {
        format!("read {provider_name} transcription response (model={model}, filename={filename})")
    })?;

    parse_transcript(provider, diarize, &body).with_context(|| {
        format!(
            "decode {provider_name} transcription response (model={model}, filename={filename})"
        )
    })
}

/// Builds the endpoint URL and multipart form for a provider's STT request.
///
/// xAI Grok STT uses a distinct `/stt` endpoint that requires the `file` field
/// last and gates `format=true` behind a language hint. `ElevenLabs` Scribe uses
/// `/v1/speech-to-text` with a `model_id` field; diarization is opt-in via
/// `diarize=true`. Mistral Voxtral needs segment timestamps enabled alongside
/// `diarize`. OpenAI/Mistral otherwise use the OpenAI-compatible
/// `/audio/transcriptions` shape.
fn build_stt_form(
    provider: ProviderKind,
    base_url: &str,
    model: &str,
    language: Option<&str>,
    diarize: bool,
    part: reqwest::multipart::Part,
) -> (String, reqwest::multipart::Form) {
    let base = base_url.trim_end_matches('/');
    match provider {
        ProviderKind::Xai => {
            let mut form = reqwest::multipart::Form::new().text("model", model.to_string());
            if let Some(lang) = language {
                form = form
                    .text("language", lang.to_string())
                    .text("format", "true");
            }
            form = form.part("file", part);
            (format!("{base}/stt"), form)
        }
        ProviderKind::ElevenLabs => {
            let mut form = reqwest::multipart::Form::new()
                .text("model_id", model.to_string())
                .part("file", part);
            if diarize {
                form = form.text("diarize", "true");
            }
            if let Some(lang) = language {
                form = form.text("language_code", lang.to_string());
            }
            (format!("{base}/v1/speech-to-text"), form)
        }
        _ => {
            let mut form = reqwest::multipart::Form::new()
                .text("model", model.to_string())
                .part("file", part);
            if let Some(lang) = language {
                form = form.text("language", lang.to_string());
            }
            if diarize {
                // Voxtral requires segment-level timestamps when diarizing.
                form = form
                    .text("diarize", "true")
                    .text("timestamp_granularities[]", "segment");
            }
            (format!("{base}/audio/transcriptions"), form)
        }
    }
}

/// Parses a provider response body into a [`Transcript`], extracting diarized
/// segments when available.
fn parse_transcript(provider: ProviderKind, diarize: bool, body: &str) -> Result<Transcript> {
    if !diarize {
        let payload: TextOnlyResponse = serde_json::from_str(body)?;
        return Ok(Transcript {
            text: payload.text,
            segments: Vec::new(),
        });
    }

    match provider {
        ProviderKind::ElevenLabs => {
            let payload: ElevenLabsResponse = serde_json::from_str(body)?;
            Ok(Transcript {
                text: payload.text,
                segments: group_elevenlabs_words(payload.words),
            })
        }
        ProviderKind::Mistral => {
            let payload: VoxtralResponse = serde_json::from_str(body)?;
            let segments = payload
                .segments
                .into_iter()
                .map(|s| TranscriptSegment {
                    speaker: s.speaker,
                    start: s.start,
                    end: s.end,
                    text: s.text.trim().to_string(),
                })
                .filter(|s| !s.text.is_empty())
                .collect();
            Ok(Transcript {
                text: payload.text,
                segments,
            })
        }
        _ => {
            let payload: TextOnlyResponse = serde_json::from_str(body)?;
            Ok(Transcript {
                text: payload.text,
                segments: Vec::new(),
            })
        }
    }
}

/// Collapses `ElevenLabs` word-level output into contiguous speaker segments.
fn group_elevenlabs_words(words: Vec<ElevenLabsWord>) -> Vec<TranscriptSegment> {
    let mut segments: Vec<TranscriptSegment> = Vec::new();
    for word in words {
        let speaker = word.speaker_id.clone();
        match segments.last_mut() {
            Some(seg) if seg.speaker == speaker => {
                seg.text.push_str(&word.text);
                if word.end.is_some() {
                    seg.end = word.end;
                }
            }
            _ => segments.push(TranscriptSegment {
                speaker,
                start: word.start,
                end: word.end,
                text: word.text,
            }),
        }
    }
    for seg in &mut segments {
        seg.text = seg.text.trim().to_string();
    }
    segments.retain(|seg| !seg.text.is_empty());
    segments
}
