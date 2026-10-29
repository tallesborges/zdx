//! One-shot media understanding: send a local audio/video/PDF/image file plus a
//! prompt to a model and return the model's text answer.
//!
//! Gemini models take every supported media type. Other providers take audio
//! only, over Chat Completions with an `input_audio` part, when both hold:
//! - the provider has audio settings: a built-in provider listed in the
//!   embedded `audio_providers.toml` (fields overridable in
//!   `[providers.<name>.audio]`), or a custom provider with
//!   `[providers.custom.<name>.audio]`;
//! - the model has `input_audio = true` in `model_overrides.toml` (see
//!   [`crate::models::model_audio_input`]).
//!
//! No provider has its own code path here. Built-in providers differ only in
//! how their credential is resolved (API key, or Muse Code's minted key).
//! [`crate::providers::audio_chat`] sends the request.
//!
//! This is a stateless helper (the file is re-sent on every call). It is the
//! shared core behind `zdx ask-media` and the `ask_media` tool.

use std::fs;
use std::path::Path;

use anyhow::{Context, Result, anyhow, bail};

use crate::config::{
    Config, CustomProviderApi, CustomProviderConfig, ProviderAudioConfig, effective_provider_audio,
    provider_config_section, shipped_provider_audio,
};
use crate::models::{
    AudioInput, ModelSpec, declared_audio_models, model_audio_input, user_model_api_override,
};
use crate::providers::audio_chat::{
    self, AudioAuth, AudioFormat, AudioPayload, AudioQuestion, AudioRoute, AudioSettings,
    base64_len,
};
use crate::providers::gemini::{GeminiClient, GeminiConfig};
use crate::providers::{ProviderKind, ProviderSelection, resolve_provider};

/// Default model: fast, cheap, built for document parsing.
pub const DEFAULT_ASK_MEDIA_MODEL: &str = "gemini:gemini-3.5-flash-lite";

/// Max raw file size sent inline. Base64 inflates ~33%, so keep well under
/// Gemini's ~20 MB total inline request cap. Larger files need the File API
/// (not yet supported). Audio converted for another provider must also fit.
pub const MAX_INLINE_BYTES: usize = 15 * 1024 * 1024;

const FFMPEG_BIN: &str = "ffmpeg";

/// Reads `path`, sends it inline with `prompt` to the model `model_input`, and
/// returns the model's text answer.
///
/// # Errors
/// Returns an error if the provider or model has no media route, its audio
/// settings are invalid, the file is missing/empty/too large, its type is
/// unsupported for that model, audio conversion fails, credentials are
/// missing, or the request fails.
pub async fn ask_media(
    path: &Path,
    prompt: &str,
    model_input: &str,
    config: &Config,
) -> Result<String> {
    // Same precedence as chat turns: a configured custom provider name wins.
    let base = ModelSpec::parse(model_input).base;
    if let Some((name, custom, model)) = config.providers.named_custom_provider_for_model(base) {
        let settings = custom_settings(name, custom, &model)?;
        let endpoint = Endpoint::Custom { name, cfg: custom };
        return ask_declared_audio(path, prompt, name, &model, settings, &endpoint).await;
    }

    let selection = resolve_provider(model_input);
    if selection.kind == ProviderKind::Gemini {
        return ask_gemini(path, prompt, &selection, config).await;
    }

    let provider_id = selection.kind.id();
    let settings = builtin_settings(selection.kind, &selection.model, config, model_input)?;
    let provider = config.providers.get(selection.kind);
    let endpoint = Endpoint::BuiltIn {
        kind: selection.kind,
        account: selection.account.as_deref(),
        config_base_url: provider.effective_base_url(),
        config_api_key: provider.effective_api_key(),
    };
    ask_declared_audio(
        path,
        prompt,
        provider_id,
        &selection.model,
        settings,
        &endpoint,
    )
    .await
}

/// Where the request goes and how its credential is resolved.
enum Endpoint<'a> {
    BuiltIn {
        kind: ProviderKind,
        account: Option<&'a str>,
        config_base_url: Option<&'a str>,
        config_api_key: Option<&'a str>,
    },
    Custom {
        name: &'a str,
        cfg: &'a CustomProviderConfig,
    },
}

