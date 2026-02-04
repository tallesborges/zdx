//! Shared OpenAI-compatible Responses API helpers.

use anyhow::{Result, bail};
use reqwest::header::HeaderMap;

pub use super::responses_sse::ResponsesSseParser;
pub use super::responses_types::{
    FunctionTool, InputContent, InputItem, ReasoningConfig, RequestBody, StreamOptions,
    SummaryItem, TextConfig,
};
use crate::providers::debug_metrics::maybe_wrap_with_metrics;
use crate::providers::{
    ChatContentBlock, ChatMessage, DebugTrace, ProviderError, ProviderErrorKind, ProviderStream,
    ReasoningBlock, ReplayToken, wrap_stream,
};
use crate::tools::{ToolDefinition, ToolResultContent};

/// Shared configuration for Responses API requests.
#[derive(Debug, Clone)]
pub struct ResponsesConfig {
    pub base_url: String,
    pub path: String,
    pub model: String,
    pub max_output_tokens: Option<u32>,
    pub reasoning_effort: Option<String>,
    pub instructions: Option<String>,
    pub text_verbosity: Option<String>,
    pub store: Option<bool>,
    pub include: Option<Vec<String>>,
    pub stream_options: Option<StreamOptions>,
    pub prompt_cache_key: Option<String>,
    pub parallel_tool_calls: Option<bool>,
    /// Tool selection strategy: "auto" (default), "required", or "none"
    pub tool_choice: Option<String>,
    /// Truncation strategy: "auto" or "disabled" (default)
    /// "auto" drops items from conversation start if context is exceeded
    pub truncation: Option<String>,
}

/// Sends a Responses API request and returns a stream of normalized events.
pub async fn send_responses_stream(
    http: &reqwest::Client,
    config: &ResponsesConfig,
    headers: HeaderMap,
    messages: &[ChatMessage],
    tools: &[ToolDefinition],
    system: Option<&str>,
) -> Result<ProviderStream> {
    let input = build_input(messages, system)?;
    if input.is_empty() {
        bail!("No input messages provided for OpenAI request");
    }

    let tool_defs = if tools.is_empty() {
        None
    } else {
        Some(tools.iter().map(FunctionTool::from).collect())
    };

    let request = RequestBody {
        model: config.model.clone(),
        stream: true,
        stream_options: config.stream_options.clone(),
        store: config.store,
        max_output_tokens: config.max_output_tokens,
        instructions: config.instructions.clone(),
        text: config.text_verbosity.as_ref().map(|verbosity| TextConfig {
            verbosity: verbosity.clone(),
        }),
        reasoning: config
            .reasoning_effort
            .as_ref()
            .map(|effort| ReasoningConfig {
                effort: effort.clone(),
                summary: Some("detailed".to_string()),
            }),
        include: config.include.clone(),
        input,
        tools: tool_defs,
        tool_choice: config.tool_choice.clone(),
        truncation: config.truncation.clone(),
        prompt_cache_key: config.prompt_cache_key.clone(),
        parallel_tool_calls: config.parallel_tool_calls,
    };

    let url = format!("{}{}", config.base_url, config.path);

    let trace = DebugTrace::from_env(&config.model, config.prompt_cache_key.as_deref());

    let response = if let Some(trace) = &trace {
        let body = serde_json::to_vec(&request)?;
        trace.write_request(&body);
        http.post(&url)
            .headers(headers)
            .body(body)
            .send()
            .await
            .map_err(classify_reqwest_error)?
    } else {
        http.post(&url)
            .headers(headers)
            .json(&request)
            .send()
            .await
            .map_err(classify_reqwest_error)?
    };

    let status = response.status();
    if !status.is_success() {
        let error_body = response.text().await.unwrap_or_default();
        return Err(ProviderError::http_status(status.as_u16(), &error_body).into());
    }

    let byte_stream = wrap_stream(trace, response.bytes_stream());
    let event_stream = ResponsesSseParser::new(byte_stream, config.model.clone());

    Ok(maybe_wrap_with_metrics(event_stream))
}

