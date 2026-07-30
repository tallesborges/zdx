//! Imagine command handler.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use chrono::Utc;
use zdx_engine::config;
use zdx_engine::images::path_mime;
use zdx_engine::providers::alibaba::{
    AlibabaImageClient, AlibabaImageGenerationOptions, AlibabaImageInput,
};
use zdx_engine::providers::gemini::{
    GeminiClient, GeminiConfig, GeminiImageGenerationOptions, ImageUsage, SourceImage,
};
use zdx_engine::providers::openai::{
    OpenAIClient, OpenAICodexClient, OpenAICodexConfig, OpenAIConfig, OpenAIImageGenerationOptions,
    OpenAIImageInput,
};
use zdx_engine::providers::{ProviderKind, resolve_provider};

const DEFAULT_IMAGINE_MODEL: &str = "gemini:gemini-3.1-flash-image-preview";
const DEFAULT_OPENAI_RESPONSES_IMAGE_SIZE: &str = "1024x1024";

pub struct ImagineRunOptions<'a> {
    pub root: &'a Path,
    pub prompt: &'a str,
    pub out: Option<&'a str>,
    pub model_override: Option<&'a str>,
    pub aspect: Option<&'a str>,
    pub size: Option<&'a str>,
    pub source: &'a [String],
    pub config: &'a config::Config,
}

pub async fn run(options: ImagineRunOptions<'_>) -> Result<()> {
    let model_input = options.model_override.unwrap_or(DEFAULT_IMAGINE_MODEL);
    let provider_selection = resolve_provider(model_input);

    let response = match provider_selection.kind {
        ProviderKind::Gemini => generate_gemini_images(&provider_selection.model, &options).await?,
        ProviderKind::OpenAI => {
            generate_openai_images(&provider_selection.model, provider_selection.fast, &options)
                .await?
        }
        ProviderKind::OpenAICodex => {
            generate_codex_images(&provider_selection.model, provider_selection.fast, &options)
                .await?
        }
        ProviderKind::Alibaba => {
            generate_alibaba_images(&provider_selection.model, &options).await?
        }
        _ => bail!(
            "zdx imagine supports Gemini, OpenAI, OpenAI Codex, and Alibaba (Qwen-Image) image generation. Use 'gemini:', 'openai:gpt-image-2', 'openai-codex:gpt-image-2', or 'alibaba:qwen-image-2.0-pro'"
        ),
    };

    if response.images.is_empty() {
        if let Some(text) = response.text_parts.first() {
            bail!("Model returned no images. Model text: {text}");
        }
        bail!("Model returned no images");
    }

    // Thinking image models can emit many billable image parts in a single
    // response. `zdx imagine` is a one-image command, so keep the first and say
    // plainly that the extras were still charged.
    if response.images.len() > 1 {
        eprintln!(
            "warning: model returned {} images for one request; keeping the first (all of them were billed)",
            response.images.len()
        );
    }
    if let Some(usage) = &response.usage_note {
        eprintln!("{usage}");
    }

    let image = &response.images[0];
    let default_dir = config::paths::artifact_root();
    let path = resolve_output_path(options.root, options.out, &default_dir, image);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("create output directory '{}'", parent.display()))?;
    }
    fs::write(&path, &image.data)
        .with_context(|| format!("write image to '{}'", path.display()))?;
    println!("{}", path.display());

    Ok(())
}

