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
///
/// # Errors
/// Returns an error if the operation fails.
pub async fn send_responses_stream(
    http: &reqwest::Client,
    config: &ResponsesConfig,
    headers: HeaderMap,
    messages: &[ChatMessage],
    tools: &[ToolDefinition],
    system: Option<&str>,
) -> Result<ProviderStream> {
    let input = build_input(messages, system);
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
            .map_err(|e| classify_reqwest_error(&e))?
    } else {
        http.post(&url)
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
    let event_stream = ResponsesSseParser::new(byte_stream, config.model.clone());

    Ok(maybe_wrap_with_metrics(event_stream))
}

fn classify_reqwest_error(e: &reqwest::Error) -> ProviderError {
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

fn build_input(messages: &[ChatMessage], system: Option<&str>) -> Vec<InputItem> {
    let mut input = Vec::new();
    if let Some(prompt) = system {
        input.push(message_item("developer", vec![InputContent::InputText {
            text: prompt.to_string(),
        }]));
    }

    for msg in messages {
        append_input_for_message(msg, &mut input);
    }

    input
}

fn append_input_for_message(msg: &ChatMessage, input: &mut Vec<InputItem>) {
    use crate::providers::MessageContent;

    match (&msg.role[..], &msg.content) {
        ("user", MessageContent::Text(text)) => {
            input.push(message_item(
                "user",
                vec![InputContent::InputText { text: text.clone() }],
            ));
        }
        ("assistant", MessageContent::Text(text)) => {
            input.push(message_item(
                "assistant",
                vec![InputContent::OutputText { text: text.clone() }],
            ));
        }
        ("assistant", MessageContent::Blocks(blocks)) => {
            append_assistant_blocks(blocks, input);
        }
        ("user", MessageContent::Blocks(blocks)) => append_user_blocks(blocks, input),
        _ => {}
    }
}

fn append_assistant_blocks(blocks: &[ChatContentBlock], input: &mut Vec<InputItem>) {
    for block in blocks {
        match block {
            ChatContentBlock::Text(text) => {
                input.push(message_item(
                    "assistant",
                    vec![InputContent::OutputText { text: text.clone() }],
                ));
            }
            ChatContentBlock::Reasoning(ReasoningBlock { text, replay }) => {
                if let Some(item) = reasoning_replay_item(text.as_deref(), replay.as_ref()) {
                    input.push(item);
                }
            }
            ChatContentBlock::ToolUse {
                id,
                name,
                input: arguments,
            } => input.push(function_call_item(id, name, arguments)),
            ChatContentBlock::ToolResult(result) => append_tool_result(result, input),
            ChatContentBlock::Image { .. } => {}
        }
    }
}

fn append_user_blocks(blocks: &[ChatContentBlock], input: &mut Vec<InputItem>) {
    let mut content_parts = Vec::new();
    for block in blocks {
        match block {
            ChatContentBlock::Text(text) => {
                content_parts.push(InputContent::InputText { text: text.clone() });
            }
            ChatContentBlock::Image { mime_type, data } => {
                content_parts.push(input_image_content(mime_type, data));
            }
            ChatContentBlock::ToolResult(result) => {
                flush_user_content_parts(input, &mut content_parts);
                append_tool_result(result, input);
            }
            _ => {}
        }
    }
    flush_user_content_parts(input, &mut content_parts);
}

fn flush_user_content_parts(input: &mut Vec<InputItem>, content_parts: &mut Vec<InputContent>) {
    if content_parts.is_empty() {
        return;
    }
    input.push(message_item("user", std::mem::take(content_parts)));
}

fn append_tool_result(result: &crate::tools::ToolResult, input: &mut Vec<InputItem>) {
    let (output, has_image) = extract_tool_result_with_image(&result.content);
    let call_id = result
        .tool_use_id
        .split('|')
        .next()
        .unwrap_or("")
        .to_string();
    if call_id.is_empty() {
        return;
    }

    input.push(function_call_output_item(call_id, output));
    if let Some((mime_type, data)) = has_image {
        input.push(message_item("user", vec![input_image_content(&mime_type, &data)]));
    }
}

fn reasoning_replay_item(text: Option<&str>, replay: Option<&ReplayToken>) -> Option<InputItem> {
    let ReplayToken::OpenAI {
        id,
        encrypted_content,
    } = replay?
    else {
        return None;
    };

    let summary_text = text
        .filter(|value| !value.trim().is_empty())
        .map_or_else(|| "(reasoning)".to_string(), str::to_owned);
    Some(InputItem {
        id: Some(id.clone()),
        item_type: "reasoning".to_string(),
        role: None,
        content: None,
        call_id: None,
        name: None,
        arguments: None,
        output: None,
        encrypted_content: Some(encrypted_content.clone()),
        summary: Some(vec![SummaryItem {
            item_type: "summary_text",
            text: summary_text,
        }]),
    })
}

fn function_call_item(id: &str, name: &str, arguments: &serde_json::Value) -> InputItem {
    let arguments = serde_json::to_string(arguments).unwrap_or_else(|_| "{}".to_string());
    let mut parts = id.split('|');
    let call_id = parts.next().and_then(non_empty_owned);
    let tool_id = parts.next().and_then(non_empty_owned);

    InputItem {
        id: tool_id,
        item_type: "function_call".to_string(),
        role: None,
        content: None,
        call_id,
        name: Some(name.to_string()),
        arguments: Some(arguments),
        output: None,
        encrypted_content: None,
        summary: None,
    }
}

fn function_call_output_item(call_id: String, output: String) -> InputItem {
    InputItem {
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
    }
}

fn message_item(role: &str, content: Vec<InputContent>) -> InputItem {
    InputItem {
        id: None,
        item_type: "message".to_string(),
        role: Some(role.to_string()),
        content: Some(content),
        call_id: None,
        name: None,
        arguments: None,
        output: None,
        encrypted_content: None,
        summary: None,
    }
}

fn input_image_content(mime_type: &str, data: &str) -> InputContent {
    InputContent::InputImage {
        image_url: format!("data:{mime_type};base64,{data}"),
        detail: Some("auto".to_string()),
    }
}

fn non_empty_owned(value: &str) -> Option<String> {
    (!value.is_empty()).then_some(value.to_string())
}

/// Extracts text and optional image from tool result content.
/// Returns (`text_output`, Option<(`mime_type`, `base64_data`)>)
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
                    crate::tools::ToolResultBlock::Image { .. } => None,
                })
                .unwrap_or_default();

            let image = blocks.iter().find_map(|block| match block {
                crate::tools::ToolResultBlock::Image { mime_type, data } => {
                    Some((mime_type.clone(), data.clone()))
                }
                crate::tools::ToolResultBlock::Text { .. } => None,
            });

            (text, image)
        }
    }
}
