use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Result, anyhow};
use zdx_core::config::{Config, ThinkingLevel};
use zdx_core::core::agent::{self, AgentOptions, ToolConfig, ToolSelection};
use zdx_core::core::context::build_effective_system_prompt_with_paths;
use zdx_core::core::thread_log::{self, ThreadEvent, ThreadLog};
use zdx_core::providers::ChatMessage;
use zdx_core::tools::{ToolRegistry, ToolSet};

use crate::telegram::{Message, TelegramClient, TelegramSettings};

mod telegram;

#[tokio::main]
async fn main() -> Result<()> {
    let mut config = Config::load().map_err(|_| anyhow!("Failed to load zdx config"))?;
    config.model = "claude-cli:claude-haiku-4-5".to_string();
    config.thinking_level = ThinkingLevel::Minimal;
    let settings = TelegramSettings::from_config(&config)?;
    let config_path = zdx_core::config::paths::config_path();
    if config_path.exists() {
        eprintln!("Config file: {}", config_path.display());
    }
    run_bot(config, settings).await
}

async fn run_bot(config: Config, settings: TelegramSettings) -> Result<()> {
    let root = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let effective = build_effective_system_prompt_with_paths(&config, &root)
        .map_err(|_| anyhow!("Failed to load system prompt context (AGENTS/skills)"))?;

    for warning in &effective.warnings {
        eprintln!("Warning: {}", warning.message);
    }

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
    let mut offset: Option<i64> = None;
    let poll_timeout = Duration::from_secs(30);
    let shutdown = tokio::signal::ctrl_c();
    tokio::pin!(shutdown);

    eprintln!(
        "zdx-bot started. Allowlist users: {}. Polling for updates...",
        settings.allowlist_user_ids.len()
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
                    if let Some(message) = update.message
                        && let Err(err) = handle_message(
                            &client,
                            &config,
                            &settings.allowlist_user_ids,
                            &root,
                            effective.prompt.as_deref(),
                            message,
                            &tool_config,
                        )
                        .await
                    {
                        eprintln!("Message handling error: {}", err);
                    }
                }
            }
        }
    }

    Ok(())
}

async fn handle_message(
    client: &TelegramClient,
    config: &Config,
    allowlist: &HashSet<i64>,
    root: &Path,
    system_prompt: Option<&str>,
    message: Message,
    tool_config: &ToolConfig,
) -> Result<()> {
    let Some(incoming) = parse_incoming_message(client, allowlist, message).await? else {
        return Ok(());
    };

    eprintln!(
        "Accepted message from user {} in chat {}",
        incoming.user_id, incoming.chat_id
    );

    if is_new_command(&incoming.text) {
        let thread_id = thread_id_for_chat(incoming.chat_id);
        clear_thread_history(&thread_id)?;
        client
            .send_message(
                incoming.chat_id,
                "History cleared. Start a new conversation anytime.",
                Some(incoming.message_id),
            )
            .await?;
        return Ok(());
    }

    let thread_id = thread_id_for_chat(incoming.chat_id);
    let (mut thread, mut messages) = load_thread_state(&thread_id)?;
    record_user_message(&mut thread, &mut messages, &incoming.text)?;

    let result = run_agent_turn_with_persist(
        messages,
        config,
        root,
        system_prompt,
        &thread_id,
        &thread,
        tool_config,
    )
    .await;

    match result {
        Ok((final_text, _messages)) => {
            thread
                .append(&ThreadEvent::assistant_message(&final_text))
                .map_err(|_| anyhow!("Failed to append assistant message"))?;
            if !final_text.trim().is_empty() {
                eprintln!("Sending reply for chat {}", incoming.chat_id);
                client
                    .send_message(incoming.chat_id, &final_text, Some(incoming.message_id))
                    .await?;
            }
        }
        Err(err) => {
            eprintln!("Agent error: {}", err);
            let _ = client
                .send_message(
                    incoming.chat_id,
                    "Sorry, something went wrong.",
                    Some(incoming.message_id),
                )
                .await;
        }
    }

    Ok(())
}

fn thread_id_for_chat(chat_id: i64) -> String {
    format!("telegram-{}", chat_id)
}

fn is_new_command(text: &str) -> bool {
    text.trim() == "/new"
}

struct IncomingMessage {
    chat_id: i64,
    message_id: i64,
    user_id: i64,
    text: String,
}

async fn parse_incoming_message(
    client: &TelegramClient,
    allowlist: &HashSet<i64>,
    message: Message,
) -> Result<Option<IncomingMessage>> {
    if !message.chat.is_private() {
        eprintln!("Ignoring non-DM chat {}", message.chat.id);
        return Ok(None);
    }

    let chat_id = message.chat.id;
    let message_id = message.message_id;

    let Some(user) = message.from else {
        eprintln!("Ignoring message without sender in chat {}", chat_id);
        return Ok(None);
    };

    if !allowlist.contains(&user.id) {
        eprintln!("Denied user {} for chat {}", user.id, chat_id);
        let _ = client
            .send_message(chat_id, "Access denied.", Some(message_id))
            .await;
        return Ok(None);
    }

    let text = match message.text.as_deref() {
        Some(raw) => {
            let trimmed = raw.trim();
            if trimmed.is_empty() {
                eprintln!("Ignoring empty message in chat {}", chat_id);
                return Ok(None);
            }
            trimmed.to_string()
        }
        None => {
            eprintln!("Ignoring non-text message in chat {}", chat_id);
            return Ok(None);
        }
    };

    Ok(Some(IncomingMessage {
        chat_id,
        message_id,
        user_id: user.id,
        text,
    }))
}

fn load_thread_state(thread_id: &str) -> Result<(ThreadLog, Vec<ChatMessage>)> {
    let thread = ThreadLog::with_id(thread_id.to_string())
        .map_err(|_| anyhow!("Failed to open thread log"))?;
    let messages = thread_log::load_thread_as_messages(thread_id)
        .map_err(|_| anyhow!("Failed to load thread history"))?;
    Ok((thread, messages))
}

fn clear_thread_history(thread_id: &str) -> Result<()> {
    let thread = ThreadLog::with_id(thread_id.to_string())
        .map_err(|_| anyhow!("Failed to resolve thread log"))?;
    let path = thread.path();
    if path.exists() {
        fs::remove_file(path).map_err(|_| anyhow!("Failed to clear thread history"))?;
    }
    Ok(())
}

fn record_user_message(
    thread: &mut ThreadLog,
    messages: &mut Vec<ChatMessage>,
    text: &str,
) -> Result<()> {
    thread
        .append(&ThreadEvent::user_message(text.to_string()))
        .map_err(|_| anyhow!("Failed to append user message"))?;
    messages.push(ChatMessage::user(text.to_string()));
    Ok(())
}

async fn run_agent_turn_with_persist(
    messages: Vec<ChatMessage>,
    config: &Config,
    root: &Path,
    system_prompt: Option<&str>,
    thread_id: &str,
    thread: &ThreadLog,
    tool_config: &ToolConfig,
) -> Result<(String, Vec<ChatMessage>)> {
    let agent_opts = AgentOptions {
        root: root.to_path_buf(),
        tool_config: tool_config.clone(),
    };

    let (agent_tx, agent_rx) = agent::create_event_channel();
    let persist_handle = thread_log::spawn_thread_persist_task(thread.clone(), agent_rx);

    let result = agent::run_turn(
        messages,
        config,
        &agent_opts,
        system_prompt,
        Some(thread_id),
        agent_tx,
    )
    .await;

    let _ = persist_handle.await;

    result
}