async fn generate_gemini_images(
    model: &str,
    options: &ImagineRunOptions<'_>,
) -> Result<GenerateImageResponse> {
    let gemini_config = GeminiConfig::from_env(
        model.to_string(),
        None,
        options.config.providers.gemini.effective_base_url(),
        options.config.providers.gemini.effective_api_key(),
        None,
    )?;

    let source_images = load_source_images(options.source)?
        .into_iter()
        .map(|image| SourceImage {
            mime_type: image.mime_type,
            data: image.data,
        })
        .collect();

    let client = GeminiClient::new(gemini_config);
    let response = client
        .generate_images(
            options.prompt,
            &GeminiImageGenerationOptions {
                aspect_ratio: options.aspect.map(std::string::ToString::to_string),
                image_size: options.size.map(std::string::ToString::to_string),
                source_images,
            },
        )
        .await
        .context("generate image")?;

    Ok(GenerateImageResponse {
        images: response
            .images
            .into_iter()
            .map(|image| GeneratedImage {
                mime_type: image.mime_type,
                data: image.data,
            })
            .collect(),
        text_parts: response.text_parts,
        usage_note: response.usage.as_ref().map(format_gemini_usage),
    })
}

/// Gemini bills IMAGE candidate tokens at a much higher rate than text output,
/// so surface the split instead of a single opaque total.
fn format_gemini_usage(usage: &ImageUsage) -> String {
    format!(
        "tokens: prompt {}, output {} (image {}, thinking {}), total {}",
        usage.prompt_tokens,
        usage.candidates_tokens,
        usage.image_tokens,
        usage.thoughts_tokens,
        usage.total_tokens
    )
}

async fn generate_alibaba_images(
    model: &str,
    options: &ImagineRunOptions<'_>,
) -> Result<GenerateImageResponse> {
    if options.aspect.is_some() {
        bail!("Alibaba (Qwen-Image) does not support --aspect; use --size WIDTHxHEIGHT instead");
    }

    let source_images: Vec<AlibabaImageInput> = load_source_images(options.source)?
        .into_iter()
        .map(|image| AlibabaImageInput {
            mime_type: image.mime_type,
            data: image.data,
        })
        .collect();
    if source_images.len() > 3 {
        bail!("Alibaba image editing supports at most 3 source images");
    }

    let size = options.size.map(alibaba_image_size).transpose()?;

    let client = AlibabaImageClient::from_env(
        model.to_string(),
        options.config.providers.alibaba.effective_api_key(),
    )?;
    let response = client
        .generate_images(
            options.prompt,
            &AlibabaImageGenerationOptions {
                size,
                n: None,
                source_images,
                negative_prompt: None,
                prompt_extend: None,
            },
        )
        .await
        .context("generate image with Alibaba (Qwen-Image)")?;

    Ok(GenerateImageResponse {
        images: response
            .images
            .into_iter()
            .map(|image| GeneratedImage {
                mime_type: image.mime_type,
                data: image.data,
            })
            .collect(),
        text_parts: response.text_parts,
        usage_note: None,
    })
}

fn alibaba_image_size(size: &str) -> Result<String> {
    // The CLI validates `--size` to these tokens; map them to DashScope `W*H`.
    match size {
        "512px" => Ok("512*512".to_string()),
        "1K" => Ok("1024*1024".to_string()),
        "2K" => Ok("2048*2048".to_string()),
        "4K" => bail!("Alibaba (Qwen-Image) does not support --size 4K; use 1K or 2K"),
        _ => bail!("unsupported Alibaba image size: {size}"),
    }
}

async fn generate_codex_images(
    model: &str,
    fast: bool,
    options: &ImagineRunOptions<'_>,
) -> Result<GenerateImageResponse> {
    let image_options = openai_family_image_options("OpenAI Codex", options)?;
    let service_tier = fast.then(|| "priority".to_string());
    let codex_config = OpenAICodexConfig::new(
        model.to_string(),
        None,
        options
            .config
            .providers
            .openai_codex
            .effective_text_verbosity(),
        None,
        service_tier,
        false,
        None,
    );
    let response = OpenAICodexClient::new(codex_config)
        .generate_images(options.prompt, &image_options)
        .await
        .context("generate image with OpenAI Codex")?;

    Ok(GenerateImageResponse {
        images: response
            .images
            .into_iter()
            .map(|image| GeneratedImage {
                mime_type: image.mime_type,
                data: image.data,
            })
            .collect(),
        text_parts: response.text_parts,
        usage_note: None,
    })
}

