//! `/commands` picker: project/context command palette for the bot.
//!
//! Mirrors the TUI's custom-command list for the chat's bound project: the
//! Markdown commands discovered via `zdx_engine::custom_commands` (bundled +
//! `$ZDX_HOME/commands` + project `.zdx/commands`). Custom commands are
//! picker-only on the bot — they are not typed commands and are not
//! registered in the native `/` menu (built-ins like `/handoff` and `/tldr`
//! live there instead). Tapping a command dispatches its prompt content as a
//! normal agent turn in the current topic.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use anyhow::Result;
use serde_json::json;
use zdx_engine::core::thread_persistence;
use zdx_engine::custom_commands::{CustomCommandSource, load_custom_commands};

use crate::bot::context::BotContext;
use crate::bot::queue::{ChatQueueMap, dispatch_message};
use crate::commands::{BotCommand, native_command_names, parse_command};
use crate::handlers::message::escape_html;
use crate::telegram::{CallbackQuery, InlineKeyboardButton, InlineKeyboardMarkup, TelegramClient};
use crate::types::IncomingMessage;

/// One tappable custom command in a `/commands` picker message.
pub(crate) struct PickerEntry {
    name: String,
    content: String,
}

/// Picker entries awaiting a tap, keyed by (`chat_id`, `message_id`) of the
/// picker message. One-shot: consumed on first tap or dismiss.
pub(crate) type CommandPickerMap = Arc<Mutex<HashMap<(i64, i64), Vec<PickerEntry>>>>;

pub(crate) fn new_command_picker_map() -> CommandPickerMap {
    Arc::new(Mutex::new(HashMap::new()))
}

/// Handles `/commands`: posts the project-command picker for the current chat
/// context. Returns `true` when the message was the `/commands` command.
pub(crate) async fn handle_commands_command(
    context: &BotContext,
    incoming: &IncomingMessage,
    reply_to_message_id: Option<i64>,
    topic_id: Option<i64>,
    thread_id: &str,
) -> Result<bool> {
    if !incoming.images.is_empty() || !incoming.audios.is_empty() {
        return Ok(false);
    }
    if !incoming
        .text
        .as_deref()
        .and_then(parse_command)
        .is_some_and(|cmd| matches!(cmd, BotCommand::Commands))
    {
        return Ok(false);
    }

    let root = command_root(context, incoming.chat_id, thread_id)?;
    let builtin_names = native_command_names();
    let loaded = load_custom_commands(&root, &builtin_names);
    for warning in &loaded.warnings {
        tracing::warn!(path = %warning.path.display(), message = %warning.message, "Custom command warning");
    }

    let mut sorted = loaded.commands;
    sorted.sort_by_key(|command| match command.source {
        CustomCommandSource::Project => 0,
        CustomCommandSource::User => 1,
        CustomCommandSource::BuiltIn => 2,
    });
    let body = picker_body(&sorted);
    let entries: Vec<PickerEntry> = sorted
        .into_iter()
        .map(|command| PickerEntry {
            name: command.name,
            content: command.content,
        })
        .collect();

    let keyboard = picker_keyboard(&entries);
    let picker = context
        .client()
        .send_message_with_markup(
            incoming.chat_id,
            &body,
            reply_to_message_id,
            topic_id,
            &keyboard,
        )
        .await?;

    let mut map = context
        .command_picker_map()
        .lock()
        .expect("command picker lock poisoned");
    map.insert((incoming.chat_id, picker.id), entries);
    Ok(true)
}

/// Handles `cmd:{idx}` / `cmd:x` callbacks from a picker message.
pub(crate) async fn handle_callback(
    context: &Arc<BotContext>,
    queues: &ChatQueueMap,
    client: &TelegramClient,
    callback: &CallbackQuery,
    data: &str,
) {
    let Some(message) = callback.message.as_ref() else {
        let _ = client
            .answer_callback_query(&callback.id, Some("No message context"))
            .await;
        return;
    };
    let chat_id = message.chat.id;

    if data == "x" {
        {
            let mut map = context
                .command_picker_map()
                .lock()
                .expect("command picker lock poisoned");
            map.remove(&(chat_id, message.id));
        }
        let _ = client.delete_message(chat_id, message.id).await;
        let _ = client.answer_callback_query(&callback.id, None).await;
        return;
    }

    let entry = {
        let mut map = context
            .command_picker_map()
            .lock()
            .expect("command picker lock poisoned");
        data.parse::<usize>().ok().and_then(|idx| {
            map.remove(&(chat_id, message.id))
                .and_then(|mut entries| (idx < entries.len()).then(|| entries.swap_remove(idx)))
        })
    };
    let Some(entry) = entry else {
        let _ = client
            .answer_callback_query(&callback.id, Some("This picker is no longer active"))
            .await;
        return;
    };

    let _ = client
        .edit_message_text(
            chat_id,
            message.id,
            &format!("▶️ /{}", escape_html(&entry.name)),
            None,
        )
        .await;
    let _ = client.answer_callback_query(&callback.id, None).await;

    dispatch_synthetic_text(queues, context, callback, message, &entry.content).await;
}

