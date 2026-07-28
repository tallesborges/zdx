//! Gemini API key provider (Generative Language API).

use anyhow::{Context, Result};
use base64::Engine;
use reqwest::header::{HeaderMap, HeaderValue};
use serde_json::{Value, json};
use zdx_types::ToolDefinition;

use super::shared::{GeminiThinkingConfig, build_gemini_request};
use super::sse::GeminiSseParser;
use crate::debug_metrics::maybe_wrap_with_metrics;
use crate::shared::{classify_reqwest_error, merge_system_prompt};
use crate::{ChatMessage, DebugTrace, ProviderError, ProviderKind, ProviderStream, wrap_stream};

/// Gemini API configuration.
#[derive(Debug, Clone)]
pub struct GeminiConfig {
    pub api_key: String,
    pub base_url: String,
    pub model: String,
    pub max_output_tokens: Option<u32>,
    /// Thinking configuration (level for Gemini 3, budget for Gemini 2.5)
    pub thinking_config: Option<GeminiThinkingConfig>,
}

impl GeminiConfig {
    /// Creates a new config from environment.
    ///
    /// Authentication resolution order:
    /// 1. `config_api_key` parameter (from config file)
    /// 2. `GEMINI_API_KEY` environment variable
    ///
    /// Environment variables:
    /// - `GEMINI_API_KEY` (fallback if not in config)
    /// - `GEMINI_BASE_URL` (optional)
    ///
    /// # Errors
    /// Returns an error if the operation fails.
    pub fn from_env(
        model: String,
        max_output_tokens: Option<u32>,
        config_base_url: Option<&str>,
        config_api_key: Option<&str>,
        thinking_config: Option<GeminiThinkingConfig>,
    ) -> Result<Self> {
        let api_key = ProviderKind::Gemini.resolve_api_key(config_api_key)?;
        let base_url = ProviderKind::Gemini.resolve_base_url(config_base_url)?;

        Ok(Self {
            api_key,
            base_url,
            model,
            max_output_tokens,
            thinking_config,
        })
    }
}

/// Gemini client.
pub struct GeminiClient {
    config: GeminiConfig,
    http: reqwest::Client,
}

/// A source image for editing / composition.
#[derive(Debug, Clone)]
pub struct SourceImage {
    pub mime_type: String,
    pub data: Vec<u8>,
}

/// Optional image generation settings.
#[derive(Debug, Clone, Default)]
pub struct GeminiImageGenerationOptions {
    /// Output image aspect ratio (e.g. "1:1", "16:9").
    pub aspect_ratio: Option<String>,
    /// Output image size preset (e.g. "1K", "2K", "4K").
    pub image_size: Option<String>,
    /// Source images for editing / multi-image composition.
    pub source_images: Vec<SourceImage>,
}

/// Output-token ceiling for image requests.
///
/// Gemini 3 image models are thinking models whose reasoning cannot be disabled,
/// and they can keep emitting billable image parts until the model-level cap
/// (32,768 tokens) is reached — a single 1K image is only ~1120 tokens, so an
/// uncapped request can silently bill for dozens of images. This leaves ample
/// room for compulsory thinking plus one 4K image while bounding the blast radius.
const IMAGE_MAX_OUTPUT_TOKENS: u32 = 8192;

/// A generated image from Gemini image models.
#[derive(Debug, Clone)]
pub struct GeneratedImage {
    pub mime_type: String,
    pub data: Vec<u8>,
}

/// Token accounting reported by Gemini for an image generation request.
#[derive(Debug, Clone, Default)]
pub struct ImageUsage {
    pub prompt_tokens: u64,
    pub candidates_tokens: u64,
    pub thoughts_tokens: u64,
    pub total_tokens: u64,
    /// Candidate tokens attributed to the IMAGE modality, which Gemini bills at
    /// the (much higher) per-image rate rather than the text output rate.
    pub image_tokens: u64,
}

/// Parsed response from a Gemini image generation request.
#[derive(Debug, Clone, Default)]
pub struct GenerateImageResponse {
    pub images: Vec<GeneratedImage>,
    pub text_parts: Vec<String>,
    pub usage: Option<ImageUsage>,
}

impl GeminiClient {
    pub fn new(config: GeminiConfig) -> Self {
        Self {
            config,
            http: reqwest::Client::new(),
        }
    }

