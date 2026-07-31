//! Alibaba `DashScope` image generation + editing (Qwen-Image).
//!
//! One synchronous endpoint (`/services/aigc/multimodal-generation/generation`)
//! serves both text-to-image (content = `[text]`) and instruction-based editing
//! (content = `[image..., text]`). Source images are sent as base64 `data:` URIs;
//! results come back as image URLs, which we download to bytes.
//!
//! This uses the `DashScope` native `/api/v1` base URL — NOT the chat provider's
//! `/compatible-mode/v1` — and borrows the `ALIBABA_API_KEY` from the Alibaba
//! chat provider.

use anyhow::{Context, Result};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use reqwest::header::{HeaderMap, HeaderValue};
use serde_json::{Value, json};

use crate::ProviderKind;
use crate::shared::{USER_AGENT, classify_reqwest_error, header_value};

/// Default `DashScope` native API base (international).
const DEFAULT_IMAGE_BASE_URL: &str = "https://dashscope-intl.aliyuncs.com/api/v1";
const IMAGE_BASE_URL_ENV: &str = "ALIBABA_IMAGE_BASE_URL";

/// A source image for editing / multi-image fusion.
#[derive(Debug, Clone)]
pub struct AlibabaImageInput {
    pub mime_type: String,
    pub data: Vec<u8>,
}

/// Optional image generation/editing settings.
#[derive(Debug, Clone, Default)]
pub struct AlibabaImageGenerationOptions {
    /// Output size as `WIDTH*HEIGHT` (e.g. `"1328*1328"`). None → model default.
    pub size: Option<String>,
    /// Number of output images (1–6). None → model default.
    pub n: Option<u32>,
    /// Source images for editing / fusion (1–3). Empty → text-to-image.
    pub source_images: Vec<AlibabaImageInput>,
    pub negative_prompt: Option<String>,
    pub prompt_extend: Option<bool>,
}

/// A generated image (downloaded bytes).
#[derive(Debug, Clone)]
pub struct AlibabaGeneratedImage {
    pub mime_type: String,
    pub data: Vec<u8>,
}

/// Parsed response from a `DashScope` image request.
#[derive(Debug, Clone, Default)]
pub struct AlibabaGenerateImageResponse {
    pub images: Vec<AlibabaGeneratedImage>,
    pub text_parts: Vec<String>,
}

/// `DashScope` image client (Qwen-Image generate + edit).
pub struct AlibabaImageClient {
    api_key: String,
    base_url: String,
    model: String,
    http: reqwest::Client,
}

impl AlibabaImageClient {
    /// Builds a client, resolving the API key from config/env (`ALIBABA_API_KEY`)
    /// and the image base URL from `ALIBABA_IMAGE_BASE_URL` or the default.
    ///
    /// # Errors
    /// Returns an error if the API key cannot be resolved.
    pub fn from_env(model: String, config_api_key: Option<&str>) -> Result<Self> {
        let api_key = ProviderKind::Alibaba.resolve_api_key(config_api_key)?;
        let base_url = std::env::var(IMAGE_BASE_URL_ENV)
            .ok()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| DEFAULT_IMAGE_BASE_URL.to_string());
        Ok(Self {
            api_key,
            base_url,
            model,
            http: reqwest::Client::new(),
        })
    }

    /// Generates images, or edits when `source_images` are present.
    ///
    /// # Errors
    /// Returns an error if the request fails, the response cannot be parsed, or
    /// a result image URL cannot be downloaded.
    pub async fn generate_images(
        &self,
        prompt: &str,
        options: &AlibabaImageGenerationOptions,
    ) -> Result<AlibabaGenerateImageResponse> {
        let request = build_request(&self.model, prompt, options);
        let url = format!(
            "{}/services/aigc/multimodal-generation/generation",
            self.base_url
        );
        let headers = build_headers(&self.api_key)?;
        crate::shared::log_request("alibaba-image", &url);

        let response = self
            .http
            .post(url)
            .headers(headers)
            .json(&request)
            .send()
            .await
            .map_err(|e| classify_reqwest_error(&e))?;

        let body = crate::shared::check_response_status("alibaba-image", response)
            .await?
            .text()
            .await
            .unwrap_or_default();

        let value: Value = serde_json::from_str(&body)
            .with_context(|| format!("Failed to parse DashScope image response JSON: {body}"))?;
        let parsed = parse_response(&value);

        let mut images = Vec::with_capacity(parsed.image_urls.len());
        for image_url in &parsed.image_urls {
            let bytes = self
                .http
                .get(image_url)
                .header("user-agent", USER_AGENT)
                .send()
                .await
                .map_err(|e| classify_reqwest_error(&e))?
                .error_for_status()
                .with_context(|| format!("download generated image from {image_url}"))?
                .bytes()
                .await
                .with_context(|| format!("read generated image bytes from {image_url}"))?;
            images.push(AlibabaGeneratedImage {
                mime_type: mime_from_url(image_url),
                data: bytes.to_vec(),
            });
        }

        Ok(AlibabaGenerateImageResponse {
            images,
            text_parts: parsed.text_parts,
        })
    }
}

