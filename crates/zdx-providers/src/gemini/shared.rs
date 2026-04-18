//! Shared Gemini API helpers for both API key and OAuth providers.
//!
//! This module contains common code for:
//! - SSE parsing (`GeminiSseParser`)
//! - Message conversion to Gemini format
//! - Error classification
//! - Common utility functions

use std::collections::HashMap;

use serde_json::{Value, json};
use zdx_types::{ThinkingLevel, ToolDefinition, ToolResultBlock, ToolResultContent};

use crate::{
    ChatContentBlock, ChatMessage, MessageContent, ProviderError, ProviderErrorKind, ReplayToken,
};

/// Thinking configuration for Gemini models.
///
/// Gemini 3 models use `thinkingLevel` (string levels).
/// Gemini 2.5 models use `thinkingBudget` (token count).
#[derive(Debug, Clone)]
pub enum GeminiThinkingConfig {
    /// For Gemini 3 models: use thinking level strings.
    /// Valid values depend on model (see `capabilities_for_model`):
    /// - Gemini 3 Pro / 3.1 Pro: `low`, `medium`, `high`
    /// - Gemini 3 Flash / Flash-Lite: `minimal`, `low`, `medium`, `high`
    /// - Gemini 3.1 Flash Image Preview: `minimal`, `high`
    /// - Gemini 3 Pro Image: `high` only
    Level(String),
    /// For Gemini 2.5 models: use token budget.
    /// -1 = dynamic (default), 0 = disabled, positive = specific budget.
    Budget(i32),
}

/// Which `thinkingLevel` strings a particular Gemini 3 model accepts.
///
/// All Gemini 3 models support `"high"`, so we only track the intermediate
/// levels explicitly. Used to clamp zdx `ThinkingLevel` values to a level the
/// target model actually accepts.
#[derive(Debug, Clone, Copy)]
struct GeminiCapabilities {
    supports_minimal: bool,
    supports_low: bool,
    supports_medium: bool,
}

/// Returns the Gemini 3 `thinkingLevel` capabilities for a given model id.
///
/// Order matters: more specific image variants are matched before the generic
/// Pro/Flash families (for example `gemini-3-pro-image-preview` must be
/// detected before `gemini-3-pro`). Unknown Gemini 3 model names fall through
/// to the Flash default (all intermediate levels supported).
fn capabilities_for_model(model: &str) -> GeminiCapabilities {
    // Pro Image variants: only "high" thinking.
    if model.contains("gemini-3-pro-image") || model.contains("gemini-3.1-pro-image") {
        return GeminiCapabilities {
            supports_minimal: false,
            supports_low: false,
            supports_medium: false,
        };
    }

    // Flash Image Preview variants: only "minimal" and "high".
    if model.contains("gemini-3.1-flash-image-preview")
        || model.contains("gemini-3-flash-image-preview")
        || model.contains("gemini-3.1-flash-image")
        || model.contains("gemini-3-flash-image")
    {
        return GeminiCapabilities {
            supports_minimal: true,
            supports_low: false,
            supports_medium: false,
        };
    }

    // Pro (text) variants: "low", "medium", "high" — no "minimal".
    if model.contains("gemini-3-pro") || model.contains("gemini-3.1-pro") {
        return GeminiCapabilities {
            supports_minimal: false,
            supports_low: true,
            supports_medium: true,
        };
    }

    // Flash / Flash-Lite and anything else on Gemini 3: full range.
    GeminiCapabilities {
        supports_minimal: true,
        supports_low: true,
        supports_medium: true,
    }
}

impl GeminiThinkingConfig {
    /// Maps zdx's `ThinkingLevel` to Gemini-specific config based on model name.
    ///
    /// For Gemini 3 models: maps to thinkingLevel strings, clamping to the
    /// nearest level the target model supports (per `capabilities_for_model`).
    /// For Gemini 2.5 models: maps to thinkingBudget tokens.
    pub fn from_thinking_level(level: ThinkingLevel, model: &str) -> Self {
        if model.contains("gemini-3") {
            let caps = capabilities_for_model(model);
            return Self::gemini_3_level(level, caps);
        }

        // Gemini 2.5 (and older) models use thinkingBudget.
        let is_flash_lite = model.contains("flash-lite");
        let is_25_pro = model.contains("2.5-pro") || model.contains("2.5 pro");

        match level {
            ThinkingLevel::Off => {
                // 2.5 Pro cannot fully disable thinking; use its minimum budget.
                if is_25_pro {
                    Self::Budget(128)
                } else {
                    Self::Budget(0)
                }
            }
            ThinkingLevel::Minimal => {
                // Flash Lite minimum is 512; other 2.5 models start at 1024.
                if is_flash_lite {
                    Self::Budget(512)
                } else {
                    Self::Budget(1024)
                }
            }
            ThinkingLevel::Low => Self::Budget(2048),
            ThinkingLevel::Medium => Self::Budget(8192),
            ThinkingLevel::High => Self::Budget(16384),
            ThinkingLevel::XHigh => {
                // Max budget depends on model family.
                if is_25_pro {
                    Self::Budget(32768)
                } else {
                    Self::Budget(24576)
                }
            }
        }
    }

    /// Resolves a Gemini 3 `thinkingLevel` string, clamping unsupported levels.
    ///
    /// Clamping rules:
    /// - `Off` / `Minimal`: pick the lowest supported level (minimal → low →
    ///   medium → high).
    /// - `Low`: prefer `low`; otherwise clamp down to `minimal` if available,
    ///   else up to `medium`, else `high`.
    /// - `Medium`: prefer `medium`; otherwise clamp **up** to `high` (closer
    ///   than clamping all the way down to `minimal`/`low`).
    /// - `High` / `XHigh`: always `high`.
    fn gemini_3_level(level: ThinkingLevel, caps: GeminiCapabilities) -> Self {
        let lowest_supported = if caps.supports_minimal {
            "minimal"
        } else if caps.supports_low {
            "low"
        } else if caps.supports_medium {
            "medium"
        } else {
            "high"
        };

        let chosen = match level {
            ThinkingLevel::Off | ThinkingLevel::Minimal => lowest_supported,
            ThinkingLevel::Low => {
                if caps.supports_low {
                    "low"
                } else if caps.supports_minimal {
                    "minimal"
                } else if caps.supports_medium {
                    "medium"
                } else {
                    "high"
                }
            }
            ThinkingLevel::Medium => {
                if caps.supports_medium {
                    "medium"
                } else {
                    // No medium → clamp up to high (closer than dropping to low).
                    "high"
                }
            }
            ThinkingLevel::High | ThinkingLevel::XHigh => "high",
        };

        Self::Level(chosen.to_string())
    }

    /// Converts to the JSON value for `generationConfig.thinkingConfig`.
    ///
    /// Returns the inner object (without the outer `thinkingConfig` wrapper);
    /// callers slot it into `generationConfig["thinkingConfig"]` and may add
    /// `includeThoughts` where the API supports it.
    pub fn to_json(&self) -> Value {
        match self {
            GeminiThinkingConfig::Level(level) => json!({ "thinkingLevel": level }),
            GeminiThinkingConfig::Budget(tokens) => json!({ "thinkingBudget": tokens }),
        }
    }
}

