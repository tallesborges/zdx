//! Shared OpenAI Responses image generation helpers.

use std::collections::HashSet;

use anyhow::{Context, Result, bail};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use serde_json::{Value, json};
use zdx_assets::IDENTITY_PROMPT_TEMPLATE;

/// Optional image generation settings for the Responses API image tool.
#[derive(Debug, Clone, Default)]
pub struct OpenAIImageGenerationOptions {
    /// Output image dimensions, for example `1024x1024` or `auto`.
    pub size: Option<String>,
}

/// A generated image returned by the Responses API image tool.
#[derive(Debug, Clone)]
pub struct OpenAIGeneratedImage {
    pub mime_type: String,
    pub data: Vec<u8>,
}

/// Parsed response from an OpenAI image generation request.
#[derive(Debug, Clone, Default)]
pub struct OpenAIGenerateImageResponse {
    pub images: Vec<OpenAIGeneratedImage>,
    pub text_parts: Vec<String>,
}

pub(super) fn build_image_generation_request(
    model: &str,
    prompt: &str,
    options: &OpenAIImageGenerationOptions,
) -> Value {
    let mut tool = serde_json::Map::from_iter([("type".to_string(), json!("image_generation"))]);
    if let Some(size) = options
        .size
        .as_deref()
        .filter(|value| !value.trim().is_empty())
    {
        tool.insert("size".to_string(), json!(size));
    }

    json!({
        "model": responses_model_for_image_generation(model),
        "stream": true,
        "store": false,
        "instructions": IDENTITY_PROMPT_TEMPLATE.trim(),
        "input": [{
            "type": "message",
            "role": "user",
            "content": [{"type": "input_text", "text": prompt}],
        }],
        "tools": [Value::Object(tool)],
        "tool_choice": { "type": "image_generation" },
    })
}

fn responses_model_for_image_generation(model: &str) -> &str {
    if model.eq_ignore_ascii_case("gpt-image-2") {
        "gpt-5.4"
    } else {
        model
    }
}

fn parse_image_generation_response(value: &Value) -> Result<OpenAIGenerateImageResponse> {
    let output = value
        .get("response")
        .unwrap_or(value)
        .get("output")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();

    let mut images = Vec::new();
    let mut text_parts = Vec::new();
    let mut seen = HashSet::new();
    for item in output {
        match item.get("type").and_then(Value::as_str) {
            Some("image_generation_call") => {
                let data_b64 = item
                    .get("result")
                    .and_then(Value::as_str)
                    .context("OpenAI image response is missing image_generation_call.result")?;
                push_image_b64(data_b64, &mut images, &mut seen)?;
            }
            Some("message") => collect_output_text(&item, &mut text_parts),
            _ => {}
        }
    }

    if images.is_empty() && text_parts.is_empty() {
        bail!("OpenAI image response contained no images or text");
    }

    Ok(OpenAIGenerateImageResponse { images, text_parts })
}

pub(super) fn parse_image_generation_sse_response(
    body: &str,
) -> Result<OpenAIGenerateImageResponse> {
    let mut final_images = Vec::new();
    let mut partial_image = None;
    let mut text_parts = Vec::new();
    let mut event_types = Vec::new();
    let mut seen = HashSet::new();

    for line in body.lines() {
        let Some(data) = line.strip_prefix("data:").map(str::trim) else {
            continue;
        };
        if data.is_empty() || data == "[DONE]" {
            continue;
        }

        let value: Value = serde_json::from_str(data)
            .with_context(|| format!("Failed to parse OpenAI image SSE event JSON: {data}"))?;
        if let Some(event_type) = value.get("type").and_then(Value::as_str) {
            event_types.push(event_type.to_string());
        }
        collect_image_generation_event(
            &value,
            &mut final_images,
            &mut partial_image,
            &mut text_parts,
            &mut seen,
        )?;
    }

    let images = if final_images.is_empty() {
        partial_image.into_iter().collect()
    } else {
        final_images
    };

    if images.is_empty() && text_parts.is_empty() {
        bail!(
            "OpenAI image response contained no images or text. SSE event types: {}",
            summarize_event_types(&event_types)
        );
    }

    Ok(OpenAIGenerateImageResponse { images, text_parts })
}