/// Dispatches `text` as the user's next message in the picker's topic (same
/// synthetic-message mechanism as follow-up buttons).
async fn dispatch_synthetic_text(
    queues: &ChatQueueMap,
    context: &Arc<BotContext>,
    callback: &CallbackQuery,
    message: &crate::telegram::Message,
    text: &str,
) {
    let chat_kind = if message.chat.is_private() {
        "private"
    } else {
        "supergroup"
    };
    let synthetic: Result<crate::telegram::Message, _> = serde_json::from_value(json!({
        "message_id": message.id,
        "chat": {
            "id": message.chat.id,
            "type": chat_kind,
            "is_forum": message.chat.is_forum_enabled(),
        },
        "from": { "id": callback.from.id, "is_bot": false },
        "text": text,
        "message_thread_id": message.effective_thread_id(),
    }));
    match synthetic {
        Ok(synthetic) => dispatch_message(queues, context, synthetic).await,
        Err(err) => {
            tracing::error!(chat_id = message.chat.id, %err, "Failed to synthesize picker command message");
        }
    }
}

/// Resolves the command-discovery root: the thread's root override (worktree)
/// or the chat's profile root.
pub(crate) fn command_root(context: &BotContext, chat_id: i64, thread_id: &str) -> Result<PathBuf> {
    let resolved = context.root_for_chat(chat_id);
    Ok(thread_persistence::read_thread_root_path(thread_id)?.map_or(resolved.root, PathBuf::from))
}

fn picker_body(commands: &[zdx_engine::custom_commands::CustomCommand]) -> String {
    let mut lines = vec!["🧰 <b>Project commands</b>".to_string()];
    for command in commands {
        let description = command
            .description
            .as_deref()
            .map(|d| format!(" — {}", escape_html(d)))
            .unwrap_or_default();
        lines.push(format!(
            "• /{}{} <i>({})</i>",
            escape_html(&command.name),
            description,
            command.source.as_str()
        ));
    }
    truncate_chars(&lines.join("\n"), 3500)
}

fn picker_keyboard(entries: &[PickerEntry]) -> InlineKeyboardMarkup {
    let mut rows: Vec<Vec<InlineKeyboardButton>> = entries
        .chunks(2)
        .enumerate()
        .map(|(row_idx, chunk)| {
            chunk
                .iter()
                .enumerate()
                .map(|(col_idx, entry)| InlineKeyboardButton {
                    text: format!("/{}", entry.name),
                    callback_data: Some(format!("cmd:{}", row_idx * 2 + col_idx)),
                    url: None,
                })
                .collect()
        })
        .collect();
    rows.push(vec![InlineKeyboardButton {
        text: "✕ Dismiss".to_string(),
        callback_data: Some("cmd:x".to_string()),
        url: None,
    }]);
    InlineKeyboardMarkup {
        inline_keyboard: rows,
    }
}

fn truncate_chars(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let truncated: String = text.chars().take(max_chars.saturating_sub(1)).collect();
    format!("{truncated}…")
}

#[cfg(test)]
mod tests {
    use zdx_engine::custom_commands::{CustomCommand, CustomCommandSource};

    use super::{PickerEntry, picker_body, picker_keyboard, truncate_chars};

    fn custom(name: &str, source: CustomCommandSource) -> CustomCommand {
        CustomCommand {
            name: name.to_string(),
            description: Some(format!("{name} description")),
            source,
            path: std::path::PathBuf::from(format!("/tmp/{name}.md")),
            content: format!("{name} prompt"),
            is_executable: false,
        }
    }

    fn entry(name: &str) -> PickerEntry {
        PickerEntry {
            name: name.to_string(),
            content: format!("{name} prompt"),
        }
    }

    #[test]
    fn picker_body_lists_custom_commands_with_source_and_no_builtins() {
        let body = picker_body(&[
            custom("plan", CustomCommandSource::BuiltIn),
            custom("deploy", CustomCommandSource::Project),
        ]);
        assert!(body.contains("Project commands"));
        assert!(body.contains("/plan"));
        assert!(body.contains("plan description"));
        assert!(body.contains("(builtin)"));
        assert!(body.contains("/deploy"));
        assert!(body.contains("(project)"));
        assert!(!body.contains("Handoff"));
        assert!(!body.contains("TLDR"));
    }

    #[test]
    fn picker_keyboard_indexes_match_entry_order_and_has_dismiss() {
        let entries = vec![entry("plan"), entry("investigate"), entry("deploy")];
        let keyboard = picker_keyboard(&entries);
        let buttons: Vec<_> = keyboard.inline_keyboard.iter().flatten().collect();
        assert_eq!(buttons.len(), 4);
        assert_eq!(buttons[0].callback_data.as_deref(), Some("cmd:0"));
        assert_eq!(buttons[0].text, "/plan");
        assert_eq!(buttons[1].callback_data.as_deref(), Some("cmd:1"));
        assert_eq!(buttons[2].callback_data.as_deref(), Some("cmd:2"));
        assert_eq!(buttons[2].text, "/deploy");
        assert_eq!(buttons[3].callback_data.as_deref(), Some("cmd:x"));
    }

    #[test]
    fn truncates_long_picker_bodies() {
        let long = "y".repeat(10_000);
        assert!(truncate_chars(&long, 3500).chars().count() == 3500);
    }
}
