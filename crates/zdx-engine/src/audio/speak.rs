use anyhow::{Context, Result, anyhow};
use base64::Engine;
use tokio_util::sync::CancellationToken;

use crate::audio::send_checked;
use crate::config::{Config, SpeechConfig};
use crate::providers::{ProviderKind, resolve_provider};

const DEFAULT_OPENAI_MODEL: &str = "gpt-4o-mini-tts";
const DEFAULT_MISTRAL_MODEL: &str = "voxtral-mini-tts-latest";
const DEFAULT_GEMINI_MODEL: &str = "gemini-3.1-flash-tts-preview";
const DEFAULT_XAI_MODEL: &str = "grok-tts";
const DEFAULT_OPENAI_VOICE: &str = "coral";
const DEFAULT_MISTRAL_VOICE: &str = "en_paul_neutral";
const DEFAULT_GEMINI_VOICE: &str = "Kore";
const DEFAULT_XAI_VOICE: &str = "eve";
const DEFAULT_XAI_LANGUAGE: &str = "en";
const OGG_FORMAT: &str = "ogg";
const DEFAULT_FORMAT: &str = OGG_FORMAT;
const FFMPEG_BIN: &str = "ffmpeg";

/// Gemini TTS returns raw PCM; the generateContent path is mono 16-bit signed LE.
const GEMINI_PCM_DEFAULT_RATE: u32 = 24000;

/// Supported speech-synthesis providers, in auto-detect priority order
/// (Mistral/Voxtral is the default when its key is available; Gemini and xAI are
/// opt-in via `--provider` so they never override the Mistral default).
const SPEECH_PROVIDERS: &[ProviderKind] = &[
    ProviderKind::Mistral,
    ProviderKind::OpenAI,
    ProviderKind::Gemini,
    ProviderKind::Xai,
];

/// Maximum input length (characters) accepted for a single synthesis request.
const MAX_INPUT_CHARS: usize = 4096;

/// Synthesized speech audio with format metadata.
#[derive(Debug)]
pub struct SpeechAudio {
    pub bytes: Vec<u8>,
    pub mime: String,
    pub ext: String,
}

/// Synthesizes speech audio from text using the configured provider.
///
/// # Errors
/// Returns an error if the text is empty/oversized, no provider is configured,
/// or the provider request fails.
pub async fn synthesize_speech(
    config: &Config,
    speech: &SpeechConfig,
    text: &str,
    cancel_token: Option<&CancellationToken>,
) -> Result<SpeechAudio> {
    let input = text.trim();
    if input.is_empty() {
        return Err(anyhow!("Cannot synthesize speech from empty text"));
    }
    let char_count = input.chars().count();
    if char_count > MAX_INPUT_CHARS {
        return Err(anyhow!(
            "Speech input too long: {char_count} characters (max {MAX_INPUT_CHARS})"
        ));
    }

    let (provider, model) = resolve_model(config, speech)?.ok_or_else(|| {
        anyhow!(
            "No speech provider configured. Set MISTRAL_API_KEY, OPENAI_API_KEY, GEMINI_API_KEY, or XAI_API_KEY, or configure [speech] in config.toml."
        )
    })?;

    let provider_config = config.providers.get(provider);
    let api_key = provider.resolve_api_key(provider_config.api_key.as_deref())?;
    let base_url = provider.resolve_base_url(provider_config.base_url.as_deref())?;

    let voice = speech
        .voice
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| default_voice(provider));

    let output_format = speech
        .format
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(DEFAULT_FORMAT);
    let want_ogg = output_format.eq_ignore_ascii_case(OGG_FORMAT);

    // Gemini uses a different API shape (generateContent → raw PCM), so it has
    // its own request path and produces WAV. OpenAI/Mistral share `/audio/speech`
    // and return MP3 (which we transcode to OGG for Telegram voice notes).
    let audio = if provider == ProviderKind::Gemini {
        synthesize_gemini(&base_url, &api_key, &model, input, voice, cancel_token).await?
    } else {
        let api_format = if want_ogg { "mp3" } else { output_format };
        synthesize(SpeechRequest {
            provider,
            base_url: &base_url,
            api_key: &api_key,
            model: &model,
            input,
            voice,
            format: api_format,
            cancel_token,
        })
        .await?
    };

    if !want_ogg {
        return Ok(audio);
    }

    if let Some(bytes) = transcode_to_ogg_opus(&audio.bytes).await? {
        Ok(SpeechAudio {
            bytes,
            mime: "audio/ogg".to_string(),
            ext: OGG_FORMAT.to_string(),
        })
    } else {
        tracing::warn!("ffmpeg not found on PATH; returning MP3 instead of an OGG/Opus voice note");
        Ok(audio)
    }
}

