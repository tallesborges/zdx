//! `OpenAI` Images API client (`/images/generations` and `/images/edits`).
//!
//! Two backends share one request body and one parser:
//! - API key (`openai:`): `https://api.openai.com/v1`, the only route for GPT
//!   Image 2.6 (`gpt-image-2.6-flare`, `gpt-image-2.6-sunburst`) and its
//!   controls (format, quality, complexity, GIF frames, markup).
//! - `ChatGPT` subscription (`openai-codex:`): `https://chatgpt.com/backend-api/codex`,
//!   the same paths Codex CLI uses, with OAuth bearer + account id headers.
//!
//! One request produces one image: a PNG, JPEG, or WebP raster, an animated
//! GIF, or an SVG document.

use anyhow::{Context, Result, bail};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use reqwest::header::{HeaderMap, HeaderValue};
use serde_json::{Map, Value, json};

use crate::ProviderKind;
use crate::shared::classify_reqwest_error;

const GENERATIONS_PATH: &str = "/images/generations";
const EDITS_PATH: &str = "/images/edits";
/// Path under the `ChatGPT` backend base URL that serves the Codex Images endpoints.
const CODEX_IMAGES_PREFIX: &str = "/codex";

/// A source image (or markup layer) sent as a base64 data URL.
#[derive(Debug, Clone)]
pub struct OpenAIImageInput {
    pub mime_type: String,
    pub data: Vec<u8>,
}

/// A decoded image from an Images API response.
#[derive(Debug, Clone)]
pub struct OpenAIGeneratedImage {
    pub mime_type: String,
    pub data: Vec<u8>,
}

/// Output format accepted by the Images API `output_format` field.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpenAIImageFormat {
    Png,
    Jpeg,
    Webp,
    Gif,
    Svg,
}

impl OpenAIImageFormat {
    /// Parses an API format name (`png`, `jpeg`, `webp`, `gif`, `svg`).
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "png" => Some(Self::Png),
            "jpeg" => Some(Self::Jpeg),
            "webp" => Some(Self::Webp),
            "gif" => Some(Self::Gif),
            "svg" => Some(Self::Svg),
            _ => None,
        }
    }

    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Png => "png",
            Self::Jpeg => "jpeg",
            Self::Webp => "webp",
            Self::Gif => "gif",
            Self::Svg => "svg",
        }
    }

    #[must_use]
    pub fn mime_type(self) -> &'static str {
        match self {
            Self::Png => "image/png",
            Self::Jpeg => "image/jpeg",
            Self::Webp => "image/webp",
            Self::Gif => "image/gif",
            Self::Svg => "image/svg+xml",
        }
    }
}

/// Settings for one Images API request. Source images turn it into an edit.
#[derive(Debug, Clone, Default)]
pub struct OpenAIImagesRequest {
    /// `auto` or `WIDTHxHEIGHT`; omitted lets the API choose.
    pub size: Option<String>,
    /// `low`, `medium`, `high`, `xhigh`, `max`, or `auto`.
    pub quality: Option<String>,
    /// `transparent`, `opaque`, or `auto`.
    pub background: Option<String>,
    /// Planning effort: `low`, `medium`, or `high` (API default `high`).
    pub complexity: Option<String>,
    pub output_format: Option<OpenAIImageFormat>,
    /// GIF only.
    pub frame_count: Option<u32>,
    /// GIF only.
    pub frame_duration_ms: Option<u32>,
    /// Images to edit. The first one is the primary image; the rest are references.
    pub source_images: Vec<OpenAIImageInput>,
    /// Transparent PNG annotation layer applied to the first source image.
    pub markup: Option<OpenAIImageInput>,
}

/// Token usage reported by the Images API.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OpenAIImageUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub total_tokens: u64,
}

/// Token usage of the model that authored an SVG, billed separately.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OpenAISvgUsage {
    pub model: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub reasoning_tokens: u64,
}

