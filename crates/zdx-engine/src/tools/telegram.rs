//! Telegram tool.
//!
//! Posts into a Telegram chat or forum topic other than the current
//! conversation: create a topic, send a message, send a document.
//!
//! Wraps `crate::telegram`, the same shared core behind the `zdx telegram`
//! CLI subcommands, so agents without a shell can reach the same capability.

use serde::Deserialize;
use serde_json::{Value, json};

use super::{ToolContext, ToolDefinition, resolve_existing_path};
use crate::core::events::ToolOutput;
use crate::telegram::{
    MAX_CAPTION_CHARS, MAX_MESSAGE_CHARS, ParseMode, TelegramOutbound, resolve_bot_token,
};

/// Returns the tool definition for the telegram tool.
pub fn definition() -> ToolDefinition {
    ToolDefinition {
        name: "Telegram".to_string(),
        description: format!(
            "Post into a Telegram chat or forum topic other than the current conversation: send a message, send a file, or create a forum topic. Use when the user asks to send something to another topic, group, or chat (\"manda isso pro tópico X\", \"post this summary in group Z\", \"create a new topic with the trip info\"). Do NOT use it to reply in the current conversation — that is just your normal answer.\n\nResolving `chat_id`: never guess it. Prefer a named profile in the Telegram Workspaces block or `$ZDX_HOME/config.toml` `[telegram.profiles.<name>]`; otherwise read it off an attachment path (`$ZDX_HOME/telegram/<chat_id>/...`); otherwise ask the user. Supergroup ids are negative (`-100...`) — keep the minus sign. `message_thread_id` is the forum topic id; omit it to post in the group's General topic.\n\nFormatting: `parse_mode` defaults to `html`, and the text is sent raw — the Markdown conversion used on the normal reply path does not run here, and one unsupported tag rejects the whole message. Supported tags: b, strong, i, em, u, s, code, pre, a href, blockquote, tg-spoiler. Not supported: headings, ul/ol/li, tables, br, div, span. Use plain newlines and bullet characters, and escape literal &, <, and >. Use `parse_mode: \"plain\"` when the text is not marked up. If the content is a table, a diagram, or longer than a couple of screens, write a file and use `send_document` instead of fighting the markup.\n\nLimits: message body {MAX_MESSAGE_CHARS} characters, caption {MAX_CAPTION_CHARS}. Split long content by section into several calls rather than truncating it.\n\nWhat this actually creates: `create_topic` plus `send_message` produces a topic containing text, not a resumable agent conversation. The posted content is not part of any thread history, and a user replying there starts a fresh thread with no context. Say so plainly instead of implying continuity.\n\nPosting is externally visible and cannot be cleanly unsent. Send without confirming only when the user named the destination in the current turn; otherwise confirm the target chat and topic first, especially for chats with other people in them."
        ),
        input_schema: json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "description": "Which operation to perform.",
                    "enum": ["send_message", "send_document", "create_topic"]
                },
                "chat_id": {
                    "type": "integer",
                    "description": "Target Telegram chat id. Supergroups are negative (e.g. -1003889440104)."
                },
                "message_thread_id": {
                    "type": "integer",
                    "description": "Forum topic id to post into. Omit to post in the group's General topic. Ignored by `create_topic`."
                },
                "text": {
                    "type": "string",
                    "description": "Message body. Required for `send_message`."
                },
                "parse_mode": {
                    "type": "string",
                    "description": "Formatting mode for `send_message`. Defaults to `html`.",
                    "enum": ["html", "markdown", "markdown-v2", "plain"]
                },
                "path": {
                    "type": "string",
                    "description": "Path to the file to send. Required for `send_document`. Relative paths resolve from the current working directory; $VAR and ~ are expanded. Generate files into $ZDX_ARTIFACT_DIR first."
                },
                "caption": {
                    "type": "string",
                    "description": "Optional caption for `send_document`. Keep detail in a preceding message instead."
                },
                "name": {
                    "type": "string",
                    "description": "Topic name. Required for `create_topic`. Returns the new topic id."
                },
                "bot_token": {
                    "type": "string",
                    "description": "Optional bot token override. Defaults to the configured bot. Use it only when the target chat belongs to a different bot than the configured one."
                }
            },
            "required": ["action", "chat_id"],
            "additionalProperties": false
        }),
    }
}

