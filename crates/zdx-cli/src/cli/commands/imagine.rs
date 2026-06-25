//! Imagine command handler.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use chrono::Utc;
use zdx_engine::config;
use zdx_engine::images::path_mime;
use zdx_engine::providers::gemini::{
    GeminiClient, GeminiConfig, GeminiImageGenerationOptions, SourceImage,
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
        ProviderKind::OpenAI => generate_openai_images(&provider_selection.model, &options).await?,
        ProviderKind::OpenAICodex => {
            generate_codex_images(&provider_selection.model, &options).await?
        }
        _ => bail!(
            "zdx imagine supports Gemini, OpenAI, and OpenAI Codex image generation. Use 'gemini:', 'openai:gpt-image-2', or 'openai-codex:gpt-image-2'"
        ),
    };

    if response.images.is_empty() {
        if let Some(text) = response.text_parts.first() {
            bail!("Model returned no images. Model text: {text}");
        }
        bail!("Model returned no images");
    }

    let default_dir = config::paths::artifact_root();
    let output_paths =
        resolve_output_paths(options.root, options.out, &default_dir, &response.images);
    for (image, path) in response.images.iter().zip(output_paths.iter()) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("create output directory '{}'", parent.display()))?;
        }
        fs::write(path, &image.data)
            .with_context(|| format!("write image to '{}'", path.display()))?;
        println!("{}", path.display());
    }

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
    })
}

async fn generate_codex_images(
    model: &str,
    options: &ImagineRunOptions<'_>,
) -> Result<GenerateImageResponse> {
    let image_options = openai_family_image_options("OpenAI Codex", options)?;
    let service_tier = options
        .config
        .providers
        .openai_codex
        .fast_mode
        .then(|| "priority".to_string());
    let codex_config = OpenAICodexConfig::new(
        model.to_string(),
        options.config.effective_max_tokens_for(model),
        None,
        options
            .config
            .providers
            .openai_codex
            .effective_text_verbosity(),
        None,
        service_tier,
        false,
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
    })
}

async fn generate_openai_images(
    model: &str,
    options: &ImagineRunOptions<'_>,
) -> Result<GenerateImageResponse> {
    let image_options = openai_family_image_options("OpenAI", options)?;
    let service_tier = options
        .config
        .providers
        .openai
        .fast_mode
        .then(|| "priority".to_string());
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
}

fn resolve_output_paths(
    root: &Path,
    out: Option<&str>,
    default_dir: &Path,
    images: &[GeneratedImage],
) -> Vec<PathBuf> {
    let out_path = out
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .map(|path| {
            if path.is_absolute() {
                path
            } else {
                root.join(path)
            }
        });

    match out_path {
        Some(path) if images.len() == 1 => vec![path],
        Some(path) => {
            let parent = path
                .parent()
                .map_or_else(|| root.to_path_buf(), Path::to_path_buf);
            let stem = path
                .file_stem()
                .and_then(|s| s.to_str())
                .filter(|s| !s.is_empty())
                .unwrap_or("image");

            images
                .iter()
                .enumerate()
                .map(|(idx, image)| {
                    let ext = path_mime::extension_for_mime_type(&image.mime_type).unwrap_or("png");
                    parent.join(format!("{stem}-{}.{}", idx + 1, ext))
                })
                .collect()
        }
        None => {
            let ts = Utc::now().format("%Y%m%d-%H%M%S");
            if images.len() == 1 {
                let ext = path_mime::extension_for_mime_type(&images[0].mime_type).unwrap_or("png");
                vec![default_dir.join(format!("image-{ts}.{ext}"))]
            } else {
                images
                    .iter()
                    .enumerate()
                    .map(|(idx, image)| {
                        let ext =
                            path_mime::extension_for_mime_type(&image.mime_type).unwrap_or("png");
                        default_dir.join(format!("image-{ts}-{}.{}", idx + 1, ext))
                    })
                    .collect()
            }
        }
    }
}
