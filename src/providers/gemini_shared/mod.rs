//! Shared Gemini API helpers for both API key and OAuth providers.
//!
//! This module contains common code for:
//! - SSE parsing (GeminiSseParser)
//! - Message conversion to Gemini format
//! - Error classification
//! - Common utility functions

pub mod sse;

use std::collections::HashMap;

use anyhow::Result;
use serde_json::{Value, json};

use crate::providers::{
    ChatContentBlock, ChatMessage, MessageContent, ProviderError, ProviderErrorKind,
};
use crate::tools::{ToolDefinition, ToolResultBlock, ToolResultContent};

/// Synthetic thought signature for active loop messages.
pub const SYNTHETIC_THOUGHT_SIGNATURE: &str = "skip_thought_signature_validator";

/// Classifies a reqwest error into a ProviderError.
pub fn classify_reqwest_error(e: reqwest::Error) -> ProviderError {
    if e.is_timeout() {
        ProviderError::timeout(format!("Request timed out: {}", e))
    } else if e.is_connect() {
        ProviderError::timeout(format!("Connection failed: {}", e))
    } else if e.is_request() {
        ProviderError::new(
            ProviderErrorKind::HttpStatus,
            format!("Request error: {}", e),
        )
    } else {
        ProviderError::new(
            ProviderErrorKind::HttpStatus,
            format!("Network error: {}", e),
        )
    }
}

/// Builds Gemini-format contents array from chat messages.
///
/// Returns the contents array and a tool name map for resolving tool results.
pub fn build_contents(messages: &[ChatMessage]) -> (Vec<Value>, HashMap<String, String>) {
    let active_loop_start = active_loop_start_index(messages);
    let mut contents = Vec::new();
    let mut tool_name_map: HashMap<String, String> = HashMap::new();

    for (idx, msg) in messages.iter().enumerate() {
        let add_thought_signature = idx >= active_loop_start;
        match (&msg.role[..], &msg.content) {
            ("user", MessageContent::Text(text)) => {
                contents.push(json!({
                    "role": "user",
                    "parts": [{"text": text}]
                }));
            }
            ("assistant", MessageContent::Text(text)) => {
                contents.push(json!({
                    "role": "model",
                    "parts": [{"text": text}]
                }));
            }
            ("assistant", MessageContent::Blocks(blocks)) => {
                let mut parts = Vec::new();
                let mut added_signature = false;
                for block in blocks {
                    match block {
                        ChatContentBlock::Text(text) => {
                            parts.push(json!({"text": text}));
                        }
                        ChatContentBlock::Image { mime_type, data } => {
                            parts.push(json!({
                                "inlineData": {
                                    "mimeType": mime_type,
                                    "data": data
                                }
                            }));
                        }
                        ChatContentBlock::ToolUse { id, name, input } => {
                            tool_name_map.insert(id.clone(), name.clone());
                            let mut part = json!({
                                "functionCall": {
                                    "name": name,
                                    "args": input
                                }
                            });
                            if add_thought_signature && !added_signature {
                                part["thoughtSignature"] = json!(SYNTHETIC_THOUGHT_SIGNATURE);
                                added_signature = true;
                            }
                            parts.push(part);
                        }
                        ChatContentBlock::Thinking { .. } => {}
                        ChatContentBlock::ToolResult(_) => {}
                    }
                }

                if !parts.is_empty() {
                    contents.push(json!({
                        "role": "model",
                        "parts": parts
                    }));
                }
            }
            ("user", MessageContent::Blocks(blocks)) => {
                let mut parts = Vec::new();
                let mut tool_results = Vec::new();
                let mut pending_images: Vec<(String, String)> = Vec::new();

                for block in blocks {
                    match block {
                        ChatContentBlock::Text(text) => parts.push(json!({"text": text})),
                        ChatContentBlock::Image { mime_type, data } => {
                            parts.push(json!({
                                "inlineData": {
                                    "mimeType": mime_type,
                                    "data": data
                                }
                            }));
                        }
                        ChatContentBlock::ToolResult(result) => tool_results.push(result),
                        _ => {}
                    }
                }

                for result in tool_results {
                    if let Some(name) = tool_name_map.get(&result.tool_use_id) {
                        // Get text and optional image from tool result
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

                        // Collect images to add as separate message after function responses
                        // (Gemini may not process inlineData mixed with functionResponse)
                        if let Some((mime_type, data)) = image {
                            pending_images.push((mime_type, data));
                        }
                    }
                }

                if !parts.is_empty() {
                    contents.push(json!({
                        "role": "user",
                        "parts": parts
                    }));
                }

                // Add tool result images as a separate user message
                // This ensures Gemini processes them as visual input
                if !pending_images.is_empty() {
                    let image_parts: Vec<Value> = pending_images
                        .into_iter()
                        .map(|(mime_type, data)| {
                            json!({
                                "inlineData": {
                                    "mimeType": mime_type,
                                    "data": data
                                }
                            })
                        })
                        .collect();
                    contents.push(json!({
                        "role": "user",
                        "parts": image_parts
                    }));
                }
            }
            _ => {}
        }
    }

    (contents, tool_name_map)
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
                        json!({
                            "name": tool.name,
                            "description": tool.description,
                            "parameters": tool.input_schema
                        })
                    })
                    .collect::<Vec<_>>()
            }
        ]))
    }
}

/// Builds a standard Gemini API request body (for API key auth).
pub fn build_gemini_request(
    messages: &[ChatMessage],
    tools: &[ToolDefinition],
    system: Option<&str>,
    max_output_tokens: u32,
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

    if max_output_tokens > 0 {
        request["generation_config"] = json!({
            "max_output_tokens": max_output_tokens
        });
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
}

/// Builds a Cloud Code Assist request body (for OAuth auth).
///
/// `session_id` and `prompt_seq` are used to generate `user_prompt_id` in the format
/// used by the official Gemini CLI: `<session_id>########<seq>`.
pub fn build_cloud_code_assist_request(
    messages: &[ChatMessage],
    tools: &[ToolDefinition],
    system: Option<&str>,
    params: CloudCodeRequestParams<'_>,
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

    if let Some(tokens) = params.max_output_tokens
        && tokens > 0
    {
        inner_request["generationConfig"] = json!({
            "maxOutputTokens": tokens
        });
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
/// Returns (text, Option<(mime_type, base64_data)>)
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
                    _ => None,
                })
                .unwrap_or_default();

            let image = blocks.iter().find_map(|block| match block {
                ToolResultBlock::Image { mime_type, data } => {
                    Some((mime_type.clone(), data.clone()))
                }
                _ => None,
            });

            (text, image)
        }
    }
}