/// Dummy thought-signature accepted by Gemini's validator when no real
/// signature is available. Always applied as a message-local fallback so
/// historical assistant messages serialize to identical bytes across turns
/// (required for implicit prompt caching).
pub const SYNTHETIC_THOUGHT_SIGNATURE: &str = "skip_thought_signature_validator";

/// Classifies a reqwest error into a `ProviderError`.
pub fn classify_reqwest_error(e: &reqwest::Error) -> ProviderError {
    if e.is_timeout() {
        ProviderError::timeout(format!("Request timed out: {e}"))
    } else if e.is_connect() {
        ProviderError::timeout(format!("Connection failed: {e}"))
    } else if e.is_request() {
        ProviderError::new(ProviderErrorKind::HttpStatus, format!("Request error: {e}"))
    } else {
        ProviderError::new(ProviderErrorKind::HttpStatus, format!("Network error: {e}"))
    }
}

/// Builds Gemini-format contents array from chat messages.
///
/// The `model` string is threaded through so message-serialization paths can
/// vary based on model capabilities (for example, Gemini 3 vs 2.5 tool-result
/// image handling).
pub fn build_contents(messages: &[ChatMessage], model: &str) -> Vec<Value> {
    let mut builder = GeminiContentsBuilder::new(model);
    for msg in messages {
        builder.append_message(msg);
    }
    builder.contents
}

struct GeminiContentsBuilder {
    model: String,
    contents: Vec<Value>,
    tool_name_map: HashMap<String, String>,
}

impl GeminiContentsBuilder {
    fn new(model: &str) -> Self {
        Self {
            model: model.to_string(),
            contents: Vec::new(),
            tool_name_map: HashMap::new(),
        }
    }

    fn append_message(&mut self, msg: &ChatMessage) {
        match (&msg.role[..], &msg.content) {
            ("user", MessageContent::Text(text)) => {
                let parts = vec![text_part(text)];
                self.push_message("user", &parts);
            }
            ("assistant", MessageContent::Text(text)) => {
                let parts = vec![text_part(text)];
                self.push_message("model", &parts);
            }
            ("assistant", MessageContent::Blocks(blocks)) => {
                self.append_assistant_blocks(blocks);
            }
            ("user", MessageContent::Blocks(blocks)) => self.append_user_blocks(blocks),
            _ => {}
        }
    }

    fn append_assistant_blocks(&mut self, blocks: &[ChatContentBlock]) {
        let mut parts = Vec::new();
        let mut added_signature = false;
        let real_signature = gemini_signature(blocks);
        let has_tool_use = blocks
            .iter()
            .any(|b| matches!(b, ChatContentBlock::ToolUse { .. }));

        // Use the real Gemini signature when available, otherwise fall back to
        // the synthetic sentinel. The fallback is message-local and stable:
        // the same historical assistant message serializes identically on
        // every turn, which is required for implicit prompt caching.
        let signature_to_use: &str = real_signature
            .as_deref()
            .unwrap_or(SYNTHETIC_THOUGHT_SIGNATURE);

        for block in blocks {
            match block {
                ChatContentBlock::Text(text) => {
                    let mut part = text_part(text);
                    // Only attach signature to text if there is no tool use in this message
                    // (Gemini prefers attaching signature to functionCall if present)
                    if !added_signature && !has_tool_use {
                        part["thoughtSignature"] = json!(signature_to_use);
                        added_signature = true;
                    }
                    parts.push(part);
                }
                ChatContentBlock::Image { mime_type, data } => {
                    parts.push(inline_data_part(mime_type, data));
                }
                ChatContentBlock::ToolUse { id, name, input } => {
                    self.tool_name_map.insert(id.clone(), name.clone());
                    let mut part = json!({
                        "functionCall": {
                            "id": id,
                            "name": name,
                            "args": input
                        }
                    });
                    if !added_signature {
                        part["thoughtSignature"] = json!(signature_to_use);
                        added_signature = true;
                    }
                    parts.push(part);
                }
                ChatContentBlock::Reasoning(_) | ChatContentBlock::ToolResult(_) => {}
            }
        }

        if !parts.is_empty() {
            self.push_message("model", &parts);
        }
    }

    fn append_user_blocks(&mut self, blocks: &[ChatContentBlock]) {
        let is_gemini_3 = self.model.contains("gemini-3");
        let mut parts = Vec::new();
        let mut tool_results = Vec::new();
        // On Gemini 2.5 and older, tool-result images cannot live inside
        // `functionResponse.parts`; they must be emitted as a separate user
        // message immediately after the functionResponse parts.
        let mut pending_images: Vec<Value> = Vec::new();

        for block in blocks {
            match block {
                ChatContentBlock::Text(text) => parts.push(text_part(text)),
                ChatContentBlock::Image { mime_type, data } => {
                    parts.push(inline_data_part(mime_type, data));
                }
                ChatContentBlock::ToolResult(result) => tool_results.push(result),
                _ => {}
            }
        }

        for result in tool_results {
            let Some(name) = self.tool_name_map.get(&result.tool_use_id) else {
                continue;
            };

            let (text, image) = extract_tool_result_with_image(&result.content);
            let mut function_response = json!({
                "id": result.tool_use_id,
                "name": name,
                "response": {
                    "content": text,
                    "is_error": result.is_error
                }
            });
            if let Some((mime_type, data)) = image {
                if is_gemini_3 {
                    function_response["parts"] = json!([inline_data_part(&mime_type, &data)]);
                } else {
                    pending_images.push(inline_data_part(&mime_type, &data));
                }
            }
            parts.push(json!({ "functionResponse": function_response }));
        }

        if !parts.is_empty() {
            self.push_message("user", &parts);
        }

        if !pending_images.is_empty() {
            self.push_message("user", &pending_images);
        }
    }

    fn push_message(&mut self, role: &str, parts: &[Value]) {
        self.contents.push(json!({ "role": role, "parts": parts }));
    }
}

fn gemini_signature(blocks: &[ChatContentBlock]) -> Option<String> {
    blocks.iter().find_map(|block| match block {
        ChatContentBlock::Reasoning(reasoning) => reasoning.replay.as_ref().and_then(|replay| {
            let ReplayToken::Gemini { signature } = replay else {
                return None;
            };
            Some(signature.clone())
        }),
        _ => None,
    })
}

fn text_part(text: &str) -> Value {
    json!({ "text": text })
}

fn inline_data_part(mime_type: &str, data: &str) -> Value {
    json!({
        "inlineData": {
            "mimeType": mime_type,
            "data": data
        }
    })
}

/// Builds the tools array for Gemini API.
pub fn build_tools(tools: &[ToolDefinition]) -> Option<Value> {
    if tools.is_empty() {
        None
    } else {
        Some(json!([
            {
                "functionDeclarations": tools
                    .iter()
                    .map(|tool| {
                        // Use lowercase tool names for Gemini (Anthropic requires PascalCase, others prefer lowercase)
                        let tool = tool.with_lowercase_name();
                        let parameters = sanitize_gemini_function_schema(&tool.input_schema);
                        json!({
                            "name": tool.name,
                            "description": tool.description,
                            "parameters": parameters
                        })
                    })
                    .collect::<Vec<_>>()
            }
        ]))
    }
}

