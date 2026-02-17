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
    let config_path = zdx_core::config::paths::config_path();
    if config_path.exists() {
        eprintln!("Config file: {}", config_path.display());
    }
    eprintln!(
        "Model: {} | Thinking: {} | Users: {:?} | Chats: {:?}",
        config.model,
        config.thinking_level.display_name(),
        config.telegram.allowlist_user_ids,
        config.telegram.allowlist_chat_ids,
    );
    run_bot(config, settings, root).await
}

async fn run_bot(config: Config, settings: TelegramSettings, root: PathBuf) -> Result<()> {
    let client = TelegramClient::new(settings.bot_token);
    let command_specs = crate::commands::telegram_command_specs();
    match client.set_my_commands(&command_specs).await {
        Ok(()) => eprintln!(
            "Telegram command menu updated ({} command(s)).",
            command_specs.len()
        ),
        Err(err) => eprintln!("Failed to update Telegram command menu: {err}"),
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

    eprintln!(
        "zdx-bot started. Allowlist: {allowlist_user_len} user(s), {allowlist_chat_len} chat(s). Polling for updates..."
    );

    loop {
        let current_offset = offset;
        tokio::select! {
            _ = &mut shutdown => {
                eprintln!("Shutting down Telegram bot.");
                break;
            }
            () = context.rebuild_notified() => {
                eprintln!("Rebuild requested via /rebuild command.");
                std::process::exit(EXIT_REBUILD);
            }
            updates = client.get_updates(current_offset, poll_timeout) => {
                let updates = match updates {
                    Ok(updates) => updates,
                    Err(err) => {
                        eprintln!("Telegram polling error: {err}");
                        tokio::time::sleep(Duration::from_secs(1)).await;
                        continue;
                    }
                };

                if !updates.is_empty() {
                    eprintln!("Received {} update(s)", updates.len());
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
        eprintln!(
            "Denied callback from non-allowlisted user {}",
            callback.from.id
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
                eprintln!("Failed to answer cancel callback: {err}");
            }
            eprintln!("Cancelled agent turn for {key:?}");
        } else if let Err(err) = client
            .answer_callback_query(&callback.id, Some("Nothing to cancel"))
            .await
        {
            eprintln!("Failed to answer callback: {err}");
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
                eprintln!("Failed to answer queue cancel callback: {err}");
            }
            eprintln!("Cancelled queued item for {key:?}");
        } else {
            // Token gone — item may have already started processing
            if let Err(err) = client
                .answer_callback_query(&callback.id, Some("Already processing"))
                .await
            {
                eprintln!("Failed to answer callback: {err}");
            }
        }
    } else {
        if let Err(err) = client.answer_callback_query(&callback.id, None).await {
            eprintln!("Failed to answer unknown callback: {err}");
        }
        eprintln!(
            "Unknown callback from user {}: {:?}",
            callback.from.id, data
        );
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
