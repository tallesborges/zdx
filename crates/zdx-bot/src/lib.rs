use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use zdx_core::config::Config;
use zdx_core::core::agent::ToolConfig;

use crate::bot::{
    BotContext, BotContextDeps, CancelKey, QueueCancelKey, dispatch_message, new_cancel_map,
    new_chat_queues, new_queue_cancel_map,
};
use crate::telegram::{CallbackQuery, TelegramClient, TelegramSettings};

mod agent;
mod bot;
mod commands;
mod handlers;
mod ingest;
pub mod telegram;
mod topic_title;
mod transcribe;
mod types;

const TELEGRAM_SURFACE_RULES: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/prompts/telegram_surface_rules.md"
));

/// Exit code used to signal the wrapper script to rebuild.
pub const EXIT_REBUILD: i32 = 42;

///
/// # Errors
/// Returns an error if the operation fails.
pub async fn run() -> Result<()> {
    let root = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    run_with_root(root).await
}

///
/// # Errors
/// Returns an error if the operation fails.
pub async fn run_with_root(root: PathBuf) -> Result<()> {
    let mut config = Config::load().context("load zdx config")?;
    // Apply telegram-specific model + thinking_level
    config.model.clone_from(&config.telegram.model);
    config.thinking_level = config.telegram.thinking_level;
    let settings = TelegramSettings::from_config(&config)?;
    let _pid_guard = zdx_core::pidfile::write("bot").context("write bot PID file")?;
    let config_path = zdx_core::config::paths::config_path();
    if config_path.exists() {
        tracing::info!(path = %config_path.display(), "Config file");
    }
    tracing::info!(
        model = %config.model,
        thinking = %config.thinking_level.display_name(),
        users = ?config.telegram.allowlist_user_ids,
        chats = ?config.telegram.allowlist_chat_ids,
        "Bot config",
    );
    run_bot(config, settings, root).await
}

async fn run_bot(config: Config, settings: TelegramSettings, root: PathBuf) -> Result<()> {
    let client = TelegramClient::new(settings.bot_token);
    let command_specs = crate::commands::telegram_command_specs();
    match client.set_my_commands(&command_specs).await {
        Ok(()) => tracing::info!(count = command_specs.len(), "Telegram command menu updated"),
        Err(err) => tracing::error!(%err, "Failed to update Telegram command menu"),
    }
    let tool_config = ToolConfig::default();

    let cancel_map = new_cancel_map();
    let queue_cancel_map = new_queue_cancel_map();
    let allowlist_user_len = settings.allowlist_user_ids.len();
    let allowlist_chat_len = settings.allowlist_chat_ids.len();
    let trimmed_surface_rules = TELEGRAM_SURFACE_RULES.trim();
    let bot_surface_rules =
        (!trimmed_surface_rules.is_empty()).then(|| trimmed_surface_rules.to_string());
    let context = Arc::new(BotContext::new(
        client.clone(),
        config,
        BotContextDeps {
            allowlist_user_ids: settings.allowlist_user_ids,
            allowlist_chat_ids: settings.allowlist_chat_ids,
            root,
            bot_surface_rules,
            tool_config,
            cancel_map,
            queue_cancel_map,
        },
    ));
    let chat_queues = new_chat_queues();

    let mut offset: Option<i64> = None;
    let poll_timeout = Duration::from_secs(30);
    let shutdown = tokio::signal::ctrl_c();
    tokio::pin!(shutdown);

    tracing::info!(
        allowlist_users = allowlist_user_len,
        allowlist_chats = allowlist_chat_len,
        "zdx-bot started, polling for updates"
    );

    loop {
        let current_offset = offset;
        tokio::select! {
            _ = &mut shutdown => {
                tracing::info!("Shutting down Telegram bot");
                break;
            }
            () = context.rebuild_notified() => {
                tracing::info!("Rebuild requested via /rebuild command");
                std::process::exit(EXIT_REBUILD);
            }
            updates = client.get_updates(current_offset, poll_timeout) => {
                let updates = match updates {
                    Ok(updates) => updates,
                    Err(err) => {
                        tracing::error!(%err, "Telegram polling error");
                        tokio::time::sleep(Duration::from_secs(1)).await;
                        continue;
                    }
                };

                if !updates.is_empty() {
                    tracing::debug!(count = updates.len(), "Received updates");
                }
                for update in updates {
                    offset = Some(update.id + 1);
                    if let Some(message) = update.message {
                        dispatch_message(&chat_queues, &context, message).await;
                    }
                    if let Some(callback) = update.callback_query {
                        handle_callback_query(&context, &client, callback).await;
                    }
                }
            }
        }
    }

    Ok(())
}