    ///
    /// # Errors
    /// Returns an error if the operation fails.
    pub async fn send_messages_stream(
        &self,
        messages: &[ChatMessage],
        tools: &[ToolDefinition],
        system: Option<&str>,
    ) -> Result<ProviderStream> {
        let system_prompt = merge_system_prompt(system);
        let request = build_gemini_request(
            messages,
            tools,
            system_prompt.as_deref(),
            self.config.max_output_tokens,
            self.config.thinking_config.as_ref(),
            &self.config.model,
        );
        let trace = DebugTrace::from_env(&self.config.model, None);
        let url = format!(
            "{}/models/{}:streamGenerateContent?alt=sse",
            self.config.base_url, self.config.model
        );
        let headers = build_headers(&self.config.api_key)?;

        let response = if let Some(trace) = &trace {
            let body = serde_json::to_vec(&request)?;
            trace.write_request(&body);
            self.http
                .post(&url)
                .headers(headers)
                .body(body)
                .send()
                .await
                .map_err(|e| classify_reqwest_error(&e))?
        } else {
            self.http
                .post(&url)
                .headers(headers)
                .json(&request)
                .send()
                .await
                .map_err(|e| classify_reqwest_error(&e))?
        };

        let status = response.status();
        if !status.is_success() {
            let error_body = response.text().await.unwrap_or_default();
            return Err(ProviderError::http_status(status.as_u16(), &error_body).into());
        }

        let byte_stream = wrap_stream(trace, response.bytes_stream());
        let event_stream = GeminiSseParser::new(byte_stream, self.config.model.clone(), "gemini");
        Ok(maybe_wrap_with_metrics(event_stream))
    }

    /// Generate image content using Gemini image-capable models.
    ///
    /// # Errors
    /// Returns an error if the request fails or the response cannot be parsed.
    pub async fn generate_images(
        &self,
        prompt: &str,
        options: &GeminiImageGenerationOptions,
    ) -> Result<GenerateImageResponse> {
        let request = build_image_generation_request(prompt, options);
        let url = format!(
            "{}/models/{}:generateContent",
            self.config.base_url, self.config.model
        );
        let headers = build_json_headers(&self.config.api_key)?;

        let response = self
            .http
            .post(url)
            .headers(headers)
            .json(&request)
            .send()
            .await
            .map_err(|e| classify_reqwest_error(&e))?;

        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        if !status.is_success() {
            return Err(ProviderError::http_status(status.as_u16(), &body).into());
        }

        let value: Value = serde_json::from_str(&body)
            .with_context(|| format!("Failed to parse Gemini image response JSON: {body}"))?;
        parse_image_generation_response(&value)
    }

    /// Send an inline media file (audio/video/PDF/image) plus a prompt to the
    /// model and return its concatenated text answer.
    ///
    /// # Errors
    /// Returns an error if the request fails, the response cannot be parsed, or
    /// the model returns no text.
    pub async fn generate_text_from_media(
        &self,
        media_mime: &str,
        media_data: &[u8],
        prompt: &str,
    ) -> Result<String> {
        let request = build_media_text_request(media_mime, media_data, prompt);
        let url = format!(
            "{}/models/{}:generateContent",
            self.config.base_url, self.config.model
        );
        let headers = build_json_headers(&self.config.api_key)?;

        let response = self
            .http
            .post(url)
            .headers(headers)
            .json(&request)
            .send()
            .await
            .map_err(|e| classify_reqwest_error(&e))?;

        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        if !status.is_success() {
            return Err(ProviderError::http_status(status.as_u16(), &body).into());
        }

        let value: Value = serde_json::from_str(&body)
            .with_context(|| format!("Failed to parse Gemini response JSON: {body}"))?;
        let parsed = parse_image_generation_response(&value)?;
        let text = parsed.text_parts.join("\n");
        if text.trim().is_empty() {
            anyhow::bail!("Gemini returned no text for the provided media");
        }
        Ok(text)
    }
}

fn build_media_text_request(media_mime: &str, media_data: &[u8], prompt: &str) -> Value {
    let b64 = base64::engine::general_purpose::STANDARD.encode(media_data);
    json!({
        "contents": [{
            "role": "user",
            "parts": [
                {"inlineData": {"mimeType": media_mime, "data": b64}},
                {"text": prompt}
            ]
        }]
    })
}