/// Gemini function declaration schema is a subset of JSON Schema/OpenAPI.
///
/// In practice, Gemini rejects `additionalProperties` inside function parameter
/// schemas for tool declarations. Strip unsupported fields recursively.
fn sanitize_gemini_function_schema(schema: &Value) -> Value {
    match schema {
        Value::Object(map) => {
            let mut sanitized = serde_json::Map::new();
            for (key, value) in map {
                if key == "additionalProperties" {
                    continue;
                }
                sanitized.insert(key.clone(), sanitize_gemini_function_schema(value));
            }
            Value::Object(sanitized)
        }
        Value::Array(values) => {
            Value::Array(values.iter().map(sanitize_gemini_function_schema).collect())
        }
        _ => schema.clone(),
    }
}

/// Builds a standard Gemini API request body (for API key auth).
///
/// `model` is threaded through to `build_contents` so message-level
/// serialization can vary by model capability (for example, Gemini 3 vs 2.5
/// tool-result image handling).
pub fn build_gemini_request(
    messages: &[ChatMessage],
    tools: &[ToolDefinition],
    system: Option<&str>,
    max_output_tokens: Option<u32>,
    thinking_config: Option<&GeminiThinkingConfig>,
    model: &str,
) -> Value {
    let contents = build_contents(messages, model);
    let tools_value = build_tools(tools);

    let mut request = json!({
        "contents": contents,
    });

    if let Some(prompt) = system
        && !prompt.trim().is_empty()
    {
        request["systemInstruction"] = json!({
            "parts": [{"text": prompt}]
        });
    }

    if let Some(tools_value) = tools_value {
        request["tools"] = tools_value;
    }

    let mut generation_config = json!({});
    if let Some(max_output_tokens) = max_output_tokens
        && max_output_tokens > 0
    {
        generation_config["maxOutputTokens"] = json!(max_output_tokens);
    }

    if let Some(thinking) = thinking_config {
        let mut thinking_obj = thinking.to_json();
        thinking_obj["includeThoughts"] = json!(true);
        generation_config["thinkingConfig"] = thinking_obj;
    }

    if generation_config.as_object().is_some_and(|o| !o.is_empty()) {
        request["generationConfig"] = generation_config;
    }

    request
}

/// Parameters for building a Cloud Code Assist request.
pub struct CloudCodeRequestParams<'a> {
    pub model: &'a str,
    pub project_id: &'a str,
    pub max_output_tokens: Option<u32>,
    pub session_id: &'a str,
    pub prompt_seq: u32,
    pub thinking_config: Option<&'a GeminiThinkingConfig>,
}

/// Builds a Cloud Code Assist request body (for OAuth auth).
///
/// `session_id` and `prompt_seq` are used to generate `user_prompt_id` in the format
/// used by the official Gemini CLI: `<session_id>########<seq>`.
pub fn build_cloud_code_assist_request(
    messages: &[ChatMessage],
    tools: &[ToolDefinition],
    system: Option<&str>,
    params: &CloudCodeRequestParams<'_>,
) -> Value {
    let contents = build_contents(messages, params.model);
    let tools_value = build_tools(tools);

    let mut inner_request = json!({
        "contents": contents,
    });

    if let Some(prompt) = system
        && !prompt.trim().is_empty()
    {
        inner_request["systemInstruction"] = json!({
            "parts": [{"text": prompt}]
        });
    }

    if let Some(tools_value) = tools_value {
        inner_request["tools"] = tools_value;
    }

    let mut generation_config = json!({});
    if let Some(tokens) = params.max_output_tokens
        && tokens > 0
    {
        generation_config["maxOutputTokens"] = json!(tokens);
    }

    // Note: Cloud Code Assist API does NOT support includeThoughts
    // (unlike the standard Gemini API at generativelanguage.googleapis.com).
    if let Some(thinking) = params.thinking_config {
        generation_config["thinkingConfig"] = thinking.to_json();
    }

    if generation_config.as_object().is_some_and(|o| !o.is_empty()) {
        inner_request["generationConfig"] = generation_config;
    }

    // Format matches official Gemini CLI: <session_id>########<seq>
    let user_prompt_id = format!("{}########{}", params.session_id, params.prompt_seq);

    json!({
        "project": params.project_id,
        "model": params.model,
        "user_prompt_id": user_prompt_id,
        "request": inner_request,
    })
}

