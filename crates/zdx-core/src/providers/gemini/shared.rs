//! Shared Gemini API helpers for both API key and OAuth providers.
//!
//! This module contains common code for:
//! - SSE parsing (`GeminiSseParser`)
//! - Message conversion to Gemini format
//! - Error classification
//! - Common utility functions

use std::collections::HashMap;

use anyhow::Result;
use serde_json::{Value, json};

use crate::config::ThinkingLevel;
use crate::providers::{
    ChatContentBlock, ChatMessage, MessageContent, ProviderError, ProviderErrorKind, ReplayToken,
};
use crate::tools::{ToolDefinition, ToolResultBlock, ToolResultContent};

/// Thinking configuration for Gemini models.
///
/// Gemini 3 models use `thinkingLevel` (string levels).
/// Gemini 2.5 models use `thinkingBudget` (token count).
#[derive(Debug, Clone)]
pub enum GeminiThinkingConfig {
    /// For Gemini 3 models: use thinking level strings.
    /// Valid values depend on model:
    /// - Gemini 3 Pro: "low", "high"
    /// - Gemini 3 Flash: "minimal", "low", "medium", "high"
    Level(String),
    /// For Gemini 2.5 models: use token budget.
    /// -1 = dynamic (default), 0 = disabled, positive = specific budget.
    Budget(i32),
    /// Use model's default (don't include thinkingConfig in request).
    Default,
}

impl GeminiThinkingConfig {
    /// Maps zdx's `ThinkingLevel` to Gemini-specific config based on model name.
    ///
    /// For Gemini 3 models: maps to thinkingLevel strings.
    /// For Gemini 2.5 models: maps to thinkingBudget tokens.
    pub fn from_thinking_level(level: ThinkingLevel, model: &str) -> Self {
        // Check if model is Gemini 3 (use thinkingLevel)
        let is_gemini_3 = model.contains("gemini-3");
        let is_gemini_3_pro = model.contains("gemini-3-pro");

        if is_gemini_3 {
            // Gemini 3 models use thinkingLevel
            match level {
                ThinkingLevel::Off => {
                    // Cannot disable thinking on Gemini 3 Pro
                    // For Flash, use "minimal" (closest to off)
                    if is_gemini_3_pro {
                        Self::Level("low".to_string())
                    } else {
                        Self::Level("minimal".to_string())
                    }
                }
                ThinkingLevel::Minimal => {
                    // Gemini 3 Pro doesn't support minimal
                    if is_gemini_3_pro {
                        Self::Level("low".to_string())
                    } else {
                        Self::Level("minimal".to_string())
                    }
                }
                ThinkingLevel::Low => Self::Level("low".to_string()),
                ThinkingLevel::Medium => {
                    // Gemini 3 Pro doesn't support medium
                    if is_gemini_3_pro {
                        Self::Level("high".to_string())
                    } else {
                        Self::Level("medium".to_string())
                    }
                }
                ThinkingLevel::High | ThinkingLevel::XHigh => Self::Level("high".to_string()),
            }
        } else {
            // Gemini 2.5 models use thinkingBudget
            // Map thinking levels to appropriate token budgets
            let is_flash_lite = model.contains("flash-lite");

            match level {
                ThinkingLevel::Off => {
                    // 2.5 Pro cannot disable thinking, use minimum budget
                    if model.contains("2.5-pro") || model.contains("2.5 pro") {
                        Self::Budget(128)
                    } else {
                        Self::Budget(0)
                    }
                }
                ThinkingLevel::Minimal => {
                    // Flash Lite minimum is 512, others can go to 0
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
                    // Max budget depends on model
                    if model.contains("2.5-pro") || model.contains("2.5 pro") {
                        Self::Budget(32768)
                    } else {
                        Self::Budget(24576)
                    }
                }
            }
        }
    }

    /// Converts to JSON value for inclusion in generationConfig.
    /// Returns None if Default (don't include in request).
    pub fn to_json(&self) -> Option<Value> {
        match self {
            GeminiThinkingConfig::Level(level) => Some(json!({
                "thinkingConfig": {
                    "thinkingLevel": level
                }
            })),
            GeminiThinkingConfig::Budget(tokens) => Some(json!({
                "thinkingConfig": {
                    "thinkingBudget": tokens
                }
            })),
            GeminiThinkingConfig::Default => None,
        }
    }
}