/// Transcodes MP3 or WAV audio bytes to Telegram-ready OGG/Opus via ffmpeg.
///
/// ffmpeg auto-probes the input container, so this accepts both the MP3 that
/// OpenAI/Mistral return and the WAV we wrap around Gemini's raw PCM.
/// Returns `Ok(None)` when ffmpeg is not available on `PATH` so callers can fall
/// back to the original audio.
async fn transcode_to_ogg_opus(audio: &[u8]) -> Result<Option<Vec<u8>>> {
    transcode_to_ogg_opus_with(FFMPEG_BIN, audio).await
}

async fn transcode_to_ogg_opus_with(ffmpeg_bin: &str, audio: &[u8]) -> Result<Option<Vec<u8>>> {
    let dir = tempfile::tempdir().context("create temp dir for audio transcode")?;
    let input = dir.path().join("in.audio");
    let output = dir.path().join("out.ogg");
    std::fs::write(&input, audio).context("write temp audio for transcode")?;

    let result = tokio::process::Command::new(ffmpeg_bin)
        .args(["-hide_banner", "-loglevel", "error", "-y", "-i"])
        .arg(&input)
        .args([
            "-c:a",
            "libopus",
            "-b:a",
            "32k",
            "-ar",
            "48000",
            "-ac",
            "1",
            "-application",
            "voip",
            "-f",
            "ogg",
        ])
        .arg(&output)
        .output()
        .await;

    let completed = match result {
        Ok(completed) => completed,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => {
            return Err(anyhow::Error::new(err).context("spawn ffmpeg for audio transcode"));
        }
    };

    if !completed.status.success() {
        return Err(anyhow!(
            "ffmpeg transcode failed: {}",
            String::from_utf8_lossy(&completed.stderr).trim()
        ));
    }

    let bytes = std::fs::read(&output).context("read transcoded ogg")?;
    Ok(Some(bytes))
}

/// Resolves the speech provider and model.
///
/// Priority: `ZDX_SPEECH_MODEL` env var > explicit config model > explicit provider > auto-detect.
/// Returns `Ok(None)` if no provider is available.
fn resolve_model(config: &Config, speech: &SpeechConfig) -> Result<Option<(ProviderKind, String)>> {
    let model_str = std::env::var("ZDX_SPEECH_MODEL")
        .ok()
        .or_else(|| speech.model.clone())
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty());

    if let Some(model_str) = model_str {
        let selection = resolve_provider(&model_str);
        if !SPEECH_PROVIDERS.contains(&selection.kind) {
            return Err(anyhow!(
                "Unsupported speech provider: {}. Only OpenAI, Mistral, Gemini, and xAI are supported.",
                selection.kind.label()
            ));
        }
        return Ok(Some((selection.kind, selection.model)));
    }

    if let Some(provider_str) = speech
        .provider
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        let provider = parse_provider(provider_str)?;
        return Ok(Some((provider, default_model(provider).to_string())));
    }

    Ok(SPEECH_PROVIDERS.iter().find_map(|&provider| {
        let provider_config = config.providers.get(provider);
        provider
            .resolve_api_key(provider_config.api_key.as_deref())
            .ok()
            .map(|_| (provider, default_model(provider).to_string()))
    }))
}