/// Extracts text and optional image from tool result content.
/// Returns (text, Option<(`mime_type`, `base64_data`)>)
fn extract_tool_result_with_image(
    content: &ToolResultContent,
) -> (String, Option<(String, String)>) {
    match content {
        ToolResultContent::Text(text) => (text.clone(), None),
        ToolResultContent::Blocks(blocks) => {
            let text = blocks
                .iter()
                .find_map(|block| match block {
                    ToolResultBlock::Text { text } => Some(text.clone()),
                    ToolResultBlock::Image { .. } => None,
                })
                .unwrap_or_default();

            let image = blocks.iter().find_map(|block| match block {
                ToolResultBlock::Image { mime_type, data } => {
                    Some((mime_type.clone(), data.clone()))
                }
                ToolResultBlock::Text { .. } => None,
            });

            (text, image)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Gemini 3 Pro: maps to thinkingLevel. No `minimal`; `medium` is supported.
    #[test]
    fn test_thinking_config_gemini_3_pro() {
        // Off -> low (lowest supported for Pro is "low")
        let config =
            GeminiThinkingConfig::from_thinking_level(ThinkingLevel::Off, "gemini-3-pro-preview");
        assert!(matches!(config, GeminiThinkingConfig::Level(ref l) if l == "low"));

        // Minimal -> low (Pro doesn't support "minimal"; clamp to lowest supported)
        let config = GeminiThinkingConfig::from_thinking_level(
            ThinkingLevel::Minimal,
            "gemini-3-pro-preview",
        );
        assert!(matches!(config, GeminiThinkingConfig::Level(ref l) if l == "low"));

        // Low -> low
        let config =
            GeminiThinkingConfig::from_thinking_level(ThinkingLevel::Low, "gemini-3-pro-preview");
        assert!(matches!(config, GeminiThinkingConfig::Level(ref l) if l == "low"));

        // Medium -> medium (Gemini 3 Pro supports medium per Vertex docs)
        let config = GeminiThinkingConfig::from_thinking_level(
            ThinkingLevel::Medium,
            "gemini-3-pro-preview",
        );
        assert!(matches!(config, GeminiThinkingConfig::Level(ref l) if l == "medium"));

        // High -> high
        let config =
            GeminiThinkingConfig::from_thinking_level(ThinkingLevel::High, "gemini-3-pro-preview");
        assert!(matches!(config, GeminiThinkingConfig::Level(ref l) if l == "high"));
    }

    /// Gemini 3.1 Pro: same capabilities as 3.0 Pro (low/medium/high).
    #[test]
    fn test_thinking_config_gemini_31_pro() {
        let config = GeminiThinkingConfig::from_thinking_level(
            ThinkingLevel::Medium,
            "gemini-3.1-pro-preview",
        );
        assert!(matches!(config, GeminiThinkingConfig::Level(ref l) if l == "medium"));

        let config = GeminiThinkingConfig::from_thinking_level(
            ThinkingLevel::Minimal,
            "gemini-3.1-pro-preview",
        );
        assert!(matches!(config, GeminiThinkingConfig::Level(ref l) if l == "low"));
    }

    /// Gemini 3.1 Flash Image Preview: only `minimal` and `high` are valid.
    /// Intermediate Low/Medium clamp to the nearest supported level:
    /// Low -> minimal (clamp down), Medium -> high (clamp up).
    #[test]
    fn test_thinking_config_gemini_31_flash_image_preview() {
        let model = "gemini-3.1-flash-image-preview";

        let config = GeminiThinkingConfig::from_thinking_level(ThinkingLevel::Off, model);
        assert!(matches!(config, GeminiThinkingConfig::Level(ref l) if l == "minimal"));

        let config = GeminiThinkingConfig::from_thinking_level(ThinkingLevel::Minimal, model);
        assert!(matches!(config, GeminiThinkingConfig::Level(ref l) if l == "minimal"));

        let config = GeminiThinkingConfig::from_thinking_level(ThinkingLevel::Low, model);
        assert!(matches!(config, GeminiThinkingConfig::Level(ref l) if l == "minimal"));

        let config = GeminiThinkingConfig::from_thinking_level(ThinkingLevel::Medium, model);
        assert!(matches!(config, GeminiThinkingConfig::Level(ref l) if l == "high"));

        let config = GeminiThinkingConfig::from_thinking_level(ThinkingLevel::High, model);
        assert!(matches!(config, GeminiThinkingConfig::Level(ref l) if l == "high"));
    }

    /// Gemini 3 Pro Image: only `high` is supported — everything clamps up.
    #[test]
    fn test_thinking_config_gemini_3_pro_image() {
        let model = "gemini-3-pro-image-preview";

        for level in [
            ThinkingLevel::Off,
            ThinkingLevel::Minimal,
            ThinkingLevel::Low,
            ThinkingLevel::Medium,
            ThinkingLevel::High,
            ThinkingLevel::XHigh,
        ] {
            let config = GeminiThinkingConfig::from_thinking_level(level, model);
            assert!(
                matches!(config, GeminiThinkingConfig::Level(ref l) if l == "high"),
                "expected 'high' for {level:?} on Pro Image, got {config:?}"
            );
        }
    }

    /// Gemini 3 Flash: maps to thinkingLevel with full support.
    #[test]
    fn test_thinking_config_gemini_3_flash() {
        // Off -> minimal (Flash can use minimal)
        let config =
            GeminiThinkingConfig::from_thinking_level(ThinkingLevel::Off, "gemini-3-flash-preview");
        assert!(matches!(config, GeminiThinkingConfig::Level(ref l) if l == "minimal"));

        // Minimal -> minimal
        let config = GeminiThinkingConfig::from_thinking_level(
            ThinkingLevel::Minimal,
            "gemini-3-flash-preview",
        );
        assert!(matches!(config, GeminiThinkingConfig::Level(ref l) if l == "minimal"));

        // Medium -> medium (Flash supports medium)
        let config = GeminiThinkingConfig::from_thinking_level(
            ThinkingLevel::Medium,
            "gemini-3-flash-preview",
        );
        assert!(matches!(config, GeminiThinkingConfig::Level(ref l) if l == "medium"));
    }

    /// Gemini 2.5 Flash: maps to thinkingBudget tokens.
    #[test]
    fn test_thinking_config_gemini_25_flash() {
        // Off -> 0 (can disable)
        let config =
            GeminiThinkingConfig::from_thinking_level(ThinkingLevel::Off, "gemini-2.5-flash");
        assert!(matches!(config, GeminiThinkingConfig::Budget(0)));

        // Low -> 2048
        let config =
            GeminiThinkingConfig::from_thinking_level(ThinkingLevel::Low, "gemini-2.5-flash");
        assert!(matches!(config, GeminiThinkingConfig::Budget(2048)));

        // Medium -> 8192
        let config =
            GeminiThinkingConfig::from_thinking_level(ThinkingLevel::Medium, "gemini-2.5-flash");
        assert!(matches!(config, GeminiThinkingConfig::Budget(8192)));

        // XHigh -> 24576 (max for flash)
        let config =
            GeminiThinkingConfig::from_thinking_level(ThinkingLevel::XHigh, "gemini-2.5-flash");
        assert!(matches!(config, GeminiThinkingConfig::Budget(24576)));
    }

    /// Gemini 2.5 Flash Lite: minimal starts at 512.
    #[test]
    fn test_thinking_config_gemini_25_flash_lite() {
        // Minimal -> 512 (flash-lite minimum)
        let config = GeminiThinkingConfig::from_thinking_level(
            ThinkingLevel::Minimal,
            "gemini-2.5-flash-lite",
        );
        assert!(matches!(config, GeminiThinkingConfig::Budget(512)));
    }

    /// `GeminiThinkingConfig::to_json` produces correct format.
    #[test]
    fn test_thinking_config_to_json() {
        // Level produces thinkingLevel
        let config = GeminiThinkingConfig::Level("medium".to_string());
        let json = config.to_json();
        assert_eq!(json["thinkingLevel"], "medium");

        // Budget produces thinkingBudget
        let config = GeminiThinkingConfig::Budget(8192);
        let json = config.to_json();
        assert_eq!(json["thinkingBudget"], 8192);
    }

    /// `build_contents` uses real Gemini thought signature when available.
    #[test]
    fn test_build_contents_uses_real_gemini_signature() {
        use crate::{ReasoningBlock, ReplayToken};

        // Case 1: Assistant message with reasoning block (Gemini signature) + tool use
        let messages = vec![
            ChatMessage::user("What files are here?"),
            ChatMessage {
                role: "assistant".to_string(),
                phase: None,
                content: MessageContent::Blocks(vec![
                    ChatContentBlock::Reasoning(ReasoningBlock {
                        text: Some("I'll check the files".to_string()),
                        replay: Some(ReplayToken::Gemini {
                            signature: "real_thought_signature_base64".to_string(),
                        }),
                    }),
                    ChatContentBlock::ToolUse {
                        id: "tool-1".to_string(),
                        name: "bash".to_string(),
                        input: serde_json::json!({"command": "ls"}),
                    },
                ]),
            },
        ];

        let contents = build_contents(&messages, "gemini-3-flash-preview");

        // The assistant message should have the real signature attached to functionCall
        let assistant_msg = &contents[1];
        let parts = assistant_msg["parts"].as_array().unwrap();

        // Should have one part (just the functionCall, reasoning is skipped)
        assert_eq!(parts.len(), 1);

        let function_call_part = &parts[0];
        assert!(function_call_part.get("functionCall").is_some());
        assert_eq!(
            function_call_part["thoughtSignature"],
            "real_thought_signature_base64"
        );

        // Case 2: Assistant message with reasoning block (Gemini signature) + ONLY text
        let messages = vec![
            ChatMessage::user("Hi"),
            ChatMessage {
                role: "assistant".to_string(),
                phase: None,
                content: MessageContent::Blocks(vec![
                    ChatContentBlock::Reasoning(ReasoningBlock {
                        text: Some("Thinking...".to_string()),
                        replay: Some(ReplayToken::Gemini {
                            signature: "text_only_signature".to_string(),
                        }),
                    }),
                    ChatContentBlock::Text("Hello!".to_string()),
                ]),
            },
        ];

        let contents = build_contents(&messages, "gemini-3-flash-preview");

        let assistant_msg = &contents[1];
        let parts = assistant_msg["parts"].as_array().unwrap();

        // Should have one text part with the signature
        assert_eq!(parts.len(), 1);
        let text_part = &parts[0];
        assert_eq!(text_part["text"], "Hello!");
        assert_eq!(text_part["thoughtSignature"], "text_only_signature");
    }

    /// `build_contents` falls back to synthetic signature when no Gemini signature available.
    #[test]
    fn test_build_contents_fallback_to_synthetic_signature() {
        // Create a message history without reasoning blocks
        let messages = vec![
            ChatMessage::user("What files are here?"),
            ChatMessage {
                role: "assistant".to_string(),
                phase: None,
                content: MessageContent::Blocks(vec![ChatContentBlock::ToolUse {
                    id: "tool-1".to_string(),
                    name: "bash".to_string(),
                    input: serde_json::json!({"command": "ls"}),
                }]),
            },
        ];

        let contents = build_contents(&messages, "gemini-3-flash-preview");

        let assistant_msg = &contents[1];
        let parts = assistant_msg["parts"].as_array().unwrap();
        let function_call_part = &parts[0];

        // Should fall back to synthetic signature
        assert_eq!(
            function_call_part["thoughtSignature"],
            SYNTHETIC_THOUGHT_SIGNATURE
        );
    }

    /// The synthetic thought signature must be message-local and stable: a
    /// given historical assistant message must serialize byte-for-byte
    /// identically across turns, even as new user messages are appended.
    /// This is the core invariant implicit prompt caching depends on.
    #[test]
    fn test_synthetic_signature_is_stable_across_turns() {
        use zdx_types::{ToolResult, ToolResultContent};

        let make_history = || {
            vec![
                ChatMessage::user("list files"),
                ChatMessage {
                    role: "assistant".to_string(),
                    phase: None,
                    content: MessageContent::Blocks(vec![ChatContentBlock::ToolUse {
                        id: "call_sig_stable".to_string(),
                        name: "bash".to_string(),
                        input: serde_json::json!({"command": "ls"}),
                    }]),
                },
                ChatMessage {
                    role: "user".to_string(),
                    phase: None,
                    content: MessageContent::Blocks(vec![ChatContentBlock::ToolResult(
                        ToolResult {
                            tool_use_id: "call_sig_stable".to_string(),
                            content: ToolResultContent::Text("a.txt\n".to_string()),
                            is_error: false,
                        },
                    )]),
                },
                ChatMessage {
                    role: "assistant".to_string(),
                    phase: None,
                    content: MessageContent::Text("Here you go.".to_string()),
                },
            ]
        };

        // Turn N: 4 messages
        let history_n = make_history();
        // Turn N+1: same 4 messages + a new user message
        let mut history_n1 = make_history();
        history_n1.push(ChatMessage::user("now count them"));

        let model = "gemini-3-flash-preview";
        let contents_n = build_contents(&history_n, model);
        let contents_n1 = build_contents(&history_n1, model);

        // The FIRST historical assistant message (the tool-use) must serialize
        // identically at both turns. `contents[1]` is the assistant tool-use
        // message in both cases.
        assert_eq!(
            contents_n[1], contents_n1[1],
            "historical assistant tool-use message must be byte-identical across turns"
        );

        // Sanity: the synthetic signature is actually attached (otherwise the
        // test would pass trivially even if stability regressed).
        let function_call = &contents_n[1]["parts"][0];
        assert_eq!(
            function_call["thoughtSignature"], SYNTHETIC_THOUGHT_SIGNATURE,
            "historical tool-use should carry the synthetic signature"
        );
    }

    /// `build_contents` includes thought signature for historical messages.
    #[test]
    fn test_build_contents_includes_signature_for_history() {
        use crate::{ReasoningBlock, ReplayToken};

        // Message 1: Assistant with signature (simulating history)
        let msg1 = ChatMessage {
            role: "assistant".to_string(),
            phase: None,
            content: MessageContent::Blocks(vec![
                ChatContentBlock::Reasoning(ReasoningBlock {
                    text: Some("Thinking...".to_string()),
                    replay: Some(ReplayToken::Gemini {
                        signature: "hist_sig".to_string(),
                    }),
                }),
                ChatContentBlock::Text("Hello".to_string()),
            ]),
        };

        // Message 2: User (makes msg1 "history")
        let msg2 = ChatMessage::user("Next");

        let messages = vec![msg1, msg2];
        let contents = build_contents(&messages, "gemini-3-flash-preview");

        let assistant_part = &contents[0]["parts"][0];
        assert_eq!(assistant_part["text"], "Hello");
        assert_eq!(
            assistant_part["thoughtSignature"], "hist_sig",
            "History should keep signature"
        );
    }

    /// `functionCall` parts must echo the original tool-use `id` so Gemini 3
    /// can correlate parallel calls with their responses.
    #[test]
    fn test_assistant_tool_use_includes_id() {
        let messages = vec![
            ChatMessage::user("run a command"),
            ChatMessage {
                role: "assistant".to_string(),
                phase: None,
                content: MessageContent::Blocks(vec![ChatContentBlock::ToolUse {
                    id: "call_123".to_string(),
                    name: "bash".to_string(),
                    input: serde_json::json!({"command": "ls"}),
                }]),
            },
        ];

        let contents = build_contents(&messages, "gemini-3-flash-preview");

        let assistant_msg = &contents[1];
        let parts = assistant_msg["parts"].as_array().unwrap();
        let function_call = parts
            .iter()
            .find_map(|p| p.get("functionCall"))
            .expect("functionCall part should exist");
        assert_eq!(function_call["id"], "call_123");
        assert_eq!(function_call["name"], "bash");
    }

    /// `functionResponse` parts must echo the original `tool_use_id` as `id`.
    #[test]
    fn test_function_response_includes_id() {
        use zdx_types::{ToolResult, ToolResultContent};

        let messages = vec![
            ChatMessage::user("run a command"),
            ChatMessage {
                role: "assistant".to_string(),
                phase: None,
                content: MessageContent::Blocks(vec![ChatContentBlock::ToolUse {
                    id: "call_123".to_string(),
                    name: "bash".to_string(),
                    input: serde_json::json!({"command": "ls"}),
                }]),
            },
            ChatMessage {
                role: "user".to_string(),
                phase: None,
                content: MessageContent::Blocks(vec![ChatContentBlock::ToolResult(ToolResult {
                    tool_use_id: "call_123".to_string(),
                    content: ToolResultContent::Text("a.txt\nb.txt".to_string()),
                    is_error: false,
                })]),
            },
        ];

        let contents = build_contents(&messages, "gemini-3-flash-preview");

        let user_followup = &contents[2];
        assert_eq!(user_followup["role"], "user");
        let parts = user_followup["parts"].as_array().unwrap();
        let function_response = parts
            .iter()
            .find_map(|p| p.get("functionResponse"))
            .expect("functionResponse part should exist");
        assert_eq!(function_response["id"], "call_123");
        assert_eq!(function_response["name"], "bash");
        assert_eq!(function_response["response"]["content"], "a.txt\nb.txt");
        assert_eq!(function_response["response"]["is_error"], false);
        assert!(
            function_response.get("parts").is_none(),
            "no nested parts when there is no image"
        );
    }

    /// On Gemini 3, a tool-result image must be embedded inside
    /// `functionResponse.parts` — no separate user message is emitted.
    #[test]
    fn test_gemini_3_tool_result_image_uses_nested_parts() {
        use zdx_types::{ToolResult, ToolResultBlock, ToolResultContent};

        let messages = vec![
            ChatMessage::user("take a screenshot"),
            ChatMessage {
                role: "assistant".to_string(),
                phase: None,
                content: MessageContent::Blocks(vec![ChatContentBlock::ToolUse {
                    id: "call_img".to_string(),
                    name: "screenshot".to_string(),
                    input: serde_json::json!({}),
                }]),
            },
            ChatMessage {
                role: "user".to_string(),
                phase: None,
                content: MessageContent::Blocks(vec![ChatContentBlock::ToolResult(ToolResult {
                    tool_use_id: "call_img".to_string(),
                    content: ToolResultContent::Blocks(vec![
                        ToolResultBlock::Text {
                            text: "captured".to_string(),
                        },
                        ToolResultBlock::Image {
                            mime_type: "image/png".to_string(),
                            data: "BASE64DATA".to_string(),
                        },
                    ]),
                    is_error: false,
                })]),
            },
        ];

        let contents = build_contents(&messages, "gemini-3-flash-preview");

        // Exactly 3 messages: original user, assistant tool-use, single user
        // tool-result message (no separate trailing image-only user message).
        assert_eq!(
            contents.len(),
            3,
            "Gemini 3: image must not produce a separate message"
        );

        let user_followup = &contents[2];
        assert_eq!(user_followup["role"], "user");
        let parts = user_followup["parts"].as_array().unwrap();
        assert_eq!(parts.len(), 1, "only the functionResponse part is emitted");

        let function_response = &parts[0]["functionResponse"];
        assert_eq!(function_response["id"], "call_img");
        assert_eq!(function_response["name"], "screenshot");
        assert_eq!(function_response["response"]["content"], "captured");

        let nested = function_response["parts"].as_array().unwrap();
        assert_eq!(nested.len(), 1);
        assert_eq!(nested[0]["inlineData"]["mimeType"], "image/png");
        assert_eq!(nested[0]["inlineData"]["data"], "BASE64DATA");
    }

    /// On Gemini 2.5 and older, `functionResponse.parts` is rejected by the
    /// API, so a tool-result image must be emitted as a separate user message
    /// immediately after the functionResponse. The functionResponse itself
    /// must carry only text in `response`, with no nested `parts`.
    #[test]
    fn test_gemini_25_tool_result_image_uses_separate_message() {
        use zdx_types::{ToolResult, ToolResultBlock, ToolResultContent};

        let messages = vec![
            ChatMessage::user("take a screenshot"),
            ChatMessage {
                role: "assistant".to_string(),
                phase: None,
                content: MessageContent::Blocks(vec![ChatContentBlock::ToolUse {
                    id: "call_img".to_string(),
                    name: "screenshot".to_string(),
                    input: serde_json::json!({}),
                }]),
            },
            ChatMessage {
                role: "user".to_string(),
                phase: None,
                content: MessageContent::Blocks(vec![ChatContentBlock::ToolResult(ToolResult {
                    tool_use_id: "call_img".to_string(),
                    content: ToolResultContent::Blocks(vec![
                        ToolResultBlock::Text {
                            text: "captured".to_string(),
                        },
                        ToolResultBlock::Image {
                            mime_type: "image/png".to_string(),
                            data: "BASE64DATA".to_string(),
                        },
                    ]),
                    is_error: false,
                })]),
            },
        ];

        let contents = build_contents(&messages, "gemini-2.5-flash");

        // 4 messages: original user, assistant tool-use, user tool-result,
        // separate user image-only message.
        assert_eq!(
            contents.len(),
            4,
            "Gemini 2.5: image must be emitted as a separate user message"
        );

        // Message 3: functionResponse WITHOUT nested parts.
        let tool_result_msg = &contents[2];
        assert_eq!(tool_result_msg["role"], "user");
        let function_response = &tool_result_msg["parts"][0]["functionResponse"];
        assert_eq!(function_response["response"]["content"], "captured");
        assert!(
            function_response.get("parts").is_none(),
            "Gemini 2.5 must NOT nest parts inside functionResponse"
        );

        // Message 4: separate user message with the image.
        let image_msg = &contents[3];
        assert_eq!(image_msg["role"], "user");
        let image_parts = image_msg["parts"].as_array().unwrap();
        assert_eq!(image_parts.len(), 1);
        assert_eq!(image_parts[0]["inlineData"]["mimeType"], "image/png");
        assert_eq!(image_parts[0]["inlineData"]["data"], "BASE64DATA");
    }

    /// Multiple tool results in one user message, reverse-ordered relative to
    /// the assistant's tool uses, with ONLY ONE of them carrying an image.
    /// On Gemini 3 the image must nest under its matching `functionResponse`,
    /// and both responses must appear in the same user message, in the order
    /// the user message delivered them.
    #[test]
    fn test_gemini_3_multiple_tool_results_mixed_image() {
        use zdx_types::{ToolResult, ToolResultBlock, ToolResultContent};

        let messages = vec![
            ChatMessage::user("do two things"),
            ChatMessage {
                role: "assistant".to_string(),
                phase: None,
                content: MessageContent::Blocks(vec![
                    ChatContentBlock::ToolUse {
                        id: "call_a".to_string(),
                        name: "bash".to_string(),
                        input: serde_json::json!({"cmd": "ls"}),
                    },
                    ChatContentBlock::ToolUse {
                        id: "call_b".to_string(),
                        name: "screenshot".to_string(),
                        input: serde_json::json!({}),
                    },
                ]),
            },
            // User returns results in REVERSE order: b (with image) then a (text only).
            ChatMessage {
                role: "user".to_string(),
                phase: None,
                content: MessageContent::Blocks(vec![
                    ChatContentBlock::ToolResult(ToolResult {
                        tool_use_id: "call_b".to_string(),
                        content: ToolResultContent::Blocks(vec![
                            ToolResultBlock::Text {
                                text: "shot".to_string(),
                            },
                            ToolResultBlock::Image {
                                mime_type: "image/png".to_string(),
                                data: "IMG_B".to_string(),
                            },
                        ]),
                        is_error: false,
                    }),
                    ChatContentBlock::ToolResult(ToolResult {
                        tool_use_id: "call_a".to_string(),
                        content: ToolResultContent::Text("files listed".to_string()),
                        is_error: false,
                    }),
                ]),
            },
        ];

        let contents = build_contents(&messages, "gemini-3-flash-preview");

        // 3 messages: original user, assistant tool-uses, single user tool-result message.
        assert_eq!(contents.len(), 3, "Gemini 3: no separate image message");

        let parts = contents[2]["parts"].as_array().unwrap();
        assert_eq!(parts.len(), 2, "both functionResponses in one message");

        // Order preserved from the input ToolResult order (b first, then a).
        let fr0 = &parts[0]["functionResponse"];
        assert_eq!(fr0["id"], "call_b");
        assert_eq!(fr0["name"], "screenshot");
        let nested = fr0["parts"].as_array().unwrap();
        assert_eq!(nested.len(), 1);
        assert_eq!(nested[0]["inlineData"]["mimeType"], "image/png");
        assert_eq!(nested[0]["inlineData"]["data"], "IMG_B");

        let fr1 = &parts[1]["functionResponse"];
        assert_eq!(fr1["id"], "call_a");
        assert_eq!(fr1["name"], "bash");
        assert_eq!(fr1["response"]["content"], "files listed");
        assert!(
            fr1.get("parts").is_none(),
            "text-only result must not nest parts"
        );
    }

    /// Same scenario on Gemini 2.5: both `functionResponse`s in the first
    /// user message (text-only), followed by one trailing user message that
    /// carries the single image.
    #[test]
    fn test_gemini_25_multiple_tool_results_mixed_image() {
        use zdx_types::{ToolResult, ToolResultBlock, ToolResultContent};

        let messages = vec![
            ChatMessage::user("do two things"),
            ChatMessage {
                role: "assistant".to_string(),
                phase: None,
                content: MessageContent::Blocks(vec![
                    ChatContentBlock::ToolUse {
                        id: "call_a".to_string(),
                        name: "bash".to_string(),
                        input: serde_json::json!({"cmd": "ls"}),
                    },
                    ChatContentBlock::ToolUse {
                        id: "call_b".to_string(),
                        name: "screenshot".to_string(),
                        input: serde_json::json!({}),
                    },
                ]),
            },
            ChatMessage {
                role: "user".to_string(),
                phase: None,
                content: MessageContent::Blocks(vec![
                    ChatContentBlock::ToolResult(ToolResult {
                        tool_use_id: "call_b".to_string(),
                        content: ToolResultContent::Blocks(vec![
                            ToolResultBlock::Text {
                                text: "shot".to_string(),
                            },
                            ToolResultBlock::Image {
                                mime_type: "image/png".to_string(),
                                data: "IMG_B".to_string(),
                            },
                        ]),
                        is_error: false,
                    }),
                    ChatContentBlock::ToolResult(ToolResult {
                        tool_use_id: "call_a".to_string(),
                        content: ToolResultContent::Text("files listed".to_string()),
                        is_error: false,
                    }),
                ]),
            },
        ];

        let contents = build_contents(&messages, "gemini-2.5-flash");

        // 4 messages: user, assistant, user (both text-only functionResponses),
        // trailing user (single image).
        assert_eq!(
            contents.len(),
            4,
            "Gemini 2.5: one trailing image-only user message"
        );

        // Message 3: both functionResponses, no nested parts on either.
        let fr_parts = contents[2]["parts"].as_array().unwrap();
        assert_eq!(fr_parts.len(), 2);
        for fr in fr_parts {
            assert!(
                fr["functionResponse"].get("parts").is_none(),
                "Gemini 2.5 must never nest parts inside functionResponse"
            );
        }

        // Message 4: single image, from call_b.
        let image_parts = contents[3]["parts"].as_array().unwrap();
        assert_eq!(image_parts.len(), 1);
        assert_eq!(image_parts[0]["inlineData"]["data"], "IMG_B");
    }
}

#[cfg(test)]
mod integration_tests {
    use zdx_types::{ThinkingLevel, ToolDefinition};

    use super::*;

    /// Cloud Code Assist API does NOT support includeThoughts for any model
    #[test]
    fn test_build_cloud_code_request_no_include_thoughts_for_25() {
        let messages = vec![ChatMessage::user("hello")];
        let tools = vec![];
        let system = Some("You are helpful");

        // Gemini 2.5 Flash with minimal thinking
        let thinking_config =
            GeminiThinkingConfig::from_thinking_level(ThinkingLevel::Minimal, "gemini-2.5-flash");

        let request = build_cloud_code_assist_request(
            &messages,
            &tools,
            system,
            &CloudCodeRequestParams {
                model: "gemini-2.5-flash",
                project_id: "test-project",
                max_output_tokens: Some(8192),
                session_id: "test-session",
                prompt_seq: 0,
                thinking_config: Some(&thinking_config),
            },
        );

        // Check that includeThoughts is NOT present (Cloud Code Assist doesn't support it)
        let gen_config = &request["request"]["generationConfig"];
        assert!(
            gen_config.get("thinkingConfig").is_some(),
            "thinkingConfig should be present"
        );
        assert!(
            gen_config.get("includeThoughts").is_none(),
            "includeThoughts should NOT be present for Cloud Code Assist API"
        );
        assert!(
            gen_config["thinkingConfig"]
                .get("includeThoughts")
                .is_none(),
            "thinkingConfig.includeThoughts should NOT be present for Cloud Code Assist API"
        );
    }

    /// Cloud Code Assist API does NOT support includeThoughts even for Gemini 3 models
    #[test]
    fn test_build_cloud_code_request_no_include_thoughts_for_3() {
        let messages = vec![ChatMessage::user("hello")];
        let tools = vec![];
        let system = Some("You are helpful");

        // Gemini 3 Flash with minimal thinking
        let thinking_config = GeminiThinkingConfig::from_thinking_level(
            ThinkingLevel::Minimal,
            "gemini-3-flash-preview",
        );

        let request = build_cloud_code_assist_request(
            &messages,
            &tools,
            system,
            &CloudCodeRequestParams {
                model: "gemini-3-flash-preview",
                project_id: "test-project",
                max_output_tokens: Some(8192),
                session_id: "test-session",
                prompt_seq: 0,
                thinking_config: Some(&thinking_config),
            },
        );

        // Cloud Code Assist does NOT support includeThoughts (unlike standard Gemini API)
        let gen_config = &request["request"]["generationConfig"];
        assert!(
            gen_config.get("thinkingConfig").is_some(),
            "thinkingConfig should be present"
        );
        assert!(
            gen_config.get("includeThoughts").is_none(),
            "includeThoughts should NOT be present for Cloud Code Assist API"
        );
        assert!(
            gen_config["thinkingConfig"]
                .get("includeThoughts")
                .is_none(),
            "thinkingConfig.includeThoughts should NOT be present for Cloud Code Assist API"
        );
    }

    /// Standard Gemini API DOES support includeThoughts for Gemini 3 models
    #[test]
    fn test_build_gemini_request_include_thoughts_for_3() {
        let messages = vec![ChatMessage::user("hello")];
        let tools = vec![];
        let system = Some("You are helpful");

        // Gemini 3 Flash with minimal thinking
        let thinking_config = GeminiThinkingConfig::from_thinking_level(
            ThinkingLevel::Minimal,
            "gemini-3-flash-preview",
        );

        let request = build_gemini_request(
            &messages,
            &tools,
            system,
            Some(8192),
            Some(&thinking_config),
            "gemini-3-flash-preview",
        );

        // Standard Gemini API supports includeThoughts for Gemini 3
        let gen_config = &request["generationConfig"];
        assert!(
            gen_config.get("thinkingConfig").is_some(),
            "thinkingConfig should be present"
        );
        assert_eq!(
            gen_config["thinkingConfig"].get("includeThoughts"),
            Some(&serde_json::json!(true)),
            "thinkingConfig.includeThoughts should be true for Gemini 3 on standard API"
        );
        assert!(
            gen_config.get("includeThoughts").is_none(),
            "includeThoughts should not be at generationConfig top-level"
        );
    }

    /// Standard Gemini API DOES include includeThoughts for Gemini 2.5/2.0
    #[test]
    fn test_build_gemini_request_includes_thoughts_for_25() {
        let messages = vec![ChatMessage::user("hello")];
        let tools = vec![];
        let system = Some("You are helpful");

        // Gemini 2.5 Flash
        let thinking_config =
            GeminiThinkingConfig::from_thinking_level(ThinkingLevel::Medium, "gemini-2.5-flash");

        let request = build_gemini_request(
            &messages,
            &tools,
            system,
            Some(8192),
            Some(&thinking_config),
            "gemini-2.5-flash",
        );

        // Gemini 2.5 uses thinkingBudget, AND should include includeThoughts for standard API
        let gen_config = &request["generationConfig"];
        assert!(
            gen_config.get("thinkingConfig").is_some(),
            "thinkingConfig should be present"
        );
        assert!(
            gen_config["thinkingConfig"].get("thinkingBudget").is_some(),
            "should use thinkingBudget for 2.5"
        );
        assert_eq!(
            gen_config["thinkingConfig"].get("includeThoughts"),
            Some(&serde_json::json!(true)),
            "includeThoughts should be present for Gemini 2.5"
        );
    }

    #[test]
    fn test_build_tools_strips_additional_properties() {
        let tools = vec![ToolDefinition {
            name: "Bash".to_string(),
            description: "Run shell command".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "command": {
                        "type": "string",
                        "additionalProperties": false
                    }
                },
                "required": ["command"],
                "additionalProperties": false
            }),
        }];

        let built = build_tools(&tools).expect("tools should be present");
        let parameters = &built[0]["functionDeclarations"][0]["parameters"];

        assert!(
            parameters.get("additionalProperties").is_none(),
            "top-level additionalProperties must be stripped"
        );
        assert!(
            parameters["properties"]["command"]
                .get("additionalProperties")
                .is_none(),
            "nested additionalProperties must be stripped"
        );
    }

    /// `build_tools` must emit the canonical camelCase `functionDeclarations`
    /// wrapper key (matching Google's published request shape) and
    /// `build_gemini_request` must use `systemInstruction` (camelCase).
    #[test]
    fn test_request_uses_canonical_camel_case_field_names() {
        let tools = vec![ToolDefinition {
            name: "Bash".to_string(),
            description: "Run shell command".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "command": {"type": "string"}
                },
                "required": ["command"]
            }),
        }];
        let messages = vec![ChatMessage::user("hello")];

        let built = build_tools(&tools).expect("tools");
        assert!(
            built[0].get("functionDeclarations").is_some(),
            "tools wrapper must use camelCase functionDeclarations"
        );
        assert!(
            built[0].get("function_declarations").is_none(),
            "snake_case function_declarations must not appear"
        );

        let request = build_gemini_request(
            &messages,
            &tools,
            Some("system prompt"),
            None,
            None,
            "gemini-3-flash-preview",
        );

        assert!(
            request.get("systemInstruction").is_some(),
            "request must use camelCase systemInstruction"
        );
        assert!(
            request.get("system_instruction").is_none(),
            "snake_case system_instruction must not appear"
        );
        assert!(
            request["tools"][0].get("functionDeclarations").is_some(),
            "tools must use camelCase functionDeclarations"
        );
    }

    /// When `thinking_config` is None, no thinkingConfig should be present.
    #[test]
    fn test_build_gemini_request_no_thinking_config_when_disabled() {
        let messages = vec![ChatMessage::user("hello")];
        let tools = vec![];
        let system = Some("You are helpful");

        let request = build_gemini_request(
            &messages,
            &tools,
            system,
            Some(8192),
            None,
            "gemini-3-flash-preview",
        );
        let gen_config = &request["generationConfig"];
        assert!(
            gen_config.get("thinkingConfig").is_none(),
            "thinkingConfig should NOT be present when None"
        );
    }

    /// Gemini 2.5 Pro: Off maps to minimum budget (128)
    #[test]
    fn test_thinking_config_gemini_25_pro_off() {
        let config =
            GeminiThinkingConfig::from_thinking_level(ThinkingLevel::Off, "gemini-2.5-pro");
        assert!(matches!(config, GeminiThinkingConfig::Budget(128)));
    }

    /// Gemini 2.5 Pro: `XHigh` maps to max budget (32768)
    #[test]
    fn test_thinking_config_gemini_25_pro_xhigh() {
        let config =
            GeminiThinkingConfig::from_thinking_level(ThinkingLevel::XHigh, "gemini-2.5-pro");
        assert!(matches!(config, GeminiThinkingConfig::Budget(32768)));
    }

    /// Non-flash-lite Gemini 2.5: Minimal maps to 1024
    #[test]
    fn test_thinking_config_gemini_25_flash_minimal() {
        let config =
            GeminiThinkingConfig::from_thinking_level(ThinkingLevel::Minimal, "gemini-2.5-flash");
        assert!(matches!(config, GeminiThinkingConfig::Budget(1024)));
    }

    /// Gemini 3 `XHigh` maps to high (since `XHigh` isn't a Gemini level)
    #[test]
    fn test_thinking_config_gemini_3_xhigh() {
        // Both Pro and Flash should map XHigh to "high"
        let config_pro =
            GeminiThinkingConfig::from_thinking_level(ThinkingLevel::XHigh, "gemini-3-pro-preview");
        assert!(matches!(config_pro, GeminiThinkingConfig::Level(ref l) if l == "high"));

        let config_flash = GeminiThinkingConfig::from_thinking_level(
            ThinkingLevel::XHigh,
            "gemini-3-flash-preview",
        );
        assert!(matches!(config_flash, GeminiThinkingConfig::Level(ref l) if l == "high"));
    }
}