impl<'a> Endpoint<'a> {
    fn resolve(&self) -> Result<(String, AudioAuth<'a>)> {
        match self {
            Self::BuiltIn {
                kind,
                account,
                config_base_url,
                config_api_key,
            } => {
                let base_url = kind.resolve_base_url(*config_base_url)?;
                let auth = if *kind == ProviderKind::MuseCode {
                    AudioAuth::MuseCode { account: *account }
                } else {
                    AudioAuth::Bearer(kind.resolve_api_key(*config_api_key)?)
                };
                Ok((base_url, auth))
            }
            Self::Custom { name, cfg } => {
                let base_url = cfg
                    .effective_base_url(CustomProviderApi::ChatCompletions)
                    .with_context(|| format!("custom provider `{name}`"))?;
                let api_key = cfg
                    .resolve_api_key()
                    .with_context(|| format!("custom provider `{name}`"))?;
                Ok((base_url, AudioAuth::Bearer(api_key)))
            }
        }
    }
}

/// Settings for a built-in provider: `audio_providers.toml` with the config's
/// `[providers.<name>.audio]` fields on top.
///
/// A built-in provider without a shipped entry is refused even when the config
/// gives it an audio block: built-in providers use their own auth schemes
/// (`x-api-key`, OAuth, query keys), so a Bearer request may be wrong for
/// them. Another OpenAI-compatible endpoint goes in as a custom provider.
fn builtin_settings(
    kind: ProviderKind,
    model: &str,
    config: &Config,
    model_input: &str,
) -> Result<AudioSettings> {
    let section = format!("[providers.{}.audio]", provider_config_section(kind));
    let Some(audio) = effective_provider_audio(&config.providers, kind)
        .filter(|_| shipped_provider_audio(kind).is_some())
    else {
        let qualified = format!("{}:{model}", kind.id());
        if config.providers.get(kind).audio.is_some() {
            bail!(
                "{section} is set, but ask-media has no audio route for {}: built-in providers need a shipped entry in audio_providers.toml. For another OpenAI-compatible endpoint, add it as [providers.custom.<name>] with an `audio` block.",
                kind.label()
            );
        }
        if model_audio_input(kind.id(), model).is_some() {
            bail!(
                "`{qualified}` has `input_audio = true` in model_overrides.toml, but ask-media has no audio route for {}",
                kind.label()
            );
        }
        bail!(
            "ask-media supports Gemini models (any media, e.g. `{DEFAULT_ASK_MEDIA_MODEL}`) and models with audio input declared: {}; got `{model_input}`",
            declared_list()
        );
    };
    validate_settings(kind.label(), &section, &audio)
}

/// Settings for a custom provider: its `[providers.custom.<name>.audio]`
/// block only (no shipped defaults), and only over Chat Completions.
fn custom_settings(name: &str, cfg: &CustomProviderConfig, model: &str) -> Result<AudioSettings> {
    let section = format!("[providers.custom.{name}.audio]");
    let Some(audio) = cfg.audio.as_deref() else {
        bail!(
            "`{name}:{model}` cannot take audio: custom provider `{name}` has no {section} block. Add one with `payload` and `formats`, and declare the model with `input_audio = true` in model_overrides.toml."
        );
    };
    let api = match user_model_api_override(name, model) {
        Some(value) => CustomProviderApi::from_id(&value).ok_or_else(|| {
            anyhow!("unsupported `api = \"{value}\"` for `{name}:{model}` in model_overrides.toml")
        })?,
        None => cfg.api,
    };
    if api != CustomProviderApi::ChatCompletions {
        bail!(
            "`{name}:{model}` uses the Anthropic Messages API; ask-media sends audio only over Chat Completions (`api = \"chat-completions\"`)"
        );
    }
    validate_settings(name, &section, audio)
}