#[derive(Debug, Deserialize)]
struct TelegramInput {
    action: String,
    // Models routinely send chat ids as strings; accept both rather than
    // failing the call on a JSON type detail.
    #[serde(deserialize_with = "zdx_tools::i64_or_string::deserialize")]
    chat_id: i64,
    #[serde(
        default,
        deserialize_with = "zdx_tools::i64_or_string::deserialize_optional"
    )]
    message_thread_id: Option<i64>,
    text: Option<String>,
    parse_mode: Option<String>,
    path: Option<String>,
    caption: Option<String>,
    name: Option<String>,
    bot_token: Option<String>,
}

fn missing(field: &str, action: &str) -> ToolOutput {
    ToolOutput::failure(
        "invalid_input",
        format!("`{field}` is required for action `{action}`"),
        None,
    )
}

fn resolve_parse_mode(mode: Option<&str>) -> Result<ParseMode, ToolOutput> {
    let mode = mode.map(str::trim).filter(|mode| !mode.is_empty());
    ParseMode::parse(mode.unwrap_or("html")).map_err(|err| {
        ToolOutput::failure(
            "invalid_input",
            "Invalid parse_mode",
            Some(format!("{err:#}")),
        )
    })
}

/// Executes the telegram tool and returns a structured envelope.
pub async fn execute(input: &Value, ctx: &ToolContext) -> ToolOutput {
    let input: TelegramInput = match serde_json::from_value(input.clone()) {
        Ok(value) => value,
        Err(err) => {
            return ToolOutput::failure(
                "invalid_input",
                "Invalid input for telegram tool",
                Some(format!("Parse error: {err}")),
            );
        }
    };

    let config = ctx.config.clone().unwrap_or_default();
    let token = match resolve_bot_token(&config, input.bot_token.as_deref()) {
        Ok(token) => token,
        Err(err) => {
            return ToolOutput::failure(
                "invalid_config",
                "No Telegram bot token",
                Some(format!("{err:#}")),
            );
        }
    };
    let client = TelegramOutbound::new(token);
    let action = input.action.trim();

    match action {
        "send_message" => {
            let Some(text) = input.text.as_deref() else {
                return missing("text", action);
            };
            let parse_mode = match resolve_parse_mode(input.parse_mode.as_deref()) {
                Ok(mode) => mode,
                Err(output) => return output,
            };

            match client
                .send_message(input.chat_id, text, input.message_thread_id, parse_mode)
                .await
            {
                Ok(()) => ToolOutput::success(json!({
                    "sent": true,
                    "chat_id": input.chat_id,
                    "message_thread_id": input.message_thread_id,
                })),
                Err(err) => failure(&err),
            }
        }
        "send_document" => {
            let Some(path) = input.path.as_deref() else {
                return missing("path", action);
            };
            let resolved = match resolve_existing_path(path, &ctx.root) {
                Ok(resolved) => resolved,
                Err(output) => return output,
            };

            match client
                .send_document(
                    input.chat_id,
                    &resolved.resolved_path,
                    input
                        .caption
                        .as_deref()
                        .filter(|caption| !caption.trim().is_empty()),
                    input.message_thread_id,
                )
                .await
            {
                Ok(()) => ToolOutput::success(json!({
                    "sent": true,
                    "chat_id": input.chat_id,
                    "message_thread_id": input.message_thread_id,
                    "file": resolved.resolved_path.display().to_string(),
                })),
                Err(err) => failure(&err),
            }
        }
        "create_topic" => {
            let Some(name) = input.name.as_deref() else {
                return missing("name", action);
            };

            match client.create_forum_topic(input.chat_id, name).await {
                Ok(message_thread_id) => ToolOutput::success(json!({
                    "created": true,
                    "chat_id": input.chat_id,
                    "message_thread_id": message_thread_id,
                })),
                Err(err) => failure(&err),
            }
        }
        other => ToolOutput::failure(
            "invalid_input",
            format!("Unknown action `{other}`"),
            Some("Expected send_message, send_document, or create_topic".to_string()),
        ),
    }
}

