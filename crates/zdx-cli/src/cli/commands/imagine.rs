//! Imagine command handler.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use chrono::Utc;
use zdx_core::config;
use zdx_core::images::path_mime;
use zdx_core::providers::gemini::{
    GeminiClient, GeminiConfig, GeminiImageGenerationOptions, GeneratedImage,
};
use zdx_core::providers::{ProviderKind, resolve_provider};

const DEFAULT_IMAGINE_MODEL: &str = "gemini:gemini-3.1-flash-image-preview";

pub struct ImagineRunOptions<'a> {
    pub root: &'a Path,
    pub prompt: &'a str,
    pub out: Option<&'a str>,
    pub model_override: Option<&'a str>,
    pub aspect: Option<&'a str>,
    pub size: Option<&'a str>,
    pub config: &'a config::Config,
}

pub async fn run(options: ImagineRunOptions<'_>) -> Result<()> {
    let model_input = options.model_override.unwrap_or(DEFAULT_IMAGINE_MODEL);
    let provider_selection = resolve_provider(model_input);
    if provider_selection.kind != ProviderKind::Gemini {
        bail!(
            "zdx imagine currently supports Gemini API models only. Use a model with the 'gemini:' prefix"
        );
    }

    let gemini_config = GeminiConfig::from_env(
        provider_selection.model,
        None,
        options.config.providers.gemini.effective_base_url(),
        options.config.providers.gemini.effective_api_key(),
        None,
    )?;

    let client = GeminiClient::new(gemini_config);
    let response = client
        .generate_images(
            options.prompt,
            &GeminiImageGenerationOptions {
                aspect_ratio: options.aspect.map(std::string::ToString::to_string),
                image_size: options.size.map(std::string::ToString::to_string),
            },
        )
        .await
        .context("generate image")?;

    if response.images.is_empty() {
        if let Some(text) = response.text_parts.first() {
            bail!("Model returned no images. Model text: {text}");
        }
        bail!("Model returned no images");
    }

    let output_paths = resolve_output_paths(options.root, options.out, &response.images);
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

fn resolve_output_paths(root: &Path, out: Option<&str>, images: &[GeneratedImage]) -> Vec<PathBuf> {
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
                vec![root.join(format!("image-{ts}.{ext}"))]
            } else {
                images
                    .iter()
                    .enumerate()
                    .map(|(idx, image)| {
                        let ext =
                            path_mime::extension_for_mime_type(&image.mime_type).unwrap_or("png");
                        root.join(format!("image-{ts}-{}.{}", idx + 1, ext))
                    })
                    .collect()
            }
        }
    }
}