/// Checks the model's audio declaration, then sends.
async fn ask_declared_audio(
    path: &Path,
    prompt: &str,
    provider: &str,
    model: &str,
    mut settings: AudioSettings,
    endpoint: &Endpoint<'_>,
) -> Result<String> {
    let qualified = format!("{provider}:{model}");
    let Some(audio) = model_audio_input(provider, model) else {
        bail!(
            "`{qualified}` has no audio input declared. Declared audio models: {}. If {} documents audio input for this model, add to $ZDX_HOME/model_overrides.toml:\n[[override]]\nid = \"{qualified}\"\ninput_audio = true",
            declared_list(),
            settings.label
        );
    };
    if let Some(formats) = model_formats(&qualified, &audio)? {
        settings.formats = formats;
    }
    ask_audio_chat(path, prompt, model, &settings, endpoint).await
}

/// Validates one merged audio block. `section` names it in errors.
fn validate_settings(
    label: &str,
    section: &str,
    audio: &ProviderAudioConfig,
) -> Result<AudioSettings> {
    let payload_name = audio
        .payload
        .as_deref()
        .ok_or_else(|| anyhow!("{section} needs `payload`"))?;
    let payload = AudioPayload::from_name(payload_name).ok_or_else(|| {
        anyhow!(
            "{section} has unknown `payload = \"{payload_name}\"` (known: {})",
            AudioPayload::NAMES.join(", ")
        )
    })?;
    let names = audio
        .formats
        .as_ref()
        .ok_or_else(|| anyhow!("{section} needs `formats`"))?;
    let formats = parse_formats(names, &format!("`formats` in {section}"))?;
    let max_base64_bytes = match audio.max_base64_bytes {
        Some(0) => bail!("`max_base64_bytes` in {section} must be greater than 0"),
        Some(value) => Some(usize::try_from(value).unwrap_or(usize::MAX)),
        None => None,
    };
    if audio.max_tokens == Some(0) {
        bail!("`max_tokens` in {section} must be greater than 0");
    }
    Ok(AudioSettings {
        label: label.to_string(),
        payload,
        formats,
        max_base64_bytes,
        max_tokens: audio.max_tokens,
        text_modalities: audio.text_modalities.unwrap_or(false),
    })
}

fn parse_formats(names: &[String], what: &str) -> Result<Vec<AudioFormat>> {
    let formats = names
        .iter()
        .map(|name| {
            AudioFormat::from_extension(name.trim()).ok_or_else(|| {
                anyhow!("{what} has unknown format `{name}` (known: wav, mp3, ogg, flac, m4a, aac)")
            })
        })
        .collect::<Result<Vec<_>>>()?;
    if !formats.contains(&AudioFormat::Wav) {
        bail!("{what} must include `wav`, the format other audio is converted to");
    }
    Ok(formats)
}