fn parse_provider(value: &str) -> Result<ProviderKind> {
    match value.to_ascii_lowercase().as_str() {
        "openai" => Ok(ProviderKind::OpenAI),
        "mistral" => Ok(ProviderKind::Mistral),
        "gemini" => Ok(ProviderKind::Gemini),
        "xai" | "grok" | "x" => Ok(ProviderKind::Xai),
        other => Err(anyhow!(
            "Unsupported speech provider: {other}. Only OpenAI, Mistral, Gemini, and xAI are supported."
        )),
    }
}

fn default_model(provider: ProviderKind) -> &'static str {
    match provider {
        ProviderKind::Mistral => DEFAULT_MISTRAL_MODEL,
        ProviderKind::Gemini => DEFAULT_GEMINI_MODEL,
        ProviderKind::Xai => DEFAULT_XAI_MODEL,
        _ => DEFAULT_OPENAI_MODEL,
    }
}

fn default_voice(provider: ProviderKind) -> &'static str {
    match provider {
        ProviderKind::Mistral => DEFAULT_MISTRAL_VOICE,
        ProviderKind::Gemini => DEFAULT_GEMINI_VOICE,
        ProviderKind::Xai => DEFAULT_XAI_VOICE,
        _ => DEFAULT_OPENAI_VOICE,
    }
}

#[derive(serde::Deserialize)]
struct MistralSpeechResponse {
    audio_data: String,
}

fn format_metadata(format: &str) -> (String, String) {
    let (mime, ext) = match format.to_ascii_lowercase().as_str() {
        "ogg" => ("audio/ogg", "ogg"),
        "opus" => ("audio/opus", "opus"),
        "aac" => ("audio/aac", "aac"),
        "flac" => ("audio/flac", "flac"),
        "wav" => ("audio/wav", "wav"),
        "pcm" => ("audio/pcm", "pcm"),
        _ => ("audio/mpeg", "mp3"),
    };
    (mime.to_string(), ext.to_string())
}

struct SpeechRequest<'a> {
    provider: ProviderKind,
    base_url: &'a str,
    api_key: &'a str,
    model: &'a str,
    input: &'a str,
    voice: &'a str,
    format: &'a str,
    cancel_token: Option<&'a CancellationToken>,
}

async fn synthesize(request: SpeechRequest<'_>) -> Result<SpeechAudio> {
    let SpeechRequest {
        provider,
        base_url,
        api_key,
        model,
        input,
        voice,
        format,
        cancel_token,
    } = request;
    let provider_name = provider.label();

    let client = reqwest::Client::new();
    // Providers diverge on request shape:
    // - OpenAI: POST /audio/speech {model, input, voice}         → raw audio bytes
    // - Mistral: POST /audio/speech {model, input, voice_id}     → JSON base64 audio_data
    // - xAI: POST /tts {text, voice_id, language}                → raw MP3 bytes
    let (url, body) = if provider == ProviderKind::Xai {
        (
            format!("{}/tts", base_url.trim_end_matches('/')),
            serde_json::json!({
                "text": input,
                "voice_id": voice,
                "language": DEFAULT_XAI_LANGUAGE,
            }),
        )
    } else {
        let voice_key = if provider == ProviderKind::Mistral {
            "voice_id"
        } else {
            "voice"
        };
        let mut body = serde_json::json!({
            "model": model,
            "input": input,
            "response_format": format,
        });
        body[voice_key] = serde_json::Value::String(voice.to_string());
        (
            format!("{}/audio/speech", base_url.trim_end_matches('/')),
            body,
        )
    };

    let send = client.post(&url).bearer_auth(api_key).json(&body);
    let context = format!("{provider_name} speech (url={url}, model={model})");
    let response = send_checked(send, cancel_token, &context).await?;

    let raw = response
        .bytes()
        .await
        .with_context(|| format!("read {provider_name} speech response (model={model})"))?;

    let bytes = if provider == ProviderKind::Mistral {
        decode_mistral_audio(&raw)?
    } else {
        raw.to_vec()
    };

    if bytes.is_empty() {
        return Err(anyhow!(
            "{provider_name} speech synthesis returned no audio data"
        ));
    }

    let (mime, ext) = format_metadata(format);
    Ok(SpeechAudio { bytes, mime, ext })
}