/// Parsed Images API response.
#[derive(Debug, Clone, Default)]
pub struct OpenAIImagesResponse {
    pub images: Vec<OpenAIGeneratedImage>,
    pub usage: Option<OpenAIImageUsage>,
    pub svg_usage: Option<OpenAISvgUsage>,
}

enum ImagesAuth {
    ApiKey { api_key: String, base_url: String },
    Codex { account: Option<String> },
}

/// `OpenAI` Images API client.
pub struct OpenAIImagesClient {
    auth: ImagesAuth,
    model: String,
    http: reqwest::Client,
}

impl OpenAIImagesClient {
    /// API-key client. Resolves the key (config, then `OPENAI_API_KEY`) and
    /// base URL (config, then `OPENAI_BASE_URL`, then the default).
    ///
    /// # Errors
    /// Returns an error if no API key is available or the base URL is invalid.
    pub fn from_env(
        model: String,
        config_base_url: Option<&str>,
        config_api_key: Option<&str>,
    ) -> Result<Self> {
        Ok(Self {
            auth: ImagesAuth::ApiKey {
                api_key: ProviderKind::OpenAI.resolve_api_key(config_api_key)?,
                base_url: ProviderKind::OpenAI.resolve_base_url(config_base_url)?,
            },
            model,
            http: crate::shared::streaming_http_client(),
        })
    }

    /// `ChatGPT` subscription client for a named OAuth account (`None` =
    /// default). Credentials are resolved, and refreshed if expired, per request.
    #[must_use]
    pub fn codex(model: String, account: Option<String>) -> Self {
        Self {
            auth: ImagesAuth::Codex { account },
            model,
            http: crate::shared::streaming_http_client(),
        }
    }

    /// Generates one image, or edits the source images when any are given.
    ///
    /// # Errors
    /// Returns an error if the request is invalid, fails, or the response
    /// cannot be parsed.
    pub async fn generate(
        &self,
        prompt: &str,
        request: &OpenAIImagesRequest,
    ) -> Result<OpenAIImagesResponse> {
        let (path, body) = build_images_request(&self.model, prompt, request)?;
        let (client_label, url, headers) = match &self.auth {
            ImagesAuth::ApiKey { api_key, base_url } => (
                "openai-image",
                format!("{base_url}{path}"),
                api_key_headers(api_key)?,
            ),
            ImagesAuth::Codex { account } => {
                let creds = super::codex::resolve_credentials(account.as_deref()).await?;
                (
                    "codex-image",
                    codex_images_url(path),
                    super::codex::build_json_headers(&creds.account_id, &creds.access)?,
                )
            }
        };
        crate::shared::log_request(client_label, &url);
        let response = self
            .http
            .post(url)
            .headers(headers)
            .json(&body)
            .send()
            .await
            .map_err(|e| classify_reqwest_error(&e))?;
        let value: Value = crate::shared::check_response_status(client_label, response)
            .await?
            .json()
            .await
            .context("Failed to parse OpenAI Images API response JSON")?;
        parse_images_response(&value)
    }
}

fn codex_images_url(path: &str) -> String {
    format!(
        "{}{CODEX_IMAGES_PREFIX}{path}",
        ProviderKind::OpenAICodex.default_base_url()
    )
}

fn api_key_headers(api_key: &str) -> Result<HeaderMap> {
    let mut headers = HeaderMap::new();
    headers.insert(
        "Authorization",
        crate::shared::header_value("OpenAI API key", &format!("Bearer {api_key}"))?,
    );
    headers.insert("content-type", HeaderValue::from_static("application/json"));
    headers.insert(
        "user-agent",
        HeaderValue::from_static(crate::shared::USER_AGENT),
    );
    Ok(headers)
}

fn data_url(image: &OpenAIImageInput) -> Value {
    json!({
        "image_url": format!("data:{};base64,{}", image.mime_type, STANDARD.encode(&image.data)),
    })
}