fn classify_reqwest_error(e: reqwest::Error) -> ProviderError {
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

fn build_input(messages: &[ChatMessage], system: Option<&str>) -> Result<Vec<InputItem>> {
    use crate::providers::MessageContent;

    let mut input = Vec::new();
    if let Some(prompt) = system {
        input.push(InputItem {
            id: None,
            item_type: "message".to_string(),
            role: Some("developer".to_string()),
            content: Some(vec![InputContent::InputText {
                text: prompt.to_string(),
            }]),
            call_id: None,
            name: None,
            arguments: None,
            output: None,
            encrypted_content: None,
            summary: None,
        });
    }

    for msg in messages {
        match (&msg.role[..], &msg.content) {
            ("user", MessageContent::Text(text)) => {
                input.push(InputItem {
                    id: None,
                    item_type: "message".to_string(),
                    role: Some("user".to_string()),
                    content: Some(vec![InputContent::InputText { text: text.clone() }]),
                    call_id: None,
                    name: None,
                    arguments: None,
                    output: None,
                    encrypted_content: None,
                    summary: None,
                });
            }
            ("assistant", MessageContent::Text(text)) => {
                input.push(InputItem {
                    id: None,
                    item_type: "message".to_string(),
                    role: Some("assistant".to_string()),
                    content: Some(vec![InputContent::OutputText { text: text.clone() }]),
                    call_id: None,
                    name: None,
                    arguments: None,
                    output: None,
                    encrypted_content: None,
                    summary: None,
                });
            }
            ("assistant", MessageContent::Blocks(blocks)) => {
                for block in blocks {
                    match block {
                        ChatContentBlock::Text(text) => {
                            input.push(InputItem {
                                id: None,
                                item_type: "message".to_string(),
                                role: Some("assistant".to_string()),
                                content: Some(vec![InputContent::OutputText {
                                    text: text.clone(),
                                }]),
                                call_id: None,
                                name: None,
                                arguments: None,
                                output: None,
                                encrypted_content: None,
                                summary: None,
                            });
                        }
                        // Only replay reasoning blocks with OpenAI replay tokens
                        ChatContentBlock::Reasoning(ReasoningBlock { text, replay }) => {
                            if let Some(ReplayToken::OpenAI {
                                id,
                                encrypted_content,
                            }) = replay.as_ref()
                            {
                                // Summary is REQUIRED when replaying reasoning items.
                                // Use the text if available and non-empty, otherwise use a placeholder.
                                let summary_text = text
                                    .as_ref()
                                    .filter(|t| !t.trim().is_empty())
                                    .cloned()
                                    .unwrap_or_else(|| "(reasoning)".to_string());
                                let summary_items = vec![SummaryItem {
                                    item_type: "summary_text",
                                    text: summary_text,
                                }];
                                input.push(InputItem {
                                    id: Some(id.clone()),
                                    item_type: "reasoning".to_string(),
                                    role: None,
                                    content: None,
                                    call_id: None,
                                    name: None,
                                    arguments: None,
                                    output: None,
                                    encrypted_content: Some(encrypted_content.clone()),
                                    summary: Some(summary_items),
                                });
                            }
                        }
                        ChatContentBlock::ToolUse {
                            id,
                            name,
                            input: arguments,
                        } => {
                            let arguments = serde_json::to_string(arguments)
                                .unwrap_or_else(|_| "{}".to_string());
                            let mut parts = id.split('|');
                            let call_id = parts.next().unwrap_or("");
                            let tool_id = parts.next().unwrap_or("");
                            let call_id = (!call_id.is_empty()).then_some(call_id.to_string());
                            let tool_id = (!tool_id.is_empty()).then_some(tool_id.to_string());

                            input.push(InputItem {
                                id: tool_id,
                                item_type: "function_call".to_string(),
                                role: None,
                                content: None,
                                call_id,
                                name: Some(name.clone()),
                                arguments: Some(arguments),
                                output: None,
                                encrypted_content: None,
                                summary: None,
                            });
                        }
                        ChatContentBlock::ToolResult(result) => {
                            let (output, has_image) =
                                extract_tool_result_with_image(&result.content);

                            let call_id = result
                                .tool_use_id
                                .split('|')
                                .next()
                                .unwrap_or("")
                                .to_string();

                            if call_id.is_empty() {
                                continue;
                            }

                            // Add the function call output (text part)
                            input.push(InputItem {
                                id: None,
                                item_type: "function_call_output".to_string(),
                                role: None,
                                content: None,
                                call_id: Some(call_id),
                                name: None,
                                arguments: None,
                                output: Some(output),
                                encrypted_content: None,
                                summary: None,
                            });

                            // If there's an image, add it as a separate user message
                            // OpenAI Responses API doesn't support images in function_call_output
                            if let Some((mime_type, data)) = has_image {
                                let image_url = format!("data:{};base64,{}", mime_type, data);
                                input.push(InputItem {
                                    id: None,
                                    item_type: "message".to_string(),
                                    role: Some("user".to_string()),
                                    content: Some(vec![InputContent::InputImage {
                                        image_url,
                                        detail: Some("auto".to_string()),
                                    }]),
                                    call_id: None,
                                    name: None,
                                    arguments: None,
                                    output: None,
                                    encrypted_content: None,
                                    summary: None,
                                });
                            }
                        }
                        ChatContentBlock::Image { .. } => {}
                    }
                }
            }
            ("user", MessageContent::Blocks(blocks)) => {
                // Collect all content for this user message
                let mut content_parts: Vec<InputContent> = Vec::new();

                for block in blocks {
                    match block {
                        ChatContentBlock::Text(text) => {
                            content_parts.push(InputContent::InputText { text: text.clone() });
                        }
                        ChatContentBlock::Image { mime_type, data } => {
                            let image_url = format!("data:{};base64,{}", mime_type, data);
                            content_parts.push(InputContent::InputImage {
                                image_url,
                                detail: Some("auto".to_string()),
                            });
                        }
                        ChatContentBlock::ToolResult(result) => {
                            // First, flush any pending content as a user message
                            if !content_parts.is_empty() {
                                input.push(InputItem {
                                    id: None,
                                    item_type: "message".to_string(),
                                    role: Some("user".to_string()),
                                    content: Some(content_parts),
                                    call_id: None,
                                    name: None,
                                    arguments: None,
                                    output: None,
                                    encrypted_content: None,
                                    summary: None,
                                });
                                content_parts = Vec::new();
                            }

                            let (output, has_image) =
                                extract_tool_result_with_image(&result.content);

                            let call_id = result
                                .tool_use_id
                                .split('|')
                                .next()
                                .unwrap_or("")
                                .to_string();

                            if call_id.is_empty() {
                                continue;
                            }

                            input.push(InputItem {
                                id: None,
                                item_type: "function_call_output".to_string(),
                                role: None,
                                content: None,
                                call_id: Some(call_id),
                                name: None,
                                arguments: None,
                                output: Some(output),
                                encrypted_content: None,
                                summary: None,
                            });

                            // If there's an image, add it as a separate user message
                            if let Some((mime_type, data)) = has_image {
                                let image_url = format!("data:{};base64,{}", mime_type, data);
                                input.push(InputItem {
                                    id: None,
                                    item_type: "message".to_string(),
                                    role: Some("user".to_string()),
                                    content: Some(vec![InputContent::InputImage {
                                        image_url,
                                        detail: Some("auto".to_string()),
                                    }]),
                                    call_id: None,
                                    name: None,
                                    arguments: None,
                                    output: None,
                                    encrypted_content: None,
                                    summary: None,
                                });
                            }
                        }
                        _ => {}
                    }
                }

                // Flush any remaining content
                if !content_parts.is_empty() {
                    input.push(InputItem {
                        id: None,
                        item_type: "message".to_string(),
                        role: Some("user".to_string()),
                        content: Some(content_parts),
                        call_id: None,
                        name: None,
                        arguments: None,
                        output: None,
                        encrypted_content: None,
                        summary: None,
                    });
                }
            }
            _ => {}
        }
    }

    Ok(input)
}

/// Extracts text and optional image from tool result content.
/// Returns (text_output, Option<(mime_type, base64_data)>)
fn extract_tool_result_with_image(
    content: &ToolResultContent,
) -> (String, Option<(String, String)>) {
    match content {
        ToolResultContent::Text(text) => (text.clone(), None),
        ToolResultContent::Blocks(blocks) => {
            let text = blocks
                .iter()
                .find_map(|block| match block {
                    crate::tools::ToolResultBlock::Text { text } => Some(text.clone()),
                    _ => None,
                })
                .unwrap_or_default();

            let image = blocks.iter().find_map(|block| match block {
                crate::tools::ToolResultBlock::Image { mime_type, data } => {
                    Some((mime_type.clone(), data.clone()))
                }
                _ => None,
            });

            (text, image)
        }
    }
}