fn collect_image_generation_event(
    value: &Value,
    final_images: &mut Vec<OpenAIGeneratedImage>,
    partial_image: &mut Option<OpenAIGeneratedImage>,
    text_parts: &mut Vec<String>,
    seen: &mut HashSet<String>,
) -> Result<()> {
    if let Some(data_b64) = final_image_b64_from_event(value) {
        push_image_b64(data_b64, final_images, seen)?;
    }
    if let Some(data_b64) = partial_image_b64_from_event(value) {
        *partial_image = Some(decode_image_b64(data_b64)?);
    }

    if matches!(
        value.get("type").and_then(Value::as_str),
        Some("response.output_item.done") | Some("response.output_item.added")
    ) && let Some(item) = value.get("item")
    {
        collect_image_generation_item(item, final_images, text_parts, seen)?;
    }

    if matches!(
        value.get("type").and_then(Value::as_str),
        Some("response.completed")
    ) && let Some(response) = value.get("response")
        && response.get("output").is_some()
    {
        let parsed = parse_image_generation_response(response)
            .context("Failed to parse completed OpenAI image response")?;
        for image in parsed.images {
            push_image_data(image.data, image.mime_type, final_images, seen);
        }
        text_parts.extend(parsed.text_parts);
    }

    Ok(())
}

fn collect_image_generation_item(
    item: &Value,
    images: &mut Vec<OpenAIGeneratedImage>,
    text_parts: &mut Vec<String>,
    seen: &mut HashSet<String>,
) -> Result<()> {
    match item.get("type").and_then(Value::as_str) {
        Some("image_generation_call") => {
            let Some(data_b64) = item.get("result").and_then(Value::as_str) else {
                return Ok(());
            };
            push_image_b64(data_b64, images, seen)?;
        }
        Some("message") => collect_output_text(item, text_parts),
        _ => {}
    }
    Ok(())
}

fn partial_image_b64_from_event(value: &Value) -> Option<&str> {
    match value.get("type").and_then(Value::as_str) {
        Some("response.image_generation_call.partial_image") => value
            .get("partial_image_b64")
            .or_else(|| value.get("b64_json"))
            .and_then(Value::as_str),
        _ => None,
    }
}

fn final_image_b64_from_event(value: &Value) -> Option<&str> {
    match value.get("type").and_then(Value::as_str) {
        Some("response.image_generation_call.completed") | Some("image_generation.completed") => {
            value
                .get("result")
                .or_else(|| value.get("b64_json"))
                .or_else(|| value.get("image_b64"))
                .and_then(Value::as_str)
        }
        _ => None,
    }
}

fn push_image_b64(
    data_b64: &str,
    images: &mut Vec<OpenAIGeneratedImage>,
    seen: &mut HashSet<String>,
) -> Result<()> {
    if !seen.insert(data_b64.to_string()) {
        return Ok(());
    }
    images.push(decode_image_b64(data_b64)?);
    Ok(())
}

fn decode_image_b64(data_b64: &str) -> Result<OpenAIGeneratedImage> {
    let data = STANDARD
        .decode(data_b64)
        .context("Failed to decode OpenAI base64 image data")?;
    Ok(OpenAIGeneratedImage {
        mime_type: "image/png".to_string(),
        data,
    })
}

fn push_image_data(
    data: Vec<u8>,
    mime_type: String,
    images: &mut Vec<OpenAIGeneratedImage>,
    seen: &mut HashSet<String>,
) {
    let key = STANDARD.encode(&data);
    if seen.insert(key) {
        images.push(OpenAIGeneratedImage { mime_type, data });
    }
}

fn summarize_event_types(event_types: &[String]) -> String {
    let mut counts = std::collections::BTreeMap::<&str, usize>::new();
    for event_type in event_types {
        *counts.entry(event_type.as_str()).or_default() += 1;
    }

    if counts.is_empty() {
        return "<none>".to_string();
    }

    counts
        .into_iter()
        .map(|(event_type, count)| format!("{event_type}={count}"))
        .collect::<Vec<_>>()
        .join(", ")
}

fn collect_output_text(item: &Value, text_parts: &mut Vec<String>) {
    let content = item
        .get("content")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();

    for part in content {
        if matches!(
            part.get("type").and_then(Value::as_str),
            Some("output_text")
        ) && let Some(text) = part.get("text").and_then(Value::as_str)
            && !text.trim().is_empty()
        {
            text_parts.push(text.to_string());
        }
    }
}

#[cfg(test)]
mod tests {
    use base64::Engine;
    use zdx_assets::IDENTITY_PROMPT_TEMPLATE;

    use super::{
        OpenAIImageGenerationOptions, build_image_generation_request,
        parse_image_generation_response, parse_image_generation_sse_response,
        responses_model_for_image_generation,
    };