/// Handle a callback query from an inline keyboard button.
/// Supports:
/// - `cancel:{chat_id}:{user_message_id}` — cancel an active agent turn
/// - `cancel_q:{chat_id}:{message_id}` — cancel a queued (not-yet-processing) item
async fn handle_callback_query(
    context: &BotContext,
    client: &TelegramClient,
    callback: CallbackQuery,
) {
    // Enforce allowlist: only authorized users can trigger cancel actions
    if !context.allowlist_user_ids().contains(&callback.from.id) {
        tracing::warn!(
            user_id = callback.from.id,
            "Denied callback from non-allowlisted user"
        );
        let _ = client
            .answer_callback_query(&callback.id, Some("Access denied"))
            .await;
        return;
    }

    let data = callback.data.as_deref().unwrap_or("");

    if let Some(key) = parse_cancel_callback(data) {
        // Cancel an active agent turn
        let token = {
            let map = context.cancel_map().lock().await;
            map.get(&key).cloned()
        };

        if let Some(token) = token {
            token.cancel();
            if let Err(err) = client
                .answer_callback_query(&callback.id, Some("Cancelling..."))
                .await
            {
                tracing::warn!(%err, "Failed to answer cancel callback");
            }
            tracing::info!(?key, "Cancelled agent turn");
        } else if let Err(err) = client
            .answer_callback_query(&callback.id, Some("Nothing to cancel"))
            .await
        {
            tracing::warn!(%err, "Failed to answer callback");
        }
    } else if let Some(key) = parse_queue_cancel_callback(data) {
        // Cancel a queued (not-yet-processing) item
        let token = {
            let map = context.queue_cancel_map().lock().await;
            map.get(&key).cloned()
        };

        if let Some(token) = token {
            token.cancel();
            if let Err(err) = client
                .answer_callback_query(&callback.id, Some("Removed from queue"))
                .await
            {
                tracing::warn!(%err, "Failed to answer queue cancel callback");
            }
            tracing::info!(?key, "Cancelled queued item");
        } else {
            // Token gone — item may have already started processing
            if let Err(err) = client
                .answer_callback_query(&callback.id, Some("Already processing"))
                .await
            {
                tracing::warn!(%err, "Failed to answer callback");
            }
        }
    } else if data.starts_with("model_provider:")
        || data.starts_with("model_set:")
        || data.starts_with("model_back:")
        || data.starts_with("model_cancel:")
    {
        handle_model_callback(context, client, &callback, data).await;
    } else if data.starts_with("thinking_set:")
        || data.starts_with("thinking_reset:")
        || data.starts_with("thinking_cancel:")
    {
        handle_thinking_callback(context, client, &callback, data).await;
    } else {
        if let Err(err) = client.answer_callback_query(&callback.id, None).await {
            tracing::warn!(%err, "Failed to answer unknown callback");
        }
        tracing::warn!(user_id = callback.from.id, ?data, "Unknown callback");
    }
}

/// Parse `cancel:{chat_id}:{user_message_id}` callback data into a `CancelKey`.
fn parse_cancel_callback(data: &str) -> Option<CancelKey> {
    let rest = data.strip_prefix("cancel:")?;
    // Guard against matching cancel_q: prefix
    if rest.starts_with('q') {
        return None;
    }
    let (chat_str, msg_str) = rest.split_once(':')?;
    let chat_id: i64 = chat_str.parse().ok()?;
    let user_message_id: i64 = msg_str.parse().ok()?;
    Some((chat_id, user_message_id))
}

/// Parse `cancel_q:{chat_id}:{message_id}` callback data into a `QueueCancelKey`.
fn parse_queue_cancel_callback(data: &str) -> Option<QueueCancelKey> {
    let rest = data.strip_prefix("cancel_q:")?;
    let (chat_str, msg_str) = rest.split_once(':')?;
    let chat_id: i64 = chat_str.parse().ok()?;
    let message_id: i64 = msg_str.parse().ok()?;
    Some((chat_id, message_id))
}

fn telegram_thread_id(chat_id: i64, thread_id: Option<i64>) -> String {
    match thread_id {
        Some(thread_id) => format!("telegram-{chat_id}-topic-{thread_id}"),
        None => format!("telegram-{chat_id}"),
    }
}