fn build_image_generation_request(prompt: &str, options: &GeminiImageGenerationOptions) -> Value {
    let mut generation_config = json!({
        "responseModalities": ["IMAGE"],
        "maxOutputTokens": IMAGE_MAX_OUTPUT_TOKENS,
    });

    let mut image_config = serde_json::Map::new();
    if let Some(aspect_ratio) = options.aspect_ratio.as_deref()
        && !aspect_ratio.trim().is_empty()
    {
        image_config.insert("aspectRatio".to_string(), json!(aspect_ratio));
    }
    if let Some(image_size) = options.image_size.as_deref()
        && !image_size.trim().is_empty()
    {
        image_config.insert("imageSize".to_string(), json!(image_size));
    }
    if !image_config.is_empty() {
        generation_config["imageConfig"] = Value::Object(image_config);
    }

    let mut parts = vec![json!({"text": prompt})];
    for image in &options.source_images {
        let b64 = base64::engine::general_purpose::STANDARD.encode(&image.data);
        parts.push(json!({
            "inlineData": {
                "mimeType": image.mime_type,
                "data": b64
            }
        }));
    }

    json!({
        "contents": [{"role": "user", "parts": parts}],
        "generationConfig": generation_config,
    })
}

fn parse_image_generation_response(value: &Value) -> Result<GenerateImageResponse> {
    let payload = value.get("response").unwrap_or(value);
    let mut images = Vec::new();
    let mut text_parts = Vec::new();

    let candidates = payload
        .get("candidates")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();

    for candidate in candidates {
        let parts = candidate
            .get("content")
            .and_then(|content| content.get("parts"))
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();

        for part in parts {
            if let Some(text) = part.get("text").and_then(Value::as_str)
                && !text.trim().is_empty()
            {
                text_parts.push(text.to_string());
            }

            let Some(inline_data) = part.get("inlineData").or_else(|| part.get("inline_data"))
            else {
                continue;
            };

            let mime_type = inline_data
                .get("mimeType")
                .or_else(|| inline_data.get("mime_type"))
                .and_then(Value::as_str)
                .unwrap_or("image/png")
                .to_string();

            let data_b64 = inline_data
                .get("data")
                .and_then(Value::as_str)
                .context("Gemini image response is missing inlineData.data")?;

            let data = base64::engine::general_purpose::STANDARD
                .decode(data_b64)
                .with_context(|| format!("Failed to decode base64 image data ({mime_type})"))?;

            images.push(GeneratedImage { mime_type, data });
        }
    }

    Ok(GenerateImageResponse {
        images,
        text_parts,
        usage: parse_image_usage(payload),
    })
}

fn parse_image_usage(payload: &Value) -> Option<ImageUsage> {
    let metadata = payload.get("usageMetadata")?;
    let count = |key: &str| metadata.get(key).and_then(Value::as_u64).unwrap_or(0);

    let image_tokens = metadata
        .get("candidatesTokensDetails")
        .and_then(Value::as_array)
        .map_or(0, |details| {
            details
                .iter()
                .filter(|entry| entry.get("modality").and_then(Value::as_str) == Some("IMAGE"))
                .filter_map(|entry| entry.get("tokenCount").and_then(Value::as_u64))
                .sum()
        });

    Some(ImageUsage {
        prompt_tokens: count("promptTokenCount"),
        candidates_tokens: count("candidatesTokenCount"),
        thoughts_tokens: count("thoughtsTokenCount"),
        total_tokens: count("totalTokenCount"),
        image_tokens,
    })
}

fn build_headers(api_key: &str) -> anyhow::Result<HeaderMap> {
    let mut headers = HeaderMap::new();
    headers.insert(
        "x-goog-api-key",
        crate::shared::header_value("Gemini API key", api_key)?,
    );
    headers.insert("accept", HeaderValue::from_static("text/event-stream"));
    headers.insert("content-type", HeaderValue::from_static("application/json"));
    headers.insert(
        "user-agent",
        HeaderValue::from_static(crate::shared::USER_AGENT),
    );
    Ok(headers)
}

fn build_json_headers(api_key: &str) -> anyhow::Result<HeaderMap> {
    let mut headers = build_headers(api_key)?;
    headers.insert("accept", HeaderValue::from_static("application/json"));
    Ok(headers)
}

