//! Gemini API key provider (Generative Language API).

use anyhow::{Context, Result};
use base64::Engine;
use reqwest::header::{HeaderMap, HeaderValue};
use serde_json::{Value, json};

use super::shared::{GeminiThinkingConfig, build_gemini_request, classify_reqwest_error};
use super::sse::GeminiSseParser;
use crate::providers::debug_metrics::maybe_wrap_with_metrics;
use crate::providers::shared::{merge_system_prompt, resolve_api_key, resolve_base_url};
use crate::providers::{ChatMessage, DebugTrace, ProviderError, ProviderStream, wrap_stream};
use crate::tools::ToolDefinition;

const DEFAULT_BASE_URL: &str = "https://generativelanguage.googleapis.com/v1beta";

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
        let api_key = resolve_api_key(config_api_key, "GEMINI_API_KEY", "gemini")?;
        let base_url = resolve_base_url(
            config_base_url,
            "GEMINI_BASE_URL",
            DEFAULT_BASE_URL,
            "Gemini",
        )?;

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

/// Optional image generation settings.
#[derive(Debug, Clone, Default)]
pub struct GeminiImageGenerationOptions {
    /// Output image aspect ratio (e.g. "1:1", "16:9").
    pub aspect_ratio: Option<String>,
    /// Output image size preset (e.g. "1K", "2K", "4K").
    pub image_size: Option<String>,
}

/// A generated image from Gemini image models.
#[derive(Debug, Clone)]
pub struct GeneratedImage {
    pub mime_type: String,
    pub data: Vec<u8>,
}

/// Parsed response from a Gemini image generation request.
#[derive(Debug, Clone, Default)]
pub struct GenerateImageResponse {
    pub images: Vec<GeneratedImage>,
    pub text_parts: Vec<String>,
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
        )?;
        let trace = DebugTrace::from_env(&self.config.model, None);
        let url = format!(
            "{}/models/{}:streamGenerateContent?alt=sse",
            self.config.base_url, self.config.model
        );
        let headers = build_headers(&self.config.api_key);

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
        let headers = build_json_headers(&self.config.api_key);

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
}

fn build_image_generation_request(prompt: &str, options: &GeminiImageGenerationOptions) -> Value {
    let mut generation_config = json!({
        "responseModalities": ["IMAGE"]
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

    json!({
        "contents": [{
            "role": "user",
            "parts": [{
                "text": prompt
            }]
        }],
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

    Ok(GenerateImageResponse { images, text_parts })
}

fn build_headers(api_key: &str) -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert(
        "x-goog-api-key",
        HeaderValue::from_str(api_key).unwrap_or_else(|_| HeaderValue::from_static("")),
    );
    headers.insert("accept", HeaderValue::from_static("text/event-stream"));
    headers.insert("content-type", HeaderValue::from_static("application/json"));
    headers.insert(
        "user-agent",
        HeaderValue::from_static(crate::providers::shared::USER_AGENT),
    );
    headers
}

fn build_json_headers(api_key: &str) -> HeaderMap {
    let mut headers = build_headers(api_key);
    headers.insert("accept", HeaderValue::from_static("application/json"));
    headers
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
    fn build_image_generation_request_sets_image_config_when_present() {
        let request = build_image_generation_request(
            "A red fox",
            &GeminiImageGenerationOptions {
                aspect_ratio: Some("16:9".to_string()),
                image_size: Some("2K".to_string()),
            },
        );

        assert_eq!(
            request["generationConfig"]["responseModalities"],
            json!(["IMAGE"])
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
}