/// Builds the endpoint path and JSON body. Rejects combinations every Images
/// API source agrees are invalid; other limits are left to the API.
pub(super) fn build_images_request(
    model: &str,
    prompt: &str,
    request: &OpenAIImagesRequest,
) -> Result<(&'static str, Value)> {
    let is_gif = request.output_format == Some(OpenAIImageFormat::Gif);
    let is_edit = !request.source_images.is_empty();
    if !is_gif && (request.frame_count.is_some() || request.frame_duration_ms.is_some()) {
        bail!("GIF frame settings require output format gif");
    }
    if is_edit && request.output_format == Some(OpenAIImageFormat::Svg) {
        bail!("SVG output is available for generation only, not for edits with source images");
    }
    if request.markup.is_some() && !is_edit {
        bail!("Markup needs a source image to annotate");
    }

    let mut body = Map::new();
    body.insert("model".to_string(), json!(model));
    body.insert("prompt".to_string(), json!(prompt));
    let optional = [
        ("size", request.size.as_deref()),
        ("quality", request.quality.as_deref()),
        ("background", request.background.as_deref()),
        ("complexity", request.complexity.as_deref()),
        (
            "output_format",
            request.output_format.map(OpenAIImageFormat::as_str),
        ),
    ];
    for (key, value) in optional {
        if let Some(value) = value.filter(|value| !value.trim().is_empty()) {
            body.insert(key.to_string(), json!(value));
        }
    }
    if let Some(frame_count) = request.frame_count {
        body.insert("frame_count".to_string(), json!(frame_count));
    }
    if let Some(frame_duration_ms) = request.frame_duration_ms {
        body.insert("frame_duration_ms".to_string(), json!(frame_duration_ms));
    }

    if !is_edit {
        return Ok((GENERATIONS_PATH, Value::Object(body)));
    }
    body.insert(
        "images".to_string(),
        Value::Array(request.source_images.iter().map(data_url).collect()),
    );
    if let Some(markup) = &request.markup {
        body.insert("markup".to_string(), data_url(markup));
    }
    Ok((EDITS_PATH, Value::Object(body)))
}

/// MIME type for decoded bytes that match no known image format.
const UNKNOWN_MIME: &str = "application/octet-stream";