/// Constructs the Gemini API client from the given context.
///
/// # Errors
/// Returns an error if the API key / base URL cannot be resolved from env or config.
pub fn build(
    ctx: &crate::ProviderBuildContext<'_>,
) -> anyhow::Result<Box<dyn crate::StreamingProvider>> {
    Ok(Box::new(GeminiClient::new(GeminiConfig::from_env(
        ctx.model.to_string(),
        ctx.config_max_tokens,
        ctx.base_url,
        ctx.api_key,
        // Always emit a thinking config — even when ThinkingLevel::Off — so that
        // `Off` sends an explicit minimum-thinking config rather than omitting
        // `thinkingConfig` (which lets Gemini fall back to its default high reasoning).
        Some(GeminiThinkingConfig::from_thinking_level(
            ctx.thinking_level,
            ctx.model,
        )),
    )?)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_image_generation_response_extracts_images_and_text() {
        let value = json!({
            "candidates": [{
                "content": {
                    "parts": [
                        { "text": "Done." },
                        {
                            "inlineData": {
                                "mimeType": "image/png",
                                "data": "AQID"
                            }
                        }
                    ]
                }
            }]
        });

        let parsed = parse_image_generation_response(&value).expect("parse should succeed");
        assert_eq!(parsed.text_parts, vec!["Done."]);
        assert_eq!(parsed.images.len(), 1);
        assert_eq!(parsed.images[0].mime_type, "image/png");
        assert_eq!(parsed.images[0].data, vec![1, 2, 3]);
    }

    #[test]
    fn parse_image_generation_response_extracts_usage_metadata() {
        let value = json!({
            "candidates": [{ "content": { "parts": [] } }],
            "usageMetadata": {
                "promptTokenCount": 601,
                "candidatesTokenCount": 33975,
                "thoughtsTokenCount": 812,
                "totalTokenCount": 35388,
                "candidatesTokensDetails": [
                    { "modality": "TEXT", "tokenCount": 455 },
                    { "modality": "IMAGE", "tokenCount": 33520 }
                ]
            }
        });

        let usage = parse_image_generation_response(&value)
            .expect("parse should succeed")
            .usage
            .expect("usage metadata should be parsed");

        assert_eq!(usage.prompt_tokens, 601);
        assert_eq!(usage.candidates_tokens, 33975);
        assert_eq!(usage.thoughts_tokens, 812);
        assert_eq!(usage.total_tokens, 35388);
        assert_eq!(usage.image_tokens, 33520);
    }

    #[test]
    fn build_image_generation_request_sets_image_config_when_present() {
        let request = build_image_generation_request(
            "A red fox",
            &GeminiImageGenerationOptions {
                aspect_ratio: Some("16:9".to_string()),
                image_size: Some("2K".to_string()),
                source_images: vec![],
            },
        );

        assert_eq!(
            request["generationConfig"]["responseModalities"],
            json!(["IMAGE"])
        );
        assert_eq!(
            request["generationConfig"]["maxOutputTokens"],
            json!(IMAGE_MAX_OUTPUT_TOKENS)
        );
        assert_eq!(
            request["generationConfig"]["imageConfig"]["aspectRatio"],
            json!("16:9")
        );
        assert_eq!(
            request["generationConfig"]["imageConfig"]["imageSize"],
            json!("2K")
        );
    }

    #[test]
    fn build_image_generation_request_includes_source_images() {
        let request = build_image_generation_request(
            "Make the sky purple",
            &GeminiImageGenerationOptions {
                aspect_ratio: None,
                image_size: None,
                source_images: vec![
                    SourceImage {
                        mime_type: "image/png".to_string(),
                        data: vec![1, 2, 3],
                    },
                    SourceImage {
                        mime_type: "image/jpeg".to_string(),
                        data: vec![4, 5],
                    },
                ],
            },
        );

        // Editing keeps IMAGE-only output: TEXT modality lets thinking image
        // models emit interleaved commentary and extra billable image parts.
        assert_eq!(
            request["generationConfig"]["responseModalities"],
            json!(["IMAGE"])
        );

        let parts = request["contents"][0]["parts"].as_array().unwrap();
        assert_eq!(parts.len(), 3);
        assert_eq!(parts[0]["text"], "Make the sky purple");
        assert_eq!(parts[1]["inlineData"]["mimeType"], "image/png");
        assert_eq!(
            parts[1]["inlineData"]["data"],
            base64::engine::general_purpose::STANDARD.encode([1, 2, 3])
        );
        assert_eq!(parts[2]["inlineData"]["mimeType"], "image/jpeg");
        assert_eq!(
            parts[2]["inlineData"]["data"],
            base64::engine::general_purpose::STANDARD.encode([4, 5])
        );
    }
}