fn current_topic_model(context: &BotContext, chat_id: i64, thread_id: Option<i64>) -> String {
    let config = context.config();
    zdx_core::core::thread_persistence::read_thread_model_override(&telegram_thread_id(
        chat_id, thread_id,
    ))
    .ok()
    .flatten()
    .unwrap_or(config.model)
}

fn current_topic_thinking(
    context: &BotContext,
    chat_id: i64,
    thread_id: Option<i64>,
) -> zdx_core::config::ThinkingLevel {
    let config = context.config();
    zdx_core::core::thread_persistence::read_thread_thinking_override(&telegram_thread_id(
        chat_id, thread_id,
    ))
    .ok()
    .flatten()
    .unwrap_or(config.thinking_level)
}

fn set_topic_model(chat_id: i64, thread_id: Option<i64>, model_id: &str) -> String {
    match zdx_core::core::thread_persistence::Thread::with_id(telegram_thread_id(
        chat_id, thread_id,
    )) {
        Ok(mut thread) => match thread.set_model_override(Some(model_id.to_string())) {
            Ok(()) => format!("✅ Model set to <code>{model_id}</code> for this topic."),
            Err(err) => format!("❌ Failed to set override: {err}"),
        },
        Err(err) => format!("❌ Failed to open thread: {err}"),
    }
}

fn set_topic_thinking(
    chat_id: i64,
    thread_id: Option<i64>,
    level: zdx_core::config::ThinkingLevel,
) -> String {
    match zdx_core::core::thread_persistence::Thread::with_id(telegram_thread_id(
        chat_id, thread_id,
    )) {
        Ok(mut thread) => match thread.set_thinking_override(Some(level)) {
            Ok(()) => format!(
                "✅ Thinking set to <code>{}</code> for this topic.",
                level.display_name()
            ),
            Err(err) => format!("❌ Failed to set override: {err}"),
        },
        Err(err) => format!("❌ Failed to open thread: {err}"),
    }
}

fn reset_topic_thinking(context: &BotContext, chat_id: i64, thread_id: Option<i64>) -> String {
    let config = context.config();
    match zdx_core::core::thread_persistence::Thread::with_id(telegram_thread_id(
        chat_id, thread_id,
    )) {
        Ok(mut thread) => match thread.set_thinking_override(None) {
            Ok(()) => format!(
                "✅ Thinking reset to default: <code>{}</code>",
                config.thinking_level.display_name()
            ),
            Err(err) => format!("❌ Failed to reset override: {err}"),
        },
        Err(err) => format!("❌ Failed to open thread: {err}"),
    }
}

fn model_picker_header(
    context: &BotContext,
    chat_id: i64,
    thread_id: Option<i64>,
    is_general: bool,
) -> String {
    let config = context.config();
    if is_general {
        format!("Current model: <code>{}</code>", config.model)
    } else {
        let override_info = zdx_core::core::thread_persistence::read_thread_model_override(
            &telegram_thread_id(chat_id, thread_id),
        )
        .ok()
        .flatten()
        .map_or_else(String::new, |m| {
            format!("\nCurrent override: <code>{m}</code>")
        });
        format!(
            "Current model: <code>{}</code>{override_info}",
            config.model
        )
    }
}

/// Handle model-selection inline keyboard callbacks.
async fn handle_model_callback(
    context: &BotContext,
    client: &TelegramClient,
    callback: &CallbackQuery,
    data: &str,
) {
    let Some(msg) = callback.message.as_ref() else {
        let _ = client
            .answer_callback_query(&callback.id, Some("No message context"))
            .await;
        return;
    };

    let chat_id = msg.chat.id;
    let message_id = msg.id;

    if let Some(rest) = data.strip_prefix("model_provider:") {
        let Some((provider, scope)) = rest.split_once(':') else {
            return;
        };
        let is_general = scope == "general";
        let keyboard =
            crate::handlers::message::build_models_keyboard(context, provider, is_general);
        let header = format!("Select a <b>{provider}</b> model:");
        if let Err(err) = client
            .edit_message_text(chat_id, message_id, &header, Some(&keyboard))
            .await
        {
            eprintln!("Failed to edit message for model provider: {err}");
        }
    } else if let Some(rest) = data.strip_prefix("model_set:") {
        let Some(last_colon) = rest.rfind(':') else {
            return;
        };
        let model_id = &rest[..last_colon];
        let scope = &rest[last_colon + 1..];
        let is_general = scope == "general";

        let reply = if is_general {
            match Config::save_telegram_model(model_id) {
                Ok(()) => {
                    context.update_config(|cfg| {
                        cfg.telegram.model = model_id.to_string();
                        cfg.model = model_id.to_string();
                    });
                    format!("✅ Default model set to <code>{model_id}</code>.")
                }
                Err(err) => format!("❌ Failed to save model: {err}"),
            }
        } else {
            set_topic_model(chat_id, msg.thread_id, model_id)
        };

        if let Err(err) = client
            .edit_message_text(chat_id, message_id, &reply, None)
            .await
        {
            eprintln!("Failed to edit message for model set: {err}");
        }
    } else if let Some(scope) = data.strip_prefix("model_back:") {
        let is_general = scope == "general";
        let keyboard = crate::handlers::message::build_provider_keyboard(context, is_general);
        let header = model_picker_header(context, chat_id, msg.thread_id, is_general);

        if let Err(err) = client
            .edit_message_text(chat_id, message_id, &header, Some(&keyboard))
            .await
        {
            eprintln!("Failed to edit message for model back: {err}");
        }
    } else if let Some(scope) = data.strip_prefix("model_cancel:") {
        let is_general = scope == "general";
        let current = if is_general {
            context.config().model
        } else {
            current_topic_model(context, chat_id, msg.thread_id)
        };
        let reply = format!("Model change cancelled. Current model: <code>{current}</code>");
        if let Err(err) = client
            .edit_message_text(chat_id, message_id, &reply, None)
            .await
        {
            eprintln!("Failed to edit message for model cancel: {err}");
        }
    }

    let _ = client.answer_callback_query(&callback.id, None).await;
}

