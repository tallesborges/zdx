use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Result, anyhow};
use zdx_core::config::{Config, ThinkingLevel};
use zdx_core::core::agent::{ToolConfig, ToolSelection};
use zdx_core::tools::{ToolRegistry, ToolSet};

use crate::bot::{BotContext, enqueue_message, new_chat_queues};
use crate::telegram::{TelegramClient, TelegramSettings};

mod agent;
mod bot;
mod handlers;
mod ingest;
mod telegram;
mod transcribe;
mod types;

const BOT_SYSTEM_PROMPT: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/prompts/bot_system_prompt.md"
));

#[tokio::main]
async fn main() -> Result<()> {
    let mut config = Config::load().map_err(|_| anyhow!("Failed to load zdx config"))?;
    config.model = "claude-cli:claude-opus-4-5".to_string();
    config.thinking_level = ThinkingLevel::Off;
    let settings = TelegramSettings::from_config(&config)?;
    let config_path = zdx_core::config::paths::config_path();
    if config_path.exists() {
        eprintln!("Config file: {}", config_path.display());
    }
    run_bot(config, settings).await
}

async fn run_bot(config: Config, settings: TelegramSettings) -> Result<()> {
    let root = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let client = TelegramClient::new(settings.bot_token);
    let mut tool_registry = ToolRegistry::builtins();
    let (telegram_def, telegram_handler) = telegram::telegram_send_tool(client.clone());
    tool_registry.register(telegram_def, telegram_handler);
    let tool_config = ToolConfig::new(
        tool_registry,
        ToolSelection::Auto {
            base: ToolSet::Default,
            include: vec!["telegram_send".to_string()],
        },
    );

    let allowlist_len = settings.allowlist_user_ids.len();
    let trimmed_prompt = BOT_SYSTEM_PROMPT.trim();
    let system_prompt = (!trimmed_prompt.is_empty()).then(|| trimmed_prompt.to_string());
    let context = Arc::new(BotContext::new(
        client.clone(),
        config,
        settings.allowlist_user_ids,
        root,
        system_prompt,
        tool_config,
    ));
    let chat_queues = new_chat_queues();

    let mut offset: Option<i64> = None;
    let poll_timeout = Duration::from_secs(30);
    let shutdown = tokio::signal::ctrl_c();
    tokio::pin!(shutdown);

    eprintln!(
        "zdx-bot started. Allowlist users: {}. Polling for updates...",
        allowlist_len
    );

    loop {
        let current_offset = offset;
        tokio::select! {
            _ = &mut shutdown => {
                eprintln!("Shutting down Telegram bot.");
                break;
            }
            updates = client.get_updates(current_offset, poll_timeout) => {
                let updates = match updates {
                    Ok(updates) => updates,
                    Err(err) => {
                        eprintln!("Telegram polling error: {}", err);
                        tokio::time::sleep(Duration::from_secs(1)).await;
                        continue;
                    }
                };

                if !updates.is_empty() {
                    eprintln!("Received {} update(s)", updates.len());
                }
                for update in updates {
                    offset = Some(update.update_id + 1);
                    if let Some(message) = update.message {
                        enqueue_message(&chat_queues, &context, message).await;
                    }
                }
            }
        }
    }

    Ok(())
}