/// Synthetic thought signature for active loop messages.
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
/// Returns the contents array and a tool name map for resolving tool results.
pub fn build_contents(messages: &[ChatMessage]) -> (Vec<Value>, HashMap<String, String>) {
    let mut builder = GeminiContentsBuilder::new(active_loop_start_index(messages));
    for (idx, msg) in messages.iter().enumerate() {
        builder.append_message(idx, msg);
    }
    (builder.contents, builder.tool_name_map)
}

struct GeminiContentsBuilder {
    active_loop_start: usize,
    contents: Vec<Value>,
    tool_name_map: HashMap<String, String>,
}

impl GeminiContentsBuilder {
    fn new(active_loop_start: usize) -> Self {
        Self {
            active_loop_start,
            contents: Vec::new(),
            tool_name_map: HashMap::new(),
        }
    }

    fn append_message(&mut self, idx: usize, msg: &ChatMessage) {
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
                self.append_assistant_blocks(idx >= self.active_loop_start, blocks);
            }
            ("user", MessageContent::Blocks(blocks)) => self.append_user_blocks(blocks),
            _ => {}
        }
    }

    fn append_assistant_blocks(
        &mut self,
        add_thought_signature: bool,
        blocks: &[ChatContentBlock],
    ) {
        let mut parts = Vec::new();
        let mut added_signature = false;
        let real_signature = gemini_signature(blocks);

        for block in blocks {
            match block {
                ChatContentBlock::Text(text) => parts.push(text_part(text)),
                ChatContentBlock::Image { mime_type, data } => {
                    parts.push(inline_data_part(mime_type, data));
                }
                ChatContentBlock::ToolUse { id, name, input } => {
                    self.tool_name_map.insert(id.clone(), name.clone());
                    let mut part = json!({
                        "functionCall": {
                            "name": name,
                            "args": input
                        }
                    });
                    if add_thought_signature && !added_signature {
                        let signature = real_signature
                            .as_deref()
                            .unwrap_or(SYNTHETIC_THOUGHT_SIGNATURE);
                        part["thoughtSignature"] = json!(signature);
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
        let mut parts = Vec::new();
        let mut tool_results = Vec::new();

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

        let mut pending_images = Vec::new();
        for result in tool_results {
            let Some(name) = self.tool_name_map.get(&result.tool_use_id) else {
                continue;
            };

            let (text, image) = extract_tool_result_with_image(&result.content);
            parts.push(json!({
                "functionResponse": {
                    "name": name,
                    "response": {
                        "content": text,
                        "is_error": result.is_error
                    }
                }
            }));
            if let Some(image) = image {
                pending_images.push(image);
            }
        }

        if !parts.is_empty() {
            self.push_message("user", &parts);
        }
        if !pending_images.is_empty() {
            let image_parts = pending_images
                .into_iter()
                .map(|(mime_type, data)| inline_data_part(&mime_type, &data))
                .collect::<Vec<_>>();
            self.push_message("user", &image_parts);
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
                "function_declarations": tools
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
/// # Errors
/// Returns an error if the operation fails.
pub fn build_gemini_request(
    messages: &[ChatMessage],
    tools: &[ToolDefinition],
    system: Option<&str>,
    max_output_tokens: u32,
    thinking_config: Option<&GeminiThinkingConfig>,
) -> Result<Value> {
    let (contents, _) = build_contents(messages);
    let tools_value = build_tools(tools);

    let mut request = json!({
        "contents": contents,
    });

    if let Some(prompt) = system
        && !prompt.trim().is_empty()
    {
        request["system_instruction"] = json!({
            "parts": [{"text": prompt}]
        });
    }

    if let Some(tools_value) = tools_value {
        request["tools"] = tools_value;
    }

    // Build generationConfig with max_output_tokens and optional thinkingConfig
    let mut generation_config = json!({});
    if max_output_tokens > 0 {
        generation_config["maxOutputTokens"] = json!(max_output_tokens);
    }

    // Add thinking config if specified and not Default
    if let Some(thinking) = thinking_config
        && let Some(thinking_json) = thinking.to_json()
        && let Some(thinking_config_obj) = thinking_json.get("thinkingConfig")
    {
        let mut thinking_config_obj = thinking_config_obj.clone();
        // Request thought summaries when thinking is enabled (Gemini 3 only)
        // includeThoughts is not supported by Gemini 2.5 models (which use thinkingBudget)
        if matches!(thinking, GeminiThinkingConfig::Level(_)) {
            thinking_config_obj["includeThoughts"] = json!(true);
        }
        generation_config["thinkingConfig"] = thinking_config_obj;
    }

    // Only add generationConfig if it has content
    if generation_config.as_object().is_some_and(|o| !o.is_empty()) {
        request["generationConfig"] = generation_config;
    }

    Ok(request)
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
///
/// # Errors
/// Returns an error if the operation fails.
pub fn build_cloud_code_assist_request(
    messages: &[ChatMessage],
    tools: &[ToolDefinition],
    system: Option<&str>,
    params: &CloudCodeRequestParams<'_>,
) -> Result<Value> {
    let (contents, _) = build_contents(messages);
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

    // Build generationConfig with max_output_tokens and optional thinkingConfig
    let mut generation_config = json!({});
    if let Some(tokens) = params.max_output_tokens
        && tokens > 0
    {
        generation_config["maxOutputTokens"] = json!(tokens);
    }

    // Add thinking config if specified and not Default
    // Note: Cloud Code Assist API does NOT support includeThoughts field
    // (unlike the standard Gemini API at generativelanguage.googleapis.com)
    if let Some(thinking) = params.thinking_config
        && let Some(thinking_json) = thinking.to_json()
        && let Some(thinking_config_obj) = thinking_json.get("thinkingConfig")
    {
        generation_config["thinkingConfig"] = thinking_config_obj.clone();
    }

    // Only add generationConfig if it has content
    if generation_config.as_object().is_some_and(|o| !o.is_empty()) {
        inner_request["generationConfig"] = generation_config;
    }

    // Format matches official Gemini CLI: <session_id>########<seq>
    let user_prompt_id = format!("{}########{}", params.session_id, params.prompt_seq);

    let request = json!({
        "project": params.project_id,
        "model": params.model,
        "user_prompt_id": user_prompt_id,
        "request": inner_request,
    });

    Ok(request)
}

fn active_loop_start_index(messages: &[ChatMessage]) -> usize {
    messages.iter().rposition(matches_user_text).unwrap_or(0)
}

fn matches_user_text(msg: &ChatMessage) -> bool {
    if msg.role != "user" {
        return false;
    }

    match &msg.content {
        MessageContent::Text(text) => !text.trim().is_empty(),
        MessageContent::Blocks(blocks) => blocks.iter().any(|block| match block {
            ChatContentBlock::Text(text) => !text.trim().is_empty(),
            _ => false,
        }),
    }
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

    /// Gemini 3 Pro: maps to thinkingLevel, no minimal/medium support.
    #[test]
    fn test_thinking_config_gemini_3_pro() {
        // Off -> low (Pro can't disable)
        let config =
            GeminiThinkingConfig::from_thinking_level(ThinkingLevel::Off, "gemini-3-pro-preview");
        assert!(matches!(config, GeminiThinkingConfig::Level(ref l) if l == "low"));

        // Minimal -> low (Pro doesn't support minimal)
        let config = GeminiThinkingConfig::from_thinking_level(
            ThinkingLevel::Minimal,
            "gemini-3-pro-preview",
        );
        assert!(matches!(config, GeminiThinkingConfig::Level(ref l) if l == "low"));

        // Low -> low
        let config =
            GeminiThinkingConfig::from_thinking_level(ThinkingLevel::Low, "gemini-3-pro-preview");
        assert!(matches!(config, GeminiThinkingConfig::Level(ref l) if l == "low"));

        // Medium -> high (Pro doesn't support medium)
        let config = GeminiThinkingConfig::from_thinking_level(
            ThinkingLevel::Medium,
            "gemini-3-pro-preview",
        );
        assert!(matches!(config, GeminiThinkingConfig::Level(ref l) if l == "high"));

        // High -> high
        let config =
            GeminiThinkingConfig::from_thinking_level(ThinkingLevel::High, "gemini-3-pro-preview");
        assert!(matches!(config, GeminiThinkingConfig::Level(ref l) if l == "high"));
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
        let json = config.to_json().unwrap();
        assert_eq!(json["thinkingConfig"]["thinkingLevel"], "medium");

        // Budget produces thinkingBudget
        let config = GeminiThinkingConfig::Budget(8192);
        let json = config.to_json().unwrap();
        assert_eq!(json["thinkingConfig"]["thinkingBudget"], 8192);

        // Default produces None
        let config = GeminiThinkingConfig::Default;
        assert!(config.to_json().is_none());
    }

    /// `build_contents` uses real Gemini thought signature when available.
    #[test]
    fn test_build_contents_uses_real_gemini_signature() {
        use crate::providers::{ReasoningBlock, ReplayToken};

        // Create a message history with:
        // 1. User message
        // 2. Assistant message with reasoning block (Gemini signature) + tool use
        // 3. Tool result
        let messages = vec![
            ChatMessage::user("What files are here?"),
            ChatMessage {
                role: "assistant".to_string(),
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

        let (contents, _) = build_contents(&messages);

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
    }

    /// `build_contents` falls back to synthetic signature when no Gemini signature available.
    #[test]
    fn test_build_contents_fallback_to_synthetic_signature() {
        // Create a message history without reasoning blocks
        let messages = vec![
            ChatMessage::user("What files are here?"),
            ChatMessage {
                role: "assistant".to_string(),
                content: MessageContent::Blocks(vec![ChatContentBlock::ToolUse {
                    id: "tool-1".to_string(),
                    name: "bash".to_string(),
                    input: serde_json::json!({"command": "ls"}),
                }]),
            },
        ];

        let (contents, _) = build_contents(&messages);

        let assistant_msg = &contents[1];
        let parts = assistant_msg["parts"].as_array().unwrap();
        let function_call_part = &parts[0];

        // Should fall back to synthetic signature
        assert_eq!(
            function_call_part["thoughtSignature"],
            SYNTHETIC_THOUGHT_SIGNATURE
        );
    }
}

#[cfg(test)]
mod integration_tests {
    use super::*;
    use crate::config::ThinkingLevel;
    use crate::tools::ToolDefinition;

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
        )
        .unwrap();

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
        )
        .unwrap();

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

        let request =
            build_gemini_request(&messages, &tools, system, 8192, Some(&thinking_config)).unwrap();

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

    /// Standard Gemini API does NOT include includeThoughts for Gemini 2.5 (uses thinkingBudget)
    #[test]
    fn test_build_gemini_request_no_include_thoughts_for_25() {
        let messages = vec![ChatMessage::user("hello")];
        let tools = vec![];
        let system = Some("You are helpful");

        // Gemini 2.5 Flash
        let thinking_config =
            GeminiThinkingConfig::from_thinking_level(ThinkingLevel::Medium, "gemini-2.5-flash");

        let request =
            build_gemini_request(&messages, &tools, system, 8192, Some(&thinking_config)).unwrap();

        // Gemini 2.5 uses thinkingBudget, not thinkingLevel, so no includeThoughts
        let gen_config = &request["generationConfig"];
        assert!(
            gen_config.get("thinkingConfig").is_some(),
            "thinkingConfig should be present"
        );
        assert!(
            gen_config["thinkingConfig"].get("thinkingBudget").is_some(),
            "should use thinkingBudget for 2.5"
        );
        assert!(
            gen_config.get("includeThoughts").is_none(),
            "includeThoughts should NOT be present for Gemini 2.5"
        );
        assert!(
            gen_config["thinkingConfig"]
                .get("includeThoughts")
                .is_none(),
            "thinkingConfig.includeThoughts should NOT be present for Gemini 2.5"
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
        let parameters = &built[0]["function_declarations"][0]["parameters"];

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

    /// When `thinking_config` is None or Default, no thinkingConfig should be present
    #[test]
    fn test_build_gemini_request_no_thinking_config_when_disabled() {
        let messages = vec![ChatMessage::user("hello")];
        let tools = vec![];
        let system = Some("You are helpful");

        // Test with None
        let request = build_gemini_request(&messages, &tools, system, 8192, None).unwrap();
        let gen_config = &request["generationConfig"];
        assert!(
            gen_config.get("thinkingConfig").is_none(),
            "thinkingConfig should NOT be present when None"
        );

        // Test with Default variant
        let thinking_config = GeminiThinkingConfig::Default;
        let request =
            build_gemini_request(&messages, &tools, system, 8192, Some(&thinking_config)).unwrap();
        let gen_config = &request["generationConfig"];
        assert!(
            gen_config.get("thinkingConfig").is_none(),
            "thinkingConfig should NOT be present for Default"
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