/// Decodes Mistral's `{ "audio_data": "<base64>" }` speech response into audio bytes.
fn decode_mistral_audio(body: &[u8]) -> Result<Vec<u8>> {
    let payload: MistralSpeechResponse =
        serde_json::from_slice(body).context("decode Mistral speech response JSON")?;
    base64::engine::general_purpose::STANDARD
        .decode(payload.audio_data.trim())
        .context("decode Mistral base64 audio data")
}

/// Synthesizes speech with Gemini TTS via `generateContent` (returns WAV).
///
/// Gemini returns raw PCM (mono 16-bit LE, 24kHz) as inline base64, which we
/// wrap in a WAV header. The default OGG path then transcodes the WAV via ffmpeg.
async fn synthesize_gemini(
    base_url: &str,
    api_key: &str,
    model: &str,
    input: &str,
    voice: &str,
    cancel_token: Option<&CancellationToken>,
) -> Result<SpeechAudio> {
    let client = reqwest::Client::new();
    let body = serde_json::json!({
        "contents": [{ "role": "user", "parts": [{ "text": input }] }],
        "generationConfig": {
            "responseModalities": ["AUDIO"],
            "speechConfig": {
                "voiceConfig": { "prebuiltVoiceConfig": { "voiceName": voice } }
            }
        }
    });

    let url = format!(
        "{}/models/{}:generateContent",
        base_url.trim_end_matches('/'),
        model
    );
    let send = client
        .post(&url)
        .header("x-goog-api-key", api_key)
        .json(&body);
    let context = format!("Gemini speech (url={url}, model={model})");
    let response = send_checked(send, cancel_token, &context).await?;

    let value: serde_json::Value = response
        .json()
        .await
        .with_context(|| format!("decode Gemini speech response JSON (model={model})"))?;

    let (mime, pcm) = extract_gemini_audio(&value)?;
    let sample_rate = parse_pcm_rate(&mime).unwrap_or(GEMINI_PCM_DEFAULT_RATE);
    let wav = pcm_to_wav(&pcm, sample_rate, 1);

    Ok(SpeechAudio {
        bytes: wav,
        mime: "audio/wav".to_string(),
        ext: "wav".to_string(),
    })
}

/// Extracts `(mimeType, decoded PCM bytes)` from a Gemini `generateContent` audio response.
fn extract_gemini_audio(value: &serde_json::Value) -> Result<(String, Vec<u8>)> {
    let candidates = value
        .get("candidates")
        .and_then(serde_json::Value::as_array)
        .filter(|c| !c.is_empty())
        .ok_or_else(|| anyhow!("Gemini speech response has no candidates"))?;

    for candidate in candidates {
        let parts = candidate
            .get("content")
            .and_then(|content| content.get("parts"))
            .and_then(serde_json::Value::as_array);
        let Some(parts) = parts else { continue };

        for part in parts {
            let Some(inline) = part.get("inlineData").or_else(|| part.get("inline_data")) else {
                continue;
            };
            let mime = inline
                .get("mimeType")
                .or_else(|| inline.get("mime_type"))
                .and_then(serde_json::Value::as_str)
                .unwrap_or("audio/L16;rate=24000")
                .to_string();
            let data_b64 = inline
                .get("data")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| anyhow!("Gemini audio part is missing inlineData.data"))?;
            let pcm = base64::engine::general_purpose::STANDARD
                .decode(data_b64.trim())
                .context("decode Gemini base64 PCM audio")?;
            return Ok((mime, pcm));
        }
    }

    Err(anyhow!("Gemini speech response contained no audio data"))
}

/// Parses the sample rate from a Gemini audio mime type like `audio/L16;rate=24000`.
fn parse_pcm_rate(mime: &str) -> Option<u32> {
    mime.split(';')
        .filter_map(|part| part.trim().strip_prefix("rate="))
        .find_map(|rate| rate.trim().parse::<u32>().ok())
}