fn declared_list() -> String {
    let models = declared_audio_models();
    if models.is_empty() {
        return "none".to_string();
    }
    models
        .iter()
        .map(|id| format!("`{id}`"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// The model's `audio_formats` from `model_overrides.toml`, which replace the
/// provider's `formats` for that model.
fn model_formats(qualified: &str, audio: &AudioInput) -> Result<Option<Vec<AudioFormat>>> {
    audio
        .formats
        .as_deref()
        .map(|names| {
            parse_formats(
                names,
                &format!("`audio_formats` for `{qualified}` in model_overrides.toml"),
            )
        })
        .transpose()
}

async fn ask_gemini(
    path: &Path,
    prompt: &str,
    selection: &ProviderSelection,
    config: &Config,
) -> Result<String> {
    let bytes = read_media(path)?;
    let mime = media_mime_for_path(path).ok_or_else(|| {
        anyhow!(
            "unsupported media type for `{}` (supported: PDF, common image/audio/video formats)",
            path.display()
        )
    })?;

    let gemini_config = GeminiConfig::from_env(
        selection.model.clone(),
        None,
        config.providers.gemini.effective_base_url(),
        config.providers.gemini.effective_api_key(),
        None,
    )?;

    GeminiClient::new(gemini_config)
        .generate_text_from_media(mime, &bytes, prompt)
        .await
        .context("Gemini media understanding request failed")
}

async fn ask_audio_chat(
    path: &Path,
    prompt: &str,
    model: &str,
    settings: &AudioSettings,
    endpoint: &Endpoint<'_>,
) -> Result<String> {
    let label = settings.label.as_str();
    let source_format = audio_format_for_path(path).ok_or_else(|| {
        anyhow!(
            "{label} models in ask-media take audio only (wav, mp3, ogg/oga/opus, m4a, aac, flac); `{}` is not one. Use a Gemini model for images, PDFs, text, and video.",
            path.display()
        )
    })?;
    let bytes = read_media(path)?;

    let (audio, format) = if settings.formats.contains(&source_format) {
        (bytes, source_format)
    } else {
        let wav = transcode_to_wav(FFMPEG_BIN, &bytes)
            .await
            .with_context(|| {
                format!("convert {} audio to WAV for {label}", source_format.name())
            })?;
        (wav, AudioFormat::Wav)
    };
    check_audio_size(settings, audio.len())?;

    let (base_url, auth) = endpoint.resolve()?;
    let route = AudioRoute {
        settings,
        base_url,
        auth,
    };
    let question = AudioQuestion {
        model,
        prompt,
        audio: &audio,
        format,
    };
    audio_chat::ask_audio(route, question)
        .await
        .with_context(|| format!("{label} audio understanding request failed"))
}

fn read_media(path: &Path) -> Result<Vec<u8>> {
    let bytes = fs::read(path).with_context(|| format!("read media file `{}`", path.display()))?;
    if bytes.is_empty() {
        bail!("media file `{}` is empty", path.display());
    }
    if bytes.len() > MAX_INLINE_BYTES {
        bail!(
            "media file `{}` is {:.1} MiB; inline limit is {} MiB (File API upload not yet supported)",
            path.display(),
            mib(bytes.len()),
            MAX_INLINE_BYTES / (1024 * 1024)
        );
    }
    Ok(bytes)
}

fn check_audio_size(settings: &AudioSettings, len: usize) -> Result<()> {
    if len > MAX_INLINE_BYTES {
        bail!(
            "audio is {:.1} MiB after conversion to WAV; inline limit is {} MiB. Send a shorter clip.",
            mib(len),
            MAX_INLINE_BYTES / (1024 * 1024)
        );
    }
    if let Some(max) = settings.max_base64_bytes
        && base64_len(len) > max
    {
        bail!(
            "audio is {:.1} MiB as base64; {} accepts at most {:.1} MiB. Send a shorter clip.",
            mib(base64_len(len)),
            settings.label,
            mib(max)
        );
    }
    Ok(())
}

#[allow(clippy::cast_precision_loss)]
fn mib(bytes: usize) -> f64 {
    bytes as f64 / (1024.0 * 1024.0)
}

/// Converts audio to mono 16-bit PCM WAV at the decoder's sample rate.
///
/// Only the container and codec change: decoding a lossy file (OGG/Opus, AAC,
/// MP3) to PCM adds no new loss, and the sample rate is kept (Opus always
/// decodes at 48 kHz), so high-frequency detail such as fricatives is not cut
/// here. Stereo is mixed down to mono. What each provider does with the audio
/// after it arrives (resampling, its own limits) is not documented and was not
/// measured. PCM is larger than the source: about 94 KiB per second at 48 kHz,
/// so the 15 MiB limit allows about 2.5 minutes, and Alibaba's 10 MB base64
/// limit about 75 s.
async fn transcode_to_wav(ffmpeg_bin: &str, audio: &[u8]) -> Result<Vec<u8>> {
    let dir = tempfile::tempdir().context("create temp dir for audio conversion")?;
    let input = dir.path().join("in.audio");
    let output = dir.path().join("out.wav");
    fs::write(&input, audio).context("write temp audio for conversion")?;

    let completed = tokio::process::Command::new(ffmpeg_bin)
        .args(["-hide_banner", "-loglevel", "error", "-y", "-i"])
        .arg(&input)
        .args(["-vn", "-ac", "1", "-c:a", "pcm_s16le", "-f", "wav"])
        .arg(&output)
        .output()
        .await
        .map_err(|err| {
            if err.kind() == std::io::ErrorKind::NotFound {
                anyhow!("ffmpeg is required to convert this audio and was not found on PATH")
            } else {
                anyhow::Error::new(err).context("spawn ffmpeg")
            }
        })?;
    if !completed.status.success() {
        bail!(
            "ffmpeg failed: {}",
            String::from_utf8_lossy(&completed.stderr).trim()
        );
    }
    fs::read(&output).context("read converted WAV")
}

fn audio_format_for_path(path: &Path) -> Option<AudioFormat> {
    AudioFormat::from_extension(path.extension()?.to_str()?)
}

/// Whether [`ask_media`] can take `path`: a supported type within the inline
/// size limit. Says nothing about whether the request itself will succeed.
#[must_use]
pub fn can_ask_media(path: &Path, size_bytes: u64) -> bool {
    media_mime_for_path(path).is_some() && size_bytes <= MAX_INLINE_BYTES as u64
}

/// Maps a file extension to a Gemini-supported MIME type.
fn media_mime_for_path(path: &Path) -> Option<&'static str> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    let mime = match ext.as_str() {
        // documents
        "pdf" => "application/pdf",
        "txt" | "text" => "text/plain",
        // images
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "webp" => "image/webp",
        "gif" => "image/gif",
        "heic" => "image/heic",
        "heif" => "image/heif",
        // audio
        "mp3" => "audio/mp3",
        "wav" => "audio/wav",
        "ogg" | "oga" | "opus" => "audio/ogg",
        "m4a" => "audio/mp4",
        "aac" => "audio/aac",
        "flac" => "audio/flac",
        // video
        "mp4" => "video/mp4",
        "mov" => "video/quicktime",
        "mpeg" | "mpg" => "video/mpeg",
        "webm" => "video/webm",
        "flv" => "video/x-flv",
        "wmv" => "video/wmv",
        "3gp" | "3gpp" => "video/3gpp",
        _ => return None,
    };
    Some(mime)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_common_media_extensions() {
        assert_eq!(
            media_mime_for_path(Path::new("a.pdf")),
            Some("application/pdf")
        );
        assert_eq!(
            media_mime_for_path(Path::new("A.PDF")),
            Some("application/pdf")
        );
        assert_eq!(
            media_mime_for_path(Path::new("clip.mp4")),
            Some("video/mp4")
        );
        assert_eq!(
            media_mime_for_path(Path::new("voice.ogg")),
            Some("audio/ogg")
        );
        assert_eq!(
            media_mime_for_path(Path::new("pic.jpeg")),
            Some("image/jpeg")
        );
    }

    #[test]
    fn rejects_unknown_extension() {
        assert_eq!(media_mime_for_path(Path::new("archive.zip")), None);
        assert_eq!(media_mime_for_path(Path::new("noext")), None);
    }

    async fn ask_err(path: &Path, model: &str) -> String {
        format!(
            "{:#}",
            ask_media(path, "What is said?", model, &Config::default())
                .await
                .unwrap_err()
        )
    }

    #[tokio::test]
    async fn provider_without_media_route_lists_audio_models() {
        let _home = crate::test_support::temp_zdx_home();
        let err = ask_err(Path::new("clip.wav"), "anthropic:model-a").await;
        assert!(err.contains("ask-media supports Gemini models"), "{err}");
        assert!(err.contains("`xiaomi:mimo-v2.6-flash`"), "{err}");
        assert!(err.contains("`muse-code:muse-wonder-1.0`"), "{err}");
        assert!(err.contains("got `anthropic:model-a`"), "{err}");
    }

    #[tokio::test]
    async fn undeclared_model_on_audio_provider_is_refused() {
        let _home = crate::test_support::temp_zdx_home();
        let err = ask_err(Path::new("clip.wav"), "mistral:voxtral-mini-latest").await;
        assert!(
            err.contains("`mistral:voxtral-mini-latest` has no audio input declared"),
            "{err}"
        );
        assert!(err.contains("`mistral:voxtral-small-latest`"), "{err}");
        assert!(err.contains("input_audio = true"), "{err}");

        let err = ask_err(Path::new("clip.wav"), "meta:muse-spark-1.3").await;
        assert!(err.contains("has no audio input declared"), "{err}");
    }

    /// A model declaration alone cannot open a built-in provider without a
    /// shipped audio entry; it fails, naming the reason. A user `false` turns
    /// off an embedded default.
    #[tokio::test]
    async fn user_overrides_cannot_add_providers_but_can_disable_defaults() {
        let home = crate::test_support::temp_zdx_home();
        std::fs::write(
            home.path().join("model_overrides.toml"),
            r#"
[[override]]
id = "openai:model-a"
input_audio = true

[[override]]
id = "xiaomi:mimo-v2.6-flash"
input_audio = false
"#,
        )
        .unwrap();

        let err = ask_err(Path::new("clip.wav"), "openai:model-a").await;
        assert!(err.contains("no audio route for OpenAI"), "{err}");

        let err = ask_err(Path::new("clip.wav"), "xiaomi:mimo-v2.6-flash").await;
        assert!(err.contains("has no audio input declared"), "{err}");
    }

    fn settings_for(config: &Config, kind: ProviderKind) -> Result<AudioSettings> {
        builtin_settings(kind, "model-a", config, "test")
    }

    fn shipped(kind: ProviderKind) -> AudioSettings {
        settings_for(&Config::default(), kind).unwrap()
    }

    /// The embedded `audio_providers.toml` produces exactly the request
    /// settings that were hard-coded in `audio_chat` before they moved out of
    /// Rust, so every existing provider request is unchanged.
    #[test]
    fn shipped_defaults_match_the_former_provider_constants() {
        use AudioFormat::{Aac, Flac, M4a, Mp3, Ogg, Wav};
        use AudioPayload::{Base64String, Base64WithFormat, DataUri, DataUriWithFormat};
        let expected = [
            (
                ProviderKind::Xiaomi,
                DataUri,
                vec![Mp3, Wav, Flac, M4a, Ogg],
                Some(52_428_800),
                None,
                false,
            ),
            (
                ProviderKind::Alibaba,
                DataUriWithFormat,
                vec![Wav, Mp3, Aac],
                Some(10_000_000),
                None,
                false,
            ),
            (
                ProviderKind::Stepfun,
                DataUri,
                vec![Mp3, Wav],
                None,
                None,
                true,
            ),
            (
                ProviderKind::Meta,
                Base64WithFormat,
                vec![Mp3, Wav],
                None,
                Some(8000),
                false,
            ),
            (
                ProviderKind::MuseCode,
                Base64WithFormat,
                vec![Mp3, Wav],
                None,
                Some(8000),
                false,
            ),
            (
                ProviderKind::Mistral,
                Base64String,
                vec![Wav],
                None,
                None,
                false,
            ),
        ];
        for (kind, payload, formats, max_base64_bytes, max_tokens, text_modalities) in expected {
            assert_eq!(
                shipped(kind),
                AudioSettings {
                    label: kind.label().to_string(),
                    payload,
                    formats,
                    max_base64_bytes,
                    max_tokens,
                    text_modalities,
                },
                "{kind:?}"
            );
        }
        assert_eq!(
            crate::config::shipped_audio_sections(),
            vec![
                "alibaba",
                "meta",
                "mistral",
                "muse_code",
                "stepfun",
                "xiaomi"
            ]
        );
    }

    /// Every entry in `audio_providers.toml` names a built-in provider and
    /// validates, and the generated config template carries none of it, so the
    /// file is the only source of these defaults.
    #[test]
    fn audio_providers_toml_is_the_single_source() {
        for section in crate::config::shipped_audio_sections() {
            let kind = ProviderKind::all()
                .iter()
                .copied()
                .find(|kind| provider_config_section(*kind) == section)
                .unwrap_or_else(|| panic!("`[{section}]` is not a built-in provider"));
            shipped(kind);
        }
        let generated = Config::generate().unwrap();
        assert!(
            !generated
                .lines()
                .any(|line| line.starts_with("[providers.") && line.ends_with(".audio]")),
            "{generated}"
        );
        assert!(Config::default().providers.stepfun.audio.is_none());
    }

    /// A config layer replaces only the fields it sets, and an explicit
    /// `false` beats a shipped `true`.
    #[test]
    fn configured_fields_override_shipped_defaults_one_by_one() {
        let config: Config = toml::from_str(
            r#"
[providers.stepfun]
models = ["model-a"]

[providers.stepfun.audio]
max_tokens = 2000
text_modalities = false

[providers.muse_code.audio]
formats = ["wav"]
"#,
        )
        .unwrap();
        let stepfun = settings_for(&config, ProviderKind::Stepfun).unwrap();
        assert_eq!(stepfun.payload, AudioPayload::DataUri);
        assert_eq!(stepfun.formats, vec![AudioFormat::Mp3, AudioFormat::Wav]);
        assert_eq!(stepfun.max_tokens, Some(2000));
        assert!(!stepfun.text_modalities);

        let muse = settings_for(&config, ProviderKind::MuseCode).unwrap();
        assert_eq!(muse.formats, vec![AudioFormat::Wav]);
        assert_eq!(muse.max_tokens, Some(8000));

        // A provider table without an audio block keeps the shipped defaults.
        let config: Config = toml::from_str("[providers.xiaomi]\nmodels = []\n").unwrap();
        assert_eq!(
            settings_for(&config, ProviderKind::Xiaomi).unwrap(),
            shipped(ProviderKind::Xiaomi)
        );
    }

    #[test]
    fn malformed_audio_settings_fail_with_the_section_name() {
        let err = |kind: ProviderKind, block: &str| {
            let config: Config = toml::from_str(block).unwrap();
            settings_for(&config, kind).unwrap_err().to_string()
        };
        let e = err(
            ProviderKind::Meta,
            "[providers.meta.audio]\npayload = \"data_uri\"\n",
        );
        assert!(
            e.contains("[providers.meta.audio] has unknown `payload = \"data_uri\"`"),
            "{e}"
        );
        assert!(e.contains("base64-with-format"), "{e}");

        let e = err(
            ProviderKind::Alibaba,
            "[providers.alibaba.audio]\nformats = [\"mp3\"]\n",
        );
        assert!(e.contains("must include `wav`"), "{e}");

        let e = err(
            ProviderKind::Alibaba,
            "[providers.alibaba.audio]\nformats = [\"wav\", \"webm\"]\n",
        );
        assert!(e.contains("unknown format `webm`"), "{e}");

        let e = err(
            ProviderKind::Xiaomi,
            "[providers.xiaomi.audio]\nmax_base64_bytes = 0\n",
        );
        assert!(
            e.contains("`max_base64_bytes` in [providers.xiaomi.audio]"),
            "{e}"
        );

        let e = err(
            ProviderKind::MuseCode,
            "[providers.muse_code.audio]\nmax_tokens = 0\n",
        );
        assert!(
            e.contains("`max_tokens` in [providers.muse_code.audio]"),
            "{e}"
        );

        // Config cannot open a route for a provider without shipped audio.
        let e = err(
            ProviderKind::OpenAI,
            "[providers.openai.audio]\npayload = \"base64-with-format\"\nformats = [\"wav\"]\n",
        );
        assert!(e.contains("no audio route for OpenAI"), "{e}");
        assert!(e.contains("[providers.custom.<name>]"), "{e}");
    }

    fn custom_config(extra: &str) -> Config {
        toml::from_str(&format!(
            "[providers.custom.acme-audio]\nbase_url = \"https://llm.example.com/v1\"\napi_key = \"test-key\"\nmodels = []\n{extra}"
        ))
        .unwrap()
    }

    fn custom_err(config: &Config, model: &str) -> String {
        let (name, cfg, model) = config
            .providers
            .named_custom_provider_for_model(model)
            .unwrap();
        custom_settings(name, cfg, &model).unwrap_err().to_string()
    }

    /// A custom provider needs its own audio block (no shipped defaults) with
    /// `payload` and `formats`, and Chat Completions as its protocol.
    #[test]
    fn custom_provider_audio_settings_are_validated() {
        let none = custom_config("");
        let e = custom_err(&none, "acme-audio:model-a");
        assert!(
            e.contains("has no [providers.custom.acme-audio.audio] block"),
            "{e}"
        );

        let partial = custom_config("[providers.custom.acme-audio.audio]\nmax_tokens = 10\n");
        let e = custom_err(&partial, "acme-audio:model-a");
        assert!(
            e.contains("[providers.custom.acme-audio.audio] needs `payload`"),
            "{e}"
        );

        let anthropic = custom_config(
            "api = \"anthropic\"\n[providers.custom.acme-audio.audio]\npayload = \"base64\"\nformats = [\"wav\"]\n",
        );
        let e = custom_err(&anthropic, "acme-audio:model-a");
        assert!(e.contains("only over Chat Completions"), "{e}");

        let ok = custom_config(
            "[providers.custom.acme-audio.audio]\npayload = \"base64-with-format\"\nformats = [\"wav\", \"mp3\"]\nmax_tokens = 10\n",
        );
        let (name, cfg, model) = ok
            .providers
            .named_custom_provider_for_model("acme-audio:model-a")
            .unwrap();
        let settings = custom_settings(name, cfg, &model).unwrap();
        assert_eq!(
            settings,
            AudioSettings {
                label: "acme-audio".to_string(),
                payload: AudioPayload::Base64WithFormat,
                formats: vec![AudioFormat::Wav, AudioFormat::Mp3],
                max_base64_bytes: None,
                max_tokens: Some(10),
                text_modalities: false,
            }
        );
    }

    #[test]
    fn model_audio_formats_are_validated() {
        let none = model_formats("stepfun:model-a", &AudioInput { formats: None }).unwrap();
        assert_eq!(none, None);

        let custom = AudioInput {
            formats: Some(vec!["wav".into(), "OGA".into()]),
        };
        assert_eq!(
            model_formats("stepfun:model-a", &custom).unwrap(),
            Some(vec![AudioFormat::Wav, AudioFormat::Ogg])
        );

        let no_wav = AudioInput {
            formats: Some(vec!["mp3".into()]),
        };
        let err = model_formats("stepfun:model-a", &no_wav).unwrap_err();
        assert!(
            err.to_string()
                .contains("`audio_formats` for `stepfun:model-a`"),
            "{err}"
        );
        assert!(err.to_string().contains("must include `wav`"), "{err}");
    }

    #[tokio::test]
    async fn audio_only_provider_refuses_other_media() {
        let _home = crate::test_support::temp_zdx_home();
        let dir = tempfile::tempdir().unwrap();
        for name in ["page.pdf", "pic.png", "clip.mp4"] {
            let path = dir.path().join(name);
            std::fs::write(&path, b"x").unwrap();
            let err = ask_err(&path, "xiaomi:mimo-v2.6-flash").await;
            assert!(err.contains("take audio only"), "{name}: {err}");
            assert!(err.contains("Use a Gemini model"), "{name}: {err}");
        }
    }

    #[tokio::test]
    async fn missing_ffmpeg_is_a_clear_error() {
        let err = transcode_to_wav("zdx-test-no-such-ffmpeg", b"x")
            .await
            .unwrap_err();
        assert!(format!("{err:#}").contains("ffmpeg is required"), "{err:#}");
    }

    #[test]
    fn audio_size_respects_provider_base64_limit() {
        let alibaba = shipped(ProviderKind::Alibaba);
        assert!(check_audio_size(&alibaba, 7_000_000).is_ok());
        let err = check_audio_size(&alibaba, 8_000_000)
            .unwrap_err()
            .to_string();
        assert!(err.contains("Alibaba accepts at most"), "{err}");

        let stepfun = shipped(ProviderKind::Stepfun);
        assert!(check_audio_size(&stepfun, 8_000_000).is_ok());
        assert!(check_audio_size(&stepfun, MAX_INLINE_BYTES + 1).is_err());
    }
}