/// Decodes `data[].b64_json`. Each image's MIME type comes from its bytes; the
/// response `output_format` is used only when the bytes match no known format.
/// The requested format is never assumed: backends can ignore it silently.
pub(super) fn parse_images_response(value: &Value) -> Result<OpenAIImagesResponse> {
    let reported = value
        .get("output_format")
        .and_then(Value::as_str)
        .and_then(OpenAIImageFormat::parse);

    let images = value
        .get("data")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default()
        .iter()
        .filter_map(|item| item.get("b64_json").and_then(Value::as_str))
        .map(|data_b64| {
            let data = STANDARD
                .decode(data_b64)
                .context("Failed to decode OpenAI Images API base64 data")?;
            let mime_type = detect_image_mime(&data)
                .or_else(|| reported.map(OpenAIImageFormat::mime_type))
                .unwrap_or(UNKNOWN_MIME);
            Ok(OpenAIGeneratedImage {
                mime_type: mime_type.to_string(),
                data,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    if images.is_empty() {
        bail!("OpenAI Images API response contained no images");
    }

    let token = |object: &Value, key: &str| object.get(key).and_then(Value::as_u64).unwrap_or(0);
    let usage = value.get("usage").map(|usage| OpenAIImageUsage {
        input_tokens: token(usage, "input_tokens"),
        output_tokens: token(usage, "output_tokens"),
        total_tokens: token(usage, "total_tokens"),
    });
    let svg_usage = value.get("svg_usage").map(|usage| OpenAISvgUsage {
        model: usage
            .get("model")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        input_tokens: token(usage, "input_tokens"),
        output_tokens: token(usage, "output_tokens"),
        reasoning_tokens: token(usage, "reasoning_tokens"),
    });

    Ok(OpenAIImagesResponse {
        images,
        usage,
        svg_usage,
    })
}

/// Raster signatures first, then an SVG document.
fn detect_image_mime(data: &[u8]) -> Option<&'static str> {
    zdx_types::sniff_raster_image_mime(data)
        .or_else(|| is_svg_document(data).then_some("image/svg+xml"))
}

/// True when the root element is `<svg>`. Skips an optional BOM, whitespace,
/// the XML declaration, processing instructions, comments, and a DOCTYPE.
fn is_svg_document(data: &[u8]) -> bool {
    let mut rest = data.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(data);
    loop {
        rest = rest.trim_ascii_start();
        let skip_to = if rest.starts_with(b"<?") {
            find(rest, b"?>").map(|end| end + 2)
        } else if rest.starts_with(b"<!--") {
            find(rest, b"-->").map(|end| end + 3)
        } else if rest.starts_with(b"<!") {
            find(rest, b">").map(|end| end + 1)
        } else {
            break;
        };
        let Some(skip_to) = skip_to else {
            return false;
        };
        rest = &rest[skip_to..];
    }
    rest.strip_prefix(b"<svg").is_some_and(|after| {
        after
            .first()
            .is_some_and(|byte| byte.is_ascii_whitespace() || matches!(byte, b'>' | b'/'))
    })
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{
        OpenAIImageFormat, OpenAIImageInput, OpenAIImagesRequest, build_images_request,
        codex_images_url, parse_images_response,
    };

    const PNG_BYTES: &[u8] = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR";

    fn png(data: &[u8]) -> OpenAIImageInput {
        OpenAIImageInput {
            mime_type: "image/png".to_string(),
            data: data.to_vec(),
        }
    }

    #[test]
    fn generation_request_carries_only_set_fields() {
        let (path, body) = build_images_request(
            "gpt-image-2.6-flare",
            "A red fox",
            &OpenAIImagesRequest {
                quality: Some("low".to_string()),
                complexity: Some("medium".to_string()),
                ..Default::default()
            },
        )
        .unwrap();

        assert_eq!(path, "/images/generations");
        assert_eq!(
            body,
            json!({
                "model": "gpt-image-2.6-flare",
                "prompt": "A red fox",
                "quality": "low",
                "complexity": "medium",
            })
        );
    }

    #[test]
    fn gif_request_sends_frame_settings() {
        let (path, body) = build_images_request(
            "gpt-image-2.6-sunburst",
            "A waving owl",
            &OpenAIImagesRequest {
                size: Some("1024x1024".to_string()),
                output_format: Some(OpenAIImageFormat::Gif),
                frame_count: Some(6),
                frame_duration_ms: Some(120),
                ..Default::default()
            },
        )
        .unwrap();

        assert_eq!(path, "/images/generations");
        assert_eq!(body["output_format"], json!("gif"));
        assert_eq!(body["frame_count"], json!(6));
        assert_eq!(body["frame_duration_ms"], json!(120));
        assert_eq!(body["size"], json!("1024x1024"));
        assert!(body.get("stream").is_none());
    }

    #[test]
    fn frame_settings_without_gif_are_rejected() {
        let err = build_images_request(
            "gpt-image-2.6-flare",
            "x",
            &OpenAIImagesRequest {
                output_format: Some(OpenAIImageFormat::Png),
                frame_count: Some(4),
                ..Default::default()
            },
        )
        .unwrap_err();
        assert!(err.to_string().contains("require output format gif"));
    }

    #[test]
    fn edit_request_uses_images_and_markup_data_urls() {
        let (path, body) = build_images_request(
            "gpt-image-2.6-flare",
            "Recolor the circled vase blue",
            &OpenAIImagesRequest {
                source_images: vec![png(&[1, 2, 3]), png(&[4])],
                markup: Some(png(&[9])),
                ..Default::default()
            },
        )
        .unwrap();

        assert_eq!(path, "/images/edits");
        assert_eq!(
            body["images"],
            json!([
                {"image_url": "data:image/png;base64,AQID"},
                {"image_url": "data:image/png;base64,BA=="},
            ])
        );
        assert_eq!(
            body["markup"],
            json!({"image_url": "data:image/png;base64,CQ=="})
        );
    }

    #[test]
    fn svg_edit_and_markup_without_source_are_rejected() {
        let svg_edit = build_images_request(
            "gpt-image-2.6-flare",
            "x",
            &OpenAIImagesRequest {
                output_format: Some(OpenAIImageFormat::Svg),
                source_images: vec![png(&[1])],
                ..Default::default()
            },
        )
        .unwrap_err();
        assert!(svg_edit.to_string().contains("generation only"));

        let markup_only = build_images_request(
            "gpt-image-2.6-flare",
            "x",
            &OpenAIImagesRequest {
                markup: Some(png(&[1])),
                ..Default::default()
            },
        )
        .unwrap_err();
        assert!(markup_only.to_string().contains("source image"));
    }

    #[test]
    fn svg_response_reports_image_and_author_usage() {
        let svg = br#"<svg xmlns="http://www.w3.org/2000/svg"/>"#;
        let encoded = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, svg);
        let value = json!({
            "data": [{"b64_json": encoded}],
            "output_format": "svg",
            "usage": {"input_tokens": 10, "output_tokens": 20, "total_tokens": 30},
            "svg_usage": {
                "model": "svg-author",
                "input_tokens": 5,
                "output_tokens": 7,
                "reasoning_tokens": 2,
                "total_tokens": 12
            }
        });

        let parsed = parse_images_response(&value).unwrap();
        assert_eq!(parsed.images.len(), 1);
        assert_eq!(parsed.images[0].mime_type, "image/svg+xml");
        assert_eq!(parsed.images[0].data, svg.to_vec());
        assert_eq!(parsed.usage.unwrap().total_tokens, 30);
        let svg_usage = parsed.svg_usage.unwrap();
        assert_eq!(svg_usage.model, "svg-author");
        assert_eq!(svg_usage.reasoning_tokens, 2);
    }

    fn b64(data: &[u8]) -> String {
        base64::Engine::encode(&base64::engine::general_purpose::STANDARD, data)
    }

    /// A backend that silently ignores the requested GIF or SVG output still
    /// returns PNG bytes; the file must be labeled PNG, whatever was asked for
    /// or reported.
    #[test]
    fn bytes_win_over_requested_and_reported_format() {
        for reported in ["gif", "svg"] {
            let value = json!({
                "data": [{"b64_json": b64(PNG_BYTES)}],
                "output_format": reported,
            });
            let parsed = parse_images_response(&value).unwrap();
            assert_eq!(
                parsed.images[0].mime_type, "image/png",
                "reported {reported}"
            );
        }
        let value = json!({"data": [{"b64_json": b64(PNG_BYTES)}]});
        assert_eq!(
            parse_images_response(&value).unwrap().images[0].mime_type,
            "image/png"
        );
    }

    #[test]
    fn detects_gif_jpeg_webp_and_svg_bytes() {
        for (bytes, mime) in [
            (&b"GIF89a\x01\0"[..], "image/gif"),
            (&[0xFF, 0xD8, 0xFF, 0xE0][..], "image/jpeg"),
            (&b"RIFF\0\0\0\0WEBPVP8 "[..], "image/webp"),
            (
                &b"\xEF\xBB\xBF\n<?xml version=\"1.0\"?>\n<svg xmlns=\"http://www.w3.org/2000/svg\"/>"[..],
                "image/svg+xml",
            ),
        ] {
            let value = json!({"data": [{"b64_json": b64(bytes)}], "output_format": "png"});
            assert_eq!(parse_images_response(&value).unwrap().images[0].mime_type, mime);
        }
    }

    #[test]
    fn svg_detection_checks_the_root_element() {
        let long_comment = format!("<!-- {} -->\n<svg viewBox=\"0 0 1 1\"/>", "x".repeat(8000));
        for (bytes, is_svg) in [
            (&b"<svg>"[..], true),
            (
                b"<!DOCTYPE svg PUBLIC \"-//W3C//DTD SVG 1.1//EN\" \"x\"><svg/>",
                true,
            ),
            (long_comment.as_bytes(), true),
            (b"<svgz/>", false),
            (b"<!-- <svg/> --><error/>", false),
            (b"<html><svg/></html>", false),
            (b"<!-- unterminated <svg/>", false),
        ] {
            // No reported format, so the detected type decides alone.
            let value = json!({"data": [{"b64_json": b64(bytes)}]});
            let mime = parse_images_response(&value).unwrap().images[0]
                .mime_type
                .clone();
            let expected = if is_svg {
                "image/svg+xml"
            } else {
                "application/octet-stream"
            };
            assert_eq!(
                mime,
                expected,
                "{}",
                String::from_utf8_lossy(&bytes[..bytes.len().min(40)])
            );
        }
    }

    #[test]
    fn unrecognized_bytes_use_reported_format_then_unknown() {
        let value = json!({"data": [{"b64_json": b64(b"opaque")}], "output_format": "webp"});
        assert_eq!(
            parse_images_response(&value).unwrap().images[0].mime_type,
            "image/webp"
        );
        let value = json!({"data": [{"b64_json": b64(b"opaque")}]});
        assert_eq!(
            parse_images_response(&value).unwrap().images[0].mime_type,
            "application/octet-stream"
        );
    }

    #[test]
    fn response_without_images_is_an_error() {
        let err = parse_images_response(&json!({"data": []})).unwrap_err();
        assert!(err.to_string().contains("no images"));
    }

    #[test]
    fn codex_urls_use_the_codex_backend_prefix() {
        assert_eq!(
            codex_images_url("/images/generations"),
            "https://chatgpt.com/backend-api/codex/images/generations"
        );
        assert_eq!(
            codex_images_url("/images/edits"),
            "https://chatgpt.com/backend-api/codex/images/edits"
        );
    }

    /// The body the live subscription probe sent (2026-11-05) and got a PNG for.
    #[test]
    fn codex_baseline_request_matches_the_probed_body() {
        let (path, body) = build_images_request(
            "gpt-image-2",
            "A red fox",
            &OpenAIImagesRequest {
                size: Some("auto".to_string()),
                quality: Some("auto".to_string()),
                background: Some("opaque".to_string()),
                ..Default::default()
            },
        )
        .unwrap();

        assert_eq!(path, "/images/generations");
        assert_eq!(
            body,
            json!({
                "model": "gpt-image-2",
                "prompt": "A red fox",
                "size": "auto",
                "quality": "auto",
                "background": "opaque",
            })
        );
    }

    #[test]
    fn codex_json_headers_carry_subscription_auth() {
        let headers = crate::openai::codex::build_json_headers("acct-1", "token-1").unwrap();
        assert_eq!(headers["authorization"], "Bearer token-1");
        assert_eq!(headers["chatgpt-account-id"], "acct-1");
        assert_eq!(headers["originator"], "zdx");
        assert_eq!(headers["accept"], "application/json");
        assert_eq!(headers["content-type"], "application/json");
        assert!(headers.get("session_id").is_none());
    }

    #[test]
    fn codex_probe_response_parses_as_png() {
        let value = json!({
            "created": 1,
            "background": "opaque",
            "data": [{"b64_json": "iVBORw0KGgo=", "generation_id": "gen-1"}],
            "output_format": "png",
            "quality": "low",
            "size": "1536x1024",
            "usage": {"input_tokens": 20, "output_tokens": 343, "total_tokens": 363}
        });
        let parsed = parse_images_response(&value).unwrap();
        assert_eq!(parsed.images[0].mime_type, "image/png");
        assert_eq!(parsed.images[0].data[..4], [0x89, b'P', b'N', b'G']);
        assert_eq!(parsed.usage.unwrap().output_tokens, 343);
    }
}