/// Wraps raw signed 16-bit little-endian PCM in a minimal WAV container.
fn pcm_to_wav(pcm: &[u8], sample_rate: u32, channels: u16) -> Vec<u8> {
    let bits_per_sample: u16 = 16;
    let block_align = channels * bits_per_sample / 8;
    let byte_rate = sample_rate * u32::from(block_align);
    let data_len = u32::try_from(pcm.len()).unwrap_or(u32::MAX);

    let mut wav = Vec::with_capacity(44 + pcm.len());
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&(36 + data_len).to_le_bytes());
    wav.extend_from_slice(b"WAVE");
    wav.extend_from_slice(b"fmt ");
    wav.extend_from_slice(&16u32.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes()); // PCM
    wav.extend_from_slice(&channels.to_le_bytes());
    wav.extend_from_slice(&sample_rate.to_le_bytes());
    wav.extend_from_slice(&byte_rate.to_le_bytes());
    wav.extend_from_slice(&block_align.to_le_bytes());
    wav.extend_from_slice(&bits_per_sample.to_le_bytes());
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&data_len.to_le_bytes());
    wav.extend_from_slice(pcm);
    wav
}

#[cfg(test)]
mod tests {
    use super::*;

    // These tests exercise only the pure resolution/guard branches that return
    // before the env-var auto-detect path, so they stay deterministic.

    #[test]
    fn resolve_model_prefers_explicit_prefixed_config_model() {
        let config = Config::default();
        let speech = SpeechConfig {
            model: Some("openai:gpt-4o-mini-tts".to_string()),
            ..SpeechConfig::default()
        };
        let resolved = resolve_model(&config, &speech).unwrap();
        assert_eq!(
            resolved,
            Some((ProviderKind::OpenAI, "gpt-4o-mini-tts".to_string()))
        );
    }

    #[test]
    fn resolve_model_uses_provider_field_default_model() {
        let config = Config::default();
        let speech = SpeechConfig {
            provider: Some("mistral".to_string()),
            ..SpeechConfig::default()
        };
        let resolved = resolve_model(&config, &speech).unwrap();
        assert_eq!(
            resolved,
            Some((ProviderKind::Mistral, DEFAULT_MISTRAL_MODEL.to_string()))
        );
    }

    #[test]
    fn resolve_model_rejects_unsupported_provider() {
        let config = Config::default();
        let speech = SpeechConfig {
            model: Some("anthropic:claude".to_string()),
            ..SpeechConfig::default()
        };
        let err = resolve_model(&config, &speech).unwrap_err();
        assert!(err.to_string().contains("Unsupported speech provider"));
    }