/// Handle thinking-selection inline keyboard callbacks.
async fn handle_thinking_callback(
    context: &BotContext,
    client: &TelegramClient,
    callback: &CallbackQuery,
    data: &str,
) {
    let Some(msg) = callback.message.as_ref() else {
        let _ = client
            .answer_callback_query(&callback.id, Some("No message context"))
            .await;
        return;
    };

    let chat_id = msg.chat.id;
    let message_id = msg.id;

    if let Some(rest) = data.strip_prefix("thinking_set:") {
        let Some((level_str, scope)) = rest.split_once(':') else {
            return;
        };
        let is_general = scope == "general";
        let level = match level_str {
            "off" => zdx_core::config::ThinkingLevel::Off,
            "minimal" => zdx_core::config::ThinkingLevel::Minimal,
            "low" => zdx_core::config::ThinkingLevel::Low,
            "medium" => zdx_core::config::ThinkingLevel::Medium,
            "high" => zdx_core::config::ThinkingLevel::High,
            "xhigh" => zdx_core::config::ThinkingLevel::XHigh,
            _ => {
                let _ = client
                    .answer_callback_query(&callback.id, Some("Unknown thinking level"))
                    .await;
                return;
            }
        };

        let reply = if is_general {
            match Config::save_telegram_thinking_level(level) {
                Ok(()) => {
                    context.update_config(|cfg| {
                        cfg.telegram.thinking_level = level;
                        cfg.thinking_level = level;
                    });
                    format!(
                        "✅ Default thinking set to <code>{}</code>.",
                        level.display_name()
                    )
                }
                Err(err) => format!("❌ Failed to save thinking level: {err}"),
            }
        } else {
            set_topic_thinking(chat_id, msg.thread_id, level)
        };

        if let Err(err) = client
            .edit_message_text(chat_id, message_id, &reply, None)
            .await
        {
            eprintln!("Failed to edit message for thinking set: {err}");
        }
    } else if let Some(scope) = data.strip_prefix("thinking_reset:") {
        let is_general = scope == "general";
        let reply = if is_general {
            let config = context.config();
            format!(
                "Default thinking: <code>{}</code>",
                config.thinking_level.display_name()
            )
        } else {
            reset_topic_thinking(context, chat_id, msg.thread_id)
        };

        if let Err(err) = client
            .edit_message_text(chat_id, message_id, &reply, None)
            .await
        {
            eprintln!("Failed to edit message for thinking reset: {err}");
        }
    } else if let Some(scope) = data.strip_prefix("thinking_cancel:") {
        let is_general = scope == "general";
        let current = if is_general {
            context.config().thinking_level
        } else {
            current_topic_thinking(context, chat_id, msg.thread_id)
        };
        let reply = format!(
            "Thinking change cancelled. Current thinking: <code>{}</code>",
            current.display_name()
        );

        if let Err(err) = client
            .edit_message_text(chat_id, message_id, &reply, None)
            .await
        {
            eprintln!("Failed to edit message for thinking cancel: {err}");
        }
    }

    let _ = client.answer_callback_query(&callback.id, None).await;
}