async fn generate_openai_images(
    model: &str,
    fast: bool,
    options: &ImagineRunOptions<'_>,
) -> Result<GenerateImageResponse> {
    let image_options = openai_family_image_options("OpenAI", options)?;
    let service_tier = fast.then(|| "priority".to_string());
    let openai_config = OpenAIConfig::from_env(
        model.to_string(),
        None,
        options.config.providers.openai.effective_base_url(),
        options.config.providers.openai.effective_api_key(),
        None,
        options.config.providers.openai.effective_text_verbosity(),
        None,
        service_tier,
        false,
    )?;
    let response = OpenAIClient::new(openai_config)
        .generate_images(options.prompt, &image_options)
        .await
        .context("generate image with OpenAI")?;

    Ok(GenerateImageResponse {
        images: response
            .images
            .into_iter()
            .map(|image| GeneratedImage {
                mime_type: image.mime_type,
                data: image.data,
            })
            .collect(),
        text_parts: response.text_parts,
        usage_note: None,
    })
}

fn openai_family_image_options(
    provider_label: &str,
    options: &ImagineRunOptions<'_>,
) -> Result<OpenAIImageGenerationOptions> {
    if options.aspect.is_some() {
        bail!(
            "{provider_label} image generation does not support --aspect yet; use --size instead"
        );
    }

    let source_images = load_source_images(options.source)?
        .into_iter()
        .map(|image| OpenAIImageInput {
            mime_type: image.mime_type,
            data: image.data,
        })
        .collect();

    Ok(OpenAIImageGenerationOptions {
        size: Some(options.size.map_or_else(
            || Ok(DEFAULT_OPENAI_RESPONSES_IMAGE_SIZE.to_string()),
            openai_family_image_size,
        )?),
        source_images,
    })
}

#[derive(Debug, Clone)]
struct LoadedSourceImage {
    mime_type: String,
    data: Vec<u8>,
}

fn load_source_images(source: &[String]) -> Result<Vec<LoadedSourceImage>> {
    source
        .iter()
        .map(|path_str| {
            let path = path_mime::normalize_input_path(path_str);
            let mime_type = path_mime::mime_type_for_extension(path_str)
                .context(format!("unsupported image format: {path_str}"))?;
            let data = fs::read(&path)
                .with_context(|| format!("read source image '{}'", path.display()))?;
            Ok(LoadedSourceImage {
                mime_type: mime_type.to_string(),
                data,
            })
        })
        .collect()
}

fn openai_family_image_size(size: &str) -> Result<String> {
    match size {
        "1K" => Ok("1024x1024".to_string()),
        "2K" => Ok("2048x2048".to_string()),
        "4K" => Ok("3840x2160".to_string()),
        "512px" => bail!("OpenAI image generation does not support --size 512px"),
        _ => bail!("unsupported OpenAI image size: {size}"),
    }
}

#[derive(Debug, Clone)]
struct GeneratedImage {
    mime_type: String,
    data: Vec<u8>,
}

#[derive(Debug, Clone, Default)]
struct GenerateImageResponse {
    images: Vec<GeneratedImage>,
    text_parts: Vec<String>,
    /// Human-readable token accounting, when the provider reports it.
    usage_note: Option<String>,
}

fn resolve_output_path(
    root: &Path,
    out: Option<&str>,
    default_dir: &Path,
    image: &GeneratedImage,
) -> PathBuf {
    if let Some(path) = out
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
    {
        return if path.is_absolute() {
            path
        } else {
            root.join(path)
        };
    }

    let ts = Utc::now().format("%Y%m%d-%H%M%S");
    let ext = path_mime::extension_for_mime_type(&image.mime_type).unwrap_or("png");
    default_dir.join(format!("image-{ts}.{ext}"))
}