    #[tokio::test]
    async fn synthesize_rejects_empty_text() {
        let config = Config::default();
        let speech = SpeechConfig::default();
        let err = synthesize_speech(&config, &speech, "   ", None)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("empty text"));
    }

    #[tokio::test]
    async fn synthesize_rejects_oversized_text() {
        let config = Config::default();
        let speech = SpeechConfig::default();
        let text = "a".repeat(MAX_INPUT_CHARS + 1);
        let err = synthesize_speech(&config, &speech, &text, None)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("too long"));
    }

    #[test]
    fn format_metadata_defaults_to_mp3() {
        assert_eq!(
            format_metadata("mp3"),
            ("audio/mpeg".to_string(), "mp3".to_string())
        );
        assert_eq!(
            format_metadata("unknown"),
            ("audio/mpeg".to_string(), "mp3".to_string())
        );
        assert_eq!(
            format_metadata("opus"),
            ("audio/opus".to_string(), "opus".to_string())
        );
        assert_eq!(
            format_metadata("ogg"),
            ("audio/ogg".to_string(), "ogg".to_string())
        );
    }

    #[tokio::test]
    async fn transcode_returns_none_when_ffmpeg_missing() {
        let result =
            transcode_to_ogg_opus_with("zdx-nonexistent-ffmpeg-binary-xyz", b"not-real-audio")
                .await
                .unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn default_model_and_voice_cover_gemini() {
        assert_eq!(
            default_model(ProviderKind::Gemini),
            "gemini-3.1-flash-tts-preview"
        );
        assert_eq!(default_voice(ProviderKind::Gemini), "Kore");
    }

    #[test]
    fn default_model_and_voice_cover_xai() {
        assert_eq!(default_model(ProviderKind::Xai), "grok-tts");
        assert_eq!(default_voice(ProviderKind::Xai), "eve");
    }

    #[test]
    fn parse_provider_accepts_xai_aliases() {
        assert_eq!(parse_provider("xai").unwrap(), ProviderKind::Xai);
        assert_eq!(parse_provider("grok").unwrap(), ProviderKind::Xai);
        assert_eq!(parse_provider("Gemini").unwrap(), ProviderKind::Gemini);
        assert!(parse_provider("anthropic").is_err());
    }

    #[test]
    fn resolve_model_uses_gemini_provider_field() {
        let config = Config::default();
        let speech = SpeechConfig {
            provider: Some("gemini".to_string()),
            ..SpeechConfig::default()
        };
        let resolved = resolve_model(&config, &speech).unwrap();
        assert_eq!(
            resolved,
            Some((
                ProviderKind::Gemini,
                "gemini-3.1-flash-tts-preview".to_string()
            ))
        );
    }

    #[test]
    fn parse_pcm_rate_reads_rate_from_mime() {
        assert_eq!(parse_pcm_rate("audio/L16;rate=24000"), Some(24000));
        assert_eq!(parse_pcm_rate("audio/pcm; rate=16000"), Some(16000));
        assert_eq!(parse_pcm_rate("audio/L16"), None);
    }

    #[test]
    fn pcm_to_wav_writes_valid_header() {
        let pcm = vec![1u8, 2, 3, 4];
        let wav = pcm_to_wav(&pcm, 24000, 1);
        assert_eq!(&wav[0..4], b"RIFF");
        assert_eq!(&wav[8..12], b"WAVE");
        assert_eq!(&wav[36..40], b"data");
        assert_eq!(wav.len(), 44 + pcm.len());
        // sample rate at offset 24 (LE)
        assert_eq!(
            u32::from_le_bytes([wav[24], wav[25], wav[26], wav[27]]),
            24000
        );
        assert_eq!(&wav[44..], pcm.as_slice());
    }

    #[test]
    fn extract_gemini_audio_decodes_inline_pcm() {
        let b64 = base64::engine::general_purpose::STANDARD.encode(b"PCMDATA");
        let value = serde_json::json!({
            "candidates": [{
                "content": { "parts": [{
                    "inlineData": { "mimeType": "audio/L16;rate=24000", "data": b64 }
                }]}
            }]
        });
        let (mime, pcm) = extract_gemini_audio(&value).unwrap();
        assert_eq!(mime, "audio/L16;rate=24000");
        assert_eq!(pcm, b"PCMDATA");
    }

    #[test]
    fn extract_gemini_audio_errors_without_audio() {
        let value = serde_json::json!({
            "candidates": [{ "content": { "parts": [{ "text": "no audio here" }] } }]
        });
        assert!(extract_gemini_audio(&value).is_err());
    }

    #[test]
    fn decode_mistral_audio_decodes_base64_payload() {
        let encoded = base64::engine::general_purpose::STANDARD.encode(b"ID3-fake-mp3-bytes");
        let body = format!(r#"{{"audio_data":"{encoded}"}}"#);
        let decoded = decode_mistral_audio(body.as_bytes()).unwrap();
        assert_eq!(decoded, b"ID3-fake-mp3-bytes");
    }

    #[test]
    fn decode_mistral_audio_errors_on_invalid_base64() {
        let body = r#"{"audio_data":"!!!not-base64!!!"}"#;
        assert!(decode_mistral_audio(body.as_bytes()).is_err());
    }

    #[test]
    fn default_voice_is_provider_specific() {
        assert_eq!(default_voice(ProviderKind::Mistral), "en_paul_neutral");
        assert_eq!(default_voice(ProviderKind::OpenAI), "coral");
    }
}