fn build_headers(api_key: &str) -> Result<HeaderMap> {
    let mut headers = HeaderMap::new();
    headers.insert(
        "Authorization",
        header_value("Alibaba API key", &format!("Bearer {api_key}"))?,
    );
    headers.insert("content-type", HeaderValue::from_static("application/json"));
    headers.insert("user-agent", HeaderValue::from_static(USER_AGENT));
    Ok(headers)
}

/// Builds the multimodal-generation request body. Source images are prepended
/// to `content` as base64 `data:` URIs, followed by the text instruction.
fn build_request(model: &str, prompt: &str, options: &AlibabaImageGenerationOptions) -> Value {
    let mut content: Vec<Value> = Vec::with_capacity(options.source_images.len() + 1);
    for image in &options.source_images {
        let b64 = STANDARD.encode(&image.data);
        content.push(json!({ "image": format!("data:{};base64,{}", image.mime_type, b64) }));
    }
    content.push(json!({ "text": prompt }));

    let mut parameters = serde_json::Map::new();
    if let Some(size) = &options.size {
        parameters.insert("size".to_string(), json!(size));
    }
    if let Some(n) = options.n {
        parameters.insert("n".to_string(), json!(n));
    }
    if let Some(neg) = &options.negative_prompt {
        parameters.insert("negative_prompt".to_string(), json!(neg));
    }
    if let Some(extend) = options.prompt_extend {
        parameters.insert("prompt_extend".to_string(), json!(extend));
    }

    json!({
        "model": model,
        "input": { "messages": [ { "role": "user", "content": content } ] },
        "parameters": Value::Object(parameters),
    })
}

struct ParsedResponse {
    image_urls: Vec<String>,
    text_parts: Vec<String>,
}

/// Extracts image URLs and text from `output.choices[].message.content[]`.
fn parse_response(value: &Value) -> ParsedResponse {
    let mut image_urls = Vec::new();
    let mut text_parts = Vec::new();

    if let Some(choices) = value
        .get("output")
        .and_then(|o| o.get("choices"))
        .and_then(Value::as_array)
    {
        for choice in choices {
            let Some(content) = choice
                .get("message")
                .and_then(|m| m.get("content"))
                .and_then(Value::as_array)
            else {
                continue;
            };
            for part in content {
                if let Some(image) = part.get("image").and_then(Value::as_str) {
                    image_urls.push(image.to_string());
                }
                if let Some(text) = part.get("text").and_then(Value::as_str)
                    && !text.is_empty()
                {
                    text_parts.push(text.to_string());
                }
            }
        }
    }

    ParsedResponse {
        image_urls,
        text_parts,
    }
}

fn mime_from_url(url: &str) -> String {
    let path = url.split('?').next().unwrap_or(url);
    let ext = path.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
    match ext.as_str() {
        "jpg" | "jpeg" => "image/jpeg",
        "webp" => "image/webp",
        "bmp" => "image/bmp",
        "tiff" => "image/tiff",
        "gif" => "image/gif",
        _ => "image/png",
    }
    .to_string()
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{
        AlibabaImageGenerationOptions, AlibabaImageInput, build_request, mime_from_url,
        parse_response,
    };

    #[test]
    fn build_request_text_to_image_has_only_text_content() {
        let req = build_request(
            "qwen-image-2.0-pro",
            "a red bicycle",
            &AlibabaImageGenerationOptions {
                size: Some("1024*1024".to_string()),
                ..Default::default()
            },
        );
        let content = &req["input"]["messages"][0]["content"];
        assert_eq!(content.as_array().unwrap().len(), 1);
        assert_eq!(content[0]["text"], json!("a red bicycle"));
        assert_eq!(req["parameters"]["size"], json!("1024*1024"));
        assert_eq!(req["model"], json!("qwen-image-2.0-pro"));
    }

    #[test]
    fn build_request_edit_prepends_source_images_as_data_uris() {
        let req = build_request(
            "qwen-image-edit-plus",
            "make it blue",
            &AlibabaImageGenerationOptions {
                source_images: vec![AlibabaImageInput {
                    mime_type: "image/png".to_string(),
                    data: vec![1, 2, 3],
                }],
                ..Default::default()
            },
        );
        let content = &req["input"]["messages"][0]["content"];
        let arr = content.as_array().unwrap();
        assert_eq!(arr.len(), 2);
        let image = arr[0]["image"].as_str().unwrap();
        assert!(image.starts_with("data:image/png;base64,"));
        assert_eq!(arr[1]["text"], json!("make it blue"));
    }

    #[test]
    fn parse_response_extracts_image_urls_and_text() {
        let value = json!({
            "output": {
                "choices": [
                    { "message": { "content": [
                        { "image": "https://example.com/out.png" },
                        { "text": "done" }
                    ] } }
                ]
            }
        });
        let parsed = parse_response(&value);
        assert_eq!(parsed.image_urls, vec!["https://example.com/out.png"]);
        assert_eq!(parsed.text_parts, vec!["done"]);
    }

    #[test]
    fn mime_from_url_infers_extension_ignoring_query() {
        assert_eq!(mime_from_url("https://x/y.jpg?a=1"), "image/jpeg");
        assert_eq!(mime_from_url("https://x/y.webp"), "image/webp");
        assert_eq!(mime_from_url("https://x/y"), "image/png");
    }
}