    #[test]
    fn image_generation_request_forces_hosted_tool() {
        let request = build_image_generation_request(
            "gpt-image-2",
            "A red fox",
            &OpenAIImageGenerationOptions {
                size: Some("1024x1024".to_string()),
            },
        );

        assert_eq!(request["model"], serde_json::json!("gpt-5.4"));
        assert_eq!(request["stream"], serde_json::json!(true));
        assert_eq!(
            request["instructions"],
            serde_json::json!(IDENTITY_PROMPT_TEMPLATE.trim())
        );
        assert_eq!(request["input"][0]["type"], serde_json::json!("message"));
        assert_eq!(request["input"][0]["role"], serde_json::json!("user"));
        assert_eq!(
            request["input"][0]["content"][0],
            serde_json::json!({"type": "input_text", "text": "A red fox"})
        );
        assert_eq!(
            request["tools"][0]["type"],
            serde_json::json!("image_generation")
        );
        assert_eq!(request["tools"][0]["size"], serde_json::json!("1024x1024"));
        assert_eq!(
            request["tool_choice"]["type"],
            serde_json::json!("image_generation")
        );
    }

    #[test]
    fn image_generation_model_alias_uses_responses_model() {
        assert_eq!(
            responses_model_for_image_generation("gpt-image-2"),
            "gpt-5.4"
        );
        assert_eq!(responses_model_for_image_generation("gpt-5.4"), "gpt-5.4");
    }

    #[test]
    fn parse_image_generation_response_extracts_image_and_text() {
        let data_b64 = base64::engine::general_purpose::STANDARD.encode([1, 2, 3]);
        let value = serde_json::json!({
            "output": [
                {
                    "type": "message",
                    "content": [{"type": "output_text", "text": "Done."}]
                },
                {"type": "image_generation_call", "result": data_b64}
            ]
        });

        let parsed = parse_image_generation_response(&value).expect("parse should succeed");
        assert_eq!(parsed.text_parts, vec!["Done."]);
        assert_eq!(parsed.images.len(), 1);
        assert_eq!(parsed.images[0].mime_type, "image/png");
        assert_eq!(parsed.images[0].data, vec![1, 2, 3]);
    }

    #[test]
    fn parse_image_generation_sse_response_extracts_done_item() {
        let data_b64 = base64::engine::general_purpose::STANDARD.encode([4, 5, 6]);
        let event = serde_json::json!({
            "type": "response.output_item.done",
            "item": {"type": "image_generation_call", "result": data_b64}
        });
        let body = format!("event: response.output_item.done\ndata: {event}\n\ndata: [DONE]\n");

        let parsed = parse_image_generation_sse_response(&body).expect("parse should succeed");
        assert_eq!(parsed.images.len(), 1);
        assert_eq!(parsed.images[0].mime_type, "image/png");
        assert_eq!(parsed.images[0].data, vec![4, 5, 6]);
    }

    #[test]
    fn parse_image_generation_sse_response_uses_partial_image_as_fallback() {
        let data_b64 = base64::engine::general_purpose::STANDARD.encode([7, 8, 9]);
        let event = serde_json::json!({
            "type": "response.image_generation_call.partial_image",
            "partial_image_b64": data_b64,
            "partial_image_index": 0
        });
        let body = format!(
            "event: response.image_generation_call.partial_image\ndata: {event}\n\ndata: [DONE]\n"
        );

        let parsed = parse_image_generation_sse_response(&body).expect("parse should succeed");
        assert_eq!(parsed.images.len(), 1);
        assert_eq!(parsed.images[0].mime_type, "image/png");
        assert_eq!(parsed.images[0].data, vec![7, 8, 9]);
    }

    #[test]
    fn parse_image_generation_sse_response_prefers_final_over_partial_image() {
        let partial_b64 = base64::engine::general_purpose::STANDARD.encode([1, 1, 1]);
        let final_b64 = base64::engine::general_purpose::STANDARD.encode([9, 9, 9]);
        let partial_event = serde_json::json!({
            "type": "response.image_generation_call.partial_image",
            "partial_image_b64": partial_b64,
            "partial_image_index": 0
        });
        let final_event = serde_json::json!({
            "type": "response.output_item.done",
            "item": {"type": "image_generation_call", "result": final_b64}
        });
        let body = format!(
            "event: response.image_generation_call.partial_image\ndata: {partial_event}\n\nevent: response.output_item.done\ndata: {final_event}\n\ndata: [DONE]\n"
        );

        let parsed = parse_image_generation_sse_response(&body).expect("parse should succeed");
        assert_eq!(parsed.images.len(), 1);
        assert_eq!(parsed.images[0].mime_type, "image/png");
        assert_eq!(parsed.images[0].data, vec![9, 9, 9]);
    }
}