fn failure(err: &anyhow::Error) -> ToolOutput {
    ToolOutput::failure(
        "execution_failed",
        "Telegram request failed",
        Some(format!("{err:#}")),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> ToolContext {
        ToolContext::new(std::env::temp_dir(), None)
    }

    #[test]
    fn definition_exposes_three_actions() {
        let definition = definition();
        assert_eq!(definition.name, "Telegram");

        let actions = definition.input_schema["properties"]["action"]["enum"]
            .as_array()
            .expect("action enum");
        assert_eq!(
            actions,
            &vec![
                json!("send_message"),
                json!("send_document"),
                json!("create_topic")
            ]
        );

        let required = definition.input_schema["required"]
            .as_array()
            .expect("required");
        assert!(required.contains(&json!("action")));
        assert!(required.contains(&json!("chat_id")));
    }

    #[tokio::test]
    async fn rejects_unknown_actions() {
        let output = execute(&json!({ "action": "delete_chat", "chat_id": -100 }), &ctx()).await;
        assert!(!output.is_ok());
    }

    #[tokio::test]
    async fn requires_action_specific_fields() {
        // Missing fields must fail before any network call.
        for input in [
            json!({ "action": "send_message", "chat_id": -100 }),
            json!({ "action": "send_document", "chat_id": -100 }),
            json!({ "action": "create_topic", "chat_id": -100 }),
        ] {
            let output = execute(&input, &ctx()).await;
            assert!(!output.is_ok(), "expected failure for {input}");
        }
    }

    #[tokio::test]
    async fn blank_optional_parse_mode_and_token_use_defaults() {
        for mode in [None, Some(""), Some(" \t\n")] {
            assert_eq!(resolve_parse_mode(mode).unwrap(), ParseMode::Html);
        }
        let mut config = crate::config::Config::default();
        config.telegram.bot_token = Some("test-token".to_string());
        let ctx = ctx().with_config(&config);
        let omitted = execute(
            &json!({ "action": "send_message", "chat_id": -100, "text": "" }),
            &ctx,
        )
        .await;
        assert!(matches!(
            &omitted,
            ToolOutput::Failure { error, .. }
                if error.details.as_deref() == Some("text must not be empty")
        ));

        for blank in ["", " \t\n"] {
            let output = execute(
                &json!({
                    "action": "send_message", "chat_id": -100, "text": "",
                    "parse_mode": blank, "bot_token": blank
                }),
                &ctx,
            )
            .await;
            assert_eq!(output, omitted);
        }

        let invalid = execute(
            &json!({ "action": "send_message", "chat_id": -100, "text": "", "parse_mode": "rst" }),
            &ctx,
        )
        .await;
        assert!(matches!(
            invalid,
            ToolOutput::Failure { error, .. } if error.message == "Invalid parse_mode"
        ));
    }

    #[tokio::test]
    async fn rejects_invalid_parse_mode() {
        let output = execute(
            &json!({
                "action": "send_message",
                "chat_id": -100,
                "text": "hi",
                "parse_mode": "rst"
            }),
            &ctx(),
        )
        .await;
        assert!(!output.is_ok());
    }

    #[test]
    fn accepts_ids_as_strings_or_integers() {
        // Models routinely send numeric ids as JSON strings; both must parse.
        let from_strings: TelegramInput = serde_json::from_value(json!({
            "action": "send_message",
            "chat_id": "-1001234567890",
            "message_thread_id": "18923",
            "text": "hi"
        }))
        .expect("string ids parse");
        assert_eq!(from_strings.chat_id, -1_001_234_567_890);
        assert_eq!(from_strings.message_thread_id, Some(18923));

        let from_ints: TelegramInput = serde_json::from_value(json!({
            "action": "send_message",
            "chat_id": -1_001_234_567_890_i64,
            "message_thread_id": 18923,
            "text": "hi"
        }))
        .expect("integer ids parse");
        assert_eq!(from_ints.chat_id, -1_001_234_567_890);
        assert_eq!(from_ints.message_thread_id, Some(18923));

        let omitted: TelegramInput = serde_json::from_value(json!({
            "action": "create_topic",
            "chat_id": -100,
            "name": "x"
        }))
        .expect("omitted thread id parses");
        assert_eq!(omitted.message_thread_id, None);
    }
}
