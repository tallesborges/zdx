use std::pin::Pin;

use anyhow::{Result, bail};
use futures_util::Stream;
use reqwest::header::{HeaderMap, HeaderValue};

use crate::providers::anthropic::{ProviderError, ProviderErrorKind, StreamEvent};
use crate::providers::openai_codex::auth::{OpenAICodexConfig, resolve_credentials};
use crate::providers::openai_codex::prompts::{get_codex_instructions, normalize_model};
use crate::providers::openai_codex::sse::CodexSseParser;
use crate::providers::openai_codex::types::{
    FunctionTool, InputContent, InputItem, ReasoningConfig, RequestBody,
};
use crate::tools::{ToolDefinition, ToolResultContent};

const DEFAULT_BASE_URL: &str = "https://chatgpt.com/backend-api";
const RESPONSES_PATH: &str = "/codex/responses";

const HEADER_BETA: &str = "OpenAI-Beta";
const HEADER_ACCOUNT_ID: &str = "chatgpt-account-id";
const HEADER_ORIGINATOR: &str = "originator";
const HEADER_SESSION_ID: &str = "session_id";
const HEADER_CONVERSATION_ID: &str = "conversation_id";

const BETA_VALUE: &str = "responses=experimental";
const ORIGINATOR_VALUE: &str = "codex_cli_rs";

pub struct OpenAICodexClient {
    config: OpenAICodexConfig,
    http: reqwest::Client,
}

impl OpenAICodexClient {
    pub fn new(config: OpenAICodexConfig) -> Self {
        Self {
            config,
            http: reqwest::Client::new(),
        }
    }

    pub async fn send_messages_stream(
        &self,
        messages: &[crate::providers::anthropic::ChatMessage],
        tools: &[ToolDefinition],
        system: Option<&str>,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<StreamEvent>> + Send>>> {
        let creds = resolve_credentials().await?;

        let input = build_input(messages, system)?;
        if input.is_empty() {
            bail!("No input messages provided for OpenAI Codex request");
        }

        let normalized_model = normalize_model(&self.config.model);
        let instructions = Some(get_codex_instructions(&normalized_model));
        let tool_defs = if tools.is_empty() {
            None
        } else {
            Some(tools.iter().map(FunctionTool::from).collect())
        };

        let request = RequestBody {
            model: normalized_model,
            stream: true,
            store: Some(false),
            instructions,
            reasoning: self
                .config
                .reasoning_effort
                .as_ref()
                .map(|effort| ReasoningConfig {
                    effort: effort.clone(),
                }),
            input,
            tools: tool_defs,
        };

        let url = format!("{}{}", DEFAULT_BASE_URL, RESPONSES_PATH);

        let headers = build_headers(&creds.account_id, &creds.access);

        let response = self
            .http
            .post(&url)
            .headers(headers)
            .json(&request)
            .send()
            .await
            .map_err(Self::classify_reqwest_error)?;

        let status = response.status();
        if !status.is_success() {
            let error_body = response.text().await.unwrap_or_default();
            return Err(ProviderError::http_status(status.as_u16(), &error_body).into());
        }

        let byte_stream = response.bytes_stream();
        let event_stream = CodexSseParser::new(byte_stream, self.config.model.clone());
        Ok(Box::pin(event_stream))
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
}

fn build_headers(account_id: &str, access_token: &str) -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert(
        "Authorization",
        HeaderValue::from_str(&format!("Bearer {}", access_token))
            .unwrap_or_else(|_| HeaderValue::from_static("")),
    );
    headers.insert(
        HEADER_ACCOUNT_ID,
        HeaderValue::from_str(account_id).unwrap_or_else(|_| HeaderValue::from_static("")),
    );
    headers.insert(HEADER_BETA, HeaderValue::from_static(BETA_VALUE));
    headers.insert(
        HEADER_ORIGINATOR,
        HeaderValue::from_static(ORIGINATOR_VALUE),
    );
    headers.insert("accept", HeaderValue::from_static("text/event-stream"));
    headers.insert("content-type", HeaderValue::from_static("application/json"));
    headers.remove(HEADER_SESSION_ID);
    headers.remove(HEADER_CONVERSATION_ID);
    headers
}

fn build_input(
    messages: &[crate::providers::anthropic::ChatMessage],
    system: Option<&str>,
) -> Result<Vec<InputItem>> {
    use crate::providers::anthropic::{ChatContentBlock, MessageContent};

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
                            });
                        }
                        ChatContentBlock::ToolUse {
                            id,
                            name,
                            input: tool_input,
                        } => {
                            let arguments = serde_json::to_string(tool_input)
                                .unwrap_or_else(|_| "{}".to_string());
                            let mut parts = id.split('|');
                            let call_id = parts.next().unwrap_or("").to_string();
                            let _tool_id = parts.next().unwrap_or("").to_string();
                            input.push(InputItem {
                                id: Some(_tool_id.clone()),
                                item_type: "function_call".to_string(),
                                role: None,
                                content: None,
                                call_id: Some(call_id),
                                name: Some(name.clone()),
                                arguments: Some(arguments),
                                output: None,
                            });
                        }
                        ChatContentBlock::Thinking { .. } => {
                            // Skip thinking blocks for OpenAI Codex.
                        }
                        ChatContentBlock::ToolResult(result) => {
                            let output = match &result.content {
                                ToolResultContent::Text(text) => text.clone(),
                                ToolResultContent::Blocks(blocks) => blocks
                                    .iter()
                                    .find_map(|block| match block {
                                        crate::tools::ToolResultBlock::Text { text } => {
                                            Some(text.clone())
                                        }
                                        _ => None,
                                    })
                                    .unwrap_or_default(),
                            };

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
                            });
                        }
                    }
                }
            }
            ("user", MessageContent::Blocks(blocks)) => {
                for block in blocks {
                    match block {
                        ChatContentBlock::Text(text) => {
                            input.push(InputItem {
                                id: None,
                                item_type: "message".to_string(),
                                role: Some("user".to_string()),
                                content: Some(vec![InputContent::InputText { text: text.clone() }]),
                                call_id: None,
                                name: None,
                                arguments: None,
                                output: None,
                            });
                        }
                        ChatContentBlock::ToolResult(result) => {
                            let output = match &result.content {
                                ToolResultContent::Text(text) => text.clone(),
                                ToolResultContent::Blocks(blocks) => blocks
                                    .iter()
                                    .find_map(|block| match block {
                                        crate::tools::ToolResultBlock::Text { text } => {
                                            Some(text.clone())
                                        }
                                        _ => None,
                                    })
                                    .unwrap_or_default(),
                            };

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
                            });
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }

    Ok(input)
}
